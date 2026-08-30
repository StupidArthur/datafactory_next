use df_core::{compile_source, Value};
use df_sdk::EngineClient;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn rust_sdk_runs_compiled_project() {
    let source = include_str!("../../../examples/two-tank/project.yaml");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project = std::env::temp_dir().join(format!("df-two-tank-{unique}.dfproject"));
    fs::write(&project, compile_source(source).unwrap()).unwrap();

    let executable = env!("CARGO_BIN_EXE_df-engine");
    let mut engine =
        EngineClient::start_with_engine(std::path::Path::new(executable), &project).unwrap();
    let metadata = engine.get("project_metadata").unwrap();
    assert_eq!(metadata["name"], "two_tank");
    let snapshot = engine
        .execute(
            3,
            BTreeMap::from([("plant_inlet_flow".into(), Value::Float(0.002))]),
        )
        .unwrap();
    assert_eq!(snapshot.cycle_index, 3);
    assert_eq!(snapshot.simulation_time, 1.5);
    let samples = engine.simulate(10, 4).unwrap();
    assert_eq!(samples.len(), 4);
    assert_eq!(samples.last().unwrap().cycle_index, 10);
    let parquet = std::env::temp_dir().join(format!("df-two-tank-{unique}.parquet"));
    engine.generate(10, 3, &parquet).unwrap();
    assert!(fs::metadata(&parquet).unwrap().len() > 0);
    engine.stop().unwrap();
    engine.stop().unwrap();
    fs::remove_file(project).unwrap();
    fs::remove_file(parquet).unwrap();
}

#[test]
fn rust_sdk_covers_lifecycle_and_remote_errors() {
    let source = include_str!("../../../examples/two-tank/project.yaml");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project = std::env::temp_dir().join(format!("df-sdk-life-{unique}.dfproject"));
    fs::write(&project, compile_source(source).unwrap()).unwrap();
    let executable = env!("CARGO_BIN_EXE_df-engine");

    std::env::set_var("DF_ENGINE_PATH", executable);
    let mut engine = EngineClient::start(&project).unwrap();
    std::env::remove_var("DF_ENGINE_PATH");
    assert!(engine.get("unknown").is_err());
    engine.stop().unwrap();
    assert!(engine.get("project_metadata").is_err());

    let active =
        EngineClient::start_with_engine(std::path::Path::new(executable), &project).unwrap();
    drop(active);
    assert!(EngineClient::start_with_engine(
        std::path::Path::new("definitely-missing-engine"),
        &project
    )
    .is_err());
    std::env::remove_var("DF_ENGINE_PATH");
    let mut default_engine = EngineClient::start(&project).unwrap();
    default_engine.stop().unwrap();
    let bad = std::env::temp_dir().join(format!("df-sdk-bad-{unique}.dfproject"));
    fs::write(&bad, b"bad").unwrap();
    assert!(EngineClient::start_with_engine(std::path::Path::new(executable), &bad).is_err());
    fs::remove_file(project).unwrap();
    fs::remove_file(bad).unwrap();
}

#[test]
fn command_interface_covers_success_and_failure_responses() {
    let executable = env!("CARGO_BIN_EXE_df-engine");
    assert!(!Command::new(executable).status().unwrap().success());

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let source_path = std::env::temp_dir().join(format!("df-cli-{unique}.yaml"));
    let project_path = std::env::temp_dir().join(format!("df-cli-{unique}.dfproject"));
    fs::write(
        &source_path,
        include_str!("../../../examples/two-tank/project.yaml"),
    )
    .unwrap();
    assert!(Command::new(executable)
        .args([
            "compile",
            source_path.to_str().unwrap(),
            project_path.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    fs::write(&source_path, ": bad").unwrap();
    assert!(!Command::new(executable)
        .args([
            "compile",
            source_path.to_str().unwrap(),
            project_path.to_str().unwrap(),
        ])
        .status()
        .unwrap()
        .success());
    fs::write(
        &source_path,
        include_str!("../../../examples/two-tank/project.yaml"),
    )
    .unwrap();

    let mut child = Command::new(executable)
        .args(["run", project_path.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    input.write_all(b"\nnot-json\n{\"operation\":\"get\",\"id\":2,\"name\":\"unknown\"}\n{\"operation\":\"execute\",\"id\":3,\"cycles\":0,\"writes\":{\"plant_inlet_flow\":1.0}}\n{\"operation\":\"simulate\",\"id\":4,\"cycles\":1,\"sample_count\":0}\n{\"operation\":\"generate\",\"id\":5,\"cycles\":1,\"sample_interval\":1,\"file_path\":\"bad.csv\"}\n{\"operation\":\"stop\",\"id\":6}\n").unwrap();
    input.flush().unwrap();
    let mut lines = Vec::new();
    for _ in 0..6 {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        lines.push(line);
    }
    assert!(lines[0].contains("1302"));
    assert!(lines[1].contains("1204"));
    assert!(lines[2].contains("1204"));
    assert!(lines[3].contains("1204"));
    assert!(lines[4].contains("1204"));
    assert!(lines[5].contains("\"code\":0"));
    assert!(child.wait().unwrap().success());
    fs::remove_file(source_path).unwrap();
    fs::remove_file(project_path).unwrap();
}
