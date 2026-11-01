#!/usr/bin/env python3
"""Assemble examples/cpm68k/bios.s into bios.sr (S-records) and bios.lst.

Needs GNU binutils for m68k (Ubuntu: binutils-m68k-linux-gnu). The outputs
are kept in the repository, so running CP/M-68K does not need them.
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "examples/cpm68k/bios.s"
BASE = 0x1B000


def tool(name):
    for prefix in ("m68k-linux-gnu-", "m68k-elf-"):
        if shutil.which(prefix + name):
            return prefix + name
    sys.exit(f"{name} for m68k not found (sudo apt install binutils-m68k-linux-gnu)")


def main():
    out = SRC.with_suffix(".sr")
    lst = SRC.with_suffix(".lst")
    with tempfile.TemporaryDirectory() as tmp:
        obj = Path(tmp) / "bios.o"
        elf = Path(tmp) / "bios.elf"
        subprocess.run(
            # Relative names keep host paths out of the listing.
            [tool("as"), "-m68000", "--register-prefix-optional", f"-al={lst.name}", "-o", obj, SRC.name],
            check=True,
            cwd=SRC.parent,
        )
        # .data and .bss follow .text, all from $1B000.
        subprocess.run([tool("ld"), "-N", f"-Ttext=0x{BASE:X}", "-e", "_init", "--no-warn-rwx-segments", "-o", elf, obj], check=True)
        subprocess.run([tool("objcopy"), "-O", "srec", "--srec-forceS3", elf, out], check=True)
        syms = subprocess.run([tool("nm"), "-n", elf], check=True, capture_output=True, text=True).stdout
    # The S0 header would carry the host path of the ELF file.
    name = b"bios.sr"
    body = bytes([len(name) + 3, 0, 0]) + name
    s0 = "S0" + (body + bytes([~sum(body) & 0xFF])).hex().upper()
    out.write_text("\n".join([s0] + out.read_text().splitlines()[1:]) + "\n")
    lines = [line.split() for line in syms.splitlines()]
    (SRC.with_suffix(".map")).write_text("".join(f"{a} {t} {n}\n" for a, t, n in lines if t.lower() != "a"))
    end = max(int(a, 16) for a, t, _ in lines if t.lower() in "tdb")
    print(f"{out.relative_to(ROOT)}: ${BASE:X}-${end:X}")


if __name__ == "__main__":
    sys.exit(main())
