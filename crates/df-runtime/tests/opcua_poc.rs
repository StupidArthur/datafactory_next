#![cfg(feature = "opcua")]

//! R5/R6 OPC UA integration tests (design §7, §9, §14.4).
//!
//! Verifies against a real server on the loopback interface:
//! 1. The §7.2 address space built from the two-tank Tag Catalog:
//!    DataFactory/ServerStatus, per-instance Status/Tags folders, initial
//!    values, engineering-unit properties, writable→AccessLevel mapping,
//!    and instance isolation (UA-04).
//! 2. Host-side `set_value` (the cross-thread publisher path R7 builds on)
//!    becomes readable and reaches client subscriptions.
//! 3. NodeIds are stable across restarts (UA-03).
//! 4. `SignAndEncrypt` + `Basic256Sha256` with generated keypairs and
//!    username/password authentication (positive and negative) still works
//!    with the catalog-driven address space (UA-01).
//! 5. Clean shutdown: the endpoint port is released after `stop`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use df_core::{TagDescriptor, Value};
use df_runtime::load_engine;
use df_runtime::opcua::{
    tag_node_identifier, InstanceCatalog, OpcUaServer, OpcUaUserConfig, UserRole,
};
use opcua::client::{ClientBuilder, DataChangeCallback, IdentityToken, Password};
use opcua::crypto::SecurityPolicy as UaSecurityPolicy;
use opcua::types::{
    AttributeId, DataValue, ExtensionObject, MessageSecurityMode, MonitoredItemCreateRequest,
    MonitoringMode, MonitoringParameters, NodeId, NumericRange, ReadValueId, TimestampsToReturn,
    Variant, WriteValue,
};

/// Grab a free TCP port by binding and immediately releasing it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A unique scratch directory for this test process.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("df_runtime_opcua_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn loopback_none_config(port: u16) -> df_runtime::OpcUaConfig {
    df_runtime::OpcUaConfig {
        enabled: true,
        endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
        application_uri: "urn:datafactory:runtime".into(),
        namespace_uri: "urn:datafactory:runtime:ns".into(),
        security: df_runtime::OpcUaSecurityConfig::default(),
        write_wait_timeout_ms: None,
    }
}

/// The Tag Catalog of the shipped two-tank project (§7.2 example layout).
fn two_tank_catalog(instance_id: &str) -> InstanceCatalog {
    let project =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/two-tank/project.yaml");
    let engine = load_engine(&project).unwrap();
    InstanceCatalog {
        instance_id: instance_id.into(),
        tags: engine.catalog().to_vec(),
    }
}

fn make_client(name: &str, secure: bool) -> opcua::client::Client {
    let mut builder = ClientBuilder::new()
        .application_name("DataFactory UA Test Client")
        .application_uri("urn:datafactory:client")
        .pki_dir(scratch_dir(name).join("client_pki"))
        .trust_server_certs(true)
        .session_retry_limit(0)
        .request_timeout(Duration::from_secs(5));
    if secure {
        builder = builder.create_sample_keypair(true);
    }
    builder.client().unwrap()
}

async fn connect(
    name: &str,
    url: &str,
    policy: UaSecurityPolicy,
    mode: MessageSecurityMode,
    identity: IdentityToken,
    secure: bool,
) -> Arc<opcua::client::Session> {
    let mut client = make_client(name, secure);
    let endpoint = (
        url,
        policy.to_str(),
        mode,
        opcua::types::UserTokenPolicy::anonymous(),
    );
    let (session, event_loop) = client
        .connect_to_matching_endpoint(endpoint, identity)
        .await
        .unwrap();
    event_loop.spawn();
    session
}

async fn connect_anonymous(name: &str, url: &str) -> Arc<opcua::client::Session> {
    connect(
        name,
        url,
        UaSecurityPolicy::None,
        MessageSecurityMode::None,
        IdentityToken::Anonymous,
        false,
    )
    .await
}

async fn connect_username(
    name: &str,
    url: &str,
    user: &str,
    password: &str,
) -> Arc<opcua::client::Session> {
    connect(
        name,
        url,
        UaSecurityPolicy::Basic256Sha256,
        MessageSecurityMode::SignAndEncrypt,
        IdentityToken::UserName(user.to_string(), Password(password.to_string())),
        true,
    )
    .await
}

async fn read_values(session: &opcua::client::Session, ids: &[NodeId]) -> Vec<DataValue> {
    let requests: Vec<ReadValueId> = ids.iter().map(|id| ReadValueId::from(id.clone())).collect();
    session
        .read(&requests, TimestampsToReturn::Both, 0.0)
        .await
        .unwrap()
}

