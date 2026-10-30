#!/usr/bin/env python3
"""Attribute perf samples to the innermost project function, including inlined code."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--data", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--skip-first", type=float, default=0.3)
    parser.add_argument("--source-root", type=Path, help="Matching source tree for this binary")
    parser.add_argument("--perf", default="/usr/lib/linux-tools/6.8.0-147-generic/perf")
    args = parser.parse_args()
    symbols = {}
    for line in subprocess.check_output(["nm", "-n", str(args.binary)], text=True).splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[1] in "tT":
            symbols[parts[2]] = int(parts[0], 16)
    raw = subprocess.check_output([args.perf, "script", "-i", str(args.data), "-F", "+symoff"], text=True, stderr=subprocess.DEVNULL)
    samples = []
    for block in raw.split("\n\n"):
        lines = block.splitlines()
        if not lines:
            continue
        header = re.search(r"\s(\d+\.\d+):\s+(\d+) cpu-clock:u:", lines[0])
        if not header:
            continue
        address = None
        top_ip = None
        for line in lines[1:]:
            match = re.match(r"\s+([0-9a-f]+)\s+(\S+)\s+\(", line)
            if match:
                if top_ip is None:
                    top_ip = match[1]
                elif match[1] != top_ip:
                    break
                token = match[2]
                name, separator, offset = token.rpartition("+0x")
                if not separator:
                    name, offset = token, "0"
                if name in symbols:
                    address = symbols[name] + int(offset, 16)
                    break
        samples.append((float(header[1]), int(header[2]), address))
    if not samples:
        raise SystemExit("No samples parsed")
    start = min(s[0] for s in samples) + args.skip_first
    samples = [s for s in samples if s[0] >= start]
    addresses = sorted({s[2] for s in samples if s[2] is not None})
    resolved = subprocess.check_output(["llvm-addr2line", "-e", str(args.binary), "-a", "-f", "-i"],
                                      input="\n".join(hex(a) for a in addresses) + "\n", text=True)
    frames = {}
    address = None
    for line in resolved.splitlines():
        if re.fullmatch(r"0x[0-9a-fA-F]+", line):
            address = int(line, 16)
            frames[address] = []
        elif address is not None:
            frames[address].append(line)
    function_cache = {}
    def function_at(path, number):
        if args.source_root:
            path = str(args.source_root / "src" / Path(path).name)
        if path not in function_cache:
            found = []
            for index, line in enumerate(Path(path).read_text().splitlines(), 1):
                name = re.search(r"\bfn\s+(\w+)\s*\(", line)
                if name:
                    found.append((index, name[1]))
            function_cache[path] = found
        function = next((name for index, name in reversed(function_cache[path]) if index <= number), "unknown")
        return f"{Path(path).name}::{function}"
    counts, lines = Counter(), Counter()
    total = sum(s[1] for s in samples)
    for _, weight, address in samples:
        key = "external-or-unresolved"
        values = frames.get(address, [])
        for index in range(1, len(values), 2):
            location = re.match(r"(.+/src/(?:cpu|machine|main)\.rs):(\d+)", values[index])
            if location and int(location[2]) > 0:
                path, number = location[1], int(location[2])
                key = function_at(path, number)
                lines[f"{Path(path).name}:{number}"] += weight
                break
        counts[key] += weight
    report = {"samples": len(samples), "skip_first_seconds": args.skip_first, "total_period": total,
              "functions": [{"function": name, "percent": count / total * 100} for name, count in counts.most_common()],
              "source_lines": [{"line": name, "percent": count / total * 100} for name, count in lines.most_common(30)]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    for row in report["functions"][:15]:
        print(f"{row['percent']:6.2f}%  {row['function']}")


if __name__ == "__main__":
    main()
