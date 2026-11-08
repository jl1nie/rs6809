//! In-circuit-emulator style debugger with a browser UI and NitrOS-9 awareness.
//!
//! The emulator thread owns the CPU and machine. The HTTP threads forward API
//! requests over a channel, so inspection always sees a consistent state.
use crate::tr;
pub mod cdecl;
pub mod dap;
pub mod debuginfo;
pub mod disasm;
pub mod expr;
pub mod frames;
pub mod http;
pub mod json;
pub mod lang;
pub mod microware;
pub mod os9;
pub mod osmon;
pub mod symbols;
mod tables;
pub mod target;
pub mod cpm68k;
pub mod fuzix;
pub mod fpga;

use crate::telnet::TelnetServer;
use crate::{Cpu, Machine};
use osmon::{CpuRegs, OsMonitor, Tracer};
use std::collections::{BTreeSet, VecDeque};
use symbols::{Symbols, Target as BpTarget};
use target::{Flow, Mc6809, Target};
pub use target::{Board68k, Mc68000};
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
fn parse_addr(s: &str) -> Option<u32> {
    let s = s.trim();
    // Addresses default to hexadecimal, as on a monitor.
    let h = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix('$'))
        .unwrap_or(s);
    u32::from_str_radix(h, 16).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    Read,
    Write,
    Access,
}

#[derive(Debug, Clone, Copy)]
pub struct Watch {
    pub start: u32,
    pub end: u32,
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
    Addr(u32),
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
    pub targets: Vec<BpTarget>,
    pub cond: BpCond,
}
/// When a breakpoint stops: a condition, a hit count, and a log message
/// (a logpoint logs and goes on). `count` is the number of times reached
/// with the condition true.
#[derive(Debug, Clone, Default)]
pub struct BpCond {
    pub cond: Option<(String, expr::Expr)>,
    pub hits: Option<(String, expr::Hits)>,
    pub log: Option<String>,
    pub count: u64,
}
impl BpCond {
    /// From the text fields (empty ones are not set).
    pub fn new(cond: &str, hits: &str, log: &str) -> Result<Self, String> {
        let cond = cond.trim();
        let hits = hits.trim();
        Ok(Self {
            cond: if cond.is_empty() { None } else { Some((cond.to_string(), expr::Expr::parse(cond)?)) },
            hits: if hits.is_empty() { None } else { Some((hits.to_string(), expr::Hits::parse(hits)?)) },
            log: Some(log.to_string()).filter(|l| !l.is_empty()),
            count: 0,
        })
    }
    pub fn is_plain(&self) -> bool {
        self.cond.is_none() && self.hits.is_none() && self.log.is_none()
    }
    fn json(&self) -> String {
        format!(
            "\"cond\":{},\"hits\":{},\"log\":{},\"count\":{}",
            json_str(self.cond.as_ref().map_or("", |c| &c.0)),
            json_str(self.hits.as_ref().map_or("", |c| &c.0)),
            json_str(self.log.as_deref().unwrap_or("")),
            self.count
        )
    }
}
/// Registers, memory, symbols and the OS process, for conditions.
struct CondEnv<'a, T: Target> {
    ice: &'a Ice<T>,
}
impl<T: Target> expr::Env for CondEnv<'_, T> {
    fn reg(&self, name: &str) -> Option<i64> {
        if name.eq_ignore_ascii_case("pid") {
            return self.ice.current_proc().map(i64::from);
        }
        self.ice.t.reg(name).map(i64::from)
    }
    fn sym(&self, name: &str) -> Option<i64> {
        let m = self.ice.t.mem();
        self.ice.symbols.symbol_targets(name).into_iter().find_map(|t| self.ice.symbols.logical(m, t)).map(i64::from)
    }
    fn mem(&self, addr: u32, size: u8) -> i64 {
        let m = self.ice.t.mem();
        (0..u32::from(size)).fold(0, |v, i| v << 8 | i64::from(m.peek(addr.wrapping_add(i))))
    }
}
/// What a reached breakpoint does.
enum BpAction {
    Go,
    Stop,
    Log(String),
    Error(String),
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
    Breakpoint(u32),
    /// Stepping back reached the oldest recorded instruction.
    HistoryStart,
    Watch { addr: u32, write: bool, value: u8 },
    SysCall { code: u8, pid: u8 },
    SysReturn(u64),
    Switch { from: u8, to: u8 },
    Error(String),
}
impl Stop {
    fn describe(&self, t: &Tracer, os: &dyn OsMonitor) -> String {
        let call = |id: &u64| {
            t.call(*id)
                .map(|c| tr!(format!("{} (プロセス{})", os.service_name(c.code), c.pid), format!("{} (process {})", os.service_name(c.code), c.pid)))
                .unwrap_or_default()
        };
        match self {
            Stop::Pause => tr!("一時停止", "paused").into(),
            Stop::Step => tr!("ステップ完了", "step done").into(),
            Stop::Breakpoint(a) => tr!(format!("ブレークポイント ${a:04X}"), format!("breakpoint ${a:04X}")),
            Stop::Watch { addr, write, value } => tr!(format!(
                "ウォッチ ${addr:04X} {} ${value:02X}",
                if *write {
                    tr!("書き込み", "write")
                } else {
                    tr!("読み出し", "read")
                }
            ), format!(
                "watch ${addr:04X} {} ${value:02X}",
                if *write {
                    tr!("書き込み", "write")
                } else {
                    tr!("読み出し", "read")
                }
            )),
            Stop::SysCall { code, pid } => {
                tr!(format!(
                    "システムコール呼び出し {} (プロセス{pid})",
                    os.service_name(*code)
                ), format!(
                    "system call {} (process {pid})",
                    os.service_name(*code)
                ))
            }
            Stop::SysReturn(id) => {
                let err = t.call(*id).and_then(|c| c.error).map(|e| os.error_name(e));
                tr!(format!(
                    "システムコール復帰 {}{}",
                    call(id),
                    err.map_or(String::new(), |e| format!(" → {e}"))
                ), format!(
                    "system call return {}{}",
                    call(id),
                    err.map_or(String::new(), |e| format!(" → {e}"))
                ))
            }
            Stop::Switch { from, to } => tr!(format!("プロセス切り替え {from} → {to}"), format!("process switch {from} → {to}")),
            Stop::Error(e) => tr!(format!("エラー: {e}"), format!("error: {e}")),
            Stop::HistoryStart => tr!("記録の先頭（これより前には戻れない）", "start of the recording (cannot go back further)").into(),
        }
    }
}

