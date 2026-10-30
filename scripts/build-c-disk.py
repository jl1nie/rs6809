#!/usr/bin/env python3
"""Build a separate NitrOS-9 disk with the native Microware C toolchain."""
from pathlib import Path
import hashlib
import os
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
REF = ROOT / "reference/nitros9-basic09-starter-kit"
OUT = ROOT / "artifacts/ccompiler"
TOOLS = ROOT / "artifacts/tools"
LW_VERSION = "4.25"
LW_SHA256 = "9bb97e987d486d7abd85d04cb98a53f666f02785c108647b795c4a111c5eb09f"


def run(*args, cwd=None):
    subprocess.run([str(a) for a in args], cwd=cwd, check=True)


def tools():
    lwasm = Path(os.environ.get("LWASM", TOOLS / f"lwtools-{LW_VERSION}/lwasm/lwasm"))
    os9 = Path(os.environ.get("OS9_TOOL", REF / "toolshed/build/unix/os9/os9"))
    if "LWASM" in os.environ and not lwasm.is_file():
        raise SystemExit(f"LWASM does not exist: {lwasm}")
    if "OS9_TOOL" in os.environ and not os9.is_file():
        raise SystemExit(f"OS9_TOOL does not exist: {os9}")
    if not lwasm.is_file():
        TOOLS.mkdir(parents=True, exist_ok=True)
        archive = TOOLS / f"lwtools-{LW_VERSION}.tar.gz"
        if not archive.is_file():
            run("curl", "-fL", "--connect-timeout", "20", f"https://www.lwtools.ca/releases/lwtools/lwtools-{LW_VERSION}.tar.gz", "-o", archive)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != LW_SHA256:
            raise SystemExit("LWTOOLS archive checksum mismatch")
        with tarfile.open(archive) as source:
            source.extractall(TOOLS, filter="data")
        run("make", "-j4", cwd=TOOLS / f"lwtools-{LW_VERSION}")
    if not os9.is_file():
        base = REF / "toolshed/build/unix"
        for name in ["libtoolshed", "libnative", "libcecb", "librbf", "libcoco", "libdecb", "libmisc", "libsys", "os9"]:
            run("make", "-j4", cwd=base / name)
    return lwasm.resolve(), os9.resolve()



def patch_sd_driver(disk, lwasm, flags):
    """Fix false LSN0 detection without relocating any boot-file bytes."""
    port = REF / "nitros9-mc09-build/level1/mc09/modules"
    work = OUT / "sd-driver"
    work.mkdir(exist_ok=True)
    source = (port / "mc09sdc.asm").read_text()
    old = "                    addb      ,s+                 form b+xl\n                    adcb      ,s+                 form b+xl+xh"
    new = "                    orb       ,s+                 combine XH\n                    orb       ,s+                 combine XL; zero only for LSN0"
    if source.count(old) != 1:
        raise SystemExit("Unexpected SD driver source; review LSN0 detection")
    (work / "mc09sdc.asm").write_text(source.replace(old, new))
    (work / "defsfile").write_text("Level equ 1\n use os9.d\n use scf.d\n use rbf.d\n use mc09.d\n")
    output = work / "mc09sdc.dr"
    run(lwasm, *flags, f"--output={output}", "mc09sdc.asm", cwd=work)
    original = (port / "mc09sdc.dr").read_bytes()
    fixed = output.read_bytes()
    pattern = bytes.fromhex("3410ebe0e9e026")
    start = original.find(pattern)
    if start < 0 or original.count(pattern) != 1 or len(original) != len(fixed):
        raise SystemExit("Unexpected SD driver binary layout")
    changes = {i for i in range(len(original) - 3) if original[i] != fixed[i]}
    if changes != {start + 2, start + 4} or fixed[start:start + 7] != bytes.fromhex("3410eae0eae026"):
        raise SystemExit("SD driver patch changed more than its two LSN0-test opcodes")
    # OS9Boot is contiguous. Equal-size module replacement preserves its LSN pointer.
    data = disk.read_bytes()
    if data.count(original) != 1:
        raise SystemExit("Expected exactly one SD driver in OS9Boot")
    disk.write_bytes(data.replace(original, fixed, 1))
    print("Patched mc09sd: OR-based LSN0 test; module size and boot-file layout preserved", flush=True)


