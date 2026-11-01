use df_core::{Engine, EngineError, Snapshot, Value};
use std::path::Path;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError,
};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_WRITE_QUEUE_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    WorkerPanicked,
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
            Self::WorkerPanicked => write!(f, "runtime worker panicked"),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineExecutionError {
    WriteRejected { code: i32, message: String },
    Fatal { code: i32, message: String },
}

/// Adapter boundary between the realtime scheduler and the model execution process.
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
        values: BTreeMap<String, Value>,
        reply: Sender<Result<WriteReceipt, RuntimeError>>,
    },
    Wake,
}

struct PendingWrite {
    values: BTreeMap<String, Value>,
    reply: Sender<Result<WriteReceipt, RuntimeError>>,
}

/// A handle to one realtime model instance.
///
/// The scheduler owns one Engine process and advances it by one cycle per
/// wall-clock deadline. Write batches are FIFO and applied atomically at a
/// cycle boundary. The latest immutable snapshot is available to data adapters.
pub struct Runtime {
    sender: SyncSender<Command>,
    status: Arc<Mutex<RuntimeStatus>>,
    snapshot: Arc<RwLock<Snapshot>>,
    stop_requested: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn load(project_path: &Path) -> Result<Self, RuntimeError> {
        let engine = Engine::load(project_path)
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        Self::start(engine)
    }

    pub fn start(engine: Engine) -> Result<Self, RuntimeError> {
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
        }));
        let snapshot = Arc::new(RwLock::new(initial_snapshot));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(config.write_queue_capacity);

        let worker_status = Arc::clone(&status);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_stop_requested = Arc::clone(&stop_requested);
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
                    worker_stop_requested,
                )
            })
            .map_err(|error| RuntimeError::Engine(format!("cannot start worker: {error}")))?;

        Ok(Self {
            sender,
            status,
            snapshot,
            stop_requested,
            worker: Some(worker),
        })
    }

    pub fn status(&self) -> RuntimeStatus {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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
        if writes.is_empty() {
            return Err(RuntimeError::EmptyWriteBatch);
        }
        if self.stop_requested.load(Ordering::Acquire) {
            return Err(RuntimeError::WorkerStopped);
        }

        let (reply, receiver) = mpsc::channel();
        match self.sender.try_send(Command::Write {
            values: writes,
            reply,
        }) {
            Ok(()) => Ok(WriteTicket { receiver }),
            Err(TrySendError::Full(_)) => Err(RuntimeError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(RuntimeError::WorkerStopped),
        }
    }

    /// Stop the scheduler and the owned Engine process. Safe to call repeatedly.
    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if let Some(worker) = self.worker.take() {
            self.stop_requested.store(true, Ordering::Release);
            // A blocking send wakes a worker waiting for its next deadline.
            // It can wait only until the worker consumes one queued command.
            let _ = self.sender.send(Command::Wake);
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
    receiver: Receiver<Result<WriteReceipt, RuntimeError>>,
}

impl WriteTicket {
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
    stop_requested: Arc<AtomicBool>,
) {
    update_status(&status, |current| current.state = RuntimeState::Running);
    let mut next_deadline = Instant::now()
        .checked_add(period)
        .unwrap_or_else(Instant::now);
    let mut pending = VecDeque::<PendingWrite>::new();

    loop {
        if stop_requested.load(Ordering::Acquire) {
            break;
        }

        let timeout = next_deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(command) => {
                enqueue(command, &mut pending);
                if stop_requested.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                loop {
                    match receiver.try_recv() {
                        Ok(command) => enqueue(command, &mut pending),
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }

                if stop_requested.load(Ordering::Acquire) {
                    break;
                }

                let active = pending.pop_front();
                let writes = active
                    .as_ref()
                    .map(|item| item.values.clone())
                    .unwrap_or_default();
                let started = Instant::now();

                let first_result = engine.execute_cycle(&writes);
                let cycle_result = match first_result {
                    Ok(snapshot) => Ok((snapshot, None)),
                    Err(EngineExecutionError::WriteRejected { code, message })
                        if !writes.is_empty() =>
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
                        publish_snapshot(&snapshot, &status, &new_snapshot);
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
                                    Err(error)
                                }
                                None => {
                                    update_status(&status, |current| {
                                        current.last_write_error = None;
                                    });
                                    Ok(WriteReceipt {
                                        effective_cycle_index: new_snapshot.cycle_index,
                                        simulation_time: new_snapshot.simulation_time,
                                    })
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
                            let _ = item
                                .reply
                                .send(Err(RuntimeError::Engine(message.clone())));
                        }
                        update_status(&status, |current| {
                            current.state = RuntimeState::Failed;
                            current.last_error = Some(message.clone());
                        });
                        reject_pending(
                            &mut pending,
                            &receiver,
                            RuntimeError::Engine(message.clone()),
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

                next_deadline =
                    advance_deadline(next_deadline, period, config.overrun_policy, &status);
            }
        }
    }

    update_status(&status, |current| {
        if current.state != RuntimeState::Failed {
            current.state = RuntimeState::Stopping;
        }
    });
    reject_pending(&mut pending, &receiver, RuntimeError::WorkerStopped);

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

fn enqueue(command: Command, pending: &mut VecDeque<PendingWrite>) {
    if let Command::Write { values, reply } = command {
        pending.push_back(PendingWrite { values, reply });
    }
}

fn reject_pending(
    pending: &mut VecDeque<PendingWrite>,
    receiver: &Receiver<Command>,
    error: RuntimeError,
) {
    while let Some(item) = pending.pop_front() {
        let _ = item.reply.send(Err(error.clone()));
    }
    loop {
        match receiver.try_recv() {
            Ok(Command::Write { reply, .. }) => {
                let _ = reply.send(Err(error.clone()));
            }
            Ok(Command::Wake) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn publish_snapshot(
    snapshot_lock: &RwLock<Snapshot>,
    status: &Mutex<RuntimeStatus>,
    snapshot: &Snapshot,
) {
    *snapshot_lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot.clone();
    update_status(status, |current| {
        current.cycle_index = snapshot.cycle_index;
        current.simulation_time = snapshot.simulation_time;
    });
}

fn record_execution_duration(
    status: &Mutex<RuntimeStatus>,
    elapsed: Duration,
    period: Duration,
) {
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
            (Self { state: Arc::clone(&state) }, state)
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
        sleep(Duration::from_millis(35));
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
}
