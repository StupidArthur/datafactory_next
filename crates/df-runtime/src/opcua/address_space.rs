//! Tag Catalog → OPC UA address space (design §7.1–§7.4).
//!
//! Builds the `DataFactory` folder tree with one `Status`/`Tags` pair per
//! instance and returns an index that the publisher (R7) and the write
//! handler (R8) use to map between instance/tag names and NodeIds.

use std::collections::{BTreeMap, BTreeSet};

use df_core::TagDescriptor;
use opcua::nodes::{AccessLevel, VariableBuilder};
use opcua::server::address_space::AddressSpace;
use opcua::server::node_manager::memory::{InMemoryNodeManager, InMemoryNodeManagerImpl};
use opcua::types::{DataTypeId, NodeId, UAString, Variant};

use super::value::{status_node_identifier, tag_node_identifier, ua_data_type, value_to_variant};

/// Tags of one runtime instance, taken from its compiled project catalog.
#[derive(Debug, Clone)]
pub struct InstanceCatalog {
    pub instance_id: String,
    pub tags: Vec<TagDescriptor>,
}

/// Address-space location and metadata of one tag variable.
#[derive(Debug, Clone)]
pub struct TagEntry {
    pub node_id: NodeId,
    /// DataFactory type name (`BOOL`/`INT64`/`FLOAT64`/`STRING`).
    pub data_type: String,
    pub writable: bool,
}

/// Node ids of one instance's address-space subtree.
#[derive(Debug, Clone)]
pub struct InstanceNodes {
    pub folder: NodeId,
    pub status_folder: NodeId,
    pub tags_folder: NodeId,
    /// Status variable nodes by name (§7.2).
    pub status: BTreeMap<String, NodeId>,
    /// Tag variables by tag name (§7.2 uses full tag names as BrowseName).
    pub tags: BTreeMap<String, TagEntry>,
}

/// Index of everything the builder created in the server namespace.
#[derive(Debug, Clone, Default)]
pub struct AddressSpaceIndex {
    pub root: Option<NodeId>,
    pub server_folder: Option<NodeId>,
    pub server_status: BTreeMap<String, NodeId>,
    pub instances: BTreeMap<String, InstanceNodes>,
}

impl AddressSpaceIndex {
    /// Entry of one tag variable.
    pub fn tag(&self, instance_id: &str, tag_name: &str) -> Option<&TagEntry> {
        self.instances.get(instance_id)?.tags.get(tag_name)
    }

    /// Node id of one instance status variable.
    pub fn status_node(&self, instance_id: &str, status_name: &str) -> Option<&NodeId> {
        self.instances.get(instance_id)?.status.get(status_name)
    }
}

/// Server status variables (§7.2), in tree order.
pub const SERVER_STATUS_NODES: &[(&str, DataTypeId)] = &[
    ("State", DataTypeId::String),
    ("InstanceCount", DataTypeId::UInt32),
    ("RunningInstanceCount", DataTypeId::UInt32),
];

/// Instance status variables (§7.2), in tree order.
pub const INSTANCE_STATUS_NODES: &[(&str, DataTypeId)] = &[
    ("State", DataTypeId::String),
    ("CycleTime", DataTypeId::Double),
    ("CycleIndex", DataTypeId::UInt64),
    ("SimulationTime", DataTypeId::Double),
    ("LastCycleDurationMs", DataTypeId::Double),
    ("OverrunCount", DataTypeId::UInt64),
    ("MissedDeadlines", DataTypeId::UInt64),
    ("RejectedWriteBatches", DataTypeId::UInt64),
];

/// Initial value of a status variable. Live values are written by the
/// snapshot publisher (R7) and the monitor (R10); the address space starts
/// with the neutral defaults below.
fn status_initial(name: &str, instance_count: u32) -> Variant {
    match name {
        "State" => Variant::String(UAString::from("Starting")),
        "InstanceCount" => Variant::UInt32(instance_count),
        "RunningInstanceCount" => Variant::UInt32(0),
        "CycleTime" | "SimulationTime" | "LastCycleDurationMs" => Variant::Double(0.0),
        "CycleIndex" | "OverrunCount" | "MissedDeadlines" | "RejectedWriteBatches" => {
            Variant::UInt64(0)
        }
        _ => Variant::Empty,
    }
}

