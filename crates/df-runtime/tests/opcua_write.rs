#![cfg(feature = "opcua")]

//! R8 closed-loop write integration tests (design §8, §14.5 WR-01..WR-09,
//! §9.1 SEC-04).
//!
//! Four scenarios over real engines, a real `SignAndEncrypt` server with
//! configured users (username/password over None is refused by §9.1) and
//! independent clients:
//!
//! 1. the rejection matrix — read-only tags, type mismatches, index ranges,
//!    value-less writes, unknown nodes, host-owned status nodes and Readers
//!    are all rejected per the §8.4 mapping without side effects, and one
//!    invalid node rejects its whole instance batch (WR-02..05, SEC-04),
//! 2. Writer writes take effect at cycle boundaries, in order, and the model
//!    responds (WR-01/WR-07/WR-08),
//! 3. a full queue rejects immediately and keeps the queued batch; a timed
//!    out write is indeterminate and still applies (WR-06, §8.5.4),
//! 4. stopping the instance rejects later writes and discards pending
//!    batches without applying them (WR-09),
//! 5. one request spanning two instances: instance A's invalid tag withholds
//!    only A's batch while instance B's valid batch commits (§8.3),
//! 6. the R9 dual-tank closed loop: two instances, two independent client
//!    sessions, a setpoint write on A that the model responds to, and B
//!    isolated from it (CL-01/CL-02).
//!
//! Certificate and PKI handling follows the proven R5/R7 pattern: every
//! fixture generates a fresh sample keypair for the server, and every client
//! gets its own PKI directory.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use df_runtime::opcua::{
    AddressSpaceIndex, InstanceCatalog, OpcUaServer, OpcUaUserConfig, SnapshotPublisher, UserRole,
};
use df_runtime::{
    load_engine, OpcUaConfig, OpcUaSecurityConfig, OverrunPolicy, Runtime, RuntimeConfig,
    RuntimeHandle, SecurityMode, SecurityPolicy,
};
use opcua::client::{ClientBuilder, IdentityToken, Password};
use opcua::crypto::SecurityPolicy as UaSecurityPolicy;
use opcua::types::{
    AttributeId, DataValue, MessageSecurityMode, NodeId, NumericRange, ReadValueId,
    TimestampsToReturn, UserTokenPolicy, Variant, WriteValue,
};

const INSTANCE: &str = "tank-a";
const SECOND_INSTANCE: &str = "tank-b";
const INLET: &str = "plant_inlet_flow";
const LEVEL: &str = "tank_1.level";
/// Initial value of `plant_inlet_flow` in the two-tank project; the reference
/// for "the write was never applied".
const INITIAL_INLET: f64 = 0.0012;

fn two_tank_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/two-tank/two-tank.dfproject")
}

fn pid_closed_loop_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/two-tank/pid-closed-loop.yaml")
}

/// A copy of the shipped two-tank YAML with a different cycle time, written
/// to a scratch directory that does not collide with the fixture directory.
fn two_tank_with_cycle_time(name: &str, cycle_time: f64) -> PathBuf {
    let original =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/two-tank/project.yaml");
    let text = std::fs::read_to_string(&original).unwrap();
    let replaced = text.replace("cycle_time: 0.5", &format!("cycle_time: {cycle_time}"));
    assert!(
        replaced.contains(&format!("cycle_time: {cycle_time}")),
        "cycle_time substitution must apply"
    );
    let dir = std::env::temp_dir().join(format!(
        "df_write_test_{}_{name}_project",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("slow_two_tank.yaml");
    std::fs::write(&path, replaced).unwrap();
    path
}

/// Configured users (§9.1); passwords live in environment variables, never
/// in the configuration. Initialised once for the whole test process.
fn write_users() -> Vec<OpcUaUserConfig> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("DF_WRITE_TEST_ENGINEER_PW", "engineer-pw");
        std::env::set_var("DF_WRITE_TEST_VIEWER_PW", "viewer-pw");
    });
    vec![
        OpcUaUserConfig {
            username: "engineer".into(),
            password_env: "DF_WRITE_TEST_ENGINEER_PW".into(),
            role: UserRole::Writer,
        },
        OpcUaUserConfig {
            username: "viewer".into(),
            password_env: "DF_WRITE_TEST_VIEWER_PW".into(),
            role: UserRole::Reader,
        },
    ]
}

fn secure_config(port: u16, dir: &Path, wait_override_ms: Option<u64>) -> OpcUaConfig {
    OpcUaConfig {
        enabled: true,
        endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
        application_uri: "urn:datafactory:runtime".into(),
        namespace_uri: "urn:datafactory:runtime:ns".into(),
        security: OpcUaSecurityConfig {
            mode: SecurityMode::SignAndEncrypt,
            policy: SecurityPolicy::Basic256Sha256,
            allow_anonymous: false,
            server_certificate: Some(dir.join("cert/server.der")),
            server_private_key: Some(dir.join("cert/server-key.pem")),
            trust_list: Some(dir.join("trusted")),
            users: write_users(),
        },
        write_wait_timeout_ms: wait_override_ms,
    }
}