fn as_double(value: &DataValue) -> f64 {
    match &value.value {
        Some(Variant::Double(v)) => *v,
        other => panic!("expected Double, got {other:?}"),
    }
}

fn as_string(value: &DataValue) -> String {
    match &value.value {
        Some(Variant::String(text)) => text.as_ref().to_string(),
        other => panic!("expected String, got {other:?}"),
    }
}

fn as_u64(value: &DataValue) -> u64 {
    match &value.value {
        Some(Variant::UInt64(v)) => *v,
        other => panic!("expected UInt64, got {other:?}"),
    }
}

/// Poll a node until it reads back `expected` or the timeout elapses.
async fn wait_for_double(
    session: &opcua::client::Session,
    id: &NodeId,
    expected: f64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let values = read_values(session, &[id.clone()]).await;
        if values.len() == 1 && (as_double(&values[0]) - expected).abs() < f64::EPSILON {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Publish a value on the host thread exactly the way R7's publisher will:
/// `set_value` on the node manager with the server's subscription cache.
fn publish(server: &OpcUaServer, id: &NodeId, value: f64) {
    server
        .node_manager()
        .set_value(server.subscriptions(), id, None, DataValue::new_now(value))
        .unwrap();
}

#[tokio::test]
async fn catalog_address_space_reads_writes_and_subscriptions() {
    let port = free_port();
    let config = loopback_none_config(port);
    let catalogs = vec![two_tank_catalog("tank-a"), two_tank_catalog("tank-b")];
    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs).unwrap();
    assert_eq!(server.endpoint_url(), config.endpoint_url);
    assert!(server.namespace_index() >= 1);

    let session = connect_anonymous("anon_session", &config.endpoint_url).await;
    assert!(
        session.wait_for_connection().await,
        "anonymous None-policy session must connect"
    );

    let index = server.address_space().clone();

    // Server status skeleton (§7.2) with neutral initial values.
    let server_ids: Vec<NodeId> = [
        index.server_status["State"].clone(),
        index.server_status["InstanceCount"].clone(),
        index.server_status["RunningInstanceCount"].clone(),
    ]
    .to_vec();
    let values = read_values(&session, &server_ids).await;
    assert_eq!(as_string(&values[0]), "Starting");
    assert!(matches!(&values[1].value, Some(Variant::UInt32(2))));
    assert!(matches!(&values[2].value, Some(Variant::UInt32(0))));

    // Instance status skeleton (§7.2).
    let status_ids: Vec<NodeId> = ["State", "CycleTime", "CycleIndex", "RejectedWriteBatches"]
        .iter()
        .map(|name| index.status_node("tank-a", name).unwrap().clone())
        .collect();
    let values = read_values(&session, &status_ids).await;
    assert_eq!(as_string(&values[0]), "Starting");
    assert!(matches!(&values[1].value, Some(Variant::Double(0.0))));
    assert_eq!(as_u64(&values[2]), 0);
    assert_eq!(as_u64(&values[3]), 0);

    // Tag nodes: type, writability and initial value (§7.4).
    let level_a = index.tag("tank-a", "tank_1.level").unwrap().clone();
    assert_eq!(level_a.data_type, "FLOAT64");
    assert!(!level_a.writable);
    let values = read_values(&session, &[level_a.node_id.clone()]).await;
    assert!((as_double(&values[0]) - 0.15).abs() < 1e-9);

    // Engineering unit property (§7.4): "m" for tank levels.
    let unit_id = NodeId::new(
        server.namespace_index(),
        format!("{}/unit", tag_node_identifier("tank-a", "tank_1.level")),
    );
    let values = read_values(&session, &[unit_id]).await;
    assert_eq!(as_string(&values[0]), "m");

    // Instance isolation (UA-04): same tag name, different NodeIds and
    // independent values.
    let level_b = index.tag("tank-b", "tank_1.level").unwrap().clone();
    assert_ne!(level_a.node_id, level_b.node_id);

    // Host-side set_value (cross-thread, the R7 publisher path).
    publish(&server, &level_a.node_id, 7.0);
    assert!(
        wait_for_double(&session, &level_a.node_id, 7.0, Duration::from_secs(5)).await,
        "tank-a level must read back 7.0 after set_value"
    );
    let values = read_values(&session, &[level_b.node_id.clone()]).await;
    assert!(
        (as_double(&values[0]) - 0.15).abs() < 1e-9,
        "tank-b must be unaffected: {:?}",
        values[0].value
    );

    // Client subscription receives host-side updates.
    let received: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let subscription_id = session
        .create_subscription(
            Duration::from_millis(100),
            60,
            10,
            0,
            0,
            true,
            DataChangeCallback::new(move |value: DataValue, _item| {
                if let Some(Variant::Double(number)) = value.value {
                    sink.lock().unwrap().push(number);
                }
            }),
        )
        .await
        .unwrap();
    let created = session
        .create_monitored_items(
            subscription_id,
            TimestampsToReturn::Both,
            vec![MonitoredItemCreateRequest {
                item_to_monitor: ReadValueId::from(level_a.node_id.clone()),
                monitoring_mode: MonitoringMode::Reporting,
                requested_parameters: MonitoringParameters {
                    client_handle: 1,
                    sampling_interval: 100.0,
                    filter: ExtensionObject::null(),
                    queue_size: 1,
                    discard_oldest: true,
                },
            }],
        )
        .await
        .unwrap();
    assert_eq!(created.len(), 1);

    publish(&server, &level_a.node_id, 9.0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !received.lock().unwrap().iter().any(|v| *v == 9.0) {
        assert!(
            Instant::now() < deadline,
            "subscription must deliver the set_value update, got {:?}",
            received.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Write path (R8 semantics, anonymous client): the anonymous identity is
    // a Reader, so even the writable tag rejects with BadUserAccessDenied
    // (SEC-04); values are served by the publisher, never stored by the
    // write handler.
    let inlet = index.tag("tank-a", "plant_inlet_flow").unwrap().clone();
    assert!(inlet.writable);
    let results = session
        .write(&[WriteValue {
            node_id: inlet.node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value: DataValue::new_now(0.002f64),
        }])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadUserAccessDenied,
        "an anonymous write must be BadUserAccessDenied, got {:?}",
        results[0]
    );

    // Read-only tag: rejected from the catalog, not the AccessLevel bits
    // (WR-02; BadNotWritable per the §8.4 mapping).
    let results = session
        .write(&[WriteValue {
            node_id: level_a.node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value: DataValue::new_now(1.0f64),
        }])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadNotWritable,
        "read-only write must be BadNotWritable, got {:?}",
        results[0]
    );

    // Unknown node (WR-04).
    let results = session
        .write(&[WriteValue {
            node_id: NodeId::new(server.namespace_index(), "df/instances/tank-a/tags/nothing"),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value: DataValue::new_now(1.0f64),
        }])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadNodeIdUnknown,
        "unknown node write must be BadNodeIdUnknown, got {:?}",
        results[0]
    );

    session.disconnect().await.unwrap();
    server.stop();
    server.stop(); // idempotent

    // The endpoint port is released after stop.
    let rebind = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(rebind.is_ok(), "port {port} must be released after stop");
}

#[tokio::test]
async fn node_ids_are_stable_across_restarts() {
    let port = free_port();
    let config = loopback_none_config(port);
    let catalogs = vec![two_tank_catalog("tank-a")];

    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs).unwrap();
    let first = server.address_space().clone();
    let first_ids: Vec<NodeId> = ["tank_1.level", "tank_2.level", "plant_inlet_flow"]
        .iter()
        .map(|tag| first.tag("tank-a", tag).unwrap().node_id.clone())
        .collect();
    let first_status: Vec<NodeId> = ["State", "CycleIndex"]
        .iter()
        .map(|name| first.status_node("tank-a", name).unwrap().clone())
        .collect();
    server.stop();

    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs).unwrap();
    let second = server.address_space().clone();
    for (name, expected) in ["tank_1.level", "tank_2.level", "plant_inlet_flow"]
        .iter()
        .zip(&first_ids)
    {
        assert_eq!(
            &second.tag("tank-a", name).unwrap().node_id,
            expected,
            "NodeId of {name} must be identical after a restart (UA-03)"
        );
    }
    for (name, expected) in ["State", "CycleIndex"].iter().zip(&first_status) {
        assert_eq!(
            second.status_node("tank-a", name).unwrap(),
            expected,
            "status NodeId of {name} must be identical after a restart"
        );
    }
    server.stop();
}

