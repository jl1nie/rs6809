//! The OS layer of the ICE: what a debugger front end needs from an
//! operating system, and the OS-independent service-request tracer.
//!
//! An [`OsMonitor`] knows one OS: how to find it in memory, how its service
//! requests look and return, and how to read its processes, loaded images
//! and memory map. The HTTP API, the browser UI and DAP only use this trait,
//! so another OS is added by implementing it (see `os9.rs` for NitrOS-9).
use super::json::Json;
use crate::{Cpu, Machine};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Memory as the OS layer and symbols read it: CPU (logical) addresses,
/// their physical RAM location, and RAM itself.
pub trait Mem {
    /// Read without side effects at a CPU address.
    fn peek(&self, addr: u32) -> u8;
    fn peek16(&self, addr: u32) -> u16 {
        u16::from_be_bytes([self.peek(addr), self.peek(addr.wrapping_add(1))])
    }
    fn peek32(&self, addr: u32) -> u32 {
        u32::from(self.peek16(addr)) << 16 | u32::from(self.peek16(addr.wrapping_add(2)))
    }
    /// Physical RAM index of a CPU address under the current mapping.
    fn translate(&self, addr: u32) -> Option<usize>;
    /// The CPU address that maps a physical RAM index now, if any.
    fn logical(&self, phys: usize) -> Option<u32>;
    fn ram(&self) -> &[u8];
}

impl Mem for Machine {
    fn peek(&self, addr: u32) -> u8 {
        Machine::peek(self, addr as u16)
    }
    fn translate(&self, addr: u32) -> Option<usize> {
        Machine::translate(self, addr as u16)
    }
    fn logical(&self, phys: usize) -> Option<u32> {
        // Eight 8 KiB slots under the current MMU task.
        (0..8u16).find_map(|slot| {
            let base = Machine::translate(self, slot << 13)?;
            (phys >= base && phys < base + 8192).then(|| u32::from(slot << 13) | (phys - base) as u32)
        })
    }
    fn ram(&self) -> &[u8] {
        &self.ram
    }
}

/// Registers at a service request, by CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuRegs {
    M6809(Regs),
    M68k(Regs68k),
}
impl CpuRegs {
    /// The 6809 registers (zero for another CPU).
    pub fn m6809(&self) -> Regs {
        match self {
            CpuRegs::M6809(r) => *r,
            CpuRegs::M68k(_) => Regs::default(),
        }
    }
}

/// 68000 registers at a service request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regs68k {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub sr: u16,
    pub pc: u32,
}

/// The CPU state the tracer needs at an instruction boundary.
pub struct CpuState {
    pub pc: u32,
    pub sp: u32,
    pub cycles: u64,
    pub regs: CpuRegs,
}
impl CpuState {
    pub fn of6809(c: &Cpu) -> Self {
        Self {
            pc: u32::from(c.pc),
            sp: u32::from(c.s),
            cycles: c.cycles,
            regs: CpuRegs::M6809(Regs::of(c)),
        }
    }
}

fn hint(pc: u32) -> usize {
    (pc & 0xffff) as usize
}

/// CPU registers at a service request (entry or return).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regs {
    pub a: u8,
    pub b: u8,
    pub dp: u8,
    pub cc: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
}
impl Regs {
    pub fn of(c: &Cpu) -> Self {
        Self {
            a: c.a,
            b: c.b,
            dp: c.dp,
            cc: c.cc,
            x: c.x,
            y: c.y,
            u: c.u,
            s: c.s,
            pc: c.pc,
        }
    }
    pub fn d(&self) -> u16 {
        u16::from_be_bytes([self.a, self.b])
    }
}

/// A service request at the instruction about to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svc {
    pub code: u8,
    /// Instruction length: the request returns to PC + len.
    pub len: u32,
    /// Never returns to the caller (exit, chain, ...).
    pub no_return: bool,
}

