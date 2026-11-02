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
//!    invalid node rejects the whole request (WR-02..05, SEC-04),
//! 2. Writer writes take effect at cycle boundaries, in order, and the model
//!    responds (WR-01/WR-07/WR-08),
//! 3. a full queue rejects immediately and keeps the queued batch; a timed
//!    out write is indeterminate and still applies (WR-06, §8.5.4),
//! 4. stopping the instance rejects later writes and discards pending
//!    batches without applying them (WR-09).
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
    SecurityMode, SecurityPolicy,
};
use opcua::client::{ClientBuilder, IdentityToken, Password};
use opcua::crypto::SecurityPolicy as UaSecurityPolicy;
use opcua::types::{
    AttributeId, DataValue, MessageSecurityMode, NodeId, NumericRange, ReadValueId,
    TimestampsToReturn, UserTokenPolicy, Variant, WriteValue,
};

const INSTANCE: &str = "tank-a";
const INLET: &str = "plant_inlet_flow";
const LEVEL: &str = "tank_1.level";

fn two_tank_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/two-tank/two-tank.dfproject")
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
