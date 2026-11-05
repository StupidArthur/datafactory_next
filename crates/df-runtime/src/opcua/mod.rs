//! OPC UA serving layer for the Runtime Host (design §9).
//!
//! `OpcUaServer` owns a dedicated OS thread and a single-threaded tokio
//! runtime (`ServerBuilder` and the server futures are not `Send`), serves
//! the configured endpoint and builds the §7.2 address space from the
//! instance Tag Catalogs (`address_space`); `value` maps between DataFactory
//! tag types and OPC UA types; `publisher` streams engine snapshots and
//! runtime status into the address space (§7.5). R8 adds the write handler.
//!
//! The `opcua` cargo feature gates this whole module: default builds never
//! compile or link any OPC UA code (design §9.2).

pub mod address_space;
pub mod publisher;
pub mod value;
pub mod write;

pub use address_space::{
    validate_catalogs, AddressSpaceIndex, InstanceCatalog, InstanceNodes, TagEntry,
    INSTANCE_STATUS_NODES, SERVER_STATUS_NODES,
};
pub use publisher::{SnapshotPublisher, DEFAULT_STATUS_REFRESH_INTERVAL};
pub use value::{
    percent_encode, status_node_identifier, tag_node_identifier, ua_data_type, value_to_variant,
    variant_to_value,
};
pub use write::{RuntimeNodeManager, WriteHandlerState};

// Secrets resolution lives in config.rs so the CLI preflight can use it
// without the opcua feature; re-exported here for the serving layer's users.
pub use crate::config::{
    resolve_user_credentials, OpcUaUserConfig, PlainUserCredentials, UserRole,
};

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use opcua::crypto::SecurityPolicy as UaSecurityPolicy;
use opcua::server::{
    Server, ServerBuilder, ServerHandle, ServerUserToken, SubscriptionCache,
    ANONYMOUS_USER_TOKEN_ID,
};
use opcua::types::MessageSecurityMode;
use tokio_util::sync::CancellationToken;

use crate::config::{
    validate_endpoint_url, validate_opcua_security, OpcUaConfig, SecurityMode as HostSecurityMode,
    SecurityPolicy as HostSecurityPolicy,
};
use crate::RuntimeError;
use crate::RuntimeHandle;

use address_space::build_address_space;
use write::runtime_node_manager;

/// Port used when the endpoint URL omits one (the OPC UA default port).
pub const DEFAULT_OPC_UA_PORT: u16 = 4840;

/// How long `OpcUaServer::start` waits for the endpoint to accept TCP
/// connections before the start is reported as failed.
const TCP_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Parse `opc.tcp://host[:port][/path]` into a bind host and port. Structural
/// URL validation is shared with `config.rs`.
pub(crate) fn parse_endpoint(url: &str) -> Result<(String, u16), RuntimeError> {
    validate_endpoint_url(url)
        .map_err(|message| RuntimeError::Config(format!("opcua.endpoint_url {url}: {message}")))?;
    let rest = url.strip_prefix("opc.tcp://").ok_or_else(|| {
        RuntimeError::Config(format!(
            "opcua.endpoint_url {url}: must use the opc.tcp scheme"
        ))
    })?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| {
            RuntimeError::Config(format!(
                "opcua.endpoint_url {url}: unterminated IPv6 address"
            ))
        })?;
        let host = authority[1..close].to_string();
        let port = authority[close + 1..]
            .strip_prefix(':')
            .and_then(|text| text.parse::<u16>().ok());
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.parse::<u16>().ok()),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() {
        return Err(RuntimeError::Config(format!(
            "opcua.endpoint_url {url}: is missing a host"
        )));
    }
    Ok((host, port.unwrap_or(DEFAULT_OPC_UA_PORT)))
}

fn map_security_mode(mode: HostSecurityMode) -> MessageSecurityMode {
    match mode {
        HostSecurityMode::None => MessageSecurityMode::None,
        HostSecurityMode::Sign => MessageSecurityMode::Sign,
        HostSecurityMode::SignAndEncrypt => MessageSecurityMode::SignAndEncrypt,
    }
}

