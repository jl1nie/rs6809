#!/usr/bin/env python3
"""Build a Multicomp NitrOS-9 Level 2 boot disk without changing the reference kit."""
import argparse
import re
import secrets
from pathlib import Path
import runpy
import subprocess

ROOT = Path(__file__).resolve().parents[1]
REF = ROOT / "reference/nitros9-basic09-starter-kit"
SOURCE = REF / "nitros9-mc09-build"
OUT = ROOT / "artifacts/level2"
WORK = OUT / "sources"
REFERENCE_COMMIT = "bf60370cfa180636cde3ee305d089d94872de82a"
PATCHED = {
    "rel.asm": "level1/coco1/modules/rel.asm",
    "mc09krn.asm": "level2/modules/kernel/mc09krn.asm",
    "fnproc.asm": "level1/modules/kernel/fnproc.asm",
    "fld.asm": "level2/modules/kernel/fld.asm",
    "falltsk.asm": "level2/modules/kernel/falltsk.asm",
    "fmove.asm": "level2/modules/kernel/fmove.asm",
    "mc09sdc.asm": "level1/mc09/modules/mc09sdc.asm",
}


def run(*args, cwd=None):
    subprocess.run([str(a) for a in args], cwd=cwd or WORK, check=True)


def main():
    global OUT, WORK
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="output directory")
    parser.add_argument("--force", action="store_true", help="replace an existing multi-user disk and its accounts")
    parser.add_argument("--multi-user", action="store_true", help="build login monitors for Telnet sessions")
    parser.add_argument("--acia-count", type=int, choices=range(1, 5), help="total ACIAs including /term")
    parser.add_argument("--user", action="append", default=[], metavar="NAME:PASSWORD", help="login account; repeat to add users")
    args = parser.parse_args()
    count = args.acia_count or (3 if args.multi_user else 1)
    if args.user and not args.multi_user:
        parser.error("--user requires --multi-user")
    users = []
    if args.multi_user:
        OUT = ROOT / "artifacts/level2-multiuser"
        WORK = OUT / "sources"
        records = args.user or [f"user{i + 1}:{secrets.token_hex(6)}" for i in range(count)]
        for record in records:
            name, separator, password = record.partition(":")
            if (not separator or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,23}", name)
                    or not password or len(password) > 32 or any(c in password for c in ",\r\n\0")
                    or not password.isascii()):
                parser.error("accounts require NAME:PASSWORD, a simple ASCII name and a 1..32 byte password without comma/newline")
            if name.lower() in [n.lower() for n, _ in users]:
                parser.error("duplicate user name")
            users.append((name, password))
        if len(users) > 64:
            parser.error("at most 64 accounts")
    if not args.multi_user and count != 1:
        parser.error("additional serial terminals require --multi-user")
    if args.output:
        OUT = args.output.resolve()
        WORK = OUT / "sources"
    if args.multi_user and (OUT / "multicomp09-level2.img").exists() and not args.force:
        parser.error("multi-user disk already exists; --force replaces its files and accounts")
    patched = dict(PATCHED)
    if args.multi_user:
        patched.update({"mc6850.asm": "level1/modules/mc6850.asm", "tsmon.asm": "level1/cmds/tsmon.asm", "sysgo.asm": "level1/modules/sysgo.asm", "login.asm": "level1/cmds/login.asm", "term_mc6850.asm": "level1/modules/term_mc6850.asm", "krnp2.asm": "level2/modules/kernel/krnp2.asm"})
    if not SOURCE.is_dir():
        raise SystemExit("Get the reference starter kit first; see README.md")
    revision = subprocess.check_output(
        ["git", "-C", str(REF), "rev-parse", "HEAD"], text=True
    ).strip()
    if revision != REFERENCE_COMMIT:
        raise SystemExit(f"Level 2 patches require starter-kit commit {REFERENCE_COMMIT}")
    # Reuse the checksum-verified local LWTOOLS and ToolShed setup.
    lwasm, os9 = runpy.run_path(str(ROOT / "scripts/build-c-disk.py"))["tools"]()
    WORK.mkdir(parents=True, exist_ok=True)
    for name, path in patched.items():
        (WORK / name).write_text((SOURCE / path).read_text())
    run("patch", "--batch", "--fuzz=0", "-p1", "-i", ROOT / "scripts/nitros9-level2.patch")
    if args.multi_user:
        (WORK / "fcpymem_compat.asm").write_text((ROOT / "scripts/os9/fcpymem_compat.asm").read_text())
        run("patch", "--batch", "--fuzz=0", "-p1", "-i", ROOT / "scripts/nitros9-telnet.patch")
    (WORK / "defsfile").write_text(
        "Level equ 2\n use os9.d\n use scf.d\n use rbf.d\n use mc09.d\n"
    )
    includes = [WORK, SOURCE / "defs", SOURCE / "level2/modules/kernel",
                SOURCE / "level1/modules/kernel", SOURCE / "level2/modules",
                SOURCE / "level1/modules", SOURCE / "level1/mc09/modules"]
    flags = ["--6809", "--format=os9", "--no-warn=ifp1",
             "--pragma=pcaspcr,nosymbolcase,condundefzero,undefextern,dollarnotlocal,noforwardrefmax",
             "-Dmc09=1", "-DNOS9VER=3", "-DNOS9MAJ=3", "-DNOS9MIN=0"]
    flags += [f"-I{p}" for p in includes]

    def build(name, path, *extra):
        source = WORK / Path(path).name
        if source.name not in patched:
            # Source-relative includes would otherwise pick Level 1's defsfile.
            source = WORK / ("source-" + name + ".asm")
            source.write_text((SOURCE / path).read_text())
        print(f"Assembling {name}", flush=True)
        run(lwasm, *flags, f"-o{OUT / name}", f"--list={OUT / name}.lst", source, *extra)

    build("rel", PATCHED["rel.asm"], "-DWidth=80")
    build("boot", "level1/coco1/modules/boot_sdc.asm", "-DCOCOSDC=1", "-DITTYP=128")
    build("krn", PATCHED["mc09krn.asm"])
    build("krnp2", "level2/modules/kernel/krnp2.asm")
    modules = [
        ("ioman", "ioman", []), ("init", "init", []), ("sysgo", "sysgo", ["-DDD=1"]),
        ("rbf", "rbf", []), ("scf", "scf", []), ("acia", "mc6850", []),
        ("term", "term_mc6850", ["-DPauses=0"]), ("sd", "mc09sdc", []),
        ("dd", "mc09sdcdesc", ["-DDNum=0", "-DSDOFFSET=0x0280", "-DDD=1",
                               "-DCyls=80", "-DSides=2", "-DSectTrk=18", "-DSectTrk0=18"]),
        ("pipeman", "pipeman", []), ("piper", "piper", []), ("pipe", "pipe", []),
        ("clock2", "clock2_soft", []),
    ]
    for name, source, extra in modules:
        candidates = [f"level2/modules/{source}.asm", f"level1/modules/{source}.asm",
                      f"level1/mc09/modules/{source}.asm"]
        path = next(p for p in candidates if (SOURCE / p).is_file())
        build(name, path, *extra)
    terminals = []
    for index in range(count - 1):
        terminal = f"t{index}"
        terminals.append(terminal)
        build(terminal, "level1/modules/term_mc6850.asm", f"-DTNum={index}",
              f"-DHwBASE={0xffd2 + index * 2}", "-DPauses=0")
    build("clock", "level2/mc09l2/modules/mc09clock.asm", "-DMainsFrq=50", "-DRTCSoft=1")
    commands = {"shell": "level1/cmds/shell_21.asm", "echo": "level1/cmds/echo.asm",
                "mfree": "level2/cmds/mfree.asm"}
    if args.multi_user:
        commands.update({"tsmon": "level1/cmds/tsmon.asm", "login": "level1/cmds/login.asm",
                         "procs": "level2/cmds/procs.asm", "sleep": "level1/cmds/sleep.asm",
                         "list": "level1/cmds/list.asm", "dir": "level1/cmds/dir.asm", "del": "level1/cmds/del.asm"})
    for name, path in commands.items():
        build(name, path)

    def tool(*args):
        run(os9, *args)

    def merge(name, parts):
        (OUT / name).write_bytes(b"".join((OUT / part).read_bytes() for part in parts))

    merge("kernel", ["rel", "boot", "krn"])
    if (OUT / "rel").stat().st_size != 0x130 or (OUT / "boot").stat().st_size != 0x1d0:
        raise SystemExit("Unexpected boot-track layout; kernel must start at $F000")
    if (OUT / "kernel").stat().st_size != 0x1200:
        raise SystemExit("Unexpected kernel size; inspect the assembly listings")
    merge("OS9Boot", ["krnp2", "ioman", "init", "rbf", "sd", "dd", "scf", "acia", "term",
                      "pipeman", "piper", "pipe", "clock", "clock2", *terminals])
    disk = OUT / "nitros9-level2.dsk"
    disk.unlink(missing_ok=True)
    tool("format", "-q", "-e", "-t80", "-ds", "-dd", disk, "-nNitrOS-9 Level 2")
    if args.multi_user:
        disk.chmod(0o600)
    tool("gen", disk, f"-b={OUT / 'OS9Boot'}", f"-t={OUT / 'kernel'}")
    tool("makdir", f"{disk},CMDS")
    for name in ["sysgo", *commands]:
        dest = f"{disk}," + (name if name == "sysgo" else "CMDS/" + name)
        tool("copy", OUT / name, dest)
        tool("attr", "-q", "-pe", "-pr", "-e", "-r", dest)
    startup = "".join(f"tsmon /{terminal} &\n" for terminal in terminals) if args.multi_user else ""
    (OUT / "startup").write_text(startup + "echo LEVEL2-STARTED\n")
    tool("copy", "-l", OUT / "startup", f"{disk},startup")
    if args.multi_user:
        tool("makdir", f"{disk},SYS")
        password_file = OUT / "password"
        password_file.touch(mode=0o600)
        password_file.chmod(0o600)
        password_file.write_text("".join(f"{name},{password},{uid},128,/dd/cmds,/dd/{name},shell\n"
                                         for uid, (name, password) in enumerate(users, 1)))
        tool("copy", "-l", password_file, f"{disk},SYS/password")
        tool("attr", "-q", "-npr", "-npw", "-npe", f"{disk},SYS/password")
        for name, _ in users:
            tool("makdir", f"{disk},{name}")
            tool("attr", "-q", "-npr", "-npw", "-npe", "-r", "-w", "-e", f"{disk},{name}")
        # ToolShed makdir creates UID 0 directories. Set the native FD owner
        # after building so each home is writable only by its own user.
        image = bytearray(disk.read_bytes())
        def descriptor(path):
            fd = int.from_bytes(image[8:11], "big") * 256
            for component in path.split("/"):
                found = None
                for segment in range(fd + 16, fd + 256, 5):
                    lsn = int.from_bytes(image[segment:segment + 3], "big")
                    sectors = int.from_bytes(image[segment + 3:segment + 5], "big")
                    if not sectors: break
                    for entry in range(lsn * 256, (lsn + sectors) * 256, 32):
                        raw = image[entry:entry + 29]
                        name = ""
                        for b in raw:
                            if not b: break
                            name += chr(b & 127)
                            if b & 128: break
                        if name.lower() == component.lower():
                            found = int.from_bytes(image[entry + 29:entry + 32], "big") * 256
                            break
                    if found is not None: break
                if found is None: raise SystemExit(f"Cannot locate home directory {path}")
                fd = found
            return fd
        for uid, (name, _) in enumerate(users, 1):
            fd = descriptor(name)
            image[fd + 1:fd + 3] = uid.to_bytes(2, "big")
        disk.write_bytes(image)
        credentials = OUT / "accounts.txt"
        credentials.touch(mode=0o600)
        credentials.chmod(0o600)
        credentials.write_text("".join(f"{name}:{password}\n" for name, password in users))
        (OUT / "serial-ports.txt").write_text(f"ACIA count: {count}\n" + "\n".join(["/term", *[f"/{t}" for t in terminals]]) + "\n")
        print(f"Login accounts saved to {credentials} (mode 0600)", flush=True)
    sd = OUT / "multicomp09-level2.img"
    if args.multi_user:
        sd.touch(mode=0o600)
        sd.chmod(0o600)
    with sd.open("wb") as stream:
        stream.seek(80 * 1024 * 1024)
        data = disk.read_bytes()
        for offset in range(0, len(data), 256):
            stream.write(data[offset:offset + 256] * 2)
    with sd.open("rb") as stream:
        stream.seek(0x284C8 * 512)
        if stream.read(2) != b"OS":
            raise SystemExit("Invalid boot track signature")
    print(f"Created {sd}", flush=True)


if __name__ == "__main__":
    main()
