use crate::error::{EngineError, Result};
use crate::format::{
    CompiledNode, CompiledProject, ExpressionOp, ProjectMetadata, Snapshot, TagDescriptor, Value,
    ENGINE_ABI_VERSION, MAGIC, PROJECT_FORMAT_VERSION,
};
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Arc;

pub struct Engine {
    project: CompiledProject,
    values: Vec<Value>,
    slots: BTreeMap<String, usize>,
    cycle_index: u64,
    lag_buffers: BTreeMap<usize, VecDeque<f64>>,
    pid_states: BTreeMap<usize, (f64, f64)>,
    random_states: BTreeMap<usize, u64>,
}

impl Engine {
    pub fn load(path: &Path) -> Result<Self> {
        Self::from_bytes(&fs::read(path)?)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 12 || &bytes[..8] != MAGIC {
            return Err(EngineError::InvalidProject("bad file header".into()));
        }
        let size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if bytes.len() != size + 12 {
            return Err(EngineError::InvalidProject("invalid payload length".into()));
        }
        let project: CompiledProject = serde_json::from_slice(&bytes[12..])
            .map_err(|e| EngineError::InvalidProject(e.to_string()))?;
        if project.format_version != PROJECT_FORMAT_VERSION {
            return Err(EngineError::InvalidProject(
                "unsupported project format version".into(),
            ));
        }
        if project.engine_abi_version != ENGINE_ABI_VERSION {
            return Err(EngineError::InvalidProject(
                "unsupported engine ABI version".into(),
            ));
        }
        Ok(Self::from_project(project))
    }

