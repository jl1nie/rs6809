//! The CPU and machine under the ICE, behind one trait so the debugger,
//! the OS layer, symbols and DAP work for the 6809 and the 68000 alike.
//!
//! The ICE is generic over [`Target`] (static dispatch, so the per-instruction
//! path costs the same as calling the CPU directly).
use super::osmon::{CpuRegs, CpuState, Mem, Regs, Regs68k};
use super::{Watch, WatchKind};
use crate::cpu::I;
use crate::m68k::machine::Sim68k;
use crate::m68k::tiny68k::Tiny68k;
use crate::m68k::{Bus68k, M68k};
use crate::{Bus, Cpu, Machine};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Next,
    /// JSR/BSR/SWI/TRAP and OS service requests: returns to the next instruction.
    Call,
    Jump,
    /// A conditional branch (falls through when not taken).
    Branch,
    Return,
}

/// One disassembled instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub addr: u32,
    pub len: u32,
    pub text: String,
    /// Branch/call destination, or the service code of an OS request.
    pub target: Option<u32>,
    pub flow: Flow,
    /// An OS service request (decoded by the OS hook).
    pub svc: bool,
}

/// The OS's service-request decoder: (length, text, code) at an address.
pub type SvcHook<'a> = &'a dyn Fn(&dyn Fn(u32) -> u8, u32) -> Option<(u32, String, u32)>;

/// A watchpoint hit: address, write, value.
pub type WatchHit = (u32, bool, u8);

pub trait Target: Send {
    /// Register snapshot kept in the instruction trace.
    type Snap: Copy;
    /// The CPU's name for the UI and DAP ("6809", "68000").
    const CPU: &'static str;
    /// Bytes read for a trace entry (the longest instruction).
    const MAX_INSN: usize;
    /// Lengths of call instructions (to find return addresses on the stack).
    const CALL_LENS: &'static [u32];

    fn mem(&self) -> &dyn Mem;
    /// Write without side effects on devices (RAM only).
    fn poke(&mut self, addr: u32, v: u8);
    fn pc(&self) -> u32;
    fn set_pc(&mut self, v: u32);
    fn sp(&self) -> u32;
    fn cycles(&self) -> u64;
    /// Address-space context: the stack pointer and mapping in use
    /// (6809: MMU task; 68000: supervisor bit).
    fn context(&self) -> u32;
    /// Address bits (16 or 24).
    fn addr_bits(&self) -> u32;
    /// Bytes of a return address on the stack.
    fn ret_size(&self) -> u32;
    /// The next step executes an instruction (not a wait or an interrupt entry).
    fn executes(&self) -> bool;
    /// Return instructions by their first bytes: (subroutine return, any return).
    fn returns(&self, bytes: &[u8]) -> (bool, bool);

    fn cpu_state(&self) -> CpuState;
    fn snap(&self) -> Self::Snap;
    fn snap_pc(s: &Self::Snap) -> u32;
    fn snap_json(s: &Self::Snap) -> String;
    /// The registers as the browser shows them.
    fn regs_json(&self) -> String;
    /// Name, value and width in bits of each register, for DAP.
    fn reg_list(&self) -> Vec<(&'static str, u32, u8)>;
    fn reg(&self, name: &str) -> Option<u32>;
    fn set_reg(&mut self, name: &str, v: u32) -> bool;
    /// Wait state for the state summary (6809: CWAI/SYNC).
    fn wait_state(&self) -> u8;

    fn disasm(&self, read: &dyn Fn(u32) -> u8, addr: u32, svc: SvcHook) -> Line;

    /// Execute one instruction (or interrupt entry); with watchpoints, the
    /// first access in range is reported.
    fn step(&mut self, watches: &[Watch]) -> Result<Option<WatchHit>, String>;
    fn disk_error(&mut self) -> Option<io::Error>;

    /// Bytes the console (first serial port) sent.
    fn console_take(&mut self) -> Vec<u8>;
    /// The console can take another input byte.
    fn console_ready(&self) -> bool;
    fn console_put(&mut self, b: u8);
    /// The MMU state for the browser.
    fn mmu_json(&self) -> String;
    fn ram_len(&self) -> usize;
    /// The 6809 Multicomp machine, for Telnet.
    fn multicomp(&mut self) -> Option<&mut Machine> {
        None
    }
}

/// The 6809 and its (Multicomp or plain) machine.
pub struct Mc6809 {
    pub cpu: Cpu,
    pub machine: Machine,
}

struct Probe<'a> {
    m: &'a mut Machine,
    watches: &'a [Watch],
    hit: Option<WatchHit>,
}
impl Probe<'_> {
    fn check(&mut self, addr: u16, write: bool, value: u8) {
        if self.hit.is_none() {
            self.hit = watch_hit(self.watches, u32::from(addr), write, value);
        }
    }
}
impl Bus for Probe<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        let v = self.m.read(addr);
        self.check(addr, false, v);
        v
    }
    fn write(&mut self, addr: u16, v: u8) {
        self.check(addr, true, v);
        self.m.write(addr, v)
    }
}

