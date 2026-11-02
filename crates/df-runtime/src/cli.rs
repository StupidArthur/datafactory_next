//! CLI front-end for df-runtime (`doc/runtime-system-design.md` §12).
//!
//! Commands:
//!
//! | 命令 | 行为 |
//! |---|---|
//! | `df-runtime run <project>` | 单实例模式（`.dfproject` 或 DSL v2 YAML），兼容旧用法 |
//! | `df-runtime check --config <runtime.yaml>` | 校验 Host 配置与项目编译，不启动调度 |
//! | `df-runtime run --config <runtime.yaml>` | 启动多实例 Host |
//! | `df-runtime version` | 输出 Runtime、Project Format 与 Engine ABI 版本 |
//!
//! Exit codes (§12.2): 0 正常停止；1 未分类错误；2 参数或配置错误；
//! 3 项目编译/加载失败；4 OPC UA 启动失败；5 Host 运行中不可恢复错误。

use std::io::{self, BufRead};
use std::path::Path;

use df_core::{ENGINE_ABI_VERSION, PROJECT_FORMAT_VERSION};
use df_runtime::{HostConfig, Runtime, RuntimeManager, RuntimeState};

#[cfg(feature = "opcua")]
use df_runtime::opcua::{InstanceCatalog, OpcUaServer, SnapshotPublisher};

/// Non-zero exit outcomes mapped onto §12.2.
pub struct Failure {
    pub code: u8,
    pub message: String,
}

impl Failure {
    fn args_error(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    fn project_error(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }

    /// OPC UA serving could not be started (§12.2).
    #[cfg(feature = "opcua")]
    fn opcua_error(message: impl Into<String>) -> Self {
        Self {
            code: 4,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug)]
pub(crate) enum CliCommand {
    RunSingle { project: String },
    Check { config: String },
    RunHost { config: String },
    Version,
}

const USAGE: &str = "usage:\n  \
    df-runtime run <project.dfproject | project.yaml>\n  \
    df-runtime check --config <runtime.yaml>\n  \
    df-runtime run --config <runtime.yaml>\n  \
    df-runtime version\n\
    stdin commands (single project): status | snapshot | stop\n\
    stdin commands (host): status [id] | snapshot <id> | stop [id] | stop";

/// Parse raw CLI arguments (excluding argv[0]).
pub(crate) fn parse_args(args: &[String]) -> Result<CliCommand, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["run", "--config"] => Err(USAGE.into()),
        ["run", project] => Ok(CliCommand::RunSingle {
            project: project.to_string(),
        }),
        ["check", "--config", config] => Ok(CliCommand::Check {
            config: config.to_string(),
        }),
        ["run", "--config", config] => Ok(CliCommand::RunHost {
            config: config.to_string(),
        }),
        ["version"] | ["--version"] | ["-V"] => Ok(CliCommand::Version),
        _ => Err(USAGE.into()),
    }
}

/// Execute one CLI command.
pub fn execute(args: &[String]) -> Result<(), Failure> {
    let command = parse_args(args).map_err(Failure::args_error)?;
    match command {
        CliCommand::RunSingle { project } => run_single(&project),
        CliCommand::Check { config } => check_config(&config),
        CliCommand::RunHost { config } => run_host(&config),
        CliCommand::Version => {
            println!("df-runtime {}", env!("CARGO_PKG_VERSION"));
            println!("project format version {PROJECT_FORMAT_VERSION}");
            println!("engine ABI version {ENGINE_ABI_VERSION}");
            Ok(())
        }
    }
}

