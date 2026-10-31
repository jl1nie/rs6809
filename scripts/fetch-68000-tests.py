#!/usr/bin/env python3
"""Fetch the SingleStepTests 68000 suite and convert it for the Rust tests.

The suite (https://github.com/SingleStepTests/680x0, pinned below) has no
licence statement, so it is downloaded at test time rather than kept in
the repository. Each `68000/v1/*.json.gz` becomes `artifacts/sst68000/*.bin`:

    b"SST1", u32 count, then per test:
      u16 name length, name (UTF-8)
      initial state, final state:
        19 x u32  d0-d7, a0-a6, usp, ssp, sr, pc
        2 x u16   prefetch
        u32 n, n x (u32 address, u8 value)
      u32 length (cycles)

All integers are little-endian. Bus transactions are not kept.
"""
import gzip
import json
import struct
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/sst68000"
COMMIT = "e0d5ece9670205cc84a0101081837deb446f86a3"
API = f"https://api.github.com/repos/SingleStepTests/680x0/contents/68000/v1?ref={COMMIT}"
RAW = f"https://raw.githubusercontent.com/SingleStepTests/680x0/{COMMIT}/68000/v1/"
REGS = ["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "usp", "ssp", "sr", "pc"]


def state(s):
    out = struct.pack("<19I", *(s[r] & 0xFFFFFFFF for r in REGS))
    out += struct.pack("<2H", *s["prefetch"])
    out += struct.pack("<I", len(s["ram"]))
    for addr, value in s["ram"]:
        out += struct.pack("<IB", addr, value)
    return out


def convert(raw):
    tests = json.loads(gzip.decompress(raw))
    out = bytearray(b"SST1" + struct.pack("<I", len(tests)))
    for t in tests:
        name = t["name"].encode()
        out += struct.pack("<H", len(name)) + name
        out += state(t["initial"]) + state(t["final"])
        out += struct.pack("<I", t["length"])
    return bytes(out)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(API) as r:
        names = sorted(e["name"] for e in json.load(r) if e["name"].endswith(".json.gz"))
    for i, name in enumerate(names, 1):
        target = OUT / name.replace(".json.gz", ".bin")
        if target.exists():
            continue
        with urllib.request.urlopen(RAW + name) as r:
            data = convert(r.read())
        target.write_bytes(data)
        print(f"[{i}/{len(names)}] {target.name}", flush=True)
    print(f"{len(names)} files in {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    sys.exit(main())