/// Structural validation of catalogs before any node is inserted: duplicate
/// instance ids or tag names and unknown tag types must be reported up front
/// instead of leaving a half-built address space behind (§4.2 strictness).
pub fn validate_catalogs(catalogs: &[InstanceCatalog]) -> Result<(), String> {
    let mut instance_ids = BTreeSet::new();
    for catalog in catalogs {
        if catalog.instance_id.trim().is_empty() {
            return Err("instance id must not be empty".into());
        }
        if !instance_ids.insert(catalog.instance_id.clone()) {
            return Err(format!("duplicate instance id {}", catalog.instance_id));
        }
        let mut tag_names = BTreeSet::new();
        for tag in &catalog.tags {
            if tag.name.trim().is_empty() {
                return Err(format!(
                    "instance {}: tag name must not be empty",
                    catalog.instance_id
                ));
            }
            if !tag_names.insert(tag.name.clone()) {
                return Err(format!(
                    "instance {}: duplicate tag name {}",
                    catalog.instance_id, tag.name
                ));
            }
            if ua_data_type(&tag.data_type).is_none() {
                return Err(format!(
                    "instance {}: tag {} has unsupported type {}",
                    catalog.instance_id, tag.name, tag.data_type
                ));
            }
        }
    }
    Ok(())
}

