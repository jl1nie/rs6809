#!/usr/bin/env python3
"""Build isolated optimization candidates from v0 and compare identical guest output."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/optimization/rust"


def inline_hints(cpu, machine):
    for method in ["reg", "set_reg", "index", "ea"]:
        cpu = cpu.replace(f"    fn {method}(", f"    #[inline]\n    fn {method}(")
    for signature in ["pub fn step(", "fn physical(", "fn read(", "fn write("]:
        machine = machine.replace(f"    {signature}", f"    #[inline]\n    {signature}")
    return cpu, machine


def split_bus(machine):
    marker = "impl Bus for Machine {\n"
    before, original = machine.split(marker, 1)
    original = original.rsplit("\n}", 1)[0]
    original = original.replace("    #[inline]\n    fn read(", "    #[cold]\n    #[inline(never)]\n    fn read_io(")
    original = original.replace("    #[inline]\n    fn write(", "    #[cold]\n    #[inline(never)]\n    fn write_io(")
    wrappers = '''
impl Machine {
    #[inline]
    fn is_io(&self, addr: u16) -> bool {
        addr == self.acia_base
            || addr == self.acia_base.wrapping_add(1)
            || addr == self.timer_base
            || (self.rom.is_some() && (0xffd2..=0xffdf).contains(&addr))
    }
}
impl Bus for Machine {
    #[inline]
    fn read(&mut self, addr: u16) -> u8 {
        if self.is_io(addr) {
            return self.read_io(addr);
        }
        if let Some(rom) = &self.rom
            && addr >= 0xe000 && !self.rom_disabled {
            return rom[addr as usize - 0xe000];
        }
        self.physical(addr).map_or(0xff, |p| self.ram[p])
    }
    #[inline]
    fn write(&mut self, addr: u16, v: u8) {
        if self.is_io(addr) {
            self.write_io(addr, v);
            return;
        }
        if self.rom.is_some() && addr >= 0xe000 && !self.rom_disabled {
            return;
        }
        if self.mapper & 0x20 != 0 {
            let slot = (addr as usize >> 13) | if self.mapper & 0x40 != 0 { 8 } else { 0 };
            if self.maps[slot] & 128 != 0 {
                return;
            }
        }
        if let Some(p) = self.physical(addr) {
            self.ram[p] = v;
        }
    }
}
'''
    return before + "impl Machine {\n" + original + "\n}\n" + wrappers


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    archive = ROOT / "artifacts/baselines/v0/source.tar.gz"
    baseline = json.loads((ROOT / "artifacts/baselines/v0/benchmark.json").read_text())
    cases = [("thin", True, False, False), ("inline", False, True, False),
             ("thin-inline", True, True, False), ("split-thin-inline", True, True, True)]
    summary = []
    for name, lto, hints, split in cases:
        work = OUT / name
        src = work / "source"
        src.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive) as tar:
            for member in tar.getmembers():
                if member.name.startswith("src/") or member.name in ["Cargo.toml", "Cargo.lock"]:
                    tar.extract(member, src, filter="data")
        cpu = (src / "src/cpu.rs").read_text()
        machine = (src / "src/machine.rs").read_text()
        if hints:
            cpu, machine = inline_hints(cpu, machine)
        if split:
            machine = split_bus(machine)
        (src / "src/cpu.rs").write_text(cpu)
        (src / "src/machine.rs").write_text(machine)
        env = os.environ.copy()
        env["CARGO_PROFILE_RELEASE_LTO"] = "thin" if lto else "false"
        env["CARGO_PROFILE_RELEASE_CODEGEN_UNITS"] = "1" if lto else "16"
        print(f"Building {name}", flush=True)
        subprocess.run(["cargo", "build", "--release", "--manifest-path", str(src / "Cargo.toml"),
                        "--target-dir", str(work / "target")], env=env, check=True)
        binary = work / "target/release/rs6809"
        result = work / "benchmark.json"
        subprocess.run(["python3", str(ROOT / "scripts/benchmark-emulator.py"), "--workload", "fixed", "--binary", str(binary),
                        "--runs", "3", "--output", str(result)], check=True)
        report = json.loads(result.read_text())
        for key in ["draw_cycles", "total_cycles", "stdout_sha256"]:
            if report["runs"][0][key] != baseline["runs"][0][key]:
                raise SystemExit(f"{name}: differs from v0 in {key}")
        if set(report["inputs"].values()) != set(baseline["inputs"].values()):
            raise SystemExit(f"{name}: inputs differ from v0")
        text_bytes = None
        for line in subprocess.check_output(["size", "-A", str(binary)], text=True).splitlines():
            if line.startswith(".text "):
                text_bytes = int(line.split()[1])
        row = {"name": name, "lto": lto, "inline_hints": hints, "split_io": split,
               "text_bytes": text_bytes, "summary": report["summary"],
               "source_sha256": {"cpu.rs": hashlib.sha256(cpu.encode()).hexdigest(),
                                 "machine.rs": hashlib.sha256(machine.encode()).hexdigest()}}
        summary.append(row)
        (OUT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"{name}: {row['summary']['median_draw_seconds']:.6f} s, .text={text_bytes}", flush=True)


if __name__ == "__main__":
    main()
