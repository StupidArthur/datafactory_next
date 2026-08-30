use crate::error::{EngineError, Result};
use crate::format::{
    CompiledNode, CompiledProject, ExpressionOp, TagDescriptor, TrendDescriptor, Value,
    ENGINE_ABI_VERSION, MAGIC, PROJECT_FORMAT_VERSION,
};
use serde_yaml::{Mapping, Value as Yaml};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Clone, Copy)]
enum NodeKind {
    Variable,
    Expression,
    Lag,
    SineWave,
    ListWave,
    Random,
    Valve,
    Tank,
    Pid,
}
impl NodeKind {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_uppercase().as_str() {
            "VARIABLE" => Ok(Self::Variable),
            "EXPRESSION" => Ok(Self::Expression),
            "LAG" => Ok(Self::Lag),
            "SINE_WAVE" => Ok(Self::SineWave),
            "LIST_WAVE" => Ok(Self::ListWave),
            "RANDOM" => Ok(Self::Random),
            "VALVE" => Ok(Self::Valve),
            "CYLINDRICAL_TANK" => Ok(Self::Tank),
            "PID" => Ok(Self::Pid),
            _ => invalid(&format!("unsupported node type: {value}")),
        }
    }
}

pub fn compile_file(source_path: &Path, output_path: &Path) -> Result<()> {
    let source = fs::read_to_string(source_path)?;
    fs::write(output_path, compile_source(&source)?)?;
    Ok(())
}