fn map_security_policy(policy: HostSecurityPolicy) -> UaSecurityPolicy {
    match policy {
        HostSecurityPolicy::None => UaSecurityPolicy::None,
        HostSecurityPolicy::Basic128Rsa15 => UaSecurityPolicy::Basic128Rsa15,
        HostSecurityPolicy::Basic256 => UaSecurityPolicy::Basic256,
        HostSecurityPolicy::Basic256Sha256 => UaSecurityPolicy::Basic256Sha256,
    }
}

/// Per-server PKI directory. A unique directory per server keeps parallel
/// servers (tests, multiple instances) from racing on sample certificate
/// generation; configured trust lists still take precedence.
fn default_pki_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("datafactory_pki_{}_{serial}", std::process::id()))
}

/// Everything the host thread needs from the server thread once the server
/// has been built.
struct ServerReady {
    namespace_index: u16,
    node_manager: Arc<RuntimeNodeManager>,
    write_state: Arc<WriteHandlerState>,
    subscriptions: Arc<SubscriptionCache>,
    stop_token: CancellationToken,
    address_space: AddressSpaceIndex,
}

/// Create the server from the Host configuration, instance catalogs, user
/// credentials and the shared write-handler state.
///
/// Runs on the OPC UA thread inside the tokio runtime context because
/// `ServerBuilder::build` creates tokio synchronization primitives.
fn build_server(
    config: &OpcUaConfig,
    catalogs: &[InstanceCatalog],
    users: &[crate::config::PlainUserCredentials],
    write_state: Arc<WriteHandlerState>,
    host: &str,
    port: u16,
) -> Result<(Server, ServerHandle, ServerReady), String> {
    let pki_dir = config
        .security
        .trust_list
        .clone()
        .unwrap_or_else(default_pki_dir);
    // Certificate material: use the configured files when present, otherwise
    // generate a sample keypair into the PKI directory. `create_sample_keypair`
    // only writes the files when they do not exist yet.
    let (certificate_path, private_key_path) = match (
        &config.security.server_certificate,
        &config.security.server_private_key,
    ) {
        (Some(cert), Some(key)) => (Some(cert.clone()), Some(key.clone())),
        _ => (
            Some(pki_dir.join("own").join("cert.der")),
            Some(pki_dir.join("private").join("private.pem")),
        ),
    };

    // Anonymous access is advertised only when the configuration allows it;
    // configured username/password users are added on top (§9.1).
    let mut user_ids = Vec::new();
    if config.security.allow_anonymous {
        user_ids.push(ANONYMOUS_USER_TOKEN_ID.to_string());
    }
    for user in users {
        user_ids.push(user.username.clone());
    }
    let user_token_ids: Vec<&str> = user_ids.iter().map(String::as_str).collect();

    let mut builder = ServerBuilder::new()
        .application_name("DataFactory Runtime")
        .application_uri(config.application_uri.clone())
        .product_uri("urn:datafactory:runtime")
        .create_sample_keypair(true)
        .pki_dir(pki_dir)
        .certificate_path(certificate_path.unwrap())
        .private_key_path(private_key_path.unwrap())
        .host(host.to_string())
        .port(port)
        .discovery_urls(vec![config.endpoint_url.clone()])
        .with_node_manager(runtime_node_manager(
            &config.namespace_uri,
            write_state.clone(),
        ));

    // Never auto-trust unknown peer certificates unless an explicitly
    // configured loopback test fixture requests that relaxation.
    if config.security.auto_trust_client_certs {
        builder = builder.trust_client_certs(true);
    }

    for user in users {
        builder = builder.add_user_token(
            user.username.clone(),
            ServerUserToken::user_pass(user.username.clone(), user.password.clone()),
        );
    }

    builder = builder
        .add_endpoint(
            "datafactory",
            (
                "/",
                map_security_policy(config.security.policy),
                map_security_mode(config.security.mode),
                user_token_ids.as_slice(),
            ),
        )
        .default_endpoint("datafactory");

    let (server, handle) = builder.build()?;
    let ready = populate_address_space(&handle, config, catalogs, &write_state)?;
    Ok((server, handle, ready))
}

