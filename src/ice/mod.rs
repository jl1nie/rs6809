//! In-circuit-emulator style debugger with a browser UI and NitrOS-9 awareness.
//!
//! The emulator thread owns the CPU and machine. The HTTP threads forward API
//! requests over a channel, so inspection always sees a consistent state.
pub mod cdecl;
pub mod dap;
pub mod debuginfo;
pub mod disasm;
pub mod frames;
pub mod http;
pub mod json;
pub mod microware;
pub mod os9;
pub mod osmon;
pub mod symbols;
mod tables;

use crate::cpu::I;
use crate::telnet::TelnetServer;
use crate::{Bus, Cpu, Machine};
use disasm::{Flow, disassemble};
use osmon::{OsMonitor, Regs, Tracer};
use std::collections::{BTreeSet, VecDeque};
use symbols::{Symbols, Target};
use std::fmt::Write as _;
use std::io;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const UI: &str = include_str!("ui.html");


pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix('$')) {
        u32::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}
fn parse_addr(s: &str) -> Option<u16> {
    let s = s.trim();
    // Addresses default to hexadecimal, as on a monitor.
    let h = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix('$'))
        .unwrap_or(s);
    u16::from_str_radix(h, 16).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    Read,
    Write,
    Access,
}

#[derive(Debug, Clone, Copy)]
pub struct Watch {
    pub start: u16,
    pub end: u16,
    pub kind: WatchKind,
}

/// Messages for the emulator thread.
pub enum Inbox {
    Http(http::Request),
    /// A Debug Adapter Protocol message from session `id`.
    Dap { id: u32, msg: json::Json, out: Sender<String> },
    DapClosed(u32),
}

