#!/usr/bin/env python3
"""Run the maintained Aura fair byte benchmark under the campaign host lock.

The manifest is a JSON list of {name, aura0, aura1, events, levels}. Input data
and generated binary outputs stay outside git. Each process includes setup;
the nested benchmark's runtime_ns measures only buffer-resident conversion.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import time


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--binary", type=pathlib.Path, required=True)
    ap.add_argument("--manifest", type=pathlib.Path, required=True)
    ap.add_argument("--output", type=pathlib.Path, required=True)
    ap.add_argument("--commit", required=True)
    ap.add_argument("--lock", default="/tmp/lean-node-20260907-1000/bench.lock")
    ap.add_argument("--iterations", type=int, default=3)
    ap.add_argument("--warmups", type=int, default=1)
    ap.add_argument("--levels", nargs="+", type=int, default=[3, 19])
    ap.add_argument("--routes", choices=["both", "aura0", "zstd"], default="both")
    ap.add_argument("--process-repeats", type=int, default=1,
                    help="use 3 with --iterations 1 for old binaries that emit only aggregate timings")
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(args.manifest.read_text())
    summary = []
    with open(args.lock, "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        routes = ([] if args.routes == "zstd" else [("aura0", 3)]) + ([] if args.routes == "aura0" else [("zstd", x) for x in args.levels])
        for sample in manifest:
            for route, level, repeat in [(r, l, i) for r, l in routes for i in range(args.process_repeats)]:
                name = f"{sample['name']}-{route}-{level}-p{repeat}"
                report = args.output / f"{name}.json"
                usage = args.output / f"{name}.usage.json"
                command = [str(args.binary.resolve()), "--operation",
                           "aura0-to-aura1-bytes" if route == "aura0" else "zstd-aura1-to-aura1-bytes",
                           "--input", sample["aura0" if route == "aura0" else "aura1"],
                           "--reference-aura0", sample["aura0"],
                           "--reference-aura1", sample["aura1"],
                           "--dataset", sample["name"], "--zstd-level", str(level),
                           "--iterations", str(args.iterations), "--warmups", str(args.warmups),
                           "--format", "json", "--output", str(report)]
                if route == "aura0":
                    command += ["--unsupported-path", "fallback-to-stable"]
                before = pathlib.Path("/proc/loadavg").read_text().strip()
                with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
                    started = time.monotonic_ns()
                    proc = subprocess.Popen(command, stdout=stdout, stderr=stderr)
                    _, status, resources = os.wait4(proc.pid, 0)
                    proc.returncode = os.waitstatus_to_exitcode(status)
                    process_wall_ns = time.monotonic_ns() - started
                    stderr.seek(0)
                    error = stderr.read().decode(errors="replace")
                usage.write_text(json.dumps({"wall_ns": process_wall_ns,
                    "user_cpu_seconds": resources.ru_utime, "system_cpu_seconds": resources.ru_stime,
                    "peak_rss_kib": resources.ru_maxrss}, indent=2) + "\n")
                item = {"name": sample["name"], "route": route, "zstd_level": level, "process_repeat": repeat,
                        "command": command, "commit": args.commit,
                        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                        "loadavg_before": before, "finished_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                        "returncode": proc.returncode, "stderr": error,
                        "memory_scope": "process peak RSS includes preload, compression setup, warmup and all iterations",
                        "consumer": "complete sealed Aura1 bytes in memory; consumer parse/replay measured separately"}
                if proc.returncode == 0:
                    result = json.loads(report.read_text())
                    item.update({"median_runtime_ns": result["median_runtime_ns"],
                                 "stored_bytes": result["benchmark_input_bytes"],
                                 "aura1_bytes": result["output_bytes"],
                                 "events": sample.get("events"), "levels": sample.get("levels"),
                                 "events_per_second": (sample.get("events") or 0) * 1e9 / result["median_runtime_ns"],
                                 "levels_per_second": (sample.get("levels") or 0) * 1e9 / result["median_runtime_ns"]})
                summary.append(item)
                (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
                print(json.dumps({k: item.get(k) for k in ["name", "route", "zstd_level", "returncode", "median_runtime_ns", "stored_bytes"]}), flush=True)
                if proc.returncode:
                    raise SystemExit(error)


if __name__ == "__main__":
    main()
