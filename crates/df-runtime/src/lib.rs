use df_core::{Engine, EngineError, Snapshot, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

mod config;
mod instance;
mod loader;
mod manager;
mod runtime_handle;

pub use config::{
    HostConfig, InstanceConfig, OpcUaConfig, OpcUaSecurityConfig, SchedulerConfig, SecurityMode,
    SecurityPolicy,
};
pub use instance::{InstanceState, InstanceSummary};
pub use loader::load_engine;
pub use manager::RuntimeManager;
pub use runtime_handle::{RuntimeHandle, WaitOutcome, WriteState, WriteStatusInfo};
use runtime_handle::{SnapshotFeed, WriteRegistry};

const DEFAULT_WRITE_QUEUE_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrunPolicy {
    /// Start the next period after the current cycle finishes.
    /// No model cycles are skipped, but simulation time can lag wall time.
    Delay,
    /// Keep the original wall-clock schedule and execute overdue cycles immediately.
    /// No model cycles are skipped.
    CatchUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub overrun_policy: OverrunPolicy,
    pub write_queue_capacity: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            overrun_policy: OverrunPolicy::Delay,
            write_queue_capacity: DEFAULT_WRITE_QUEUE_CAPACITY,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeStatus {
    pub state: RuntimeState,
    pub cycle_time: f64,
    pub cycle_index: u64,
    pub simulation_time: f64,
    pub last_execution_duration: Duration,
    pub max_execution_duration: Duration,
    pub overrun_count: u64,
    pub missed_deadlines: u64,
    pub rejected_write_batches: u64,
    pub last_write_error: Option<String>,
    pub last_error: Option<String>,
    /// Revisions are bumped once per successfully published cycle.
    pub snapshot_revision: u64,
    /// Write batches currently queued and not yet taken by the scheduler.
    pub queue_depth: usize,
    /// Wall-clock time at which the most recent cycle started.
    pub last_cycle_started_at: Option<SystemTime>,
    /// How much later than its wall-clock deadline the most recent cycle started.
    pub last_cycle_lag: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteReceipt {
    /// Engine cycle in which this write batch became effective.
    pub effective_cycle_index: u64,
    pub simulation_time: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    Engine(String),
    InvalidCycleTime,
    InvalidQueueCapacity,
    EmptyWriteBatch,
    QueueFull,
    WorkerStopped,
    Timeout,
    WriteRejected { code: i32, message: String },
    WriteCancelled,
    WorkerPanicked,
    Config(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(message) => write!(f, "runtime engine failure: {message}"),
            Self::InvalidCycleTime => {
                write!(f, "project cycle_time must be a finite positive duration")
            }
            Self::InvalidQueueCapacity => write!(f, "write queue capacity must be greater than 0"),
            Self::EmptyWriteBatch => write!(f, "write batch must not be empty"),
            Self::QueueFull => write!(f, "runtime write queue is full"),
            Self::WorkerStopped => write!(f, "runtime worker is stopped"),
            Self::Timeout => write!(f, "timed out waiting for the write result"),
            Self::WriteRejected { code, message } => {
                write!(f, "write rejected by engine (code {code}): {message}")
            }
            Self::WriteCancelled => write!(f, "write batch was cancelled before it was applied"),
            Self::WorkerPanicked => write!(f, "runtime worker panicked"),
            Self::Config(message) => write!(f, "runtime host configuration error: {message}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineExecutionError {
    WriteRejected { code: i32, message: String },
    Fatal { code: i32, message: String },
}

/// Adapter boundary between the realtime scheduler and the model execution engine.
///
/// execute_cycle must advance exactly one model cycle on success. A rejected write
/// batch must leave the model state and cycle index unchanged.
pub trait EngineExecutor: Send + 'static {
    fn cycle_time(&mut self) -> Result<f64, String>;
    fn current_snapshot(&mut self) -> Result<Snapshot, String>;
    fn execute_cycle(
        &mut self,
        writes: &BTreeMap<String, Value>,
    ) -> Result<Snapshot, EngineExecutionError>;
    fn stop(&mut self) -> Result<(), String>;
}

impl EngineExecutor for Engine {
    fn cycle_time(&mut self) -> Result<f64, String> {
        Ok(self.metadata().cycle_time)
    }

    fn current_snapshot(&mut self) -> Result<Snapshot, String> {
        Ok(self.snapshot())
    }

    fn execute_cycle(
        &mut self,
        writes: &BTreeMap<String, Value>,
    ) -> Result<Snapshot, EngineExecutionError> {
        self.execute(1, writes).map_err(map_engine_error)
    }

    fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn map_engine_error(error: EngineError) -> EngineExecutionError {
    let code = error.code();
    let message = error.to_string();
    if (1201..=1203).contains(&code) {
        EngineExecutionError::WriteRejected { code, message }
    } else {
        EngineExecutionError::Fatal { code, message }
    }
}

enum Command {
    Write {
        request_id: u64,
        values: BTreeMap<String, Value>,
        reply: Sender<Result<WriteReceipt, RuntimeError>>,
    },
}

/// A handle to one realtime model instance.
///
/// The scheduler owns one Engine instance and advances it by one cycle per
/// wall-clock deadline. Write batches are FIFO and applied atomically at a
/// cycle boundary. The latest immutable snapshot is available to data adapters.
pub struct Runtime {
    sender: SyncSender<Command>,
    status: Arc<Mutex<RuntimeStatus>>,
    snapshot: Arc<RwLock<Snapshot>>,
    feed: Arc<SnapshotFeed>,
    writes: Arc<WriteRegistry>,
    pending_writes: Arc<AtomicUsize>,
    stop_signal: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    /// Load a `.dfproject` or DSL v2 `.yaml`/`.yml` project and start a
    /// realtime instance with default scheduler settings.
    pub fn load(project_path: &Path) -> Result<Self, RuntimeError> {
        let engine = loader::load_engine(project_path)?;
        Self::start(engine)
    }

    pub fn start<E: EngineExecutor>(engine: E) -> Result<Self, RuntimeError> {
        Self::start_with_config(engine, RuntimeConfig::default())
    }

    pub fn start_with_config<E: EngineExecutor>(
        mut engine: E,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        if config.write_queue_capacity == 0 {
            let _ = engine.stop();
            return Err(RuntimeError::InvalidQueueCapacity);
        }

        let cycle_time = match engine.cycle_time() {
            Ok(value) => value,
            Err(error) => {
                let _ = engine.stop();
                return Err(RuntimeError::Engine(error));
            }
        };
        if !cycle_time.is_finite() || cycle_time <= 0.0 {
            let _ = engine.stop();
            return Err(RuntimeError::InvalidCycleTime);
        }

        let period = match Duration::try_from_secs_f64(cycle_time) {
            Ok(value) if !value.is_zero() => value,
            _ => {
                let _ = engine.stop();
                return Err(RuntimeError::InvalidCycleTime);
            }
        };
        if Instant::now().checked_add(period).is_none() {
            let _ = engine.stop();
            return Err(RuntimeError::InvalidCycleTime);
        }

        let initial_snapshot = match engine.current_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = engine.stop();
                return Err(RuntimeError::Engine(error));
            }
        };

        let status = Arc::new(Mutex::new(RuntimeStatus {
            state: RuntimeState::Starting,
            cycle_time,
            cycle_index: initial_snapshot.cycle_index,
            simulation_time: initial_snapshot.simulation_time,
            last_execution_duration: Duration::ZERO,
            max_execution_duration: Duration::ZERO,
            overrun_count: 0,
            missed_deadlines: 0,
            rejected_write_batches: 0,
            last_write_error: None,
            last_error: None,
            snapshot_revision: 0,
            queue_depth: 0,
            last_cycle_started_at: None,
            last_cycle_lag: None,
        }));
        let snapshot = Arc::new(RwLock::new(initial_snapshot));
        let stop_signal = Arc::new((Mutex::new(false), Condvar::new()));
        let (sender, receiver) = mpsc::sync_channel(config.write_queue_capacity);
        let feed = Arc::new(SnapshotFeed::new());
        let writes = Arc::new(WriteRegistry::new());
        let pending_writes = Arc::new(AtomicUsize::new(0));

        let worker_status = Arc::clone(&status);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_stop_signal = Arc::clone(&stop_signal);
        let worker_feed = Arc::clone(&feed);
        let worker_writes = Arc::clone(&writes);
        let worker_pending_writes = Arc::clone(&pending_writes);
        let worker = thread::Builder::new()
            .name("df-runtime-clock".into())
            .spawn(move || {
                run_worker(
                    engine,
                    period,
                    config,
                    receiver,
                    worker_status,
                    worker_snapshot,
                    worker_feed,
                    worker_writes,
                    worker_pending_writes,
                    worker_stop_signal,
                )
            })
            .map_err(|error| RuntimeError::Engine(format!("cannot start worker: {error}")))?;

        Ok(Self {
            sender,
            status,
            snapshot,
            feed,
            writes,
            pending_writes,
            stop_signal,
            worker: Some(worker),
        })
    }

    /// Return a cloneable, read-only handle to this runtime instance.
    ///
    /// Data adapters use the handle to observe snapshots, wait for new cycles
    /// and submit write batches. The handle cannot stop the runtime; lifecycle
    /// ownership stays with this `Runtime` value.
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            sender: self.sender.clone(),
            status: Arc::clone(&self.status),
            snapshot: Arc::clone(&self.snapshot),
            feed: Arc::clone(&self.feed),
            writes: Arc::clone(&self.writes),
            pending_writes: Arc::clone(&self.pending_writes),
            stop_signal: Arc::clone(&self.stop_signal),
        }
    }

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

    /// Queue one atomic write batch for the next available model cycle.
    ///
    /// Queue admission is synchronous. The returned ticket reports whether the
    /// Engine accepted the values and the cycle in which they became effective.
    pub fn submit_writes(
        &self,
        writes: BTreeMap<String, Value>,
    ) -> Result<WriteTicket, RuntimeError> {
        self.handle().submit_writes(writes)
    }

    /// Stop the scheduler and the owned Engine instance. Safe to call repeatedly.
    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if let Some(worker) = self.worker.take() {
            request_stop(&self.stop_signal);
            self.feed.mark_stopped();
            if worker.join().is_err() {
                let mut status = self
                    .status
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                status.state = RuntimeState::Failed;
                status.last_error = Some("runtime worker panicked".into());
                return Err(RuntimeError::WorkerPanicked);
            }
        }

        let status = self.status();
        if status.state == RuntimeState::Failed {
            return Err(RuntimeError::Engine(
                status
                    .last_error
                    .unwrap_or_else(|| "runtime entered failed state".into()),
            ));
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct WriteTicket {
    request_id: u64,
    receiver: Receiver<Result<WriteReceipt, RuntimeError>>,
}

impl WriteTicket {
    /// Request id under which this batch is recorded; query it via
    /// `RuntimeHandle::write_state`.
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Wait for the Engine's cycle-boundary acknowledgement.
    /// A timeout does not cancel the queued write; this ticket can be waited again.
    pub fn wait(&self, timeout: Duration) -> Result<WriteReceipt, RuntimeError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(RuntimeError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(RuntimeError::WorkerStopped),
        }
    }
}

fn run_worker<E: EngineExecutor>(
    mut engine: E,
    period: Duration,
    config: RuntimeConfig,
    receiver: Receiver<Command>,
    status: Arc<Mutex<RuntimeStatus>>,
    snapshot: Arc<RwLock<Snapshot>>,
    feed: Arc<SnapshotFeed>,
    writes: Arc<WriteRegistry>,
    pending_writes: Arc<AtomicUsize>,
    stop_signal: Arc<(Mutex<bool>, Condvar)>,
) {
    update_status(&status, |current| current.state = RuntimeState::Running);
    let mut next_deadline = Instant::now()
        .checked_add(period)
        .unwrap_or_else(Instant::now);

    loop {
        if wait_until_deadline(next_deadline, &stop_signal) {
            break;
        }

        // Take at most one batch per cycle. The bounded channel remains the
        // only pending-write buffer, so queued work cannot grow without limit.
        let active = match receiver.try_recv() {
            Ok(Command::Write {
                request_id,
                values,
                reply,
            }) => {
                pending_writes.fetch_sub(1, Ordering::SeqCst);
                if writes.begin_apply(request_id) {
                    Some(PendingWrite {
                        request_id,
                        values,
                        reply,
                    })
                } else {
                    // The batch was cancelled while queued; skip it and keep
                    // the model tick running without writes.
                    let _ = reply.send(Err(RuntimeError::WriteCancelled));
                    None
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        };
        let writes_for_cycle = active
            .as_ref()
            .map(|item| item.values.clone())
            .unwrap_or_default();
        let started = Instant::now();
        let started_at = SystemTime::now();
        let schedule_lag = started.saturating_duration_since(next_deadline);
        update_status(&status, |current| {
            current.last_cycle_started_at = Some(started_at);
            current.last_cycle_lag = Some(schedule_lag);
        });

        let first_result = engine.execute_cycle(&writes_for_cycle);
        let cycle_result = match first_result {
            Ok(snapshot) => Ok((snapshot, None)),
            Err(EngineExecutionError::WriteRejected { code, message })
                if !writes_for_cycle.is_empty() =>
            {
                // A bad external write must not consume the scheduled model tick.
                // Reject the batch atomically, then execute this tick without writes.
                match engine.execute_cycle(&BTreeMap::new()) {
                    Ok(snapshot) => Ok((
                        snapshot,
                        Some(RuntimeError::WriteRejected { code, message }),
                    )),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };

        let elapsed = started.elapsed();
        record_execution_duration(&status, elapsed, period);

        match cycle_result {
            Ok((new_snapshot, write_error)) => {
                publish_snapshot(&snapshot, &status, &feed, &new_snapshot);
                if let Some(item) = active {
                    let result = match write_error {
                        Some(error) => {
                            update_status(&status, |current| {
                                current.rejected_write_batches =
                                    current.rejected_write_batches.saturating_add(1);
                                if let RuntimeError::WriteRejected { message, .. } = &error {
                                    current.last_write_error = Some(message.clone());
                                }
                            });
                            writes.finish(item.request_id, Err(&error));
                            Err(error)
                        }
                        None => {
                            update_status(&status, |current| {
                                current.last_write_error = None;
                            });
                            let receipt = WriteReceipt {
                                effective_cycle_index: new_snapshot.cycle_index,
                                simulation_time: new_snapshot.simulation_time,
                            };
                            writes.finish(item.request_id, Ok(&receipt));
                            Ok(receipt)
                        }
                    };
                    let _ = item.reply.send(result);
                }
            }
            Err(error) => {
                let message = match error {
                    EngineExecutionError::WriteRejected { code, message } => {
                        format!("unexpected write rejection during an empty cycle (code {code}): {message}")
                    }
                    EngineExecutionError::Fatal { code, message } => {
                        format!("engine error {code}: {message}")
                    }
                };
                if let Some(item) = active {
                    writes.finish(item.request_id, Err(&RuntimeError::Engine(message.clone())));
                    let _ = item.reply.send(Err(RuntimeError::Engine(message.clone())));
                }
                feed.mark_stopped();
                update_status(&status, |current| {
                    current.state = RuntimeState::Failed;
                    current.last_error = Some(message.clone());
                });
                reject_pending(
                    &receiver,
                    RuntimeError::Engine(message.clone()),
                    &writes,
                    &pending_writes,
                );
                if let Err(stop_error) = engine.stop() {
                    update_status(&status, |current| {
                        current.last_error = Some(format!(
                            "{}; engine shutdown also failed: {}",
                            message, stop_error
                        ));
                    });
                }
                return;
            }
        }

        next_deadline = advance_deadline(next_deadline, period, config.overrun_policy, &status);
    }

    update_status(&status, |current| {
        if current.state != RuntimeState::Failed {
            current.state = RuntimeState::Stopping;
        }
    });
    reject_pending(
        &receiver,
        RuntimeError::WorkerStopped,
        &writes,
        &pending_writes,
    );

    match engine.stop() {
        Ok(()) => update_status(&status, |current| {
            if current.state != RuntimeState::Failed {
                current.state = RuntimeState::Stopped;
            }
        }),
        Err(error) => update_status(&status, |current| {
            current.state = RuntimeState::Failed;
            current.last_error = Some(format!("engine shutdown failed: {error}"));
        }),
    }
}

struct PendingWrite {
    request_id: u64,
    values: BTreeMap<String, Value>,
    reply: Sender<Result<WriteReceipt, RuntimeError>>,
}

fn stop_requested(signal: &(Mutex<bool>, Condvar)) -> bool {
    *signal
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn request_stop(signal: &(Mutex<bool>, Condvar)) {
    let mut requested = signal
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *requested = true;
    signal.1.notify_all();
}

fn wait_until_deadline(deadline: Instant, signal: &(Mutex<bool>, Condvar)) -> bool {
    let mut requested = signal
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while !*requested {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let (guard, _) = signal
            .1
            .wait_timeout(requested, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        requested = guard;
    }
    true
}

fn reject_pending(
    receiver: &Receiver<Command>,
    error: RuntimeError,
    writes: &WriteRegistry,
    pending_writes: &AtomicUsize,
) {
    loop {
        match receiver.try_recv() {
            Ok(Command::Write {
                request_id, reply, ..
            }) => {
                pending_writes.fetch_sub(1, Ordering::SeqCst);
                writes.finish(request_id, Err(&error));
                let _ = reply.send(Err(error.clone()));
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn publish_snapshot(
    snapshot_lock: &RwLock<Snapshot>,
    status: &Mutex<RuntimeStatus>,
    feed: &SnapshotFeed,
    snapshot: &Snapshot,
) {
    *snapshot_lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot.clone();
    let revision = feed.publish();
    update_status(status, |current| {
        current.cycle_index = snapshot.cycle_index;
        current.simulation_time = snapshot.simulation_time;
        current.snapshot_revision = revision;
    });
}

fn record_execution_duration(status: &Mutex<RuntimeStatus>, elapsed: Duration, period: Duration) {
    update_status(status, |current| {
        current.last_execution_duration = elapsed;
        current.max_execution_duration = current.max_execution_duration.max(elapsed);
        if elapsed > period {
            current.overrun_count = current.overrun_count.saturating_add(1);
        }
    });
}

fn advance_deadline(
    previous: Instant,
    period: Duration,
    policy: OverrunPolicy,
    status: &Mutex<RuntimeStatus>,
) -> Instant {
    let next = previous.checked_add(period).unwrap_or_else(Instant::now);
    let now = Instant::now();
    if next > now {
        return next;
    }

    let period_nanos = period.as_nanos().max(1);
    let lateness_nanos = now.duration_since(next).as_nanos();
    let missed = lateness_nanos / period_nanos + 1;
    update_status(status, |current| {
        current.missed_deadlines = current
            .missed_deadlines
            .saturating_add(missed.min(u64::MAX as u128) as u64);
    });

    match policy {
        OverrunPolicy::Delay => now.checked_add(period).unwrap_or(now),
        OverrunPolicy::CatchUp => next,
    }
}

fn update_status<F>(status: &Mutex<RuntimeStatus>, update: F)
where
    F: FnOnce(&mut RuntimeStatus),
{
    let mut current = status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    update(&mut current);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[derive(Debug)]
    struct FakeState {
        cycle_time: f64,
        cycle_index: u64,
        values: BTreeMap<String, Value>,
        stopped: bool,
        fail_next_cycle: bool,
    }

    #[derive(Clone)]
    struct FakeEngine {
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeEngine {
        fn new(cycle_time: f64) -> (Self, Arc<Mutex<FakeState>>) {
            let state = Arc::new(Mutex::new(FakeState {
                cycle_time,
                cycle_index: 0,
                values: BTreeMap::from([("input".into(), Value::Float(1.0))]),
                stopped: false,
                fail_next_cycle: false,
            }));
            (
                Self {
                    state: Arc::clone(&state),
                },
                state,
            )
        }

        fn snapshot(state: &FakeState) -> Snapshot {
            Snapshot {
                cycle_index: state.cycle_index,
                simulation_time: state.cycle_index as f64 * state.cycle_time,
                values: state.values.clone(),
            }
        }
    }

    impl EngineExecutor for FakeEngine {
        fn cycle_time(&mut self) -> Result<f64, String> {
            Ok(self.state.lock().unwrap().cycle_time)
        }

        fn current_snapshot(&mut self) -> Result<Snapshot, String> {
            Ok(Self::snapshot(&self.state.lock().unwrap()))
        }

        fn execute_cycle(
            &mut self,
            writes: &BTreeMap<String, Value>,
        ) -> Result<Snapshot, EngineExecutionError> {
            let mut state = self.state.lock().unwrap();
            if state.fail_next_cycle {
                state.fail_next_cycle = false;
                return Err(EngineExecutionError::Fatal {
                    code: 999,
                    message: "injected failure".into(),
                });
            }
            if writes.contains_key("invalid") {
                return Err(EngineExecutionError::WriteRejected {
                    code: 1201,
                    message: "unknown tag: invalid".into(),
                });
            }
            for (name, value) in writes {
                state.values.insert(name.clone(), value.clone());
            }
            state.cycle_index += 1;
            Ok(Self::snapshot(&state))
        }

        fn stop(&mut self) -> Result<(), String> {
            self.state.lock().unwrap().stopped = true;
            Ok(())
        }
    }

    #[test]
    fn rejects_invalid_cycle_time_and_stops_engine() {
        let (engine, state) = FakeEngine::new(0.0);
        assert!(matches!(
            Runtime::start(engine),
            Err(RuntimeError::InvalidCycleTime)
        ));
        assert!(state.lock().unwrap().stopped);
    }

    #[test]
    fn publishes_cycles_and_stops_cleanly() {
        let (engine, state) = FakeEngine::new(0.01);
        let mut runtime = Runtime::start(engine).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.snapshot().cycle_index < 2 && Instant::now() < deadline {
            sleep(Duration::from_millis(5));
        }
        let snapshot = runtime.snapshot();
        assert!(snapshot.cycle_index >= 2);
        assert_eq!(runtime.status().state, RuntimeState::Running);
        runtime.stop().unwrap();
        runtime.stop().unwrap();
        assert_eq!(runtime.status().state, RuntimeState::Stopped);
        assert!(state.lock().unwrap().stopped);
    }

    #[test]
    fn acknowledges_a_write_at_the_cycle_where_it_takes_effect() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let ticket = runtime
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(8.5))]))
            .unwrap();
        let receipt = ticket.wait(Duration::from_secs(1)).unwrap();
        assert!(receipt.effective_cycle_index >= 1);
        assert_eq!(
            runtime.snapshot().values.get("input"),
            Some(&Value::Float(8.5))
        );
        drop(runtime);
    }

    #[test]
    fn rejected_batch_is_atomic_and_does_not_consume_the_tick() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let ticket = runtime
            .submit_writes(BTreeMap::from([
                ("input".into(), Value::Float(99.0)),
                ("invalid".into(), Value::Float(1.0)),
            ]))
            .unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(1)),
            Err(RuntimeError::WriteRejected { code: 1201, .. })
        ));
        assert_eq!(
            runtime.snapshot().values.get("input"),
            Some(&Value::Float(1.0))
        );
        assert!(runtime.snapshot().cycle_index >= 1);
        assert_eq!(runtime.status().rejected_write_batches, 1);
        drop(runtime);
    }

    #[test]
    fn empty_batches_are_rejected_before_queueing() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        assert!(matches!(
            runtime.submit_writes(BTreeMap::new()),
            Err(RuntimeError::EmptyWriteBatch)
        ));
        drop(runtime);
    }

    #[test]
    fn invalid_queue_capacity_stops_engine() {
        let (engine, state) = FakeEngine::new(0.01);
        let result = Runtime::start_with_config(
            engine,
            RuntimeConfig {
                overrun_policy: OverrunPolicy::Delay,
                write_queue_capacity: 0,
            },
        );
        assert!(matches!(result, Err(RuntimeError::InvalidQueueCapacity)));
        assert!(state.lock().unwrap().stopped);
    }

    #[test]
    fn bounded_queue_rejects_excess_batches_and_stop_wakes_the_worker() {
        let (engine, _) = FakeEngine::new(2.0);
        let mut runtime = Runtime::start_with_config(
            engine,
            RuntimeConfig {
                overrun_policy: OverrunPolicy::Delay,
                write_queue_capacity: 1,
            },
        )
        .unwrap();

        let ticket = runtime
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(2.0))]))
            .unwrap();
        assert!(matches!(
            runtime.submit_writes(BTreeMap::from([("input".into(), Value::Float(3.0))])),
            Err(RuntimeError::QueueFull)
        ));

        let started = Instant::now();
        runtime.stop().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            ticket.wait(Duration::from_millis(20)),
            Err(RuntimeError::WorkerStopped)
        ));
    }

    #[test]
    fn fatal_engine_error_moves_runtime_to_failed() {
        let (engine, state) = FakeEngine::new(0.01);
        state.lock().unwrap().fail_next_cycle = true;
        let mut runtime = Runtime::start(engine).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.status().state != RuntimeState::Failed && Instant::now() < deadline {
            sleep(Duration::from_millis(5));
        }

        assert_eq!(runtime.status().state, RuntimeState::Failed);
        assert!(runtime.stop().is_err());
        assert!(state.lock().unwrap().stopped);
    }

    #[test]
    fn handle_keeps_observing_after_owner_is_moved_away() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let handle = runtime.handle();
        let owner = thread::spawn(move || {
            sleep(Duration::from_millis(120));
            drop(runtime);
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while handle.snapshot().cycle_index < 3 && Instant::now() < deadline {
            sleep(Duration::from_millis(2));
        }
        assert!(handle.snapshot().cycle_index >= 3);
        owner.join().unwrap();

        assert_eq!(
            handle.wait_for_snapshot(handle.snapshot_revision(), Duration::from_secs(2)),
            WaitOutcome::Stopped
        );
        assert!(matches!(
            handle.submit_writes(BTreeMap::from([("input".into(), Value::Float(1.0))])),
            Err(RuntimeError::WorkerStopped)
        ));
    }

    #[test]
    fn snapshot_revision_wait_reports_changed_and_timeout() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let handle = runtime.handle();
        let before = handle.snapshot_revision();
        match handle.wait_for_snapshot(before, Duration::from_secs(2)) {
            WaitOutcome::Changed { revision } => assert!(revision > before),
            other => panic!("expected a new revision, got {other:?}"),
        }
        drop(runtime);

        // A slow engine publishes nothing within the wait window: Timeout.
        let (slow_engine, _) = FakeEngine::new(2.0);
        let slow_runtime = Runtime::start(slow_engine).unwrap();
        let slow_handle = slow_runtime.handle();
        assert_eq!(
            slow_handle
                .wait_for_snapshot(slow_handle.snapshot_revision(), Duration::from_millis(150)),
            WaitOutcome::Timeout
        );
        drop(slow_runtime);
    }

    #[test]
    fn write_lifecycle_cancelled_while_queued() {
        let (engine, _) = FakeEngine::new(2.0);
        let runtime = Runtime::start(engine).unwrap();
        let handle = runtime.handle();
        let ticket = handle
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(3.0))]))
            .unwrap();

        assert_eq!(
            handle.write_state(ticket.request_id()).unwrap().state,
            WriteState::Queued
        );
        assert!(handle.cancel_write(ticket.request_id()));
        assert_eq!(
            handle.write_state(ticket.request_id()).unwrap().state,
            WriteState::Cancelled
        );
        assert!(!handle.cancel_write(ticket.request_id()));
        assert!(matches!(
            ticket.wait(Duration::from_secs(3)),
            Err(RuntimeError::WriteCancelled)
        ));

        assert!(handle.write_state(9_999).is_none());
        assert!(!handle.cancel_write(9_999));
        drop(runtime);
    }

    #[test]
    fn write_lifecycle_applied_with_receipt() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let handle = runtime.handle();
        let ticket = handle
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(8.5))]))
            .unwrap();
        let receipt = ticket.wait(Duration::from_secs(1)).unwrap();

        let info = handle.write_state(ticket.request_id()).unwrap();
        assert_eq!(info.state, WriteState::Applied);
        assert_eq!(info.tag_count, 1);
        assert_eq!(info.error, None);
        assert_eq!(info.receipt, Some(receipt));
        assert!(matches!(
            handle.wait_for_snapshot(0, Duration::from_secs(1)),
            WaitOutcome::Changed { .. }
        ));
        drop(runtime);
    }

    #[test]
    fn write_lifecycle_rejected_records_the_error() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let handle = runtime.handle();
        let ticket = handle
            .submit_writes(BTreeMap::from([
                ("input".into(), Value::Float(99.0)),
                ("invalid".into(), Value::Float(1.0)),
            ]))
            .unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(1)),
            Err(RuntimeError::WriteRejected { code: 1201, .. })
        ));

        let info = handle.write_state(ticket.request_id()).unwrap();
        assert_eq!(info.state, WriteState::Rejected);
        assert!(info.error.as_deref().unwrap().contains("1201"));
        assert_eq!(info.receipt, None);
        drop(runtime);
    }

    #[test]
    fn queue_depth_tracks_pending_batches_and_stop_rejects_them() {
        let (engine, _) = FakeEngine::new(2.0);
        let mut runtime = Runtime::start_with_config(
            engine,
            RuntimeConfig {
                overrun_policy: OverrunPolicy::Delay,
                write_queue_capacity: 4,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let first = handle
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(2.0))]))
            .unwrap();
        let second = handle
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(3.0))]))
            .unwrap();

        assert_eq!(handle.queue_depth(), 2);
        assert_eq!(handle.status().queue_depth, 2);

        runtime.stop().unwrap();
        assert_eq!(handle.queue_depth(), 0);
        assert_eq!(handle.status().queue_depth, 0);
        assert_eq!(
            handle.write_state(first.request_id()).unwrap().state,
            WriteState::Rejected
        );
        assert_eq!(
            handle.write_state(second.request_id()).unwrap().state,
            WriteState::Rejected
        );
        assert!(matches!(
            first.wait(Duration::from_millis(20)),
            Err(RuntimeError::WorkerStopped)
        ));
    }

    #[test]
    fn queue_full_rejection_is_queryable() {
        let (engine, _) = FakeEngine::new(2.0);
        let runtime = Runtime::start_with_config(
            engine,
            RuntimeConfig {
                overrun_policy: OverrunPolicy::Delay,
                write_queue_capacity: 1,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let first = handle
            .submit_writes(BTreeMap::from([("input".into(), Value::Float(2.0))]))
            .unwrap();
        assert!(matches!(
            handle.submit_writes(BTreeMap::from([("input".into(), Value::Float(3.0))])),
            Err(RuntimeError::QueueFull)
        ));

        let info = handle.write_state(first.request_id() + 1).unwrap();
        assert_eq!(info.state, WriteState::Rejected);
        assert!(info.error.as_deref().unwrap().contains("full"));
        drop(runtime);
    }

    #[test]
    fn status_exposes_revision_start_time_and_lag() {
        let (engine, _) = FakeEngine::new(0.01);
        let runtime = Runtime::start(engine).unwrap();
        let handle = runtime.handle();
        let deadline = Instant::now() + Duration::from_secs(2);
        while handle.snapshot_revision() < 2 && Instant::now() < deadline {
            sleep(Duration::from_millis(2));
        }

        let status = handle.status();
        assert!(status.snapshot_revision >= 2);
        assert!(status.last_cycle_started_at.is_some());
        assert!(status.last_cycle_lag.is_some());
        assert_eq!(status.queue_depth, 0);
        drop(runtime);
    }

    #[test]
    fn registry_prunes_oldest_terminal_records() {
        let registry = WriteRegistry::new();
        let total = runtime_handle::WRITE_REGISTRY_CAPACITY + 50;
        for index in 0..total {
            let request_id = registry.register(1);
            registry.finish(
                request_id,
                Ok(&WriteReceipt {
                    effective_cycle_index: index as u64,
                    simulation_time: 0.0,
                }),
            );
        }

        // The oldest 50 terminal records were pruned, capacity is enforced.
        assert!(registry.status_of(1).is_none());
        assert!(registry.status_of(50).is_none());
        assert_eq!(registry.status_of(51).unwrap().state, WriteState::Applied);
        assert_eq!(
            registry.status_of(total as u64).unwrap().state,
            WriteState::Applied
        );
    }
}