struct WriteFixture {
    server: OpcUaServer,
    _publisher: SnapshotPublisher,
    runtime: Runtime,
    index: AddressSpaceIndex,
    endpoint_url: String,
}

impl Drop for WriteFixture {
    fn drop(&mut self) {
        self.runtime.stop();
        self.server.stop();
    }
}

impl WriteFixture {
    fn tag(&self, tag_name: &str) -> NodeId {
        self.index.tag(INSTANCE, tag_name).unwrap().node_id.clone()
    }

    fn inlet(&self) -> NodeId {
        self.index.tag(INSTANCE, INLET).unwrap().node_id.clone()
    }

    fn level(&self) -> NodeId {
        self.index.tag(INSTANCE, LEVEL).unwrap().node_id.clone()
    }

    fn status_node(&self, name: &str) -> NodeId {
        self.index.status_node(INSTANCE, name).unwrap().clone()
    }

    /// Wait until a cycle has just finished so the next cycle boundary is a
    /// full cycle away. Submissions in the following window cannot race with
    /// the scheduler taking a batch mid-test.
    fn wait_for_cycle_start(&self) {
        let handle = self.runtime.handle();
        let before = handle.status().cycle_index;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if handle.status().cycle_index > before {
                return;
            }
            assert!(Instant::now() < deadline, "the engine must keep cycling");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Start the engine, the secure server and the publisher, and attach the
/// runtime to the server. `name` scopes the fixture's scratch directory.
fn start_fixture(
    name: &str,
    project: &Path,
    queue_capacity: usize,
    wait_override_ms: Option<u64>,
) -> WriteFixture {
    let dir = std::env::temp_dir().join(format!("df_write_test_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cert")).unwrap();
    std::fs::create_dir_all(dir.join("trusted")).unwrap();

    let port = free_port();
    let config = secure_config(port, &dir, wait_override_ms);

    let engine = load_engine(project).unwrap();
    let catalogs = vec![InstanceCatalog {
        instance_id: INSTANCE.into(),
        tags: engine.catalog().to_vec(),
    }];
    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs).unwrap();
    let runtime = Runtime::start_with_config(
        engine,
        RuntimeConfig {
            overrun_policy: OverrunPolicy::Delay,
            write_queue_capacity: queue_capacity,
        },
    )
    .unwrap();
    server
        .attach_runtimes(&[(INSTANCE.into(), runtime.handle())])
        .unwrap();
    let publisher =
        SnapshotPublisher::start(&server, &[(INSTANCE.into(), runtime.handle())]).unwrap();
    let index = server.address_space().clone();
    WriteFixture {
        server,
        _publisher: publisher,
        runtime,
        index,
        endpoint_url: config.endpoint_url.clone(),
    }
}

/// Two independent runtime instances behind one secure server, used to prove
/// per-instance batch boundaries (§8.3).
struct DualWriteFixture {
    server: OpcUaServer,
    _publisher: SnapshotPublisher,
    runtime_a: Runtime,
    runtime_b: Runtime,
    index: AddressSpaceIndex,
    endpoint_url: String,
}

impl Drop for DualWriteFixture {
    fn drop(&mut self) {
        let _ = self.runtime_a.stop();
        let _ = self.runtime_b.stop();
        self.server.stop();
    }
}

impl DualWriteFixture {
    fn tag(&self, instance_id: &str, tag_name: &str) -> NodeId {
        self.index
            .tag(instance_id, tag_name)
            .unwrap()
            .node_id
            .clone()
    }

