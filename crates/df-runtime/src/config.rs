//! Runtime Host configuration (`doc/runtime-system-design.md` §4).
//!
//! The Host configuration (`runtime.yaml`) describes runtime instances and the
//! OPC UA service. It is strictly parsed: unknown fields, duplicate instance
//! ids, invalid paths and malformed endpoints must be reported before any
//! instance starts. Relative paths are resolved against the directory of the
//! configuration file, never against the current working directory.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{OverrunPolicy, RuntimeError, DEFAULT_WRITE_QUEUE_CAPACITY};

/// The only Host configuration version this build accepts.
pub const SUPPORTED_RUNTIME_VERSION: u32 = 1;

/// Parsed and validated Runtime Host configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct HostConfig {
    pub runtime_version: u32,
    /// Present when the `opcua` section exists; `enabled` decides serving.
    pub opcua: Option<OpcUaConfig>,
    /// Instances in declaration order; project paths are absolute.
    pub instances: Vec<InstanceConfig>,
}

/// One runtime instance entry.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceConfig {
    pub id: String,
    /// Absolute path to a DSL v2 `.yaml`/`.yml` or compiled `.dfproject`.
    pub project: PathBuf,
    pub enabled: bool,
    pub scheduler: SchedulerConfig,
}

/// Per-instance scheduler settings; missing fields use the documented defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    pub overrun_policy: OverrunPolicy,
    pub write_queue_capacity: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            overrun_policy: OverrunPolicy::Delay,
            write_queue_capacity: DEFAULT_WRITE_QUEUE_CAPACITY,
        }
    }
}

/// OPC UA service section of the Host configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct OpcUaConfig {
    pub enabled: bool,
    pub endpoint_url: String,
    pub application_uri: String,
    pub namespace_uri: String,
    pub security: OpcUaSecurityConfig,
}

/// OPC UA security settings; paths are absolute after loading.
#[derive(Debug, Clone, PartialEq)]
pub struct OpcUaSecurityConfig {
    pub mode: SecurityMode,
    pub policy: SecurityPolicy,
    pub allow_anonymous: bool,
    pub server_certificate: Option<PathBuf>,
    pub server_private_key: Option<PathBuf>,
    pub trust_list: Option<PathBuf>,
}

impl Default for OpcUaSecurityConfig {
    fn default() -> Self {
        Self {
            mode: SecurityMode::None,
            policy: SecurityPolicy::None,
            allow_anonymous: true,
            server_certificate: None,
            server_private_key: None,
            trust_list: None,
        }
    }
}

/// OPC UA message security mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SecurityMode {
    #[serde(rename = "None")]
    None,
    #[serde(rename = "Sign")]
    Sign,
    #[serde(rename = "SignAndEncrypt")]
    SignAndEncrypt,
}