/// Temporary stop condition for step over / step out / run to.
#[derive(Debug, Clone, Copy)]
enum Goal {
    None,
    /// Stop at `pc` once S has unwound to at least `s` in the same process.
    At {
        pc: u32,
        s: u32,
        proc_: Option<u32>,
    },
    /// Stop after a return instruction that leaves S above `s`.
    Out {
        s: u32,
        proc_: Option<u32>,
    },
    /// Source-level step: stop on another source line. Without `into`,
    /// deeper frames (S below `s`) are skipped.
    Line {
        table: usize,
        src: usize,
        line: u32,
        s: u32,
        proc_: Option<u32>,
        /// Hardware task: Level 2 runs interrupts and service requests in
        /// the system task on another stack, which must not end the step.
        task: u32,
        into: bool,
        /// An RTS/PULS PC left the starting frame.
        left: bool,
    },
}

#[derive(Clone, Copy)]
pub struct TraceEntry<S> {
    pub cycle: u64,
    pub regs: S,
    pub bytes: [u8; 10],
    pub pid: u8,
}

pub struct Options {
    pub actions: VecDeque<(String, String)>,
    pub echo_console: bool,
}

pub struct Ice<T: Target = Mc6809> {
    /// The CPU and machine (for the 6809: `t.cpu`, `t.machine`).
    pub t: T,
    pub breakpoints: BTreeSet<u32>,
    /// Recent idle samples (one bit each) and the start of idle pacing.
    idle_bits: u16,
    idle_mark: Option<(Instant, u64)>,
    /// Recording for stepping back: the CPU before each instruction and the
    /// number of RAM bytes it wrote (their old values are in `rec_writes`).
    pub recording: bool,
    rec: VecDeque<(T::CpuCopy, u32)>,
    rec_writes: VecDeque<(usize, u8)>,
    rec_buf: Vec<(usize, u8)>,
    /// Refresh the symbol placements once back in this context (after
    /// having left it).
    refresh_after: Option<(u32, bool)>,
    /// `os.sleep_code()`, and the cycle of the last such call.
    sleep_code: Option<u8>,
    last_sleep: u64,
    /// Conditions of the address breakpoints (`breakpoints`) that have one.
    pub bp_conds: std::collections::BTreeMap<u32, BpCond>,
    /// Logpoint output and condition errors, numbered.
    bp_log: VecDeque<(u64, String)>,
    bp_log_seq: u64,
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
    pub trace: VecDeque<TraceEntry<T::Snap>>,
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
    /// The script as given, replayed after a reset.
    script: VecDeque<(String, String)>,
    /// Prompts answered whenever they show (`?PROMPT` lines).
    optional: Vec<(String, String)>,
    /// The browser console's size (rows, columns), kept in the OS's tty.
    console_size: Option<(u16, u16)>,
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

impl Ice<Mc6809> {
    /// An ICE for a 6809 running NitrOS-9 (the OS of the Multicomp images).
    pub fn new(cpu: Cpu, machine: Machine, options: Options) -> Self {
        Self::with_os(cpu, machine, options, Box::new(os9::Nitros9::default()))
    }
    pub fn with_os(cpu: Cpu, machine: Machine, options: Options, os: Box<dyn OsMonitor>) -> Self {
        Ice::for_target(Mc6809 { cpu, machine, start: None }, options, os)
    }
}

impl<B: target::Board68k> Ice<Mc68000<B>> {
    /// An ICE for a bare 68000 program (no OS).
    pub fn m68000(t: Mc68000<B>, options: Options) -> Self {
        Ice::for_target(t, options, Box::new(osmon::NoOs))
    }
}

impl<T: Target> Ice<T> {
    pub fn for_target(mut t: T, options: Options, os: Box<dyn OsMonitor>) -> Self {
        t.mark_start();
        let bits = 1usize << t.addr_bits();
        Self {
            svc_prefix: os.svc_prefix(),
            sleep_code: os.sleep_code(),
            os_slot: None,
            t,
            breakpoints: BTreeSet::new(),
            idle_bits: 0,
            idle_mark: None,
            refresh_after: None,
            recording: false,
            rec: VecDeque::new(),
            rec_writes: VecDeque::new(),
            rec_buf: Vec::new(),
            last_sleep: 0,
            bp_conds: Default::default(),
            bp_log: VecDeque::new(),
            bp_log_seq: 0,
            bp_bits: vec![0; bits.div_ceil(64)],
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
            optional: options
                .actions
                .iter()
                .filter_map(|(p, r)| p.strip_prefix('?').map(|p| (p.to_string(), r.clone())))
                .collect(),
            script: options.actions.iter().filter(|(p, _)| !p.starts_with('?')).cloned().collect(),
            console_size: None,
            actions: options.actions.into_iter().filter(|(p, _)| !p.starts_with('?')).collect(),
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
    fn set_bp(&mut self, a: u32, on: bool) {
        let a = a & self.amask();
        if on {
            self.breakpoints.insert(a);
        } else {
            self.breakpoints.remove(&a);
            self.bp_conds.remove(&a);
        }
        self.rebuild_bps();
    }
    fn bp_at(&self, a: u32) -> bool {
        self.bp_bits.get((a >> 6) as usize).is_some_and(|w| w & (1 << (a & 63)) != 0)
    }
    fn phys_bp_at(&self, pc: u32) -> bool {
        self.phys_count > 0
            && self
                .t
                .mem()
                .translate(pc)
                .is_some_and(|p| self.phys_bits.get(p >> 6).is_some_and(|w| w & (1 << (p & 63)) != 0))
    }
    /// Recompute the bit tables from user and symbolic breakpoints.
    fn rebuild_bps(&mut self) {
        self.bp_bits.iter_mut().for_each(|w| *w = 0);
        self.phys_bits = vec![0; self.t.ram_len().div_ceil(64)];
        self.phys_count = 0;
        let logical = self.breakpoints.iter().copied().chain(
            self.sym_bps
                .iter()
                .flat_map(|b| b.targets.iter())
                .filter_map(|t| match t {
                    BpTarget::Logical(a) => Some(*a),
                    BpTarget::Phys(_) => None,
                }),
        );
        for a in logical.collect::<Vec<_>>() {
            if let Some(w) = self.bp_bits.get_mut((a >> 6) as usize) {
                *w |= 1 << (a & 63);
            }
        }
        for b in &self.sym_bps {
            for t in &b.targets {
                if let BpTarget::Phys(p) = *t
                    && let Some(w) = self.phys_bits.get_mut(p >> 6)
                {
                    *w |= 1 << (p & 63);
                    self.phys_count += 1;
                }
            }
        }
        self.bp_version += 1;
    }
    fn resolve_bp(&self, spec: &BpSpec) -> (Option<u32>, Vec<BpTarget>) {
        match spec {
            BpSpec::Line { file, line } => self.symbols.line_targets(file, *line),
            BpSpec::Symbol(name) => {
                let t = self.symbols.symbol_targets(name);
                if t.is_empty()
                    && let Some(a) = name.strip_prefix('$').or_else(|| name.strip_prefix("0x"))
                    && let Ok(a) = u32::from_str_radix(a, 16)
                {
                    return (None, vec![BpTarget::Logical(a)]);
                }
                (None, t)
            }
            BpSpec::Addr(a) => (None, vec![BpTarget::Logical(*a)]),
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
            cond: BpCond::default(),
        });
        self.rebuild_bps();
        id
    }
    /// Set the condition of a symbolic breakpoint.
    pub fn set_sym_cond(&mut self, id: u32, cond: BpCond) {
        if let Some(b) = self.sym_bps.iter_mut().find(|b| b.id == id) {
            b.cond = cond;
        }
    }
    /// One instruction, remembering how to undo it: the CPU and the RAM it
    /// wrote. Devices are not undone (a write to one, such as a console
    /// byte or a timer acknowledge, happens again when run forward).
    fn step_recorded(&mut self) -> Result<Option<target::WatchHit>, String> {
        const DEPTH: usize = 200_000;
        let cpu = self.t.cpu_copy();
        let mut log = std::mem::take(&mut self.rec_buf);
        log.clear();
        let r = self.t.step_logged(&self.watches, &mut log);
        let out = match r {
            Ok((hit, _io)) => {
                self.rec.push_back((cpu, log.len() as u32));
                self.rec_writes.extend(log.iter().copied());
                while self.rec.len() > DEPTH {
                    let (_, n) = self.rec.pop_front().unwrap();
                    self.rec_writes.drain(..n as usize);
                }
                Ok(hit)
            }
            Err(e) => Err(e),
        };
        self.rec_buf = log;
        out
    }
    /// Undo the last recorded instruction. False when there is none.
    fn back_one(&mut self) -> bool {
        let Some((cpu, n)) = self.rec.pop_back() else { return false };
        for _ in 0..n {
            let (p, v) = self.rec_writes.pop_back().unwrap();
            self.t.restore_ram(p, v);
        }
        self.t.set_cpu(cpu);
        let c = self.t.cycles();
        while self.trace.back().is_some_and(|e| e.cycle >= c) {
            self.trace.pop_back();
        }
        true
    }
    /// Step back `n` instructions, or (reverse continue) until a breakpoint.
    fn step_back(&mut self, n: usize, to_breakpoint: bool) -> Stop {
        self.running = false;
        self.goal = Goal::None;
        for i in 0.. {
            if !to_breakpoint && i >= n {
                return Stop::Step;
            }
            if !self.back_one() {
                return Stop::HistoryStart;
            }
            let pc = self.t.pc();
            if to_breakpoint && (self.bp_at(pc) || self.phys_bp_at(pc)) && self.bp_check(pc) {
                return Stop::Breakpoint(pc);
            }
        }
        Stop::Step
    }
    /// Changes made outside the instructions (editing, reset, loading) end
    /// the way back.
    fn forget_history(&mut self) {
        self.rec.clear();
        self.rec_writes.clear();
    }
    pub fn rec_json(&self) -> String {
        format!("{{\"on\":{},\"depth\":{}}}", self.recording, self.rec.len())
    }
    /// A breakpoint was reached at `pc`: evaluate the conditions of every
    /// breakpoint there. It stops if any of them says so.
    fn bp_check(&mut self, pc: u32) -> bool {
        let phys = self.t.mem().translate(pc);
        let mut conds: Vec<BpCond> = Vec::new();
        let mut keys: Vec<Option<u32>> = Vec::new();
        if self.breakpoints.contains(&pc) {
            conds.push(self.bp_conds.get(&pc).cloned().unwrap_or_default());
            keys.push(None);
        }
        for b in &self.sym_bps {
            if b.targets.iter().any(|t| match *t {
                BpTarget::Logical(a) => a == pc,
                BpTarget::Phys(p) => Some(p) == phys,
            }) {
                conds.push(b.cond.clone());
                keys.push(Some(b.id));
            }
        }
        if conds.is_empty() {
            return true;
        }
        let mut stop = false;
        for (c, key) in conds.iter_mut().zip(&keys) {
            let action = self.bp_action(c);
            match key {
                None => {
                    if let Some(x) = self.bp_conds.get_mut(&pc) {
                        x.count = c.count;
                    }
                }
                Some(id) => {
                    if let Some(b) = self.sym_bps.iter_mut().find(|b| b.id == *id) {
                        b.cond.count = c.count;
                    }
                }
            }
            match action {
                BpAction::Go => {}
                BpAction::Stop => stop = true,
                BpAction::Log(t) => self.bp_log_line(t),
                BpAction::Error(e) => {
                    self.bp_log_line(tr!(format!("${pc:X} 条件のエラー: {e}"), format!("${pc:X} condition error: {e}")));
                    stop = true;
                }
            }
        }
        stop
    }
    fn bp_action(&self, c: &mut BpCond) -> BpAction {
        if c.is_plain() {
            c.count += 1;
            return BpAction::Stop;
        }
        let env = CondEnv { ice: self };
        if let Some((_, e)) = &c.cond {
            match e.eval(&env) {
                Ok(0) => return BpAction::Go,
                Ok(_) => {}
                Err(err) => return BpAction::Error(err),
            }
        }
        c.count += 1;
        if let Some((_, h)) = c.hits
            && !h.stops(c.count)
        {
            return BpAction::Go;
        }
        match &c.log {
            Some(m) => BpAction::Log(expr::log_text(m, &env)),
            None => BpAction::Stop,
        }
    }
    fn bp_log_line(&mut self, text: String) {
        self.dap.log(&format!("{text}\n"));
        self.bp_log.push_back((self.bp_log_seq, text));
        self.bp_log_seq += 1;
        while self.bp_log.len() > 200 {
            self.bp_log.pop_front();
        }
    }
    pub fn remove_sym_bps(&mut self, keep: impl Fn(&SymBp) -> bool) {
        self.sym_bps.retain(keep);
        self.rebuild_bps();
    }
    /// Load symbols (a listing, map or directory) and re-place them.
    pub fn load_symbols(&mut self, spec: &str) -> Result<Vec<usize>, String> {
        let r = self.symbols.load(spec)?;
        let tables = &self.symbols.tables;
        // Absolute symbols, for an OS layer that finds the kernel by name.
        self.os.use_symbols(&|name| {
            tables
                .iter()
                .filter(|t| t.module.is_none())
                .find_map(|t| t.symbols.iter().find(|s| s.1 == name).map(|s| s.0))
        });
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
            self.os_ready = self.os.detect(self.t.mem());
            self.os_slot = self.os.current_slot();
        }
        let images = if self.os_ready { self.os.images(self.t.mem()) } else { Vec::new() };
        if self.symbols.place(&images) {
            self.reresolve();
        }
    }
    /// The return address heuristic: the value follows a call instruction.
    pub fn is_return_address(&self, w: u32) -> bool {
        self.call_before(w, None)
    }
    /// Is `w` the address after a call (to `target`, when known)?
    pub(crate) fn call_before(&self, w: u32, target: Option<u32>) -> bool {
        let m = self.t.mem();
        T::CALL_LENS.iter().any(|&k| {
            let at = w.wrapping_sub(k);
            let l = self.t.disasm(&|a| m.peek(a), at, &|_, _| None);
            l.len == k
                && l.flow == Flow::Call
                && !l.text.starts_with("SWI")
                && !l.text.starts_with("TRAP")
                && !l.svc
                && (target.is_none() || l.target.is_none() || l.target == target)
        })
    }
    /// A return address stored on the stack.
    pub fn read_ret(&self, at: u32) -> u32 {
        let m = self.t.mem();
        if self.t.ret_size() == 4 { m.peek32(at) } else { u32::from(m.peek16(at)) }
    }
    /// Return addresses on the stack, innermost first: (stack address, value).
    pub fn call_chain(&self, max: usize) -> Vec<(u32, u32)> {
        self.call_chain_from(self.t.sp(), max)
    }
    /// The OS layer, once the OS has been found in memory.
    pub fn os(&self) -> Option<&dyn OsMonitor> {
        self.os_ready.then_some(&*self.os)
    }
    /// The OS layer for names and conventions, even before the OS runs.
    pub fn os_layer(&self) -> &dyn OsMonitor {
        &*self.os
    }
    fn current_proc(&self) -> Option<u32> {
        self.os().map(|o| o.current(self.t.mem()))
    }
    fn detect(&mut self) {
        if !self.os_ready && self.t.cycles() >= self.detect_at {
            self.os_ready = self.os.detect(self.t.mem());
            self.os_slot = self.os.current_slot();
            self.detect_at = self.t.cycles() + 200_000;
        }
    }
    /// Disassemble with the OS's service-request convention.
    pub fn dis(&self, addr: u32) -> target::Line {
        let m = self.t.mem();
        self.dis_with(&|a| m.peek(a), addr)
    }
    pub fn dis_with(&self, read: &dyn Fn(u32) -> u8, addr: u32) -> target::Line {
        let os = &*self.os;
        self.t.disasm(read, addr, &|read, at| {
            os.svc_at(read, at)
                .map(|s| (s.len, format!("{} {}", os.svc_mnemonic(), os.service_name(s.code)), u32::from(s.code)))
        })
    }

