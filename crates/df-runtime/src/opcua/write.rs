//! OPC UA closed-loop write handler (design §8).
//!
//! A custom [`InMemoryNodeManagerImpl`] overrides only the `write` service:
//! every other service keeps the in-memory default, so browsing, reading and
//! subscriptions continue to work against the address space populated by the
//! publisher (§7.5).
//!
//! The write path never stores values into the address space; §7.5 makes the
//! publisher the single source of tag values. A write is validated against
//! the Tag Catalog (§8.1), submitted through the instance's
//! `RuntimeHandle::submit_writes` queue (§8.3) and answered only after the
//! Engine's cycle-boundary acknowledgement (WR-07, §8.5).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use opcua::core::sync::RwLock as UaRwLock;
use opcua::server::address_space::AddressSpace;
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{
    InMemoryNodeManager, InMemoryNodeManagerBuilder, InMemoryNodeManagerImpl,
};
use opcua::server::node_manager::{
    NodeManagerBuilder, ParsedWriteValue, RequestContext, ServerContext, WriteNode,
};
use opcua::types::{AttributeId, Identifier, NodeId, NumericRange, StatusCode};

use df_core::Value;

use crate::config::UserRole;
use crate::opcua::address_space::AddressSpaceIndex;
use crate::opcua::value::variant_to_value;
use crate::{RuntimeError, RuntimeHandle};

/// Wait bounds for the cycle-boundary acknowledgement (§8.5): the wait must
/// cover a full cycle, the cycle computation and a communication margin. The
/// derived wait (2× cycle_time + 2 s) is clamped to this range.
const MIN_WRITE_WAIT: Duration = Duration::from_secs(2);
const MAX_WRITE_WAIT: Duration = Duration::from_secs(30);
const WRITE_WAIT_MARGIN: Duration = Duration::from_secs(2);

/// Derive the write wait from the instance cycle time (§8.5.1).
fn derive_write_wait(cycle_time: f64) -> Duration {
    if !cycle_time.is_finite() || cycle_time <= 0.0 {
        return MIN_WRITE_WAIT;
    }
    (Duration::from_secs_f64(cycle_time * 2.0) + WRITE_WAIT_MARGIN)
        .clamp(MIN_WRITE_WAIT, MAX_WRITE_WAIT)
}

/// Address-space key of one node: our namespace uses string identifiers only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct NodeKey {
    namespace: u16,
    identifier: String,
}

fn node_key(node_id: &NodeId) -> Option<NodeKey> {
    match &node_id.identifier {
        Identifier::String(value) => Some(NodeKey {
            namespace: node_id.namespace,
            identifier: value.as_ref().to_string(),
        }),
        // Numeric/GUID/opaque identifiers are never ours (§7.3).
        _ => None,
    }
}

/// Catalog facts of one tag variable, mirrored from the address-space index
/// after the address space has been built.
#[derive(Debug, Clone)]
struct TagTarget {
    instance_id: String,
    tag_name: String,
    writable: bool,
    data_type: String,
}

/// Node-id maps used by the write handler. Filled once after the address
/// space has been built and read on every write request.
#[derive(Debug, Default)]
struct WriteMaps {
    tags: BTreeMap<NodeKey, TagTarget>,
    /// Status variables, folders and the server-status nodes: host-owned,
    /// never writable through OPC UA (§7.2, §8.1).
    host_owned: BTreeSet<NodeKey>,
}

/// Shared state behind the custom node manager: the node-id maps, the
/// configured user roles (§9.1), one handle per attached runtime instance and
/// the optional write-wait override (§8.5).
pub struct WriteHandlerState {
    maps: RwLock<WriteMaps>,
    roles: BTreeMap<String, UserRole>,
    handles: Mutex<BTreeMap<String, RuntimeHandle>>,
    wait_override: Option<Duration>,
}

impl std::fmt::Debug for WriteHandlerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let instances = self
            .handles
            .lock()
            .map(|handles| handles.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|_| vec!["<poisoned>".into()]);
        f.debug_struct("WriteHandlerState")
            .field("roles", &self.roles.keys().collect::<Vec<_>>())
            .field("instances", &instances)
            .field("wait_override", &self.wait_override)
            .finish_non_exhaustive()
    }
}