/// OPC UA security policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SecurityPolicy {
    #[serde(rename = "None")]
    None,
    #[serde(rename = "Basic128Rsa15")]
    Basic128Rsa15,
    #[serde(rename = "Basic256")]
    Basic256,
    #[serde(rename = "Basic256Sha256")]
    Basic256Sha256,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHostConfig {
    runtime_version: u32,
    #[serde(default)]
    opcua: Option<RawOpcUaConfig>,
    instances: Vec<RawInstanceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOpcUaConfig {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_endpoint_url")]
    endpoint_url: String,
    application_uri: String,
    namespace_uri: String,
    #[serde(default)]
    security: RawOpcUaSecurityConfig,
}

/// Design §9.1: the endpoint defaults to the loopback interface, so an
/// unconfigured Host never exposes anything beyond the local machine.
fn default_endpoint_url() -> String {
    "opc.tcp://127.0.0.1:4840".to_string()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOpcUaSecurityConfig {
    #[serde(default)]
    mode: Option<SecurityMode>,
    #[serde(default)]
    policy: Option<SecurityPolicy>,
    #[serde(default)]
    allow_anonymous: Option<bool>,
    #[serde(default)]
    server_certificate: Option<PathBuf>,
    #[serde(default)]
    server_private_key: Option<PathBuf>,
    #[serde(default)]
    trust_list: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstanceConfig {
    id: String,
    project: PathBuf,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    scheduler: RawSchedulerConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchedulerConfig {
    #[serde(default)]
    overrun_policy: Option<OverrunPolicy>,
    #[serde(default)]
    write_queue_capacity: Option<usize>,
}

fn default_true() -> bool {
    true
}

impl HostConfig {
    /// Read and validate a Host configuration file. Relative project and
    /// certificate paths are resolved against the file's directory.
    pub fn load(path: &Path) -> Result<Self, RuntimeError> {
        let canonical = canonicalize_simple(path).map_err(|error| {
            RuntimeError::Config(format!(
                "cannot read host config {}: {error}",
                path.display()
            ))
        })?;
        let contents = fs::read_to_string(&canonical).map_err(|error| {
            RuntimeError::Config(format!(
                "cannot read host config {}: {error}",
                canonical.display()
            ))
        })?;
        let base_dir = canonical
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::parse(&contents, &base_dir)
    }

    /// Parse and validate Host configuration text against `base_dir`.
    pub fn parse(contents: &str, base_dir: &Path) -> Result<Self, RuntimeError> {
        let raw: RawHostConfig = serde_yaml::from_str(contents)
            .map_err(|error| RuntimeError::Config(format!("invalid host config: {error}")))?;
        raw.validate(base_dir)
    }
}

impl RawHostConfig {
    fn validate(self, base_dir: &Path) -> Result<HostConfig, RuntimeError> {
        let fail = |message: String| Err(RuntimeError::Config(message));

        if self.runtime_version != SUPPORTED_RUNTIME_VERSION {
            return fail(format!(
                "unsupported runtime_version {}: this build supports {}",
                self.runtime_version, SUPPORTED_RUNTIME_VERSION
            ));
        }
        if self.instances.is_empty() {
            return fail("instances must not be empty".into());
        }

        // Structural validation runs over every instance first, so that
        // duplicate ids and invalid settings are reported as such even when
        // other instances also reference missing files.
        let mut seen_ids = BTreeSet::new();
        for raw in &self.instances {
            if !is_valid_instance_id(&raw.id) {
                return fail(format!(
                    "instance id {:?} is invalid: use 1-64 ASCII letters, digits, '_' or '-'",
                    raw.id
                ));
            }
            if !seen_ids.insert(raw.id.clone()) {
                return fail(format!("duplicate instance id {:?}", raw.id));
            }
            if raw.scheduler.write_queue_capacity == Some(0) {
                return fail(format!(
                    "instance {:?}: write_queue_capacity must be greater than 0",
                    raw.id
                ));
            }
        }

        // Path resolution runs after the structural pass.
        let mut instances = Vec::with_capacity(self.instances.len());
        for raw in self.instances {
            let project = resolve_project(base_dir, &raw.project, &raw.id)?;
            instances.push(InstanceConfig {
                id: raw.id,
                project,
                enabled: raw.enabled,
                scheduler: SchedulerConfig {
                    overrun_policy: raw.scheduler.overrun_policy.unwrap_or(OverrunPolicy::Delay),
                    write_queue_capacity: raw
                        .scheduler
                        .write_queue_capacity
                        .unwrap_or(DEFAULT_WRITE_QUEUE_CAPACITY),
                },
            });
        }

        let opcua = match self.opcua {
            Some(raw) => Some(raw.validate(base_dir)?),
            None => None,
        };

        Ok(HostConfig {
            runtime_version: self.runtime_version,
            opcua,
            instances,
        })
    }
}

impl RawOpcUaConfig {
    fn validate(self, base_dir: &Path) -> Result<OpcUaConfig, RuntimeError> {
        let fail = |message: String| Err(RuntimeError::Config(message));
        let security = self.security.validate(base_dir)?;

        if self.enabled {
            if let Err(message) = validate_endpoint_url(&self.endpoint_url) {
                return fail(format!(
                    "opcua.endpoint_url {}: {message}",
                    self.endpoint_url
                ));
            }
            if self.application_uri.trim().is_empty() {
                return fail("opcua.application_uri must not be empty".into());
            }
            if self.namespace_uri.trim().is_empty() {
                return fail("opcua.namespace_uri must not be empty".into());
            }
        }
        if security.mode != SecurityMode::None {
            if security.policy == SecurityPolicy::None {
                return fail(
                    "opcua.security: a message security mode other than None requires a security policy"
                        .into(),
                );
            }
            let (certificate, key) = match (&security.server_certificate, &security.server_private_key) {
                (Some(certificate), Some(key)) => (certificate, key),
                _ => {
                    return fail(
                        "opcua.security: mode Sign/SignAndEncrypt requires server_certificate and server_private_key"
                            .into(),
                    )
                }
            };
            if !certificate.is_file() {
                return fail(format!(
                    "opcua.security.server_certificate {} does not exist",
                    certificate.display()
                ));
            }
            if !key.is_file() {
                return fail(format!(
                    "opcua.security.server_private_key {} does not exist",
                    key.display()
                ));
            }
        }
        if let Some(trust_list) = &security.trust_list {
            if !trust_list.is_dir() {
                return fail(format!(
                    "opcua.security.trust_list {} is not a directory",
                    trust_list.display()
                ));
            }
        }

        let opcua = OpcUaConfig {
            enabled: self.enabled,
            endpoint_url: self.endpoint_url,
            application_uri: self.application_uri,
            namespace_uri: self.namespace_uri,
            security,
        };
        if opcua.enabled {
            validate_opcua_security(&opcua)?;
        }
        Ok(opcua)
    }
}

/// Design §9.1 security rules for a resolved OPC UA configuration: outside
/// the loopback interface, serving must be encrypted and must not allow
/// anonymous access. Also enforces the namespace-ownership invariant:
/// `application_uri` and `namespace_uri` must differ, because the built-in
/// diagnostics node manager claims the application URI namespace and would
/// capture reads for our nodes.
///
/// Used both when the Host configuration is loaded and again by the OPC UA
/// serving layer at server start, so a programmatically built `OpcUaConfig`
/// that never passed through `HostConfig::load` cannot bypass the rules.
pub(crate) fn validate_opcua_security(config: &OpcUaConfig) -> Result<(), RuntimeError> {
    if config.application_uri == config.namespace_uri {
        return Err(RuntimeError::Config(
            "opcua: application_uri and namespace_uri must differ; the application URI namespace is owned by the server diagnostics"
                .into(),
        ));
    }
    let host = endpoint_host(&config.endpoint_url).unwrap_or_default();
    if !is_loopback_host(&host) {
        if config.security.mode == SecurityMode::None
            || config.security.policy == SecurityPolicy::None
        {
            return Err(RuntimeError::Config(
                "opcua.security: a non-loopback endpoint_url requires security.mode and security.policy other than None"
                    .into(),
            ));
        }
        if config.security.allow_anonymous {
            return Err(RuntimeError::Config(
                "opcua.security: a non-loopback endpoint_url requires allow_anonymous: false"
                    .into(),
            ));
        }
    }
    Ok(())
}

impl RawOpcUaSecurityConfig {
    fn validate(self, base_dir: &Path) -> Result<OpcUaSecurityConfig, RuntimeError> {
        let fail = |message: String| Err(RuntimeError::Config(message));
        let resolve = |field: &str, raw: &Path| -> Result<PathBuf, RuntimeError> {
            let absolute = resolve_relative(base_dir, raw);
            if absolute.exists() {
                Ok(absolute)
            } else {
                fail(format!(
                    "opcua.security.{field} {} does not exist",
                    absolute.display()
                ))
            }
        };

        let server_certificate = match &self.server_certificate {
            Some(raw) => Some(resolve("server_certificate", raw)?),
            None => None,
        };
        let server_private_key = match &self.server_private_key {
            Some(raw) => Some(resolve("server_private_key", raw)?),
            None => None,
        };
        // The trust list is a directory that may start out empty, so only its
        // presence (not emptiness) is checked here; existence is checked in
        // the mode-dependent rules below.
        let trust_list = match &self.trust_list {
            Some(raw) => {
                let absolute = resolve_relative(base_dir, raw);
                Some(absolute)
            }
            None => None,
        };

        Ok(OpcUaSecurityConfig {
            mode: self.mode.unwrap_or(SecurityMode::None),
            policy: self.policy.unwrap_or(SecurityPolicy::None),
            allow_anonymous: self.allow_anonymous.unwrap_or(true),
            server_certificate,
            server_private_key,
            trust_list,
        })
    }
}

/// Canonicalize `path` without Windows extended-length prefixes so that
/// resolved paths compare equal to ordinary user-facing paths
/// (`\\?\C:\...` → `C:\...`, `\\?\UNC\server\share` → `\\server\share`).
fn canonicalize_simple(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    let text = canonical.as_os_str().to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        Ok(PathBuf::from(format!(r"\\{stripped}")))
    } else if let Some(stripped) = text.strip_prefix(r"\\?\") {
        Ok(PathBuf::from(stripped))
    } else {
        Ok(canonical)
    }
}

/// Instance ids: 1-64 ASCII letters, digits, `_` or `-`, non-empty.
fn is_valid_instance_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Resolve `raw` against `base_dir` and verify it is a loadable project file.
fn resolve_project(
    base_dir: &Path,
    raw: &Path,
    instance_id: &str,
) -> Result<PathBuf, RuntimeError> {
    let absolute = resolve_relative(base_dir, raw);
    if !absolute.is_file() {
        return Err(RuntimeError::Config(format!(
            "instance {instance_id:?}: project file {} does not exist",
            absolute.display()
        )));
    }
    let supported = matches!(
        absolute
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("yaml" | "yml" | "dfproject")
    );
    if !supported {
        return Err(RuntimeError::Config(format!(
            "instance {instance_id:?}: unsupported project format {:?}; expected .yaml, .yml or .dfproject",
            absolute
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
        )));
    }
    Ok(absolute)
}

/// Join relative paths onto `base_dir`; absolute paths pass through unchanged.
fn resolve_relative(base_dir: &Path, raw: &Path) -> PathBuf {
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        base_dir.join(raw)
    }
}

/// Basic structural validation of an `opc.tcp://` endpoint URL.
pub(crate) fn validate_endpoint_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("opc.tcp://")
        .ok_or_else(|| "must use the opc.tcp scheme".to_string())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err("is missing a host".into());
    }
    if authority.starts_with('[') {
        // IPv6 literal such as [::1] or [::1]:4840.
        let close = authority
            .find(']')
            .ok_or_else(|| "has an unterminated IPv6 address".to_string())?;
        let after = &authority[close + 1..];
        if after.is_empty() {
            return Ok(());
        }
        let port = after
            .strip_prefix(':')
            .ok_or_else(|| "has invalid characters after the IPv6 address".to_string())?;
        return validate_port(port);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return Err("is missing a host".into());
            }
            validate_port(port)?;
            Ok(())
        }
        None => Ok(()),
    }
}

