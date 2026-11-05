#!/usr/bin/env python3
"""Physical macOS 2-instance Runtime soak recorder (stdlib only).

This is not a replacement for OPC UA external-client testing.
Usage:
    python3 work/macos_soak.py --out work/reports/macos-YYYYMMDD-HHMMSS/soak

Tests default to the existing two-instance host fixture on main.
"""
from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
STATUS = re.compile(
    r"^instance (?P<id>[^:]+): state=(?P<state>\w+) "
    r"cycle_index=(?P<cycle>\d+) simulation_time=(?P<sim>[0-9.eE+-]+)s"
)


def sample_resources(pid: int) -> tuple[int | None, float | None]:
    result = subprocess.run(
        ["ps", "-p", str(pid), "-o", "rss=", "-o", "%cpu="],
        capture_output=True,
        text=True,
        check=False,
    )
    fields = result.stdout.strip().split()
    if result.returncode != 0 or len(fields) < 2:
        return None, None
    try:
        return int(fields[0]), float(fields[1])
    except ValueError:
        return None, None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--duration-secs", type=int, default=1800)
    parser.add_argument("--sample-secs", type=int, default=30)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--binary", type=Path, default=ROOT / "target/debug/df-runtime"
    )
    parser.add_argument(
        "--config", type=Path, default=ROOT / "examples/host/runtime.yaml"
    )
    parser.add_argument("--instances", default="tank-a,tank-b")
    args = parser.parse_args()
    if args.duration_secs < 60 or args.sample_secs < 1:
        parser.error("duration-secs must be >= 60 and sample-secs >= 1")

    names = [name.strip() for name in args.instances.split(",") if name.strip()]
    if len(names) < 2 or len(set(names)) != len(names):
        parser.error("--instances must specify at least two different IDs")

    binary = args.binary.resolve()
    config = args.config.resolve()
    if not binary.is_file() or not config.is_file():
        parser.error(f"missing Runtime binary or host config: {binary}, {config}")

    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    log_path = out / "runtime.log"
    csv_path = out / "resources.csv"
    report_path = out / "soak-summary.json"

    status = {name: [] for name in names}
    samples = []
    notes = []
    duration = 0.0
    returncode = None
    print(f"Starting {args.duration_secs}s soak, pid/resources in {csv_path}", flush=True)

    with log_path.open("w", encoding="utf-8") as log, csv_path.open(
        "w", newline="", encoding="utf-8"
    ) as csvfile:
        writer = csv.writer(csvfile)
        writer.writerow(["elapsed_s", "rss_kib", "cpu_percent", "process_alive"])
        proc = subprocess.Popen(
            [str(binary), "run", "--config", str(config)],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        started = time.monotonic()
        try:
            while (time.monotonic() - started) < args.duration_secs:
                elapsed = time.monotonic() - started
                if proc.poll() is not None:
                    notes.append(f"Runtime exited unexpectedly after {elapsed:.1f}s")
                    break

                assert proc.stdin is not None
                for name in names:
                    proc.stdin.write(f"status {name}\n")
                proc.stdin.flush()

                rss, cpu = sample_resources(proc.pid)
                writer.writerow([round(elapsed, 2), rss, cpu, proc.poll() is None])
                samples.append({"t": elapsed, "rss_kib": rss, "cpu_percent": cpu})
                csvfile.flush()
                print(f"{elapsed:7.1f}s / {args.duration_secs}s  rss={rss} KiB cpu={cpu}%", flush=True)
                time.sleep(min(args.sample_secs, max(0.0, args.duration_secs - elapsed)))
            duration = time.monotonic() - started

            if proc.poll() is None:
                assert proc.stdin is not None
                for name in names:
                    proc.stdin.write(f"status {name}\n")
                    proc.stdin.write(f"snapshot {name}\n")
                proc.stdin.write("stop\n")
                proc.stdin.flush()
            try:
                returncode = proc.wait(timeout=45)
            except subprocess.TimeoutExpired:
                notes.append("Runtime did not stop within 45s")
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
                returncode = proc.returncode
        except (KeyboardInterrupt, BrokenPipeError, OSError) as error:
            notes.append(f"Interrupted: {error}")
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
            returncode = proc.returncode
            duration = time.monotonic() - started

    for line in log_path.read_text(encoding="utf-8", errors="replace").splitlines():
        match = STATUS.match(line)
        if match and match.group("id") in status:
            status[match.group("id")].append(
                {
                    "state": match.group("state"),
                    "cycle_index": int(match.group("cycle")),
                    "simulation_time": float(match.group("sim")),
                }
            )
    gates = {
        "process_exit_zero": returncode == 0,
        "ran_full_duration": duration >= args.duration_secs - 1,
        "both_instances_reported": all(status[name] for name in names),
        "no_instance_failed": all(
            all(row["state"] == "Running" for row in status[name])
            for name in names
        ),
        "cycle_monotonic": all(
            all(b["cycle_index"] > a["cycle_index"] for a, b in zip(status[name], status[name][1:]))
            for name in names
        ),
        "sim_time_matches_cycle_half_second": all(
            all(abs(row["simulation_time"] - 0.5 * row["cycle_index"]) < 0.001 for row in status[name])
            for name in names
        ),
        "cycle_progress_95_percent": all(
            status[name] and status[name][-1]["cycle_index"] >= 0.95 * args.duration_secs / 0.5
            for name in names
        ),
        "resource_samples_collected": len(samples) >= max(2, int(args.duration_secs / args.sample_secs) - 1)
        and all(sample["rss_kib"] is not None for sample in samples),
    }
    usable = [row for row in samples if row["rss_kib"] is not None]
    rss_max = max((row["rss_kib"] for row in usable), default=None)
    warm = [row["rss_kib"] for row in usable if row["t"] >= 300]
    if len(warm) >= 10:
        # Diagnostic only: macOS RSS fluctuations are workload dependent.
        rss_start = sum(warm[: min(10, len(warm))]) / min(10, len(warm))
        rss_end = sum(warm[-min(10, len(warm)):]) / min(10, len(warm))
        if rss_end > rss_start * 1.2 + 50 * 1024:
            notes.append("RSS increased >20% plus 50MiB after warm-up: investigate")
    report = {
        "command": [str(binary), "run", "--config", str(config)],
        "requested_duration_secs": args.duration_secs,
        "elapsed_secs": round(duration, 2),
        "exit_code": returncode,
        "instance_results": {
            name: {
                "samples": len(status[name]),
                "first": status[name][0] if status[name] else None,
                "last": status[name][-1] if status[name] else None,
            }
            for name in names
        },
        "resource_samples": len(samples),
        "max_rss_kib": rss_max,
        "max_cpu_percent": max((row["cpu_percent"] for row in samples if row["cpu_percent"] is not None), default=None),
        "gates": gates,
        "notes": notes,
        "pass": all(gates.values()) and not notes,
    }
    report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Summary: {report_path}")
    print(f"Result: {'PASS' if report['pass'] else 'FAIL'}")
    if notes:
        print("\n".join(notes), file=sys.stderr)
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