/// Resolve the namespace and build the §7.2 address space from the instance
/// Tag Catalogs; mirror the catalog facts into the write handler.
fn populate_address_space(
    handle: &ServerHandle,
    config: &OpcUaConfig,
    catalogs: &[InstanceCatalog],
    write_state: &Arc<WriteHandlerState>,
) -> Result<ServerReady, String> {
    let namespace_index = handle
        .get_namespace_index(&config.namespace_uri)
        .ok_or_else(|| format!("namespace {} was not registered", config.namespace_uri))?;
    let node_manager = handle
        .node_managers()
        .get_of_type::<RuntimeNodeManager>()
        .ok_or_else(|| "the datafactory node manager is missing from the server".to_string())?;

    let address_space = build_address_space(&node_manager, namespace_index, catalogs)?;
    write_state.load_address_space(&address_space);

    Ok(ServerReady {
        namespace_index,
        node_manager,
        write_state: write_state.clone(),
        subscriptions: handle.subscriptions().clone(),
        stop_token: handle.token().clone(),
        address_space,
    })
}

/// A running OPC UA server owned by the Runtime Host.
///
/// The server runs on a dedicated OS thread with its own single-threaded
/// tokio runtime. `stop` cancels the server token; `run` observes it and
/// shuts the server down gracefully.
pub struct OpcUaServer {
    endpoint_url: String,
    namespace_index: u16,
    node_manager: Arc<RuntimeNodeManager>,
    write_state: Arc<WriteHandlerState>,
    subscriptions: Arc<SubscriptionCache>,
    address_space: AddressSpaceIndex,
    stop_token: CancellationToken,
    /// Set once the server thread finished, feeding `is_running`.
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OpcUaServer {
    /// Start the OPC UA server described by `config` without instance nodes.
    pub fn start(config: &OpcUaConfig) -> Result<Self, RuntimeError> {
        Self::start_with_catalogs(config, &[])
    }

    /// Start the server and build the §7.2 address space for the given
    /// instance Tag Catalogs.
    ///
    /// User credentials come from the configuration (`opcua.security.users`);
    /// their passwords are resolved from the environment here (§9.1). The
    /// §9.1 security rules are re-checked at start time. Runtimes are
    /// attached separately via `attach_runtimes`; until then every write
    /// answers `BadOutOfService`.
    pub fn start_with_catalogs(
        config: &OpcUaConfig,
        catalogs: &[InstanceCatalog],
    ) -> Result<Self, RuntimeError> {
        if !config.enabled {
            return Err(RuntimeError::Config("opcua service is not enabled".into()));
        }
        validate_opcua_security(config)?;
        let users = resolve_user_credentials(&config.security)?;
        // §8.5: optional write-wait override from the Host configuration.
        let wait_override = config
            .write_wait_timeout_ms
            .map(|ms| Duration::from_millis(ms));
        let roles: BTreeMap<String, crate::config::UserRole> = config
            .security
            .users
            .iter()
            .map(|user| (user.username.clone(), user.role))
            .collect();
        let write_state = Arc::new(WriteHandlerState::new(roles, wait_override));

        let (host, port) = parse_endpoint(&config.endpoint_url)?;
        let endpoint_url = config.endpoint_url.clone();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<ServerReady, String>>();
        let finished = Arc::new(AtomicBool::new(false));
        let run_error = Arc::new(Mutex::new(None::<String>));

        let thread_config = config.clone();
        let thread_catalogs = catalogs.to_vec();
        let thread_users = users;
        let thread_write_state = write_state.clone();
        let thread_host = host.clone();
        let thread_finished = Arc::clone(&finished);
        let thread_run_error = Arc::clone(&run_error);

        let thread = std::thread::Builder::new()
            .name("df-opcua-server".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("cannot build tokio runtime: {error}")));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let built = build_server(
                        &thread_config,
                        &thread_catalogs,
                        &thread_users,
                        thread_write_state,
                        &thread_host,
                        port,
                    );
                    let (server, _handle, ready) = match built {
                        Ok(built) => built,
                        Err(message) => {
                            let _ = ready_tx.send(Err(message));
                            thread_finished.store(true, Ordering::SeqCst);
                            return;
                        }
                    };

                    // Bind before signalling readiness, so a bind failure
                    // (e.g. the port is already taken) is reported as a
                    // start error instead of a probe timeout.
                    let listener = match tokio::net::TcpListener::bind((thread_host.as_str(), port))
                        .await
                    {
                        Ok(listener) => listener,
                        Err(error) => {
                            let _ = ready_tx
                                .send(Err(format!("cannot bind {thread_host}:{port}: {error}")));
                            thread_finished.store(true, Ordering::SeqCst);
                            return;
                        }
                    };

                    if ready_tx.send(Ok(ready)).is_err() {
                        // `start` gave up; do not run the server.
                        return;
                    }

                    // `run_with` observes the server token internally and
                    // shuts down gracefully when it is cancelled.
                    let result = server.run_with(listener).await;
                    if let Err(message) = result {
                        *thread_run_error.lock().unwrap() = Some(message);
                    }
                    thread_finished.store(true, Ordering::SeqCst);
                });
            })
            .map_err(|error| {
                RuntimeError::Config(format!("cannot spawn the opcua thread: {error}"))
            })?;

        let ready = match ready_rx.recv_timeout(Duration::from_secs(120)) {
            Ok(result) => result,
            Err(_) => {
                let _ = thread.join();
                return Err(RuntimeError::Config(
                    "opcua server thread exited before the endpoint became ready".into(),
                ));
            }
        };
        let ready = match ready {
            Ok(ready) => ready,
            Err(message) => {
                let _ = thread.join();
                return Err(RuntimeError::Config(format!(
                    "opcua server failed to start: {message}"
                )));
            }
        };

        // Confirm the endpoint accepts TCP connections before reporting
        // success. A runtime failure after readiness surfaces as `finished`
        // plus `run_error` (this feeds the CLI exit code 4 in R8).
        let probe_deadline = Instant::now() + TCP_PROBE_TIMEOUT;
        loop {
            if finished.load(Ordering::SeqCst) {
                let _ = thread.join();
                let message = run_error
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "opcua server stopped unexpectedly".into());
                return Err(RuntimeError::Config(format!(
                    "opcua server failed to start: {message}"
                )));
            }
            if TcpStream::connect((host.as_str(), port)).is_ok() {
                break;
            }
            if Instant::now() >= probe_deadline {
                ready.stop_token.cancel();
                let _ = thread.join();
                return Err(RuntimeError::Config(format!(
                    "opcua endpoint {endpoint_url} did not accept connections within {}s",
                    TCP_PROBE_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Ok(Self {
            endpoint_url,
            namespace_index: ready.namespace_index,
            node_manager: ready.node_manager,
            write_state: ready.write_state,
            subscriptions: ready.subscriptions,
            address_space: ready.address_space,
            stop_token: ready.stop_token,
            finished,
            thread: Some(thread),
        })
    }

    /// The configured endpoint URL.
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// Namespace index assigned to the configured `namespace_uri`.
    pub fn namespace_index(&self) -> u16 {
        self.namespace_index
    }

    /// The node manager backing the address space.
    pub fn node_manager(&self) -> &Arc<RuntimeNodeManager> {
        &self.node_manager
    }

    /// The shared write-handler state (node maps, roles, runtime handles).
    pub fn write_state(&self) -> &Arc<WriteHandlerState> {
        &self.write_state
    }

    /// Subscription cache used to notify monitoring clients.
    pub fn subscriptions(&self) -> &Arc<SubscriptionCache> {
        &self.subscriptions
    }

    /// Index of the instance/tag nodes built from the Tag Catalogs.
    pub fn address_space(&self) -> &AddressSpaceIndex {
        &self.address_space
    }

    /// Attach runtime handles so writes flow into the running instances.
    ///
    /// Every instance id must exist in the address space index; unknown ids
    /// are rejected so a typo cannot silently black-hole writes. Existing
    /// attachments for the same instance are replaced.
    pub fn attach_runtimes(
        &self,
        runtimes: &[(String, RuntimeHandle)],
    ) -> Result<(), RuntimeError> {
        for (instance_id, _) in runtimes {
            if !self.address_space.instances.contains_key(instance_id) {
                return Err(RuntimeError::Config(format!(
                    "cannot attach runtime {instance_id}: no such opcua instance"
                )));
            }
        }
        self.write_state.attach_runtimes(runtimes);
        Ok(())
    }

    /// `true` while the server thread has not finished. A mid-run server
    /// death (endpoint failure) flips this and the Host stops with exit
    /// code 5 (§12.2).
    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }

    /// Stop the server and wait for its thread to exit. Idempotent.
    pub fn stop(&mut self) {
        self.stop_token.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // The server is gone; detached handles make later writes answer
        // BadOutOfService instead of queueing into an orphaned runtime.
        self.write_state.detach_runtimes();
    }
}

impl Drop for OpcUaServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoint_handles_host_port_and_ipv6() {
        assert_eq!(
            parse_endpoint("opc.tcp://127.0.0.1:4840").unwrap(),
            ("127.0.0.1".to_string(), 4840)
        );
        assert_eq!(
            parse_endpoint("opc.tcp://localhost").unwrap(),
            ("localhost".to_string(), DEFAULT_OPC_UA_PORT)
        );
        assert_eq!(
            parse_endpoint("opc.tcp://[::1]:50000/Path").unwrap(),
            ("::1".to_string(), 50000)
        );
        assert_eq!(
            parse_endpoint("opc.tcp://192.168.1.5:9").unwrap(),
            ("192.168.1.5".to_string(), 9)
        );
        // Errors
        assert!(parse_endpoint("http://127.0.0.1:4840").is_err());
        assert!(parse_endpoint("opc.tcp://:4840").is_err());
        assert!(parse_endpoint("opc.tcp://host:notaport").is_err());
        assert!(parse_endpoint("opc.tcp://[::1:4840").is_err());
    }

    #[test]
    fn start_rejects_disabled_and_insecure_config() {
        // Disabled config is rejected outright.
        let mut config = OpcUaConfig {
            enabled: false,
            endpoint_url: "opc.tcp://127.0.0.1:48400".into(),
            application_uri: "urn:datafactory:runtime".into(),
            namespace_uri: "urn:datafactory:runtime:ns".into(),
            security: Default::default(),
            write_wait_timeout_ms: None,
        };
        assert!(matches!(
            OpcUaServer::start(&config),
            Err(RuntimeError::Config(message)) if message.contains("not enabled")
        ));

        // A programmatic non-loopback config cannot bypass the §9.1 rules.
        config.enabled = true;
        config.endpoint_url = "opc.tcp://0.0.0.0:48400".into();
        assert!(matches!(
            OpcUaServer::start(&config),
            Err(RuntimeError::Config(message)) if message.contains("non-loopback")
        ));
    }

    #[test]
    fn start_fails_cleanly_when_the_port_is_taken() {
        // Occupy a port, then try to start a server on it: the bind failure
        // must surface as a clean start error instead of reporting success.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = OpcUaConfig {
            enabled: true,
            endpoint_url: format!("opc.tcp://127.0.0.1:{port}"),
            application_uri: "urn:datafactory:runtime".into(),
            namespace_uri: "urn:datafactory:runtime:ns".into(),
            security: Default::default(),
            write_wait_timeout_ms: None,
        };
        let error = match OpcUaServer::start(&config) {
            Err(error) => error,
            Ok(_) => panic!("starting on an occupied port must fail"),
        };
        assert!(error.to_string().contains("failed to start"), "{error}");
        // The bind failure is reported and the thread has exited.
        drop(listener);
    }
}