pub fn compile_source(source: &str) -> Result<Vec<u8>> {
    let root: Yaml =
        serde_yaml::from_str(source).map_err(|e| EngineError::InvalidSource(e.to_string()))?;
    let root = mapping(&root, "root")?;
    if integer(required(root, "dsl_version")?, "dsl_version")? != 2 {
        return invalid("only dsl_version 2 is supported");
    }
    let metadata = root
        .get(Yaml::String("metadata".into()))
        .and_then(Yaml::as_mapping);
    let name = metadata
        .and_then(|m| m.get(Yaml::String("name".into())))
        .and_then(Yaml::as_str)
        .unwrap_or("unnamed")
        .to_owned();
    let clock = mapping(required(root, "clock")?, "clock")?;
    let cycle_time = number(required(clock, "cycle_time")?, "clock.cycle_time")?;
    if !cycle_time.is_finite() || cycle_time <= 0.0 {
        return invalid("cycle_time must be finite and > 0");
    }
    let program = required(root, "program")?
        .as_sequence()
        .ok_or_else(|| source_error("program must be a sequence"))?;
    if program.is_empty() {
        return invalid("project must contain at least one program node");
    }
    let mut tags = Vec::new();
    let mut names = BTreeSet::new();
    let mut raw_nodes = Vec::new();
    for raw in program {
        let node = mapping(raw, "program node")?;
        let node_name = text(required(node, "name")?, "node.name")?.to_owned();
        validate_name(&node_name)?;
        let kind = NodeKind::parse(text(required(node, "type")?, "node.type")?)?;
        match kind {
            NodeKind::Variable => {
                let initial = yaml_value(required(node, "value")?)?;
                add_tag(
                    &mut tags,
                    &mut names,
                    &node_name,
                    initial,
                    boolean(node.get(Yaml::String("writable".into())), false)?,
                    "",
                    "变量",
                )?
            }
            NodeKind::Expression | NodeKind::Lag => {
                add_float(&mut tags, &mut names, &node_name, 0.0, false, "", "计算值")?
            }
            NodeKind::SineWave | NodeKind::ListWave | NodeKind::Random => add_float(
                &mut tags,
                &mut names,
                &format!("{node_name}.out"),
                0.0,
                false,
                "",
                "输出",
            )?,
            NodeKind::Valve => {
                let p = params(node)?;
                let opening = optional_number(p, "initial_opening", 0.0)?;
                for (suffix, value, unit) in [
                    ("target_opening", opening, "%"),
                    ("current_opening", opening, "%"),
                    ("outlet_flow", 0.0, "m3/s"),
                ] {
                    add_float(
                        &mut tags,
                        &mut names,
                        &format!("{node_name}.{suffix}"),
                        value,
                        false,
                        unit,
                        "阀门状态",
                    )?
                }
            }
            NodeKind::Tank => {
                let p = params(node)?;
                let initial = optional_number(p, "initial_level", 0.0)?;
                for (suffix, value, unit) in [
                    ("level", initial, "m"),
                    ("inlet_flow", 0.0, "m3/s"),
                    ("outlet_flow", 0.0, "m3/s"),
                ] {
                    add_float(
                        &mut tags,
                        &mut names,
                        &format!("{node_name}.{suffix}"),
                        value,
                        false,
                        unit,
                        "水箱状态",
                    )?
                }
            }
            NodeKind::Pid => {
                let p = params(node)?;
                for (suffix, value) in [
                    ("PV", optional_number(p, "PV", 0.0)?),
                    ("SV", optional_number(p, "SV", 0.0)?),
                    ("MV", optional_number(p, "MV", 0.0)?),
                    ("MODE", optional_number(p, "MODE", 5.0)?),
                ] {
                    add_float(
                        &mut tags,
                        &mut names,
                        &format!("{node_name}.{suffix}"),
                        value,
                        false,
                        "",
                        "PID状态",
                    )?
                }
            }
        }
        raw_nodes.push((node_name, kind, node));
    }
    let mut default_trends = Vec::new();
    for (node_name, _, node) in &raw_nodes {
        collect_display_args(node, node_name, &names, &mut default_trends)?;
    }
    let export = mapping(required(root, "export")?, "export")?;
    let export_tags = required(export, "default_columns")?
        .as_sequence()
        .ok_or_else(|| source_error("export.default_columns must be a sequence"))?
        .iter()
        .map(|value| text(value, "export.default_columns item").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    if export_tags.is_empty() {
        return invalid("export.default_columns must not be empty");
    }
    for name in &export_tags {
        if !names.contains(name) {
            return invalid(&format!("unknown export tag: {name}"));
        }
    }
    let slots: BTreeMap<String, usize> = tags
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    let mut nodes = Vec::new();
    for (node_name, kind, node) in raw_nodes {
        let slot = |r: &str| {
            slots
                .get(r)
                .copied()
                .ok_or_else(|| source_error(&format!("unknown reference: {r}")))
        };
        let compiled = match kind {
            NodeKind::Variable => CompiledNode::Variable,
            NodeKind::Expression => CompiledNode::Expression {
                output_slot: slot(&node_name)?,
                bytecode: compile_expression(text(required(node, "formula")?, "formula")?, &slots)?,
            },
            NodeKind::Lag => {
                let delay = integer(required(node, "delay")?, "delay")?;
                if delay <= 0 {
                    return invalid("Lag delay must be > 0");
                };
                CompiledNode::Lag {
                    source_slot: slot(text(required(node, "source")?, "source")?)?,
                    output_slot: slot(&node_name)?,
                    delay: delay as usize,
                }
            }
            NodeKind::SineWave => {
                let p = params(node)?;
                let period = required_number(p, "period")?;
                if period <= 0.0 {
                    return invalid("SINE_WAVE period must be > 0");
                };
                CompiledNode::SineWave {
                    output_slot: slot(&format!("{node_name}.out"))?,
                    amplitude: required_number(p, "amplitude")?,
                    period,
                    phase: optional_number(p, "phase", 0.0)?,
                    offset: optional_number(p, "offset", 0.0)?,
                }
            }
            NodeKind::ListWave => {
                let p = params(node)?;
                let list = required(p, "wave_list")?
                    .as_sequence()
                    .ok_or_else(|| source_error("wave_list must be a sequence"))?;
                let mut wave = Vec::new();
                for item in list {
                    let pair = item
                        .as_sequence()
                        .ok_or_else(|| source_error("wave item must be [value,time]"))?;
                    if pair.len() != 2 {
                        return invalid("wave item must contain value and time");
                    };
                    let duration = number(&pair[1], "wave duration")?;
                    if duration <= 0.0 {
                        return invalid("wave duration must be > 0");
                    };
                    wave.push((number(&pair[0], "wave value")?, duration));
                }
                if wave.is_empty() {
                    return invalid("wave_list must not be empty");
                };
                CompiledNode::ListWave {
                    output_slot: slot(&format!("{node_name}.out"))?,
                    wave_list: wave,
                }
            }
            NodeKind::Random => {
                let p = params(node)?;
                let low = required_number(p, "L")?;
                let high = required_number(p, "H")?;
                let max_step = required_number(p, "max_step")?;
                if high < low || max_step < 0.0 {
                    return invalid("invalid RANDOM parameters");
                };
                CompiledNode::Random {
                    output_slot: slot(&format!("{node_name}.out"))?,
                    low,
                    high,
                    max_step,
                    seed: stable_seed(&node_name),
                }
            }
            NodeKind::Valve => {
                let p = params(node)?;
                let ins = inputs(node)?;
                let travel = required_number(p, "full_travel_time")?;
                if travel <= 0.0 {
                    return invalid("full_travel_time must be > 0");
                };
                CompiledNode::Valve {
                    target_slot: slot(input(ins, "target_opening")?)?,
                    inlet_slot: slot(input(ins, "inlet_flow")?)?,
                    target_output_slot: slot(&format!("{node_name}.target_opening"))?,
                    opening_slot: slot(&format!("{node_name}.current_opening"))?,
                    outlet_slot: slot(&format!("{node_name}.outlet_flow"))?,
                    full_travel_time: travel,
                }
            }
            NodeKind::Tank => {
                let p = params(node)?;
                let ins = inputs(node)?;
                let height = required_number(p, "height")?;
                let radius = required_number(p, "radius")?;
                let outlet_area = required_number(p, "outlet_area")?;
                let initial = optional_number(p, "initial_level", 0.0)?;
                if height <= 0.0
                    || radius <= 0.0
                    || outlet_area < 0.0
                    || !(0.0..=height).contains(&initial)
                {
                    return invalid("invalid tank parameters");
                };
                CompiledNode::CylindricalTank {
                    name: node_name.clone(),
                    inlet_slot: slot(input(ins, "inlet_flow")?)?,
                    level_slot: slot(&format!("{node_name}.level"))?,
                    outlet_slot: slot(&format!("{node_name}.outlet_flow"))?,
                    height,
                    radius,
                    outlet_area,
                }
            }
            NodeKind::Pid => {
                let p = params(node)?;
                let ins = inputs(node)?;
                let sv_ref = ins.get(Yaml::String("SV".into())).and_then(Yaml::as_str);
                let pb = required_number(p, "PB")?;
                if pb == 0.0 {
                    return invalid("PID PB must not be zero");
                };
                CompiledNode::Pid {
                    pv_input_slot: slot(input(ins, "PV")?)?,
                    sv_input_slot: sv_ref.map(&slot).transpose()?,
                    pv_slot: slot(&format!("{node_name}.PV"))?,
                    sv_slot: slot(&format!("{node_name}.SV"))?,
                    mv_slot: slot(&format!("{node_name}.MV"))?,
                    mode_slot: slot(&format!("{node_name}.MODE"))?,
                    pb,
                    ti: optional_number(p, "TI", 0.0)?,
                    td: optional_number(p, "TD", 0.0)?,
                    sv_default: optional_number(p, "SV", 0.0)?,
                    mv_initial: optional_number(p, "MV", 0.0)?,
                    mv_low: optional_number(p, "MVL", 0.0)?,
                    mv_high: optional_number(p, "MVH", 100.0)?,
                }
            }
        };
        nodes.push(compiled);
    }
    let project = CompiledProject {
        format_version: PROJECT_FORMAT_VERSION,
        engine_abi_version: ENGINE_ABI_VERSION,
        name,
        cycle_time,
        tags,
        default_trends,
        export_tags,
        nodes,
    };
    let payload =
        serde_json::to_vec(&project).map_err(|e| EngineError::InvalidProject(e.to_string()))?;
    let mut output = Vec::with_capacity(12 + payload.len());
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    output.extend_from_slice(&payload);
    Ok(output)
}

fn collect_display_args(
    node: &Mapping,
    node_name: &str,
    names: &BTreeSet<String>,
    output: &mut Vec<TrendDescriptor>,
) -> Result<()> {
    let Some(value) = node.get(Yaml::String("display_args".into())) else {
        return Ok(());
    };
    let args = value
        .as_sequence()
        .ok_or_else(|| source_error("display_args must be a sequence"))?;
    for value in args {
        let spec = text(value, "display_args item")?;
        let (local_name, scale) =
            if let Some(open) = spec.strip_suffix(']').and_then(|s| s.rfind('[')) {
                let scale = spec[open + 1..spec.len() - 1]
                    .parse::<f64>()
                    .map_err(|_| source_error("display_args scale must be a number"))?;
                (&spec[..open], scale)
            } else {
                (spec, 100.0)
            };
        if !scale.is_finite() || scale <= 0.0 || local_name.is_empty() {
            return invalid("display_args name must be non-empty and scale must be finite and > 0");
        }
        let qualified = if names.contains(local_name) {
            local_name.to_owned()
        } else {
            format!("{node_name}.{local_name}")
        };
        if !names.contains(&qualified) {
            return invalid(&format!("unknown display_args tag: {qualified}"));
        }
        if !output.iter().any(|item| item.name == qualified) {
            output.push(TrendDescriptor {
                name: qualified,
                scale,
            });
        }
    }
    Ok(())
}

fn mapping<'a>(v: &'a Yaml, name: &str) -> Result<&'a Mapping> {
    v.as_mapping()
        .ok_or_else(|| source_error(&format!("{name} must be a mapping")))
}
fn required<'a>(m: &'a Mapping, key: &str) -> Result<&'a Yaml> {
    m.get(Yaml::String(key.into()))
        .ok_or_else(|| source_error(&format!("missing {key}")))
}
fn text<'a>(v: &'a Yaml, name: &str) -> Result<&'a str> {
    v.as_str()
        .ok_or_else(|| source_error(&format!("{name} must be a string")))
}
fn number(v: &Yaml, name: &str) -> Result<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|x| x as f64))
        .filter(|x| x.is_finite())
        .ok_or_else(|| source_error(&format!("{name} must be finite number")))
}
fn integer(v: &Yaml, name: &str) -> Result<i64> {
    v.as_i64()
        .ok_or_else(|| source_error(&format!("{name} must be integer")))
}
fn boolean(v: Option<&Yaml>, default: bool) -> Result<bool> {
    match v {
        None => Ok(default),
        Some(x) => x
            .as_bool()
            .ok_or_else(|| source_error("boolean value expected")),
    }
}
fn params(node: &Mapping) -> Result<&Mapping> {
    mapping(required(node, "params")?, "params")
}
fn inputs(node: &Mapping) -> Result<&Mapping> {
    mapping(required(node, "inputs")?, "inputs")
}
fn input<'a>(m: &'a Mapping, key: &str) -> Result<&'a str> {
    text(required(m, key)?, key)
}
fn required_number(m: &Mapping, key: &str) -> Result<f64> {
    number(required(m, key)?, key)
}
fn optional_number(m: &Mapping, key: &str, default: f64) -> Result<f64> {
    m.get(Yaml::String(key.into()))
        .map(|v| number(v, key))
        .unwrap_or(Ok(default))
}
fn yaml_value(v: &Yaml) -> Result<Value> {
    if let Some(x) = v.as_bool() {
        Ok(Value::Bool(x))
    } else if let Some(x) = v.as_i64() {
        Ok(Value::Int(x))
    } else if let Some(x) = v.as_f64() {
        Ok(Value::Float(x))
    } else if let Some(x) = v.as_str() {
        Ok(Value::String(x.into()))
    } else {
        invalid("Variable value must be scalar")
    }
}
fn add_float(
    tags: &mut Vec<TagDescriptor>,
    names: &mut BTreeSet<String>,
    name: &str,
    value: f64,
    writable: bool,
    unit: &str,
    description: &str,
) -> Result<()> {
    add_tag(
        tags,
        names,
        name,
        Value::Float(value),
        writable,
        unit,
        description,
    )
}
fn add_tag(
    tags: &mut Vec<TagDescriptor>,
    names: &mut BTreeSet<String>,
    name: &str,
    initial: Value,
    writable: bool,
    unit: &str,
    description: &str,
) -> Result<()> {
    validate_name(name)?;
    if !names.insert(name.into()) {
        return invalid(&format!("duplicate tag: {name}"));
    };
    tags.push(TagDescriptor {
        name: name.into(),
        data_type: initial.type_name().into(),
        writable,
        unit: unit.into(),
        description: description.into(),
        initial,
    });
    Ok(())
}
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return invalid(&format!("invalid name: {name}"));
    }
    Ok(())
}
fn stable_seed(name: &str) -> u64 {
    name.bytes().fold(1469598103934665603, |h, b| {
        (h ^ b as u64).wrapping_mul(1099511628211)
    })
}
fn source_error(message: &str) -> EngineError {
    EngineError::InvalidSource(message.into())
}
fn invalid<T>(message: &str) -> Result<T> {
    Err(source_error(message))
}