/// Single-project realtime run (compatibility mode, CLI-02).
fn run_single(project: &str) -> Result<(), Failure> {
    let mut runtime = Runtime::load(Path::new(project))
        .map_err(|error| Failure::project_error(format!("cannot start runtime: {error}")))?;

    let status = runtime.status();
    println!(
        "runtime started: state={:?}, cycle_time={}s, cycle_index={}",
        status.state, status.cycle_time, status.cycle_index
    );

    for line in io::stdin().lock().lines() {
        let line = line.map_err(|error| Failure {
            code: 1,
            message: format!("stdin error: {error}"),
        })?;
        match line.trim() {
            "" => {}
            "status" => {
                let status = runtime.status();
                println!(
                    "state={:?} cycle_index={} simulation_time={}s last_cycle={:?} max_cycle={:?} overruns={} missed_deadlines={} rejected_writes={} queue_depth={} revision={}",
                    status.state, status.cycle_index, status.simulation_time,
                    status.last_execution_duration, status.max_execution_duration,
                    status.overrun_count, status.missed_deadlines,
                    status.rejected_write_batches, status.queue_depth,
                    status.snapshot_revision
                );
                if let Some(error) = status.last_error {
                    println!("last_error={error}");
                }
                if let Some(error) = status.last_write_error {
                    println!("last_write_error={error}");
                }
            }
            "snapshot" => {
                let snapshot = runtime.snapshot();
                println!(
                    "cycle_index={} simulation_time={}s values={:?}",
                    snapshot.cycle_index, snapshot.simulation_time, snapshot.values
                );
            }
            "stop" => break,
            command => eprintln!("unknown command: {command}; expected status, snapshot or stop"),
        }
        if runtime.status().state == RuntimeState::Failed {
            break;
        }
    }

    runtime.stop().map_err(|error| {
        // A fatal engine error during operation is a host-level unrecoverable
        // failure (exit 5), not an unclassified error.
        let code = if runtime.status().state == RuntimeState::Failed {
            5
        } else {
            1
        };
        Failure {
            code,
            message: format!("runtime stopped with error: {error}"),
        }
    })?;
    println!("runtime stopped");
    Ok(())
}

/// Validate the Host configuration and project compilation without starting
/// any scheduler (CFG-01). Exit 0 / 2 / 3 per §12.2.
fn check_config(config_path: &str) -> Result<(), Failure> {
    let config = HostConfig::load(Path::new(config_path))
        .map_err(|error| Failure::args_error(format!("host config check failed: {error}")))?;

    println!("host config ok: {config_path}");
    match &config.opcua {
        Some(opcua) => {
            // Resolve the configured users' passwords from the environment
            // now (§9.1), so `run` cannot fail later on a missing variable.
            let users = df_runtime::resolve_user_credentials(&opcua.security).map_err(|error| {
                Failure::args_error(format!("host config check failed: {error}"))
            })?;
            println!(
                "opcua: enabled={} endpoint={} users={}",
                opcua.enabled,
                opcua.endpoint_url,
                users.len()
            );
        }
        None => println!("opcua: not configured"),
    }
    println!("instances ({}):", config.instances.len());

    let mut project_failures = 0usize;
    for instance in &config.instances {
        // Compile/load the project exactly as an instance would, then drop
        // the engine; no scheduler is started (design §12.1 check).
        match df_runtime::load_engine(&instance.project) {
            Ok(_) => println!(
                "  {} enabled={} project={} [ok]",
                instance.id,
                instance.enabled,
                instance.project.display()
            ),
            Err(error) => {
                project_failures += 1;
                println!(
                    "  {} enabled={} project={} [FAILED: {error}]",
                    instance.id,
                    instance.enabled,
                    instance.project.display()
                );
            }
        }
    }

    if project_failures > 0 {
        return Err(Failure::project_error(format!(
            "{project_failures} instance project(s) failed to compile or load"
        )));
    }
    Ok(())
}

