#![cfg(feature = "opcua")]

//! R5 OPC UA library proof of concept (design §9, §14.4).
//!
//! Verifies, against a real server on the loopback interface:
//! 1. Anonymous + `SecurityPolicy None` connect, read, dynamic read callback,
//!    host-side `set_value` (the cross-thread publisher path R7 builds on),
//!    write callback capture and client subscription notifications.
//! 2. `SignAndEncrypt` + `Basic256Sha256` with generated sample keypairs and
//!    username/password authentication (positive and negative).
//! 3. Clean shutdown: the endpoint port is released after `stop`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use df_runtime::opcua::{poc_paths, OpcUaServer, PlainUserCredentials};
use df_runtime::{OpcUaConfig, OpcUaSecurityConfig, SecurityMode, SecurityPolicy};
use opcua::client::{ClientBuilder, DataChangeCallback, IdentityToken};
use opcua::crypto::SecurityPolicy as UaSecurityPolicy;
use opcua::types::{
    AttributeId, DataValue, ExtensionObject, MessageSecurityMode, MonitoredItemCreateRequest,
    MonitoringMode, MonitoringParameters, NumericRange, ReadValueId, TimestampsToReturn,
    UserTokenPolicy, Variant, WriteValue,
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
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("df_runtime_opcua_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn loopback_none_config(port: u16) -> OpcUaConfig {
    OpcUaConfig {
        enabled: true,
        endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
        application_uri: "urn:datafactory:runtime".into(),
        namespace_uri: "urn:datafactory:runtime:ns".into(),
        security: OpcUaSecurityConfig::default(),
    }
}

fn make_client(name: &str, secure: bool) -> opcua::client::Client {
    let mut builder = ClientBuilder::new()
        .application_name("DataFactory PoC Client")
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
    let endpoint = (url, policy.to_str(), mode, UserTokenPolicy::anonymous());
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
        IdentityToken::UserName(
            user.to_string(),
            opcua::client::Password(password.to_string()),
        ),
        true,
    )
    .await
}

async fn read_double(session: &opcua::client::Session, id: &opcua::types::NodeId) -> Option<f64> {
    let values = session
        .read(
            &[ReadValueId::from(id.clone())],
            TimestampsToReturn::Both,
            0.0,
        )
        .await
        .ok()?;
    values.first().and_then(|value| match &value.value {
        Some(Variant::Double(number)) => Some(*number),
        _ => None,
    })
}

/// Poll `predicate` until it holds or the timeout elapses.
async fn wait_until<F: FnMut() -> bool>(mut predicate: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll a node until it reads back `expected` or the timeout elapses.
async fn wait_for_double(
    session: &opcua::client::Session,
    id: &opcua::types::NodeId,
    expected: f64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if read_double(session, id).await == Some(expected) {
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
fn publish(server: &OpcUaServer, id: &opcua::types::NodeId, value: f64) {
    server
        .node_manager()
        .set_value(server.subscriptions(), id, None, DataValue::new_now(value))
        .unwrap();
}

#[tokio::test]
async fn anonymous_poc_connect_read_write_and_subscription() {
    let port = free_port();
    let config = loopback_none_config(port);
    let mut server = OpcUaServer::start(&config).unwrap();
    assert_eq!(server.endpoint_url(), config.endpoint_url);
    // The namespace is assigned during server build; index 0 is the standard
    // OPC UA namespace, so ours is at least 1.
    assert!(server.namespace_index() >= 1);

    let session = connect_anonymous("anon_session", &config.endpoint_url).await;
    assert!(
        session.wait_for_connection().await,
        "anonymous None-policy session must connect"
    );

    let counter_id = server.poc_node_id(poc_paths::COUNTER);
    let echo_id = server.poc_node_id(poc_paths::ECHO);
    let writable_id = server.poc_node_id(poc_paths::WRITABLE);

    // 1. Read the initial counter and the Echo read-callback node.
    let values = session
        .read(
            &[
                ReadValueId::from(counter_id.clone()),
                ReadValueId::from(echo_id.clone()),
            ],
            TimestampsToReturn::Both,
            0.0,
        )
        .await
        .unwrap();
    assert_eq!(values.len(), 2);
    assert!(
        matches!(&values[0].value, Some(Variant::Double(v)) if *v == 0.0),
        "counter must start at 0.0, got {:?}",
        values[0].value
    );
    assert!(
        matches!(&values[1].value, Some(Variant::Double(v)) if *v == 42.0),
        "the Echo read callback must serve 42.0, got {:?}",
        values[1].value
    );

    // 2. Host-side set_value (cross-thread) becomes readable.
    publish(&server, &counter_id, 7.0);
    assert!(
        wait_for_double(&session, &counter_id, 7.0, Duration::from_secs(5)).await,
        "counter must read back 7.0 after set_value"
    );

    // 3. Client subscription receives host-side updates (R7 publisher path).
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
                item_to_monitor: ReadValueId::from(counter_id.clone()),
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

    publish(&server, &counter_id, 9.0);
    assert!(
        wait_until(
            || received.lock().unwrap().iter().any(|v| *v == 9.0),
            Duration::from_secs(5)
        )
        .await,
        "subscription must deliver the set_value update, got {:?}",
        received.lock().unwrap()
    );

    // 4. Client write is captured by the write callback and served back by
    //    the read callback (SimpleNodeManager does not store callback writes).
    let results = session
        .write(&[WriteValue {
            node_id: writable_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            value: DataValue::new_now(3.5f64),
        }])
        .await
        .unwrap();
    assert!(results[0].is_good(), "write result {:?}", results[0]);
    assert!(
        wait_for_double(&session, &writable_id, 3.5, Duration::from_secs(5)).await,
        "writable value must read back 3.5 after the client write"
    );

    session.disconnect().await.unwrap();
    server.stop();
    server.stop(); // idempotent

    // 5. The endpoint port is released after stop.
    let rebind = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(rebind.is_ok(), "port {port} must be released after stop");
}

#[tokio::test]
async fn secure_channel_with_username_password_auth() {
    let dir = scratch_dir("secure");
    let cert_dir = dir.join("cert");
    let trust_dir = dir.join("trusted");
    std::fs::create_dir_all(&cert_dir).unwrap();
    std::fs::create_dir_all(&trust_dir).unwrap();

    let port = free_port();
    let config = OpcUaConfig {
        enabled: true,
        endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
        application_uri: "urn:datafactory:runtime".into(),
        namespace_uri: "urn:datafactory:runtime:ns".into(),
        security: OpcUaSecurityConfig {
            mode: SecurityMode::SignAndEncrypt,
            policy: SecurityPolicy::Basic256Sha256,
            allow_anonymous: false,
            server_certificate: Some(cert_dir.join("server.der")),
            server_private_key: Some(cert_dir.join("server-key.pem")),
            trust_list: Some(trust_dir),
        },
    };
    let users = vec![PlainUserCredentials {
        username: "engineer".into(),
        password: "s3cret".into(),
    }];
    let mut server = OpcUaServer::start_with_users(&config, &users).unwrap();

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

    // Correct credentials: session activates and reads work over the
    // encrypted channel.
    let session = connect_username("secure_good", &config.endpoint_url, "engineer", "s3cret").await;
    assert!(
        session.wait_for_connection().await,
        "username/password session must activate over SignAndEncrypt"
    );
    let echo_id = server.poc_node_id(poc_paths::ECHO);
    assert!(
        wait_for_double(&session, &echo_id, 42.0, Duration::from_secs(5)).await,
        "Echo must read 42.0 over the encrypted channel"
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