impl WriteHandlerState {
    /// Create the state with the configured user roles. The anonymous token
    /// is intentionally absent: anonymous is always a Reader (§9.1).
    pub(crate) fn new(roles: BTreeMap<String, UserRole>, wait_override: Option<Duration>) -> Self {
        Self {
            maps: RwLock::new(WriteMaps::default()),
            roles,
            handles: Mutex::new(BTreeMap::new()),
            wait_override,
        }
    }

    /// Mirror the address-space index into the write-handler maps. Called
    /// once after `build_address_space`; until then every tag write is
    /// answered `BadNodeIdUnknown`.
    pub(crate) fn load_address_space(&self, index: &AddressSpaceIndex) {
        let mut maps = WriteMaps::default();
        let mut host_owned = |node_id: &NodeId| {
            if let Some(key) = node_key(node_id) {
                maps.host_owned.insert(key);
            }
        };
        if let Some(root) = &index.root {
            host_owned(root);
        }
        if let Some(server_folder) = &index.server_folder {
            host_owned(server_folder);
        }
        for server_status_node in index.server_status.values() {
            host_owned(server_status_node);
        }
        for (instance_id, nodes) in &index.instances {
            host_owned(&nodes.folder);
            host_owned(&nodes.status_folder);
            host_owned(&nodes.tags_folder);
            for status_node in nodes.status.values() {
                host_owned(status_node);
            }
            for (tag_name, entry) in &nodes.tags {
                if let Some(key) = node_key(&entry.node_id) {
                    maps.tags.insert(
                        key,
                        TagTarget {
                            instance_id: instance_id.clone(),
                            tag_name: tag_name.clone(),
                            writable: entry.writable,
                            data_type: entry.data_type.clone(),
                        },
                    );
                }
            }
        }
        *self
            .maps
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = maps;
    }

    /// Attach (or replace) the runtime handles used to submit write batches.
    pub(crate) fn attach_runtimes(&self, handles: &[(String, RuntimeHandle)]) {
        let mut map = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (instance_id, handle) in handles {
            map.insert(instance_id.clone(), handle.clone());
        }
    }