/// Multi-instance host mode (§12.1): start enabled instances, serve OPC UA
/// when configured (§9) and run the interactive console until `stop` or end
/// of stdin.
fn run_host(config_path: &str) -> Result<(), Failure> {
    let config = HostConfig::load(Path::new(config_path))
        .map_err(|error| Failure::args_error(format!("host config error: {error}")))?;
    let manager = RuntimeManager::from_config(config.clone());

    println!("starting runtime host: {config_path}");
    let failures = manager.start_all();
    for (id, error) in &failures {
        eprintln!("instance {id} failed to start: {error}");
    }

    let enabled = manager
        .list_instances()
        .into_iter()
        .filter(|instance| instance.enabled)
        .count();
    if enabled > 0 && failures.len() == enabled {
        return Err(Failure::project_error(
            "all enabled instances failed to start",
        ));
    }

    // Ids that were running when the host came up: if one of these hits a
    // fatal error later, the host stops with exit 5 (§12.2). Instances that
    // failed to start above are already reported and do not re-trigger.
    let started: Vec<String> = manager
        .list_instances()
        .into_iter()
        .filter(|summary| summary.enabled && summary.state == df_runtime::InstanceState::Running)
        .map(|summary| summary.id)
        .collect();

    // OPC UA serving (§9): the address space is built from the Tag Catalogs
    // of the running instances; runtimes are attached for closed-loop writes.
    #[cfg(feature = "opcua")]
    let opcua_server = match start_opcua_serving(&config, &manager) {
        Ok(server) => server,
        Err(message) => {
            let stop_failures = manager.shutdown();
            for (id, error) in &stop_failures {
                eprintln!("instance {id} failed during shutdown: {error}");
            }
            return Err(Failure::opcua_error(message));
        }
    };
    #[cfg(not(feature = "opcua"))]
    if config.opcua.as_ref().is_some_and(|opcua| opcua.enabled) {
        eprintln!(
            "note: opcua.enabled=true, but this binary was built without the opcua feature; serving is disabled"
        );
    }

    for summary in manager.list_instances() {
        println!(
            "instance {}: state={:?} project={}",
            summary.id,
            summary.state,
            summary.project.display()
        );
    }
    println!("host console commands: status [id] | snapshot <id> | stop [id] | stop");

    for line in io::stdin().lock().lines() {
        let line = line.map_err(|error| Failure {
            code: 1,
            message: format!("stdin error: {error}"),
        })?;
        if handle_host_command(&manager, line.trim()).is_stop() {
            break;
        }
        // Mid-run unrecoverable failures stop the host with exit 5 (§12.2).
        let crashed: Vec<String> = manager
            .list_instances()
            .into_iter()
            .filter(|summary| summary.state == df_runtime::InstanceState::Failed)
            .filter(|summary| started.contains(&summary.id))
            .map(|summary| summary.id)
            .collect();
        if !crashed.is_empty() {
            let stop_failures = manager.shutdown();
            for (id, error) in &stop_failures {
                eprintln!("instance {id} failed during shutdown: {error}");
            }
            return Err(Failure {
                code: 5,
                message: format!("instance(s) {crashed:?} failed during operation"),
            });
        }
        #[cfg(feature = "opcua")]
        if let Some(server) = &opcua_server {
            if !server.is_running() {
                let stop_failures = manager.shutdown();
                for (id, error) in &stop_failures {
                    eprintln!("instance {id} failed during shutdown: {error}");
                }
                return Err(Failure {
                    code: 5,
                    message: "the opcua server stopped unexpectedly".into(),
                });
            }
        }
    }

    #[cfg(feature = "opcua")]
    if let Some(mut server) = opcua_server {
        server.stop();
    }
    let stop_failures = manager.shutdown();
    for (id, error) in &stop_failures {
        eprintln!("instance {id} failed during shutdown: {error}");
    }
    println!("runtime host stopped");
    Ok(())
}

/// Start OPC UA serving for the running instances (§9): one Tag Catalog per
/// running instance feeds the address space; the runtimes are attached so
/// writes flow into the engines. Returns `None` when serving is disabled.
#[cfg(feature = "opcua")]
fn start_opcua_serving(
    config: &HostConfig,
    manager: &RuntimeManager,
) -> Result<Option<OpcUaServer>, String> {
    let Some(opcua) = &config.opcua else {
        return Ok(None);
    };
    if !opcua.enabled {
        return Ok(None);
    }

    let mut catalogs: Vec<InstanceCatalog> = Vec::new();
    let mut handles: Vec<(String, df_runtime::RuntimeHandle)> = Vec::new();
    for summary in manager.list_instances() {
        if summary.state != df_runtime::InstanceState::Running {
            continue;
        }
        let engine = df_runtime::load_engine(&summary.project).map_err(|error| {
            format!(
                "cannot load project {} for the opcua catalog of instance {}: {error}",
                summary.project.display(),
                summary.id
            )
        })?;
        catalogs.push(InstanceCatalog {
            instance_id: summary.id.clone(),
            tags: engine.catalog().to_vec(),
        });
        let handle = manager.get_instance(&summary.id).map_err(|error| {
            format!("instance {} is unavailable for opcua: {error}", summary.id)
        })?;
        handles.push((summary.id.clone(), handle));
    }

    let mut server = OpcUaServer::start_with_catalogs(opcua, &catalogs)
        .map_err(|error| format!("opcua server failed to start: {error}"))?;
    if let Err(error) = server.attach_runtimes(&handles) {
        server.stop();
        return Err(format!("opcua runtime attach failed: {error}"));
    }
    if let Err(error) = SnapshotPublisher::start(&server, &handles) {
        server.stop();
        return Err(format!("opcua snapshot publisher failed to start: {error}"));
    }
    println!("opcua serving at {}", server.endpoint_url());
    Ok(Some(server))
}