fn watch_hit(watches: &[Watch], a: u32, write: bool, value: u8) -> Option<WatchHit> {
    watches
        .iter()
        .any(|w| {
            let kind_ok = match w.kind {
                WatchKind::Read => !write,
                WatchKind::Write => write,
                WatchKind::Access => true,
            };
            kind_ok && (w.start..=w.end).contains(&a)
        })
        .then_some((a, write, value))
}

fn line6809(l: super::disasm::Line) -> Line {
    Line {
        addr: u32::from(l.addr),
        len: u32::from(l.len),
        text: l.text,
        target: l.target.map(u32::from),
        flow: match l.flow {
            super::disasm::Flow::Next => Flow::Next,
            super::disasm::Flow::Call => Flow::Call,
            super::disasm::Flow::Jump => Flow::Jump,
            super::disasm::Flow::Return => Flow::Return,
        },
        svc: l.svc,
    }
}

impl Target for Mc6809 {
    type Snap = Regs;
    const CPU: &'static str = "6809";
    const MAX_INSN: usize = 5;
    const CALL_LENS: &'static [u32] = &[2, 3, 4];

    fn mem(&self) -> &dyn Mem {
        &self.machine
    }
    fn poke(&mut self, addr: u32, v: u8) {
        self.machine.poke(addr as u16, v)
    }
    fn pc(&self) -> u32 {
        u32::from(self.cpu.pc)
    }
    fn set_pc(&mut self, v: u32) {
        self.cpu.pc = v as u16
    }
    fn sp(&self) -> u32 {
        u32::from(self.cpu.s)
    }
    fn cycles(&self) -> u64 {
        self.cpu.cycles
    }
    fn context(&self) -> u32 {
        u32::from(self.machine.mmu().task)
    }
    fn addr_bits(&self) -> u32 {
        16
    }
    fn ret_size(&self) -> u32 {
        2
    }
    fn executes(&self) -> bool {
        let irq = self.machine.irq();
        let wait = self.cpu.waiting();
        (wait == 0 || (wait == 1 && irq)) && !(irq && self.cpu.cc & I == 0)
    }
    fn returns(&self, b: &[u8]) -> (bool, bool) {
        let rts = b[0] == 0x39 || (b[0] == 0x35 && b[1] & 0x80 != 0);
        (rts, rts || b[0] == 0x3b)
    }
    fn cpu_state(&self) -> CpuState {
        CpuState::of6809(&self.cpu)
    }
    fn snap(&self) -> Regs {
        Regs::of(&self.cpu)
    }
    fn snap_pc(s: &Regs) -> u32 {
        u32::from(s.pc)
    }
    fn snap_json(r: &Regs) -> String {
        format!(
            "{{\"a\":{},\"b\":{},\"dp\":{},\"cc\":{},\"x\":{},\"y\":{},\"u\":{},\"s\":{},\"pc\":{}}}",
            r.a, r.b, r.dp, r.cc, r.x, r.y, r.u, r.s, r.pc
        )
    }
    fn regs_json(&self) -> String {
        Self::snap_json(&Regs::of(&self.cpu))
    }
    fn reg_list(&self) -> Vec<(&'static str, u32, u8)> {
        let r = Regs::of(&self.cpu);
        vec![
            ("A", u32::from(r.a), 8),
            ("B", u32::from(r.b), 8),
            ("D", u32::from(r.d()), 16),
            ("X", u32::from(r.x), 16),
            ("Y", u32::from(r.y), 16),
            ("U", u32::from(r.u), 16),
            ("S", u32::from(r.s), 16),
            ("PC", u32::from(r.pc), 16),
            ("DP", u32::from(r.dp), 8),
            ("CC", u32::from(r.cc), 8),
        ]
    }
    fn reg(&self, name: &str) -> Option<u32> {
        self.reg_list()
            .into_iter()
            .find(|r| r.0.eq_ignore_ascii_case(name))
            .map(|r| r.1)
    }
    fn set_reg(&mut self, name: &str, v: u32) -> bool {
        let c = &mut self.cpu;
        match name.to_ascii_lowercase().as_str() {
            "a" => c.a = v as u8,
            "b" => c.b = v as u8,
            "d" => c.set_d(v as u16),
            "dp" => c.dp = v as u8,
            "cc" => c.cc = v as u8,
            "x" => c.x = v as u16,
            "y" => c.y = v as u16,
            "u" => c.u = v as u16,
            "s" => c.s = v as u16,
            "pc" => c.pc = v as u16,
            _ => return false,
        }
        true
    }
    fn wait_state(&self) -> u8 {
        self.cpu.waiting()
    }
    fn disasm(&self, read: &dyn Fn(u32) -> u8, addr: u32, svc: SvcHook) -> Line {
        let read16 = |a: u16| read(u32::from(a));
        line6809(super::disasm::disassemble_with(&read16, addr as u16, &|r, at| {
            let r32 = |a: u32| r(a as u16);
            svc(&r32, u32::from(at)).map(|(len, text, code)| (len as u16, text, code as u16))
        }))
    }
    fn step(&mut self, watches: &[Watch]) -> Result<Option<WatchHit>, String> {
        if watches.is_empty() {
            return self.machine.step(&mut self.cpu).map(|_| None).map_err(|e| e.to_string());
        }
        let irq = self.machine.irq();
        let old = self.cpu.cycles;
        let mut probe = Probe {
            m: &mut self.machine,
            watches,
            hit: None,
        };
        let r = if !self.cpu.interrupt(&mut probe, irq, false, false) {
            self.cpu.step(&mut probe).map(|_| ())
        } else {
            Ok(())
        };
        let hit = probe.hit;
        self.machine.tick(self.cpu.cycles - old);
        r.map(|_| hit).map_err(|e| e.to_string())
    }
    fn disk_error(&mut self) -> Option<io::Error> {
        self.machine.disk_error()
    }
    fn console_take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.machine.acia.tx)
    }
    fn console_ready(&self) -> bool {
        self.machine.acia.rx.is_empty()
    }
    fn console_put(&mut self, b: u8) {
        self.machine.acia.rx.push_back(b)
    }
    fn mmu_json(&self) -> String {
        let mmu = self.machine.mmu();
        let maps: Vec<String> = mmu.maps.iter().map(|m| m.to_string()).collect();
        format!(
            "{{\"enabled\":{},\"task\":{},\"romdis\":{},\"frt\":{},\"maps\":[{}]}}",
            mmu.enabled,
            mmu.task,
            mmu.rom_disabled,
            mmu.fixed_top,
            maps.join(",")
        )
    }
    fn ram_len(&self) -> usize {
        self.machine.ram.len()
    }
    fn multicomp(&mut self) -> Option<&mut Machine> {
        Some(&mut self.machine)
    }
}

