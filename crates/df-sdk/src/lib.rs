#![allow(unexpected_cfgs)]
#![allow(unused_features)]
#![cfg_attr(coverage, feature(coverage_attribute))]

pub use df_core::{EngineError, Result, Snapshot, Value};
use df_core::{Request, Response};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct EngineClient {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    stopped: bool,
}

impl EngineClient {
    pub fn start(project_path: &Path) -> Result<Self> {
        let engine_path = match std::env::var_os("DF_ENGINE_PATH") {
            Some(path) => path,
            None => "df-engine".into(),
        };
        Self::start_with_engine(Path::new(&engine_path), project_path)
    }

    pub fn start_with_engine(engine_path: &Path, project_path: &Path) -> Result<Self> {
        let mut child = Command::new(engine_path)
            .arg("run")
            .arg(project_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| EngineError::Process(format!("cannot start engine: {e}")))?;
        let input = child.stdin.take().expect("piped engine stdin must exist");
        let output = BufReader::new(child.stdout.take().expect("piped engine stdout must exist"));
        let mut client = Self {
            child,
            input,
            output,
            next_id: 1,
            stopped: false,
        };
        if let Err(error) = client.get("project_metadata") {
            let _ = client.child.kill();
            let _ = client.child.wait();
            client.stopped = true;
            return Err(error);
        }
        Ok(client)
    }

    pub fn execute(&mut self, cycles: u64, writes: BTreeMap<String, Value>) -> Result<Snapshot> {
        let id = self.allocate_id();
        let response = self.exchange(&Request::Execute { id, cycles, writes })?;
        require_snapshot(response)
    }

    pub fn simulate(&mut self, cycles: u64, sample_count: u64) -> Result<Vec<Snapshot>> {
        let id = self.allocate_id();
        let response = self.exchange(&Request::Simulate {
            id,
            cycles,
            sample_count,
        })?;
        require_snapshots(response)
    }

    pub fn generate(&mut self, cycles: u64, sample_interval: u64, file_path: &Path) -> Result<()> {
        let id = self.allocate_id();
        self.exchange(&Request::Generate {
            id,
            cycles,
            sample_interval,
            file_path: file_path.to_string_lossy().into_owned(),
        })?;
        Ok(())
    }

    pub fn get(&mut self, name: &str) -> Result<JsonValue> {
        let id = self.allocate_id();
        let response = self.exchange(&Request::Get {
            id,
            name: name.into(),
        })?;
        require_content(response)
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let id = self.allocate_id();
        self.exchange(&Request::Stop { id })?;
        let status = wait_result(self.child.wait())?;
        self.stopped = true;
        validate_exit_status(status)
    }

    fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    fn exchange(&mut self, request: &Request) -> Result<Response> {
        if self.stopped {
            return Err(EngineError::Process("engine is stopped".into()));
        }
        serde_json::to_writer(&mut self.input, request)
            .map_err(|e| EngineError::Protocol(e.to_string()))?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let mut line = String::new();
        self.output.read_line(&mut line)?;
        decode_response(request.id(), &line)
    }
}

fn decode_response(request_id: u64, line: &str) -> Result<Response> {
    if line.is_empty() {
        return Err(EngineError::Process(
            "engine closed the protocol stream".into(),
        ));
    }
    let response: Response =
        serde_json::from_str(line).map_err(|e| EngineError::Protocol(e.to_string()))?;
    if response.id != request_id {
        return Err(EngineError::Protocol("response id mismatch".into()));
    }
    if response.code != 0 {
        return Err(EngineError::Remote {
            code: response.code,
            message: response.message.unwrap_or_default(),
        });
    }
    Ok(response)
}

fn validate_exit_status(status: std::process::ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(EngineError::Process(format!("engine exited with {status}")))
    }
}

fn wait_result(
    status: std::io::Result<std::process::ExitStatus>,
) -> Result<std::process::ExitStatus> {
    status.map_err(|error| EngineError::Process(error.to_string()))
}

fn require_snapshot(response: Response) -> Result<Snapshot> {
    response
        .snapshot
        .ok_or_else(|| EngineError::Protocol("execute response has no snapshot".into()))
}

fn require_snapshots(response: Response) -> Result<Vec<Snapshot>> {
    response
        .snapshots
        .ok_or_else(|| EngineError::Protocol("simulate response has no snapshots".into()))
}

fn require_content(response: Response) -> Result<JsonValue> {
    response
        .content
        .ok_or_else(|| EngineError::Protocol("get response has no content".into()))
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::os::windows::process::ExitStatusExt;

    #[test]
    fn response_validation_covers_protocol_and_remote_failures() {
        assert!(matches!(
            decode_response(1, ""),
            Err(EngineError::Process(_))
        ));
        assert!(matches!(
            decode_response(1, "not-json"),
            Err(EngineError::Protocol(_))
        ));
        assert!(matches!(
            decode_response(1, r#"{"id":2,"code":0}"#),
            Err(EngineError::Protocol(_))
        ));
        let error = decode_response(1, r#"{"id":1,"code":1204,"message":"bad"}"#).unwrap_err();
        assert!(matches!(error, EngineError::Remote { code: 1204, .. }));
        assert_eq!(decode_response(1, r#"{"id":1,"code":0}"#).unwrap().id, 1);
    }

    #[test]
    fn exit_status_must_be_successful() {
        assert!(validate_exit_status(std::process::ExitStatus::from_raw(0)).is_ok());
        assert!(matches!(
            validate_exit_status(std::process::ExitStatus::from_raw(1)),
            Err(EngineError::Process(_))
        ));
        assert!(wait_result(Err(std::io::Error::other("wait"))).is_err());
        assert!(wait_result(Ok(std::process::ExitStatus::from_raw(0))).is_ok());
    }

    #[test]
    fn response_payloads_are_required() {
        assert!(require_snapshot(Response::ok(1)).is_err());
        assert!(require_snapshots(Response::ok(1)).is_err());
        assert!(require_content(Response::ok(1)).is_err());
        assert!(require_snapshot(Response::snapshot(
            1,
            Snapshot {
                cycle_index: 0,
                simulation_time: 0.0,
                values: BTreeMap::new()
            }
        ))
        .is_ok());
        assert!(require_content(Response::content(1, JsonValue::Null)).is_ok());
        assert!(require_snapshots(Response::snapshots(1, Vec::new())).is_ok());
    }
}
