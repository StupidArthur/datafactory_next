//! Snapshot publisher (design §7.5).
//!
//! One thread per attached instance blocks on the instance's snapshot feed
//! and batch-writes the new snapshot values plus the instance status nodes
//! into the address space with `InMemoryNodeManager::set_values` (a single
//! address-space write lock per cycle, so clients never observe a torn mix
//! of two cycles). A shared thread keeps the server status folder honest.
//!
//! Slow consumers can never block the engine: the threads only read
//! published snapshots and the UA subscription machinery owns notification
//! queuing and filtering.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use opcua::server::SubscriptionCache;
use opcua::types::{DataValue, DateTime, NodeId, StatusCode, UAString, Variant};

use crate::{RuntimeError, RuntimeHandle, RuntimeState, RuntimeStatus, WaitOutcome};

use super::address_space::InstanceNodes;
use super::value::value_to_variant;
use super::write::RuntimeNodeManager;
use super::OpcUaServer;

/// How often the publisher re-checks instance status between snapshot
/// notifications and refreshes the server status folder.
pub const DEFAULT_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(200);

/// String exposed by the `State` status variables (§10.1 lifecycle names).
fn state_string(state: RuntimeState) -> &'static str {
    match state {
        RuntimeState::Starting => "Starting",
        RuntimeState::Running => "Running",
        RuntimeState::Stopping => "Stopping",
        RuntimeState::Stopped => "Stopped",
        RuntimeState::Failed => "Failed",
    }
}

/// Base status code of every tag of an instance (§7.5): only the instance
/// lifecycle decides; there is deliberately no per-tag quality yet.
fn tag_status_code(state: RuntimeState) -> StatusCode {
    match state {
        RuntimeState::Starting => StatusCode::BadWaitingForInitialData,
        RuntimeState::Running | RuntimeState::Stopping => StatusCode::Good,
        RuntimeState::Stopped | RuntimeState::Failed => StatusCode::BadOutOfService,
    }
}

/// Aggregated host state for the `ServerStatus/State` variable.
fn host_state_string(states: &[RuntimeState]) -> &'static str {
    if states.iter().any(|s| *s == RuntimeState::Failed) {
        "Failed"
    } else if !states.is_empty()
        && states
            .iter()
            .all(|s| matches!(s, RuntimeState::Stopped | RuntimeState::Failed))
    {
        "Stopped"
    } else if states.iter().any(|s| *s == RuntimeState::Running) {
        "Running"
    } else if states.iter().any(|s| *s == RuntimeState::Stopping) {
        "Stopping"
    } else if states.is_empty() {
        "Stopped"
    } else {
        "Starting"
    }
}

/// The instance status variable values for one status sample (§7.2 names).
fn status_values(status: &RuntimeStatus) -> Vec<(&'static str, Variant)> {
    vec![
        (
            "State",
            Variant::String(UAString::from(state_string(status.state))),
        ),
        ("CycleTime", Variant::Double(status.cycle_time)),
        ("CycleIndex", Variant::UInt64(status.cycle_index)),
        ("SimulationTime", Variant::Double(status.simulation_time)),
        (
            "LastCycleDurationMs",
            Variant::Double(status.last_execution_duration.as_secs_f64() * 1000.0),
        ),
        ("OverrunCount", Variant::UInt64(status.overrun_count)),
        ("MissedDeadlines", Variant::UInt64(status.missed_deadlines)),
        (
            "RejectedWriteBatches",
            Variant::UInt64(status.rejected_write_batches),
        ),
    ]
}

fn data_value(variant: Variant, status: StatusCode, now: DateTime) -> DataValue {
    DataValue {
        value: Some(variant),
        status: Some(status),
        source_timestamp: Some(now),
        ..Default::default()
    }
}

