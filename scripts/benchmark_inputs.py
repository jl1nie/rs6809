"""Frozen guest inputs: application demo changes must not change emulator comparisons."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def fixed_inputs():
    manifest = json.loads((ROOT / "benchmarks/fixed-inputs.json").read_text())
    paths = []
    for item in manifest["files"]:
        path = ROOT / item["path"]
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != item["sha256"]:
            raise SystemExit(f"Frozen benchmark input missing or changed: {path}")
        paths.append(path)
    return tuple(paths)


def workload_inputs(name):
    if name == "fixed":
        return fixed_inputs(), "Mandelbrot / Microware C / 6809", "fixed-point Mandelbrot, 76x25, 48 iterations"
    paths = (ROOT / "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
             ROOT / "artifacts/ccompiler/multicomp09-c.img", ROOT / "examples/mandelbrot.tsv")
    if not all(path.is_file() for path in paths):
        raise SystemExit("Build the C demo first: python3 scripts/build-c-disk.py")
    return paths, "Mandelbrot / BASIC09 to C / 6809", "BASIC09-to-C Mandelbrot, 79x25, 16 real iterations"