/// Registers of a 68000 trace entry or service request.
pub fn regs68k_json(d: &[u32; 8], a: &[u32; 8], sr: u16, pc: u32, usp: u32, ssp: u32) -> String {
    let mut o = String::from("{");
    for (i, v) in d.iter().enumerate() {
        o.push_str(&format!("\"d{i}\":{v},"));
    }
    for (i, v) in a.iter().enumerate() {
        o.push_str(&format!("\"a{i}\":{v},"));
    }
    o.push_str(&format!("\"sr\":{sr},\"pc\":{pc},\"usp\":{usp},\"ssp\":{ssp}}}"));
    o
}

/// A 68000 machine: memory, devices and the console.
pub trait Board68k: Bus68k + Mem + Send {
    /// Write RAM only (no device side effects).
    fn poke(&mut self, addr: u32, v: u8);
    /// Advance the devices by CPU cycles.
    fn tick(&mut self, _cycles: u64) {}
    /// Interrupt request: (level 0–7, vector or autovector).
    fn irq(&self) -> (u8, Option<u8>) {
        (0, None)
    }
    fn console_take(&mut self) -> Vec<u8>;
    fn console_ready(&self) -> bool;
    fn console_put(&mut self, b: u8);
    fn disk_error(&mut self) -> Option<io::Error> {
        None
    }
}