#[derive(Debug, Clone)]
pub struct Process {
    pub id: u8,
    pub parent: u8,
    /// The OS's handle (e.g. descriptor address), as returned by `current`.
    pub key: u32,
    pub name: String,
    /// `running`, `-` or a queue key from `info().queues`.
    pub queue: &'static str,
    /// Everything else, as sent to the UI (columns are described by `info`).
    pub fields: Vec<(&'static str, Json)>,
}

/// Loaded code (an OS-9 module, a program image), for symbol placement.
#[derive(Debug, Clone)]
pub struct Image {
    pub name: String,
    pub fields: Vec<(&'static str, Json)>,
    /// (physical start, length, image offset of the start)
    pub chunks: Vec<(usize, usize, u32)>,
}

#[derive(Debug, Clone)]
pub struct Service {
    pub code: u8,
    pub name: String,
    pub desc: String,
    pub explain: &'static str,
}

/// Names learned for open paths, per (process, path number).
pub type PathNames = HashMap<(u8, u8), String>;

pub trait OsMonitor: Send {
    /// Kernel symbols loaded into the ICE, for OSes found through them
    /// (FUZIX: `udata_shadow`, `ptab`). Called after each symbol load.
    fn use_symbols(&mut self, _find: &dyn Fn(&str) -> Option<u32>) {}
    /// Look for the OS in memory; true once it is running.
    fn detect(&mut self, m: &dyn Mem) -> bool;
    /// Name, terms, process columns, queue names and memory-map kind for the UI.
    fn info(&self) -> Json;
    /// OS-specific fields of the state summary (e.g. level, queues).
    fn summary(&self, m: &dyn Mem) -> Vec<(&'static str, Json)>;

    /// The service request at `pc`, if the instruction is one. Also used
    /// before the OS is detected, by the disassembler.
    fn svc_at(&self, read: &dyn Fn(u32) -> u8, pc: u32) -> Option<Svc>;
    /// Complete a request found by `svc_at` from the registers at the
    /// instruction, for OSes that pass the function in a register
    /// (CP/M-68K: D0.W). Only called when the request is about to run.
    fn svc_regs(&self, svc: Svc, _regs: &CpuRegs) -> Svc {
        svc
    }
    /// Requests internal to the OS by their code or the caller's address
    /// (CP/M-68K: BIOS calls, and BDOS calls made by the BDOS itself),
    /// counted under the user call that made them.
    fn internal(&self, _code: u8, _pc: u32) -> bool {
        false
    }
    /// Leading bytes every service request starts with (OS-9: `$10 $3F`),
    /// so the per-instruction check stays cheap; empty to always ask.
    fn svc_prefix(&self) -> &'static [u8] {
        &[]
    }
    /// Mnemonic for service requests in disassembly (OS-9: `OS9`).
    fn svc_mnemonic(&self) -> &'static str;
    fn service_name(&self, code: u8) -> String;
    fn service_code(&self, name: &str) -> Option<u8>;
    fn explain(&self, code: u8) -> &'static str;
    fn error_name(&self, code: u8) -> String;
    fn services(&self) -> Vec<Service>;
    /// Requests after which loaded images may have changed.
    fn image_calls(&self) -> &'static [u8];

    /// The running process (an OS handle; 0 for none).
    fn current(&self, m: &dyn Mem) -> u32;
    fn pid_of(&self, m: &dyn Mem, key: u32) -> u8;
    /// Physical RAM address of the big-endian 16-bit `current` handle,
    /// when it has a fixed home (OS-9: `D.Proc`). Lets the tracer follow
    /// process switches with a memory read per instruction.
    fn current_slot(&self) -> Option<usize> {
        None
    }
    /// The process is in system state (requests are kernel-internal).
    fn in_system(&self, m: &dyn Mem, key: u32) -> bool;
    fn describe_entry(&self, m: &dyn Mem, paths: &PathNames, pid: u8, code: u8, r: &CpuRegs) -> String;
    fn describe_exit(&self, m: &dyn Mem, call: &Call, r: &CpuRegs) -> String;
    fn error_of(&self, r: &CpuRegs) -> Option<u8>;
    /// The error of a completed call, when it depends on the request.
    fn error_for(&self, _call: &Call, r: &CpuRegs) -> Option<u8> {
        self.error_of(r)
    }
    /// Learn from a completed user request (e.g. names of opened paths).
    fn learn(&self, call: &Call, out: &CpuRegs, pid: u8, paths: &mut PathNames);