/// What a symbolic breakpoint refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BpSpec {
    Line { file: String, line: u32 },
    Symbol(String),
    Addr(u16),
}
/// A breakpoint on a source line, symbol or address, re-resolved whenever
/// the OS-9 module directory changes (modules load at varying addresses).
#[derive(Debug, Clone)]
pub struct SymBp {
    pub id: u32,
    /// 0 for the browser UI, otherwise the DAP session.
    pub owner: u32,
    pub spec: BpSpec,
    /// The line actually used (the next line with code).
    pub line: Option<u32>,
    pub targets: Vec<Target>,
}
impl SymBp {
    pub fn text(&self) -> String {
        match &self.spec {
            BpSpec::Line { file, line } => format!("{file}:{}", self.line.unwrap_or(*line)),
            BpSpec::Symbol(s) => s.clone(),
            BpSpec::Addr(a) => format!("${a:04X}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Pause,
    Step,
    Breakpoint(u16),
    Watch { addr: u16, write: bool, value: u8 },
    SysCall { code: u8, pid: u8 },
    SysReturn(u64),
    Switch { from: u8, to: u8 },
    Error(String),
}
impl Stop {
    fn describe(&self, t: &Tracer, os: &dyn OsMonitor) -> String {
        let call = |id: &u64| {
            t.call(*id)
                .map(|c| format!("{} (プロセス{})", os.service_name(c.code), c.pid))
                .unwrap_or_default()
        };
        match self {
            Stop::Pause => "一時停止".into(),
            Stop::Step => "ステップ完了".into(),
            Stop::Breakpoint(a) => format!("ブレークポイント ${a:04X}"),
            Stop::Watch { addr, write, value } => format!(
                "ウォッチ ${addr:04X} {} ${value:02X}",
                if *write {
                    "書き込み"
                } else {
                    "読み出し"
                }
            ),
            Stop::SysCall { code, pid } => {
                format!(
                    "システムコール呼び出し {} (プロセス{pid})",
                    os.service_name(*code)
                )
            }
            Stop::SysReturn(id) => {
                let err = t.call(*id).and_then(|c| c.error).map(|e| os.error_name(e));
                format!(
                    "システムコール復帰 {}{}",
                    call(id),
                    err.map_or(String::new(), |e| format!(" → {e}"))
                )
            }
            Stop::Switch { from, to } => format!("プロセス切り替え {from} → {to}"),
            Stop::Error(e) => format!("エラー: {e}"),
        }
    }
}

/// Temporary stop condition for step over / step out / run to.
#[derive(Debug, Clone, Copy)]
enum Goal {
    None,
    /// Stop at `pc` once S has unwound to at least `s` in the same process.
    At {
        pc: u16,
        s: u16,
        proc_: Option<u16>,
    },
    /// Stop after a return instruction that leaves S above `s`.
    Out {
        s: u16,
        proc_: Option<u16>,
    },
    /// Source-level step: stop on another source line. Without `into`,
    /// deeper frames (S below `s`) are skipped.
    Line {
        table: usize,
        src: usize,
        line: u32,
        s: u16,
        proc_: Option<u16>,
        /// Hardware task: Level 2 runs interrupts and service requests in
        /// the system task on another stack, which must not end the step.
        task: u8,
        into: bool,
        /// An RTS/PULS PC left the starting frame.
        left: bool,
    },
}

#[derive(Clone, Copy)]
pub struct TraceEntry {
    pub cycle: u64,
    pub regs: Regs,
    pub bytes: [u8; 5],
    pub pid: u8,
}

struct Probe<'a> {
    m: &'a mut Machine,
    watches: &'a [Watch],
    hit: Option<(u16, bool, u8)>,
}
impl Probe<'_> {
    fn check(&mut self, addr: u16, write: bool, value: u8) {
        if self.hit.is_some() {
            return;
        }
        for w in self.watches {
            let kind_ok = match w.kind {
                WatchKind::Read => !write,
                WatchKind::Write => write,
                WatchKind::Access => true,
            };
            if kind_ok && (w.start..=w.end).contains(&addr) {
                self.hit = Some((addr, write, value));
                return;
            }
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

pub struct Options {
    pub actions: VecDeque<(String, String)>,
    pub echo_console: bool,
}

pub struct Ice {
    pub cpu: Cpu,
    pub machine: Machine,
    pub breakpoints: BTreeSet<u16>,
    bp_bits: Vec<u64>,
    pub watches: Vec<Watch>,
    pub break_calls: BTreeSet<u8>,
    pub break_returns: BTreeSet<u8>,
    pub break_errors: bool,
    pub break_switch: bool,
    pub break_pid: Option<u8>,
    /// Record OS-9 service requests and context switches.
    pub trace_os9: bool,
    pub tracer: Tracer,
    pub trace: VecDeque<TraceEntry>,
    pub console: Vec<u8>,
    console_base: u64,
    pub running: bool,
    pub stop: Option<Stop>,
    goal: Goal,
    skip_checks: bool,
    /// The OS layer; its structures are read once `os_ready`.
    os: Box<dyn OsMonitor>,
    os_ready: bool,
    /// `os.svc_prefix()`, cached for the per-instruction check.
    svc_prefix: &'static [u8],
    /// `os.current_slot()` once the OS is ready.
    os_slot: Option<usize>,
    detect_at: u64,
    /// Guest cycles per host second; 0 runs at full speed.
    pub speed: u64,
    actions: VecDeque<(String, String)>,
    seen: String,
    pending_input: VecDeque<u8>,
    next_input: u64,
    echo_console: bool,
    telnet: Option<TelnetServer>,
    pub symbols: Symbols,
    pub sym_bps: Vec<SymBp>,
    next_bp_id: u32,
    /// One bit per RAM byte for breakpoints in placed modules.
    phys_bits: Vec<u64>,
    phys_count: usize,
    /// Bumped whenever the effective breakpoints change.
    pub bp_version: u64,
    dap: dap::Hub,
    /// DAP variable tree nodes, valid while stopped.
    var_refs: Vec<frames::VarRef>,
}

impl Ice {
    /// An ICE for NitrOS-9 (the OS of the Multicomp images).
    pub fn new(cpu: Cpu, machine: Machine, options: Options) -> Self {
        Self::with_os(cpu, machine, options, Box::new(os9::Nitros9::default()))
    }
    pub fn with_os(cpu: Cpu, machine: Machine, options: Options, os: Box<dyn OsMonitor>) -> Self {
        Self {
            svc_prefix: os.svc_prefix(),
            os_slot: None,
            cpu,
            machine,
            breakpoints: BTreeSet::new(),
            bp_bits: vec![0; 1024],
            watches: Vec::new(),
            break_calls: BTreeSet::new(),
            break_returns: BTreeSet::new(),
            break_errors: false,
            break_switch: false,
            break_pid: None,
            trace_os9: true,
            tracer: Tracer::default(),
            trace: VecDeque::new(),
            console: Vec::new(),
            console_base: 0,
            running: false,
            stop: Some(Stop::Pause),
            goal: Goal::None,
            skip_checks: false,
            os,
            os_ready: false,
            detect_at: 0,
            speed: 0,
            actions: options.actions,
            seen: String::new(),
            pending_input: VecDeque::new(),
            next_input: 0,
            echo_console: options.echo_console,
            telnet: None,
            symbols: Symbols::default(),
            sym_bps: Vec::new(),
            next_bp_id: 1,
            phys_bits: Vec::new(),
            phys_count: 0,
            bp_version: 0,
            dap: dap::Hub::default(),
            var_refs: Vec::new(),
        }
    }
    pub fn set_telnet(&mut self, server: TelnetServer) {
        self.telnet = Some(server);
    }
    fn set_bp(&mut self, a: u16, on: bool) {
        if on {
            self.breakpoints.insert(a);
        } else {
            self.breakpoints.remove(&a);
        }
        self.rebuild_bps();
    }
    fn bp_at(&self, a: u16) -> bool {
        self.bp_bits[usize::from(a >> 6)] & (1 << (a & 63)) != 0
    }
    fn phys_bp_at(&self, pc: u16) -> bool {
        self.phys_count > 0
            && self
                .machine
                .translate(pc)
                .is_some_and(|p| self.phys_bits.get(p >> 6).is_some_and(|w| w & (1 << (p & 63)) != 0))
    }
    /// Recompute the bit tables from user and symbolic breakpoints.
    fn rebuild_bps(&mut self) {
        self.bp_bits.iter_mut().for_each(|w| *w = 0);
        self.phys_bits = vec![0; self.machine.ram.len().div_ceil(64)];
        self.phys_count = 0;
        let logical = self.breakpoints.iter().copied().chain(
            self.sym_bps
                .iter()
                .flat_map(|b| b.targets.iter())
                .filter_map(|t| match t {
                    Target::Logical(a) => Some(*a),
                    Target::Phys(_) => None,
                }),
        );
        for a in logical.collect::<Vec<_>>() {
            self.bp_bits[usize::from(a >> 6)] |= 1 << (a & 63);
        }
        for b in &self.sym_bps {
            for t in &b.targets {
                if let Target::Phys(p) = *t
                    && let Some(w) = self.phys_bits.get_mut(p >> 6)
                {
                    *w |= 1 << (p & 63);
                    self.phys_count += 1;
                }
            }
        }
        self.bp_version += 1;
    }
    fn resolve_bp(&self, spec: &BpSpec) -> (Option<u32>, Vec<Target>) {
        match spec {
            BpSpec::Line { file, line } => self.symbols.line_targets(file, *line),
            BpSpec::Symbol(name) => {
                let t = self.symbols.symbol_targets(name);
                if t.is_empty()
                    && let Some(a) = name.strip_prefix('$').or_else(|| name.strip_prefix("0x"))
                    && let Ok(a) = u16::from_str_radix(a, 16)
                {
                    return (None, vec![Target::Logical(a)]);
                }
                (None, t)
            }
            BpSpec::Addr(a) => (None, vec![Target::Logical(*a)]),
        }
    }
    pub fn add_sym_bp(&mut self, owner: u32, spec: BpSpec) -> u32 {
        self.refresh_symbols();
        let (line, targets) = self.resolve_bp(&spec);
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        self.sym_bps.push(SymBp {
            id,
            owner,
            spec,
            line,
            targets,
        });
        self.rebuild_bps();
        id
    }
    pub fn remove_sym_bps(&mut self, keep: impl Fn(&SymBp) -> bool) {
        self.sym_bps.retain(keep);
        self.rebuild_bps();
    }
    /// Load symbols (a listing, map or directory) and re-place them.
    pub fn load_symbols(&mut self, spec: &str) -> Result<Vec<usize>, String> {
        let r = self.symbols.load(spec)?;
        self.refresh_symbols();
        self.reresolve();
        Ok(r)
    }
    fn reresolve(&mut self) {
        let specs: Vec<BpSpec> = self.sym_bps.iter().map(|b| b.spec.clone()).collect();
        for (i, spec) in specs.iter().enumerate() {
            let (line, targets) = self.resolve_bp(spec);
            self.sym_bps[i].line = line;
            self.sym_bps[i].targets = targets;
        }
        self.rebuild_bps();
    }
    /// Follow modules that loaded, moved or went away.
    fn refresh_symbols(&mut self) {
        if self.symbols.tables.is_empty() {
            return;
        }
        if !self.os_ready {
            self.os_ready = self.os.detect(&self.machine);
            self.os_slot = self.os.current_slot();
        }
        let images = if self.os_ready { self.os.images(&self.machine) } else { Vec::new() };
        if self.symbols.place(&images) {
            self.reresolve();
        }
    }
    /// The return address heuristic: the word follows a call instruction.
    pub fn is_return_address(&self, w: u16) -> bool {
        let m = &self.machine;
        (2..=4u16).any(|k| {
            let at = w.wrapping_sub(k);
            let l = disassemble(&|a| m.peek(a), at);
            l.len == k && l.flow == Flow::Call && !l.text.starts_with("SWI") && !l.svc
        })
    }
    /// Return addresses on the stack, innermost first: (stack address, value).
    pub fn call_chain(&self, max: usize) -> Vec<(u16, u16)> {
        self.call_chain_from(self.cpu.s, max)
    }
    /// The OS layer, once the OS has been found in memory.
    pub fn os(&self) -> Option<&dyn OsMonitor> {
        self.os_ready.then_some(&*self.os)
    }
    /// The OS layer for names and conventions, even before the OS runs.
    pub fn os_layer(&self) -> &dyn OsMonitor {
        &*self.os
    }
    fn current_proc(&self) -> Option<u16> {
        self.os().map(|o| o.current(&self.machine))
    }
    fn detect(&mut self) {
        if !self.os_ready && self.cpu.cycles >= self.detect_at {
            self.os_ready = self.os.detect(&self.machine);
            self.os_slot = self.os.current_slot();
            self.detect_at = self.cpu.cycles + 200_000;
        }
    }
    /// Disassemble with the OS's service-request convention.
    pub fn dis(&self, addr: u16) -> disasm::Line {
        let m = &self.machine;
        self.dis_with(&|a| m.peek(a), addr)
    }
    pub fn dis_with(&self, read: &dyn Fn(u16) -> u8, addr: u16) -> disasm::Line {
        let os = &*self.os;
        disasm::disassemble_with(read, addr, &|read, at| {
            os.svc_at(read, at).map(|s| (s.len, format!("{} {}", os.svc_mnemonic(), os.service_name(s.code)), u16::from(s.code)))
        })
    }

    /// Execute one instruction (or interrupt entry) with all debug checks.
    pub fn step_one(&mut self) -> Option<Stop> {
        self.detect();
        let irq = self.machine.irq();
        let wait = self.cpu.waiting();
        let executes = (wait == 0 || (wait == 1 && irq)) && !(irq && self.cpu.cc & I == 0);
        let pc = self.cpu.pc;
        let skip = std::mem::take(&mut self.skip_checks);
        if !skip && (self.bp_at(pc) || self.phys_bp_at(pc)) {
            return Some(Stop::Breakpoint(pc));
        }
        let tracing = self.trace_os9 && self.os_ready;
        let current = tracing.then(|| self.tracer.current());
        if let Goal::At {
            pc: target,
            s,
            proc_,
        } = self.goal
            && pc == target
            && self.cpu.s >= s
            && (proc_.is_none() || self.current_proc() == proc_)
        {
            self.goal = Goal::None;
            return Some(Stop::Step);
        }
        if let Goal::Line {
            table,
            src,
            line,
            s,
            proc_,
            task,
            into,
            left,
        } = self.goal
            && self.machine.mmu().task == task
            && (proc_.is_none() || self.current_proc() == proc_)
        {
            let here = self.symbols.line(&self.machine, pc);
            let other = here.is_some_and(|(t, l)| !(t == table && l.src == src && l.line == line));
            // Returned from the starting frame into code without line information.
            let returned = here.is_none() && left;
            if (other && (into || self.cpu.s >= s)) || returned {
                self.goal = Goal::None;
                return Some(Stop::Step);
            }
        }
        let mut stop = None;
        let mut return_insn = false;
        let mut rts = false;
        if executes {
            let m = &self.machine;
            let bytes = [0u16, 1, 2, 3, 4].map(|i| m.peek(pc.wrapping_add(i)));
            rts = bytes[0] == 0x39 || (bytes[0] == 0x35 && bytes[1] & 0x80 != 0);
            return_insn = rts || bytes[0] == 0x3b;
            let pid = current.map_or(0, |c| c.1);
            // Decode from the bytes already fetched (a request fits in five).
            let svc = if bytes.starts_with(self.svc_prefix) {
                self.os.svc_at(&|a| bytes.get(usize::from(a.wrapping_sub(pc))).map_or_else(|| m.peek(a), |&b| b), pc)
            } else {
                None
            };
            if let Some(svc) = svc
                && !skip
                && self.break_calls.contains(&svc.code)
                && self.break_pid.is_none_or(|p| p == pid)
            {
                return Some(Stop::SysCall { code: svc.code, pid });
            }
            if self.trace.len() >= 4096 {
                self.trace.pop_front();
            }
            self.trace.push_back(TraceEntry {
                cycle: self.cpu.cycles,
                regs: Regs::of(&self.cpu),
                bytes,
                pid,
            });
            if tracing && svc.is_some() {
                self.tracer.before(&self.machine, &*self.os, &self.cpu);
            }
        }
        let old = self.cpu.cycles;
        let result = if self.watches.is_empty() {
            self.machine.step(&mut self.cpu).map(|_| ())
        } else {
            let mut probe = Probe {
                m: &mut self.machine,
                watches: &self.watches,
                hit: None,
            };
            let r = if !self.cpu.interrupt(&mut probe, irq, false, false) {
                self.cpu.step(&mut probe).map(|_| ())
            } else {
                Ok(())
            };
            if let Some((addr, write, value)) = probe.hit {
                stop = Some(Stop::Watch { addr, write, value });
            }
            self.machine.tick(self.cpu.cycles - old);
            r
        };
        if let Err(e) = result {
            return Some(Stop::Error(e.to_string()));
        }
        if let Some(e) = self.machine.disk_error() {
            return Some(Stop::Error(e.to_string()));
        }
        if tracing {
            let (done, switch) = self.tracer.after(&self.machine, &*self.os, self.os_slot, &self.cpu);
            if let Some(id) = done
                && let Some(c) = self.tracer.call(id)
                && (self.break_returns.contains(&c.code)
                    || (self.break_errors && c.error.is_some()))
                && self.break_pid.is_none_or(|p| p == c.pid)
            {
                stop = stop.or(Some(Stop::SysReturn(id)));
            }
            let module_change = done
                .and_then(|id| self.tracer.call(id))
                .is_some_and(|c| self.os.image_calls().contains(&c.code));
            if module_change || switch.is_some() {
                self.refresh_symbols();
            }
            if let Some(s) = switch
                && self.break_switch
            {
                stop = stop.or(Some(Stop::Switch {
                    from: s.from,
                    to: s.to,
                }));
            }
        }
        // The debugged code's own return (kernel code may also RTS on
        // another stack in the same task).
        if rts
            && let Goal::Line { s, proc_, .. } = self.goal
            && self.cpu.s > s
            && (proc_.is_none() || self.current_proc() == proc_)
            && self.symbols.locate(&self.machine, pc).is_some()
            && let Goal::Line { left, .. } = &mut self.goal
        {
            *left = true;
        }
        if let Goal::Out { s, proc_ } = self.goal
            && return_insn
            && self.cpu.s > s
            && (proc_.is_none() || self.current_proc() == proc_)
        {
            self.goal = Goal::None;
            stop = stop.or(Some(Stop::Step));
        }
        self.pump_console();
        stop
    }

    fn pump_console(&mut self) {
        if !self.machine.acia.tx.is_empty() {
            let bytes = std::mem::take(&mut self.machine.acia.tx);
            if let Some(t) = &mut self.telnet {
                t.send(0, &bytes, &mut self.machine);
            }
            if self.echo_console {
                use std::io::Write;
                let mut o = io::stdout().lock();
                let _ = o.write_all(&bytes);
                let _ = o.flush();
            }
            self.dap.output(&bytes);
            for &b in &bytes {
                self.seen.push(char::from(b));
            }
            self.console.extend_from_slice(&bytes);
            if self.console.len() > 1 << 20 {
                let cut = self.console.len() - (1 << 19);
                self.console.drain(..cut);
                self.console_base += cut as u64;
            }
            if let Some((prompt, _)) = self.actions.front()
                && self.seen.contains(prompt.as_str())
            {
                let (_, response) = self.actions.pop_front().unwrap();
                self.pending_input.extend(response.bytes());
                self.pending_input.push_back(13);
                self.seen.clear();
                self.next_input = self.cpu.cycles + 100_000;
            }
            if self.seen.len() > 4096 {
                let cut = self.seen.len() - 2048;
                let cut = (cut..self.seen.len())
                    .find(|&i| self.seen.is_char_boundary(i))
                    .unwrap_or(0);
                self.seen.drain(..cut);
            }
        }
        if self.cpu.cycles >= self.next_input
            && self.machine.acia.rx.is_empty()
            && let Some(b) = self.pending_input.pop_front()
        {
            self.machine.acia.rx.push_back(b);
            self.next_input = self.cpu.cycles + 30_000;
        }
    }

    fn resume(&mut self, goal: Goal) {
        self.goal = goal;
        if !self.running {
            self.dap.continued();
        }
        self.running = true;
        self.stop = None;
        self.skip_checks = true;
        self.var_refs.clear();
        self.refresh_symbols();
    }
    fn halt(&mut self, s: Stop) {
        self.running = false;
        self.goal = Goal::None;
        self.refresh_symbols();
        let hits = match s {
            Stop::Breakpoint(pc) => self.hit_ids(pc),
            _ => Vec::new(),
        };
        self.dap.stopped(&s, &s.describe(&self.tracer, &*self.os), hits);
        self.stop = Some(s);
    }
    /// Source-level step (as the editor's step over / step into).
    pub fn step_source(&mut self, into: bool) {
        self.step_line(into);
    }
    /// Step to the next source line, or one instruction without line info.
    fn step_line(&mut self, into: bool) {
        match self.symbols.line(&self.machine, self.cpu.pc) {
            Some((table, l)) => {
                let goal = Goal::Line {
                    table,
                    src: l.src,
                    line: l.line,
                    s: self.cpu.s,
                    proc_: self.current_proc(),
                    task: self.machine.mmu().task,
                    into,
                    left: false,
                };
                self.resume(goal);
            }
            None if into => self.single_step(),
            None => self.step_over(),
        }
    }

    /// Run for up to `budget` host time; returns when stopped or out of time.
    pub fn run_slice(&mut self, budget: Duration) {
        let start = Instant::now();
        let c0 = self.cpu.cycles;
        let limit = if self.speed == 0 {
            u64::MAX
        } else {
            (self.speed as f64 * budget.as_secs_f64()).max(1.0) as u64
        };
        self.refresh_symbols();
        let mut n = 0u32;
        while self.running {
            if let Some(s) = self.step_one() {
                self.halt(s);
                break;
            }
            n += 1;
            if n & 1023 == 0 {
                if start.elapsed() >= budget {
                    break;
                }
                self.poll_telnet();
            }
            if self.cpu.cycles - c0 >= limit {
                let rest = budget.saturating_sub(start.elapsed());
                std::thread::sleep(rest);
                break;
            }
        }
    }
    fn poll_telnet(&mut self) {
        if let Some(t) = &mut self.telnet {
            let _ = t.poll(&mut self.machine);
        }
    }

    fn step_over(&mut self) {
        let l = self.dis(self.cpu.pc);
        if l.flow == Flow::Call {
            let goal = Goal::At {
                pc: self.cpu.pc.wrapping_add(l.len),
                s: self.cpu.s,
                proc_: self.current_proc(),
            };
            self.resume(goal);
        } else {
            self.single_step();
        }
    }
    fn single_step(&mut self) {
        self.skip_checks = true;
        let s = self.step_one().unwrap_or(Stop::Step);
        self.halt(s);
    }

    pub fn handle(&mut self, req: &http::Request) -> http::Response {
        let p = |k: &str| req.param(k);
        let ok = || http::Response::json("{\"ok\":true}".into());
        let bad = |m: &str| http::Response::error(400, m);
        match req.path.as_str() {
            "/api/state" => http::Response::json(self.state_json()),
            "/api/run" => {
                if !self.running {
                    self.resume(Goal::None);
                }
                ok()
            }
            "/api/pause" => {
                if self.running {
                    self.halt(Stop::Pause);
                }
                ok()
            }
            "/api/step" => {
                let n = p("n").and_then(parse_num).unwrap_or(1).clamp(1, 1_000_000);
                self.running = false;
                for i in 0..n {
                    self.skip_checks = i == 0;
                    if let Some(s) = self.step_one() {
                        self.halt(s);
                        return ok();
                    }
                }
                self.halt(Stop::Step);
                ok()
            }
            "/api/over" => {
                self.step_over();
                ok()
            }
            "/api/out" => {
                let goal = Goal::Out {
                    s: self.cpu.s,
                    proc_: self.current_proc(),
                };
                self.resume(goal);
                ok()
            }
            "/api/runto" => match p("addr").and_then(parse_addr) {
                Some(a) => {
                    let goal = Goal::At {
                        pc: a,
                        s: 0,
                        proc_: None,
                    };
                    self.resume(goal);
                    ok()
                }
                None => bad("addr"),
            },
            "/api/speed" => {
                self.speed = p("hz").and_then(parse_num).map_or(0, u64::from);
                ok()
            }
            "/api/reg" => {
                let (Some(name), Some(v)) = (p("name"), p("val").and_then(parse_addr)) else {
                    return bad("name/val");
                };
                let c = &mut self.cpu;
                match name.to_ascii_lowercase().as_str() {
                    "a" => c.a = v as u8,
                    "b" => c.b = v as u8,
                    "d" => c.set_d(v),
                    "dp" => c.dp = v as u8,
                    "cc" => c.cc = v as u8,
                    "x" => c.x = v,
                    "y" => c.y = v,
                    "u" => c.u = v,
                    "s" => c.s = v,
                    "pc" => c.pc = v,
                    _ => return bad("register"),
                }
                ok()
            }
            "/api/bp" => {
                if let Some(a) = p("addr").and_then(parse_addr) {
                    let on = match p("op") {
                        Some("del") => false,
                        Some("toggle") => !self.breakpoints.contains(&a),
                        _ => true,
                    };
                    self.set_bp(a, on);
                }
                // A symbol or FILE:LINE, placed wherever the module is loaded.
                if let Some(t) = p("sym").map(str::trim).filter(|t| !t.is_empty()) {
                    let spec = match t.rsplit_once(':') {
                        Some((file, line)) if line.parse::<u32>().is_ok() => BpSpec::Line {
                            file: file.to_string(),
                            line: line.parse().unwrap(),
                        },
                        _ => BpSpec::Symbol(t.to_string()),
                    };
                    self.add_sym_bp(0, spec);
                }
                if let Some(id) = p("symdel").and_then(parse_num) {
                    self.remove_sym_bps(|b| b.id != id);
                }
                if p("op") == Some("clear") {
                    self.breakpoints.clear();
                    self.remove_sym_bps(|b| b.owner != 0);
                }
                http::Response::json(self.state_json())
            }
            "/api/watch" => {
                match p("op") {
                    Some("clear") => self.watches.clear(),
                    Some("del") => {
                        if let Some(i) = p("index").and_then(parse_num)
                            && (i as usize) < self.watches.len()
                        {
                            self.watches.remove(i as usize);
                        }
                    }
                    _ => {
                        let Some(start) = p("addr").and_then(parse_addr) else {
                            return bad("addr");
                        };
                        let len = p("len").and_then(parse_num).unwrap_or(1).max(1);
                        let kind = match p("kind") {
                            Some("r") => WatchKind::Read,
                            Some("rw") => WatchKind::Access,
                            _ => WatchKind::Write,
                        };
                        let end = start.saturating_add((len - 1).min(0xffff) as u16);
                        self.watches.push(Watch { start, end, kind });
                    }
                }
                http::Response::json(self.state_json())
            }
            "/api/os/break" => {
                if let Some(c) = p("code") {
                    let code = self.os.service_code(c).or_else(|| parse_addr(c).map(|v| v as u8));
                    let Some(code) = code else { return bad("code") };
                    let set = if p("when") == Some("return") {
                        &mut self.break_returns
                    } else {
                        &mut self.break_calls
                    };
                    if p("op") == Some("del") {
                        set.remove(&code);
                    } else {
                        set.insert(code);
                    }
                }
                if let Some(v) = p("errors") {
                    self.break_errors = v == "1";
                }
                if let Some(v) = p("switch") {
                    self.break_switch = v == "1";
                }
                if let Some(v) = p("trace") {
                    self.trace_os9 = v == "1";
                }
                if let Some(v) = p("pid") {
                    self.break_pid = parse_num(v).filter(|&n| n > 0).map(|n| n as u8);
                }
                if p("op") == Some("clear") {
                    self.break_calls.clear();
                    self.break_returns.clear();
                }
                http::Response::json(self.state_json())
            }
            "/api/disasm" => {
                let addr = p("addr").and_then(parse_addr).unwrap_or(self.cpu.pc);
                let n = p("n").and_then(parse_num).unwrap_or(32).min(256);
                // Start a few instructions early by resynchronizing from before PC.
                let back = p("back").and_then(parse_num).unwrap_or(0).min(64) as u16;
                let start = self.resync(addr, back);
                http::Response::json(self.disasm_json(start, n as usize))
            }
            "/api/mem" => {
                let addr = p("addr").and_then(parse_addr).unwrap_or(0);
                let len = p("len").and_then(parse_num).unwrap_or(256).min(4096) as u16;
                if req.method == "POST" {
                    let Some(hex) = p("val") else {
                        return bad("val");
                    };
                    let bytes: Vec<u8> = hex
                        .as_bytes()
                        .chunks(2)
                        .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
                        .collect();
                    for (i, b) in bytes.into_iter().enumerate() {
                        self.machine.poke(addr.wrapping_add(i as u16), b);
                    }
                    return ok();
                }
                http::Response::json(self.mem_json(addr, len, p("space").unwrap_or("cpu")))
            }
            "/api/trace" => {
                let n = p("n").and_then(parse_num).unwrap_or(200).min(4096) as usize;
                http::Response::json(self.trace_json(n))
            }
            "/api/console" => {
                if req.method == "POST" {
                    for &b in &req.body {
                        self.machine.acia.rx.push_back(match b {
                            10 => 13,
                            127 => 8,
                            _ => b,
                        });
                    }
                    return ok();
                }
                let since = p("since").and_then(parse_num).map_or(0, u64::from);
                let from = since.max(self.console_base);
                let i = (from - self.console_base) as usize;
                let text = String::from_utf8_lossy(&self.console[i.min(self.console.len())..]);
                http::Response::json(format!(
                    "{{\"next\":{},\"text\":{}}}",
                    self.console_base + self.console.len() as u64,
                    json_str(&text)
                ))
            }
            "/api/os/info" => http::Response::json(self.os.info().to_string()),
            "/api/os/procs" => http::Response::json(self.procs_json()),
            "/api/os/modules" => http::Response::json(self.modules_json()),
            "/api/os/memmap" => http::Response::json(self.memmap_json()),
            "/api/os/calls" => {
                let since = p("since").and_then(parse_num).map_or(0, u64::from);
                let limit = p("limit").and_then(parse_num).unwrap_or(2000) as usize;
                let user = p("user") == Some("1");
                let parent = p("parent").and_then(parse_num).map(u64::from);
                let pid = p("pid").and_then(parse_num).map(|n| n as u8);
                let f = |c: &osmon::Call| {
                    (!user || !c.system)
                        && parent.is_none_or(|q| c.parent == Some(q))
                        && pid.is_none_or(|q| c.pid == q)
                };
                http::Response::json(self.calls_json(since, limit, &f))
            }
            "/api/os/services" => http::Response::json(self.services_json()),
            "/api/symbols" => {
                if p("op") == Some("clear") {
                    self.symbols.clear();
                    self.reresolve();
                }
                if let Some(dir) = p("dir") {
                    self.symbols.add_source_dir(dir.into());
                }
                if let Some(path) = p("path")
                    && let Err(e) = self.load_symbols(path)
                {
                    return bad(&e);
                }
                self.refresh_symbols();
                http::Response::json(self.symbols_json())
            }
            "/api/resolve" => {
                self.refresh_symbols();
                match p("expr").and_then(|e| self.symbols.resolve(&self.machine, e)) {
                    Some(a) => http::Response::json(format!("{{\"addr\":{a}}}")),
                    None => bad("expr"),
                }
            }
            "/api/vars" => {
                self.refresh_symbols();
                let f = p("frame").and_then(parse_num).unwrap_or(0) as usize;
                http::Response::json(self.vars_json(f))
            }
            "/api/stack" => {
                self.refresh_symbols();
                let n = p("n").and_then(parse_num).unwrap_or(32).clamp(1, 256) as u16;
                http::Response::json(self.stack_json(n))
            }
            "/api/source" => {
                let (Some(t), Some(si)) = (p("table").and_then(parse_num), p("src").and_then(parse_num)) else {
                    return bad("table/src");
                };
                match self.symbols.tables.get(t as usize).and_then(|t| t.sources.get(si as usize)) {
                    Some(src) => {
                        let lines: Vec<String> = src.text.iter().map(|(n, l)| format!("[{n},{}]", json_str(l))).collect();
                        http::Response::json(format!("{{\"name\":{},\"lines\":[{}]}}", json_str(&src.name), lines.join(",")))
                    }
                    None => bad("source"),
                }
            }
            _ => http::Response::error(404, "unknown API"),
        }
    }

    /// Find an instruction boundary about `back` instructions before `addr`.
    fn resync(&self, addr: u16, back: u16) -> u16 {
        if back == 0 {
            return addr;
        }
        // Try start points and keep one whose decode lands exactly on addr.
        for dist in (back..=back * 4).rev() {
            let mut a = addr.wrapping_sub(dist);
            let mut count = 0;
            while a != addr && addr.wrapping_sub(a) <= dist {
                a = a.wrapping_add(self.dis(a).len.max(1));
                count += 1;
            }
            if a == addr && count >= back {
                let mut a = addr.wrapping_sub(dist);
                for _ in 0..count - back {
                    a = a.wrapping_add(self.dis(a).len.max(1));
                }
                return a;
            }
        }
        addr
    }

    fn regs_json(r: &Regs) -> String {
        format!(
            "{{\"a\":{},\"b\":{},\"dp\":{},\"cc\":{},\"x\":{},\"y\":{},\"u\":{},\"s\":{},\"pc\":{}}}",
            r.a, r.b, r.dp, r.cc, r.x, r.y, r.u, r.s, r.pc
        )
    }

    fn state_json(&self) -> String {
        let mmu = self.machine.mmu();
        let mut o = String::new();
        let stop = self
            .stop
            .as_ref()
            .map(|s| s.describe(&self.tracer, &*self.os))
            .unwrap_or_default();
        let _ = write!(
            o,
            "{{\"running\":{},\"stop\":{},\"cycles\":{},\"speed\":{},\"wait\":{},\"regs\":{},",
            self.running,
            json_str(&stop),
            self.cpu.cycles,
            self.speed,
            self.cpu.waiting(),
            Self::regs_json(&Regs::of(&self.cpu))
        );
        let maps: Vec<String> = mmu.maps.iter().map(|m| m.to_string()).collect();
        let _ = write!(
            o,
            "\"mmu\":{{\"enabled\":{},\"task\":{},\"romdis\":{},\"frt\":{},\"maps\":[{}]}},\"ram\":{},",
            mmu.enabled,
            mmu.task,
            mmu.rom_disabled,
            mmu.fixed_top,
            maps.join(","),
            self.machine.ram.len()
        );
        let bps: Vec<String> = self.breakpoints.iter().map(|a| a.to_string()).collect();
        let sym_bps: Vec<String> = self
            .sym_bps
            .iter()
            .map(|b| {
                let at: Vec<String> = b
                    .targets
                    .iter()
                    .filter_map(|t| self.symbols.logical(&self.machine, *t))
                    .map(|a| a.to_string())
                    .collect();
                format!(
                    "{{\"id\":{},\"owner\":{},\"text\":{},\"resolved\":{},\"addrs\":[{}]}}",
                    b.id,
                    b.owner,
                    json_str(&b.text()),
                    b.targets.len(),
                    at.join(",")
                )
            })
            .collect();
        let _ = write!(o, "\"symBreakpoints\":[{}],\"symbols\":{},", sym_bps.join(","), self.symbols.tables.len());
        let ws: Vec<String> = self
            .watches
            .iter()
            .map(|w| {
                let k = match w.kind {
                    WatchKind::Read => "r",
                    WatchKind::Write => "w",
                    WatchKind::Access => "rw",
                };
                format!(
                    "{{\"start\":{},\"end\":{},\"kind\":\"{k}\"}}",
                    w.start, w.end
                )
            })
            .collect();
        let names = |s: &BTreeSet<u8>| {
            s.iter()
                .map(|c| json_str(&self.os.service_name(*c)))
                .collect::<Vec<_>>()
                .join(",")
        };
        let _ = write!(
            o,
            "\"breakpoints\":[{}],\"watches\":[{}],\"osbreak\":{{\"calls\":[{}],\"returns\":[{}],\"errors\":{},\"switch\":{},\"pid\":{},\"trace\":{}}},",
            bps.join(","),
            ws.join(","),
            names(&self.break_calls),
            names(&self.break_returns),
            self.break_errors,
            self.break_switch,
            self.break_pid.unwrap_or(0),
            self.trace_os9
        );
        match self.os() {
            Some(os) => {
                let m = &self.machine;
                // OS-specific fields (e.g. level and queues), then the trace counters.
                let mut fields: Vec<String> = os
                    .summary(m)
                    .into_iter()
                    .map(|(k, v)| format!("{}:{v}", json_str(k)))
                    .collect();
                fields.insert(1.min(fields.len()), format!("\"pid\":{}", os.pid_of(m, os.current(m))));
                fields.push(format!("\"callVersion\":{}", self.tracer.version));
                fields.push(format!("\"calls\":{}", self.tracer.last_id()));
                fields.push(format!("\"switches\":{}", self.tracer.switches.len()));
                let _ = write!(o, "\"os\":{{{}}},", fields.join(","));
            }
            None => o.push_str("\"os\":null,"),
        }
        let _ = write!(
            o,
            "\"console\":{}}}",
            self.console_base + self.console.len() as u64
        );
        o
    }

    fn disasm_json(&self, start: u16, n: usize) -> String {
        let m = &self.machine;
        let mut a = start;
        let mut items = Vec::new();
        for _ in 0..n {
            let l = self.dis(a);
            let bytes: Vec<String> = (0..l.len)
                .map(|i| format!("{:02X}", m.peek(a.wrapping_add(i))))
                .collect();
            let explain = if l.svc {
                l.target.map(|c| self.os.explain(c as u8)).unwrap_or("")
            } else {
                ""
            };
            let sym = &self.symbols;
            let label = sym.exact(m, a).map_or("null".into(), json_str);
            // Name the destination of branches, calls and extended operands.
            let target = match l.target {
                Some(t) if !l.svc => sym.label(m, t),
                _ => None,
            };
            let src = match sym.line(m, a) {
                Some((ti, r)) if r.off == sym.locate(m, a).map_or(u16::MAX, |x| x.1) => {
                    let s = &sym.tables[ti].sources[r.src];
                    let text = s.text.get(&r.line).map_or("", String::as_str);
                    format!(
                        "{{\"file\":{},\"line\":{},\"text\":{}}}",
                        json_str(&s.name),
                        r.line,
                        json_str(text)
                    )
                }
                _ => "null".into(),
            };
            items.push(format!(
                "{{\"addr\":{},\"bytes\":\"{}\",\"text\":{},\"bp\":{},\"phys\":{},\"note\":{},\"label\":{},\"tsym\":{},\"src\":{},\"svc\":{}}}",
                a,
                bytes.join(" "),
                json_str(&l.text),
                self.bp_at(a) || self.phys_bp_at(a),
                m.translate(a).map_or(-1, |p| p as i64),
                json_str(explain),
                label,
                target.map_or("null".into(), |t| json_str(&t)),
                src,
                l.svc
            ));
            a = a.wrapping_add(l.len.max(1));
        }
        format!("{{\"lines\":[{}]}}", items.join(","))
    }

    fn symbols_json(&self) -> String {
        let placed = |i: usize| self.symbols.placements.iter().filter(|p| p.table == i).count();
        let tables: Vec<String> = self
            .symbols
            .tables
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let sources: Vec<String> = t
                    .sources
                    .iter()
                    .map(|s| {
                        format!(
                            "{{\"name\":{},\"path\":{}}}",
                            json_str(&s.name),
                            s.path.as_ref().map_or("null".into(), |p| json_str(&p.to_string_lossy()))
                        )
                    })
                    .collect();
                format!(
                    "{{\"path\":{},\"kind\":\"{}\",\"module\":{},\"symbols\":{},\"lines\":{},\"placed\":{},\"sources\":[{}]}}",
                    json_str(&t.path.to_string_lossy()),
                    t.kind,
                    t.module.as_ref().map_or("null".into(), |m| json_str(m)),
                    t.symbols.len(),
                    t.lines.len(),
                    placed(i),
                    sources.join(",")
                )
            })
            .collect();
        format!("{{\"tables\":[{}]}}", tables.join(","))
    }

    fn stack_json(&self, n: u16) -> String {
        let m = &self.machine;
        let s = self.cpu.s;
        let items: Vec<String> = (0..n)
            .map(|i| {
                let at = s.wrapping_add(i * 2);
                let w = m.peek16(at);
                let ret = self.is_return_address(w);
                let label = self.symbols.label(m, w);
                format!(
                    "{{\"addr\":{at},\"value\":{w},\"ret\":{ret},\"label\":{}}}",
                    label.map_or("null".into(), |l| json_str(&l))
                )
            })
            .collect();
        let chain: Vec<String> = self
            .call_chain(16)
            .iter()
            .map(|&(at, w)| {
                format!(
                    "{{\"addr\":{at},\"value\":{w},\"label\":{}}}",
                    self.symbols.label(m, w).map_or("null".into(), |l| json_str(&l))
                )
            })
            .collect();
        format!(
            "{{\"s\":{s},\"pc\":{},\"where\":{},\"words\":[{}],\"chain\":[{}]}}",
            self.cpu.pc,
            self.symbols.label(m, self.cpu.pc).map_or("null".into(), |l| json_str(&l)),
            items.join(","),
            chain.join(",")
        )
    }

    fn mem_json(&self, addr: u16, len: u16, space: &str) -> String {
        let m = &self.machine;
        let bytes: Vec<u8> = match space {
            "phys" => {
                // Physical addresses use the 16-bit address as a page within
                // the bank given by `bank`.
                (0..len)
                    .map(|i| {
                        m.ram
                            .get(usize::from(addr) + usize::from(i))
                            .copied()
                            .unwrap_or(0xff)
                    })
                    .collect()
            }
            // OS address spaces (OS-9: `sys`, `pidN`).
            space => match self.os() {
                Some(os) if space == "sys" || space.starts_with("pid") => (0..len)
                    .map(|i| os.read_space(m, space, addr.wrapping_add(i)).unwrap_or(0xff))
                    .collect(),
                None if space == "sys" => (0..len).map(|i| m.peek(addr.wrapping_add(i))).collect(),
                _ if space == "sys" || space.starts_with("pid") => vec![0xff; usize::from(len)],
                _ => (0..len).map(|i| m.peek(addr.wrapping_add(i))).collect(),
            },
        };
        let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        format!("{{\"addr\":{addr},\"hex\":\"{hex}\"}}")
    }

    fn trace_json(&self, n: usize) -> String {
        let skip = self.trace.len().saturating_sub(n);
        let items: Vec<String> = self
            .trace
            .iter()
            .skip(skip)
            .map(|t| {
                let pc = t.regs.pc;
                let b = t.bytes;
                let l = self.dis_with(&|a| b.get(usize::from(a.wrapping_sub(pc))).copied().unwrap_or(0), pc);
                format!(
                    "{{\"cycle\":{},\"pid\":{},\"regs\":{},\"text\":{}}}",
                    t.cycle,
                    t.pid,
                    Self::regs_json(&t.regs),
                    json_str(&l.text)
                )
            })
            .collect();
        format!("{{\"trace\":[{}]}}", items.join(","))
    }

    fn procs_json(&self) -> String {
        let Some(os) = self.os() else {
            return "{\"procs\":[]}".into();
        };
        let items: Vec<String> = os
            .processes(&self.machine)
            .iter()
            .map(|p| {
                let mut f = vec![
                    format!("\"id\":{}", p.id),
                    format!("\"parent\":{}", p.parent),
                    format!("\"queue\":{}", json_str(p.queue)),
                ];
                f.extend(p.fields.iter().map(|(k, v)| format!("{}:{v}", json_str(k))));
                format!("{{{}}}", f.join(","))
            })
            .collect();
        format!("{{\"procs\":[{}]}}", items.join(","))
    }

    fn modules_json(&self) -> String {
        let Some(os) = self.os() else {
            return "{\"modules\":[]}".into();
        };
        let items: Vec<String> = os
            .images(&self.machine)
            .iter()
            .map(|m| {
                let mut f = vec![format!("\"name\":{}", json_str(&m.name))];
                f.extend(m.fields.iter().map(|(k, v)| format!("{}:{v}", json_str(k))));
                format!("{{{}}}", f.join(","))
            })
            .collect();
        format!("{{\"modules\":[{}]}}", items.join(","))
    }

    fn memmap_json(&self) -> String {
        match self.os() {
            Some(os) => os.memory_map(&self.machine).to_string(),
            None => "{\"unit\":0,\"map\":[]}".into(),
        }
    }

    fn calls_json(&self, since: u64, limit: usize, filter: &dyn Fn(&osmon::Call) -> bool) -> String {
        let generic = |r: &Regs| {
            format!(
                "A=${:02X} B=${:02X} X=${:04X} Y=${:04X} U=${:04X}",
                r.a, r.b, r.x, r.y, r.u
            )
        };
        let items: Vec<String> = self
            .tracer
            .calls
            .values()
            .filter(|c| c.version > since && filter(c))
            .rev()
            .take(limit)
            .map(|c| {
                format!(
                    "{{\"id\":{},\"parent\":{},\"cycle\":{},\"end\":{},\"pid\":{},\"code\":{},\"name\":{},\"system\":{},\"depth\":{},\"in\":{},\"out\":{},\"args\":{},\"result\":{},\"error\":{},\"noReturn\":{},\"inner\":[{}]}}",
                    c.id,
                    c.parent.map_or("null".into(), |p| p.to_string()),
                    c.cycle,
                    c.end.map_or("null".into(), |e| e.to_string()),
                    c.pid,
                    c.code,
                    json_str(&self.os.service_name(c.code)),
                    c.system,
                    c.depth,
                    Self::regs_json(&c.inp),
                    c.out.as_ref().map_or("null".into(), Self::regs_json),
                    json_str(&if c.args.is_empty() && c.system { generic(&c.inp) } else { c.args.clone() }),
                    json_str(&if c.result.is_empty() && c.out.is_some() { "OK".to_string() } else { c.result.clone() }),
                    c.error.map_or("null".into(), |e| e.to_string()),
                    c.no_return,
                    c.inner
                        .iter()
                        .map(|(code, n)| format!("[{},{n}]", json_str(&self.os.service_name(*code))))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect();
        let sw: Vec<String> = self
            .tracer
            .switches
            .iter()
            .rev()
            .take(limit)
            .map(|s| format!("[{},{},{}]", s.cycle, s.from, s.to))
            .collect();
        format!(
            "{{\"version\":{},\"calls\":[{}],\"switches\":[{}]}}",
            self.tracer.version,
            items.join(","),
            sw.join(",")
        )
    }

    fn services_json(&self) -> String {
        let items: Vec<String> = self
            .os
            .services()
            .iter()
            .map(|v| {
                format!(
                    "{{\"code\":{},\"name\":{},\"desc\":{},\"explain\":{}}}",
                    v.code,
                    json_str(&v.name),
                    json_str(&v.desc),
                    json_str(v.explain)
                )
            })
            .collect();
        format!("{{\"services\":[{}]}}", items.join(","))
    }

    /// Run an API request directly (as the browser would), returning the
    /// HTTP status and JSON body. Used by tests and scripts.
    pub fn command(&mut self, method: &str, target: &str, body: &[u8]) -> (u16, String) {
        let (path, query) = http::parse_target(target);
        let (reply, _) = mpsc::channel();
        let req = http::Request {
            method: method.into(),
            path,
            query,
            body: body.to_vec(),
            reply,
        };
        let r = self.handle(&req);
        (r.status, String::from_utf8_lossy(&r.body).into_owned())
    }
    /// Keep executing while `running`, at most `max_steps` instructions.
    pub fn run_steps(&mut self, max_steps: u64) {
        for _ in 0..max_steps {
            if !self.running {
                return;
            }
            if let Some(s) = self.step_one() {
                self.halt(s);
            }
        }
    }

    fn dispatch(&mut self, m: Inbox) {
        match m {
            Inbox::Http(req) => {
                let r = self.handle(&req);
                let _ = req.reply.send(r);
            }
            Inbox::Dap { id, msg, out } => self.dap_message(id, &msg, out),
            Inbox::DapClosed(id) => self.dap_closed(id),
        }
    }

    /// Serve the UI on `listener` (and DAP on `dap`) until terminated.
    pub fn serve(mut self, listener: TcpListener, dap: Option<TcpListener>, start_running: bool) -> io::Result<()> {
        let (tx, rx): (_, Receiver<Inbox>) = mpsc::channel();
        http::start(listener, tx.clone(), UI);
        if let Some(l) = dap {
            dap::start(l, tx);
        } else {
            drop(tx);
        }
        if start_running {
            self.resume(Goal::None);
        }
        loop {
            if self.running {
                self.run_slice(Duration::from_millis(8));
                while let Ok(m) = rx.try_recv() {
                    self.dispatch(m);
                }
            } else {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(m) => self.dispatch(m),
                    Err(RecvTimeoutError::Timeout) => self.poll_telnet(),
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
            self.dap_sync();
        }
    }
}
