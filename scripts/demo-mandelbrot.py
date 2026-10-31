#!/usr/bin/env python3
"""Several OS-9 Level 2 users compile and run the Mandelbrot with Microware cc1 at once over Telnet."""
import argparse
from pathlib import Path
import re
import runpy
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/level2-mandel"
TITLE = b"Mandelbrot / BASIC09 to C / 6809\r\n"
DONE = b"MANDELBROT-DONE"
PROMPT = b"OS9:"


def compile_command(name):
    # Without -f, c.link writes to the shared execution directory /dd/CMDS.
    return f"cc1 mandel.c -f=/dd/{name}/mandel"


def picture(reply):
    """Return the 25 rows between the title and the completion marker."""
    body = reply.split(TITLE, 1)[1].split(DONE, 1)[0].decode("ascii")
    rows = body.replace("\r\n", "\n").rstrip("\n").split("\n")
    if len(rows) != 25 or any(len(row) != 79 for row in rows):
        raise AssertionError(f"unexpected Mandelbrot output: {body!r}")
    return rows


def check_compiled(name, reply):
    text = reply.decode("latin-1")
    if "c.link:" not in text or re.search(r"error|ERROR #", text):
        raise AssertionError(f"{name}: cc1 failed:\n{text}")


def compile_status(buffer):
    stages = re.findall(rb"(c\.[a-z0-9]+):", buffer)
    return stages[-1].decode() if stages else "cc1"


def draw_status(buffer):
    rows = buffer.split(TITLE, 1)[1].count(b"\n") if TITLE in buffer else 0
    return f"{min(rows, 25):2d}/25 rows"


def run_together(names, sessions, command, marker, status, timeout):
    """Send command to every session, then wait for marker on all; return replies and elapsed times."""
    for name, session in zip(names, sessions):
        session.send(command(name))
    started = time.monotonic()
    finished, replies = {}, {}
    while len(finished) < len(sessions):
        if time.monotonic() - started > timeout:
            raise AssertionError(f"timed out waiting for {marker!r}")
        for name, session in zip(names, sessions):
            if name in finished:
                continue
            if not session.pump(0.01):
                raise AssertionError(f"{name}: Telnet session closed")
            if marker in session.buffer:
                finished[name] = time.monotonic() - started
                replies[name] = session.receive(marker)
        if sys.stderr.isatty():
            cells = [f"{name} " + (f"done {finished[name]:5.1f}s" if name in finished else f"{status(session.buffer):>10}")
                     for name, session in zip(names, sessions)]
            sys.stderr.write(f"\r  {time.monotonic() - started:5.1f}s  " + " | ".join(cells))
            sys.stderr.flush()
    if sys.stderr.isatty():
        sys.stderr.write("\n")
    return replies, finished


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--users", type=int, choices=range(1, 5), default=3)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/rs6809")
    parser.add_argument("--skip-build", action="store_true", help="reuse artifacts/level2-mandel as built")
    parser.add_argument("--timeout", type=float, default=300, help="seconds allowed per phase")
    args = parser.parse_args()
    if not args.skip_build:
        subprocess.run([sys.executable, str(ROOT / "scripts/build-level2-disk.py"), "--multi-user", "--force",
                        "--acia-count", str(args.users), "--output", str(OUT)],
                       cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    accounts = [line.split(":", 1) for line in (OUT / "accounts.txt").read_text().splitlines()]
    if len(accounts) < args.users:
        raise SystemExit(f"{OUT / 'accounts.txt'} has {len(accounts)} accounts; rebuild without --skip-build")
    telnet = runpy.run_path(str(ROOT / "scripts/test-telnet.py"))
    emulator = telnet["Emulator"](args.binary.resolve(), OUT / "multicomp09-level2.img", OUT)
    sessions = []
    try:
        names = [name for name, _ in accounts[:args.users]]
        for name, password in accounts[:args.users]:
            session = telnet["Session"](emulator.port)
            sessions.append(session)
            session.login(name, password)
        print(f"Logged in {', '.join(names)} on 127.0.0.1:{emulator.port}", flush=True)

        first, alone = names[0], {}
        print(f"\n[1] {first} alone: {compile_command(first)}, then /dd/{first}/mandel", flush=True)
        replies, times = run_together(names[:1], sessions[:1], compile_command, PROMPT, compile_status, args.timeout)
        check_compiled(first, replies[first])
        alone["compile"] = times[first]
        replies, times = run_together(names[:1], sessions[:1], lambda n: f"/dd/{n}/mandel", DONE, draw_status, args.timeout)
        reference = picture(replies[first])
        alone["run"] = times[first]
        sessions[0].receive(PROMPT)
        print("\n".join(reference))
        print(f"    compile {alone['compile']:.1f}s / run {alone['run']:.1f}s", flush=True)

        together = {}
        print(f"\n[2] {len(names)} users compile mandel.c in their own homes at the same time", flush=True)
        replies, times = run_together(names, sessions, compile_command, PROMPT, compile_status, args.timeout)
        for name in names:
            check_compiled(name, replies[name])
        together["compile"] = max(times.values())
        print(f"    all {len(names)} compilations succeeded in {together['compile']:.1f}s", flush=True)

        print(f"\n[3] {len(names)} users run their own /dd/<user>/mandel at the same time", flush=True)
        replies, times = run_together(names, sessions, lambda n: f"/dd/{n}/mandel", DONE, draw_status, args.timeout)
        for name, session in zip(names, sessions):
            session.receive(PROMPT)
            if picture(replies[name]) != reference:
                raise AssertionError(f"{name}: picture differs from the single-user run")
        together["run"] = max(times.values())
        print(f"    all {len(names)} pictures match the single-user run")
        for phase in ["compile", "run"]:
            print(f"    {phase:7s} alone {alone[phase]:4.1f}s / together {together[phase]:4.1f}s "
                  f"(x{together[phase] / alone[phase]:.2f} for x{len(names)} work on one shared 6809)")
        sys.stdout.flush()
    finally:
        for session in sessions:
            session.close()
        emulator.stop()


if __name__ == "__main__":
    main()
