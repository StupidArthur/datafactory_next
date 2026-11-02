//! Instance lifecycle state and bookkeeping (`doc/runtime-system-design.md` §5.4).

use std::path::PathBuf;

use crate::{InstanceConfig, Runtime, RuntimeState, RuntimeStatus};

/// Instance lifecycle states, tracked by the manager on top of the runtime's
/// own scheduler state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    /// Configuration parsed; the instance has not been started.
    Configured,
    /// Project is being loaded and the Engine initialized.
    Starting,
    /// The scheduler is running normally.
    Running,
    /// A stop request was issued; resources are being released.
    Stopping,
    /// The instance stopped; pending writes were rejected.
    Stopped,
    /// The instance hit an unrecoverable error and cannot advance cycles.
    Failed,
}

/// Read-only summary of one managed instance (`list_instances`).
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSummary {
    pub id: String,
    pub enabled: bool,
    pub state: InstanceState,
    pub project: PathBuf,
    pub cycle_index: u64,
    pub simulation_time: f64,
    pub snapshot_revision: u64,
    pub queue_depth: usize,
    pub overrun_count: u64,
    pub missed_deadlines: u64,
    /// Present for load failures and runtime fatal errors.
    pub error: Option<String>,
}

/// One managed instance: its original configuration plus the owned Runtime.
///
/// The manager owns the [`Runtime`] (lifecycle owner); adapters only receive
/// cloned [`crate::RuntimeHandle`]s, so the registry lock is never held while
/// a model cycle executes.
pub(crate) struct Instance {
    pub config: InstanceConfig,
    pub state: InstanceState,
    pub runtime: Option<Runtime>,
    pub error: Option<String>,
}

impl Instance {
    pub(crate) fn new(config: InstanceConfig) -> Self {
        Self {
            config,
            state: InstanceState::Configured,
            runtime: None,
            error: None,
        }
    }

    /// Refresh lifecycle state from the live runtime status. A manager-level
    /// `Failed` (load error) is never overwritten by runtime observation.
    pub(crate) fn sync_state(&mut self) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let status: RuntimeStatus = runtime.status();
        match status.state {
            RuntimeState::Starting | RuntimeState::Running => {
                if self.state != InstanceState::Failed {
                    self.state = InstanceState::Running;
                }
            }
            RuntimeState::Stopping => {
                if self.state != InstanceState::Failed {
                    self.state = InstanceState::Stopping;
                }
            }
            RuntimeState::Stopped => {
                if self.state != InstanceState::Failed {
                    self.state = InstanceState::Stopped;
                }
            }
            RuntimeState::Failed => {
                self.state = InstanceState::Failed;
                self.error = Some(
                    status
                        .last_error
                        .unwrap_or_else(|| "runtime entered failed state".into()),
                );
            }
        }
    }

    pub(crate) fn summary(&self) -> InstanceSummary {
        let (
            cycle_index,
            simulation_time,
            snapshot_revision,
            queue_depth,
            overrun_count,
            missed_deadlines,
        ) = match &self.runtime {
            Some(runtime) => {
                let status = runtime.status();
                (
                    status.cycle_index,
                    status.simulation_time,
                    status.snapshot_revision,
                    status.queue_depth,
                    status.overrun_count,
                    status.missed_deadlines,
                )
            }
            None => (0, 0.0, 0, 0, 0, 0),
        };
        InstanceSummary {
            id: self.config.id.clone(),
            enabled: self.config.enabled,
            state: self.state,
            project: self.config.project.clone(),
            cycle_index,
            simulation_time,
            snapshot_revision,
            queue_depth,
            overrun_count,
            missed_deadlines,
            error: self.error.clone(),
        }
    }
}