    /// Remove every attached runtime handle (used when the server stops).
    pub(crate) fn detach_runtimes(&self) {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Role of the requesting identity. Anonymous and unregistered users are
    /// Readers: writes need an explicitly configured Writer (SEC-04, §9.1).
    fn role_for(&self, token: &str, anonymous: bool) -> UserRole {
        if anonymous {
            return UserRole::Reader;
        }
        self.roles.get(token).copied().unwrap_or(UserRole::Reader)
    }

    /// Effective wait for the Engine acknowledgement: the configured
    /// override, or 2× cycle_time + 2 s clamped to [2 s, 30 s] (§8.5).
    fn wait_duration(&self, handle: &RuntimeHandle) -> Duration {
        if let Some(override_wait) = self.wait_override {
            return override_wait;
        }
        derive_write_wait(handle.status().cycle_time)
    }

    /// Validate one write operation against the §8.4 mapping. Returns the
    /// pending submission on success or the rejection status code. A rejection
    /// carries the instance id whenever the target is a known tag: that
    /// instance's whole batch must then be withheld (§8.3).
    fn validate_write(
        &self,
        write: &ParsedWriteValue,
        role: UserRole,
    ) -> Result<PendingWrite, WriteRejection> {
        // Unknown node (WR-04): non-string identifiers are never ours.
        let key = node_key(&write.node_id)
            .ok_or_else(|| WriteRejection::new(StatusCode::BadNodeIdUnknown))?;
        let maps = self
            .maps
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Host-owned nodes (status, folders, server status) are never
        // writable; §8.4 maps this to BadNotWritable.
        if maps.host_owned.contains(&key) {
            return Err(WriteRejection::new(StatusCode::BadNotWritable));
        }
        let tag = maps
            .tags
            .get(&key)
            .ok_or_else(|| WriteRejection::new(StatusCode::BadNodeIdUnknown))?;
        let reject = |status| WriteRejection::for_tag(status, tag);
        // Only the Value attribute of tag variables can be written.
        if write.attribute_id != AttributeId::Value {
            return Err(reject(StatusCode::BadNotWritable));
        }
        // Array/index-range writes are not part of the tag model.
        if write.index_range != NumericRange::None {
            return Err(reject(StatusCode::BadIndexRangeInvalid));
        }
        let Some(data_value) = write.value.value.as_ref() else {
            // A Write operation without a value has nothing to do.
            return Err(reject(StatusCode::BadNothingToDo));
        };
        // Read-only protection comes before the identity check (§8.4 order).
        if !tag.writable {
            return Err(reject(StatusCode::BadNotWritable));
        }
        if role != UserRole::Writer {
            return Err(reject(StatusCode::BadUserAccessDenied));
        }
        // Exact type match, no coercion (WR-03).
        let value = variant_to_value(data_value, &tag.data_type)
            .ok_or_else(|| reject(StatusCode::BadTypeMismatch))?;
        Ok(PendingWrite {
            instance_id: tag.instance_id.clone(),
            tag_name: tag.tag_name.clone(),
            value,
        })
    }

    /// The overridden write service (§8.2/§8.3).
    ///
    /// Per-node statuses are always set and `Ok(())` is returned, so the
    /// service layer reports one status per operation. Invalid nodes never
    /// enter a batch; a valid node whose own instance has an invalid node in
    /// the same request answers `BadNothingToDo` (WR-05). Instances are
    /// independent: another instance's invalid write does not veto this
    /// instance's valid batch.
    pub(crate) async fn handle_write(
        &self,
        context: &RequestContext,
        nodes_to_write: &mut [&mut WriteNode],
    ) -> Result<(), StatusCode> {
        let role = self.role_for(&context.token.0, context.token.is_anonymous());

        // Phase 1 — validate every operation (§8.3). Invalid nodes get their
        // specific status; an invalid tag vetoes its own instance only.
        let mut valid: Vec<(usize, PendingWrite)> = Vec::new();
        let mut vetoed_instances: BTreeSet<String> = BTreeSet::new();
        for (index, node) in nodes_to_write.iter_mut().enumerate() {
            // Keep a more specific pre-set parse error (e.g.
            // BadAttributeIdInvalid); the pre-set status of a node whose
            // fate is undetermined is the default BadNodeIdUnknown.
            let preset = node.status();
            match self.validate_write(node.value(), role) {
                Ok(pending) => valid.push((index, pending)),
                Err(rejection) => {
                    let reported = if preset.is_bad() && preset != StatusCode::BadNodeIdUnknown {
                        preset
                    } else {
                        rejection.status
                    };
                    node.set_status(reported);
                    if let Some(instance_id) = rejection.instance_id {
                        vetoed_instances.insert(instance_id);
                    }
                }
            }
        }

        // Phase 2 — group by instance (§8.3: per-instance batches; no
        // cross-instance transaction is promised).
        let mut groups: BTreeMap<String, Vec<(usize, PendingWrite)>> = BTreeMap::new();
        for (index, pending) in valid {
            groups
                .entry(pending.instance_id.clone())
                .or_default()
                .push((index, pending));
        }

        for (instance_id, group) in groups {
            // One invalid tag in this instance's batch withholds the whole
            // instance batch without touching the other instances.
            if vetoed_instances.contains(&instance_id) {
                for (index, _) in group {
                    nodes_to_write[index].set_status(StatusCode::BadNothingToDo);
                }
                continue;
            }
            let handle = self
                .handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&instance_id)
                .cloned();
            let Some(handle) = handle else {
                // No runtime attached (standalone server) or the instance was
                // stopped: the write cannot take effect (§8.4).
                for (index, _) in group {
                    nodes_to_write[index].set_status(StatusCode::BadOutOfService);
                }
                continue;
            };

            let batch: BTreeMap<String, Value> = group
                .iter()
                .map(|(_, pending)| (pending.tag_name.clone(), pending.value.clone()))
                .collect();
            // Admission is synchronous (§8.3: reject immediately when full).
            let ticket = match handle.submit_writes(batch) {
                Ok(ticket) => ticket,
                Err(RuntimeError::QueueFull) => {
                    for (index, _) in group {
                        nodes_to_write[index].set_status(StatusCode::BadServerTooBusy);
                    }
                    continue;
                }
                Err(RuntimeError::WorkerStopped | RuntimeError::WriteCancelled) => {
                    for (index, _) in group {
                        nodes_to_write[index].set_status(StatusCode::BadOutOfService);
                    }
                    continue;
                }
                Err(error) => {
                    // EmptyWriteBatch cannot happen here (validated
                    // non-empty); anything else is a host bug.
                    log_host_write_error(&error);
                    for (index, _) in group {
                        nodes_to_write[index].set_status(StatusCode::BadInternalError);
                    }
                    continue;
                }
            };

            // The Engine acknowledgement arrives at a cycle boundary; block
            // on the ticket channel through the blocking pool because the
            // server runs on a current-thread runtime (§9.3).
            let wait = self.wait_duration(&handle);
            let outcome = match tokio::task::spawn_blocking(move || ticket.wait(wait)).await {
                Ok(Ok(_receipt)) => StatusCode::Good,
                Ok(Err(RuntimeError::Timeout)) => {
                    // Indeterminate, not cancelled (§8.5.4): the batch stays
                    // queued and remains queryable via `write_state`.
                    StatusCode::BadTimeout
                }
                Ok(Err(RuntimeError::WorkerStopped | RuntimeError::WriteCancelled)) => {
                    StatusCode::BadOutOfService
                }
                Ok(Err(error)) => {
                    log_host_write_error(&error);
                    StatusCode::BadInternalError
                }
                Err(_join_error) => StatusCode::BadInternalError,
            };
            for (index, _) in group {
                nodes_to_write[index].set_status(outcome);
            }
        }
        Ok(())
    }
}

