#![cfg(feature = "opcua")]

//! R7 snapshot publisher integration tests (design §7.5, §14.4 UA-07/08/09).
//!
//! Runs the real two-tank engine on its scheduler thread, attaches the
//! `SnapshotPublisher` to a real server address space and observes the
//! result with an independent OPC UA client:
//! - tag values follow the engine cycles (subscription data changes),
//! - instance status nodes track `RuntimeStatus` (State/CycleIndex/...),
//! - server status aggregates the instance states,
//! - tag reads carry a valid UTC source timestamp,
//! - after the runtime stops, tags leave with `BadOutOfService`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use df_runtime::opcua::{InstanceCatalog, OpcUaServer, SnapshotPublisher};
use df_runtime::{load_engine, Runtime};
use opcua::client::{ClientBuilder, DataChangeCallback, IdentityToken};
use opcua::crypto::SecurityPolicy as UaSecurityPolicy;
use opcua::types::{
    DataValue, ExtensionObject, MessageSecurityMode, MonitoredItemCreateRequest, MonitoringMode,
    MonitoringParameters, NodeId, ReadValueId, StatusCode, TimestampsToReturn, Variant,
};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn loopback_none_config(port: u16) -> df_runtime::OpcUaConfig {
    df_runtime::OpcUaConfig {
        enabled: true,
        endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
        application_uri: "urn:datafactory:runtime".into(),
        namespace_uri: "urn:datafactory:runtime:ns".into(),
        security: df_runtime::OpcUaSecurityConfig::default(),
    }
}

/// The shipped compiled two-tank project and its catalog.
fn two_tank() -> (PathBuf, Vec<InstanceCatalog>) {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/two-tank/two-tank.dfproject");
    let engine = load_engine(&project).unwrap();
    let catalogs = vec![InstanceCatalog {
        instance_id: "tank-a".into(),
        tags: engine.catalog().to_vec(),
    }];
    (project, catalogs)
}