impl Board68k for Sim68k {
    fn poke(&mut self, addr: u32, v: u8) {
        Sim68k::poke(self, addr, v)
    }
    fn console_take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }
    fn console_ready(&self) -> bool {
        self.rx.is_empty()
    }
    fn console_put(&mut self, b: u8) {
        self.rx.push_back(b)
    }
}

impl Mem for Tiny68k {
    fn peek(&self, addr: u32) -> u8 {
        Tiny68k::peek(self, addr)
    }
    fn translate(&self, addr: u32) -> Option<usize> {
        let a = (addr & 0xff_ffff) as usize;
        (a < self.ram.len()).then_some(a)
    }
    fn logical(&self, phys: usize) -> Option<u32> {
        (phys < self.ram.len()).then_some(phys as u32)
    }
    fn ram(&self) -> &[u8] {
        &self.ram
    }
}

impl Board68k for Tiny68k {
    fn poke(&mut self, addr: u32, v: u8) {
        Tiny68k::poke(self, addr, v)
    }
    fn tick(&mut self, cycles: u64) {
        Tiny68k::tick(self, cycles)
    }
    fn irq(&self) -> (u8, Option<u8>) {
        Tiny68k::irq(self)
    }
    fn console_take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.duart.a.tx)
    }
    fn console_ready(&self) -> bool {
        self.duart.a.rx.is_empty()
    }
    fn console_put(&mut self, b: u8) {
        self.duart.a.rx.push_back(b)
    }
    fn disk_error(&mut self) -> Option<io::Error> {
        self.ide.error.take()
    }
}

/// The 68000 and its machine (by default the plain RAM + console one).
pub struct Mc68000<B: Board68k = Sim68k> {
    pub cpu: M68k,
    pub machine: B,
}

impl<B: Board68k> Mc68000<B> {
    /// Reset from the vectors at address 0.
    pub fn new(mut machine: B) -> Self {
        let mut cpu = M68k::default();
        cpu.reset(&mut machine);
        Self { cpu, machine }
    }
    /// Start at `pc` in supervisor state with the stack at `ssp`, without
    /// reading the reset vectors (a program loaded without a ROM).
    pub fn start(machine: B, pc: u32, ssp: u32) -> Self {
        let mut cpu = M68k::default();
        cpu.sr = crate::m68k::S | 0x0700;
        cpu.a[7] = ssp;
        cpu.pc = pc;
        Self { cpu, machine }
    }
    /// One instruction or interrupt entry on the machine, with its devices.
    pub fn run_one(&mut self) {
        let (level, vector) = self.machine.irq();
        self.cpu.irq = level;
        self.cpu.irq_vector = vector;
        let old = self.cpu.cycles;
        self.cpu.step(&mut self.machine);
        self.machine.tick(self.cpu.cycles - old);
    }
}

impl Mem for Sim68k {
    fn peek(&self, addr: u32) -> u8 {
        Sim68k::peek(self, addr)
    }
    fn translate(&self, addr: u32) -> Option<usize> {
        let a = (addr & 0xff_ffff) as usize;
        (a < self.ram.len()).then_some(a)
    }
    fn logical(&self, phys: usize) -> Option<u32> {
        (phys < self.ram.len()).then_some(phys as u32)
    }
    fn ram(&self) -> &[u8] {
        &self.ram
    }
}

