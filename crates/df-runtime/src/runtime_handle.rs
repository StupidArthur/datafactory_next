//! Read-only runtime access for data adapters (R1 of `doc/runtime-system-design.md`).
//!
//! [`RuntimeHandle`] separates lifecycle ownership from observation: adapters
//! (OPC UA, HTTP, tests) can read the latest snapshot, wait for new cycles,
//! submit write batches and query their state, but they can never stop the
//! model or obtain an Engine reference. Lifecycle ownership stays with
//! [`crate::Runtime`], which fulfils the "Runtime Owner" role.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use df_core::{Snapshot, Value};

use crate::{stop_requested, Command, RuntimeError, RuntimeStatus, WriteReceipt, WriteTicket};

/// Maximum number of retained write records. Oldest terminal records are
/// pruned first; queued or in-flight batches are never evicted.
pub(crate) const WRITE_REGISTRY_CAPACITY: usize = 1024;

/// Outcome of waiting for a newly published snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// A revision greater than the requested one has been published.
    Changed {
        /// The revision that satisfied the wait.
        revision: u64,
    },
    /// No new revision arrived within the timeout. The wait can be retried
    /// with the same `after_revision`; timeouts do not consume anything.
    Timeout,
    /// The runtime stopped or failed; no further revisions are expected.
    Stopped,
}

/// Lifecycle state of one write batch (`doc/runtime-system-design.md` §8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteState {
    /// Accepted into the bounded queue; the batch can still be cancelled.
    Queued,
    /// The scheduler dequeued the batch and is applying it at the cycle boundary.
    Applying,
    /// The Engine confirmed the batch in the reported cycle.
    Applied,
    /// The batch was rejected atomically (invalid values, stopped runtime, ...).
    Rejected,
    /// The batch was cancelled before the scheduler started applying it.
    Cancelled,
}

impl WriteState {
    /// Terminal states keep their recorded result and are prune candidates.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Applied | Self::Rejected | Self::Cancelled)
    }
}

/// Queryable status of one previously submitted write batch.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteStatusInfo {
    pub request_id: u64,
    pub state: WriteState,
    /// Number of tags contained in the batch.
    pub tag_count: usize,
    /// Rejection reason; present exactly for [`WriteState::Rejected`].
    pub error: Option<String>,
    /// Effective-cycle evidence; present exactly for [`WriteState::Applied`].
    pub receipt: Option<WriteReceipt>,
}

#[derive(Debug)]
struct FeedInner {
    revision: u64,
    stopped: bool,
}

/// Revision counter and change notification for published snapshots.
///
/// The scheduler bumps the revision once per successfully executed cycle and
/// notifies all waiters. Waiters re-read the snapshot after waking, so slow
/// consumers can never block the model and may skip intermediate revisions.
#[derive(Debug)]
pub(crate) struct SnapshotFeed {
    inner: Mutex<FeedInner>,
    condvar: Condvar,
}

impl SnapshotFeed {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(FeedInner {
                revision: 0,
                stopped: false,
            }),
            condvar: Condvar::new(),
        }
    }

    /// Publish one new revision and wake every waiter.
    pub(crate) fn publish(&self) -> u64 {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.revision = inner.revision.wrapping_add(1);
        let revision = inner.revision;
        self.condvar.notify_all();
        revision
    }

    /// Mark the feed as finished; waiters wake up with [`WaitOutcome::Stopped`].
    pub(crate) fn mark_stopped(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.stopped = true;
        self.condvar.notify_all();
    }

    /// Revision of the most recently published snapshot.
    pub fn current_revision(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revision
    }

    /// Wait until a revision greater than `after_revision` is published, the
    /// runtime stops, or the timeout elapses.
    pub fn wait_for_change(&self, after_revision: u64, timeout: Duration) -> WaitOutcome {
        // A timeout large enough to overflow `Instant` is clamped to one hour.
        let deadline = Instant::now()
            .checked_add(timeout)
            .or_else(|| Instant::now().checked_add(Duration::from_secs(3600)))
            .unwrap_or_else(Instant::now);
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            // Deliver a final revision even when the runtime already stopped.
            if inner.revision > after_revision {
                return WaitOutcome::Changed {
                    revision: inner.revision,
                };
            }
            if inner.stopped {
                return WaitOutcome::Stopped;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return WaitOutcome::Timeout;
            }
            let (guard, _) = self
                .condvar
                .wait_timeout(inner, remaining)
                .unwrap_or_else(|p| p.into_inner());
            inner = guard;
        }
    }
}

#[derive(Debug, Clone)]
struct WriteRecord {
    state: WriteState,
    tag_count: usize,
    error: Option<String>,
    receipt: Option<WriteReceipt>,
}

/// Registry of write batches keyed by monotonically increasing request ids.
#[derive(Debug)]
pub(crate) struct WriteRegistry {
    records: Mutex<BTreeMap<u64, WriteRecord>>,
    next_request_id: AtomicU64,
}