fn make_client(name: &str) -> opcua::client::Client {
    let dir = std::env::temp_dir().join(format!("df_publisher_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ClientBuilder::new()
        .application_name("DataFactory Publisher Test Client")
        .application_uri("urn:datafactory:client")
        .pki_dir(dir.join("client_pki"))
        .session_retry_limit(0)
        .request_timeout(Duration::from_secs(5))
        .client()
        .unwrap()
}

async fn connect_anonymous(name: &str, url: &str) -> Arc<opcua::client::Session> {
    let mut client = make_client(name);
    let endpoint = (
        url,
        UaSecurityPolicy::None.to_str(),
        MessageSecurityMode::None,
        opcua::types::UserTokenPolicy::anonymous(),
    );
    let (session, event_loop) = client
        .connect_to_matching_endpoint(endpoint, IdentityToken::Anonymous)
        .await
        .unwrap();
    event_loop.spawn();
    assert!(session.wait_for_connection().await, "session must connect");
    session
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

/// Poll a node's String value until it equals `expected`.
async fn wait_for_string(
    session: &opcua::client::Session,
    id: &NodeId,
    expected: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let values = read_values(session, &[id.clone()]).await;
        if values.len() == 1 && as_string(&values[0]) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "node {id} never reached {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll until the predicate holds for the node's DataValue.
async fn wait_for_value<F>(
    session: &opcua::client::Session,
    id: &NodeId,
    timeout: Duration,
    mut predicate: F,
) -> DataValue
where
    F: FnMut(&DataValue) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let values = read_values(session, &[id.clone()]).await;
        if values.len() == 1 && predicate(&values[0]) {
            return values.into_iter().next().unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "node {id} never satisfied the condition"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn publisher_streams_cycles_status_and_marks_out_of_service_on_stop() {
    let port = free_port();
    let config = loopback_none_config(port);
    let (project, catalogs) = two_tank();
    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs, &[]).unwrap();

    let engine = load_engine(&project).unwrap();
    let mut runtime = Runtime::start(engine).unwrap();
    let mut publisher =
        SnapshotPublisher::start(&server, &[("tank-a".into(), runtime.handle())]).unwrap();

    let session = connect_anonymous("pub_session", &config.endpoint_url).await;
    let index = server.address_space().clone();
    let level = index.tag("tank-a", "tank_1.level").unwrap().clone();

    // The publisher publishes the current snapshot immediately; wait for the
    // instance to report Running and the server status to aggregate it.
    let state = index.status_node("tank-a", "State").unwrap().clone();
    wait_for_string(&session, &state, "Running", Duration::from_secs(5)).await;
    let server_state = index.server_status["State"].clone();
    let server_count = index.server_status["InstanceCount"].clone();
    let server_running = index.server_status["RunningInstanceCount"].clone();
    wait_for_string(&session, &server_state, "Running", Duration::from_secs(5)).await;
    let values = read_values(&session, &[server_count, server_running.clone()]).await;
    assert!(matches!(&values[0].value, Some(Variant::UInt32(1))));
    assert!(matches!(&values[1].value, Some(Variant::UInt32(1))));

    // Subscribe to the level tag: the engine advances one cycle every 0.5 s
    // and the level rises every cycle, so the subscription must deliver
    // several distinct values (UA-07).
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
                item_to_monitor: ReadValueId::from(level.node_id.clone()),
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

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let distinct = {
            let values = received.lock().unwrap();
            values
                .iter()
                .scan(f64::NAN, |last, v| {
                    let is_new = (v - *last).abs() > 1e-9;
                    *last = *v;
                    Some(is_new)
                })
                .filter(|is_new| *is_new)
                .count()
        };
        if distinct >= 3 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "subscription must deliver several cycle updates, got {:?}",
            received.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Instance status nodes track the runtime status (§7.2).
    let status_ids: Vec<NodeId> = [
        "CycleTime",
        "CycleIndex",
        "SimulationTime",
        "LastCycleDurationMs",
    ]
    .iter()
    .map(|name| index.status_node("tank-a", name).unwrap().clone())
    .collect();
    let values = read_values(&session, &status_ids).await;
    assert!((as_double(&values[0]) - 0.5).abs() < 1e-9, "CycleTime");
    assert!(as_u64(&values[1]) >= 3, "CycleIndex must advance");
    assert!(as_double(&values[2]) >= 1.5, "SimulationTime");
    assert!(as_double(&values[3]) >= 0.0, "LastCycleDurationMs");

    // Tag reads carry a valid UTC source timestamp (UA-09).
    let read = wait_for_value(&session, &level.node_id, Duration::from_secs(2), |v| {
        v.status == Some(StatusCode::Good)
    })
    .await;
    assert!(read.source_timestamp.is_some(), "source timestamp");
    assert!(
        read.source_timestamp.unwrap().as_chrono().timestamp() > 1_600_000_000,
        "source timestamp must be a recent UTC time"
    );

    // Stopping the runtime flips the tags to BadOutOfService and the status
    // variables to Stopped (§7.5).
    runtime.stop().unwrap();
    let final_value = wait_for_value(&session, &level.node_id, Duration::from_secs(5), |v| {
        v.status == Some(StatusCode::BadOutOfService)
    })
    .await;
    assert!(
        final_value.value.is_some(),
        "the last snapshot value stays visible, flagged out of service"
    );
    wait_for_string(&session, &state, "Stopped", Duration::from_secs(5)).await;
    wait_for_string(&session, &server_state, "Stopped", Duration::from_secs(5)).await;
    let values = read_values(&session, &[server_running.clone()]).await;
    assert!(matches!(&values[0].value, Some(Variant::UInt32(0))));

    session.disconnect().await.unwrap();
    publisher.stop();
    publisher.stop(); // idempotent
    server.stop();

    let rebind = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(rebind.is_ok(), "port {port} must be released after stop");
}

#[tokio::test]
async fn publisher_rejects_unknown_and_duplicate_instances() {
    let port = free_port();
    let config = loopback_none_config(port);
    let (_, catalogs) = two_tank();
    let mut server = OpcUaServer::start_with_catalogs(&config, &catalogs, &[]).unwrap();

    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/two-tank/two-tank.dfproject");
    let engine = load_engine(&project).unwrap();
    let mut runtime = Runtime::start(engine).unwrap();
    let handle = runtime.handle();

    // Unknown instance: the address space was built without its catalog.
    let error = match SnapshotPublisher::start(&server, &[("ghost".into(), handle.clone())]) {
        Err(error) => error,
        Ok(_) => panic!("unknown instance must be rejected"),
    };
    assert!(error.to_string().contains("unknown"), "{error}");

    // Duplicate instance attachment is a programming error.
    let error = match SnapshotPublisher::start(
        &server,
        &[
            ("tank-a".into(), handle.clone()),
            ("tank-a".into(), handle.clone()),
        ],
    ) {
        Err(error) => error,
        Ok(_) => panic!("duplicate instance must be rejected"),
    };
    assert!(error.to_string().contains("duplicate"), "{error}");

    // An empty attachment set is allowed (host without enabled instances).
    let mut publisher = SnapshotPublisher::start(&server, &[]).unwrap();
    publisher.stop();

    runtime.stop().unwrap();
    server.stop();
}