/// One validated, not-yet-submitted tag write.
#[derive(Debug)]
struct PendingWrite {
    instance_id: String,
    tag_name: String,
    value: Value,
}

/// Why one write operation was refused (§8.4). `instance_id` is present when
/// the target is a known tag: the operation then belongs to that instance's
/// batch, so the batch must be withheld as a whole (§8.3).
#[derive(Debug)]
struct WriteRejection {
    status: StatusCode,
    instance_id: Option<String>,
}

impl WriteRejection {
    /// A rejection that cannot be attributed to an instance (unknown node,
    /// host-owned node): it vetoes no batch.
    fn new(status: StatusCode) -> Self {
        Self {
            status,
            instance_id: None,
        }
    }

    /// A rejection of a known tag: the tag's instance batch is vetoed.
    fn for_tag(status: StatusCode, tag: &TagTarget) -> Self {
        Self {
            status,
            instance_id: Some(tag.instance_id.clone()),
        }
    }
}

/// Host-side write failures are host bugs, not client errors: log them for
/// diagnostics while the client still receives a proper status code.
fn log_host_write_error(error: &RuntimeError) {
    eprintln!("df-runtime opcua write handler: {error}");
}

/// The DataFactory node-manager implementation: defaults for every service
/// except `write`, which routes through the [`WriteHandlerState`].
pub struct RuntimeNodeManagerImpl {
    namespaces: Vec<NamespaceMetadata>,
    state: std::sync::Arc<WriteHandlerState>,
}

/// The node manager type the DataFactory server is built from: the library's
/// generic in-memory manager with the runtime write handler as its impl.
pub type RuntimeNodeManager = InMemoryNodeManager<RuntimeNodeManagerImpl>;

#[async_trait::async_trait]
impl InMemoryNodeManagerImpl for RuntimeNodeManagerImpl {
    async fn init(&self, _address_space: &mut AddressSpace, _context: ServerContext) {
        // The §7.2 address space is populated separately by
        // `populate_address_space` after the server has been built.
    }

    fn name(&self) -> &str {
        "datafactory"
    }

    fn namespaces(&self) -> Vec<NamespaceMetadata> {
        self.namespaces.clone()
    }

    async fn write(
        &self,
        context: &RequestContext,
        _address_space: &UaRwLock<AddressSpace>,
        nodes_to_write: &mut [&mut WriteNode],
    ) -> Result<(), StatusCode> {
        self.state.handle_write(context, nodes_to_write).await
    }
}