impl WriteRegistry {
    pub(crate) fn new() -> Self {
        Self {
            records: Mutex::new(BTreeMap::new()),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Register a new batch and return its request id.
    pub(crate) fn register(&self, tag_count: usize) -> u64 {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        records.insert(
            request_id,
            WriteRecord {
                state: WriteState::Queued,
                tag_count,
                error: None,
                receipt: None,
            },
        );
        if records.len() > WRITE_REGISTRY_CAPACITY {
            let excess = records.len() - WRITE_REGISTRY_CAPACITY;
            let evictable: Vec<u64> = records
                .iter()
                .filter(|(_, record)| record.state.is_terminal())
                .map(|(request_id, _)| *request_id)
                .take(excess)
                .collect();
            for request_id in evictable {
                records.remove(&request_id);
            }
        }
        request_id
    }

    /// Transition Queued → Applying. Returns `false` when the batch was
    /// cancelled or is not pending anymore; the caller must not apply it and
    /// keeps the model tick running without writes.
    pub(crate) fn begin_apply(&self, request_id: u64) -> bool {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        match records.get_mut(&request_id) {
            Some(record) if record.state == WriteState::Queued => {
                record.state = WriteState::Applying;
                true
            }
            _ => false,
        }
    }

    /// Record the final result of a batch. Terminal states are never overwritten.
    pub(crate) fn finish(&self, request_id: u64, result: Result<&WriteReceipt, &RuntimeError>) {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(record) = records.get_mut(&request_id) {
            if record.state.is_terminal() {
                return;
            }
            match result {
                Ok(receipt) => {
                    record.state = WriteState::Applied;
                    record.receipt = Some(receipt.clone());
                    record.error = None;
                }
                Err(error) => {
                    record.state = WriteState::Rejected;
                    record.error = Some(error.to_string());
                }
            }
        }
    }

    /// Cancel a batch that has not started applying yet.
    pub(crate) fn cancel(&self, request_id: u64) -> bool {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        match records.get_mut(&request_id) {
            Some(record) if record.state == WriteState::Queued => {
                record.state = WriteState::Cancelled;
                true
            }
            _ => false,
        }
    }

    /// Query the recorded status of one batch.
    pub(crate) fn status_of(&self, request_id: u64) -> Option<WriteStatusInfo> {
        let records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        records.get(&request_id).map(|record| WriteStatusInfo {
            request_id,
            state: record.state,
            tag_count: record.tag_count,
            error: record.error.clone(),
            receipt: record.receipt.clone(),
        })
    }
}

/// Cloneable, read-only access to one realtime model instance.
///
/// The handle exposes exactly the adapter surface promised by the system
/// design (§5.2): observe status and snapshots, wait for new cycles, submit
/// write batches, query and cancel them. It cannot stop the runtime.
#[derive(Clone)]
pub struct RuntimeHandle {
    pub(crate) sender: SyncSender<Command>,
    pub(crate) status: Arc<Mutex<RuntimeStatus>>,
    pub(crate) snapshot: Arc<RwLock<Snapshot>>,
    pub(crate) feed: Arc<SnapshotFeed>,
    pub(crate) writes: Arc<WriteRegistry>,
    pub(crate) pending_writes: Arc<AtomicUsize>,
    pub(crate) stop_signal: Arc<(Mutex<bool>, Condvar)>,
}

impl RuntimeHandle {
    /// Latest lifecycle status; `queue_depth` reflects the live write queue.
    pub fn status(&self) -> RuntimeStatus {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        status.queue_depth = self.pending_writes.load(Ordering::SeqCst);
        status
    }

    /// Return the most recently published, immutable model snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Revision of the most recently published snapshot.
    pub fn snapshot_revision(&self) -> u64 {
        self.feed.current_revision()
    }

    /// Wait for a snapshot newer than `after_revision`. Timeouts are benign
    /// and can be retried; once the runtime stops the outcome is [`WaitOutcome::Stopped`].
    pub fn wait_for_snapshot(&self, after_revision: u64, timeout: Duration) -> WaitOutcome {
        self.feed.wait_for_change(after_revision, timeout)
    }

    /// Queue one atomic write batch for the next available model cycle.
    ///
    /// Queue admission is synchronous. The returned ticket reports whether the
    /// Engine accepted the values and the cycle in which they became effective.
    /// The batch is recorded under a monotonically increasing request id which
    /// can be queried via [`Self::write_state`].
    pub fn submit_writes(
        &self,
        writes: BTreeMap<String, Value>,
    ) -> Result<WriteTicket, RuntimeError> {
        if writes.is_empty() {
            return Err(RuntimeError::EmptyWriteBatch);
        }
        if stop_requested(&self.stop_signal) {
            return Err(RuntimeError::WorkerStopped);
        }

        let request_id = self.writes.register(writes.len());
        let (reply, receiver) = std::sync::mpsc::channel();
        // Increment before sending so the worker can never underflow the
        // counter by consuming a batch faster than admission finishes.
        self.pending_writes.fetch_add(1, Ordering::SeqCst);
        match self.sender.try_send(Command::Write {
            request_id,
            values: writes,
            reply,
        }) {
            Ok(()) => Ok(WriteTicket {
                request_id,
                receiver,
            }),
            Err(TrySendError::Full(_)) => {
                self.pending_writes.fetch_sub(1, Ordering::SeqCst);
                self.writes
                    .finish(request_id, Err(&RuntimeError::QueueFull));
                Err(RuntimeError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.pending_writes.fetch_sub(1, Ordering::SeqCst);
                self.writes
                    .finish(request_id, Err(&RuntimeError::WorkerStopped));
                Err(RuntimeError::WorkerStopped)
            }
        }
    }

    /// Query the lifecycle state of a previously submitted write batch.
    ///
    /// Returns `None` for unknown ids and for ids pruned after reaching the
    /// registry capacity with terminal states.
    pub fn write_state(&self, request_id: u64) -> Option<WriteStatusInfo> {
        self.writes.status_of(request_id)
    }

    /// Cancel a batch that is still queued. Only batches that have not started
    /// applying can be cancelled; returns `false` for unknown, applying and
    /// terminal batches.
    pub fn cancel_write(&self, request_id: u64) -> bool {
        self.writes.cancel(request_id)
    }

    /// Number of write batches currently queued and not yet taken by the scheduler.
    pub fn queue_depth(&self) -> usize {
        self.pending_writes.load(Ordering::SeqCst)
    }
}
