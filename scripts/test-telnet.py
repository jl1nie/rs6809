#!/usr/bin/env python3
"""Exercise real OS-9 multi-user sessions and persistent writes over localhost Telnet."""
import argparse
from pathlib import Path
import re
import select
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


class Session:
    def __init__(self, port):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=5)
        self.socket.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.buffer = bytearray()
        self.state = "data"
        self.option = None

    def send(self, text):
        self.socket.sendall(text.encode("ascii") + b"\r\n")

    def receive(self, marker, timeout=8):
        end = time.monotonic() + timeout
        while marker not in self.buffer and time.monotonic() < end:
            if not select.select([self.socket], [], [], 0.1)[0]:
                continue
            data = self.socket.recv(8192)
            if not data:
                break
            for byte in data:
                if self.state == "data":
                    if byte == 255:
                        self.state = "command"
                    else:
                        self.buffer.append(byte)
                elif self.state == "command":
                    if byte in (251, 252, 253, 254):
                        self.option = byte
                        self.state = "option"
                    else:
                        if byte == 255:
                            self.buffer.append(byte)
                        self.state = "data"
                elif self.state == "option":
                    if self.option == 251:
                        self.socket.sendall(bytes([255, 253 if byte in (1, 3) else 254, byte]))
                    elif self.option == 253:
                        self.socket.sendall(bytes([255, 251 if byte == 3 else 252, byte]))
                    self.state = "data"
        if marker not in self.buffer:
            raise AssertionError(f"missing {marker!r}: {bytes(self.buffer)[-1000:]!r}")
        end = self.buffer.index(marker) + len(marker)
        result = bytes(self.buffer[:end])
        del self.buffer[:end]
        return result

    def login(self, name, password, invalid_first=False, welcomed=False):
        if not welcomed:
            self.receive(b"log in.")
        self.send("")
        self.receive(b"User name?:")
        if invalid_first:
            self.send(name)
            self.receive(b"Password:")
            self.send("incorrect-password")
            response = self.receive(b"User name?:")
            assert b"Invalid password." in response
            assert b"incorrect-password" not in response
        self.send(name)
        self.receive(b"Password:")
        self.send(password)
        response = self.receive(b"OS9:")
        assert b"Welcome!" in response
        assert password.encode() not in response, "OS-9 must suppress password echo"

    def command(self, command):
        self.send(command)
        return self.receive(b"OS9:")

    def close(self):
        self.socket.close()


class Emulator:
    def __init__(self, binary, image, directory):
        self.console = (directory / "console.log").open("wb")
        self.process = subprocess.Popen(
            [str(binary), "--rom", str(ROOT / "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin"),
             "--disk", str(image), "--telnet", "--boot"],
            cwd=ROOT, stdin=subprocess.DEVNULL, stdout=self.console, stderr=subprocess.PIPE,
        )
        ready, _, _ = select.select([self.process.stderr], [], [], 10)
        if not ready:
            self.stop()
            raise AssertionError("Telnet listener did not start")
        line = self.process.stderr.readline().decode()
        match = re.search(r"127\.0\.0\.1:(\d+)", line)
        if not match:
            self.stop()
            raise AssertionError(line)
        self.port = int(match[1])
        end = time.monotonic() + 10
        while b"LEVEL2-STARTED" not in (directory / "console.log").read_bytes():
            if self.process.poll() is not None or time.monotonic() >= end:
                self.stop()
                raise AssertionError("OS-9 boot failed")
            time.sleep(0.01)

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)
        self.console.close()
        self.process.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/rs6809")
    parser.add_argument("--acia-count", type=int, choices=range(1, 5), default=3)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rs6809-telnet-") as temporary:
        directory = Path(temporary)
        build = ["python3", str(ROOT / "scripts/build-level2-disk.py"), "--multi-user",
                 "--acia-count", str(args.acia_count), "--output", str(directory / "disk")]
        for user in ["alice", "bob", "carol", "dave"][:args.acia_count]:
            build.extend(["--user", f"{user}:{user}-test"])
        subprocess.run(build, cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
        image = directory / "disk/multicomp09-level2.img"
        emulator = Emulator(args.binary.resolve(), image, directory)
        sessions = []
        try:
            for index, user in enumerate(["alice", "bob", "carol", "dave"][:args.acia_count]):
                session = Session(emulator.port)
                sessions.append(session)
                session.login(user, user + "-test", invalid_first=index == 0)
            extra = Session(emulator.port)
            try:
                extra.receive(b"busy")
            finally:
                extra.close()
            # Send to every terminal before reading replies: each UART must
            # carry only its own command/output while all users are active.
            for index, session in enumerate(sessions):
                session.send(f"echo SESSION-{index}")
            for index, session in enumerate(sessions):
                reply = session.receive(b"OS9:")
                assert f"\r\nSESSION-{index}\r\n".encode() in reply
                for other in range(len(sessions)):
                    if other != index:
                        assert f"SESSION-{other}".encode() not in reply
            if len(sessions) > 1:
                reply = sessions[1].command("sleep 30000 &")
                assert b"&" in reply
                processes = sessions[1].command("procs")
                assert re.search(rb"\$[CEce][0-9A-Fa-f]", processes), processes
                assert b"Sleep" in processes or b"sleep" in processes, processes
                assert b"\r\nOTHER-USER-RUNS\r\n" in sessions[0].command("echo OTHER-USER-RUNS")
            assert b"ERROR" not in sessions[0].command("echo PERSISTENT-CONTENT >keep").upper()
            reply=sessions[0].command("list keep"); assert b"\r\nPERSISTENT-CONTENT " in reply, reply
            if len(sessions) > 1:
                assert b"PERSISTENT-CONTENT" not in sessions[1].command("list /dd/alice/keep")
                assert b"alice-test" not in sessions[1].command("list /dd/sys/password")
            sessions[0].close()
            # Reuse is gated by guest close/reopen, not a host-side process kill.
            end = time.monotonic() + 8
            while True:
                fresh = Session(emulator.port)
                try:
                    fresh.receive(b"log in.", timeout=1)
                    break
                except AssertionError:
                    fresh.close()
                    if time.monotonic() >= end:
                        raise
                    time.sleep(0.01)
            sessions[0] = fresh
            fresh.login("alice", "alice-test", welcomed=True)
            assert b"\r\nPERSISTENT-CONTENT " in fresh.command("list keep")
        finally:
            for session in sessions:
                session.close()
            emulator.stop()
        # Kill/restart the emulator, reopening the same SD backing file.
        emulator = Emulator(args.binary.resolve(), image, directory)
        session = Session(emulator.port)
        try:
            session.login("alice", "alice-test")
            assert b"\r\nPERSISTENT-CONTENT " in session.command("list keep")
        finally:
            session.close()
            emulator.stop()
    print(f"PASS: {args.acia_count} OS-9 sessions, authentication, isolation, capacity, reconnect and persistent files")


if __name__ == "__main__":
    main()