/// Bounded re-sync for the final publish: the feed is marked stopped only
/// after the lifecycle status reached a terminal state (`Runtime::stop` and
/// the fatal worker path both keep this order), so the first `status()` read
/// is already terminal in practice. The wait is a defensive guarantee that
/// the out-of-service write always carries the terminal state even if a
/// future stop path violates that ordering.
fn wait_for_terminal_status(handle: &RuntimeHandle, limit: Duration) -> RuntimeStatus {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let status = handle.status();
        if matches!(status.state, RuntimeState::Stopped | RuntimeState::Failed)
            || std::time::Instant::now() >= deadline
        {
            return status;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Batch-write one instance's tag values and status nodes. All nodes go into
/// a single `set_values` call: one address-space write lock, so a client
/// reading several tags mid-publish sees one consistent cycle (§14.4 UA-08).
fn publish_instance(
    node_manager: &RuntimeNodeManager,
    subscriptions: &SubscriptionCache,
    nodes: &InstanceNodes,
    handle: &RuntimeHandle,
    status: &RuntimeStatus,
) {
    let now = DateTime::now();
    let code = tag_status_code(status.state);
    let snapshot = handle.snapshot();

    let mut batch: Vec<(NodeId, DataValue)> = Vec::with_capacity(nodes.tags.len() + 8);
    for (tag_name, entry) in &nodes.tags {
        if let Some(value) = snapshot.values.get(tag_name) {
            batch.push((
                entry.node_id.clone(),
                data_value(value_to_variant(value), code, now),
            ));
        }
    }
    for (name, variant) in status_values(status) {
        if let Some(node_id) = nodes.status.get(name) {
            batch.push((node_id.clone(), data_value(variant, StatusCode::Good, now)));
        }
    }

    // Unknown nodes cannot happen: the index was built from the address
    // space. A failing write here would mean a broken invariant, so the
    // error is swallowed only as a last resort to keep the thread alive.
    let _ = node_manager.set_values(
        subscriptions,
        batch.iter().map(|(id, value)| (id, None, value.clone())),
    );
}

/// Per-instance publisher loop: publish on every snapshot revision, refresh
/// status between revisions, and mark everything out of service once the
/// runtime stops (§7.5).
fn instance_publisher_loop(
    stop: Arc<AtomicBool>,
    refresh: Duration,
    node_manager: Arc<RuntimeNodeManager>,
    subscriptions: Arc<SubscriptionCache>,
    nodes: InstanceNodes,
    handle: RuntimeHandle,
) {
    let mut last_revision = handle.snapshot_revision();

    // Publish the current snapshot immediately so the address space carries
    // real values (flagged Starting/BadWaitingForInitialData if the runtime
    // has not completed a cycle yet).
    let initial = handle.status();
    publish_instance(&node_manager, &subscriptions, &nodes, &handle, &initial);
    let mut last_status: Option<RuntimeStatus> = Some(initial);

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match handle.wait_for_snapshot(last_revision, refresh) {
            WaitOutcome::Changed { revision } => {
                last_revision = revision;
                let status = handle.status();
                publish_instance(&node_manager, &subscriptions, &nodes, &handle, &status);
                last_status = Some(status);
            }
            WaitOutcome::Timeout => {
                let status = handle.status();
                if last_status.as_ref() != Some(&status) {
                    publish_instance(&node_manager, &subscriptions, &nodes, &handle, &status);
                    last_status = Some(status);
                }
            }
            WaitOutcome::Stopped => {
                // Final write: the terminal status (Stopped/Failed) puts the
                // tags out of service and the State variable carries the
                // lifecycle name (§7.5).
                let status = wait_for_terminal_status(&handle, Duration::from_secs(5));
                publish_instance(&node_manager, &subscriptions, &nodes, &handle, &status);
                return;
            }
        }
    }
}

/// Shared server status loop: aggregates the instance states into the
/// `ServerStatus` folder (§7.2) and refreshes it while anything can change.
fn server_status_loop(
    stop: Arc<AtomicBool>,
    refresh: Duration,
    node_manager: Arc<RuntimeNodeManager>,
    subscriptions: Arc<SubscriptionCache>,
    server_status: std::collections::BTreeMap<String, NodeId>,
    handles: Vec<RuntimeHandle>,
) {
    let mut last: Option<(&'static str, u32, u32)> = None;
    loop {
        std::thread::sleep(refresh);
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let states: Vec<RuntimeState> = handles.iter().map(|h| h.status().state).collect();
        let running = states
            .iter()
            .filter(|s| **s == RuntimeState::Running)
            .count() as u32;
        let sample = (host_state_string(&states), handles.len() as u32, running);
        if last.as_ref() == Some(&sample) {
            continue;
        }
        let now = DateTime::now();
        let batch: Vec<(NodeId, DataValue)> = [
            ("State", Variant::String(UAString::from(sample.0))),
            ("InstanceCount", Variant::UInt32(sample.1)),
            ("RunningInstanceCount", Variant::UInt32(sample.2)),
        ]
        .into_iter()
        .filter_map(|(name, variant)| {
            server_status
                .get(name)
                .map(|id| (id.clone(), data_value(variant, StatusCode::Good, now)))
        })
        .collect();
        let _ = node_manager.set_values(
            &subscriptions,
            batch.iter().map(|(id, value)| (id, None, value.clone())),
        );
        last = Some(sample);
    }
}

/// Publishes engine snapshots and runtime status into the address space of a
/// running [`OpcUaServer`] (§7.5).
///
/// The publisher never blocks the engine: it only observes published
/// snapshots via [`RuntimeHandle`] and writes into the address space, whose
/// notification policy belongs to the server.
pub struct SnapshotPublisher {
    stop: Arc<AtomicBool>,
    instance_threads: Vec<JoinHandle<()>>,
    server_thread: Option<JoinHandle<()>>,
}

impl SnapshotPublisher {
    /// Attach the given `(instance_id, handle)` pairs to the server's address
    /// space and start publishing. Instance ids must exist in the address
    /// space index built from the Tag Catalogs.
    pub fn start(
        server: &OpcUaServer,
        instances: &[(String, RuntimeHandle)],
    ) -> Result<Self, RuntimeError> {
        Self::start_with_refresh(server, instances, DEFAULT_STATUS_REFRESH_INTERVAL)
    }

    /// [`Self::start`] with an explicit status refresh interval (tests).
    pub fn start_with_refresh(
        server: &OpcUaServer,
        instances: &[(String, RuntimeHandle)],
        refresh: Duration,
    ) -> Result<Self, RuntimeError> {
        let index = server.address_space();
        let stop = Arc::new(AtomicBool::new(false));
        let mut instance_threads = Vec::with_capacity(instances.len());

        let mut seen = std::collections::BTreeSet::new();
        for (instance_id, handle) in instances {
            if !seen.insert(instance_id.as_str()) {
                return Err(RuntimeError::Config(format!(
                    "duplicate opcua publisher instance {instance_id}"
                )));
            }
            let Some(nodes) = index.instances.get(instance_id).cloned() else {
                return Err(RuntimeError::Config(format!(
                    "unknown opcua publisher instance {instance_id}: \
                     the address space was built without its catalog"
                )));
            };
            let thread = std::thread::Builder::new()
                .name(format!("df-opcua-publisher-{instance_id}"))
                .spawn({
                    let stop = Arc::clone(&stop);
                    let node_manager = Arc::clone(server.node_manager());
                    let subscriptions = Arc::clone(server.subscriptions());
                    let handle = handle.clone();
                    move || {
                        instance_publisher_loop(
                            stop,
                            refresh,
                            node_manager,
                            subscriptions,
                            nodes,
                            handle,
                        );
                    }
                })
                .map_err(|error| {
                    RuntimeError::Config(format!("cannot spawn the opcua publisher: {error}"))
                })?;
            instance_threads.push(thread);
        }

        let server_thread = std::thread::Builder::new()
            .name("df-opcua-server-status".into())
            .spawn({
                let stop = Arc::clone(&stop);
                let node_manager = Arc::clone(server.node_manager());
                let subscriptions = Arc::clone(server.subscriptions());
                let server_status = index.server_status.clone();
                let handles = instances.iter().map(|(_, h)| h.clone()).collect();
                move || {
                    server_status_loop(
                        stop,
                        refresh,
                        node_manager,
                        subscriptions,
                        server_status,
                        handles,
                    );
                }
            })
            .map_err(|error| {
                RuntimeError::Config(format!("cannot spawn the opcua server status: {error}"))
            })?;

        Ok(Self {
            stop,
            instance_threads,
            server_thread: Some(server_thread),
        })
    }

    /// Stop all publisher threads. Idempotent. The server itself is not
    /// touched; stopping the server stops the address space anyway.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.instance_threads.drain(..) {
            let _ = thread.join();
        }
        if let Some(thread) = self.server_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SnapshotPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn set(states: &[RuntimeState]) -> Vec<RuntimeState> {
        states.to_vec()
    }

    #[test]
    fn state_strings_follow_the_lifecycle_names() {
        assert_eq!(state_string(RuntimeState::Starting), "Starting");
        assert_eq!(state_string(RuntimeState::Running), "Running");
        assert_eq!(state_string(RuntimeState::Stopping), "Stopping");
        assert_eq!(state_string(RuntimeState::Stopped), "Stopped");
        assert_eq!(state_string(RuntimeState::Failed), "Failed");
    }

    #[test]
    fn tag_status_codes_follow_section_7_5() {
        assert_eq!(
            tag_status_code(RuntimeState::Starting),
            StatusCode::BadWaitingForInitialData
        );
        assert_eq!(tag_status_code(RuntimeState::Running), StatusCode::Good);
        assert_eq!(tag_status_code(RuntimeState::Stopping), StatusCode::Good);
        assert_eq!(
            tag_status_code(RuntimeState::Stopped),
            StatusCode::BadOutOfService
        );
        assert_eq!(
            tag_status_code(RuntimeState::Failed),
            StatusCode::BadOutOfService
        );
    }

    #[test]
    fn host_state_aggregates_instance_states() {
        assert_eq!(host_state_string(&set(&[])), "Stopped");
        assert_eq!(
            host_state_string(&set(&[RuntimeState::Starting])),
            "Starting"
        );
        assert_eq!(
            host_state_string(&set(&[RuntimeState::Starting, RuntimeState::Starting])),
            "Starting"
        );
        assert_eq!(host_state_string(&set(&[RuntimeState::Running])), "Running");
        assert_eq!(
            host_state_string(&set(&[RuntimeState::Starting, RuntimeState::Running])),
            "Running"
        );
        assert_eq!(
            host_state_string(&set(&[RuntimeState::Stopping, RuntimeState::Running])),
            "Running"
        );
        assert_eq!(
            host_state_string(&set(&[RuntimeState::Stopping])),
            "Stopping"
        );
        assert_eq!(host_state_string(&set(&[RuntimeState::Stopped])), "Stopped");
        assert_eq!(host_state_string(&set(&[RuntimeState::Failed])), "Failed");
        assert_eq!(
            host_state_string(&set(&[RuntimeState::Stopped, RuntimeState::Failed])),
            "Failed"
        );
    }

    #[test]
    fn status_values_use_the_section_7_2_names_and_types() {
        let status = RuntimeStatus {
            state: RuntimeState::Running,
            cycle_time: 0.5,
            cycle_index: 7,
            simulation_time: 3.5,
            last_execution_duration: Duration::from_micros(250),
            max_execution_duration: Duration::from_micros(900),
            overrun_count: 1,
            missed_deadlines: 2,
            rejected_write_batches: 3,
            last_write_error: None,
            last_error: None,
            snapshot_revision: 7,
            queue_depth: 0,
            last_cycle_started_at: None,
            last_cycle_lag: None,
        };
        let values = status_values(&status);
        let get = |name: &str| {
            values
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("State"), Variant::String(UAString::from("Running")));
        assert_eq!(get("CycleTime"), Variant::Double(0.5));
        assert_eq!(get("CycleIndex"), Variant::UInt64(7));
        assert_eq!(get("SimulationTime"), Variant::Double(3.5));
        assert_eq!(get("LastCycleDurationMs"), Variant::Double(0.25));
        assert_eq!(get("OverrunCount"), Variant::UInt64(1));
        assert_eq!(get("MissedDeadlines"), Variant::UInt64(2));
        assert_eq!(get("RejectedWriteBatches"), Variant::UInt64(3));
    }
}
