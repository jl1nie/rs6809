#!/usr/bin/env python3
"""Compile/run the C port and compare its 1,975 cells with the original BASIC09."""
import argparse
import json
import os
from pathlib import Path
import re
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/rs6809")
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/ccompiler/basic09-comparison.json")
    args = parser.parse_args()
    common = [str(args.binary.resolve()), "--rom", "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
              "--disk", "artifacts/ccompiler/multicomp09-c.img", "--boot"]
    c = subprocess.run([*common, "--script", "examples/mandelbrot.tsv", "--until", "MANDELBROT-DONE",
                        "--max-cycles", "1000000000"], cwd=ROOT, stdin=subprocess.DEVNULL,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
    commands = ("Time ?\t2026/10/31 12:00:00\nOS9:\tbasic09\n"
                "B:\tload /dd/SOURCES/mandel.b09\nB:\trun mandel\nB:\tbye\n"
                "OS9:\techo REFERENCE-DONE\n")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".tsv") as script:
        script.write(commands)
        script.flush()
        process = subprocess.Popen([*common, "--script", script.name, "--until", "\r\nREFERENCE-DONE\r\n",
                                    "--max-cycles", "10000000000"], cwd=ROOT, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        transcript = bytearray()
        last = time.monotonic()
        deadline = last + 20
        keys = 0
        try:
            while process.poll() is None and time.monotonic() < deadline:
                ready, _, _ = select.select([process.stdout], [], [], 0.05)
                if ready:
                    data = os.read(process.stdout.fileno(), 4096)
                    if data:
                        transcript.extend(data)
                        last = time.monotonic()
                pos = transcript.find(b"B:run mandel")
                # BASIC09 keeps terminal paging enabled; advance a paused page.
                if pos >= 0 and b"Ready" not in transcript[pos:] and time.monotonic() - last > 0.15:
                    process.stdin.write(b"\r")
                    process.stdin.flush()
                    last = time.monotonic()
                    keys += 1
            if process.poll() is None:
                process.terminate()
            remaining, errors = process.communicate(timeout=5)
            transcript.extend(remaining)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    original = transcript.decode("latin1").replace("\r\n", "\n")
    brows = [line for line in original.splitlines() if re.fullmatch("[0-9A-F ]{79}", line)]
    port = c.stdout.decode("latin1").replace("\r\n", "\n")
    crows = port.split("Mandelbrot / BASIC09 to C / 6809\n", 1)[1].split("MANDELBROT-DONE", 1)[0].splitlines()
    complete = len(brows) == len(crows) == 25 and process.returncode == 0
    different = sum(a != b for ra, rb in zip(brows, crows) for a, b in zip(ra, rb)) if complete else None
    report = {"width": 79, "height": 25, "cells": 1975, "basic09_rows": len(brows), "c_rows": len(crows),
              "different_cells": different, "match": complete and different == 0, "page_advance_keys": keys,
              "stderr": errors.decode("latin1")}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    args.output.with_suffix(".basic09.txt").write_bytes(transcript)
    args.output.with_suffix(".c.txt").write_bytes(c.stdout)
    print(json.dumps(report, indent=2))
    if not report["match"]:
        raise SystemExit("C and BASIC09 output differ or execution did not complete")


if __name__ == "__main__":
    main()
