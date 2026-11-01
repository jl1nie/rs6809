#!/usr/bin/env python3
"""Fetch the CP/M-68K 1.3 binary distribution into artifacts/cpm68k/.

`68kv1_3.zip` from the Unofficial CP/M Web Site holds the system without a
BIOS as S-records (`CPM15000.SR`, `CPM400.SR` and their `.MAP` files), the
BIOS sources of the reference ports, the utilities (PIP, STAT, ED, DDT) and
the Alcyon C compiler. CP/M is offered under the 2022 licence from DRDOS,
Inc. (http://www.cpm.z80.de/license.html); whether Alcyon C is covered is not
stated, so neither is kept in the repository.
"""
import hashlib
import sys
import shutil
import subprocess
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/cpm68k"
URL = "http://www.cpm.z80.de/download/68kv1_3.zip"
SHA256 = "63afddd9bd39935cdf8e831344cc5861f1bdfb22d2729784624de6bc442e7f4a"


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    archive = OUT / "68kv1_3.zip"
    if not archive.exists():
        with urllib.request.urlopen(URL) as r:
            data = r.read()
        digest = hashlib.sha256(data).hexdigest()
        if digest != SHA256:
            sys.exit(f"SHA-256 mismatch: {digest}")
        archive.write_bytes(data)
    elif hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        sys.exit(f"{archive}: SHA-256 mismatch (delete it and run again)")
    # The archive uses the old "implode" method, which zipfile cannot read.
    if shutil.which("unzip") is None:
        sys.exit("unzip is required (sudo apt install unzip)")
    dist = OUT / "dist"
    shutil.rmtree(dist, ignore_errors=True)
    subprocess.run(["unzip", "-q", "-o", str(archive), "-d", str(dist)], check=True)
    files = [f for f in dist.rglob("*") if f.is_file()]
    print(f"{len(files)} files in {dist.relative_to(ROOT)}")

if __name__ == "__main__":
    sys.exit(main())
