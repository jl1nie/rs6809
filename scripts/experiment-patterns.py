#!/usr/bin/env python3
"""Compare isolated implementation patterns against the saved round-2 baseline."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/optimization/round2"


def irq_guard(cpu, machine):
    old = "if !cpu.interrupt(self, irq, false, false) {"
    assert machine.count(old) == 1
    return cpu, machine.replace(old, "if !irq || !cpu.interrupt(self, irq, false, false) {")


def word_inline(cpu, machine, hint="inline"):
    for name in ["read16", "write16"]:
        old = f"    fn {name}("
        assert cpu.count(old) == 1
        cpu = cpu.replace(old, f"    #[{hint}]\n{old}")
    return cpu, machine


def index_match(cpu, machine):
    begin = cpu.index("    fn index(")
    end = cpu.index("    #[inline]\n    fn ea(", begin)
    part = cpu[begin:end]
    part = part.replace("let r = 1 + ((p >> 5) & 3);\n        let base = self.reg(r);", "let r = (p >> 5) & 3;\n        let base = match r { 0 => self.x, 1 => self.y, 2 => self.u, _ => self.s };")
    part = part.replace("self.set_reg(r,", "self.set_index_reg(r,")
    helper = '''    #[inline]
    fn set_index_reg(&mut self, r: u8, v: u16) {
        match r {
            0 => self.x = v,
            1 => self.y = v,
            2 => self.u = v,
            _ => { self.s = v; self.nmi_armed = true; }
        }
    }
'''
    return cpu[:begin] + part + helper + cpu[end:], machine


def cached_map(cpu, machine):
    machine = machine.replace("    maps: [u8; 16],", "    maps: [u8; 16],\n    read_bases: [usize; 8],\n    writable: [bool; 8],")
    machine = machine.replace("            maps: [0; 16],", "            maps: [0; 16],\n            read_bases: [0, 8192, 16384, 24576, 32768, 40960, 49152, 57344],\n            writable: [true; 8],")
    begin = machine.index("    #[inline]\n    fn physical(")
    end = machine.index("\n}\nimpl Machine", begin)
    replacement = '''    // Recompute mapping invariants only when the private MMU registers change.
    fn refresh_mapping(&mut self) {
        let enabled = self.mapper & 0x20 != 0;
        let task = if self.mapper & 0x40 != 0 { 8 } else { 0 };
        for slot in 0..8 {
            let entry = if enabled { self.maps[slot | task] } else { slot as u8 };
            let bank = usize::from(entry & 0x7f);
            self.read_bases[slot] = if bank < 8 { bank << 13 } else { 65536 };
            self.writable[slot] = !enabled || entry & 0x80 == 0;
        }
    }
    #[inline]
    fn physical(&self, addr: u16) -> Option<usize> {
        if self.fixed_top && addr >= 0xfe00 {
            return Some(usize::from(addr));
        }
        let translated = self.read_bases[usize::from(addr) >> 13] | (usize::from(addr) & 8191);
        // This range check also proves the following RAM access is in bounds.
        (translated < 65536).then_some(translated)
    }'''
    machine = machine[:begin] + replacement + machine[end:]
    old = "self.rom_disabled = v & 128 != 0 || (self.rom_disabled && v & 0x20 != 0);"
    assert machine.count(old) == 1
    machine = machine.replace(old, old + "\n                        self.refresh_mapping();")
    old = "self.maps[(self.mapper & 15) as usize] = v;"
    machine = machine.replace(old, old + "\n                    self.refresh_mapping();")
    old = '''        if self.mapper & 0x20 != 0 {
            let slot = (addr as usize >> 13) | if self.mapper & 0x40 != 0 { 8 } else { 0 };
            if self.maps[slot] & 128 != 0 {
                return;
            }
        }'''
    assert machine.count(old) == 1
    machine = machine.replace(old, '''        if !self.writable[usize::from(addr) >> 13] {
            return;
        }''')
    return cpu, machine



def hybrid_map(cpu, machine):
    cpu, machine = cached_map(cpu, machine)
    machine = machine.replace("if self.fixed_top && addr >= 0xfe00 {", "if self.mapper & 0x20 == 0 || (self.fixed_top && addr >= 0xfe00) {")
    machine = machine.replace("if !self.writable[usize::from(addr) >> 13] {", "if self.mapper & 0x20 != 0 && !self.writable[usize::from(addr) >> 13] {")
    return cpu, machine



def boxed_map(cpu, machine):
    cpu, machine = hybrid_map(cpu, machine)
    fields = "    read_bases: [usize; 8],\n    writable: [bool; 8],"
    assert fields in machine
    machine = machine.replace(fields, "    mapping: Box<Mapping>,", 1)
    default = "            read_bases: [0, 8192, 16384, 24576, 32768, 40960, 49152, 57344],\n            writable: [true; 8],"
    assert default in machine
    machine = machine.replace(default, "            mapping: Box::new(Mapping { read_bases: [0, 8192, 16384, 24576, 32768, 40960, 49152, 57344], writable: [true; 8] }),", 1)
    machine = machine.replace("self.read_bases", "self.mapping.read_bases").replace("self.writable", "self.mapping.writable")
    marker = "/// 64 KiB physical RAM;"
    machine = machine.replace(marker, "struct Mapping { read_bases: [usize; 8], writable: [bool; 8] }\n\n" + marker, 1)
    return cpu, machine


def flag_mask(cpu, machine):
    old = "if on { self.cc |= m } else { self.cc &= !m }"
    assert cpu.count(old) == 1
    return cpu.replace(old, "self.cc = (self.cc & !m) | (0u8.wrapping_sub(u8::from(on)) & m);"), machine


TRANSFORMS = {"irq-guard": irq_guard, "word-inline": word_inline,
              "word-always": lambda c, m: word_inline(c, m, "inline(always)"),
              "index-match": index_match, "cached-map": cached_map, "hybrid-map": hybrid_map, "boxed-map": boxed_map, "flag-mask": flag_mask}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", action="append", dest="cases", help="Pattern name, or names joined with +")
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    expected = json.loads((ROOT / "docs/rust-optimization.json").read_text())["adopted"]
    summary_path = OUT / "patterns.json"
    summary = json.loads(summary_path.read_text()) if summary_path.is_file() else {}
    for name in args.cases or list(TRANSFORMS):
        names = name.split("+")
        if any(part not in TRANSFORMS for part in names):
            parser.error(f"unknown pattern in {name}")
        work = OUT / name
        source = work / "source"
        source.mkdir(parents=True, exist_ok=True)
        with tarfile.open(OUT / "base/source.tar.gz") as archive:
            archive.extractall(source, filter="data")
        cpu = (source / "src/cpu.rs").read_text()
        machine = (source / "src/machine.rs").read_text()
        for part in names:
            cpu, machine = TRANSFORMS[part](cpu, machine)
        (source / "src/cpu.rs").write_text(cpu)
        (source / "src/machine.rs").write_text(machine)
        print(f"Building {name}", flush=True)
        subprocess.run(["cargo", "build", "--release", "--manifest-path", str(source / "Cargo.toml"),
                        "--target-dir", str(work / "target")], check=True)
        binary = work / "target/release/rs6809"
        result = work / "benchmark.json"
        subprocess.run(["python3", str(ROOT / "scripts/benchmark-emulator.py"), "--workload", "fixed", "--binary", str(binary),
                        "--runs", str(args.runs), "--output", str(result)], check=True)
        report = json.loads(result.read_text())
        for key in ["draw_cycles", "total_cycles", "stdout_sha256"]:
            if report["runs"][0][key] != expected["runs"][0][key]:
                raise SystemExit(f"{name}: mismatch in {key}")
        if set(report["inputs"].values()) != set(expected["inputs"].values()):
            raise SystemExit(f"{name}: changed input hashes")
        text = next(int(line.split()[1]) for line in subprocess.check_output(["size", "-A", str(binary)], text=True).splitlines() if line.startswith(".text "))
        summary[name] = {"patterns": names, "summary": report["summary"], "text_bytes": text,
                         "source_sha256": {"cpu.rs": hashlib.sha256(cpu.encode()).hexdigest(),
                                           "machine.rs": hashlib.sha256(machine.encode()).hexdigest()}}
        summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        print(f"{name}: {report['summary']['median_draw_seconds']:.6f} s, .text={text}", flush=True)


if __name__ == "__main__":
    main()
