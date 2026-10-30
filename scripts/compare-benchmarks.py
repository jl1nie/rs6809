#!/usr/bin/env python3
"""Alternate two emulator binaries to reduce measurement-order bias."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
from benchmark_inputs import workload_inputs

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, default=ROOT / "target/release/rs6809")
    parser.add_argument("--workload", choices=["basic09", "fixed"], default="basic09")
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    files, marker, label = workload_inputs(args.workload)
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    common = ["--rom", str(files[0]), "--disk", str(files[1]), "--boot", "--script", str(files[2]),
              "--until", "MANDELBROT-DONE", "--max-cycles", "1000000000",
              "--measure-from", marker]
    signature = None
    def run(name):
        nonlocal signature
        process = subprocess.run([str(binaries[name]), *common], cwd=ROOT, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        stats = process.stderr.decode()
        draw = re.search(r"measured_cycles=(\d+) host_seconds=([0-9.]+)", stats)
        total = re.search(r"total_cycles=(\d+) host_seconds=([0-9.]+)", stats)
        if not draw or not total or b"MANDELBROT-DONE" not in process.stdout:
            raise SystemExit("Incomplete workload")
        current = {"draw_cycles": int(draw[1]), "total_cycles": int(total[1]),
                   "stdout_sha256": hashlib.sha256(process.stdout).hexdigest()}
        if signature is None:
            signature = current
        elif current != signature:
            raise SystemExit(f"Guest behavior differs: {current} != {signature}")
        return {**current, "seconds": float(draw[2]), "total_seconds": float(total[2])}
    for name in binaries:
        run(name)
    runs = {name: [] for name in binaries}
    order_log = []
    for index in range(args.runs):
        order = list(binaries) if index % 2 == 0 else list(reversed(binaries))
        order_log.append(order)
        for name in order:
            row = run(name)
            runs[name].append(row)
            print(f"{index + 1} {name}: {row['seconds']:.6f} s", flush=True)
    summaries = {}
    for name, rows in runs.items():
        times = [r["seconds"] for r in rows]
        median = statistics.median(times)
        summaries[name] = {"median_seconds": median, "min_seconds": min(times), "max_seconds": max(times),
                           "effective_mhz": signature["draw_cycles"] / median / 1e6}
    report = {"workload": label, "binary_sha256": {name: sha(p) for name, p in binaries.items()},
              "inputs": {str(p.relative_to(ROOT)): sha(p) for p in files},
              "warmup_each": 1, "order": order_log, "runs": runs, "summary": summaries,
              "speedup": summaries["baseline"]["median_seconds"] / summaries["candidate"]["median_seconds"]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Speedup {report['speedup']:.4f}x; {args.output}")


if __name__ == "__main__":
    main()
