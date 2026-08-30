use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAGIC: &[u8; 8] = b"DFPRJ001";
pub const PROJECT_FORMAT_VERSION: u32 = 3;
pub const ENGINE_ABI_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "BOOL",
            Self::Int(_) => "INT64",
            Self::Float(_) => "FLOAT64",
            Self::String(_) => "STRING",
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TagDescriptor {
    pub name: String,
    pub data_type: String,
    pub writable: bool,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub description: String,
    pub initial: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrendDescriptor {
    pub name: String,
    pub scale: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMetadata {
    pub name: String,
    pub cycle_time: f64,
    pub format_version: u32,
    pub engine_abi_version: u32,
    pub tag_count: usize,
    pub node_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub cycle_index: u64,
    pub simulation_time: f64,
    pub values: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CompiledProject {
    pub format_version: u32,
    pub engine_abi_version: u32,
    pub name: String,
    pub cycle_time: f64,
    pub tags: Vec<TagDescriptor>,
    pub default_trends: Vec<TrendDescriptor>,
    pub export_tags: Vec<String>,
    pub nodes: Vec<CompiledNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CompiledNode {
    Variable,
    Expression {
        output_slot: usize,
        bytecode: Vec<ExpressionOp>,
    },
    Lag {
        source_slot: usize,
        output_slot: usize,
        delay: usize,
    },
    SineWave {
        output_slot: usize,
        amplitude: f64,
        period: f64,
        phase: f64,
        offset: f64,
    },
    ListWave {
        output_slot: usize,
        wave_list: Vec<(f64, f64)>,
    },
    Random {
        output_slot: usize,
        low: f64,
        high: f64,
        max_step: f64,
        seed: u64,
    },
    Valve {
        target_slot: usize,
        inlet_slot: usize,
        target_output_slot: usize,
        opening_slot: usize,
        outlet_slot: usize,
        full_travel_time: f64,
    },
    CylindricalTank {
        name: String,
        inlet_slot: usize,
        level_slot: usize,
        outlet_slot: usize,
        height: f64,
        radius: f64,
        outlet_area: f64,
    },
    Pid {
        pv_input_slot: usize,
        sv_input_slot: Option<usize>,
        pv_slot: usize,
        sv_slot: usize,
        mv_slot: usize,
        mode_slot: usize,
        pb: f64,
        ti: f64,
        td: f64,
        sv_default: f64,
        mv_initial: f64,
        mv_low: f64,
        mv_high: f64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum ExpressionOp {
    Number(f64),
    Slot(usize),
    Add,
    Sub,
    Mul,
    Div,
    Sin,
    Cos,
    Sqrt,
    Abs,
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn value_types_and_numeric_conversion_are_explicit() {
        let values = [
            Value::Bool(true),
            Value::Int(2),
            Value::Float(3.0),
            Value::String("x".into()),
        ];
        assert_eq!(
            values.iter().map(Value::type_name).collect::<Vec<_>>(),
            ["BOOL", "INT64", "FLOAT64", "STRING"]
        );
        assert_eq!(values[0].as_f64(), None);
        assert_eq!(values[1].as_f64(), Some(2.0));
        assert_eq!(values[2].as_f64(), Some(3.0));
        assert_eq!(values[3].as_f64(), None);
    }
}