struct Probe68<'a, B: Board68k> {
    m: &'a mut B,
    watches: &'a [Watch],
    hit: Option<WatchHit>,
}
impl<B: Board68k> Bus68k for Probe68<'_, B> {
    fn read8(&mut self, a: u32) -> u8 {
        let v = self.m.read8(a);
        if self.hit.is_none() {
            self.hit = watch_hit(self.watches, a & 0xff_ffff, false, v);
        }
        v
    }
    fn write8(&mut self, a: u32, v: u8) {
        if self.hit.is_none() {
            self.hit = watch_hit(self.watches, a & 0xff_ffff, true, v);
        }
        self.m.write8(a, v)
    }
    fn read16(&mut self, a: u32) -> u16 {
        let v = self.m.read16(a);
        if self.hit.is_none() {
            let [h, l] = v.to_be_bytes();
            self.hit = watch_hit(self.watches, a & 0xff_ffff, false, h)
                .or_else(|| watch_hit(self.watches, (a + 1) & 0xff_ffff, false, l));
        }
        v
    }
    fn write16(&mut self, a: u32, v: u16) {
        if self.hit.is_none() {
            let [h, l] = v.to_be_bytes();
            self.hit = watch_hit(self.watches, a & 0xff_ffff, true, h)
                .or_else(|| watch_hit(self.watches, (a + 1) & 0xff_ffff, true, l));
        }
        self.m.write16(a, v)
    }
}

const D_NAMES: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
const A_NAMES: [&str; 8] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7"];

impl<B: Board68k> Target for Mc68000<B> {
    type Snap = Regs68k;
    const CPU: &'static str = "68000";
    const MAX_INSN: usize = 10;
    const CALL_LENS: &'static [u32] = &[2, 4, 6];

