//! Manually gated long-run / instance-scale measurements. Not a default CI test.
//!
//! Run with:
//!   DF_SOAK_DURATION_SECS=1800 DF_SOAK_INSTANCES=2 \
//!     cargo test -p df-runtime --test runtime_soak -- --ignored --nocapture
//!
//! To save CSV directly, set DF_SOAK_OUTPUT=<path>. The test records runtime
//! scheduler facts. CPU, RSS and OPC UA client-side latency need separate
//! host instrumentation; none are inferred from these counters.

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use df_runtime::{load_engine, Runtime, RuntimeState};

fn positive_env(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(raw) => {
            let parsed: u64 = raw.parse().expect("expected unsigned decimal integer");
            assert!(parsed > 0, "{name} must be > 0");
            parsed
        }
        Err(_) => default,
    }
}

#[test]
#[ignore = "manual capacity / 30-minute long-stability run with explicit host conditions"]
fn realtime_multi_instance_soak_csv() {
    let duration_secs = positive_env("DF_SOAK_DURATION_SECS", 1800);
    let instances = positive_env("DF_SOAK_INSTANCES", 2);
    assert!(instances <= 256, "avoid accidental unbounded process load");

    let project =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/two-tank/project.yaml");
    let mut runtimes: Vec<Runtime> = (0..instances)
        .map(|_| {
            Runtime::start(load_engine(&project).expect("load example Engine"))
                .expect("start realtime instance")
        })
        .collect();

    let mut out: Box<dyn Write> = match std::env::var("DF_SOAK_OUTPUT") {
        Ok(path) => Box::new(File::create(path).expect("create CSV")),
        Err(_) => Box::new(io::stdout()),
    };
    writeln!(
        out,
        "utc_epoch_ms,elapsed_sec,instance,cycle_index,simulation_time,last_execution_ms,max_execution_ms,overrun_count,missed_deadlines,rejected_write_batches,queue_depth,state"
    )
    .unwrap();

    let started = Instant::now();
    let duration = Duration::from_secs(duration_secs);
    while started.elapsed() < duration {
        thread::sleep(Duration::from_secs(1));
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("valid system clock")
            .as_millis();
        for (index, runtime) in runtimes.iter().enumerate() {
            let status = runtime.status();
            assert_ne!(
                status.state,
                RuntimeState::Failed,
                "instance {index}: {:?}",
                status.last_error
            );
            writeln!(
                out,
                "{timestamp},{:.3},{index},{},{:.9},{:.6},{:.6},{},{},{},{},{:?}",
                started.elapsed().as_secs_f64(),
                status.cycle_index,
                status.simulation_time,
                status.last_execution_duration.as_secs_f64() * 1000.0,
                status.max_execution_duration.as_secs_f64() * 1000.0,
                status.overrun_count,
                status.missed_deadlines,
                status.rejected_write_batches,
                status.queue_depth,
                status.state
            )
            .expect("append CSV");
        }
    }
    out.flush().unwrap();
    for runtime in &mut runtimes {
        runtime.stop().expect("graceful stop");
        assert!(
            runtime.status().cycle_index > 0,
            "instance made no progress"
        );
    }
}