/// Node-manager builder wiring the runtime write handler into the server.
///
/// The namespace is registered exactly like the library's own builder does
/// (type tree first, then the address space), so `get_namespace_index` finds
/// it and the outer manager routes our namespace to this impl.
pub fn runtime_node_manager(
    namespace_uri: &str,
    state: std::sync::Arc<WriteHandlerState>,
) -> impl NodeManagerBuilder {
    let namespace_uri = namespace_uri.to_string();
    InMemoryNodeManagerBuilder::new(
        move |context: ServerContext, address_space: &mut AddressSpace| {
            let namespace_index = {
                let mut type_tree = context.type_tree.write();
                type_tree.namespaces_mut().add_namespace(&namespace_uri)
            };
            address_space.add_namespace(&namespace_uri, namespace_index);
            RuntimeNodeManagerImpl {
                namespaces: vec![NamespaceMetadata {
                    namespace_uri,
                    namespace_index,
                    ..Default::default()
                }],
                state,
            }
        },
    )
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::str::FromStr;

    use opcua::types::{DataValue, Variant};

    fn tag_key(identifier: &str) -> NodeKey {
        NodeKey {
            namespace: 2,
            identifier: identifier.to_string(),
        }
    }

    fn state_with(tags: &[(&str, bool, &str)], roles: &[(&str, UserRole)]) -> WriteHandlerState {
        let state = WriteHandlerState::new(
            roles
                .iter()
                .map(|(name, role)| (name.to_string(), *role))
                .collect(),
            None,
        );
        {
            let mut maps = state.maps.write().unwrap();
            for (identifier, writable, data_type) in tags {
                maps.tags.insert(
                    tag_key(identifier),
                    TagTarget {
                        instance_id: "a".to_string(),
                        tag_name: format!("tag_{identifier}"),
                        writable: *writable,
                        data_type: data_type.to_string(),
                    },
                );
            }
            maps.host_owned
                .insert(tag_key("df/instances/a/status/CycleIndex"));
        }
        state
    }

    fn value_write(identifier: &str, variant: Variant) -> ParsedWriteValue {
        ParsedWriteValue {
            node_id: NodeId::new(2, identifier),
            attribute_id: AttributeId::Value,
            index_range: NumericRange::None,
            value: DataValue::value_only(variant),
        }
    }

    fn rejected(state: &WriteHandlerState, write: &ParsedWriteValue, role: UserRole) -> StatusCode {
        state.validate_write(write, role).unwrap_err().status
    }

    const FLOW: &str = "df/instances/a/tags/flow";
    const LEVEL: &str = "df/instances/a/tags/level";

    #[test]
    fn unknown_and_non_string_nodes_are_rejected() {
        let state = state_with(
            &[(FLOW, true, "FLOAT64")],
            &[("engineer", UserRole::Writer)],
        );
        // Unknown string node (WR-04).
        let write = value_write("df/instances/a/tags/missing", Variant::Double(1.0));
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadNodeIdUnknown
        );
        // Non-string identifiers are never ours.
        let write = ParsedWriteValue {
            node_id: NodeId::new(2, 42u32),
            attribute_id: AttributeId::Value,
            index_range: NumericRange::None,
            value: DataValue::value_only(Variant::Double(1.0)),
        };
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadNodeIdUnknown
        );
    }

    #[test]
    fn host_owned_nodes_and_non_value_attributes_reject_writes() {
        let state = state_with(
            &[(FLOW, true, "FLOAT64")],
            &[("engineer", UserRole::Writer)],
        );
        // Status node: BadNotWritable despite a valid value.
        let write = value_write("df/instances/a/status/CycleIndex", Variant::UInt64(7));
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadNotWritable
        );
        // Non-Value attribute on a tag variable.
        let write = ParsedWriteValue {
            node_id: NodeId::new(2, FLOW),
            attribute_id: AttributeId::DisplayName,
            index_range: NumericRange::None,
            value: DataValue::value_only(Variant::String("x".into())),
        };
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadNotWritable
        );
    }

    #[test]
    fn index_ranges_and_missing_values_are_rejected() {
        let state = state_with(
            &[(FLOW, true, "FLOAT64")],
            &[("engineer", UserRole::Writer)],
        );
        let write = ParsedWriteValue {
            node_id: NodeId::new(2, FLOW),
            attribute_id: AttributeId::Value,
            index_range: NumericRange::from_str("1:2").unwrap(),
            value: DataValue::value_only(Variant::Double(1.0)),
        };
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadIndexRangeInvalid
        );
        // A Write without a value has nothing to do.
        let write = ParsedWriteValue {
            node_id: NodeId::new(2, FLOW),
            attribute_id: AttributeId::Value,
            index_range: NumericRange::None,
            value: DataValue::default(),
        };
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadNothingToDo
        );
    }

    #[test]
    fn readonly_tags_reject_writes() {
        let state = state_with(
            &[(LEVEL, false, "FLOAT64")],
            &[("engineer", UserRole::Writer)],
        );
        let write = value_write(LEVEL, Variant::Double(1.0));
        // Even a Writer is rejected: the catalog decides (WR-02).
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadNotWritable
        );
    }

    #[test]
    fn identities_without_the_writer_role_are_rejected() {
        let state = state_with(
            &[(FLOW, true, "FLOAT64")],
            &[("engineer", UserRole::Writer)],
        );
        let write = value_write(FLOW, Variant::Double(1.0));
        // Readers — anonymous, registered or unknown — are rejected (SEC-04).
        assert_eq!(
            rejected(&state, &write, UserRole::Reader),
            StatusCode::BadUserAccessDenied
        );
    }

    #[test]
    fn type_mismatches_are_rejected_without_coercion() {
        let state = state_with(
            &[
                (FLOW, true, "FLOAT64"),
                ("df/instances/a/tags/mode", true, "STRING"),
            ],
            &[("engineer", UserRole::Writer)],
        );
        // String into a FLOAT64 tag (WR-03).
        let write = value_write(FLOW, Variant::String("fast".into()));
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadTypeMismatch
        );
        // Numeric near-miss: Int64 into FLOAT64 must not coerce.
        let write = value_write(FLOW, Variant::Int64(2));
        assert_eq!(
            rejected(&state, &write, UserRole::Writer),
            StatusCode::BadTypeMismatch
        );
        // Correct types map onto the tag name and value.
        let write = value_write(FLOW, Variant::Double(1.5));
        let pending = state.validate_write(&write, UserRole::Writer).unwrap();
        assert_eq!(pending.instance_id, "a");
        assert_eq!(pending.tag_name, format!("tag_{FLOW}"));
        assert_eq!(pending.value, Value::Float(1.5));
    }

    #[test]
    fn write_wait_derives_from_cycle_time_and_clamps() {
        assert_eq!(derive_write_wait(0.5), Duration::from_secs(3));
        assert_eq!(derive_write_wait(2.0), Duration::from_secs(6));
        // The upper bound clamps long cycles.
        assert_eq!(derive_write_wait(60.0), MAX_WRITE_WAIT);
        // The margin keeps every valid cycle above the lower bound, so the
        // minimum is only reached for degenerate inputs.
        assert_eq!(derive_write_wait(0.05), Duration::from_millis(2100));
        assert_eq!(derive_write_wait(0.0), MIN_WRITE_WAIT);
        assert_eq!(derive_write_wait(-1.0), MIN_WRITE_WAIT);
        assert_eq!(derive_write_wait(f64::NAN), MIN_WRITE_WAIT);
    }

    #[test]
    fn wait_override_takes_precedence() {
        let state = WriteHandlerState::new(BTreeMap::new(), Some(Duration::from_millis(250)));
        assert_eq!(state.wait_override, Some(Duration::from_millis(250)));
    }

    #[test]
    fn rejections_of_known_tags_name_their_instance_for_batch_vetoes() {
        let state = state_with(
            &[(FLOW, true, "FLOAT64"), (LEVEL, false, "FLOAT64")],
            &[("engineer", UserRole::Writer)],
        );

        // A known tag's rejection vetoes exactly that tag's instance batch.
        let write = value_write(LEVEL, Variant::Double(1.0));
        let rejection = state.validate_write(&write, UserRole::Writer).unwrap_err();
        assert_eq!(rejection.status, StatusCode::BadNotWritable);
        assert_eq!(rejection.instance_id.as_deref(), Some("a"));

        // Rejections outside any tag batch veto nothing.
        let write = value_write("df/instances/a/tags/missing", Variant::Double(1.0));
        assert_eq!(
            state
                .validate_write(&write, UserRole::Writer)
                .unwrap_err()
                .instance_id,
            None
        );
        let write = value_write("df/instances/a/status/CycleIndex", Variant::UInt64(7));
        assert_eq!(
            state
                .validate_write(&write, UserRole::Writer)
                .unwrap_err()
                .instance_id,
            None
        );
    }
}
