#!/usr/bin/env python3
"""Compile a C file on the guest with source-line information for the ICE.

Runs Microware C on NitrOS-9 inside the emulator, without the optimizer:

  c.prep -l  →  c.pass1  →  c.pass2  →  c.asm -l  →  c.link -m -s

`c.prep -l` copies each source line into the assembly as a comment, the RMA
listing gives the offset of every instruction, and the linker map places
each section in the module. The listing (.lst), map (.map) and line records
(.m) are copied to the output directory; load them with
`--symbols DIR --source-dir <directory of the C file>`.
The executable is written to /dd/SOURCES/<name> on the guest disk.
"""
import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REF = ROOT / "reference/nitros9-basic09-starter-kit"
ROM = REF / "nitros9-runtime/6809M.bin"
OS9 = REF / "toolshed/build/unix/os9/os9"
SD_OFFSET = 80 * 1024 * 1024


def read_dsk(img):
    """The RBF image inside the SD card file: each 256-byte sector is stored twice."""
    with img.open("rb") as f:
        f.seek(SD_OFFSET)
        lsn0 = f.read(512)[:256]
        total = int.from_bytes(lsn0[0:3], "big")
        f.seek(SD_OFFSET)
        return b"".join(f.read(512)[:256] for _ in range(total))


def write_dsk(img, data):
    with img.open("r+b") as f:
        f.seek(SD_OFFSET)
        for offset in range(0, len(data), 256):
            sector = data[offset:offset + 256]
            f.write(sector + sector)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("source", type=Path, help="C source file on the host")
    parser.add_argument("--level2", action="store_true", help="use the Level 2 disk")
    parser.add_argument("--disk", type=Path, help="SD image (default: the C or Level 2 disk)")
    parser.add_argument("--out", type=Path, help="output directory (default: artifacts/cdebug/<name>)")
    args = parser.parse_args()
    source = args.source.resolve()
    name = source.stem
    if not name.replace("_", "").isalnum() or len(name) > 24:
        parser.error("the file name must be a short OS-9 name (letters, digits, _)")
    disk = (args.disk or ROOT / ("artifacts/level2/multicomp09-level2.img" if args.level2 else "artifacts/ccompiler/multicomp09-c.img")).resolve()
    out = (args.out or ROOT / "artifacts/cdebug" / name).resolve()
    for need in [ROM, OS9, disk, source]:
        if not need.is_file():
            raise SystemExit(f"missing {need}; see README.md")
    subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)
    emulator = ROOT / "target/release/rs6809"
    work = Path(tempfile.mkdtemp(prefix="c-debug-"))
    try:
        dsk = work / "disk.dsk"
        dsk.write_bytes(read_dsk(disk))
        subprocess.run([OS9, "copy", "-r", "-l", source, f"{dsk},SOURCES/{name}.c"], check=True)
        write_dsk(disk, dsk.read_bytes())
        steps = [
            "chd /dd/SOURCES",
            f"c.prep -l {name}.c >{name}.m",
            f"c.pass1 {name}.m -o={name}.i",
            f"c.pass2 {name}.i -o={name}.a",
            f"c.asm {name}.a -o={name}.r -l >{name}.lst",
            f"c.link /dd/lib/cstart.r {name}.r -o=/dd/SOURCES/{name} -l=/dd/lib/clib.l -m -s >{name}.map",
            "echo DEBUG-BUILD-DONE",
        ]
        script = work / "build.tsv"
        lines = [] if args.level2 else ["Time ?\t2026/10/31 12:00:00"]
        script.write_text("\n".join(lines + [f"OS9:\t{s}" for s in steps]) + "\n")
        run = subprocess.run(
            [emulator, "--rom", ROM, "--disk", disk, "--ram-kb", "512", "--boot", "--persist-disk",
             "--script", script, "--until", "\r\nDEBUG-BUILD-DONE\r\n", "--max-cycles", "3000000000"],
            stdin=subprocess.DEVNULL, capture_output=True, text=True, errors="replace")
        if run.returncode != 0:
            raise SystemExit(run.stdout[-2000:] + run.stderr)
        dsk.write_bytes(read_dsk(disk))
        out.mkdir(parents=True, exist_ok=True)
        for ext in ["lst", "map", "m"]:
            target = out / f"{name}.{ext}"
            target.unlink(missing_ok=True)
            subprocess.run([OS9, "copy", "-l", f"{dsk},SOURCES/{name}.{ext}", target], check=True, capture_output=True)
        listing = (out / f"{name}.lst").read_text(errors="replace")
        linkmap = (out / f"{name}.map").read_text(errors="replace")
        if "*** error" in listing or "Linkage map" not in linkmap:
            raise SystemExit(f"compilation failed; see {out}")
        print(f"Built /dd/SOURCES/{name} on {disk.relative_to(ROOT) if disk.is_relative_to(ROOT) else disk}")
        print(f"Symbols in {out}")
        print(f"  --symbols {out} --source-dir {source.parent}")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
