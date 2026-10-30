#!/usr/bin/env python3
"""Enable perf sampling only after the guest Mandelbrot drawing banner."""
import argparse
import os
from pathlib import Path
import subprocess
from benchmark_inputs import fixed_inputs

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--perf", default="/usr/lib/linux-tools/6.8.0-147-generic/perf")
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    rom, disk, script = fixed_inputs()
    read_fd, write_fd = os.pipe()
    command = [args.perf, "record", "-e", "cpu-clock:u", "-F", "997", "--call-graph", "dwarf",
               "--delay", "-1", "--control", f"fd:{read_fd}", "-o", str(args.output), "--", str(args.binary.resolve()),
               "--rom", str(rom), "--disk", str(disk), "--boot", "--script", str(script),
               "--until", "MANDELBROT-DONE", "--max-cycles", "1000000000",
               "--measure-from", "Mandelbrot / Microware C / 6809"]
    transcript = bytearray()
    enabled = False
    with args.output.with_suffix(".stderr.txt").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=errors, pass_fds=(read_fd,))
        os.close(read_fd)
        try:
            for line in iter(process.stdout.readline, b""):
                transcript.extend(line)
                if not enabled and b"Mandelbrot / Microware C / 6809" in line:
                    os.write(write_fd, b"enable\n")
                    enabled = True
            status = process.wait(timeout=30)
        finally:
            os.close(write_fd)
    args.output.with_suffix(".stdout.txt").write_bytes(transcript)
    if status != 0 or not enabled or b"MANDELBROT-DONE" not in transcript:
        raise SystemExit(f"Profile failed; see {args.output.with_suffix('.stderr.txt')}")
    print(f"Drawing-only profile saved to {args.output}")


if __name__ == "__main__":
    main()