    fn processes(&self, m: &dyn Mem) -> Vec<Process>;
    fn images(&self, m: &dyn Mem) -> Vec<Image>;
    fn memory_map(&self, m: &dyn Mem) -> Json;
    /// Read an OS address space other than the CPU's (`sys`, `pidN`).
    fn read_space(&self, m: &dyn Mem, space: &str, addr: u32) -> Option<u8>;
}

/// One traced service request, filled in on entry and completed on return.
#[derive(Debug, Clone)]
pub struct Call {
    pub id: u64,
    pub version: u64,
    pub cycle: u64,
    pub end: Option<u64>,
    pub pid: u8,
    pub code: u8,
    pub system: bool,
    pub depth: u8,
    /// Innermost unfinished call of the same process when this one started.
    pub parent: Option<u64>,
    /// Kernel-internal requests made while this user call ran, by code.
    /// Kept even after the individual internal calls leave the log.
    pub inner: Vec<(u8, u32)>,
    pub inp: CpuRegs,
    pub out: Option<CpuRegs>,
    pub args: String,
    pub result: String,
    pub error: Option<u8>,
    /// Never returns to the caller or was abandoned.
    pub no_return: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Switch {
    pub cycle: u64,
    pub from: u8,
    pub to: u8,
}

struct Pending {
    index: u64,
    /// The outermost user-state call this request runs on behalf of.
    root: u64,
    ret_pc: u32,
    s: u32,
    pid: u8,
}

/// Records every service request and context switch.
pub struct Tracer {
    pub calls: BTreeMap<u64, Call>,
    user_ids: VecDeque<u64>,
    system_ids: VecDeque<u64>,
    pub switches: VecDeque<Switch>,
    next_id: u64,
    pub version: u64,
    pending: Vec<Pending>,
    ret_hint: Vec<u8>,
    last_proc: u32,
    /// The ID of `last_proc`, read when it became current.
    last_pid: u8,
    paths: PathNames,
    capacity: usize,
}
impl Default for Tracer {
    fn default() -> Self {
        Self {
            calls: BTreeMap::new(),
            user_ids: VecDeque::new(),
            system_ids: VecDeque::new(),
            switches: VecDeque::new(),
            next_id: 1,
            version: 0,
            pending: Vec::new(),
            ret_hint: vec![0; 65536],
            last_proc: 0,
            last_pid: 0,
            paths: HashMap::new(),
            capacity: 60000,
        }
    }
}

impl Tracer {
    /// User calls and system-state calls are kept separately so the flood
    /// of kernel-internal requests never evicts a pending user request.
    fn push(&mut self, c: Call) {
        let (ids, cap) = if c.system {
            (&mut self.system_ids, self.capacity)
        } else {
            (&mut self.user_ids, self.capacity / 4)
        };
        if ids.len() >= cap
            && let Some(old) = ids.pop_front()
        {
            self.calls.remove(&old);
        }
        ids.push_back(c.id);
        self.calls.insert(c.id, c);
    }
    fn get_mut(&mut self, id: u64) -> Option<&mut Call> {
        self.calls.get_mut(&id)
    }
    pub fn last_id(&self) -> u64 {
        self.next_id - 1
    }
    /// Call before executing the instruction at `cpu.pc`. Returns the new
    /// call id when the instruction is a service request.
    pub fn before(&mut self, m: &dyn Mem, os: &dyn OsMonitor, cpu: &CpuState) -> Option<u64> {
        let svc = os.svc_regs(os.svc_at(&|a| m.peek(a), cpu.pc)?, &cpu.regs);
        let code = svc.code;
        let desc = os.current(m);
        let pid = os.pid_of(m, desc);
        let system = (desc != 0 && os.in_system(m, desc)) || os.internal(code, cpu.pc);
        let inp = cpu.regs;
        // Kernel-internal requests are frequent; their registers are formatted on demand.
        // Requests the OS marks internal are few enough to describe (CP/M-68K's BIOS).
        let args = if system && !os.internal(code, cpu.pc) {
            String::new()
        } else {
            os.describe_entry(m, &self.paths, pid, code, &inp)
        };
        if !system {
            // A process in user state has no unfinished kernel calls: any
            // leftovers left through another path (signals, process exit).
            let ret_hint = &mut self.ret_hint;
            let calls = &mut self.calls;
            self.pending.retain(|p| {
                if p.pid != pid {
                    return true;
                }
                ret_hint[hint(p.ret_pc)] -= 1;
                if let Some(c) = calls.get_mut(&p.index) {
                    c.no_return = true;
                }
                false
            });
        }
        let depth = self
            .pending
            .iter()
            .filter(|p| p.pid == pid)
            .count()
            .min(255) as u8;
        let outer = self.pending.iter().rev().find(|p| p.pid == pid);
        let parent = outer.map(|p| p.index);
        let id = self.next_id;
        let root = outer.map_or(id, |p| p.root);
        if root != id
            && let Some(r) = self.calls.get_mut(&root)
        {
            match r.inner.iter_mut().find(|(c, _)| *c == code) {
                Some((_, n)) => *n += 1,
                None => r.inner.push((code, 1)),
            }
        }
        self.next_id += 1;
        self.version += 1;
        let no_return = svc.no_return;
        self.push(Call {
            id,
            version: self.version,
            cycle: cpu.cycles,
            end: None,
            pid,
            code,
            system,
            depth,
            parent,
            inner: Vec::new(),
            inp,
            out: None,
            args,
            result: if no_return {
                "(戻らない)".into()
            } else {
                String::new()
            },
            error: None,
            no_return,
        });
        if !no_return {
            let ret_pc = cpu.pc.wrapping_add(svc.len);
            // A process re-entering the same frame abandoned its old call.
            if let Some(i) = self
                .pending
                .iter()
                .position(|p| p.ret_pc == ret_pc && p.s == cpu.sp && p.pid == pid)
            {
                let old = self.pending.remove(i);
                self.ret_hint[hint(old.ret_pc)] -= 1;
                if let Some(c) = self.get_mut(old.index) {
                    c.no_return = true;
                }
            }
            if self.pending.len() >= 256 {
                let old = self.pending.remove(0);
                self.ret_hint[hint(old.ret_pc)] -= 1;
            }
            self.ret_hint[hint(ret_pc)] += 1;
            self.pending.push(Pending {
                index: id,
                root,
                ret_pc,
                s: cpu.sp,
                pid,
            });
        }
        Some(id)
    }

