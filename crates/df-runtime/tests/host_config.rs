//! Host configuration integration checks (`doc/runtime-system-design.md` §14.1
//! CFG-01/CFG-03/CFG-04) against the shipped example fixture.

use std::collections::BTreeSet;
use std::path::PathBuf;

use df_runtime::{load_engine, EngineExecutor, HostConfig, OverrunPolicy};

fn example_config() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/host/runtime.yaml")
}

#[test]
fn example_host_config_lists_instances_with_resolved_projects() {
    let config = HostConfig::load(&example_config()).unwrap();

    assert_eq!(config.runtime_version, 1);
    assert!(config.opcua.is_some());
    assert_eq!(config.instances.len(), 2);

    let ids: BTreeSet<&str> = config.instances.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, BTreeSet::from(["tank-a", "tank-b"]));

    for instance in &config.instances {
        assert!(instance.project.is_absolute());
        assert!(instance.project.is_file());
        assert!(instance.enabled);
    }
    assert_eq!(
        config.instances[0].scheduler.overrun_policy,
        OverrunPolicy::Delay
    );
    assert_eq!(
        config.instances[1].scheduler.overrun_policy,
        OverrunPolicy::CatchUp
    );
}

#[test]
fn example_host_config_projects_load_as_engines() {
    let config = HostConfig::load(&example_config()).unwrap();
    for instance in &config.instances {
        let mut engine = load_engine(&instance.project).unwrap();
        // Both instances point at the two-tank model with a 0.5s cycle.
        let cycle_time = engine.cycle_time().unwrap();
        assert!((cycle_time - 0.5).abs() < f64::EPSILON);
    }
}
