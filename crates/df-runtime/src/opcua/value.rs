//! Mapping between the DataFactory tag model and OPC UA types
//! (design §7.3 NodeId rules and §7.4 type/metadata mapping).

use df_core::Value;
use opcua::types::{DataTypeId, UAString, Variant};

/// OPC UA data type for a DataFactory tag type name (§7.4). Unknown type
/// names are rejected by the address-space builder, never guessed.
pub fn ua_data_type(tag_type: &str) -> Option<DataTypeId> {
    match tag_type {
        "BOOL" => Some(DataTypeId::Boolean),
        "INT64" => Some(DataTypeId::Int64),
        "FLOAT64" => Some(DataTypeId::Double),
        "STRING" => Some(DataTypeId::String),
        _ => None,
    }
}

/// Convert a DataFactory value into an OPC UA variant of the same type.
pub fn value_to_variant(value: &Value) -> Variant {
    match value {
        Value::Bool(v) => Variant::Boolean(*v),
        Value::Int(v) => Variant::Int64(*v),
        Value::Float(v) => Variant::Double(*v),
        Value::String(v) => Variant::String(UAString::from(v.as_str())),
    }
}

/// Convert an OPC UA variant into a DataFactory value of exactly `tag_type`.
/// Everything else — including `Variant::Empty` and numeric near-misses — is
/// rejected: the write path must not silently coerce types (WR-03).
pub fn variant_to_value(variant: &Variant, tag_type: &str) -> Option<Value> {
    match (tag_type, variant) {
        ("BOOL", Variant::Boolean(v)) => Some(Value::Bool(*v)),
        ("INT64", Variant::Int64(v)) => Some(Value::Int(*v)),
        ("FLOAT64", Variant::Double(v)) => Some(Value::Float(*v)),
        ("STRING", Variant::String(v)) => Some(Value::String(v.as_ref().to_string())),
        _ => None,
    }
}

/// Percent-encode a NodeId path component (§7.3). RFC 3986 unreserved
/// characters pass through unchanged; every other byte becomes uppercase
/// `%XX`, so a `/` inside a tag name can never collide with the path
/// structure and the encoding stays reversible.
pub fn percent_encode(component: &str) -> String {
    let mut encoded = String::with_capacity(component.len());
    for byte in component.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// NodeId string identifier of a tag variable (§7.3):
/// `df/instances/{instance_id}/tags/{percent_encoded_tag_name}`.
///
/// Instance ids are restricted by the Host configuration validator (1–64
/// characters of letters, digits, underscore and hyphen), so only the tag
/// name needs percent-encoding.
pub fn tag_node_identifier(instance_id: &str, tag_name: &str) -> String {
    format!(
        "df/instances/{instance_id}/tags/{}",
        percent_encode(tag_name)
    )
}

/// NodeId string identifier of an instance status variable (§7.3).
pub fn status_node_identifier(instance_id: &str, status_name: &str) -> String {
    format!("df/instances/{instance_id}/status/{status_name}")
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn data_type_matrix_matches_the_design_table() {
        assert_eq!(ua_data_type("BOOL"), Some(DataTypeId::Boolean));
        assert_eq!(ua_data_type("INT64"), Some(DataTypeId::Int64));
        assert_eq!(ua_data_type("FLOAT64"), Some(DataTypeId::Double));
        assert_eq!(ua_data_type("STRING"), Some(DataTypeId::String));
        // Unknown, lowercase and empty names are rejected, never guessed.
        assert_eq!(ua_data_type("FLOAT32"), None);
        assert_eq!(ua_data_type("float64"), None);
        assert_eq!(ua_data_type(""), None);
    }

    #[test]
    fn value_variant_round_trip_preserves_the_type() {
        let values = [
            Value::Bool(true),
            Value::Int(-42),
            Value::Float(0.15),
            Value::String("steam".into()),
        ];
        let types = ["BOOL", "INT64", "FLOAT64", "STRING"];
        for (value, tag_type) in values.iter().zip(types) {
            let variant = value_to_variant(value);
            assert_eq!(
                variant_to_value(&variant, tag_type).as_ref(),
                Some(value),
                "round trip failed for {tag_type}"
            );
        }
    }

    #[test]
    fn variant_to_value_rejects_type_mismatches_and_empty() {
        // Numeric near-misses must not be silently coerced (WR-03).
        assert_eq!(variant_to_value(&Variant::Double(1.0), "INT64"), None);
        assert_eq!(variant_to_value(&Variant::Int64(1), "FLOAT64"), None);
        assert_eq!(variant_to_value(&Variant::Boolean(true), "FLOAT64"), None);
        assert_eq!(
            variant_to_value(&Variant::String(UAString::from("x")), "BOOL"),
            None
        );
        assert_eq!(variant_to_value(&Variant::Empty, "FLOAT64"), None);
        // Unknown tag types reject everything.
        assert_eq!(variant_to_value(&Variant::Double(1.0), "FLOAT32"), None);
        // A null UA string normalizes to an empty STRING value: the 0.19
        // UAString derefs to &str and renders null as "".
        assert_eq!(
            variant_to_value(&Variant::String(UAString::null()), "STRING"),
            Some(Value::String(String::new()))
        );
    }

    #[test]
    fn percent_encoding_keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(percent_encode("tank_2.level"), "tank_2.level");
        assert_eq!(percent_encode("PID-1~PV"), "PID-1~PV");
        assert_eq!(percent_encode("a/b"), "a%2Fb");
        assert_eq!(percent_encode("100%"), "100%25");
        assert_eq!(percent_encode("level m"), "level%20m");
        assert_eq!(percent_encode("温度"), "%E6%B8%A9%E5%BA%A6");
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn node_identifiers_follow_the_design_paths() {
        assert_eq!(
            tag_node_identifier("tank-a", "tank_2.level"),
            "df/instances/tank-a/tags/tank_2.level"
        );
        assert_eq!(
            tag_node_identifier("tank-a", "weird/tag name"),
            "df/instances/tank-a/tags/weird%2Ftag%20name"
        );
        assert_eq!(
            status_node_identifier("tank-a", "CycleIndex"),
            "df/instances/tank-a/status/CycleIndex"
        );
    }
}