    /// Execute one instruction (or interrupt entry) with all debug checks.
    pub fn step_one(&mut self) -> Option<Stop> {
        self.detect();
        let executes = self.t.executes();
        let pc = self.t.pc();
        let skip = std::mem::take(&mut self.skip_checks);
        if !skip && (self.bp_at(pc) || self.phys_bp_at(pc)) && self.bp_check(pc) {
            return Some(Stop::Breakpoint(pc));
        }
        let tracing = self.trace_os9 && self.os_ready;
        let current = tracing.then(|| self.tracer.current());
        if let Goal::At { pc: target, s, proc_ } = self.goal
            && pc == target
            && self.t.sp() >= s
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
            && self.t.context() == task
            && (proc_.is_none() || self.current_proc() == proc_)
        {
            let here = self.symbols.line(self.t.mem(), pc);
            let other = here.is_some_and(|(t, l)| !(t == table && l.src == src && l.line == line));
            // Returned from the starting frame into code without line information.
            let returned = here.is_none() && left;
            if (other && (into || self.t.sp() >= s)) || returned {
                self.goal = Goal::None;
                return Some(Stop::Step);
            }
        }
        let mut stop = None;
        let mut return_insn = false;
        let mut rts = false;
        if executes {
            let m = self.t.mem();
            let mut bytes = [0u8; 10];
            for (i, b) in bytes.iter_mut().enumerate().take(T::MAX_INSN) {
                *b = m.peek(pc.wrapping_add(i as u32));
            }
            (rts, return_insn) = self.t.returns(&bytes);
            let pid = current.map_or(0, |c| c.1);
            // Decode from the bytes already fetched.
            let svc = if bytes.starts_with(self.svc_prefix) {
                self.os.svc_at(
                    &|a| bytes.get(a.wrapping_sub(pc) as usize).filter(|_| (a.wrapping_sub(pc) as usize) < T::MAX_INSN).map_or_else(|| m.peek(a), |&b| b),
                    pc,
                )
            } else {
                None
            };
            let svc = svc.map(|v| self.os.svc_regs(v, &self.t.cpu_state().regs));
            if svc.is_some_and(|s| Some(s.code) == self.sleep_code) {
                self.last_sleep = self.t.cycles();
            }
            // A new program (FUZIX execve) never returns from its call: place
            // its symbols when the CPU is back in the calling context.
            if svc.is_some_and(|s| self.os.image_calls().contains(&s.code)) {
                self.refresh_after = Some((self.t.context(), false));
            }
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
                cycle: self.t.cycles(),
                regs: self.t.snap(),
                bytes,
                pid,
            });
            if tracing && svc.is_some() {
                let state = self.t.cpu_state();
                self.tracer.before(self.t.mem(), &*self.os, &state);
            }
        }
        let stepped = if self.recording {
            self.step_recorded()
        } else {
            self.t.step(&self.watches)
        };
        match stepped {
            Err(e) => return Some(Stop::Error(e)),
            Ok(Some((addr, write, value))) => stop = Some(Stop::Watch { addr, write, value }),
            Ok(None) => {}
        }
        if let Some((ctx, left)) = self.refresh_after {
            let now = self.t.context();
            if now != ctx {
                self.refresh_after = Some((ctx, true));
            } else if left {
                self.refresh_after = None;
                self.refresh_symbols();
            }
        }
        if let Some(e) = self.t.disk_error() {
            return Some(Stop::Error(e.to_string()));
        }
        if tracing {
            let t = &self.t;
            let (done, switch) = self.tracer.after_with(t.mem(), &*self.os, self.os_slot, t.pc(), t.sp(), t.cycles(), || {
                t.cpu_state().regs
            });
            if let Some(id) = done
                && let Some(c) = self.tracer.call(id)
                && (self.break_returns.contains(&c.code) || (self.break_errors && c.error.is_some()))
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
                stop = stop.or(Some(Stop::Switch { from: s.from, to: s.to }));
            }
        }
        // The debugged code's own return (kernel code may also RTS on
        // another stack in the same task).
        if rts
            && let Goal::Line { s, proc_, .. } = self.goal
            && self.t.sp() > s
            && (proc_.is_none() || self.current_proc() == proc_)
            && self.symbols.locate(self.t.mem(), pc).is_some()
            && let Goal::Line { left, .. } = &mut self.goal
        {
            *left = true;
        }
        if let Goal::Out { s, proc_ } = self.goal
            && return_insn
            && self.t.sp() > s
            && (proc_.is_none() || self.current_proc() == proc_)
        {
            self.goal = Goal::None;
            stop = stop.or(Some(Stop::Step));
        }
        self.pump_console();
        stop
    }

    fn pump_console(&mut self) {
        let bytes = self.t.console_take();
        if !bytes.is_empty() {
            if let Some(t) = &mut self.telnet
                && let Some(m) = self.t.multicomp()
            {
                t.send(0, &bytes, m);
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
                self.next_input = self.t.cycles() + 100_000;
            } else if let Some((_, response)) = self.optional.iter().find(|(p, _)| self.seen.contains(p.as_str())) {
                // An optional prompt (`?PROMPT` in a script): answered
                // whenever it shows, without holding up the others.
                let response = response.clone();
                self.pending_input.extend(response.bytes());
                self.pending_input.push_back(13);
                self.seen.clear();
                self.next_input = self.t.cycles() + 100_000;
            }
            if self.seen.len() > 4096 {
                let cut = self.seen.len() - 2048;
                let cut = (cut..self.seen.len())
                    .find(|&i| self.seen.is_char_boundary(i))
                    .unwrap_or(0);
                self.seen.drain(..cut);
            }
        }
        if self.t.cycles() >= self.next_input
            && self.t.console_ready()
            && let Some(b) = self.pending_input.pop_front()
        {
            self.t.console_put(b);
            self.next_input = self.t.cycles() + 30_000;
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
        // A CPU on the FPGA keeps running until it is told to stop.
        if let Some(h) = self.t.hw()
            && h.running()
            && let Err(e) = h.stop()
        {
            self.bp_log_line(e);
        }
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
        match self.symbols.line(self.t.mem(), self.t.pc()) {
            Some((table, l)) => {
                let goal = Goal::Line {
                    table,
                    src: l.src,
                    line: l.line,
                    s: self.t.sp(),
                    proc_: self.current_proc(),
                    task: self.t.context(),
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
        if self.t.hw().is_some() {
            return self.run_hw(budget);
        }
        let start = Instant::now();
        let c0 = self.t.cycles();
        let limit = if self.speed == 0 {
            u64::MAX
        } else {
            (self.speed as f64 * budget.as_secs_f64()).max(1.0) as u64
        };
        self.apply_console_size();
        self.refresh_symbols();
        let mut n = 0u32;
        while self.running {
            // Waiting for an interrupt (68000 STOP): let the time pass in
            // real time instead of executing the wait, until a device or
            // scripted input can wake the CPU.
            if let Some(idle) = self.t.idle_cycles() {
                let hz = self.t.clock_hz().max(1);
                let mut cycles = idle;
                if !self.pending_input.is_empty() {
                    cycles = cycles.min(self.next_input.saturating_sub(self.t.cycles()).max(1));
                }
                let left = budget.saturating_sub(start.elapsed());
                let room = (left.as_secs_f64() * hz as f64) as u64;
                if room == 0 {
                    break;
                }
                let cycles = cycles.min(room);
                self.t.poll_io();
                self.t.skip_idle(cycles);
                std::thread::sleep(Duration::from_secs_f64(cycles as f64 / hz as f64));
                self.pump_console();
                continue;
            }
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
                if self.speed == 0 && self.idle_pace(start, budget) {
                    break;
                }
            }
            if self.t.cycles() - c0 >= limit {
                let rest = budget.saturating_sub(start.elapsed());
                std::thread::sleep(rest);
                break;
            }
        }
    }
    /// Run a CPU on the FPGA: it runs by itself with the breakpoints and
    /// watchpoints in hardware, and the ICE waits for it to stop. What
    /// needs every instruction (source-line steps, stepping out, OS call
    /// breaks, recording) steps it one instruction at a time instead.
    fn run_hw(&mut self, budget: Duration) {
        let start = Instant::now();
        self.apply_console_size();
        while self.running {
            let hw_running = self.t.hw().is_some_and(|h| h.running());
            if !hw_running {
                let free = matches!(self.goal, Goal::None | Goal::At { .. })
                    && !self.recording
                    && self.break_calls.is_empty()
                    && self.break_returns.is_empty()
                    && !self.break_errors
                    && !self.break_switch;
                if !free || self.skip_checks {
                    // one instruction on the board (also to leave a breakpoint)
                    if let Some(s) = self.step_one() {
                        self.halt(s);
                        return;
                    }
                    if !free {
                        if start.elapsed() >= budget {
                            return;
                        }
                        continue;
                    }
                }
                let mut logical: Vec<u32> = self.breakpoints.iter().copied().collect();
                let mut phys = Vec::new();
                for b in &self.sym_bps {
                    for t in &b.targets {
                        match *t {
                            BpTarget::Logical(a) => logical.push(a),
                            BpTarget::Phys(p) => phys.push(p),
                        }
                    }
                }
                if let Goal::At { pc, .. } = self.goal {
                    logical.push(pc);
                }
                logical.sort_unstable();
                logical.dedup();
                phys.sort_unstable();
                phys.dedup();
                let watches = self.watches.clone();
                let r = self.t.hw().map(|h| h.run(&logical, &phys, &watches));
                if let Some(Err(e)) = r {
                    self.halt(Stop::Error(e));
                    return;
                }
            }
            let left = budget.saturating_sub(start.elapsed()).max(Duration::from_millis(1));
            let r = self.t.hw().map(|h| h.wait(left.min(Duration::from_millis(20))));
            match r {
                Some(Err(e)) => {
                    self.halt(Stop::Error(e));
                    return;
                }
                Some(Ok(Some(stop))) => {
                    self.refresh_symbols();
                    let pc = self.t.pc();
                    match stop {
                        fpga::HwStop::Watch { addr, write, value } => {
                            self.halt(Stop::Watch { addr, write, value });
                            return;
                        }
                        fpga::HwStop::Breakpoint => {
                            if let Goal::At { pc: target, s, proc_ } = self.goal
                                && pc == target
                                && self.t.sp() >= s
                                && (proc_.is_none() || self.current_proc() == proc_)
                            {
                                self.halt(Stop::Step);
                                return;
                            }
                            if (self.bp_at(pc) || self.phys_bp_at(pc)) && self.bp_check(pc) {
                                self.halt(Stop::Breakpoint(pc));
                                return;
                            }
                            // a condition that does not hold, or not yet
                            // back in the calling frame: on again
                            self.skip_checks = true;
                        }
                        _ => {
                            self.halt(Stop::Pause);
                            return;
                        }
                    }
                }
                Some(Ok(None)) | None => {}
            }
            self.pump_console();
            self.poll_telnet();
            if start.elapsed() >= budget {
                return;
            }
        }
    }
    /// A guest that waits in a loop (the OS has nothing to run) runs at its
    /// nominal clock instead of flat out: every instruction still executes in
    /// the same order, only the host sleeps. Sampled every 1024 instructions;
    /// three quarters of the last 16 samples idle starts the pacing. Returns
    /// true when the slice's time is used up.
    fn idle_pace(&mut self, start: Instant, budget: Duration) -> bool {
        // Waiting: nothing to run, and a process gave the CPU up lately
        // (within about two clock ticks).
        let lately = self.t.cycles().saturating_sub(self.last_sleep) < 2 * self.t.clock_hz() / 50;
        let idle = lately && self.pending_input.is_empty() && self.os().is_some_and(|os| os.idle(self.t.mem()));
        self.idle_bits = self.idle_bits << 1 | u16::from(idle);
        if self.idle_bits.count_ones() < 12 {
            self.idle_mark = None;
            return false;
        }
        let now = Instant::now();
        let (t0, c0) = *self.idle_mark.get_or_insert((now, self.t.cycles()));
        let hz = self.t.clock_hz().max(1) as f64;
        let ahead = (self.t.cycles() - c0) as f64 / hz - now.duration_since(t0).as_secs_f64();
        if ahead <= 0.0 {
            return false;
        }
        let rest = budget.saturating_sub(start.elapsed());
        let nap = Duration::from_secs_f64(ahead).min(rest);
        std::thread::sleep(nap);
        nap >= rest
    }
    fn poll_telnet(&mut self) {
        self.t.poll_io();
        if let Some(t) = &mut self.telnet
            && let Some(m) = self.t.multicomp()
        {
            let _ = t.poll(m);
        }
    }

    fn step_over(&mut self) {
        let l = self.dis(self.t.pc());
        if l.flow == Flow::Call {
            let goal = Goal::At {
                pc: self.t.pc().wrapping_add(l.len),
                s: self.t.sp(),
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
            // The language of the ICE's text: new stop reasons, call
            // descriptions and explanations (recorded ones stay as they are).
            "/api/lang" => {
                match p("lang") {
                    Some("en") => lang::set_en(true),
                    Some("ja") => lang::set_en(false),
                    Some(_) => return bad("lang"),
                    None => {}
                }
                http::Response::json(format!("{{\"lang\":\"{}\"}}", lang::code()))
            }
            "/api/console/size" => {
                let (Some(rows), Some(cols)) = (p("rows").and_then(parse_num), p("cols").and_then(parse_num)) else {
                    return bad("rows/cols");
                };
                self.console_size = Some((rows.clamp(1, 999) as u16, cols.clamp(1, 999) as u16));
                self.apply_console_size();
                ok()
            }
            "/api/reset" => {
                self.reset_machine();
                ok()
            }
            "/api/machine/save" => {
                let path = p("path").unwrap_or("rs6809.state");
                let disk = p("disk").map(|d| d == "1");
                match self.save_machine(path, disk) {
                    Ok(n) => http::Response::json(format!("{{\"ok\":true,\"path\":{},\"bytes\":{n}}}", json_str(path))),
                    Err(e) => bad(&e),
                }
            }
            "/api/machine/load" => {
                let path = p("path").unwrap_or("rs6809.state");
                match self.load_machine(path) {
                    Ok(()) => ok(),
                    Err(e) => bad(&e),
                }
            }
            "/api/pause" => {
                if self.running {
                    self.halt(Stop::Pause);
                }
                ok()
            }
            // Recording for stepping back (on=1|0), and stepping back.
            "/api/record" => {
                if let Some(on) = p("on") {
                    self.recording = on == "1" || on == "true";
                    if !self.recording {
                        self.forget_history();
                    }
                }
                http::Response::json(self.rec_json())
            }
            "/api/stepback" => {
                let n = p("n").and_then(parse_num).unwrap_or(1).clamp(1, 1_000_000) as usize;
                let s = self.step_back(n, false);
                self.halt(s);
                ok()
            }
            "/api/reverse" => {
                let s = self.step_back(0, true);
                self.halt(s);
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
                    s: self.t.sp(),
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
                if !self.t.set_reg(name, v) {
                    return bad("register");
                }
                self.forget_history();
                ok()
            }
            "/api/bp" => {
                // Condition, hit count and log message (cond=, hits=, log=).
                let cond = if p("cond").is_some() || p("hits").is_some() || p("log").is_some() {
                    match BpCond::new(p("cond").unwrap_or(""), p("hits").unwrap_or(""), p("log").unwrap_or("")) {
                        Ok(c) => Some(c),
                        Err(e) => return bad(&e),
                    }
                } else {
                    None
                };
                if let Some(a) = p("addr").and_then(parse_addr) {
                    let on = match p("op") {
                        Some("del") => false,
                        Some("toggle") => !self.breakpoints.contains(&a),
                        _ => true,
                    };
                    self.set_bp(a, on);
                    let a = a & self.amask();
                    if on && let Some(c) = cond.clone() {
                        if c.is_plain() {
                            self.bp_conds.remove(&a);
                        } else {
                            self.bp_conds.insert(a, c);
                        }
                    }
                }
                // Change the condition of a symbolic breakpoint.
                if let (Some(id), Some(c)) = (p("id").and_then(parse_num), cond.clone()) {
                    self.set_sym_cond(id, c);
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
                    let id = self.add_sym_bp(0, spec);
                    if let Some(c) = cond {
                        self.set_sym_cond(id, c);
                    }
                }
                if let Some(id) = p("symdel").and_then(parse_num) {
                    self.remove_sym_bps(|b| b.id != id);
                }
                if p("op") == Some("clear") {
                    self.breakpoints.clear();
                    self.bp_conds.clear();
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
                        let end = start.saturating_add((len - 1).min(0xffff)) & self.amask();
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
                let addr = p("addr").and_then(parse_addr).unwrap_or(self.t.pc());
                let n = p("n").and_then(parse_num).unwrap_or(32).min(256);
                // Start a few instructions early by resynchronizing from before PC.
                let back = p("back").and_then(parse_num).unwrap_or(0).min(64);
                let start = self.resync(addr, back);
                http::Response::json(self.disasm_json(start, n as usize))
            }
            "/api/mem" => {
                let addr = p("addr").and_then(parse_addr).unwrap_or(0);
                let len = p("len").and_then(parse_num).unwrap_or(256).min(4096);
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
                        self.t.poke(addr.wrapping_add(i as u32), b);
                    }
                    self.forget_history();
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
                        self.t.console_put(match b {
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
            // Watch expressions: a C variable (when stopped in C code), else
            // an expression over registers, memory and symbols.
            "/api/eval" => {
                self.refresh_symbols();
                let e = p("expr").unwrap_or("").trim();
                if e.is_empty() {
                    return bad("expr");
                }
                if !self.running {
                    let frame = p("frame").and_then(parse_num).unwrap_or(0) as usize;
                    if let Some((table, ty, place)) = self.eval_var(frame, e)
                        && let Some(v) = self.with_ctx(table, |c| c.format(&ty, place))
                    {
                        return http::Response::json(format!("{{\"text\":{}}}", json_str(&v)));
                    }
                }
                match expr::Expr::parse(e).and_then(|x| x.eval(&CondEnv { ice: self })) {
                    Ok(v) => http::Response::json(format!("{{\"value\":{v}}}")),
                    Err(err) => http::Response::json(format!("{{\"error\":{}}}", json_str(&err))),
                }
            }
            // Find bytes in memory from addr on (pat: hex digits).
            "/api/mem/find" => {
                let from = p("addr").and_then(parse_addr).unwrap_or(0);
                let pat: Vec<u8> = p("pat")
                    .unwrap_or("")
                    .as_bytes()
                    .chunks(2)
                    .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
                    .collect();
                if pat.is_empty() {
                    return bad("pat");
                }
                let m = self.t.mem();
                let end = 1u64 << self.t.addr_bits();
                let mut found = None;
                let mut a = u64::from(from);
                while a + pat.len() as u64 <= end {
                    if pat.iter().enumerate().all(|(i, &b)| m.peek((a + i as u64) as u32) == b) {
                        found = Some(a);
                        break;
                    }
                    a += 1;
                }
                http::Response::json(format!("{{\"addr\":{}}}", found.map_or("null".into(), |a| a.to_string())))
            }
            "/api/resolve" => {
                self.refresh_symbols();
                match p("expr").and_then(|e| self.symbols.resolve(self.t.mem(), e)) {
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
                let n = p("n").and_then(parse_num).unwrap_or(32).clamp(1, 256);
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
    /// Mask of the CPU's address bits.
    pub fn amask(&self) -> u32 {
        ((1u64 << self.t.addr_bits()) - 1) as u32
    }

    fn resync(&self, addr: u32, back: u32) -> u32 {
        let mask = self.amask();
        if back == 0 {
            return addr;
        }
        // Instructions that ran (in the trace) are certain starts: decode on
        // from the earliest one in range that lands exactly on addr.
        let span = back * 4 * T::MAX_INSN as u32 / 2;
        let mut known: Vec<u32> = self
            .trace
            .iter()
            .map(|e| T::snap_pc(&e.regs))
            .filter(|&p| p != addr && addr.wrapping_sub(p) & mask <= span)
            .collect();
        // Labels usually start code too.
        let m = self.t.mem();
        known.extend((1..=span).map(|d| addr.wrapping_sub(d) & mask).filter(|&a| self.symbols.exact(m, a).is_some()));
        known.sort_by_key(|&p| std::cmp::Reverse(addr.wrapping_sub(p) & mask));
        known.dedup();
        for k in known {
            let mut starts = vec![k];
            let mut a = k;
            while a != addr && addr.wrapping_sub(a) & mask <= span {
                a = a.wrapping_add(self.dis(a).len.max(1)) & mask;
                starts.push(a);
            }
            if a == addr && starts.len() > back as usize {
                return starts[starts.len() - 1 - back as usize];
            }
        }
        // Otherwise try start points and keep one whose decode lands exactly on addr.
        for dist in (back..=back * 4).rev() {
            let mut a = addr.wrapping_sub(dist) & mask;
            let mut count = 0;
            while a != addr && addr.wrapping_sub(a) & mask <= dist {
                a = a.wrapping_add(self.dis(a).len.max(1)) & mask;
                count += 1;
            }
            if a == addr && count >= back {
                let mut a = addr.wrapping_sub(dist) & mask;
                for _ in 0..count - back {
                    a = a.wrapping_add(self.dis(a).len.max(1)) & mask;
                }
                return a;
            }
        }
        addr
    }

    /// Registers of a traced service request.
    fn call_regs_json(r: &CpuRegs) -> String {
        match r {
            CpuRegs::M6809(r) => Mc6809::snap_json(r),
            CpuRegs::M68k(r) => target::regs68k_json(&r.d, &r.a, r.sr, r.pc, r.a[7], r.a[7]),
        }
    }

    fn state_json(&self) -> String {
        let mut o = String::new();
        let stop = self
            .stop
            .as_ref()
            .map(|s| s.describe(&self.tracer, &*self.os))
            .unwrap_or_default();
        let _ = write!(
            o,
            "{{\"lang\":\"{}\",\"cpu\":\"{}\",\"machine\":{},\"clock\":{},\"osName\":{},\"running\":{},\"stop\":{},\"cycles\":{},\"speed\":{},\"wait\":{},\"regs\":{},",
            lang::code(),
            T::CPU,
            json_str(self.t.machine_name()),
            self.t.clock_hz(),
            json_str(self.os().and_then(|os| os.info().get("name").str().map(str::to_string)).as_deref().unwrap_or("")),
            self.running,
            json_str(&stop),
            self.t.cycles(),
            self.speed,
            self.t.wait_state(),
            self.t.regs_json()
        );
        let _ = write!(o, "\"mmu\":{},\"ram\":{},", self.t.mmu_json(), self.t.ram_len());
        let bps: Vec<String> = self.breakpoints.iter().map(|a| a.to_string()).collect();
        let sym_bps: Vec<String> = self
            .sym_bps
            .iter()
            .map(|b| {
                let at: Vec<String> = b
                    .targets
                    .iter()
                    .filter_map(|t| self.symbols.logical(self.t.mem(), *t))
                    .map(|a| a.to_string())
                    .collect();
                format!(
                    "{{\"id\":{},\"owner\":{},\"text\":{},\"resolved\":{},\"addrs\":[{}],{}}}",
                    b.id,
                    b.owner,
                    json_str(&b.text()),
                    b.targets.len(),
                    at.join(","),
                    b.cond.json()
                )
            })
            .collect();
        let _ = write!(o, "\"symBreakpoints\":[{}],\"symbols\":{},", sym_bps.join(","), self.symbols.tables.len());
        let conds: Vec<String> = self.bp_conds.iter().map(|(a, c)| format!("{{\"addr\":{a},{}}}", c.json())).collect();
        let log: Vec<String> = self
            .bp_log
            .iter()
            .rev()
            .take(50)
            .map(|(n, t)| format!("{{\"n\":{n},\"text\":{}}}", json_str(t)))
            .collect();
        let _ = write!(o, "\"bpConds\":[{}],\"bpLog\":[{}],\"record\":{},", conds.join(","), log.join(","), self.rec_json());
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
                let m = self.t.mem();
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

    fn disasm_json(&self, start: u32, n: usize) -> String {
        let m = self.t.mem();
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
                Some((ti, r)) if r.off == sym.locate(m, a).map_or(u32::MAX, |x| x.1) => {
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
                "{{\"addr\":{},\"bytes\":\"{}\",\"text\":{},\"bp\":{},\"phys\":{},\"note\":{},\"label\":{},\"tsym\":{},\"target\":{},\"src\":{},\"svc\":{}}}",
                a,
                bytes.join(" "),
                json_str(&l.text),
                self.bp_at(a) || self.phys_bp_at(a),
                m.translate(a).map_or(-1, |p| p as i64),
                json_str(explain),
                label,
                target.map_or("null".into(), |t| json_str(&t)),
                l.target.filter(|_| !l.svc && matches!(l.flow, target::Flow::Call | target::Flow::Jump | target::Flow::Branch)).map_or("null".into(), |t| t.to_string()),
                src,
                l.svc
            ));
            a = a.wrapping_add(l.len.max(1)) & self.amask();
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

    fn stack_json(&self, n: u32) -> String {
        let m = self.t.mem();
        let s = self.t.sp();
        let step = self.t.ret_size();
        let items: Vec<String> = (0..n)
            .map(|i| {
                let at = s.wrapping_add(i * step) & self.amask();
                let w = self.read_ret(at);
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
            self.t.pc(),
            self.symbols.label(m, self.t.pc()).map_or("null".into(), |l| json_str(&l)),
            items.join(","),
            chain.join(",")
        )
    }

    fn mem_json(&self, addr: u32, len: u32, space: &str) -> String {
        let m = self.t.mem();
        let bytes: Vec<u8> = match space {
            "phys" => {
                // Physical addresses use the 16-bit address as a page within
                // the bank given by `bank`.
                (0..len)
                    .map(|i| {
                        m.phys(addr as usize + i as usize).unwrap_or(0xff)
                    })
                    .collect()
            }
            // OS address spaces (OS-9: `sys`, `pidN`).
            space => match self.os() {
                Some(os) if space == "sys" || space.starts_with("pid") => (0..len)
                    .map(|i| os.read_space(m, space, addr.wrapping_add(i)).unwrap_or(0xff))
                    .collect(),
                None if space == "sys" => (0..len).map(|i| m.peek(addr.wrapping_add(i))).collect(),
                _ if space == "sys" || space.starts_with("pid") => vec![0xff; len as usize],
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
                let pc = T::snap_pc(&t.regs);
                let b = t.bytes;
                let l = self.dis_with(&|a| b.get(a.wrapping_sub(pc) as usize).copied().unwrap_or(0), pc);
                format!(
                    "{{\"cycle\":{},\"pid\":{},\"regs\":{},\"text\":{}}}",
                    t.cycle,
                    t.pid,
                    T::snap_json(&t.regs),
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
            .processes(self.t.mem())
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
            .images(self.t.mem())
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
            Some(os) => os.memory_map(self.t.mem()).to_string(),
            None => "{\"unit\":0,\"map\":[]}".into(),
        }
    }

    fn calls_json(&self, since: u64, limit: usize, filter: &dyn Fn(&osmon::Call) -> bool) -> String {
        // Kernel-internal requests are shown with their raw registers.
        let generic = |r: &CpuRegs| match r {
            CpuRegs::M6809(r) => format!("A=${:02X} B=${:02X} X=${:04X} Y=${:04X} U=${:04X}", r.a, r.b, r.x, r.y, r.u),
            CpuRegs::M68k(r) => format!(
                "D0=${:08X} D1=${:08X} A0=${:08X} A1=${:08X}",
                r.d[0], r.d[1], r.a[0], r.a[1]
            ),
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
                    Self::call_regs_json(&c.inp),
                    c.out.as_ref().map_or("null".into(), Self::call_regs_json),
                    json_str(&if c.args.is_empty() && c.system && !self.os.internal(c.code, 0) { generic(&c.inp) } else { c.args.clone() }),
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
    /// Reset the machine to its startup state (disk contents stay) and
    /// start the boot script over. Breakpoints and symbols stay.
    pub fn reset_machine(&mut self) {
        self.t.reset();
        self.actions = self.script.clone();
        self.forget_run(tr!("リセット", "reset"));
    }
    /// Save the machine state to a file. The disk is included unless its
    /// writes already go to a file (`disk` overrides).
    pub fn save_machine(&self, path: &str, disk: Option<bool>) -> Result<usize, String> {
        let data = self.t.save_state(disk.unwrap_or(!self.t.disk_persistent()))?;
        std::fs::write(path, &data).map_err(|e| format!("{path}: {e}"))?;
        Ok(data.len())
    }
    /// Continue from a saved state; the boot script is not replayed.
    pub fn load_machine(&mut self, path: &str) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        self.t.load_state(&data)?;
        self.actions.clear();
        self.forget_run(&tr!(format!("状態 {path} を読み込み"), format!("state {path} loaded")));
        Ok(())
    }
    /// Keep the console's size in the OS: re-applied after the OS changes it
    /// (FUZIX's login runs stty from .profile) or boots again.
    fn apply_console_size(&mut self) {
        let Some((rows, cols)) = self.console_size else { return };
        if !self.os_ready {
            return;
        }
        for (a, v) in self.os.console_size_writes(self.t.mem(), rows, cols) {
            self.t.poke(a, v);
        }
    }
    /// What was learned from the previous run no longer holds.
    fn forget_run(&mut self, why: &str) {
        self.tracer = Tracer::default();
        self.trace.clear();
        self.forget_history();
        self.goal = Goal::None;
        self.os_ready = false;
        self.os_slot = None;
        self.os.forget();
        self.detect_at = 0;
        self.pending_input.clear();
        self.seen.clear();
        self.next_input = 0;
        self.var_refs.clear();
        let _ = self.t.console_take();
        let note = format!("\r\n[rs6809: {why}]\r\n");
        self.console.extend_from_slice(note.as_bytes());
        self.refresh_symbols();
        if !self.running {
            self.stop = Some(Stop::Pause);
        }
    }
    /// Keep executing while `running`, at most `max_steps` instructions.
    pub fn run_steps(&mut self, max_steps: u64) {
        self.apply_console_size();
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