    /// The running process and its ID as of the last instruction.
    pub fn current(&self) -> (u32, u8) {
        (self.last_proc, self.last_pid)
    }

    /// Call after each instruction. Returns a completed call id and any
    /// context switch. `slot` is `os.current_slot()`.
    pub fn after(
        &mut self,
        m: &dyn Mem,
        os: &dyn OsMonitor,
        slot: Option<usize>,
        cpu: &CpuState,
    ) -> (Option<u64>, Option<Switch>) {
        self.after_with(m, os, slot, cpu.pc, cpu.sp, cpu.cycles, || cpu.regs)
    }
    /// `after` with the registers read only when a request returns.
    #[allow(clippy::too_many_arguments)]
    pub fn after_with(
        &mut self,
        m: &dyn Mem,
        os: &dyn OsMonitor,
        slot: Option<usize>,
        pc: u32,
        sp: u32,
        cycles: u64,
        regs: impl FnOnce() -> CpuRegs,
    ) -> (Option<u64>, Option<Switch>) {
        let mut switch = None;
        let desc = match slot {
            Some(a) => {
                let ram = m.ram();
                u32::from(u16::from_be_bytes([ram.get(a).copied().unwrap_or(0), ram.get(a + 1).copied().unwrap_or(0)]))
            }
            None => os.current(m),
        };
        if desc != self.last_proc {
            let to = os.pid_of(m, desc);
            let s = Switch {
                cycle: cycles,
                from: os.pid_of(m, self.last_proc),
                to,
            };
            self.last_proc = desc;
            self.last_pid = to;
            if s.from != s.to {
                if self.switches.len() >= self.capacity {
                    self.switches.pop_front();
                }
                self.switches.push_back(s);
                switch = Some(s);
            }
        }
        if self.ret_hint[hint(pc)] == 0 {
            return (None, switch);
        }
        let pid = self.last_pid;
        let Some(i) = self
            .pending
            .iter()
            .rposition(|p| p.ret_pc == pc && p.s == sp && p.pid == pid)
        else {
            return (None, switch);
        };
        let p = self.pending.remove(i);
        self.ret_hint[hint(p.ret_pc)] -= 1;
        let out = regs();
        let Some(call) = self.calls.get(&p.index) else {
            return (None, switch);
        };
        let error = os.error_for(call, &out);
        // Internal requests described on entry are described on return too.
        let result = if call.system && error.is_none() && call.args.is_empty() {
            String::new()
        } else {
            os.describe_exit(m, call, &out)
        };
        // Learn path names so later I/O calls show what a path number refers to.
        if error.is_none() && !call.system {
            let call = call.clone();
            os.learn(&call, &out, pid, &mut self.paths);
        }
        self.version += 1;
        let version = self.version;
        if let Some(c) = self.get_mut(p.index) {
            c.end = Some(cycles);
            c.out = Some(out);
            c.result = result;
            c.error = error;
            c.version = version;
        }
        (Some(p.index), switch)
    }
    pub fn call(&self, id: u64) -> Option<&Call> {
        self.calls.get(&id)
    }
}

/// No operating system: a bare program. Nothing is ever detected, so the
/// OS views stay empty and no service requests are traced.
#[derive(Default)]
pub struct NoOs;

impl OsMonitor for NoOs {
    fn detect(&mut self, _: &dyn Mem) -> bool {
        false
    }
    fn info(&self) -> Json {
        super::json::obj([("os", "none".into()), ("name", "OSなし".into()), ("title", "OSなし".into())])
    }
    fn summary(&self, _: &dyn Mem) -> Vec<(&'static str, Json)> {
        Vec::new()
    }
    fn svc_at(&self, _: &dyn Fn(u32) -> u8, _: u32) -> Option<Svc> {
        None
    }
    /// Longer than the bytes fetched per instruction, so it never matches
    /// and `svc_at` is not asked.
    fn svc_prefix(&self) -> &'static [u8] {
        &[0; 11]
    }
    fn svc_mnemonic(&self) -> &'static str {
        "SVC"
    }
    fn service_name(&self, code: u8) -> String {
        format!("${code:02X}")
    }
    fn service_code(&self, _: &str) -> Option<u8> {
        None
    }
    fn explain(&self, _: u8) -> &'static str {
        ""
    }
    fn error_name(&self, code: u8) -> String {
        format!("${code:02X}")
    }
    fn services(&self) -> Vec<Service> {
        Vec::new()
    }
    fn image_calls(&self) -> &'static [u8] {
        &[]
    }
    fn current(&self, _: &dyn Mem) -> u32 {
        0
    }
    fn pid_of(&self, _: &dyn Mem, _: u32) -> u8 {
        0
    }
    fn in_system(&self, _: &dyn Mem, _: u32) -> bool {
        false
    }
    fn describe_entry(&self, _: &dyn Mem, _: &PathNames, _: u8, _: u8, _: &CpuRegs) -> String {
        String::new()
    }
    fn describe_exit(&self, _: &dyn Mem, _: &Call, _: &CpuRegs) -> String {
        String::new()
    }
    fn error_of(&self, _: &CpuRegs) -> Option<u8> {
        None
    }
    fn learn(&self, _: &Call, _: &CpuRegs, _: u8, _: &mut PathNames) {}
    fn processes(&self, _: &dyn Mem) -> Vec<Process> {
        Vec::new()
    }
    fn images(&self, _: &dyn Mem) -> Vec<Image> {
        Vec::new()
    }
    fn memory_map(&self, _: &dyn Mem) -> Json {
        Json::Null
    }
    fn read_space(&self, _: &dyn Mem, _: &str, _: u32) -> Option<u8> {
        None
    }
}
