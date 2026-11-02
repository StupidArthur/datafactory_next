//! Multi-instance management (`doc/runtime-system-design.md` §5).
//!
//! The manager owns every [`Runtime`] (lifecycle owner) and hands out cloned,
//! read-only [`RuntimeHandle`]s. The instance registry lock is only held for
//! lookups and state bookkeeping — never while a model cycle executes and
//! never while a project loads, so adapters and instances stay isolated.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use crate::config::HostConfig;
use crate::instance::{Instance, InstanceState, InstanceSummary};
use crate::loader::load_engine;
use crate::{Runtime, RuntimeConfig, RuntimeError, RuntimeHandle};

/// Manages the lifecycle of all instances declared in one Host configuration.
///
/// All project and certificate paths arrive already resolved to absolute paths
/// by [`HostConfig::load`], so no base directory is retained.
pub struct RuntimeManager {
    instances: Mutex<BTreeMap<String, Instance>>,
}

impl RuntimeManager {
    /// Parse and validate a Host configuration file. Instances start out
    /// [`InstanceState::Configured`]; nothing is started automatically.
    pub fn load_config(path: &Path) -> Result<Self, RuntimeError> {
        Ok(Self::from_config(HostConfig::load(path)?))
    }

    /// Build a manager from an already-parsed configuration.
    pub fn from_config(config: HostConfig) -> Self {
        let instances = config
            .instances
            .into_iter()
            .map(|instance_config| (instance_config.id.clone(), Instance::new(instance_config)))
            .collect();
        Self {
            instances: Mutex::new(instances),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Instance>> {
        self.instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Start every enabled instance. Each instance loads its project and
    /// starts independently: a failing instance is marked [`InstanceState::Failed`]
    /// and reported, the rest keep starting (design §5.3).
    ///
    /// Instances that already run are skipped without error.
    pub fn start_all(&self) -> Vec<(String, RuntimeError)> {
        let ids: Vec<String> = {
            let instances = self.lock();
            instances
                .values()
                .filter(|instance| instance.config.enabled)
                .map(|instance| instance.config.id.clone())
                .collect()
        };
        let mut failures = Vec::new();
        for id in ids {
            if let Err(error) = self.start_instance(&id) {
                failures.push((id, error));
            }
        }
        failures
    }

    /// Start one instance from its original configuration. A failed load
    /// marks the instance [`InstanceState::Failed`] with the recorded error;
    /// other instances are unaffected.
    pub fn start_instance(&self, id: &str) -> Result<(), RuntimeError> {
        let unknown = || RuntimeError::Config(format!("unknown instance id {id:?}"));
        let (project, scheduler) = {
            let mut instances = self.lock();
            let instance = instances.get_mut(id).ok_or_else(unknown)?;
            match instance.state {
                InstanceState::Running | InstanceState::Starting => {
                    return Err(RuntimeError::Config(format!(
                        "instance {id:?} is already running"
                    )))
                }
                InstanceState::Stopping => {
                    return Err(RuntimeError::Config(format!(
                        "instance {id:?} is stopping; retry once it has stopped"
                    )))
                }
                InstanceState::Configured | InstanceState::Stopped | InstanceState::Failed => {}
            }
            // A previous (failed or stopped) runtime is released before a
            // fresh Engine is created; restarts never resume old state.
            if let Some(mut previous) = instance.runtime.take() {
                let _ = previous.stop();
            }
            instance.state = InstanceState::Starting;
            instance.error = None;
            (instance.config.project.clone(), instance.config.scheduler)
        };

        let start_result = load_engine(&project).and_then(|engine| {
            Runtime::start_with_config(
                engine,
                RuntimeConfig {
                    overrun_policy: scheduler.overrun_policy,
                    write_queue_capacity: scheduler.write_queue_capacity,
                },
            )
        });

        let mut instances = self.lock();
        let instance = instances.get_mut(id).ok_or_else(unknown)?;
        match start_result {
            Ok(runtime) => {
                instance.runtime = Some(runtime);
                instance.state = InstanceState::Running;
                instance.error = None;
                Ok(())
            }
            Err(error) => {
                instance.state = InstanceState::Failed;
                instance.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Stop one instance: reject its queued writes and release the Engine.
    /// Stopping a Configured/Stopped instance is a no-op; a Failed instance
    /// keeps its Failed state and recorded error.
    pub fn stop_instance(&self, id: &str) -> Result<(), RuntimeError> {
        let unknown = || RuntimeError::Config(format!("unknown instance id {id:?}"));
        let mut runtime = {
            let mut instances = self.lock();
            let instance = instances.get_mut(id).ok_or_else(unknown)?;
            match instance.state {
                InstanceState::Configured | InstanceState::Stopped => return Ok(()),
                InstanceState::Failed => {}
                _ => instance.state = InstanceState::Stopping,
            }
            instance.runtime.take()
        };

        let Some(runtime) = runtime.as_mut() else {
            return Ok(());
        };
        let stop_result = runtime.stop();

        let mut instances = self.lock();
        let instance = instances.get_mut(id).ok_or_else(unknown)?;
        match stop_result {
            Ok(()) => {
                instance.state = InstanceState::Stopped;
                instance.error = None;
            }
            Err(error) => {
                instance.state = InstanceState::Failed;
                instance.error = Some(error.to_string());
            }
        }
        Ok(())
    }

    /// Restart one instance from its original configuration: a fresh Engine
    /// with the YAML/dfproject initial values. Prior state is not resumed.
    pub fn restart_instance(&self, id: &str) -> Result<(), RuntimeError> {
        self.stop_instance(id)?;
        self.start_instance(id)
    }

    /// Return a read-only handle to a running instance. The handle cannot
    /// stop the instance; lifecycle ownership stays with the manager.
    pub fn get_instance(&self, id: &str) -> Result<RuntimeHandle, RuntimeError> {
        let unknown = || RuntimeError::Config(format!("unknown instance id {id:?}"));
        let instances = self.lock();
        let instance = instances.get(id).ok_or_else(unknown)?;
        instance
            .runtime
            .as_ref()
            .map(Runtime::handle)
            .ok_or_else(|| {
                RuntimeError::Config(format!(
                    "instance {id:?} is not running (state {:?})",
                    instance.state
                ))
            })
    }

    /// Summaries of all instances in id order, with state synced from the
    /// live runtimes.
    pub fn list_instances(&self) -> Vec<InstanceSummary> {
        let mut instances = self.lock();
        let mut summaries = Vec::with_capacity(instances.len());
        for instance in instances.values_mut() {
            instance.sync_state();
            summaries.push(instance.summary());
        }
        summaries
    }

    /// Stop all instances in id order and return any stop failures. Safe to
    /// call repeatedly.
    pub fn shutdown(&self) -> Vec<(String, RuntimeError)> {
        let ids: Vec<String> = self.lock().keys().cloned().collect();
        let mut failures = Vec::new();
        for id in ids {
            if let Err(error) = self.stop_instance(&id) {
                failures.push((id, error));
            }
        }
        failures
    }
}

impl Drop for RuntimeManager {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use crate::{Value, WaitOutcome, WriteState};

    /// Minimal valid DSL v2 project, embedded from the two-tank example.
    const TWO_TANK_YAML: &str = include_str!("../../../examples/two-tank/project.yaml");
    const BROKEN_YAML: &str = "dsl_version: 2\nmetadata: {name: broken\n";

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "df_runtime_manager_{}_{name}_{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A four-instance host: tank-a (delay), tank-b (catch_up), broken
    /// (invalid project), off (disabled).
    fn setup(name: &str) -> (RuntimeManager, PathBuf) {
        let root = temp_root(name);
        fs::write(root.join("two_tank.yaml"), TWO_TANK_YAML).unwrap();
        fs::write(root.join("broken.yaml"), BROKEN_YAML).unwrap();
        let config_text = "runtime_version: 1\n\
            instances:\n  \
            - id: tank-a\n    \
              project: ./two_tank.yaml\n  \
            - id: tank-b\n    \
              project: ./two_tank.yaml\n    \
              scheduler:\n      \
                overrun_policy: catch_up\n      \
                write_queue_capacity: 8\n  \
            - id: broken\n    \
              project: ./broken.yaml\n  \
            - id: off\n    \
              project: ./two_tank.yaml\n    \
              enabled: false\n";
        let config = HostConfig::parse(config_text, &root).unwrap();
        (RuntimeManager::from_config(config), root)
    }

    fn wait_until(predicate: impl Fn() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    fn summary<'a>(manager: &'a RuntimeManager, id: &str) -> InstanceSummary {
        manager
            .list_instances()
            .into_iter()
            .find(|summary| summary.id == id)
            .unwrap_or_else(|| panic!("instance {id} missing"))
    }

    #[test]
    fn start_all_isolates_failures_and_skips_disabled() {
        let (manager, _root) = setup("start_all");
        let failures = manager.start_all();

        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].0, "broken");
        assert!(
            failures[0].1.to_string().contains("cannot compile"),
            "{}",
            failures[0].1
        );

        assert_eq!(summary(&manager, "tank-a").state, InstanceState::Running);
        assert_eq!(summary(&manager, "tank-b").state, InstanceState::Running);
        let broken = summary(&manager, "broken");
        assert_eq!(broken.state, InstanceState::Failed);
        assert!(broken.error.as_deref().unwrap().contains("cannot compile"));
        assert_eq!(summary(&manager, "off").state, InstanceState::Configured);

        manager.shutdown();
    }

    #[test]
    fn instances_keep_independent_engines_and_schedulers() {
        let (manager, _root) = setup("isolation");
        manager.start_all();

        let handle_a = manager.get_instance("tank-a").unwrap();
        let handle_b = manager.get_instance("tank-b").unwrap();

        // Both instances advance independently from the same project file.
        assert!(wait_until(
            || handle_a.snapshot().cycle_index >= 2 && handle_b.snapshot().cycle_index >= 2,
            Duration::from_secs(4)
        ));

        // Stopping tank-a must not affect tank-b.
        let b_before = handle_b.snapshot().cycle_index;
        manager.stop_instance("tank-a").unwrap();
        assert_eq!(summary(&manager, "tank-a").state, InstanceState::Stopped);
        assert!(wait_until(
            || handle_b.snapshot().cycle_index > b_before,
            Duration::from_secs(4)
        ));
        assert_eq!(summary(&manager, "tank-a").state, InstanceState::Stopped);

        // Writes stay scoped to their instance: tank-a is stopped, so its
        // handle rejects submissions while tank-b keeps accepting them.
        assert!(matches!(
            handle_a.submit_writes(BTreeMap::from([(
                "plant_inlet_flow".into(),
                Value::Float(0.5)
            )])),
            Err(RuntimeError::WorkerStopped)
        ));
        let ticket = handle_b
            .submit_writes(BTreeMap::from([(
                "plant_inlet_flow".into(),
                Value::Float(0.5),
            )]))
            .unwrap();
        assert!(ticket.wait(Duration::from_secs(3)).is_ok());
        assert_eq!(
            handle_b.write_state(ticket.request_id()).unwrap().state,
            WriteState::Applied
        );

        manager.shutdown();
    }

    #[test]
    fn restart_resets_the_engine_to_initial_values() {
        let (manager, _root) = setup("restart");
        manager.start_instance("tank-a").unwrap();
        let handle = manager.get_instance("tank-a").unwrap();

        assert!(wait_until(
            || handle.snapshot().cycle_index >= 2,
            Duration::from_secs(4)
        ));
        let level_before = match handle.snapshot().values.get("tank_1.level") {
            Some(Value::Float(level)) => *level,
            other => panic!("unexpected tank_1.level: {other:?}"),
        };
        assert!(
            level_before > 0.155,
            "level should have advanced: {level_before}"
        );

        manager.restart_instance("tank-a").unwrap();
        let fresh = manager.get_instance("tank-a").unwrap();
        assert!(wait_until(
            || fresh.snapshot().cycle_index >= 1,
            Duration::from_secs(4)
        ));
        // The fresh engine restarted the simulation timeline from zero: at
        // most one 0.5s cycle has run, so the level is back near the initial
        // 0.15 instead of continuing from the pre-restart value.
        assert!(fresh.snapshot().cycle_index <= 1);
        let level_after = match fresh.snapshot().values.get("tank_1.level") {
            Some(Value::Float(level)) => *level,
            other => panic!("unexpected tank_1.level: {other:?}"),
        };
        assert!(
            (0.15..0.157).contains(&level_after),
            "level should be reset to the initial value region, got {level_after}"
        );

        manager.shutdown();
    }

    #[test]
    fn double_start_is_rejected_and_double_stop_is_idempotent() {
        let (manager, _root) = setup("double");
        manager.start_instance("tank-a").unwrap();
        let error = manager.start_instance("tank-a").unwrap_err();
        assert!(error.to_string().contains("already running"), "{error}");

        manager.stop_instance("tank-a").unwrap();
        manager.stop_instance("tank-a").unwrap();
        assert_eq!(summary(&manager, "tank-a").state, InstanceState::Stopped);

        // Restart works after a clean stop.
        manager.restart_instance("tank-a").unwrap();
        assert_eq!(summary(&manager, "tank-a").state, InstanceState::Running);
        manager.shutdown();
    }

    #[test]
    fn unknown_instance_ids_are_reported() {
        let (manager, _root) = setup("unknown");
        for result in [
            manager.start_instance("nope").map(|_| ()),
            manager.stop_instance("nope").map(|_| ()),
            manager.get_instance("nope").map(|_| ()),
        ] {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("unknown instance id"), "{error}");
        }
    }

    #[test]
    fn shutdown_stops_everything_and_handles_report_stopped() {
        let (manager, _root) = setup("shutdown");
        manager.start_all();
        let handle_a = manager.get_instance("tank-a").unwrap();

        assert!(manager.shutdown().is_empty());
        assert_eq!(summary(&manager, "tank-a").state, InstanceState::Stopped);
        assert_eq!(summary(&manager, "tank-b").state, InstanceState::Stopped);
        assert_eq!(summary(&manager, "broken").state, InstanceState::Failed);
        // A disabled instance was never started and stays Configured.
        assert_eq!(summary(&manager, "off").state, InstanceState::Configured);

        // Old handles observe the stop and reject new writes.
        assert_eq!(
            handle_a.wait_for_snapshot(handle_a.snapshot_revision(), Duration::from_secs(2)),
            WaitOutcome::Stopped
        );
        assert!(matches!(
            handle_a.submit_writes(BTreeMap::from([(
                "plant_inlet_flow".into(),
                Value::Float(0.5)
            )])),
            Err(RuntimeError::WorkerStopped)
        ));

        // Shutdown is repeatable.
        assert!(manager.shutdown().is_empty());
    }

    #[test]
    fn summaries_expose_live_metrics() {
        let (manager, _root) = setup("metrics");
        manager.start_instance("tank-a").unwrap();
        let handle = manager.get_instance("tank-a").unwrap();

        assert!(wait_until(
            || handle.snapshot_revision() >= 2,
            Duration::from_secs(4)
        ));
        let entry = summary(&manager, "tank-a");
        assert_eq!(entry.state, InstanceState::Running);
        assert!(entry.enabled);
        assert!(entry.cycle_index >= 2);
        assert!(entry.snapshot_revision >= 2);
        assert!(entry.simulation_time > 0.0);
        assert_eq!(entry.error, None);
        assert!(entry.project.ends_with("two_tank.yaml"));

        manager.shutdown();
    }

    #[test]
    fn manager_loads_the_shipped_example_host_config() {
        let config_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/host/runtime.yaml");
        let manager = RuntimeManager::load_config(&config_path).unwrap();
        assert_eq!(manager.list_instances().len(), 2);
        assert!(manager.start_all().is_empty());
        assert_eq!(
            manager
                .get_instance("tank-a")
                .unwrap()
                .snapshot()
                .cycle_index,
            0
        );
        assert!(manager.shutdown().is_empty());
    }

    #[test]
    fn manager_config_load_errors_are_reported() {
        let root = temp_root("bad_manager_config");
        let error = match RuntimeManager::load_config(&root.join("missing.yaml")) {
            Ok(_) => panic!("expected the config load to fail"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("cannot read host config"),
            "{error}"
        );
    }
}
