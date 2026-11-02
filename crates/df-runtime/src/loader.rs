//! Project loading for runtime instances (`doc/runtime-system-design.md` §4.3).
//!
//! Loading order:
//!
//! 1. `.yaml`/`.yml`: read the source file, compile it with
//!    `df_core::compile_source` and create the Engine from the compiled bytes.
//!    No temporary file is produced.
//! 2. `.dfproject`: load the compiled artifact with `Engine::load`.
//! 3. Any other extension is rejected explicitly; the format is never guessed
//!    from file contents.

use std::fs;
use std::path::Path;

use df_core::Engine;

use crate::RuntimeError;

/// Create an Engine from a DSL v2 YAML source or a compiled `.dfproject`.
pub fn load_engine(project_path: &Path) -> Result<Engine, RuntimeError> {
    let extension = project_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("yaml" | "yml") => {
            let source = fs::read_to_string(project_path).map_err(|error| {
                RuntimeError::Engine(format!(
                    "cannot read project source {}: {error}",
                    project_path.display()
                ))
            })?;
            let bytes = df_core::compile_source(&source).map_err(|error| {
                RuntimeError::Engine(format!(
                    "cannot compile project source {}: {error}",
                    project_path.display()
                ))
            })?;
            Engine::from_bytes(&bytes).map_err(|error| RuntimeError::Engine(error.to_string()))
        }
        Some("dfproject") => {
            Engine::load(project_path).map_err(|error| RuntimeError::Engine(error.to_string()))
        }
        _ => Err(RuntimeError::Config(format!(
            "unsupported project format {}: expected .yaml, .yml or .dfproject",
            project_path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::{EngineExecutor, OverrunPolicy, Runtime, RuntimeConfig, RuntimeState};

    /// Minimal valid DSL v2 project, embedded from the two-tank example.
    const TWO_TANK_YAML: &str = include_str!("../../../examples/two-tank/project.yaml");

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "df_runtime_loader_{}_{name}_{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_yaml_source_by_compiling_it() {
        let dir = temp_dir("yaml");
        let path = dir.join("two_tank.yaml");
        fs::write(&path, TWO_TANK_YAML).unwrap();

        let mut engine = load_engine(&path).unwrap();
        // The compiled engine reports the model's cycle time.
        assert!((engine.cycle_time().unwrap() - 0.5).abs() < f64::EPSILON);

        // The loaded engine runs through the realtime scheduler unchanged.
        let mut runtime = Runtime::start_with_config(
            engine,
            RuntimeConfig {
                overrun_policy: OverrunPolicy::Delay,
                write_queue_capacity: 4,
            },
        )
        .unwrap();
        let started = std::time::Instant::now();
        while runtime.snapshot().cycle_index < 2 {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(3),
                "engine loaded from YAML did not advance"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(runtime.status().state, RuntimeState::Running);
        runtime.stop().unwrap();
    }

    #[test]
    fn loads_compiled_dfproject() {
        let dir = temp_dir("dfproject");
        let yaml_path = dir.join("two_tank.yaml");
        let project_path = dir.join("two_tank.dfproject");
        fs::write(&yaml_path, TWO_TANK_YAML).unwrap();

        let bytes = df_core::compile_source(TWO_TANK_YAML).unwrap();
        fs::write(&project_path, &bytes).unwrap();

        let mut engine = load_engine(&project_path).unwrap();
        assert!((engine.cycle_time().unwrap() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn unsupported_extension_is_rejected() {
        let dir = temp_dir("txt");
        let path = dir.join("project.txt");
        fs::write(&path, "anything").unwrap();
        let error = load_engine(&path).err().expect("expected failure");
        assert!(
            error.to_string().contains("unsupported project format"),
            "{error}"
        );
    }

    #[test]
    fn no_extension_is_rejected() {
        let dir = temp_dir("noext");
        let path = dir.join("project");
        fs::write(&path, "anything").unwrap();
        let error = load_engine(&path).err().expect("expected failure");
        assert!(
            error.to_string().contains("unsupported project format"),
            "{error}"
        );
    }

    #[test]
    fn broken_yaml_source_reports_the_compile_error() {
        let dir = temp_dir("broken_yaml");
        let path = dir.join("broken.yaml");
        fs::write(&path, "dsl_version: 2\nmetadata: {name: broken\n").unwrap();
        let error = load_engine(&path).err().expect("expected failure");
        assert!(
            error.to_string().contains("cannot compile project source"),
            "{error}"
        );
    }

    #[test]
    fn corrupted_dfproject_reports_the_engine_error() {
        let dir = temp_dir("corrupt");
        let path = dir.join("corrupt.dfproject");
        fs::write(&path, b"definitely not a project").unwrap();
        let error = load_engine(&path).err().expect("expected failure");
        assert!(
            error.to_string().contains("runtime engine failure"),
            "{error}"
        );
    }

    #[test]
    fn missing_file_is_rejected() {
        let dir = temp_dir("missing");
        let error = load_engine(&dir.join("two_tank.yaml"))
            .err()
            .expect("expected failure");
        assert!(
            error.to_string().contains("cannot read project source"),
            "{error}"
        );
    }
}