#[tokio::test]
async fn tag_metadata_description_and_unit_property() {
    let port = free_port();
    let config = loopback_none_config(port);
    let catalog = InstanceCatalog {
        instance_id: "unit-demo".into(),
        tags: vec![TagDescriptor {
            name: "level".into(),
            data_type: "FLOAT64".into(),
            writable: false,
            unit: "m".into(),
            description: "water level".into(),
            initial: Value::Float(1.0),
        }],
    };
    let mut server = OpcUaServer::start_with_catalogs(&config, &[catalog]).unwrap();

    let session = connect_anonymous("meta_session", &config.endpoint_url).await;
    assert!(session.wait_for_connection().await);

    let tag = server
        .address_space()
        .tag("unit-demo", "level")
        .unwrap()
        .clone();

    // Initial value from the descriptor (§7.4).
    let values = read_values(&session, &[tag.node_id.clone()]).await;
    assert!((as_double(&values[0]) - 1.0).abs() < 1e-9);

    // Description attribute (UA-06): the OPC UA Description attribute is a
    // LocalizedText, not a String.
    let mut request = ReadValueId::from(tag.node_id.clone());
    request.attribute_id = AttributeId::Description as u32;
    let values = session
        .read(&[request], TimestampsToReturn::Both, 0.0)
        .await
        .unwrap();
    match &values[0].value {
        Some(Variant::LocalizedText(text)) => {
            assert_eq!(text.text.as_ref(), "water level", "description text");
        }
        other => panic!("expected LocalizedText, got {other:?}"),
    }

    // Engineering unit property (UA-06).
    let unit_id = NodeId::new(
        server.namespace_index(),
        format!("{}/unit", tag_node_identifier("unit-demo", "level")),
    );
    let values = read_values(&session, &[unit_id]).await;
    assert_eq!(as_string(&values[0]), "m");

    session.disconnect().await.unwrap();
    server.stop();
}

