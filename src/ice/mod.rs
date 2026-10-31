//! In-circuit-emulator style debugger with a browser UI and NitrOS-9 awareness.
//!
//! The emulator thread owns the CPU and machine. The HTTP threads forward API
//! requests over a channel, so inspection always sees a consistent state.
pub mod disasm;
pub mod http;
pub mod os9;
mod tables;

use crate::cpu::I;
use crate::telnet::TelnetServer;
use crate::{Bus, Cpu, Machine};
use disasm::{Flow, disassemble};
use os9::{Level, Regs, Tracer, View};
use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::io;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
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
    fn describe(&self, t: &Tracer) -> String {
        let call = |id: &u64| {
            t.call(*id)
                .map(|c| format!("{} (プロセス{})", os9::service_name(c.code), c.pid))
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
                    os9::service_name(*code)
                )
            }
            Stop::SysReturn(id) => {
                let err = t.call(*id).and_then(|c| c.error).map(os9::error_name);
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
    level: Option<Level>,
    detect_at: u64,
    /// Guest cycles per host second; 0 runs at full speed.
    pub speed: u64,
    actions: VecDeque<(String, String)>,
    seen: String,
    pending_input: VecDeque<u8>,
    next_input: u64,
    echo_console: bool,
    telnet: Option<TelnetServer>,
}