/// Extract the host portion of an `opc.tcp://` endpoint URL, stripping any
/// port and the brackets around IPv6 literals.
pub(crate) fn endpoint_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("opc.tcp://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.starts_with('[') {
        let close = authority.find(']')?;
        return Some(authority[1..close].to_string());
    }
    match authority.rsplit_once(':') {
        Some((host, _)) => Some(host.to_string()),
        None => Some(authority.to_string()),
    }
}

/// True for hosts that bind to the loopback interface only (`localhost`,
/// `127.0.0.0/8`, IPv6 `::1`); see design §9.1. `0.0.0.0` is deliberately
/// *not* loopback: it binds every interface, including external ones.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn validate_port(port: &str) -> Result<(), String> {
    let port: u16 = port
        .parse()
        .map_err(|_| format!("has an invalid port {port:?}"))?;
    if port == 0 {
        return Err("port must be between 1 and 65535".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    /// Minimal valid DSL v2 project, embedded from the two-tank example.
    const TWO_TANK_YAML: &str = include_str!("../../../examples/two-tank/project.yaml");

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "df_runtime_config_{}_{name}_{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a config directory containing `two_tank.yaml` and a runtime.yaml.
    fn setup(name: &str, config_body: &str) -> PathBuf {
        let root = temp_root(name);
        fs::write(root.join("two_tank.yaml"), TWO_TANK_YAML).unwrap();
        fs::write(root.join("runtime.yaml"), config_body).unwrap();
        root
    }

    const VALID_CONFIG: &str = "runtime_version: 1\n\
        instances:\n  \
        - id: tank-a\n    \
        project: ./two_tank.yaml\n  \
        - id: tank-b\n    \
        project: ./two_tank.yaml\n    \
        scheduler:\n      \
        overrun_policy: catch_up\n      \
        write_queue_capacity: 8\n";

    #[test]
    fn valid_config_uses_declared_and_default_values() {
        let root = setup("valid", VALID_CONFIG);
        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();

        assert_eq!(config.runtime_version, 1);
        assert_eq!(config.opcua, None);
        assert_eq!(config.instances.len(), 2);

        let first = &config.instances[0];
        assert_eq!(first.id, "tank-a");
        assert!(first.enabled);
        assert_eq!(first.scheduler, SchedulerConfig::default());
        assert_eq!(first.project, root.join("two_tank.yaml"));

        let second = &config.instances[1];
        assert_eq!(second.id, "tank-b");
        assert_eq!(second.scheduler.overrun_policy, OverrunPolicy::CatchUp);
        assert_eq!(second.scheduler.write_queue_capacity, 8);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let root = setup(
            "unknown_field",
            "runtime_version: 1\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n    typo: 1\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn duplicate_instance_ids_are_rejected() {
        let root = setup(
            "duplicate",
            "runtime_version: 1\ninstances:\n  - id: tank-a\n    project: ./two_tank.yaml\n  - id: tank-a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error.to_string().contains("duplicate instance id"),
            "{error}"
        );
    }

    #[test]
    fn structural_errors_are_reported_before_path_errors() {
        // Both instances reference missing files, but the duplicate id is the
        // structural error that must be named (CFG-02).
        let root = setup(
            "dup_before_path",
            "runtime_version: 1\ninstances:\n  - id: same\n    project: ./missing-a.yaml\n  - id: same\n    project: ./missing-b.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("duplicate instance id"), "{message}");
        assert!(!message.contains("does not exist"), "{message}");
    }

    #[test]
    fn invalid_instance_ids_are_rejected() {
        for (name, id) in [
            ("empty_id", ""),
            ("bad_char", "tank a"),
            ("too_long", "x".repeat(65).as_str()),
        ] {
            let body = format!(
                "runtime_version: 1\ninstances:\n  - id: \"{id}\"\n    project: ./two_tank.yaml\n"
            );
            let root = setup(name, &body);
            let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
            assert!(error.to_string().contains("is invalid"), "{name}: {error}");
        }
    }

    #[test]
    fn unsupported_runtime_version_is_rejected() {
        let root = setup(
            "bad_version",
            "runtime_version: 2\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error.to_string().contains("unsupported runtime_version 2"),
            "{error}"
        );
    }

    #[test]
    fn zero_write_queue_capacity_is_rejected() {
        let root = setup(
            "zero_capacity",
            "runtime_version: 1\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n    scheduler:\n      write_queue_capacity: 0\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("greater than 0"), "{error}");
    }

    #[test]
    fn relative_paths_resolve_against_config_dir_not_cwd() {
        let root = setup("path_base", VALID_CONFIG);
        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();
        for instance in &config.instances {
            assert!(
                instance.project.is_absolute(),
                "project path must be absolute: {}",
                instance.project.display()
            );
            assert_eq!(instance.project, root.join("two_tank.yaml"));
        }
    }

    #[test]
    fn absolute_project_paths_are_kept() {
        let root = setup("absolute", VALID_CONFIG);
        let absolute = root.join("two_tank.yaml");
        let body = format!(
            "runtime_version: 1\ninstances:\n  - id: a\n    project: {}\n",
            absolute.display()
        );
        let config_root = temp_root("absolute_config");
        fs::write(config_root.join("runtime.yaml"), body).unwrap();
        let config = HostConfig::load(&config_root.join("runtime.yaml")).unwrap();
        assert_eq!(config.instances[0].project, absolute);
    }

    #[test]
    fn missing_project_file_is_rejected() {
        let root = setup(
            "missing_project",
            "runtime_version: 1\ninstances:\n  - id: a\n    project: ./nope.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn unsupported_project_extension_is_rejected() {
        let root = temp_root("bad_extension");
        fs::write(root.join("project.txt"), "not a model").unwrap();
        fs::write(
            root.join("runtime.yaml"),
            "runtime_version: 1\ninstances:\n  - id: a\n    project: ./project.txt\n",
        )
        .unwrap();
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error.to_string().contains("unsupported project format"),
            "{error}"
        );
    }

    #[test]
    fn missing_config_file_is_reported() {
        let root = temp_root("missing_config");
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error.to_string().contains("cannot read host config"),
            "{error}"
        );
    }

    #[test]
    fn disabled_opcua_section_is_kept_but_not_endpoint_checked() {
        let root = setup(
            "opcua_disabled",
            "runtime_version: 1\nopcua:\n  enabled: false\n  endpoint_url: \"http://not-opc\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();
        let opcua = config.opcua.expect("opcua section must be kept");
        assert!(!opcua.enabled);
        assert_eq!(opcua.security.mode, SecurityMode::None);
        assert!(opcua.security.allow_anonymous);
    }

    #[test]
    fn enabled_opcua_requires_opc_tcp_endpoint() {
        let root = setup(
            "opcua_bad_endpoint",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"http://127.0.0.1:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("opc.tcp"), "{error}");
    }

    #[test]
    fn opcua_cert_paths_resolve_against_config_dir() {
        let root = setup(
            "opcua_certs",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://127.0.0.1:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\n  security:\n    mode: SignAndEncrypt\n    policy: Basic256Sha256\n    allow_anonymous: false\n    server_certificate: ./cert/server.der\n    server_private_key: ./cert/server-key.pem\n    trust_list: ./cert/trusted\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        fs::create_dir_all(root.join("cert")).unwrap();
        fs::write(root.join("cert/server.der"), b"cert").unwrap();
        fs::write(root.join("cert/server-key.pem"), b"key").unwrap();
        fs::create_dir_all(root.join("cert/trusted")).unwrap();

        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();
        let opcua = config.opcua.unwrap();
        assert_eq!(opcua.security.mode, SecurityMode::SignAndEncrypt);
        assert_eq!(opcua.security.policy, SecurityPolicy::Basic256Sha256);
        assert!(!opcua.security.allow_anonymous);
        assert_eq!(
            opcua.security.server_certificate.unwrap(),
            root.join("cert/server.der")
        );
        assert_eq!(
            opcua.security.server_private_key.unwrap(),
            root.join("cert/server-key.pem")
        );
        assert_eq!(
            opcua.security.trust_list.unwrap(),
            root.join("cert/trusted")
        );
    }

    #[test]
    fn secure_mode_requires_policy_and_existing_material() {
        // SignAndEncrypt without a policy is rejected.
        let root = setup(
            "sec_no_policy",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://127.0.0.1:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\n  security:\n    mode: SignAndEncrypt\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error.to_string().contains("requires a security policy"),
            "{error}"
        );

        // Sign with a policy but without certificate material is rejected.
        let root = setup(
            "sec_no_certs",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://127.0.0.1:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\n  security:\n    mode: Sign\n    policy: Basic256Sha256\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("server_certificate and server_private_key"),
            "{error}"
        );

        // Certificate paths that do not exist are rejected.
        let root = setup(
            "sec_missing_cert",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://127.0.0.1:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\n  security:\n    mode: Sign\n    policy: Basic256Sha256\n    server_certificate: ./cert/server.der\n    server_private_key: ./cert/server-key.pem\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn endpoint_url_defaults_to_loopback() {
        let root = setup(
            "opcua_default_url",
            "runtime_version: 1\nopcua:\n  enabled: true\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();
        let opcua = config.opcua.expect("opcua section must be kept");
        assert_eq!(opcua.endpoint_url, "opc.tcp://127.0.0.1:4840");
        assert!(opcua.enabled);
    }

    #[test]
    fn non_loopback_unsecured_endpoint_is_rejected() {
        for (name, url) in [
            ("wildcard", "opc.tcp://0.0.0.0:4840"),
            ("lan_ip", "opc.tcp://192.168.1.10:4840"),
            ("ipv6_global", "opc.tcp://[2001:db8::1]:4840"),
        ] {
            let body = format!(
                "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"{url}\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n"
            );
            let root = setup(name, &body);
            let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("non-loopback"), "{name}: {message}");
        }
    }

    #[test]
    fn non_loopback_anonymous_endpoint_is_rejected() {
        // Fully encrypted but still anonymous: rejected on non-loopback.
        let root = setup(
            "opcua_anon_remote",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://192.168.1.10:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\n  security:\n    mode: SignAndEncrypt\n    policy: Basic256Sha256\n    server_certificate: ./cert/server.der\n    server_private_key: ./cert/server-key.pem\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        fs::create_dir_all(root.join("cert")).unwrap();
        fs::write(root.join("cert/server.der"), b"cert").unwrap();
        fs::write(root.join("cert/server-key.pem"), b"key").unwrap();

        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(
            error.to_string().contains("allow_anonymous: false"),
            "{error}"
        );
    }

    #[test]
    fn non_loopback_secure_non_anonymous_endpoint_is_accepted() {
        let root = setup(
            "opcua_secure_remote",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://192.168.1.10:4840\"\n  application_uri: \"urn:datafactory:runtime\"\n  namespace_uri: \"urn:datafactory:runtime:ns\"\n  security:\n    mode: SignAndEncrypt\n    policy: Basic256Sha256\n    allow_anonymous: false\n    server_certificate: ./cert/server.der\n    server_private_key: ./cert/server-key.pem\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        fs::create_dir_all(root.join("cert")).unwrap();
        fs::write(root.join("cert/server.der"), b"cert").unwrap();
        fs::write(root.join("cert/server-key.pem"), b"key").unwrap();

        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();
        let opcua = config.opcua.unwrap();
        assert!(!opcua.security.allow_anonymous);
        assert_eq!(opcua.security.mode, SecurityMode::SignAndEncrypt);
    }

    #[test]
    fn loopback_host_matrix() {
        // Loopback forms are accepted.
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.9.9.9"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        // Anything that binds beyond loopback is not.
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.10"));
        assert!(!is_loopback_host("[2001:db8::1]"));
        assert!(!is_loopback_host("example.com"));
        // host extraction from full endpoint URLs
        assert_eq!(
            endpoint_host("opc.tcp://127.0.0.1:4840").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            endpoint_host("opc.tcp://localhost:50000/Path").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            endpoint_host("opc.tcp://[::1]:4840").as_deref(),
            Some("::1")
        );
        assert_eq!(
            endpoint_host("opc.tcp://192.168.1.10").as_deref(),
            Some("192.168.1.10")
        );
    }

    #[test]
    fn opcua_uris_must_differ() {
        let root = setup(
            "opcua_same_uris",
            "runtime_version: 1\nopcua:\n  enabled: true\n  endpoint_url: \"opc.tcp://127.0.0.1:4840\"\n  application_uri: \"urn:same\"\n  namespace_uri: \"urn:same\"\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("must differ"), "{error}");

        // Disabled sections keep the URIs but skip the check.
        let root = setup(
            "opcua_same_uris_disabled",
            "runtime_version: 1\nopcua:\n  enabled: false\n  endpoint_url: \"opc.tcp://127.0.0.1:4840\"\n  application_uri: \"urn:same\"\n  namespace_uri: \"urn:same\"\ninstances:\n  - id: a\n    project: ./two_tank.yaml\n",
        );
        let config = HostConfig::load(&root.join("runtime.yaml")).unwrap();
        assert!(!config.opcua.unwrap().enabled);
    }

    #[test]
    fn endpoint_url_validation_matrix() {
        assert!(validate_endpoint_url("opc.tcp://127.0.0.1:4840").is_ok());
        assert!(validate_endpoint_url("opc.tcp://localhost").is_ok());
        assert!(validate_endpoint_url("opc.tcp://[::1]:4840").is_ok());
        assert!(validate_endpoint_url("opc.tcp://0.0.0.0:0").is_err());
        assert!(validate_endpoint_url("http://127.0.0.1:4840").is_err());
        assert!(validate_endpoint_url("opc.tcp://").is_err());
        assert!(validate_endpoint_url("opc.tcp://host:notaport").is_err());
    }

    #[test]
    fn empty_instances_are_rejected() {
        let root = setup("empty_instances", "runtime_version: 1\ninstances: []\n");
        let error = HostConfig::load(&root.join("runtime.yaml")).unwrap_err();
        assert!(error.to_string().contains("must not be empty"), "{error}");
    }
}