    /// The engine-level value of `plant_inlet_flow` for one instance, i.e.
    /// what the instance actually applied.
    fn applied_inlet(&self, instance_id: &str) -> f64 {
        let runtime = if instance_id == INSTANCE {
            &self.runtime_a
        } else {
            &self.runtime_b
        };
        match runtime
            .handle()
            .snapshot()
            .values
            .get(INLET)
            .cloned()
            .expect("the snapshot must carry the inlet")
        {
            df_core::Value::Float(value) => value,
            other => panic!("expected Float, got {other:?}"),
        }
    }
}

/// Start two independent engines from the same project and attach both to one
/// server. `name` scopes the fixture's scratch directory.
fn start_dual_fixture(name: &str, project: &Path, queue_capacity: usize) -> DualWriteFixture {
    let dir = std::env::temp_dir().join(format!("df_write_test_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cert")).unwrap();
    std::fs::create_dir_all(dir.join("trusted")).unwrap();

    let port = free_port();
    let config = secure_config(port, &dir, None);

    let engine_a = load_engine(project).unwrap();
    let engine_b = load_engine(project).unwrap();
    let catalogs = vec![
        InstanceCatalog {
            instance_id: INSTANCE.into(),
            tags: engine_a.catalog().to_vec(),
        },
        InstanceCatalog {
            instance_id: SECOND_INSTANCE.into(),
            tags: engine_b.catalog().to_vec(),
        },
    ];
    let server = OpcUaServer::start_with_catalogs(&config, &catalogs).unwrap();

    let runtime_config = || RuntimeConfig {
        overrun_policy: OverrunPolicy::Delay,
        write_queue_capacity: queue_capacity,
    };
    let runtime_a = Runtime::start_with_config(engine_a, runtime_config()).unwrap();
    let runtime_b = Runtime::start_with_config(engine_b, runtime_config()).unwrap();
    let handles = [
        (INSTANCE.to_string(), runtime_a.handle()),
        (SECOND_INSTANCE.to_string(), runtime_b.handle()),
    ];
    server.attach_runtimes(&handles).unwrap();
    let publisher = SnapshotPublisher::start(&server, &handles).unwrap();
    let index = server.address_space().clone();
    DualWriteFixture {
        server,
        _publisher: publisher,
        runtime_a,
        runtime_b,
        index,
        endpoint_url: config.endpoint_url.clone(),
    }
}

/// Block until the instance has completed `cycles` more cycles, so assertions
/// after a rejected write cover at least one full cycle boundary.
fn wait_for_cycles(handle: &RuntimeHandle, cycles: u64) {
    let start = handle.status().cycle_index;
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.status().cycle_index < start + cycles {
        assert!(Instant::now() < deadline, "the engine must keep cycling");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn make_client(name: &str) -> opcua::client::Client {
    let dir = std::env::temp_dir().join(format!("df_write_test_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ClientBuilder::new()
        .application_name("DataFactory Write Test Client")
        .application_uri("urn:datafactory:client")
        .pki_dir(dir.join("client_pki"))
        .trust_server_certs(true)
        .create_sample_keypair(true)
        .session_retry_limit(0)
        .request_timeout(Duration::from_secs(5))
        .client()
        .unwrap()
}

async fn connect_user(
    name: &str,
    username: &str,
    password: &str,
    url: &str,
) -> Arc<opcua::client::Session> {
    let mut client = make_client(name);
    let endpoint = (
        url,
        UaSecurityPolicy::Basic256Sha256.to_str(),
        MessageSecurityMode::SignAndEncrypt,
        UserTokenPolicy::anonymous(),
    );
    let (session, event_loop) = client
        .connect_to_matching_endpoint(
            endpoint,
            IdentityToken::UserName(username.to_string(), Password(password.to_string())),
        )
        .await
        .unwrap();
    event_loop.spawn();
    let connected = tokio::time::timeout(Duration::from_secs(15), session.wait_for_connection())
        .await
        .unwrap_or_else(|_| panic!("{username} session must activate within 15 s"));
    assert!(
        connected,
        "{username} session must activate over SignAndEncrypt"
    );
    session
}

async fn read_values(session: &opcua::client::Session, ids: &[NodeId]) -> Vec<DataValue> {
    let requests: Vec<ReadValueId> = ids.iter().map(|id| ReadValueId::from(id.clone())).collect();
    session
        .read(&requests, TimestampsToReturn::Both, 0.0)
        .await
        .unwrap()
}

async fn write_one(
    session: &opcua::client::Session,
    node_id: &NodeId,
    value: DataValue,
) -> opcua::types::StatusCode {
    let results = session
        .write(&[WriteValue {
            node_id: node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value,
        }])
        .await
        .unwrap();
    results[0]
}

fn double_value(v: f64) -> DataValue {
    let mut value = DataValue::new_now(v);
    value.status = None;
    value
}

fn string_value(text: &str) -> DataValue {
    let mut value = DataValue::new_now(text);
    value.status = None;
    value
}

fn int_value(v: i64) -> DataValue {
    let mut value = DataValue::new_now(v);
    value.status = None;
    value
}

/// Poll the tag until the engine-applied value arrives through the
/// publisher (WR-07 evidence chain: engine applied → snapshot → address
/// space → client read).
async fn wait_for_value(session: &opcua::client::Session, node_id: &NodeId, expected: f64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let values = read_values(session, &[node_id.clone()]).await;
        if values[0].status == Some(opcua::types::StatusCode::Good) {
            if let Some(Variant::Double(v)) = &values[0].value {
                if (v - expected).abs() < 1e-9 {
                    return;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "tag must reach {expected} through the engine round trip, got {:?}",
            values[0]
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_double_condition<F>(
    session: &opcua::client::Session,
    node_id: &NodeId,
    timeout: Duration,
    description: &str,
    predicate: F,
) -> f64
where
    F: Fn(f64) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let values = read_values(session, &[node_id.clone()]).await;
        if values[0].status == Some(opcua::types::StatusCode::Good) {
            let value = as_double(&values[0]);
            if predicate(value) {
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}; last value: {:?}",
            values[0]
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn as_double(value: &DataValue) -> f64 {
    match &value.value {
        Some(Variant::Double(v)) => *v,
        other => panic!("expected Double, got {other:?}"),
    }
}

/// The §8.4 rejection matrix over one fixture: every invalid write is
/// rejected immediately, nothing is ever submitted, and identities without
/// the Writer role cannot write at all.
#[tokio::test]
async fn rejection_matrix_enforces_catalog_identity_and_atomicity() {
    let fixture = start_fixture("rejections", &two_tank_project(), 256, None);
    let engineer = connect_user(
        "rej_engineer",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;
    let viewer = connect_user("rej_viewer", "viewer", "viewer-pw", &fixture.endpoint_url).await;
    let inlet = fixture.inlet();
    let level = fixture.level();

    // WR-02: tank_1.level is a component output — writable=false.
    let status = write_one(&engineer, &level, double_value(0.5)).await;
    assert!(
        status == opcua::types::StatusCode::BadNotWritable,
        "read-only write must be BadNotWritable, got {status:?}"
    );

    // WR-03: exact type match, no coercion.
    let status = write_one(&engineer, &inlet, string_value("fast")).await;
    assert!(
        status == opcua::types::StatusCode::BadTypeMismatch,
        "String into FLOAT64 must be BadTypeMismatch, got {status:?}"
    );
    let status = write_one(&engineer, &inlet, int_value(2)).await;
    assert!(
        status == opcua::types::StatusCode::BadTypeMismatch,
        "Int64 into FLOAT64 must be BadTypeMismatch, got {status:?}"
    );

    // Index-range writes are not part of the tag model.
    let results = engineer
        .write(&[WriteValue {
            node_id: inlet.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::from_str("0:1").unwrap(),
            value: double_value(1.0),
        }])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadIndexRangeInvalid,
        "index-range write must be BadIndexRangeInvalid, got {:?}",
        results[0]
    );

    // A Write without a value has nothing to do.
    let results = engineer
        .write(&[WriteValue {
            node_id: inlet.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value: DataValue::default(),
        }])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadNothingToDo,
        "value-less write must be BadNothingToDo, got {:?}",
        results[0]
    );

    // WR-04.
    let unknown = NodeId::new(
        fixture.server.namespace_index(),
        "df/instances/tank-a/tags/does_not_exist",
    );
    let status = write_one(&engineer, &unknown, double_value(1.0)).await;
    assert!(
        status == opcua::types::StatusCode::BadNodeIdUnknown,
        "unknown node write must be BadNodeIdUnknown, got {status:?}"
    );

    // WR-05: one valid + one invalid node → the whole request is rejected
    // without partial application; the valid node answers BadNothingToDo.
    let results = engineer
        .write(&[
            WriteValue {
                node_id: inlet.clone(),
                attribute_id: AttributeId::Value as u32,
                index_range: NumericRange::None,
                value: double_value(0.002),
            },
            WriteValue {
                node_id: level.clone(),
                attribute_id: AttributeId::Value as u32,
                index_range: NumericRange::None,
                value: double_value(0.5),
            },
        ])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadNothingToDo,
        "the valid node of a rejected batch must be BadNothingToDo, got {:?}",
        results[0]
    );
    assert!(
        results[1] == opcua::types::StatusCode::BadNotWritable,
        "the invalid node of a rejected batch must be BadNotWritable, got {:?}",
        results[1]
    );

    // SEC-04: the Viewer role can connect and read but never write.
    let status = write_one(&viewer, &inlet, double_value(0.002)).await;
    assert!(
        status == opcua::types::StatusCode::BadUserAccessDenied,
        "a Reader write must be BadUserAccessDenied, got {status:?}"
    );

    // Host-owned status nodes reject Writer writes.
    let cycle_index = fixture.status_node("CycleIndex");
    let status = write_one(&engineer, &cycle_index, int_value(99)).await;
    assert!(
        status == opcua::types::StatusCode::BadNotWritable,
        "a status node write must be BadNotWritable, got {status:?}"
    );

    // The rejected batch's valid value was never submitted: the follow-up
    // write lands exactly on its own value.
    let status = write_one(&engineer, &inlet, double_value(0.002)).await;
    assert!(status.is_good(), "follow-up write must succeed: {status:?}");
    wait_for_value(&engineer, &inlet, 0.002).await;

    viewer.disconnect().await.unwrap();
    engineer.disconnect().await.unwrap();
}

/// WR-01/WR-07/WR-08: a Writer write is confirmed after the engine applied
/// it, the published value follows, the model responds (closed loop), and
/// consecutive writes apply in order.
#[tokio::test]
async fn writer_writes_apply_in_order_at_cycle_boundaries() {
    let fixture = start_fixture("applies", &two_tank_project(), 256, None);
    let engineer = connect_user(
        "apply_engineer",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;
    let inlet = fixture.inlet();
    let level = fixture.level();

    let status = write_one(&engineer, &inlet, double_value(0.002)).await;
    assert!(
        status.is_good(),
        "a Writer write to a writable tag must succeed, got {status:?}"
    );
    wait_for_value(&engineer, &inlet, 0.002).await;

    // The model responded: tank_1.level rises above its initial 0.15 as the
    // changed inlet flow feeds the tank (real closed loop, not a replay).
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let values = read_values(&engineer, &[level.clone()]).await;
        if values[0].status == Some(opcua::types::StatusCode::Good) && as_double(&values[0]) > 0.15
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tank level must rise after the setpoint change, got {:?}",
            values[0]
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // WR-08: the second batch applies after the first, one per cycle.
    let status = write_one(&engineer, &inlet, double_value(0.003)).await;
    assert!(status.is_good(), "second write must succeed: {status:?}");
    wait_for_value(&engineer, &inlet, 0.003).await;

    engineer.disconnect().await.unwrap();
}

/// WR-06 and §8.5.4: with a full queue the second write is rejected
/// immediately without displacing the queued batch, and a write that times
/// out is indeterminate — it still applies at the next cycle boundary.
#[tokio::test]
async fn queue_full_rejects_and_timed_out_writes_still_apply() {
    // 2 s cycles; the 500 ms override cannot cover one, so the first write
    // answers BadTimeout while the batch stays queued.
    let project = two_tank_with_cycle_time("queue", 2.0);
    let fixture = start_fixture("queue", &project, 1, Some(500));
    let engineer = connect_user(
        "queue_engineer",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;
    let inlet = fixture.inlet();

    // Synchronise to a fresh cycle so both submissions happen well before
    // the next boundary and cannot race with the scheduler.
    fixture.wait_for_cycle_start();

    let status = write_one(&engineer, &inlet, double_value(0.002)).await;
    assert!(
        status == opcua::types::StatusCode::BadTimeout,
        "the admitted write must time out, got {status:?}"
    );

    // The queue is full now: the second write is rejected immediately and
    // does not overwrite or displace the queued batch (WR-06).
    let status = write_one(&engineer, &inlet, double_value(0.004)).await;
    assert!(
        status == opcua::types::StatusCode::BadServerTooBusy,
        "a write into a full queue must be BadServerTooBusy, got {status:?}"
    );
    assert_eq!(
        fixture.runtime.handle().queue_depth(),
        1,
        "the queued batch must be preserved"
    );

    // §8.5.4: the batch applies anyway — the client must treat the outcome
    // as indeterminate, not cancelled.
    wait_for_value(&engineer, &inlet, 0.002).await;

    engineer.disconnect().await.unwrap();
}

/// WR-09: writes into a stopped instance are refused outright, and a batch
/// that was still pending when the runtime stopped is discarded unapplied.
#[tokio::test]
async fn stopped_instances_reject_writes_and_pending_batches_are_discarded() {
    // 10 s cycles: after the synchronised submission the pending batch
    // cannot apply before the stop.
    let project = two_tank_with_cycle_time("stop", 10.0);
    let mut fixture = start_fixture("stop", &project, 4, Some(300));
    let engineer = connect_user(
        "stop_engineer",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;
    let inlet = fixture.inlet();

    fixture.wait_for_cycle_start();

    let status = write_one(&engineer, &inlet, double_value(0.002)).await;
    assert!(
        status == opcua::types::StatusCode::BadTimeout,
        "the queued write must time out, got {status:?}"
    );
    assert_eq!(fixture.runtime.handle().queue_depth(), 1);

    fixture.runtime.stop();

    // The queued batch was never applied: the snapshot still carries the
    // initial 0.0012.
    let snapshot = fixture.runtime.handle().snapshot();
    let applied = snapshot
        .values
        .get(INLET)
        .and_then(|value| match value {
            df_core::Value::Float(v) => Some(*v),
            _ => None,
        })
        .unwrap();
    assert!(
        (applied - 0.0012).abs() < 1e-9,
        "the pending batch must not be applied after the stop, got {applied}"
    );

    // Later writes are refused.
    let status = write_one(&engineer, &inlet, double_value(0.004)).await;
    assert!(
        status == opcua::types::StatusCode::BadOutOfService,
        "writes after the stop must be BadOutOfService, got {status:?}"
    );

    engineer.disconnect().await.unwrap();
}

/// R9 minimal closed loop (CL-01/CL-02): two independent runtimes load the
/// two-tank project, two independent client sessions read both instances, a
/// Writer sets instance A's inlet, the model responds with a rising level,
/// and instance B keeps its own state. The runtimes and the server are then
/// stopped and the port is released.
#[tokio::test]
async fn two_tank_closed_loop_is_isolated_between_two_instances() {
    let mut fixture = start_dual_fixture("r9_loop", &two_tank_project(), 256);
    // Two independent sessions, one per instance (§4.2 of the round entry).
    let session_a = connect_user(
        "r9_session_a",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;
    let session_b = connect_user(
        "r9_session_b",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;

    let inlet_a = fixture.tag(INSTANCE, INLET);
    let level_a = fixture.tag(INSTANCE, LEVEL);
    let inlet_b = fixture.tag(SECOND_INSTANCE, INLET);
    let level_b = fixture.tag(SECOND_INSTANCE, LEVEL);

    // Both instances start from the same project state.
    let initial_b = read_values(&session_b, &[inlet_b.clone(), level_b.clone()]).await;
    assert_eq!(initial_b[0].status, Some(opcua::types::StatusCode::Good));
    assert!((as_double(&initial_b[0]) - INITIAL_INLET).abs() < 1e-9);
    let initial_level_b = as_double(&initial_b[1]);
    let initial_level_a = as_double(&read_values(&session_a, &[level_a.clone()]).await[0]);
    assert!((initial_level_a - initial_level_b).abs() < 1e-9);

    // CL-01: the setpoint enters the engine and the write is confirmed only
    // after the cycle-boundary acknowledgement.
    let status = write_one(&session_a, &inlet_a, double_value(0.004)).await;
    assert!(
        status.is_good(),
        "the setpoint write must succeed: {status:?}"
    );
    wait_for_value(&session_a, &inlet_a, 0.004).await;

    // CL-02: the response comes from the running model, not from a replay —
    // tank A's level rises after the setpoint change.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let values = read_values(&session_a, &[level_a.clone()]).await;
        if values[0].status == Some(opcua::types::StatusCode::Good)
            && as_double(&values[0]) > initial_level_a + 1e-4
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tank A's level must rise after the setpoint change, got {:?}",
            values[0]
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A second setpoint change keeps tracking the model (not a fixed script).
    let status = write_one(&session_a, &inlet_a, double_value(0.006)).await;
    assert!(
        status.is_good(),
        "the second setpoint write must succeed: {status:?}"
    );
    wait_for_value(&session_a, &inlet_a, 0.006).await;

    // Instance B never saw A's setpoint: the written tag still carries B's own
    // value in the client read, in B's engine snapshot, and A's snapshot
    // carries A's. Level is a derived output that keeps evolving in both
    // engines, so it is compared against its own initial state instead.
    let after_b = read_values(&session_b, &[inlet_b.clone(), level_b.clone()]).await;
    assert_eq!(after_b[0].status, Some(opcua::types::StatusCode::Good));
    assert!(
        (as_double(&after_b[0]) - INITIAL_INLET).abs() < 1e-9,
        "instance B's inlet must stay at its own value, got {:?}",
        after_b[0]
    );
    assert!(
        (fixture.applied_inlet(SECOND_INSTANCE) - INITIAL_INLET).abs() < 1e-9,
        "instance B's engine must not receive instance A's setpoint, got {}",
        fixture.applied_inlet(SECOND_INSTANCE)
    );
    assert!(
        (fixture.applied_inlet(INSTANCE) - 0.006).abs() < 1e-9,
        "instance A's engine must keep its own setpoint, got {}",
        fixture.applied_inlet(INSTANCE)
    );
    assert!(
        as_double(&after_b[1]) > initial_level_b + 1e-6,
        "instance B must keep producing its own model output, got {:?}",
        after_b[1]
    );
    let level_a_now = as_double(&read_values(&session_a, &[level_a.clone()]).await[0]);
    assert!(
        (as_double(&after_b[1]) - level_a_now).abs() > 1e-3,
        "the two instances must not share level state: B {:?} vs A {level_a_now}",
        after_b[1]
    );

    // Stopping instance A rejects further writes; the server and instance B
    // keep running independently.
    fixture.runtime_a.stop().unwrap();
    let status = write_one(&session_a, &inlet_a, double_value(0.002)).await;
    assert!(
        status == opcua::types::StatusCode::BadOutOfService,
        "writes into the stopped instance must be BadOutOfService, got {status:?}"
    );
    let still_running = read_values(&session_b, &[inlet_b.clone()]).await;
    assert_eq!(
        still_running[0].status,
        Some(opcua::types::StatusCode::Good)
    );

    session_a.disconnect().await.unwrap();
    session_b.disconnect().await.unwrap();

    // Stopping the server releases the endpoint: the port stops accepting.
    let address = fixture
        .endpoint_url
        .trim_start_matches("opc.tcp://")
        .to_string();
    fixture.server.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(&address).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the endpoint must stop accepting connections after stop"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// §8.3: atomicity is per instance, not per OPC UA Write request. One request
/// carries a valid and an invalid write for instance A and a valid write for
/// instance B: A's batch is withheld as a whole, B's batch still commits at
/// its own cycle boundary, and neither instance observes the other's writes.
#[tokio::test]
async fn multi_instance_request_vetoes_one_batch_without_blocking_the_other() {
    let fixture = start_dual_fixture("dual_veto", &two_tank_project(), 256);
    let engineer = connect_user(
        "dual_engineer",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;
    let inlet_a = fixture.tag(INSTANCE, INLET);
    let level_a = fixture.tag(INSTANCE, LEVEL);
    let inlet_b = fixture.tag(SECOND_INSTANCE, INLET);

    let results = engineer
        .write(&[
            WriteValue {
                node_id: inlet_a.clone(),
                attribute_id: AttributeId::Value as u32,
                index_range: NumericRange::None,
                value: double_value(0.002),
            },
            // Read-only tag of instance A: vetoes only A's batch.
            WriteValue {
                node_id: level_a.clone(),
                attribute_id: AttributeId::Value as u32,
                index_range: NumericRange::None,
                value: double_value(0.5),
            },
            WriteValue {
                node_id: inlet_b.clone(),
                attribute_id: AttributeId::Value as u32,
                index_range: NumericRange::None,
                value: double_value(0.003),
            },
        ])
        .await
        .unwrap();

    assert!(
        results[0] == opcua::types::StatusCode::BadNothingToDo,
        "instance A's valid write must be withheld with BadNothingToDo, got {:?}",
        results[0]
    );
    assert!(
        results[1] == opcua::types::StatusCode::BadNotWritable,
        "instance A's invalid write must report BadNotWritable, got {:?}",
        results[1]
    );
    assert!(
        results[2].is_good(),
        "instance B's valid write must succeed independently, got {:?}",
        results[2]
    );

    // B committed at a cycle boundary and the model sees it.
    wait_for_value(&engineer, &inlet_b, 0.003).await;

    // A's batch was not partially applied: after two more of A's own cycles
    // the engine-level value is still the initial one.
    wait_for_cycles(&fixture.runtime_a.handle(), 2);
    assert!(
        (fixture.applied_inlet(INSTANCE) - INITIAL_INLET).abs() < 1e-9,
        "instance A must not partially apply a vetoed batch, got {}",
        fixture.applied_inlet(INSTANCE)
    );
    wait_for_value(&engineer, &inlet_a, INITIAL_INLET).await;

    // And B's write did not leak into A (independence in both directions).
    assert!(
        (fixture.applied_inlet(SECOND_INSTANCE) - 0.003).abs() < 1e-9,
        "instance B must keep its own value, got {}",
        fixture.applied_inlet(SECOND_INSTANCE)
    );

    engineer.disconnect().await.unwrap();
}


/// External setpoint → PID → valve → two tanks → PID feedback (§14.5 CL-01/CL-02).
///
/// The project starts near a steady operating point. The client first raises
/// the setpoint and then lowers it, proving that the controller and actuator
/// respond in both directions while the process variables retain their
/// physical lag.
#[tokio::test]
async fn opcua_pid_setpoint_controls_valve_and_two_tank_levels() {
    let mut fixture = start_fixture("pid_closed_loop", &pid_closed_loop_project(), 32, None);
    let session = connect_user(
        "pid_closed_loop_client",
        "engineer",
        "engineer-pw",
        &fixture.endpoint_url,
    )
    .await;

    let setpoint = fixture.tag("level_setpoint");
    let pid_sv = fixture.tag("pid2.SV");
    let pid_pv = fixture.tag("pid2.PV");
    let pid_mv = fixture.tag("pid2.MV");
    let valve_target = fixture.tag("valve_1.target_opening");
    let valve_opening = fixture.tag("valve_1.current_opening");
    let tank_1_level = fixture.tag("tank_1.level");
    let tank_2_level = fixture.tag("tank_2.level");

    assert!(
        fixture.index.tag(INSTANCE, "level_setpoint").unwrap().writable,
        "the external setpoint must be writable"
    );
    for read_only_tag in [
        "pid2.SV",
        "pid2.PV",
        "pid2.MV",
        "valve_1.target_opening",
        "valve_1.current_opening",
        "tank_1.level",
        "tank_2.level",
    ] {
        assert!(
            !fixture.index.tag(INSTANCE, read_only_tag).unwrap().writable,
            "{read_only_tag} must remain a read-only process/status tag"
        );
    }

    // Wait for the first Engine cycle to publish the configured operating point.
    wait_for_value(&session, &valve_target, 23.0).await;
    wait_for_value(&session, &valve_opening, 23.0).await;
    let nodes = [
        setpoint.clone(),
        pid_sv.clone(),
        pid_pv.clone(),
        pid_mv.clone(),
        valve_target.clone(),
        valve_opening.clone(),
        tank_1_level.clone(),
        tank_2_level.clone(),
    ];
    let initial = read_values(&session, &nodes).await;
    for (name, value) in [
        "level_setpoint",
        "pid2.SV",
        "pid2.PV",
        "pid2.MV",
        "valve_1.target_opening",
        "valve_1.current_opening",
        "tank_1.level",
        "tank_2.level",
    ]
    .iter()
    .zip(initial.iter())
    {
        assert_eq!(
            value.status,
            Some(opcua::types::StatusCode::Good),
            "{name} must be readable at the initial operating point"
        );
    }
    let initial_setpoint = as_double(&initial[0]);
    let initial_pv = as_double(&initial[2]);
    let initial_mv = as_double(&initial[3]);
    let initial_target = as_double(&initial[4]);
    let initial_opening = as_double(&initial[5]);
    let initial_tank_1 = as_double(&initial[6]);
    let initial_tank_2 = as_double(&initial[7]);

    assert!(
        (initial_setpoint - 0.0970642201834862).abs() < 1e-6,
        "unexpected initial setpoint: {initial_setpoint}"
    );
    assert!(
        (initial_pv - initial_tank_2).abs() < 1e-3,
        "PID PV must reflect the measured second-tank level"
    );

    // Step the external setpoint up. Good is returned only after the Runtime
    // confirms the write at an Engine cycle boundary.
    let write_status = write_one(&session, &setpoint, double_value(0.40)).await;
    assert!(
        write_status.is_good(),
        "raising the external setpoint must succeed: {write_status:?}"
    );
    wait_for_value(&session, &setpoint, 0.40).await;
    wait_for_value(&session, &pid_sv, 0.40).await;

    let raised_mv = wait_for_double_condition(
        &session,
        &pid_mv,
        Duration::from_secs(5),
        "PID output to increase after the setpoint step",
        |value| value > initial_mv + 10.0,
    )
    .await;
    let raised_target = wait_for_double_condition(
        &session,
        &valve_target,
        Duration::from_secs(5),
        "valve target opening to follow PID output",
        |value| value > initial_target + 10.0,
    )
    .await;
    let raised_opening = wait_for_double_condition(
        &session,
        &valve_opening,
        Duration::from_secs(10),
        "physical valve opening to follow its target",
        |value| value > initial_opening + 10.0,
    )
    .await;
    let raised_tank_1 = wait_for_double_condition(
        &session,
        &tank_1_level,
        Duration::from_secs(30),
        "first-tank level to respond to increased inlet flow",
        |value| value > initial_tank_1 + 0.02,
    )
    .await;
    let raised_tank_2 = wait_for_double_condition(
        &session,
        &tank_2_level,
        Duration::from_secs(40),
        "second-tank level to respond through the cascade",
        |value| value > initial_tank_2 + 0.01,
    )
    .await;

    let high_values = read_values(
        &session,
        &[pid_sv.clone(), pid_pv.clone(), pid_mv.clone(), valve_target.clone(), valve_opening.clone()],
    )
    .await;
    assert!((as_double(&high_values[0]) - 0.40).abs() < 1e-9);
    assert!(as_double(&high_values[1]) > initial_pv + 0.01);
    let high_mv = as_double(&high_values[2]);
    let high_target = as_double(&high_values[3]);
    let high_opening = as_double(&high_values[4]);
    assert!((high_mv - high_target).abs() < 1e-9);

    // Step the setpoint down. PID output and valve target respond first;
    // actual tank levels follow later because the valve and process have lag.
    let write_status = write_one(&session, &setpoint, double_value(0.20)).await;
    assert!(
        write_status.is_good(),
        "lowering the external setpoint must succeed: {write_status:?}"
    );
    wait_for_value(&session, &setpoint, 0.20).await;
    wait_for_value(&session, &pid_sv, 0.20).await;

    let lowered_mv = wait_for_double_condition(
        &session,
        &pid_mv,
        Duration::from_secs(5),
        "PID output to decrease after the setpoint is lowered",
        |value| value < high_mv - 5.0,
    )
    .await;
    let lowered_target = wait_for_double_condition(
        &session,
        &valve_target,
        Duration::from_secs(5),
        "valve target opening to decrease with PID output",
        |value| value < high_target - 5.0,
    )
    .await;
    let lowered_opening = wait_for_double_condition(
        &session,
        &valve_opening,
        Duration::from_secs(10),
        "physical valve opening to move toward the lower target",
        |value| value < high_opening - 5.0,
    )
    .await;

    println!(
        "PID_CLOSED_LOOP_EVIDENCE initial={{setpoint:{initial_setpoint:.6},pv:{initial_pv:.6},mv:{initial_mv:.3},valve:{initial_opening:.3},tank1:{initial_tank_1:.6},tank2:{initial_tank_2:.6}}} step_up={{setpoint:0.400000,mv:{raised_mv:.3},target:{raised_target:.3},opening:{raised_opening:.3},tank1:{raised_tank_1:.6},tank2:{raised_tank_2:.6}}} step_down={{setpoint:0.200000,mv:{lowered_mv:.3},target:{lowered_target:.3},opening:{lowered_opening:.3}}}"
    );

    session.disconnect().await.unwrap();
}