impl Ice {
    pub fn new(cpu: Cpu, machine: Machine, options: Options) -> Self {
        Self {
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
            level: None,
            detect_at: 0,
            speed: 0,
            actions: options.actions,
            seen: String::new(),
            pending_input: VecDeque::new(),
            next_input: 0,
            echo_console: options.echo_console,
            telnet: None,
        }
    }
    pub fn set_telnet(&mut self, server: TelnetServer) {
        self.telnet = Some(server);
    }
    fn set_bp(&mut self, a: u16, on: bool) {
        let (w, b) = (usize::from(a >> 6), a & 63);
        if on {
            self.bp_bits[w] |= 1 << b;
            self.breakpoints.insert(a);
        } else {
            self.bp_bits[w] &= !(1 << b);
            self.breakpoints.remove(&a);
        }
    }
    fn bp_at(&self, a: u16) -> bool {
        self.bp_bits[usize::from(a >> 6)] & (1 << (a & 63)) != 0
    }
    pub fn view(&self) -> Option<View<'_>> {
        self.level.map(|level| View::new(&self.machine, level))
    }
    fn current_proc(&self) -> Option<u16> {
        self.view().map(|v| v.current())
    }
    fn detect(&mut self) {
        if self.level.is_none() && self.cpu.cycles >= self.detect_at {
            self.level = View::detect(&self.machine).map(|v| v.level);
            self.detect_at = self.cpu.cycles + 200_000;
        }
    }

    /// Execute one instruction (or interrupt entry) with all debug checks.
    pub fn step_one(&mut self) -> Option<Stop> {
        self.detect();
        let irq = self.machine.irq();
        let wait = self.cpu.waiting();
        let executes = (wait == 0 || (wait == 1 && irq)) && !(irq && self.cpu.cc & I == 0);
        let pc = self.cpu.pc;
        let skip = std::mem::take(&mut self.skip_checks);
        if !skip && self.bp_at(pc) {
            return Some(Stop::Breakpoint(pc));
        }
        let os9 = if self.trace_os9 { self.level } else { None };
        let desc = os9.map(|level| View::new(&self.machine, level).current());
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
        let mut stop = None;
        let mut return_insn = false;
        if executes {
            let m = &self.machine;
            let bytes = [0u16, 1, 2, 3, 4].map(|i| m.peek(pc.wrapping_add(i)));
            return_insn =
                bytes[0] == 0x39 || bytes[0] == 0x3b || (bytes[0] == 0x35 && bytes[1] & 0x80 != 0);
            let pid = match (os9, desc) {
                (Some(level), Some(d)) => View::new(m, level).pid_of(d),
                _ => 0,
            };
            if bytes[0] == 0x10 && bytes[1] == 0x3f && !skip {
                let code = bytes[2];
                if self.break_calls.contains(&code) && self.break_pid.is_none_or(|p| p == pid) {
                    return Some(Stop::SysCall { code, pid });
                }
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
            if let Some(level) = os9
                && bytes[0] == 0x10
                && bytes[1] == 0x3f
            {
                let v = View::new(&self.machine, level);
                self.tracer.before(&self.machine, &v, &self.cpu);
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
        if let Some(level) = os9 {
            let v = View::new(&self.machine, level);
            let (done, switch) = self.tracer.after(&self.machine, &v, &self.cpu);
            if let Some(id) = done
                && let Some(c) = self.tracer.call(id)
                && (self.break_returns.contains(&c.code)
                    || (self.break_errors && c.error.is_some()))
                && self.break_pid.is_none_or(|p| p == c.pid)
            {
                stop = stop.or(Some(Stop::SysReturn(id)));
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
        self.running = true;
        self.stop = None;
        self.skip_checks = true;
    }
    fn halt(&mut self, s: Stop) {
        self.running = false;
        self.goal = Goal::None;
        self.stop = Some(s);
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
        let m = &self.machine;
        let l = disassemble(&|a| m.peek(a), self.cpu.pc);
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
                        Some("toggle") => !self.bp_at(a),
                        _ => true,
                    };
                    self.set_bp(a, on);
                }
                if p("op") == Some("clear") {
                    for a in std::mem::take(&mut self.breakpoints) {
                        self.set_bp(a, false);
                    }
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
            "/api/os9break" => {
                if let Some(c) = p("code") {
                    let code = os9::service_code(c).or_else(|| parse_addr(c).map(|v| v as u8));
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
            "/api/os9/procs" => http::Response::json(self.procs_json()),
            "/api/os9/modules" => http::Response::json(self.modules_json()),
            "/api/os9/memmap" => http::Response::json(self.memmap_json()),
            "/api/os9/calls" => {
                let since = p("since").and_then(parse_num).map_or(0, u64::from);
                let limit = p("limit").and_then(parse_num).unwrap_or(2000) as usize;
                let user = p("user") == Some("1");
                let parent = p("parent").and_then(parse_num).map(u64::from);
                let pid = p("pid").and_then(parse_num).map(|n| n as u8);
                let f = |c: &os9::Call| {
                    (!user || !c.system)
                        && parent.is_none_or(|q| c.parent == Some(q))
                        && pid.is_none_or(|q| c.pid == q)
                };
                http::Response::json(self.calls_json(since, limit, &f))
            }
            "/api/os9/services" => http::Response::json(Self::services_json()),
            _ => http::Response::error(404, "unknown API"),
        }
    }

    /// Find an instruction boundary about `back` instructions before `addr`.
    fn resync(&self, addr: u16, back: u16) -> u16 {
        if back == 0 {
            return addr;
        }
        let m = &self.machine;
        let read = |a| m.peek(a);
        // Try start points and keep one whose decode lands exactly on addr.
        for dist in (back..=back * 4).rev() {
            let mut a = addr.wrapping_sub(dist);
            let mut count = 0;
            while a != addr && addr.wrapping_sub(a) <= dist {
                a = a.wrapping_add(disassemble(&read, a).len.max(1));
                count += 1;
            }
            if a == addr && count >= back {
                let mut a = addr.wrapping_sub(dist);
                for _ in 0..count - back {
                    a = a.wrapping_add(disassemble(&read, a).len.max(1));
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
            .map(|s| s.describe(&self.tracer))
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
                .map(|c| json_str(&os9::service_name(*c)))
                .collect::<Vec<_>>()
                .join(",")
        };
        let _ = write!(
            o,
            "\"breakpoints\":[{}],\"watches\":[{}],\"os9break\":{{\"calls\":[{}],\"returns\":[{}],\"errors\":{},\"switch\":{},\"pid\":{},\"trace\":{}}},",
            bps.join(","),
            ws.join(","),
            names(&self.break_calls),
            names(&self.break_returns),
            self.break_errors,
            self.break_switch,
            self.break_pid.unwrap_or(0),
            self.trace_os9
        );
        match self.view() {
            Some(v) => {
                let cur = v.current();
                let [aq, wq, sq] = v.queues();
                let list = |q: &[u8]| {
                    q.iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let _ = write!(
                    o,
                    "\"os9\":{{\"level\":{},\"pid\":{},\"active\":[{}],\"waiting\":[{}],\"sleeping\":[{}],\"callVersion\":{},\"calls\":{},\"switches\":{}}},",
                    if v.level == Level::One { 1 } else { 2 },
                    v.pid_of(cur),
                    list(&aq),
                    list(&wq),
                    list(&sq),
                    self.tracer.version,
                    self.tracer.last_id(),
                    self.tracer.switches.len()
                );
            }
            None => o.push_str("\"os9\":null,"),
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
        let read = |a| m.peek(a);
        let mut a = start;
        let mut items = Vec::new();
        for _ in 0..n {
            let l = disassemble(&read, a);
            let bytes: Vec<String> = (0..l.len)
                .map(|i| format!("{:02X}", m.peek(a.wrapping_add(i))))
                .collect();
            let explain = if l.text.starts_with("OS9 ") {
                l.target.map(|c| os9::explain(c as u8)).unwrap_or("")
            } else {
                ""
            };
            items.push(format!(
                "{{\"addr\":{},\"bytes\":\"{}\",\"text\":{},\"bp\":{},\"phys\":{},\"note\":{}}}",
                a,
                bytes.join(" "),
                json_str(&l.text),
                self.bp_at(a),
                m.translate(a).map_or(-1, |p| p as i64),
                json_str(explain)
            ));
            a = a.wrapping_add(l.len.max(1));
        }
        format!("{{\"lines\":[{}]}}", items.join(","))
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
            "sys" => match self.view() {
                Some(v) => (0..len).map(|i| v.sys(addr.wrapping_add(i))).collect(),
                None => (0..len).map(|i| m.peek(addr.wrapping_add(i))).collect(),
            },
            s if s.starts_with("pid") => {
                let pid = s[3..].parse::<u8>().unwrap_or(0);
                match self.view().and_then(|v| {
                    v.processes()
                        .into_iter()
                        .find(|p| p.id == pid)
                        .map(|p| (v, p.addr))
                }) {
                    Some((v, d)) => (0..len)
                        .map(|i| v.proc_read(d, addr.wrapping_add(i)))
                        .collect(),
                    None => vec![0xff; usize::from(len)],
                }
            }
            _ => (0..len).map(|i| m.peek(addr.wrapping_add(i))).collect(),
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
                let l = disassemble(
                    &|a| b.get(usize::from(a.wrapping_sub(pc))).copied().unwrap_or(0),
                    pc,
                );
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
        let Some(v) = self.view() else {
            return "{\"procs\":[]}".into();
        };
        let items: Vec<String> = v
            .processes()
            .iter()
            .map(|p| {
                let paths: Vec<String> = p.paths.iter().map(|x| x.to_string()).collect();
                let dat: Vec<String> = p.dat.iter().map(|x| x.to_string()).collect();
                let ticks = p.ticks.map_or("null".into(), |(u, s, f, i)| {
                    format!("{{\"user\":{u},\"system\":{s},\"fcalls\":{f},\"icalls\":{i}}}")
                });
                format!(
                    "{{\"id\":{},\"addr\":{},\"parent\":{},\"sibling\":{},\"child\":{},\"sp\":{},\"task\":{},\"pages\":{},\"user\":{},\"prior\":{},\"age\":{},\"state\":{},\"queue\":\"{}\",\"module\":{},\"signal\":{},\"paths\":[{}],\"dat\":[{}],\"ticks\":{}}}",
                    p.id, p.addr, p.parent, p.sibling, p.child, p.sp,
                    p.task.map_or("null".into(), |t| t.to_string()),
                    p.pages, p.user, p.prior, p.age, p.state, p.queue,
                    json_str(&p.module), p.signal, paths.join(","), dat.join(","), ticks
                )
            })
            .collect();
        format!("{{\"procs\":[{}]}}", items.join(","))
    }

    fn modules_json(&self) -> String {
        let Some(v) = self.view() else {
            return "{\"modules\":[]}".into();
        };
        let items: Vec<String> = v
            .modules()
            .iter()
            .map(|m| {
                let blocks: Vec<String> = m.blocks.iter().map(|b| b.to_string()).collect();
                format!(
                    "{{\"name\":{},\"addr\":{},\"size\":{},\"type\":{},\"revs\":{},\"links\":{},\"blocks\":[{}]}}",
                    json_str(&m.name), m.addr, m.size, m.kind, m.revs, m.links, blocks.join(",")
                )
            })
            .collect();
        format!("{{\"modules\":[{}]}}", items.join(","))
    }

    fn memmap_json(&self) -> String {
        let Some(v) = self.view() else {
            return "{\"unit\":0,\"map\":[]}".into();
        };
        let (unit, map) = v.memory_map();
        let cells: Vec<String> = map.iter().map(|b| b.to_string()).collect();
        let sys: Vec<String> = if v.level == Level::Two {
            v.sys_dat().iter().map(|b| b.to_string()).collect()
        } else {
            Vec::new()
        };
        format!(
            "{{\"unit\":{unit},\"map\":[{}],\"sysdat\":[{}]}}",
            cells.join(","),
            sys.join(",")
        )
    }

    fn calls_json(&self, since: u64, limit: usize, filter: &dyn Fn(&os9::Call) -> bool) -> String {
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
                    json_str(&os9::service_name(c.code)),
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
                        .map(|(code, n)| format!("[{},{n}]", json_str(&os9::service_name(*code))))
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

    fn services_json() -> String {
        let items: Vec<String> = tables::SERVICES
            .iter()
            .map(|(c, n, d)| {
                format!(
                    "{{\"code\":{c},\"name\":{},\"desc\":{},\"explain\":{}}}",
                    json_str(n),
                    json_str(d),
                    json_str(os9::explain(*c))
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

    /// Serve the UI on `listener` and run until the process is terminated.
    pub fn serve(mut self, listener: TcpListener, start_running: bool) -> io::Result<()> {
        let (tx, rx): (_, Receiver<http::Request>) = mpsc::channel();
        http::start(listener, tx, UI);
        if start_running {
            self.resume(Goal::None);
        }
        loop {
            if self.running {
                self.run_slice(Duration::from_millis(8));
                while let Ok(req) = rx.try_recv() {
                    let r = self.handle(&req);
                    let _ = req.reply.send(r);
                }
            } else {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(req) => {
                        let r = self.handle(&req);
                        let _ = req.reply.send(r);
                    }
                    Err(RecvTimeoutError::Timeout) => self.poll_telnet(),
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
        }
    }
}