    fn mem(&self) -> &dyn Mem {
        &self.machine
    }
    fn poke(&mut self, addr: u32, v: u8) {
        Board68k::poke(&mut self.machine, addr, v)
    }
    fn pc(&self) -> u32 {
        self.cpu.pc & 0xff_ffff
    }
    fn set_pc(&mut self, v: u32) {
        self.cpu.pc = v & 0xff_ffff
    }
    fn sp(&self) -> u32 {
        self.cpu.a[7]
    }
    fn cycles(&self) -> u64 {
        self.cpu.cycles
    }
    fn context(&self) -> u32 {
        u32::from(self.cpu.supervisor())
    }
    fn addr_bits(&self) -> u32 {
        24
    }
    fn ret_size(&self) -> u32 {
        4
    }
    fn executes(&self) -> bool {
        let c = &self.cpu;
        let level = c.irq & 7;
        let irq = level > 0 && (level == 7 || level > ((c.sr >> 8) & 7) as u8);
        !c.halted && !c.stopped && !irq
    }
    fn returns(&self, b: &[u8]) -> (bool, bool) {
        let op = u16::from_be_bytes([b[0], b[1]]);
        let rts = op == 0x4e75 || op == 0x4e77;
        (rts, rts || op == 0x4e73)
    }
    fn cpu_state(&self) -> CpuState {
        CpuState {
            pc: self.pc(),
            sp: self.sp(),
            cycles: self.cpu.cycles,
            regs: CpuRegs::M68k(self.snap()),
        }
    }
    fn snap(&self) -> Regs68k {
        Regs68k {
            d: self.cpu.d,
            a: self.cpu.a,
            sr: self.cpu.sr,
            pc: self.pc(),
        }
    }
    fn snap_pc(s: &Regs68k) -> u32 {
        s.pc
    }
    fn snap_json(r: &Regs68k) -> String {
        regs68k_json(&r.d, &r.a, r.sr, r.pc, r.a[7], r.a[7])
    }
    fn regs_json(&self) -> String {
        let c = &self.cpu;
        regs68k_json(&c.d, &c.a, c.sr, self.pc(), c.usp(), c.ssp())
    }
    fn reg_list(&self) -> Vec<(&'static str, u32, u8)> {
        let c = &self.cpu;
        let mut v: Vec<(&'static str, u32, u8)> = Vec::with_capacity(20);
        v.extend(D_NAMES.iter().zip(c.d).map(|(n, x)| (*n, x, 32)));
        v.extend(A_NAMES.iter().zip(c.a).map(|(n, x)| (*n, x, 32)));
        v.push(("PC", self.pc(), 32));
        v.push(("SR", u32::from(c.sr), 16));
        v.push(("USP", c.usp(), 32));
        v.push(("SSP", c.ssp(), 32));
        v
    }
    fn reg(&self, name: &str) -> Option<u32> {
        if name.eq_ignore_ascii_case("sp") {
            return Some(self.sp());
        }
        self.reg_list()
            .into_iter()
            .find(|r| r.0.eq_ignore_ascii_case(name))
            .map(|r| r.1)
    }
    fn set_reg(&mut self, name: &str, v: u32) -> bool {
        let n = name.to_ascii_uppercase();
        let c = &mut self.cpu;
        let idx = |p: char| {
            n.strip_prefix(p)
                .and_then(|k| k.parse::<usize>().ok())
                .filter(|&k| k < 8 && n.len() == 2)
        };
        if let Some(k) = idx('D') {
            c.d[k] = v;
        } else if let Some(k) = idx('A') {
            c.a[k] = v;
        } else {
            match n.as_str() {
                "SP" => c.a[7] = v,
                "PC" => c.pc = v & 0xff_ffff,
                "SR" => c.set_sr(v as u16),
                "CCR" => c.set_sr((c.sr & 0xff00) | (v as u16 & 0x1f)),
                "USP" => c.set_usp(v),
                "SSP" => c.set_ssp(v),
                _ => return false,
            }
        }
        true
    }
    fn wait_state(&self) -> u8 {
        u8::from(self.cpu.stopped)
    }
    fn disasm(&self, read: &dyn Fn(u32) -> u8, addr: u32, svc: SvcHook) -> Line {
        if let Some((len, text, code)) = svc(read, addr) {
            return Line {
                addr,
                len,
                text,
                target: Some(code),
                flow: Flow::Call,
                svc: true,
            };
        }
        let l = crate::m68k::disasm::disassemble(read, addr);
        use crate::m68k::disasm::Flow as F;
        Line {
            addr: l.addr,
            len: l.len,
            text: l.text,
            target: l.target,
            flow: match l.flow {
                F::Next => Flow::Next,
                F::Call => Flow::Call,
                F::Jump => Flow::Jump,
                F::Branch => Flow::Branch,
                F::Return => Flow::Return,
            },
            svc: false,
        }
    }
    fn step(&mut self, watches: &[Watch]) -> Result<Option<WatchHit>, String> {
        if self.cpu.halted {
            return Err("二重フォールトでCPUが停止した".into());
        }
        let hit = if watches.is_empty() {
            self.run_one();
            None
        } else {
            let (level, vector) = self.machine.irq();
            self.cpu.irq = level;
            self.cpu.irq_vector = vector;
            let old = self.cpu.cycles;
            let mut probe = Probe68 {
                m: &mut self.machine,
                watches,
                hit: None,
            };
            self.cpu.step(&mut probe);
            let hit = probe.hit;
            self.machine.tick(self.cpu.cycles - old);
            hit
        };
        if self.cpu.halted {
            return Err("二重フォールトでCPUが停止した".into());
        }
        Ok(hit)
    }
    fn disk_error(&mut self) -> Option<io::Error> {
        self.machine.disk_error()
    }
    fn console_take(&mut self) -> Vec<u8> {
        self.machine.console_take()
    }
    fn console_ready(&self) -> bool {
        self.machine.console_ready()
    }
    fn console_put(&mut self, b: u8) {
        self.machine.console_put(b)
    }
    fn mmu_json(&self) -> String {
        "null".into()
    }
    fn ram_len(&self) -> usize {
        self.machine.ram().len()
    }
}