struct ExprParser<'a> {
    text: &'a str,
    pos: usize,
    slots: &'a BTreeMap<String, usize>,
}
fn compile_expression(text: &str, slots: &BTreeMap<String, usize>) -> Result<Vec<ExpressionOp>> {
    let mut p = ExprParser {
        text,
        pos: 0,
        slots,
    };
    let out = p.expression()?;
    p.space();
    if p.pos != text.len() {
        return invalid("invalid expression");
    };
    Ok(out)
}
impl ExprParser<'_> {
    fn space(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1
        }
    }
    fn take(&mut self, c: u8) -> bool {
        self.space();
        if self.text.as_bytes().get(self.pos) == Some(&c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expression(&mut self) -> Result<Vec<ExpressionOp>> {
        let mut out = self.term()?;
        loop {
            if self.take(b'+') {
                out.extend(self.term()?);
                out.push(ExpressionOp::Add)
            } else if self.take(b'-') {
                out.extend(self.term()?);
                out.push(ExpressionOp::Sub)
            } else {
                break;
            }
        }
        Ok(out)
    }
    fn term(&mut self) -> Result<Vec<ExpressionOp>> {
        let mut out = self.factor()?;
        loop {
            if self.take(b'*') {
                out.extend(self.factor()?);
                out.push(ExpressionOp::Mul)
            } else if self.take(b'/') {
                out.extend(self.factor()?);
                out.push(ExpressionOp::Div)
            } else {
                break;
            }
        }
        Ok(out)
    }
    fn factor(&mut self) -> Result<Vec<ExpressionOp>> {
        self.space();
        if self.take(b'-') {
            let mut out = vec![ExpressionOp::Number(0.0)];
            out.extend(self.factor()?);
            out.push(ExpressionOp::Sub);
            return Ok(out);
        }
        if self.take(b'(') {
            let out = self.expression()?;
            if !self.take(b')') {
                return invalid("missing closing parenthesis");
            };
            return Ok(out);
        }
        let start = self.pos;
        while self
            .text
            .as_bytes()
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
        {
            self.pos += 1
        }
        if self.pos > start {
            return self.text[start..self.pos]
                .parse::<f64>()
                .map(|v| vec![ExpressionOp::Number(v)])
                .map_err(|_| source_error("invalid number"));
        }
        while self
            .text
            .as_bytes()
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.')
        {
            self.pos += 1
        }
        if self.pos == start {
            return invalid("expression operand expected");
        };
        let name = &self.text[start..self.pos];
        if self.take(b'(') {
            let mut out = self.expression()?;
            if !self.take(b')') {
                return invalid("missing function parenthesis");
            };
            out.push(match name {
                "sin" => ExpressionOp::Sin,
                "cos" => ExpressionOp::Cos,
                "sqrt" => ExpressionOp::Sqrt,
                "abs" => ExpressionOp::Abs,
                _ => return invalid("unknown expression function"),
            });
            Ok(out)
        } else {
            Ok(vec![ExpressionOp::Slot(*self.slots.get(name).ok_or_else(
                || source_error(&format!("unknown expression reference: {name}")),
            )?)])
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn compiles_all_templates() {
        for name in [
            "expression_demo.yaml",
            "lag_demo.yaml",
            "tank_level_control.yaml",
            "two_tank_pid.yaml",
            "two_tank_open_loop.yaml",
            "two_tank_plant.yaml",
        ] {
            let source = fs::read_to_string(format!("../../template/{name}")).unwrap();
            assert!(compile_source(&source).is_ok(), "{name}");
        }
    }

    fn project(program: &str) -> String {
        format!("dsl_version: 2\nmetadata: {{name: test}}\nclock: {{cycle_time: 0.5}}\nprogram:\n  - {{name: export_sink, type: Variable, value: 0}}\n{program}export: {{default_columns: [export_sink]}}\n")
    }

    fn rejected(source: &str) {
        assert!(compile_source(source).is_err(), "{source}");
    }

    #[test]
    fn rejects_invalid_projects_and_expression_forms() {
        rejected(": bad");
        rejected("dsl_version: 1\nclock: {cycle_time: 1}\nprogram: []");
        rejected("dsl_version: 2\nclock: {cycle_time: 0}\nprogram: []");
        rejected("dsl_version: 2\nclock: {cycle_time: 1}\nprogram: []");
        rejected("dsl_version: 2\nclock: {cycle_time: 1}\nprogram: [{name: x, type: Variable, value: 1}]");
        rejected("dsl_version: 2\nclock: {cycle_time: 1}\nprogram: [{name: x, type: Variable, value: 1}]\nexport: {default_columns: []}");
        rejected("dsl_version: 2\nclock: {cycle_time: 1}\nprogram: [{name: x, type: Variable, value: 1}]\nexport: {default_columns: [missing]}");
        rejected(&project("  - {name: bad-name, type: Variable, value: 1}\n"));
        rejected(&project("  - {name: x, type: Unknown}\n"));
        rejected(&project("  - {name: x, type: Variable, value: [1]}\n"));
        rejected(&project(
            "  - {name: x, type: Variable, value: 1, display_args: ['x[0]']}\n",
        ));
        rejected(&project(
            "  - {name: x, type: Variable, value: 1, display_args: [missing]}\n",
        ));
        rejected(&project(
            "  - {name: x, type: Variable, value: 1}\n  - {name: x, type: Variable, value: 2}\n",
        ));
        rejected(&project("  - {name: x, type: Lag, source: x, delay: 0}\n"));
        rejected(&project(
            "  - {name: s, type: SINE_WAVE, params: {amplitude: 1, period: 0}, inputs: {}}\n",
        ));
        for wave in ["[]", "[[1]]", "[[1, 0]]"] {
            rejected(&project(&format!(
                "  - {{name: w, type: LIST_WAVE, params: {{wave_list: {wave}}}, inputs: {{}}}}\n"
            )));
        }
        rejected(&project(
            "  - {name: r, type: RANDOM, params: {L: 2, H: 1, max_step: 1}, inputs: {}}\n",
        ));
        rejected(&project("  - {name: x, type: Variable, value: 1}\n  - {name: v, type: VALVE, params: {full_travel_time: 0}, inputs: {target_opening: x, inlet_flow: x}}\n"));
        rejected(&project("  - {name: x, type: Variable, value: 1}\n  - {name: t, type: CYLINDRICAL_TANK, params: {height: 0, radius: 1, outlet_area: 0, initial_level: 0}, inputs: {inlet_flow: x}}\n"));
        rejected(&project("  - {name: x, type: Variable, value: 1}\n  - {name: p, type: PID, params: {PB: 0}, inputs: {PV: x}}\n"));
        for formula in ["1 2", "(1", "", "foo(1)", "sin(1", "missing + 1"] {
            rejected(&project(&format!(
                "  - {{name: e, type: Expression, formula: '{formula}'}}\n"
            )));
        }
    }

    #[test]
    fn expression_arithmetic_and_functions_compile() {
        for formula in [
            "2-1", "4/2", "-1", "(1+2)*3", "cos(0)", "sqrt(4)", "abs(-2)",
        ] {
            assert!(
                compile_source(&project(&format!(
                    "  - {{name: e, type: Expression, formula: '{formula}'}}\n"
                )))
                .is_ok(),
                "{formula}"
            );
        }
    }
}
