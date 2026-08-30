use crate::format::{Snapshot, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Request {
    Execute {
        id: u64,
        cycles: u64,
        #[serde(default)]
        writes: BTreeMap<String, Value>,
    },
    Simulate {
        id: u64,
        cycles: u64,
        sample_count: u64,
    },
    Generate {
        id: u64,
        cycles: u64,
        sample_interval: u64,
        file_path: String,
    },
    Get {
        id: u64,
        name: String,
    },
    Stop {
        id: u64,
    },
}

impl Request {
    pub fn id(&self) -> u64 {
        match self {
            Self::Execute { id, .. }
            | Self::Simulate { id, .. }
            | Self::Generate { id, .. }
            | Self::Get { id, .. }
            | Self::Stop { id } => *id,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<Vec<Snapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<JsonValue>,
}

impl Response {
    pub fn snapshot(id: u64, snapshot: Snapshot) -> Self {
        Self {
            id,
            code: 0,
            message: None,
            snapshot: Some(snapshot),
            snapshots: None,
            content: None,
        }
    }
    pub fn snapshots(id: u64, snapshots: Vec<Snapshot>) -> Self {
        Self {
            id,
            code: 0,
            message: None,
            snapshot: None,
            snapshots: Some(snapshots),
            content: None,
        }
    }
    pub fn content(id: u64, content: JsonValue) -> Self {
        Self {
            id,
            code: 0,
            message: None,
            snapshot: None,
            snapshots: None,
            content: Some(content),
        }
    }
    pub fn ok(id: u64) -> Self {
        Self {
            id,
            code: 0,
            message: None,
            snapshot: None,
            snapshots: None,
            content: None,
        }
    }
    pub fn error(id: u64, code: i32, message: String) -> Self {
        Self {
            id,
            code,
            message: Some(message),
            snapshot: None,
            snapshots: None,
            content: None,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn response_constructors_have_one_payload_shape() {
        let snapshot = Snapshot {
            cycle_index: 0,
            simulation_time: 0.0,
            values: BTreeMap::new(),
        };
        assert!(Response::snapshot(1, snapshot).snapshot.is_some());
        assert_eq!(
            Response::snapshots(5, Vec::new()).snapshots.unwrap().len(),
            0
        );
        assert!(Response::content(2, JsonValue::Bool(true))
            .content
            .is_some());
        assert_eq!(Response::ok(3).code, 0);
        let error = Response::error(4, 1204, "bad".into());
        assert_eq!(
            (error.id, error.code, error.message.as_deref()),
            (4, 1204, Some("bad"))
        );
    }
}
