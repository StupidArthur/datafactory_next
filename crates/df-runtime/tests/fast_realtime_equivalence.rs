//! Deterministic model-value equivalence across the Fast and Realtime hosts.
//!
//! The two hosts deliberately use separate Engine instances compiled from the
//! *same* DSL. Realtime write receipts identify the exact model cycle that
//! accepted each input; Fast replays that input at that cycle before comparing
//! complete snapshots. Wall-clock scheduling and skipped observations are not
//! used as model-value oracles.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use df_core::{Snapshot, Value};
use df_runtime::{load_engine, Runtime};

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/two-tank")
        .join(name)
}

fn assert_same_model_state(fast: &Snapshot, realtime: &Snapshot) {
    assert_eq!(fast.cycle_index, realtime.cycle_index, "cycle index");
    assert!(
        (fast.simulation_time - realtime.simulation_time).abs() < 1e-10,
        "simulation time at cycle {}: {} vs {}",
        fast.cycle_index,
        fast.simulation_time,
        realtime.simulation_time
    );
    assert_eq!(fast.values.len(), realtime.values.len(), "tag count");
    for (name, expected) in &fast.values {
        let actual = realtime.values.get(name).expect("missing realtime tag");
        match (expected, actual) {
            (Value::Float(a), Value::Float(b)) => assert!(
                a.is_finite() && b.is_finite()
                    && (a - b).abs() <= 1e-10 * (1.0 + a.abs().max(b.abs())),
                "{name} at cycle {}: {a} vs {b}",
                fast.cycle_index
            ),
            _ => assert_eq!(expected, actual, "{name} at cycle {}", fast.cycle_index),
        }
    }
}

/// A write can wait in the realtime queue for any number of wall-clock ticks.
/// The receipt, not wall-clock timing, determines its model-cycle position.
#[test]
fn two_tank_fast_realtime_equivalence_with_external_writes() {
    let source = example("project.yaml");
    let mut fast = load_engine(&source).expect("load fast Engine");
    let mut realtime = Runtime::start(load_engine(&source).expect("load realtime Engine"))
        .expect("start realtime scheduler");

    assert_same_model_state(&fast.snapshot(), &realtime.snapshot());

    for inlet in [0.0020, 0.0005, 0.0012] {
        let values = BTreeMap::from([("plant_inlet_flow".to_owned(), Value::Float(inlet))]);
        let ticket = realtime
            .submit_writes(values.clone())
            .expect("admit write batch");
        let receipt = ticket
            .wait(Duration::from_secs(8))
            .expect("write must be applied at one model cycle");
        let before = fast.snapshot().cycle_index;
        assert!(
            receipt.effective_cycle_index > before,
            "the realtime write must apply after the last compared cycle"
        );

        // Advance the Fast Engine across cycles where Realtime had no input.
        fast.execute(
            receipt.effective_cycle_index - before - 1,
            &BTreeMap::new(),
        )
        .expect("fast gap replay");
        let written = fast.execute(1, &values).expect("fast write replay");
        assert_eq!(written.cycle_index, receipt.effective_cycle_index);
        assert!(
            (written.simulation_time - receipt.simulation_time).abs() < 1e-10,
            "receipt simulation-time evidence"
        );

        // Realtime might have completed additional *empty-input* cycles by the
        // time this snapshot is read. Replay exactly those cycles as well.
        let observed = realtime.snapshot();
        assert!(observed.cycle_index >= written.cycle_index);
        fast.execute(
            observed.cycle_index - written.cycle_index,
            &BTreeMap::new(),
        )
        .expect("fast post-write replay");
        assert_same_model_state(&fast.snapshot(), &observed);
    }

    realtime.stop().expect("graceful shutdown");
}

/// The PID/valve/two-tank example is also computed by the same Engine in both
/// hosts; this does not claim PID settling or physical-controller validation.
#[test]
fn pid_example_fast_realtime_equivalence_without_external_writes() {
    let source = example("pid-closed-loop.yaml");
    let mut fast = load_engine(&source).expect("load fast PID Engine");
    let mut realtime = Runtime::start(load_engine(&source).expect("load realtime PID Engine"))
        .expect("start realtime PID scheduler");

    let deadline = Instant::now() + Duration::from_secs(8);
    while realtime.snapshot().cycle_index < 4 {
        assert!(Instant::now() < deadline, "realtime scheduler made no progress");
        thread::sleep(Duration::from_millis(10));
    }

    let observed = realtime.snapshot();
    fast.execute(observed.cycle_index, &BTreeMap::new())
        .expect("fast PID replay");
    assert_same_model_state(&fast.snapshot(), &observed);
    realtime.stop().expect("graceful shutdown");
}
