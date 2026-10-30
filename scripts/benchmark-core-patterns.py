#!/usr/bin/env python3
"""Cross-check candidate patterns on flat RAM, mapped RAM, and serial workloads."""
import argparse
import json
import hashlib
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/optimization/round2"



def interleaved(args):
    cases = args.cases or ["base", "hybrid-map", "hybrid-map+word-inline", "hybrid-map+flag-mask", "current"]
    if args.pin:
        os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})
    binaries = {name: (ROOT if name == "current" else OUT / name) / "target/release/examples/bench_core" for name in cases}
    if not all(p.is_file() for p in binaries.values()):
        raise SystemExit("Build all requested cases with this script first")
    runs = {name: {} for name in cases}
    medians = {name: {} for name in cases}
    signatures = {}
    for mode in ["indexed-flat", "indexed-mmu", "serial"]:
        for name, binary in binaries.items():
            subprocess.run([str(binary), mode, "100000000"], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
            runs[name][mode] = []
        for i in range(args.runs):
            order = cases[i % len(cases):] + cases[:i % len(cases)]
            if i % 2:
                order = list(reversed(order))
            for name in order:
                row = json.loads(subprocess.check_output([str(binaries[name]), mode, "100000000"], cwd=ROOT, text=True))
                signature = {k: row[k] for k in ["cycles", "steps", "checksum", "tx_bytes"]}
                if mode not in signatures:
                    signatures[mode] = signature
                elif signature != signatures[mode]:
                    raise SystemExit(f"Guest state mismatch: {name}/{mode}")
                runs[name][mode].append(row)
        for name in cases:
            median = statistics.median(r["seconds"] for r in runs[name][mode])
            medians[name][mode] = median
            print(f"{name:40} {mode:14} {median:.6f} s", flush=True)
    report = {"runs": runs, "medians": medians, "affinity": sorted(os.sched_getaffinity(0)),
              "binary_sha256": {name: hashlib.sha256(p.read_bytes()).hexdigest() for name, p in binaries.items()}}
    (OUT / "core-interleaved.json").write_text(json.dumps(report, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", action="append", dest="cases")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--interleaved", action="store_true")
    parser.add_argument("--pin", action="store_true", help="Pin this comparison to the first allowed CPU")
    args = parser.parse_args()
    if args.interleaved:
        interleaved(args)
        return
    base = OUT / "base"
    source = base / "source"
    source.mkdir(parents=True, exist_ok=True)
    with tarfile.open(base / "source.tar.gz") as archive:
        archive.extractall(source, filter="data")
    path = OUT / "core-patterns.json"
    results = json.loads(path.read_text()) if path.is_file() else {}
    checksums = {}
    if "base" in results:
        checksums = {mode: {k: data["runs"][0][k] for k in ["cycles", "steps", "checksum", "tx_bytes"]}
                     for mode, data in results["base"].items()}
    for name in args.cases or ["base", "cached-map", "flag-mask", "cached-map+flag-mask", "cached-map+flag-mask+word-inline"]:
        work = ROOT if name == "current" else OUT / name
        source = ROOT if name == "current" else work / "source"
        if name != "current":
            (source / "examples").mkdir(exist_ok=True)
            shutil.copyfile(ROOT / "examples/bench_core.rs", source / "examples/bench_core.rs")
        subprocess.run(["cargo", "build", "--release", "--example", "bench_core", "--manifest-path", str(source / "Cargo.toml"),
                        "--target-dir", str(work / "target")], check=True)
        binary = work / "target/release/examples/bench_core"
        by_mode = {}
        for mode in ["indexed-flat", "indexed-mmu", "serial"]:
            command = [str(binary), mode, "100000000"]
            subprocess.run(command, cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
            runs = []
            for _ in range(args.runs):
                result = json.loads(subprocess.check_output(command, cwd=ROOT, text=True))
                signature = {k: result[k] for k in ["cycles", "steps", "checksum", "tx_bytes"]}
                if mode not in checksums:
                    checksums[mode] = signature
                elif checksums[mode] != signature:
                    raise SystemExit(f"{name}/{mode}: incorrect guest state: {signature} != {checksums[mode]}")
                runs.append(result)
            median = statistics.median(row["seconds"] for row in runs)
            by_mode[mode] = {"median_seconds": median, "runs": runs}
            print(f"{name:40} {mode:14} {median:.6f} s", flush=True)
        results[name] = by_mode
        (OUT / "core-patterns.json").write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