#[tokio::test]
async fn secure_channel_with_username_password_auth() {
    let dir = scratch_dir("secure");
    let cert_dir = dir.join("cert");
    let trust_dir = dir.join("trusted");
    std::fs::create_dir_all(&cert_dir).unwrap();
    std::fs::create_dir_all(&trust_dir).unwrap();

    let port = free_port();
    // Unique environment variable so parallel test processes cannot collide.
    let password_env = format!("DF_POC_SECURE_PW_{}", std::process::id());
    std::env::set_var(&password_env, "s3cret");
    let config = df_runtime::OpcUaConfig {
        enabled: true,
        endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
        application_uri: "urn:datafactory:runtime".into(),
        namespace_uri: "urn:datafactory:runtime:ns".into(),
        security: df_runtime::OpcUaSecurityConfig {
            mode: df_runtime::SecurityMode::SignAndEncrypt,
            policy: df_runtime::SecurityPolicy::Basic256Sha256,
            allow_anonymous: false,
            server_certificate: Some(cert_dir.join("server.der")),
            server_private_key: Some(cert_dir.join("server-key.pem")),
            trust_list: Some(trust_dir),
            // The engineer is a Writer: the R8 write tests authenticate as
            // this user (§9.1: username/password only over SignAndEncrypt).
            users: vec![OpcUaUserConfig {
                username: "engineer".into(),
                password_env: password_env.clone(),
                role: UserRole::Writer,
            }],
        },
        write_wait_timeout_ms: None,
    };
    let catalogs = vec![two_tank_catalog("tank-a")];
    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs).unwrap();

    // Sample keypairs were generated at the configured paths.
    assert!(
        config
            .security
            .server_certificate
            .as_ref()
            .unwrap()
            .is_file(),
        "server certificate must be generated when missing"
    );
    assert!(config
        .security
        .server_private_key
        .as_ref()
        .unwrap()
        .is_file());

    // Correct credentials: session activates and tag reads work over the
    // encrypted channel.
    let session = connect_username("secure_good", &config.endpoint_url, "engineer", "s3cret").await;
    assert!(
        session.wait_for_connection().await,
        "username/password session must activate over SignAndEncrypt"
    );
    let level = server
        .address_space()
        .tag("tank-a", "tank_1.level")
        .unwrap()
        .clone();
    assert!(
        wait_for_double(&session, &level.node_id, 0.15, Duration::from_secs(5)).await,
        "tank level must read 0.15 over the encrypted channel"
    );

    // Writer identity, standalone server (no runtime attached): the writable
    // tag answers BadOutOfService because no engine can take the write
    // (§8.4 stopped instance; full closed-loop writes arrive with R8 tests).
    let inlet = server
        .address_space()
        .tag("tank-a", "plant_inlet_flow")
        .unwrap()
        .clone();
    let results = session
        .write(&[WriteValue {
            node_id: inlet.node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value: DataValue::new_now(0.002f64),
        }])
        .await
        .unwrap();
    assert!(
        results[0] == opcua::types::StatusCode::BadOutOfService,
        "a Writer write without an attached runtime must be BadOutOfService, got {:?}",
        results[0]
    );
    session.disconnect().await.unwrap();

    // Wrong credentials: the session never becomes connected.
    let bad = connect_username("secure_bad", &config.endpoint_url, "engineer", "wrong").await;
    let outcome = tokio::time::timeout(Duration::from_secs(3), bad.wait_for_connection()).await;
    assert!(
        !matches!(outcome, Ok(true)),
        "a wrong password must not yield a connected session"
    );

    server.stop();
}
