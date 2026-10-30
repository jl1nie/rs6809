#!/usr/bin/env python3
"""Repeat the native-C Mandelbrot workload without rebuilding the emulator."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time
from benchmark_inputs import workload_inputs

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/rs6809")
    parser.add_argument("--workload", choices=["basic09", "fixed"], default="basic09")
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/benchmark/current.json")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    binary = args.binary.resolve()
    paths, marker, label = workload_inputs(args.workload)
    rom, disk, script = paths
    command = [str(binary), "--rom", str(rom), "--disk", str(disk), "--boot", "--script", str(script),
               "--until", "MANDELBROT-DONE", "--max-cycles", "1000000000",
               "--measure-from", marker]
    # One warm-up; exclude it from the reported timing distribution.
    subprocess.run(command, cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                   stderr=subprocess.PIPE, check=True)
    runs = []
    for index in range(args.runs):
        start = time.perf_counter()
        result = subprocess.run(command, cwd=ROOT, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        wall = time.perf_counter() - start
        stats = result.stderr.decode()
        measured = re.search(r"measured_cycles=(\d+) host_seconds=([0-9.]+)", stats)
        total = re.search(r"total_cycles=(\d+) host_seconds=([0-9.]+)", stats)
        if not measured or not total or b"MANDELBROT-DONE" not in result.stdout:
            raise SystemExit("Incomplete workload or missing statistics")
        cycles, seconds = int(measured[1]), float(measured[2])
        row = {"run": index + 1, "draw_cycles": cycles, "draw_seconds": seconds,
               "effective_mhz": cycles / seconds / 1e6, "total_cycles": int(total[1]),
               "total_seconds": float(total[2]), "process_seconds": wall,
               "stdout_sha256": hashlib.sha256(result.stdout).hexdigest()}
        runs.append(row)
        print(f"{index + 1}: {seconds:.6f} s, {row['effective_mhz']:.1f} MHz equivalent", flush=True)
    if len({(r['draw_cycles'], r['total_cycles'], r['stdout_sha256']) for r in runs}) != 1:
        raise SystemExit("Non-deterministic guest cycles or output; investigate before comparing speed")
    times = [r["draw_seconds"] for r in runs]
    median = statistics.median(times)
    cpu = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")), "unknown")
    report = {"workload": label, "cpu_model": cpu,
              "kernel": platform.release(), "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
              "binary": str(binary), "binary_sha256": digest(binary),
              "inputs": {str(p.relative_to(ROOT)): digest(p) for p in [rom, disk, script]},
              "command": command, "warmup_runs": 1, "runs": runs,
              "summary": {"median_draw_seconds": median, "min_draw_seconds": min(times),
                          "max_draw_seconds": max(times), "draw_cycles": runs[0]["draw_cycles"],
                          "median_effective_mhz": runs[0]["draw_cycles"] / median / 1e6}}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Median: {median:.6f} s; saved {args.output}")


if __name__ == "__main__":
    main()
