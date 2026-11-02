use df_runtime::{Runtime, RuntimeState};
use std::env;
use std::io::{self, BufRead};
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") if args.len() == 3 => run_project(&args[2]),
        _ => Err("usage: df-runtime run <project.dfproject>\ncommands on stdin: status | snapshot | stop".into()),
    }
}

fn run_project(project_path: &str) -> Result<(), String> {
    let mut runtime = Runtime::load(Path::new(project_path))
        .map_err(|error| format!("cannot start runtime: {error}"))?;

    let status = runtime.status();
    println!(
        "runtime started: state={:?}, cycle_time={}s, cycle_index={}",
        status.state, status.cycle_time, status.cycle_index
    );

    for line in io::stdin().lock().lines() {
        let line = line.map_err(|error| format!("stdin error: {error}"))?;
        match line.trim() {
            "" => {}
            "status" => {
                let status = runtime.status();
                println!(
                    "state={:?} cycle_index={} simulation_time={}s last_cycle={:?} max_cycle={:?} overruns={} missed_deadlines={} rejected_writes={}",
                    status.state, status.cycle_index, status.simulation_time,
                    status.last_execution_duration, status.max_execution_duration,
                    status.overrun_count, status.missed_deadlines, status.rejected_write_batches
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

    runtime
        .stop()
        .map_err(|error| format!("runtime stopped with error: {error}"))?;
    println!("runtime stopped");
    Ok(())
}
