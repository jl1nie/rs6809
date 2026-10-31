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
    pub len: u16,
    /// Never returns to the caller (exit, chain, ...).
    pub no_return: bool,
}

#[derive(Debug, Clone)]
pub struct Process {
    pub id: u8,
    pub parent: u8,
    /// The OS's handle (e.g. descriptor address), as returned by `current`.
    pub key: u16,
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
    pub chunks: Vec<(usize, usize, u16)>,
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
    /// Look for the OS in memory; true once it is running.
    fn detect(&mut self, m: &Machine) -> bool;
    /// Name, terms, process columns, queue names and memory-map kind for the UI.
    fn info(&self) -> Json;
    /// OS-specific fields of the state summary (e.g. level, queues).
    fn summary(&self, m: &Machine) -> Vec<(&'static str, Json)>;

    /// The service request at `pc`, if the instruction is one. Also used
    /// before the OS is detected, by the disassembler.
    fn svc_at(&self, read: &dyn Fn(u16) -> u8, pc: u16) -> Option<Svc>;
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
    fn current(&self, m: &Machine) -> u16;
    fn pid_of(&self, m: &Machine, key: u16) -> u8;
    /// Physical RAM address of the big-endian 16-bit `current` handle,
    /// when it has a fixed home (OS-9: `D.Proc`). Lets the tracer follow
    /// process switches with a memory read per instruction.
    fn current_slot(&self) -> Option<usize> {
        None
    }
    /// The process is in system state (requests are kernel-internal).
    fn in_system(&self, m: &Machine, key: u16) -> bool;
    fn describe_entry(&self, m: &Machine, paths: &PathNames, pid: u8, code: u8, r: &Regs)
    -> String;
    fn describe_exit(&self, m: &Machine, call: &Call, r: &Regs) -> String;
    fn error_of(&self, r: &Regs) -> Option<u8>;
    /// Learn from a completed user request (e.g. names of opened paths).
    fn learn(&self, call: &Call, out: &Regs, pid: u8, paths: &mut PathNames);

    fn processes(&self, m: &Machine) -> Vec<Process>;
    fn images(&self, m: &Machine) -> Vec<Image>;
    fn memory_map(&self, m: &Machine) -> Json;
    /// Read an OS address space other than the CPU's (`sys`, `pidN`).
    fn read_space(&self, m: &Machine, space: &str, addr: u16) -> Option<u8>;
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
    pub inp: Regs,
    pub out: Option<Regs>,
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
    ret_pc: u16,
    s: u16,
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
    last_proc: u16,
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
    pub fn before(&mut self, m: &Machine, os: &dyn OsMonitor, cpu: &Cpu) -> Option<u64> {
        let svc = os.svc_at(&|a| m.peek(a), cpu.pc)?;
        let code = svc.code;
        let desc = os.current(m);
        let pid = os.pid_of(m, desc);
        let system = desc != 0 && os.in_system(m, desc);
        let inp = Regs::of(cpu);
        // Kernel-internal requests are frequent; their registers are formatted on demand.
        let args = if system {
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
                ret_hint[usize::from(p.ret_pc)] -= 1;
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
                .position(|p| p.ret_pc == ret_pc && p.s == cpu.s && p.pid == pid)
            {
                let old = self.pending.remove(i);
                self.ret_hint[usize::from(old.ret_pc)] -= 1;
                if let Some(c) = self.get_mut(old.index) {
                    c.no_return = true;
                }
            }
            if self.pending.len() >= 256 {
                let old = self.pending.remove(0);
                self.ret_hint[usize::from(old.ret_pc)] -= 1;
            }
            self.ret_hint[usize::from(ret_pc)] += 1;
            self.pending.push(Pending {
                index: id,
                root,
                ret_pc,
                s: cpu.s,
                pid,
            });
        }
        Some(id)
    }

    /// The running process and its ID as of the last instruction.
    pub fn current(&self) -> (u16, u8) {
        (self.last_proc, self.last_pid)
    }

    /// Call after each instruction. Returns a completed call id and any
    /// context switch. `slot` is `os.current_slot()`.
    pub fn after(
        &mut self,
        m: &Machine,
        os: &dyn OsMonitor,
        slot: Option<usize>,
        cpu: &Cpu,
    ) -> (Option<u64>, Option<Switch>) {
        let mut switch = None;
        let desc = match slot {
            Some(a) => u16::from_be_bytes([
                m.ram.get(a).copied().unwrap_or(0),
                m.ram.get(a + 1).copied().unwrap_or(0),
            ]),
            None => os.current(m),
        };
        if desc != self.last_proc {
            let to = os.pid_of(m, desc);
            let s = Switch {
                cycle: cpu.cycles,
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
        if self.ret_hint[usize::from(cpu.pc)] == 0 {
            return (None, switch);
        }
        let pid = self.last_pid;
        let Some(i) = self
            .pending
            .iter()
            .rposition(|p| p.ret_pc == cpu.pc && p.s == cpu.s && p.pid == pid)
        else {
            return (None, switch);
        };
        let p = self.pending.remove(i);
        self.ret_hint[usize::from(p.ret_pc)] -= 1;
        let out = Regs::of(cpu);
        let Some(call) = self.calls.get(&p.index) else {
            return (None, switch);
        };
        let error = os.error_of(&out);
        let result = if call.system && error.is_none() {
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
            c.end = Some(cpu.cycles);
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
