use df_core::{compile_file, Engine, Request, Response};
use std::env;
use std::io::{self, BufRead, Write};
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
        Some("compile") if args.len() == 4 => compile_file(Path::new(&args[2]), Path::new(&args[3])).map_err(|e| e.to_string()),
        Some("run") if args.len() == 3 => serve(&args[2]),
        _ => Err("usage: df-engine compile <source.yaml> <project.dfproject> | df-engine run <project.dfproject>".into()),
    }
}

fn serve(project_path: &str) -> Result<(), String> {
    let mut engine = Engine::load(Path::new(project_path)).map_err(|e| e.to_string())?;
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let response = Response::error(0, 1302, format!("invalid request JSON: {e}"));
                write_response(&mut stdout, &response)?;
                continue;
            }
        };
        let id = request.id();
        let (response, stop) = match request {
            Request::Execute { cycles, writes, .. } => match engine.execute(cycles, &writes) {
                Ok(snapshot) => (Response::snapshot(id, snapshot), false),
                Err(e) => (Response::error(id, e.code(), e.to_string()), false),
            },
            Request::Simulate {
                cycles,
                sample_count,
                ..
            } => match engine.simulate(cycles, sample_count) {
                Ok(snapshots) => (Response::snapshots(id, snapshots), false),
                Err(e) => (Response::error(id, e.code(), e.to_string()), false),
            },
            Request::Generate {
                cycles,
                sample_interval,
                file_path,
                ..
            } => match engine.generate(cycles, sample_interval, Path::new(&file_path)) {
                Ok(()) => (Response::ok(id), false),
                Err(e) => (Response::error(id, e.code(), e.to_string()), false),
            },
            Request::Get { name, .. } => match engine.get(&name) {
                Ok(content) => (Response::content(id, content), false),
                Err(e) => (Response::error(id, e.code(), e.to_string()), false),
            },
            Request::Stop { .. } => (Response::ok(id), true),
        };
        write_response(&mut stdout, &response)?;
        if stop {
            break;
        }
    }
    Ok(())
}

fn write_response(output: &mut dyn Write, response: &Response) -> Result<(), String> {
    serde_json::to_writer(&mut *output, response).map_err(|e| e.to_string())?;
    output
        .write_all(b"\n")
        .and_then(|_| output.flush())
        .map_err(|e| e.to_string())
}