fn add_folder(
    space: &mut AddressSpace,
    node_id: &NodeId,
    name: &str,
    parent: &NodeId,
) -> Result<(), String> {
    if !space.add_folder(node_id, name, name, parent) {
        return Err(format!("cannot create the {name} folder ({node_id})"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_variable(
    space: &mut AddressSpace,
    node_id: &NodeId,
    browse_name: &str,
    parent: &NodeId,
    data_type: DataTypeId,
    initial: Variant,
    writable: bool,
    description: Option<&str>,
) -> Result<(), String> {
    let mut builder = VariableBuilder::new(node_id, browse_name, browse_name)
        .organized_by(parent)
        // async-opcua 0.19 requires an explicit data type; an invalid
        // variable panics on insert instead of returning an error.
        .data_type(data_type)
        .value(initial);
    if writable {
        builder = builder.user_access_level(AccessLevel::CURRENT_READ | AccessLevel::CURRENT_WRITE);
    }
    if let Some(text) = description {
        builder = builder.description(text);
    }
    if !builder.insert(&mut *space) {
        return Err(format!(
            "cannot create the variable {browse_name} ({node_id})"
        ));
    }
    Ok(())
}

/// Build the §7.2 address space for the given instance catalogs. `ns` is the
/// namespace index resolved from the configured `namespace_uri`. Generic over
/// the in-memory manager's impl so tests can use the stock manager while the
/// server uses the runtime write handler.
pub(crate) fn build_address_space<TImpl: InMemoryNodeManagerImpl>(
    manager: &InMemoryNodeManager<TImpl>,
    namespace_index: u16,
    catalogs: &[InstanceCatalog],
) -> Result<AddressSpaceIndex, String> {
    validate_catalogs(catalogs)?;
    let ns = namespace_index;
    let node = |identifier: &str| NodeId::new(ns, identifier);
    let mut index = AddressSpaceIndex::default();
    let instance_count = catalogs.len() as u32;

    let mut space = manager.address_space().write();

    // DataFactory root folder under Objects (§7.2).
    let root_id = node("df");
    add_folder(
        &mut space,
        &root_id,
        "DataFactory",
        &NodeId::objects_folder_id(),
    )?;
    index.root = Some(root_id.clone());

    // ServerStatus folder (§7.2).
    let server_id = node("df/server");
    add_folder(&mut space, &server_id, "ServerStatus", &root_id)?;
    index.server_folder = Some(server_id.clone());
    for (name, data_type) in SERVER_STATUS_NODES {
        let id = node(&format!("df/server/status/{name}"));
        insert_variable(
            &mut space,
            &id,
            name,
            &server_id,
            *data_type,
            status_initial(name, instance_count),
            false,
            None,
        )?;
        index.server_status.insert(name.to_string(), id);
    }

    // Instances folder and one subtree per instance (§7.2).
    let instances_id = node("df/instances");
    add_folder(&mut space, &instances_id, "Instances", &root_id)?;

    for catalog in catalogs {
        let instance_id = &catalog.instance_id;
        let folder_id = node(&format!("df/instances/{instance_id}"));
        add_folder(&mut space, &folder_id, instance_id, &instances_id)?;
        let status_folder_id = node(&format!("df/instances/{instance_id}/status"));
        add_folder(&mut space, &status_folder_id, "Status", &folder_id)?;
        let tags_folder_id = node(&format!("df/instances/{instance_id}/tags"));
        add_folder(&mut space, &tags_folder_id, "Tags", &folder_id)?;

        let mut status = BTreeMap::new();
        for (name, data_type) in INSTANCE_STATUS_NODES {
            let id = node(&status_node_identifier(instance_id, name));
            insert_variable(
                &mut space,
                &id,
                name,
                &status_folder_id,
                *data_type,
                status_initial(name, instance_count),
                false,
                None,
            )?;
            status.insert(name.to_string(), id);
        }

        let mut tags = BTreeMap::new();
        for descriptor in &catalog.tags {
            let identifier = tag_node_identifier(instance_id, &descriptor.name);
            let tag_id = node(&identifier);
            let data_type = ua_data_type(&descriptor.data_type)
                .ok_or_else(|| format!("unsupported tag type {}", descriptor.data_type))?;
            let description = if descriptor.description.is_empty() {
                None
            } else {
                Some(descriptor.description.as_str())
            };
            insert_variable(
                &mut space,
                &tag_id,
                &descriptor.name,
                &tags_folder_id,
                data_type,
                value_to_variant(&descriptor.initial),
                descriptor.writable,
                description,
            )?;

            // Engineering unit as its own browsable property (§7.4): the
            // unit must never be dropped, and a String property keeps the
            // dependency surface small.
            if !descriptor.unit.is_empty() {
                let unit_id = node(&format!("{identifier}/unit"));
                if !VariableBuilder::new(&unit_id, "EngineeringUnit", "EngineeringUnit")
                    .property_of(&tag_id)
                    .data_type(DataTypeId::String)
                    .value(Variant::String(UAString::from(descriptor.unit.as_str())))
                    .insert(&mut *space)
                {
                    return Err(format!("cannot create the unit property of {identifier}"));
                }
            }

            tags.insert(
                descriptor.name.clone(),
                TagEntry {
                    node_id: tag_id,
                    data_type: descriptor.data_type.clone(),
                    writable: descriptor.writable,
                },
            );
        }

        index.instances.insert(
            instance_id.clone(),
            InstanceNodes {
                folder: folder_id,
                status_folder: status_folder_id,
                tags_folder: tags_folder_id,
                status,
                tags,
            },
        );
    }

    Ok(index)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use df_core::Value;

    fn tag(name: &str, data_type: &str, writable: bool) -> TagDescriptor {
        TagDescriptor {
            name: name.into(),
            data_type: data_type.into(),
            writable,
            unit: String::new(),
            description: String::new(),
            initial: Value::Float(0.0),
        }
    }

    #[test]
    fn catalogs_reject_duplicates_and_unknown_types() {
        let ok = [InstanceCatalog {
            instance_id: "tank-a".into(),
            tags: vec![tag("level", "FLOAT64", false), tag("sv", "FLOAT64", true)],
        }];
        assert!(validate_catalogs(&ok).is_ok());

        // Duplicate instance id.
        let duplicated = [
            InstanceCatalog {
                instance_id: "tank-a".into(),
                tags: vec![],
            },
            InstanceCatalog {
                instance_id: "tank-a".into(),
                tags: vec![],
            },
        ];
        let error = validate_catalogs(&duplicated).unwrap_err();
        assert!(error.contains("duplicate instance id"), "{error}");

        // Duplicate tag name inside one instance.
        let duplicate_tag = [InstanceCatalog {
            instance_id: "tank-a".into(),
            tags: vec![
                tag("level", "FLOAT64", false),
                tag("level", "FLOAT64", false),
            ],
        }];
        let error = validate_catalogs(&duplicate_tag).unwrap_err();
        assert!(error.contains("duplicate tag name"), "{error}");

        // Unknown tag type.
        let unknown_type = [InstanceCatalog {
            instance_id: "tank-a".into(),
            tags: vec![tag("level", "FLOAT32", false)],
        }];
        let error = validate_catalogs(&unknown_type).unwrap_err();
        assert!(error.contains("unsupported type"), "{error}");

        // Empty instance id and empty tag name.
        let empty_instance = [InstanceCatalog {
            instance_id: "  ".into(),
            tags: vec![],
        }];
        assert!(validate_catalogs(&empty_instance)
            .unwrap_err()
            .contains("instance id"));
        let empty_tag = [InstanceCatalog {
            instance_id: "tank-a".into(),
            tags: vec![tag(" ", "FLOAT64", false)],
        }];
        assert!(validate_catalogs(&empty_tag)
            .unwrap_err()
            .contains("tag name"));
    }

    #[test]
    fn status_initial_values_follow_the_node_types() {
        assert_eq!(
            status_initial("State", 2),
            Variant::String(UAString::from("Starting"))
        );
        assert_eq!(status_initial("InstanceCount", 2), Variant::UInt32(2));
        assert_eq!(
            status_initial("RunningInstanceCount", 2),
            Variant::UInt32(0)
        );
        assert_eq!(status_initial("CycleTime", 2), Variant::Double(0.0));
        assert_eq!(status_initial("CycleIndex", 2), Variant::UInt64(0));
        assert_eq!(
            status_initial("RejectedWriteBatches", 2),
            Variant::UInt64(0)
        );
    }
}