struct Flow(bool);

impl Flow {
    fn is_stop(&self) -> bool {
        self.0
    }
}

/// Handle one console command; returns stop when the host should shut down.
fn handle_host_command(manager: &RuntimeManager, line: &str) -> Flow {
    let mut tokens = line.split_whitespace();
    match (tokens.next(), tokens.next()) {
        (Some("stop"), None) => Flow(true),
        (Some("status"), None) => {
            let summaries = manager.list_instances();
            if summaries.is_empty() {
                println!("no instances configured");
            }
            for summary in summaries {
                print_summary(&summary);
            }
            Flow(false)
        }
        (Some("status"), Some(id)) => {
            match manager.get_instance(id) {
                Ok(handle) => {
                    let status = handle.status();
                    println!(
                        "instance {id}: state={:?} cycle_index={} simulation_time={}s last_cycle={:?} max_cycle={:?} overruns={} missed_deadlines={} rejected_writes={} queue_depth={} revision={}",
                        status.state, status.cycle_index, status.simulation_time,
                        status.last_execution_duration, status.max_execution_duration,
                        status.overrun_count, status.missed_deadlines,
                        status.rejected_write_batches, status.queue_depth,
                        status.snapshot_revision
                    );
                    if let Some(error) = status.last_error {
                        println!("last_error={error}");
                    }
                    if let Some(error) = status.last_write_error {
                        println!("last_write_error={error}");
                    }
                }
                Err(error) => eprintln!("{error}"),
            }
            Flow(false)
        }
        (Some("snapshot"), Some(id)) => {
            match manager.get_instance(id) {
                Ok(handle) => {
                    let snapshot = handle.snapshot();
                    println!(
                        "instance {id}: cycle_index={} simulation_time={}s values={:?}",
                        snapshot.cycle_index, snapshot.simulation_time, snapshot.values
                    );
                }
                Err(error) => eprintln!("{error}"),
            }
            Flow(false)
        }
        (Some("stop"), Some(id)) => {
            if let Err(error) = manager.stop_instance(id) {
                eprintln!("{error}");
            }
            Flow(false)
        }
        (None, _) => Flow(false),
        (Some(command), _) => {
            eprintln!(
                "unknown command: {command:?}; expected status [id], snapshot <id>, stop [id] or stop"
            );
            Flow(false)
        }
    }
}

fn print_summary(summary: &df_runtime::InstanceSummary) {
    let mut line = format!(
        "instance {}: state={:?} enabled={} cycle_index={} simulation_time={}s revision={} queue_depth={} overruns={} missed_deadlines={}",
        summary.id,
        summary.state,
        summary.enabled,
        summary.cycle_index,
        summary.simulation_time,
        summary.snapshot_revision,
        summary.queue_depth,
        summary.overrun_count,
        summary.missed_deadlines
    );
    if let Some(error) = &summary.error {
        line.push_str(&format!(" error={error}"));
    }
    println!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn parses_all_command_forms() {
        assert!(matches!(
            parse_args(&args(&["run", "p.dfproject"])),
            Ok(CliCommand::RunSingle { .. })
        ));
        assert!(matches!(
            parse_args(&args(&["check", "--config", "runtime.yaml"])),
            Ok(CliCommand::Check { .. })
        ));
        assert!(matches!(
            parse_args(&args(&["run", "--config", "runtime.yaml"])),
            Ok(CliCommand::RunHost { .. })
        ));
        assert!(matches!(
            parse_args(&args(&["version"])),
            Ok(CliCommand::Version)
        ));
        assert!(matches!(
            parse_args(&args(&["--version"])),
            Ok(CliCommand::Version)
        ));
    }

    #[test]
    fn rejects_incomplete_arguments_with_usage() {
        for bad in [
            vec![],
            vec!["p.dfproject"],
            vec!["run"],
            vec!["check"],
            vec!["check", "--config"],
            vec!["run", "--config"],
            vec!["status"],
            vec!["run", "a", "b"],
            vec!["check", "--config", "a", "b"],
        ] {
            let bad: Vec<String> = bad.iter().map(|item| item.to_string()).collect();
            let error = parse_args(&bad).unwrap_err();
            assert!(error.contains("usage:"), "{bad:?}: {error}");
        }
    }
}
