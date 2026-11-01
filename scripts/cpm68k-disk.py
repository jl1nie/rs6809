#!/usr/bin/env python3
"""Make and read CP/M-68K disk images for the rs6809 Tiny68K BIOS.

The format is the one in examples/cpm68k/bios.s: 8 MiB of 512-byte IDE
sectors; 2048 tracks of 32 128-byte records; track 0 reserved; 4 KiB blocks
(2047 of them), 512 directory entries in blocks 0-3; block numbers are 16
bits, little-endian (as on 8080 CP/M, although the BDOS runs on a
big-endian CPU).

    cpm68k-disk.py create IMAGE [FILE[=NAME]]...   new disk with these files
    cpm68k-disk.py add IMAGE FILE[=NAME]...        add (or replace) files
    cpm68k-disk.py ls IMAGE                        list the directory
    cpm68k-disk.py get IMAGE NAME [OUT]            extract a file (- = stdout)
    cpm68k-disk.py dist IMAGE                      the CP/M-68K 1.3 tools and examples

NAME is 8.3 (upper-cased), optionally with a user number: `3:FOO.C`.
Text files get the ^Z padding CP/M expects when their extension is in TEXT.
`dist` stores the distribution's .REL commands as .68K: BDOS Program Load
relocates them, so RELOC is not needed.
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "artifacts/cpm68k/dist"
SIZE = 8 << 20
RECORD = 128
TRACK = 32 * RECORD
OFF = 1  # reserved tracks
BLOCK = 4096
DSM = 2046
DRM = 511
DIR_BLOCKS = 4
EXM = 1  # two logical extents per directory entry
TEXT = {"C", "H", "SUB", "TXT", "ASM", "S", "DOC", "LST"}


def block_at(b):
    return OFF * TRACK + b * BLOCK


class Disk:
    def __init__(self, data):
        self.data = bytearray(data)
        if len(self.data) != SIZE:
            sys.exit(f"image must be {SIZE} bytes")

    @classmethod
    def blank(cls):
        d = bytearray(SIZE)
        start = block_at(0)
        d[start : start + DIR_BLOCKS * BLOCK] = b"\xe5" * (DIR_BLOCKS * BLOCK)
        return cls(d)

    def entries(self):
        base = block_at(0)
        for i in range(DRM + 1):
            yield i, self.data[base + 32 * i : base + 32 * i + 32]

    def put_entry(self, i, e):
        base = block_at(0) + 32 * i
        self.data[base : base + 32] = e

    def used_blocks(self):
        used = set(range(DIR_BLOCKS))
        for _, e in self.entries():
            if e[0] < 16:
                for k in range(8):
                    b = int.from_bytes(e[16 + 2 * k : 18 + 2 * k], "little")
                    if b:
                        used.add(b)
        return used

    def files(self):
        out = {}
        for _, e in self.entries():
            if e[0] >= 16:
                continue
            name = (e[0], bytes(e[1:9]).decode("latin1").rstrip(), bytes(b & 0x7F for b in e[9:12]).decode("latin1").rstrip())
            ext = (e[12] & 0x1F) | (e[14] << 5)
            entry_no = ext // (EXM + 1)
            blocks = [int.from_bytes(e[16 + 2 * k : 18 + 2 * k], "little") for k in range(8)]
            records = (ext % (EXM + 1)) * 128 + e[15]
            out.setdefault(name, {})[entry_no] = (blocks, records)
        return out

    def read(self, key):
        parts = self.files()[key]
        data = bytearray()
        for n in sorted(parts):
            blocks, records = parts[n]
            chunk = b"".join(self.data[block_at(b) : block_at(b) + BLOCK] for b in blocks if b)
            data += chunk[: records * RECORD]
        return bytes(data)

    def delete(self, key):
        for i, e in self.entries():
            if e[0] == key[0] and parse_entry_name(e) == key[1:]:
                e = bytearray(e)
                e[0] = 0xE5
                self.put_entry(i, e)

    def write(self, user, name, ext, data):
        self.delete((user, name, ext))
        if len(data) % RECORD:
            data += b"\x1a" * (RECORD - len(data) % RECORD)
        records = len(data) // RECORD
        free = sorted(set(range(DSM + 1)) - self.used_blocks())
        need = -(-len(data) // BLOCK)
        if need > len(free):
            sys.exit(f"disk full ({name}.{ext})")
        blocks = free[:need]
        for k, b in enumerate(blocks):
            chunk = data[k * BLOCK : (k + 1) * BLOCK]
            self.data[block_at(b) : block_at(b) + len(chunk)] = chunk
        slots = [i for i, e in self.entries() if e[0] == 0xE5]
        per_entry = 8 * BLOCK // RECORD  # records per directory entry
        n_entries = max(1, -(-records // per_entry))
        if n_entries > len(slots):
            sys.exit("directory full")
        for j in range(n_entries):
            r = min(per_entry, records - j * per_entry)
            logical = j * (EXM + 1) + (max(r, 1) - 1) // 128
            rc = r - ((max(r, 1) - 1) // 128) * 128
            e = bytearray(32)
            e[0] = user
            e[1:9] = name.ljust(8).encode("latin1")
            e[9:12] = ext.ljust(3).encode("latin1")
            e[12] = logical & 0x1F
            e[14] = logical >> 5
            e[15] = rc
            for k, b in enumerate(blocks[j * 8 : j * 8 + 8]):
                e[16 + 2 * k : 18 + 2 * k] = b.to_bytes(2, "little")
            self.put_entry(slots[j], e)


def parse_entry_name(e):
    return (bytes(e[1:9]).decode("latin1").rstrip(), bytes(b & 0x7F for b in e[9:12]).decode("latin1").rstrip())


def parse_name(s):
    user = 0
    if ":" in s:
        u, s = s.split(":", 1)
        user = int(u)
    stem, _, ext = s.upper().partition(".")
    if not stem or len(stem) > 8 or len(ext) > 3:
        sys.exit(f"not an 8.3 name: {s}")
    return user, stem, ext


def add_files(disk, specs):
    for spec in specs:
        path, _, name = spec.partition("=")
        user, stem, ext = parse_name(name or Path(path).name)
        data = Path(path).read_bytes()
        if ext in TEXT:
            data = data.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")
        disk.write(user, stem, ext, data)


def dist_files():
    """(path, name) of the 1.3 distribution's tools and headers."""
    if not DIST.exists():
        sys.exit("run scripts/fetch-cpm68k.py first")
    out = []
    seen = set()
    for d in sorted(DIST.iterdir()):
        for f in sorted(d.iterdir()):
            stem, _, ext = f.name.upper().partition(".")
            if ext == "REL":
                ext = "68K"
            name = f"{stem}.{ext}" if ext else stem
            # The VME/10 and EXORmacs system files are not needed on the disk.
            if name in seen or ext in ("SYS", "SR", "MAP") or stem in ("CPM", "XCPM", "CPMLIB", "LDRLIB"):
                continue
            seen.add(name)
            out.append((f, name))
    return out


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    cmd, image = sys.argv[1], Path(sys.argv[2])
    rest = sys.argv[3:]
    if cmd == "create":
        disk = Disk.blank()
        add_files(disk, rest)
        image.write_bytes(disk.data)
    elif cmd == "dist":
        disk = Disk.blank()
        for f, name in dist_files():
            # As is: the distribution's text files already have CR LF and ^Z.
            stem, _, ext = name.partition(".")
            disk.write(0, stem, ext, f.read_bytes())
        add_files(disk, rest)
        image.write_bytes(disk.data)
    elif cmd == "add":
        disk = Disk(image.read_bytes())
        add_files(disk, rest)
        image.write_bytes(disk.data)
    elif cmd == "ls":
        disk = Disk(image.read_bytes())
        for (user, name, ext), parts in sorted(disk.files().items()):
            size = sum(r for _, r in parts.values()) * RECORD
            print(f"{user:2}: {name:8}.{ext:3} {size:8}")
    elif cmd == "get":
        disk = Disk(image.read_bytes())
        data = disk.read(parse_name(rest[0]))
        if len(rest) > 1 and rest[1] != "-":
            Path(rest[1]).write_bytes(data)
        else:
            sys.stdout.buffer.write(data)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    sys.exit(main())