def main():
    if not REF.is_dir():
        raise SystemExit("Get the reference starter kit first; see README.md")
    OUT.mkdir(parents=True, exist_ok=True)
    lwasm, os9 = tools()
    sources = REF / "nitros9-languages/ccompiler"
    defs = REF / "nitros9/defs"
    flags = ["--no-warn=ifp1", "--6809", "--format=os9", "--pragma=pcaspcr,nosymbolcase,condundefzero,undefextern,dollarnotlocal,noforwardrefmax", f"--includedir={defs}"]
    commands = {name: f"{name}_dis.asm" for name in ["cc1", "c.pass1", "c.pass2", "c.opt", "c.link", "make"]}
    commands.update({"c.prep": "c.prep.asm", "c.asm": "c_asm.asm"})
    defs_mtime = max(path.stat().st_mtime for path in defs.glob("*.d"))
    for name, source in commands.items():
        output = OUT / name
        if not output.is_file() or output.stat().st_mtime < max(defs_mtime, (sources / source).stat().st_mtime, (sources / "defsfile").stat().st_mtime):
            print(f"Assembling {name}", flush=True)
            run(lwasm, *flags, f"--output={output}", source, cwd=sources)
    run(lwasm, *flags, f"--output={OUT / 'cc'}", ROOT / "examples/cc.asm")
    commands["cc"] = "cc.asm"
    disk = OUT / "nitros9-c.dsk"
    shutil.copyfile(REF / "nitros9-runtime/NOS9_mc09_basic09.dsk", disk)

    def os9run(*args):
        run(os9, *args)

    for directory in ["DEFS", "LIB", "SOURCES"]:
        os9run("makdir", f"{disk},{directory}")
    for name in commands:
        os9run("copy", "-o=0", OUT / name, f"{disk},CMDS/{name}")
        os9run("attr", "-q", "-pe", "-npw", "-pr", "-e", "-w", "-r", f"{disk},CMDS/{name}")
    for header in sorted((sources / "defs").glob("*.h")):
        os9run("copy", "-o=0", "-l", header, f"{disk},DEFS/{header.name}")
    for library in sorted((sources / "lib").iterdir()):
        if library.suffix in (".l", ".r"):
            os9run("copy", "-o=0", library, f"{disk},LIB/{library.name}")
    for name in ["hello.c", "mandel.c"]:
        os9run("copy", "-o=0", "-l", ROOT / "examples" / name, f"{disk},SOURCES/{name}")
    os9run("copy", "-o=0", "-l", REF / "nitros9-runtime/mandel.b09", f"{disk},SOURCES/mandel.b09")
    startup = OUT / "startup"
    startup.write_text("echo * NitrOS-9 L1 mc09 / C + BASIC09 *\nsetime </term\ndate -t\necho * C: chd /dd/SOURCES then cc hello *\n")
    os9run("copy", "-r", "-o=0", "-l", startup, f"{disk},startup")
    patch_sd_driver(disk, lwasm, flags)
    sd = OUT / "multicomp09-c.img"
    with sd.open("wb") as stream:
        stream.seek(80 * 1024 * 1024)
        data = disk.read_bytes()
        for offset in range(0, len(data), 256):
            sector = data[offset:offset + 256]
            stream.write(sector + sector)
    with sd.open("rb") as stream:
        stream.seek(0x284C8 * 512)
        if stream.read(2) != b"OS":
            raise SystemExit("Invalid boot track signature")
    os9run("dir", f"{disk},CMDS")
    os9run("free", f"{disk},")
    print(f"Created {sd.relative_to(ROOT)}", flush=True)


if __name__ == "__main__":
    main()