    fn from_project(project: CompiledProject) -> Self {
        let values: Vec<Value> = project.tags.iter().map(|t| t.initial.clone()).collect();
        let slots = project
            .tags
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.clone(), i))
            .collect();
        let mut lag_buffers = BTreeMap::new();
        let mut pid_states = BTreeMap::new();
        let mut random_states = BTreeMap::new();
        for (index, node) in project.nodes.iter().enumerate() {
            match node {
                CompiledNode::Lag {
                    source_slot, delay, ..
                } => {
                    let initial = values[*source_slot].as_f64().unwrap_or(0.0);
                    lag_buffers.insert(index, std::iter::repeat_n(initial, *delay).collect());
                }
                CompiledNode::Pid { .. } => {
                    pid_states.insert(index, (0.0, 0.0));
                }
                CompiledNode::Random { seed, .. } => {
                    random_states.insert(index, *seed);
                }
                _ => {}
            }
        }
        Self {
            project,
            values,
            slots,
            cycle_index: 0,
            lag_buffers,
            pid_states,
            random_states,
        }
    }

    pub fn metadata(&self) -> ProjectMetadata {
        ProjectMetadata {
            name: self.project.name.clone(),
            cycle_time: self.project.cycle_time,
            format_version: self.project.format_version,
            engine_abi_version: self.project.engine_abi_version,
            tag_count: self.project.tags.len(),
            node_count: self.project.nodes.len(),
        }
    }
    pub fn catalog(&self) -> &[TagDescriptor] {
        &self.project.tags
    }
    pub fn get(&self, name: &str) -> Result<JsonValue> {
        match name {
            "project_metadata" => serde_json::to_value(self.metadata()),
            "tag_schema" => serde_json::to_value(self.catalog()),
            "default_trends" => serde_json::to_value(&self.project.default_trends),
            "default_export_tags" => serde_json::to_value(&self.project.export_tags),
            "runtime_status" => serde_json::to_value(BTreeMap::from([
                ("cycle_index", JsonValue::from(self.cycle_index)),
                ("simulation_time", JsonValue::from(self.simulation_time())),
            ])),
            _ => {
                return Err(EngineError::InvalidRequest(format!(
                    "unknown resource: {name}"
                )))
            }
        }
        .map_err(|e| EngineError::Protocol(e.to_string()))
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            cycle_index: self.cycle_index,
            simulation_time: self.simulation_time(),
            values: self
                .project
                .tags
                .iter()
                .enumerate()
                .map(|(i, t)| (t.name.clone(), self.values[i].clone()))
                .collect(),
        }
    }
    pub fn execute(&mut self, cycles: u64, writes: &BTreeMap<String, Value>) -> Result<Snapshot> {
        if cycles == 0 && !writes.is_empty() {
            return Err(EngineError::InvalidRequest(
                "writes must be empty when cycles is 0".into(),
            ));
        }
        let validated = self.validate_writes(writes)?;
        for (slot, value) in validated {
            self.values[slot] = value;
        }
        for _ in 0..cycles {
            self.step()?;
            self.cycle_index += 1;
        }
        Ok(self.snapshot())
    }

    pub fn simulate(&self, cycles: u64, sample_count: u64) -> Result<Vec<Snapshot>> {
        if sample_count == 0 {
            return Err(EngineError::InvalidRequest(
                "sample_count must be greater than 0".into(),
            ));
        }
        let mut engine = Self::from_project(self.project.clone());
        let count = sample_count.min(cycles.max(1));
        let mut snapshots = Vec::with_capacity(count as usize);
        let mut previous = 0;
        for index in 1..=count {
            let target = if cycles == 0 {
                0
            } else {
                ((index as u128) * (cycles as u128)).div_ceil(count as u128) as u64
            };
            snapshots.push(engine.execute(target - previous, &BTreeMap::new())?);
            previous = target;
        }
        Ok(snapshots)
    }

    pub fn generate(&self, cycles: u64, sample_interval: u64, path: &Path) -> Result<()> {
        if sample_interval == 0 {
            return Err(EngineError::InvalidRequest(
                "sample_interval must be greater than 0".into(),
            ));
        }
        if path.extension().and_then(|value| value.to_str()) != Some("parquet") {
            return Err(EngineError::InvalidRequest(
                "generate output path must end with .parquet".into(),
            ));
        }
        if path.exists() {
            return Err(EngineError::InvalidRequest(
                "generate output path already exists".into(),
            ));
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| EngineError::InvalidRequest("invalid output file name".into()))?;
        let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
        if temporary.exists() {
            return Err(EngineError::InvalidRequest(
                "temporary output path already exists".into(),
            ));
        }
        let result = self.write_parquet(cycles, sample_interval, &temporary);
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        fs::rename(&temporary, path)?;
        Ok(())
    }

    fn write_parquet(&self, cycles: u64, sample_interval: u64, path: &Path) -> Result<()> {
        let schema = Arc::new(self.parquet_schema()?);
        let properties = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let file = fs::File::create(path)?;
        let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(properties))
            .map_err(|error| EngineError::Output(error.to_string()))?;
        let mut engine = Self::from_project(self.project.clone());
        let mut rows = Vec::with_capacity(8192);
        if cycles == 0 {
            rows.push(engine.snapshot());
        } else {
            let mut completed: u64 = 0;
            loop {
                let target = completed.saturating_add(sample_interval).min(cycles);
                rows.push(engine.execute(target - completed, &BTreeMap::new())?);
                completed = target;
                if rows.len() == 8192 || completed == cycles {
                    writer
                        .write(&self.parquet_batch(schema.clone(), &rows)?)
                        .map_err(|error| EngineError::Output(error.to_string()))?;
                    rows.clear();
                }
                if completed == cycles {
                    break;
                }
            }
        }
        if !rows.is_empty() {
            writer
                .write(&self.parquet_batch(schema, &rows)?)
                .map_err(|error| EngineError::Output(error.to_string()))?;
        }
        writer
            .close()
            .map_err(|error| EngineError::Output(error.to_string()))?;
        Ok(())
    }

    fn parquet_schema(&self) -> Result<Schema> {
        let mut fields = vec![
            Field::new("cycle_index", DataType::UInt64, false),
            Field::new("simulation_time", DataType::Float64, false),
        ];
        for name in &self.project.export_tags {
            let tag = self
                .project
                .tags
                .iter()
                .find(|tag| &tag.name == name)
                .ok_or_else(|| {
                    EngineError::InvalidProject(format!("unknown export tag: {name}"))
                })?;
            let data_type = match tag.data_type.as_str() {
                "FLOAT64" => DataType::Float64,
                "INT64" => DataType::Int64,
                "BOOL" => DataType::Boolean,
                "STRING" => DataType::Utf8,
                value => {
                    return Err(EngineError::InvalidProject(format!(
                        "unsupported export type: {value}"
                    )))
                }
            };
            fields.push(Field::new(name, data_type, false));
        }
        Ok(Schema::new(fields))
    }

    fn parquet_batch(&self, schema: Arc<Schema>, rows: &[Snapshot]) -> Result<RecordBatch> {
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from_iter_values(
                rows.iter().map(|row| row.cycle_index),
            )),
            Arc::new(Float64Array::from_iter_values(
                rows.iter().map(|row| row.simulation_time),
            )),
        ];
        for name in &self.project.export_tags {
            let values = rows.iter().map(|row| {
                row.values
                    .get(name)
                    .expect("compiled export tag must exist")
            });
            let array: ArrayRef = match self.project.tags[self.slots[name]].data_type.as_str() {
                "FLOAT64" => Arc::new(Float64Array::from_iter_values(
                    values
                        .map(|value| {
                            value.as_f64().ok_or_else(|| {
                                EngineError::InvalidProject("FLOAT64 export value expected".into())
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                )),
                "INT64" => Arc::new(Int64Array::from_iter_values(
                    values
                        .map(|value| match value {
                            Value::Int(value) => Ok(*value),
                            _ => Err(EngineError::InvalidProject(
                                "INT64 export value expected".into(),
                            )),
                        })
                        .collect::<Result<Vec<_>>>()?,
                )),
                "BOOL" => Arc::new(
                    values
                        .map(|value| match value {
                            Value::Bool(value) => Ok(*value),
                            _ => Err(EngineError::InvalidProject(
                                "BOOL export value expected".into(),
                            )),
                        })
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .collect::<BooleanArray>(),
                ),
                "STRING" => Arc::new(StringArray::from_iter_values(
                    values
                        .map(|value| match value {
                            Value::String(value) => Ok(value.as_str()),
                            _ => Err(EngineError::InvalidProject(
                                "STRING export value expected".into(),
                            )),
                        })
                        .collect::<Result<Vec<_>>>()?,
                )),
                value => {
                    return Err(EngineError::InvalidProject(format!(
                        "unsupported export type: {value}"
                    )))
                }
            };
            arrays.push(array);
        }
        RecordBatch::try_new(schema, arrays).map_err(|error| EngineError::Output(error.to_string()))
    }

    fn simulation_time(&self) -> f64 {
        self.cycle_index as f64 * self.project.cycle_time
    }
    fn validate_writes(&self, writes: &BTreeMap<String, Value>) -> Result<Vec<(usize, Value)>> {
        let mut result = Vec::with_capacity(writes.len());
        for (name, value) in writes {
            let slot = *self
                .slots
                .get(name)
                .ok_or_else(|| EngineError::UnknownTag(name.clone()))?;
            let tag = &self.project.tags[slot];
            if !tag.writable {
                return Err(EngineError::NotWritable(name.clone()));
            }
            let normalized = match (tag.data_type.as_str(), value) {
                ("FLOAT64", Value::Float(v)) if v.is_finite() => Value::Float(*v),
                ("FLOAT64", Value::Int(v)) => Value::Float(*v as f64),
                ("INT64", Value::Int(v)) => Value::Int(*v),
                ("BOOL", Value::Bool(v)) => Value::Bool(*v),
                ("STRING", Value::String(v)) => Value::String(v.clone()),
                _ => return Err(EngineError::TypeMismatch(name.clone())),
            };
            result.push((slot, normalized));
        }
        Ok(result)
    }
    fn step(&mut self) -> Result<()> {
        for index in 0..self.project.nodes.len() {
            let node = self.project.nodes[index].clone();
            match node {
                CompiledNode::Variable => {}
                CompiledNode::Expression {
                    output_slot,
                    bytecode,
                } => {
                    self.values[output_slot] =
                        Value::Float(eval_expression(&bytecode, &self.values)?);
                }
                CompiledNode::Lag {
                    source_slot,
                    output_slot,
                    ..
                } => {
                    let source = self.numeric(source_slot)?;
                    let buffer = self
                        .lag_buffers
                        .get_mut(&index)
                        .ok_or_else(|| EngineError::InvalidProject("missing Lag state".into()))?;
                    let output = buffer
                        .pop_front()
                        .ok_or_else(|| EngineError::InvalidProject("empty Lag state".into()))?;
                    buffer.push_back(source);
                    self.values[output_slot] = Value::Float(output);
                }
                CompiledNode::SineWave {
                    output_slot,
                    amplitude,
                    period,
                    phase,
                    offset,
                } => {
                    let t = self.simulation_time();
                    self.values[output_slot] = Value::Float(
                        offset + amplitude * (std::f64::consts::TAU * t / period + phase).sin(),
                    );
                }
                CompiledNode::ListWave {
                    output_slot,
                    wave_list,
                } => {
                    let total: f64 = wave_list.iter().map(|x| x.1).sum();
                    let mut t = self.simulation_time() % total;
                    let mut output = wave_list[0].0;
                    for (value, duration) in wave_list {
                        output = value;
                        if t < duration {
                            break;
                        }
                        t -= duration;
                    }
                    self.values[output_slot] = Value::Float(output);
                }
                CompiledNode::Random {
                    output_slot,
                    low,
                    high,
                    max_step,
                    ..
                } => {
                    let current = self.numeric(output_slot)?;
                    let state = self.random_states.get_mut(&index).ok_or_else(|| {
                        EngineError::InvalidProject("missing RANDOM state".into())
                    })?;
                    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let unit = (*state >> 11) as f64 / ((1_u64 << 53) as f64);
                    let target = low + (high - low) * unit;
                    self.values[output_slot] = Value::Float(
                        (current + (target - current).clamp(-max_step, max_step)).clamp(low, high),
                    );
                }
                CompiledNode::Valve {
                    target_slot,
                    inlet_slot,
                    target_output_slot,
                    opening_slot,
                    outlet_slot,
                    full_travel_time,
                } => {
                    let target = self.numeric(target_slot)?.clamp(0.0, 100.0);
                    let opening = self.numeric(opening_slot)?;
                    let max_change = 100.0 * self.project.cycle_time / full_travel_time;
                    let next = opening + (target - opening).clamp(-max_change, max_change);
                    let inlet = self.numeric(inlet_slot)?.max(0.0);
                    self.values[target_output_slot] = Value::Float(target);
                    self.values[opening_slot] = Value::Float(next);
                    self.values[outlet_slot] = Value::Float(inlet * next / 100.0);
                }
                CompiledNode::CylindricalTank {
                    inlet_slot,
                    level_slot,
                    outlet_slot,
                    height,
                    radius,
                    outlet_area,
                    ..
                } => {
                    let inlet = self.values[inlet_slot]
                        .as_f64()
                        .ok_or_else(|| {
                            EngineError::InvalidProject("tank inlet slot is not numeric".into())
                        })?
                        .max(0.0);
                    let level = self.values[level_slot].as_f64().ok_or_else(|| {
                        EngineError::InvalidProject("tank level slot is not numeric".into())
                    })?;
                    let outlet = if level > 0.0 {
                        outlet_area * (2.0 * 9.81 * level).sqrt()
                    } else {
                        0.0
                    };
                    let base_area = std::f64::consts::PI * radius * radius;
                    let next_level = (level
                        + (inlet - outlet) * self.project.cycle_time / base_area)
                        .clamp(0.0, height);
                    self.values[outlet_slot] = Value::Float(outlet);
                    self.values[level_slot] = Value::Float(next_level);
                }
                CompiledNode::Pid {
                    pv_input_slot,
                    sv_input_slot,
                    pv_slot,
                    sv_slot,
                    mv_slot,
                    mode_slot,
                    pb,
                    ti,
                    td,
                    sv_default,
                    mv_initial,
                    mv_low,
                    mv_high,
                } => {
                    let pv = self.numeric(pv_input_slot)?;
                    let sv = match sv_input_slot {
                        Some(slot) => self.numeric(slot)?,
                        None => sv_default,
                    };
                    let error = sv - pv;
                    let state = self
                        .pid_states
                        .get_mut(&index)
                        .ok_or_else(|| EngineError::InvalidProject("missing PID state".into()))?;
                    state.0 += error * self.project.cycle_time;
                    let derivative = (error - state.1) / self.project.cycle_time;
                    let kp = 100.0 / pb;
                    let integral = if ti > 0.0 { state.0 / ti } else { 0.0 };
                    let mv = (mv_initial + kp * (error + integral + td * derivative))
                        .clamp(mv_low, mv_high);
                    state.1 = error;
                    self.values[pv_slot] = Value::Float(pv);
                    self.values[sv_slot] = Value::Float(sv);
                    self.values[mv_slot] = Value::Float(mv);
                    self.values[mode_slot] = Value::Float(5.0);
                }
            }
        }
        // Public inlet_flow state reflects each tank's resolved input for the completed cycle.
        for node in &self.project.nodes {
            let CompiledNode::CylindricalTank {
                name, inlet_slot, ..
            } = node
            else {
                continue;
            };
            if let Some(dst) = self.slots.get(&format!("{name}.inlet_flow")) {
                self.values[*dst] = self.values[*inlet_slot].clone();
            }
        }
        Ok(())
    }

    fn numeric(&self, slot: usize) -> Result<f64> {
        self.values
            .get(slot)
            .and_then(Value::as_f64)
            .ok_or_else(|| EngineError::InvalidProject("node slot is not numeric".into()))
    }
}

fn eval_expression(bytecode: &[ExpressionOp], values: &[Value]) -> Result<f64> {
    let mut stack = Vec::new();
    for op in bytecode {
        match op {
            ExpressionOp::Number(v) => stack.push(*v),
            ExpressionOp::Slot(slot) => {
                stack.push(values.get(*slot).and_then(Value::as_f64).ok_or_else(|| {
                    EngineError::InvalidProject("expression slot is not numeric".into())
                })?)
            }
            ExpressionOp::Add => binary(&mut stack, |a, b| a + b)?,
            ExpressionOp::Sub => binary(&mut stack, |a, b| a - b)?,
            ExpressionOp::Mul => binary(&mut stack, |a, b| a * b)?,
            ExpressionOp::Div => binary(&mut stack, |a, b| a / b)?,
            ExpressionOp::Sin => unary(&mut stack, f64::sin)?,
            ExpressionOp::Cos => unary(&mut stack, f64::cos)?,
            ExpressionOp::Sqrt => unary(&mut stack, f64::sqrt)?,
            ExpressionOp::Abs => unary(&mut stack, f64::abs)?,
        }
    }
    if stack.len() != 1 || !stack[0].is_finite() {
        return Err(EngineError::InvalidProject(
            "invalid expression result".into(),
        ));
    }
    Ok(stack[0])
}
fn binary(stack: &mut Vec<f64>, op: impl FnOnce(f64, f64) -> f64) -> Result<()> {
    let b = stack
        .pop()
        .ok_or_else(|| EngineError::InvalidProject("expression stack underflow".into()))?;
    let a = stack
        .pop()
        .ok_or_else(|| EngineError::InvalidProject("expression stack underflow".into()))?;
    stack.push(op(a, b));
    Ok(())
}
fn unary(stack: &mut Vec<f64>, op: impl FnOnce(f64) -> f64) -> Result<()> {
    let value = stack
        .pop()
        .ok_or_else(|| EngineError::InvalidProject("expression stack underflow".into()))?;
    stack.push(op(value));
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::compile_source;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SOURCE: &str = include_str!("../../../examples/two-tank/project.yaml");

    fn engine() -> Engine {
        Engine::from_bytes(&compile_source(SOURCE).unwrap()).unwrap()
    }

    #[test]
    fn simulation_is_evenly_sampled_and_does_not_mutate_runtime_state() {
        let mut engine = engine();
        engine.execute(5, &BTreeMap::new()).unwrap();
        let samples = engine.simulate(10, 4).unwrap();
        assert_eq!(
            samples
                .iter()
                .map(|row| row.cycle_index)
                .collect::<Vec<_>>(),
            [3, 5, 8, 10]
        );
        assert_eq!(engine.snapshot().cycle_index, 5);
        assert_eq!(engine.simulate(3, 10).unwrap().len(), 3);
        assert_eq!(engine.simulate(0, 10).unwrap()[0].cycle_index, 0);
        assert!(engine.simulate(10, 0).is_err());
    }

    #[test]
    fn generation_streams_valid_parquet_and_protects_paths() {
        let engine = engine();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("df-generate-{unique}.parquet"));
        engine.generate(10, 4, &path).unwrap();
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(fs::File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        let cycles = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        assert_eq!(
            (0..cycles.len())
                .map(|index| cycles.value(index))
                .collect::<Vec<_>>(),
            [4, 8, 10]
        );
        assert_eq!(engine.snapshot().cycle_index, 0);
        assert!(engine.generate(1, 1, &path).is_err());
        assert!(engine
            .generate(1, 0, &path.with_extension("other.parquet"))
            .is_err());
        assert!(engine.generate(1, 1, &path.with_extension("csv")).is_err());
        fs::remove_file(path).unwrap();

        let reserved = std::env::temp_dir().join(format!("df-generate-reserved-{unique}.parquet"));
        let reserved_name = reserved.file_name().unwrap().to_str().unwrap();
        let temporary =
            reserved.with_file_name(format!(".{reserved_name}.{}.tmp", std::process::id()));
        fs::write(&temporary, b"reserved").unwrap();
        assert!(engine.generate(1, 1, &reserved).is_err());
        fs::remove_file(temporary).unwrap();

        let missing_parent = std::env::temp_dir()
            .join(format!("df-missing-{unique}"))
            .join("out.parquet");
        assert!(engine.generate(1, 1, &missing_parent).is_err());

        let zero = std::env::temp_dir().join(format!("df-generate-zero-{unique}.parquet"));
        engine.generate(0, 1, &zero).unwrap();
        fs::remove_file(zero).unwrap();
    }

    #[test]
    fn parquet_generation_preserves_all_dsl_types_and_rejects_corrupt_schema() {
        let source = "dsl_version: 2\nmetadata: {name: types}\nclock: {cycle_time: 1}\nprogram:\n  - {name: float, type: Variable, value: 1.5}\n  - {name: integer, type: Variable, value: 2}\n  - {name: boolean, type: Variable, value: true}\n  - {name: string, type: Variable, value: text}\nexport: {default_columns: [float, integer, boolean, string]}\n";
        let engine = Engine::from_bytes(&compile_source(source).unwrap()).unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("df-types-{unique}.parquet"));
        engine.generate(1, 1, &path).unwrap();
        let batch = ParquetRecordBatchReaderBuilder::try_new(fs::File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(batch.num_columns(), 6);
        fs::remove_file(path).unwrap();

        let mut corrupt = engine;
        let schema = Arc::new(corrupt.parquet_schema().unwrap());
        for (name, wrong) in [
            ("float", Value::String("wrong".into())),
            ("integer", Value::Bool(false)),
            ("boolean", Value::Int(0)),
            ("string", Value::Float(0.0)),
        ] {
            let mut row = corrupt.snapshot();
            row.values.insert(name.into(), wrong);
            assert!(corrupt.parquet_batch(schema.clone(), &[row]).is_err());
        }
        corrupt.project.export_tags.push("missing".into());
        assert!(corrupt.parquet_schema().is_err());
        corrupt.project.export_tags.pop();
        corrupt.project.tags[0].data_type = "UNKNOWN".into();
        assert!(corrupt.parquet_schema().is_err());
        assert!(corrupt
            .parquet_batch(schema.clone(), &[corrupt.snapshot()])
            .is_err());
        corrupt.project.tags[0].data_type = "FLOAT64".into();
        assert!(corrupt
            .parquet_batch(Arc::new(Schema::empty()), &[])
            .is_err());
    }

    #[test]
    fn every_classic_template_executes_in_engine() {
        for name in [
            "expression_demo.yaml",
            "lag_demo.yaml",
            "tank_level_control.yaml",
            "two_tank_pid.yaml",
            "two_tank_open_loop.yaml",
            "two_tank_plant.yaml",
        ] {
            let source = fs::read_to_string(format!("../../template/{name}")).unwrap();
            let mut engine = Engine::from_bytes(&compile_source(&source).unwrap()).unwrap();
            let snapshot = engine.execute(100, &BTreeMap::new()).unwrap();
            assert_eq!(snapshot.cycle_index, 100, "{name}");
            assert!(
                snapshot
                    .values
                    .values()
                    .all(|value| value.as_f64().is_none_or(f64::is_finite)),
                "{name}"
            );
        }
    }

    #[test]
    fn wave_expression_and_runtime_state_paths_execute() {
        let source="dsl_version: 2\nmetadata: {name: operators}\nclock: {cycle_time: 0.5}\nprogram:\n  - {name: wave, type: LIST_WAVE, params: {wave_list: [[1, 0.5], [2, 0.5]]}, inputs: {}}\n  - {name: calc, type: Expression, formula: 'abs(-wave.out) + cos(0) + sqrt(4) - 2 / 2'}\n  - {name: noise, type: RANDOM, params: {L: 0, H: 1, max_step: 1}, inputs: {}}\nexport: {default_columns: [wave.out, calc, noise.out]}\n";
        let mut engine = Engine::from_bytes(&compile_source(source).unwrap()).unwrap();
        let snapshot = engine.execute(3, &BTreeMap::new()).unwrap();
        assert_eq!(snapshot.values["wave.out"], Value::Float(1.0));
        assert_eq!(snapshot.values["calc"], Value::Float(3.0));
        engine.random_states.clear();
        assert!(matches!(
            engine.execute(1, &BTreeMap::new()),
            Err(EngineError::InvalidProject(_))
        ));
        assert!(eval_expression(&[ExpressionOp::Slot(0)], &[Value::String("x".into())]).is_err());
        assert!(eval_expression(&[ExpressionOp::Add], &[]).is_err());
        assert!(eval_expression(
            &[
                ExpressionOp::Number(1.0),
                ExpressionOp::Number(0.0),
                ExpressionOp::Div
            ],
            &[]
        )
        .is_err());
    }

    #[test]
    fn zero_cycles_only_reads_snapshot() {
        let mut engine = engine();
        let before = engine.snapshot();
        let after = engine.execute(0, &BTreeMap::new()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn batched_cycles_equal_individual_cycles() {
        let mut batch = engine();
        let mut singles = engine();
        let batched = batch.execute(20, &BTreeMap::new()).unwrap();
        for _ in 0..20 {
            singles.execute(1, &BTreeMap::new()).unwrap();
        }
        assert_eq!(batched, singles.snapshot());
    }

    #[test]
    fn invalid_write_is_atomic_and_does_not_advance() {
        let mut engine = engine();
        let before = engine.snapshot();
        let writes = BTreeMap::from([
            ("plant_inlet_flow".into(), Value::Float(0.002)),
            ("tank_1.level".into(), Value::Float(0.9)),
        ]);
        assert!(matches!(
            engine.execute(1, &writes),
            Err(EngineError::NotWritable(_))
        ));
        assert_eq!(before, engine.snapshot());
    }

    #[test]
    fn simulation_time_uses_project_cycle_time() {
        let mut engine = engine();
        let snapshot = engine.execute(3, &BTreeMap::new()).unwrap();
        assert_eq!(snapshot.cycle_index, 3);
        assert_eq!(snapshot.simulation_time, 1.5);
        assert_ne!(snapshot.values["tank_1.level"], Value::Float(0.15));
    }

    #[test]
    fn loads_file_and_exposes_all_fixed_resources() {
        let bytes = compile_source(SOURCE).unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("df-engine-load-{unique}.dfproject"));
        fs::write(&path, bytes).unwrap();
        let engine = Engine::load(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(engine.metadata().name, "two_tank");
        assert_eq!(engine.catalog().len(), 7);
        assert_eq!(engine.get("project_metadata").unwrap()["cycle_time"], 0.5);
        assert!(engine.get("default_trends").unwrap().is_array());
        assert!(engine.get("default_export_tags").unwrap().is_array());
        assert!(engine.get("tag_schema").unwrap().is_array());
        assert_eq!(engine.get("runtime_status").unwrap()["cycle_index"], 0);
        assert!(matches!(
            engine.get("unknown"),
            Err(EngineError::InvalidRequest(_))
        ));
    }

    #[test]
    fn rejects_corrupted_and_incompatible_compiled_projects() {
        assert!(matches!(
            Engine::from_bytes(b"short"),
            Err(EngineError::InvalidProject(_))
        ));
        let mut bytes = compile_source(SOURCE).unwrap();
        bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
        assert!(matches!(
            Engine::from_bytes(&bytes),
            Err(EngineError::InvalidProject(_))
        ));

        let valid = compile_source(SOURCE).unwrap();
        let mut project: CompiledProject = serde_json::from_slice(&valid[12..]).unwrap();
        for (format, abi) in [(99, ENGINE_ABI_VERSION), (PROJECT_FORMAT_VERSION, 99)] {
            project.format_version = format;
            project.engine_abi_version = abi;
            let bytes = encode(&project);
            assert!(matches!(
                Engine::from_bytes(&bytes),
                Err(EngineError::InvalidProject(_))
            ));
        }
        let mut bad_json = Vec::from(MAGIC);
        bad_json.extend_from_slice(&1_u32.to_le_bytes());
        bad_json.push(b'{');
        assert!(matches!(
            Engine::from_bytes(&bad_json),
            Err(EngineError::InvalidProject(_))
        ));
    }

    #[test]
    fn validates_every_write_type_and_request_rule() {
        let source = "dsl_version: 2\nmetadata: {name: types}\nclock: {cycle_time: 1}\nprogram:\n  - {name: input, type: Variable, value: 0.0, writable: true}\n  - {name: flag, type: Variable, value: false, writable: true}\n  - {name: count, type: Variable, value: 0, writable: true}\n  - {name: label, type: Variable, value: old, writable: true}\n  - {name: tank, type: CYLINDRICAL_TANK, params: {height: 1, radius: 0.2, outlet_area: 0.001, initial_level: 0}, inputs: {inlet_flow: input}}\nexport: {default_columns: [input, flag, count, label]}\n";
        let mut engine = Engine::from_bytes(&compile_source(source).unwrap()).unwrap();
        let writes = BTreeMap::from([
            ("input".into(), Value::Int(1)),
            ("flag".into(), Value::Bool(true)),
            ("count".into(), Value::Int(2)),
            ("label".into(), Value::String("new".into())),
        ]);
        let snapshot = engine.execute(1, &writes).unwrap();
        assert_eq!(snapshot.values["input"], Value::Float(1.0));
        assert_eq!(snapshot.values["flag"], Value::Bool(true));
        assert!(matches!(
            engine.execute(0, &writes),
            Err(EngineError::InvalidRequest(_))
        ));
        assert!(matches!(
            engine.execute(1, &BTreeMap::from([("missing".into(), Value::Int(1))])),
            Err(EngineError::UnknownTag(_))
        ));
        for (name, value) in [
            ("flag", Value::Int(1)),
            ("count", Value::Float(1.0)),
            ("label", Value::Bool(true)),
            ("input", Value::Float(f64::NAN)),
        ] {
            assert!(matches!(
                engine.execute(1, &BTreeMap::from([(name.into(), value)])),
                Err(EngineError::TypeMismatch(_))
            ));
        }
    }

    #[test]
    fn handles_zero_level_clamping_and_rejects_invalid_internal_slots() {
        let source = "dsl_version: 2\nmetadata: {name: bounds}\nclock: {cycle_time: 1}\nprogram:\n  - {name: input, type: Variable, value: 100.0, writable: true}\n  - {name: tank, type: CYLINDRICAL_TANK, params: {height: 1, radius: 0.1, outlet_area: 0.001, initial_level: 0}, inputs: {inlet_flow: input}}\nexport: {default_columns: [input]}\n";
        let bytes = compile_source(source).unwrap();
        let mut engine = Engine::from_bytes(&bytes).unwrap();
        let snapshot = engine.execute(1, &BTreeMap::new()).unwrap();
        assert_eq!(snapshot.values["tank.outlet_flow"], Value::Float(0.0));
        assert_eq!(snapshot.values["tank.level"], Value::Float(1.0));

        let mut project: CompiledProject = serde_json::from_slice(&bytes[12..]).unwrap();
        project.tags.push(TagDescriptor {
            name: "bad".into(),
            data_type: "STRING".into(),
            writable: false,
            unit: String::new(),
            description: String::new(),
            initial: Value::String("x".into()),
        });
        let bad_slot = project.tags.len() - 1;
        let CompiledNode::CylindricalTank { inlet_slot, .. } = &mut project.nodes[1] else {
            unreachable!()
        };
        *inlet_slot = bad_slot;
        let mut engine = Engine::from_bytes(&encode(&project)).unwrap();
        assert!(matches!(
            engine.execute(1, &BTreeMap::new()),
            Err(EngineError::InvalidProject(_))
        ));

        let mut project: CompiledProject = serde_json::from_slice(&bytes[12..]).unwrap();
        let CompiledNode::CylindricalTank { name, .. } = &mut project.nodes[1] else {
            unreachable!()
        };
        *name = "missing_state_tag".into();
        let mut engine = Engine::from_bytes(&encode(&project)).unwrap();
        assert!(engine.execute(1, &BTreeMap::new()).is_ok());

        project.tags.push(TagDescriptor {
            name: "bad".into(),
            data_type: "STRING".into(),
            writable: false,
            unit: String::new(),
            description: String::new(),
            initial: Value::String("x".into()),
        });
        let CompiledNode::CylindricalTank {
            inlet_slot,
            level_slot,
            name,
            ..
        } = &mut project.nodes[1]
        else {
            unreachable!()
        };
        *inlet_slot = 0;
        *level_slot = project.tags.len() - 1;
        *name = "missing_state_tag".into();
        let mut engine = Engine::from_bytes(&encode(&project)).unwrap();
        assert!(matches!(
            engine.execute(1, &BTreeMap::new()),
            Err(EngineError::InvalidProject(_))
        ));
    }

    fn encode(project: &CompiledProject) -> Vec<u8> {
        let payload = serde_json::to_vec(project).unwrap();
        let mut bytes = Vec::from(MAGIC);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }
}
