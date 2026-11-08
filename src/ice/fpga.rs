//! The ICE on real hardware: the 6809 and 68000 machines of rs6809-fpga
//! (Tang Nano 20K), over the board's USB serial port or a TCP connection
//! (the rs6809-fpga simulation, or a serial-to-TCP bridge).
//!
//! The board's serial port carries the guest console and the debugger at
//! once (rs6809-fpga `common/dbg_link.v`): console bytes as they are, DLE
//! (0x10) doubled, and `DLE code len data...` frames for commands,
//! responses and events. Between stops the CPU runs at full speed on the
//! FPGA, with breakpoints and watchpoints in hardware; while stopped, the
//! ICE reads registers and memory through the debug unit. Stepping is done
//! by the hardware one instruction at a time, so the features that look at
//! every instruction (the OS call record, recording for stepping back,
//! source-line steps) work, slowly, by stepping.
use super::osmon::{CpuState, Mem, Regs};
use super::target::{Board68k, Line, Mc68000, SvcHook, Target, WatchHit};
use super::{Watch, WatchKind};
use crate::m68k::state::{Reader, Writer};
use crate::m68k::{Bus68k, M68k};
use crate::tr;
use crate::Cpu;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const DLE: u8 = 0x10;
/// How long a command may take to be answered.
const REPLY: Duration = Duration::from_secs(3);

/// Why the CPU on the board stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwStop {
    Halt,
    Breakpoint,
    Step,
    Watch { addr: u32, write: bool, value: u8 },
    Reset,
}

/// A target that runs by itself on hardware between stops.
pub trait Hw {
    /// Run with these breakpoints (CPU addresses and physical RAM
    /// addresses) and watchpoints.
    fn run(&mut self, bps: &[u32], phys: &[usize], watches: &[Watch]) -> Result<(), String>;
    /// Wait up to `d` for the CPU to stop.
    fn wait(&mut self, d: Duration) -> Result<Option<HwStop>, String>;
    /// Stop the running CPU.
    fn stop(&mut self) -> Result<(), String>;
    fn running(&self) -> bool;
}

#[derive(Default)]
struct LinkState {
    console: Vec<u8>,
    frames: VecDeque<(u8, Vec<u8>)>,
    closed: Option<String>,
}

/// The serial line to the board.
pub struct Link {
    out: Mutex<Box<dyn Write + Send>>,
    st: Arc<(Mutex<LinkState>, Condvar)>,
}

impl Link {
    /// `tcp:HOST:PORT`, or a serial device (set to `baud` with stty).
    pub fn open(spec: &str, baud: u32) -> Result<Link, String> {
        let (r, w): (Box<dyn Read + Send>, Box<dyn Write + Send>) = if let Some(addr) = spec.strip_prefix("tcp:") {
            let s = std::net::TcpStream::connect(addr).map_err(|e| format!("{addr}: {e}"))?;
            let _ = s.set_nodelay(true);
            let r = s.try_clone().map_err(|e| e.to_string())?;
            (Box::new(r), Box::new(s))
        } else {
            // The serial line's speed and raw mode, with the system's stty
            // (no FFI): Linux takes -F, macOS and the BSDs -f.
            let mut ok = false;
            for flag in ["-F", "-f"] {
                let st = std::process::Command::new("stty")
                    .args([flag, spec, &baud.to_string(), "raw", "-echo", "-ixon", "-ixoff", "-crtscts", "clocal"])
                    .stderr(std::process::Stdio::null())
                    .status();
                if st.is_ok_and(|s| s.success()) {
                    ok = true;
                    break;
                }
            }
            if !ok {
                eprintln!("rs6809: could not set {spec} to {baud} baud with stty; using it as it is");
            }
            let f = std::fs::OpenOptions::new().read(true).write(true).open(spec).map_err(|e| format!("{spec}: {e}"))?;
            let r = f.try_clone().map_err(|e| e.to_string())?;
            (Box::new(r), Box::new(f))
        };
        let st = Arc::new((Mutex::new(LinkState::default()), Condvar::new()));
        let st2 = st.clone();
        std::thread::spawn(move || reader(r, st2));
        Ok(Link { out: Mutex::new(w), st })
    }
    fn send(&self, bytes: &[u8]) -> Result<(), String> {
        let mut o = self.out.lock().unwrap();
        o.write_all(bytes).and_then(|_| o.flush()).map_err(|e| e.to_string())
    }
    /// A command and its response (events that come meanwhile stay queued).
    pub fn cmd(&self, code: u8, data: &[u8]) -> Result<(u8, Vec<u8>), String> {
        let mut f = vec![DLE, code, data.len() as u8];
        f.extend_from_slice(data);
        self.send(&f)?;
        let (m, cv) = &*self.st;
        let mut s = m.lock().unwrap();
        let until = Instant::now() + REPLY;
        loop {
            if let Some(i) = s.frames.iter().position(|(c, _)| *c != b'S') {
                return Ok(s.frames.remove(i).unwrap());
            }
            if let Some(e) = &s.closed {
                return Err(e.clone());
            }
            let now = Instant::now();
            if now >= until {
                return Err(tr!(
                    format!("FPGAから応答がない（コマンド {}）", code as char),
                    format!("no answer from the FPGA (command {})", code as char)
                ));
            }
            s = cv.wait_timeout(s, until - now).unwrap().0;
        }
    }
    /// A command that must answer 'k'.
    fn ok(&self, code: u8, data: &[u8]) -> Result<(), String> {
        match self.cmd(code, data)? {
            (b'k', _) => Ok(()),
            (c, _) => Err(tr!(
                format!("FPGAがコマンド {} を断った（{}）", code as char, c as char),
                format!("the FPGA refused command {} ({})", code as char, c as char)
            )),
        }
    }
    /// The next stop event, waiting up to `d`.
    pub fn event(&self, d: Duration) -> Result<Option<Vec<u8>>, String> {
        let (m, cv) = &*self.st;
        let mut s = m.lock().unwrap();
        let until = Instant::now() + d;
        loop {
            if let Some(i) = s.frames.iter().position(|(c, _)| *c == b'S') {
                return Ok(Some(s.frames.remove(i).unwrap().1));
            }
            if let Some(e) = &s.closed {
                return Err(e.clone());
            }
            let now = Instant::now();
            if now >= until {
                return Ok(None);
            }
            s = cv.wait_timeout(s, until - now).unwrap().0;
        }
    }
    pub fn console_take(&self) -> Vec<u8> {
        std::mem::take(&mut self.st.0.lock().unwrap().console)
    }
    pub fn console_send(&self, bytes: &[u8]) {
        let mut f = Vec::with_capacity(bytes.len() * 2);
        for &b in bytes {
            f.push(b);
            if b == DLE {
                f.push(DLE);
            }
        }
        let _ = self.send(&f);
    }
    /// Read memory: `n` bytes (1-256) from `addr` in `space`.
    fn read(&self, space: u8, addr: u32, n: usize) -> Result<Vec<u8>, String> {
        let [_, a2, a1, a0] = addr.to_be_bytes();
        match self.cmd(b'M', &[space, a2, a1, a0, n as u8])? {
            (b'm', d) if d.len() == n => Ok(d),
            (c, _) => Err(format!("memory read refused ({})", c as char)),
        }
    }
    fn write(&self, space: u8, addr: u32, data: &[u8]) -> Result<(), String> {
        for (i, chunk) in data.chunks(200).enumerate() {
            let [_, a2, a1, a0] = (addr + 200 * i as u32).to_be_bytes();
            let mut p = vec![space, a2, a1, a0];
            p.extend_from_slice(chunk);
            self.ok(b'P', &p)?;
        }
        Ok(())
    }
}

fn reader(mut r: Box<dyn Read + Send>, st: Arc<(Mutex<LinkState>, Condvar)>) {
    enum S {
        Data,
        Esc,
        Len(u8),
        Body(u8, usize, Vec<u8>),
    }
    let mut s = S::Data;
    let mut buf = [0u8; 4096];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => {
                st.0.lock().unwrap().closed = Some(tr!("FPGAとの接続が切れた", "the connection to the FPGA closed").into());
                st.1.notify_all();
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                st.0.lock().unwrap().closed = Some(e.to_string());
                st.1.notify_all();
                return;
            }
        };
        let mut g = st.0.lock().unwrap();
        for &b in &buf[..n] {
            s = match s {
                S::Data if b == DLE => S::Esc,
                S::Data => {
                    g.console.push(b);
                    S::Data
                }
                S::Esc if b == DLE => {
                    g.console.push(DLE);
                    S::Data
                }
                S::Esc => S::Len(b),
                S::Len(code) if b == 0 => {
                    g.frames.push_back((code, Vec::new()));
                    S::Data
                }
                S::Len(code) => S::Body(code, b as usize, Vec::with_capacity(b as usize)),
                S::Body(code, n, mut d) => {
                    d.push(b);
                    if d.len() == n {
                        g.frames.push_back((code, d));
                        S::Data
                    } else {
                        S::Body(code, n, d)
                    }
                }
            };
        }
        drop(g);
        st.1.notify_all();
    }
}

/// Memory read through the debug unit, kept while the CPU stays stopped.
struct Cache {
    link: Arc<Link>,
    lines: RefCell<HashMap<(u8, u32), [u8; 64]>>,
}
impl Cache {
    fn get(&self, space: u8, addr: u32) -> u8 {
        let line = addr & !63;
        if let Some(l) = self.lines.borrow().get(&(space, line)) {
            return l[(addr & 63) as usize];
        }
        match self.link.read(space, line, 64) {
            Ok(d) => {
                let mut l = [0u8; 64];
                l.copy_from_slice(&d);
                self.lines.borrow_mut().insert((space, line), l);
                l[(addr & 63) as usize]
            }
            // refused while the CPU runs: try again next time
            Err(_) => 0xff,
        }
    }
    fn clear(&self) {
        self.lines.borrow_mut().clear();
    }
    fn put(&self, space: u8, addr: u32, v: u8) -> Result<(), String> {
        self.link.write(space, addr, &[v])?;
        self.lines.borrow_mut().retain(|k, _| k.1 != addr & !63);
        Ok(())
    }
}

fn ident(link: &Link, cpu: u8) -> Result<usize, String> {
    match link.cmd(b'I', &[])? {
        (b'i', d) if d.len() >= 6 && d[0] == cpu => Ok(usize::from(u16::from_le_bytes([d[4], d[5]])) * 1024),
        (b'i', d) if !d.is_empty() => Err(tr!(
            format!("FPGAの機械が違う（CPU ${:02X}）", d[0]),
            format!("the FPGA has another machine (CPU ${:02X})", d[0])
        )),
        _ => Err(tr!("FPGAの識別に失敗した", "the FPGA did not identify itself").into()),
    }
}

fn watch_mode(k: WatchKind) -> u8 {
    match k {
        WatchKind::Read => 1,
        WatchKind::Write => 2,
        WatchKind::Access => 3,
    }
}

// ======================================================================
// 6809 (Multicomp09 on cpu09p)
// ======================================================================

/// The 6809 machine's memory: CPU addresses through its MMU (space 0),
/// physical RAM (space 1) and the MMU registers (space 2), read when the
/// CPU stopped.
pub struct Mem09 {
    cache: Cache,
    ram: usize,
    /// maps 0-15, mapper, flags (bit 0 fixed top, bit 1 ROM off)
    mmu: [u8; 18],
}
impl Mem09 {
    fn enabled(&self) -> bool {
        self.mmu[16] & 0x20 != 0
    }
    fn fixed_top(&self) -> bool {
        self.mmu[17] & 1 != 0
    }
    fn rom_off(&self) -> bool {
        self.mmu[17] & 2 != 0
    }
    fn task(&self) -> u8 {
        (self.mmu[16] >> 6) & 1
    }
    fn physical(&self, addr: u16) -> Option<usize> {
        if !self.enabled() || (self.fixed_top() && addr >= 0xfe00) {
            return Some(usize::from(addr)).filter(|&p| p < self.ram);
        }
        let e = self.mmu[usize::from(addr >> 13) | if self.task() != 0 { 8 } else { 0 }];
        let p = (usize::from(e & 0x7f) << 13) | (usize::from(addr) & 8191);
        (p < self.ram).then_some(p)
    }
}
impl Mem for Mem09 {
    fn peek(&self, addr: u32) -> u8 {
        self.cache.get(0, addr & 0xffff)
    }
    fn translate(&self, addr: u32) -> Option<usize> {
        let a = addr as u16;
        if (0xffd0..=0xffdf).contains(&a) || (a >= 0xe000 && !self.rom_off()) {
            return None;
        }
        self.physical(a)
    }
    fn logical(&self, phys: usize) -> Option<u32> {
        (0..8u16).find_map(|slot| {
            let base = self.translate(u32::from(slot << 13))?;
            (phys >= base && phys < base + 8192).then(|| u32::from(slot << 13) | (phys - base) as u32)
        })
    }
    fn phys(&self, p: usize) -> Option<u8> {
        (p < self.ram).then(|| self.cache.get(1, p as u32))
    }
    fn ram_size(&self) -> usize {
        self.ram
    }
}

/// The 6809 machine on the FPGA.
pub struct Fpga6809 {
    link: Arc<Link>,
    mem: Mem09,
    cpu: Cpu,
    dirty: bool,
    running: bool,
    t0: Instant,
    bps: Vec<u32>,
}

impl Fpga6809 {
    pub fn connect(spec: &str, baud: u32) -> Result<Self, String> {
        let link = Arc::new(Link::open(spec, baud)?);
        let ram = ident(&link, 0x09)?;
        let mut t = Self {
            mem: Mem09 {
                cache: Cache { link: link.clone(), lines: RefCell::new(HashMap::new()) },
                ram,
                mmu: [0; 18],
            },
            link,
            cpu: Cpu::default(),
            dirty: false,
            running: true,
            t0: Instant::now(),
            bps: Vec::new(),
        };
        // stop it, to start from a known state
        match t.link.cmd(b'Q', &[])? {
            (b'q', d) if d.first() == Some(&1) => t.running = false,
            _ => {
                t.link.ok(b'H', &[])?;
                t.wait_stop(REPLY)?.ok_or_else(|| tr!("FPGAのCPUが止まらない", "the CPU on the FPGA does not stop"))?;
            }
        }
        t.refresh()?;
        Ok(t)
    }
    fn refresh(&mut self) -> Result<(), String> {
        self.mem.cache.clear();
        let r = match self.link.cmd(b'R', &[])? {
            (b'r', d) if d.len() == 14 => d,
            _ => return Err("register read failed".into()),
        };
        let w = |i: usize| u16::from_be_bytes([r[i], r[i + 1]]);
        let c = &mut self.cpu;
        c.cc = r[0];
        c.a = r[1];
        c.b = r[2];
        c.dp = r[3];
        c.x = w(4);
        c.y = w(6);
        c.u = w(8);
        c.s = w(10);
        c.pc = w(12);
        c.cycles = self.t0.elapsed().as_micros() as u64;
        let m = self.link.read(2, 0, 18)?;
        self.mem.mmu.copy_from_slice(&m);
        self.dirty = false;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let c = &self.cpu;
        let regs: [(u8, u16); 9] = [
            (0, u16::from(c.cc)),
            (1, u16::from(c.a)),
            (2, u16::from(c.b)),
            (3, u16::from(c.dp)),
            (4, c.x),
            (5, c.y),
            (6, c.u),
            (7, c.s),
            (8, c.pc),
        ];
        for (sel, v) in regs {
            let [h, l] = v.to_be_bytes();
            self.link.ok(b'W', &[sel, h, l])?;
        }
        self.dirty = false;
        Ok(())
    }
    fn wait_stop(&mut self, d: Duration) -> Result<Option<HwStop>, String> {
        let Some(e) = self.link.event(d)? else { return Ok(None) };
        self.running = false;
        self.refresh()?;
        Ok(Some(match e.first() {
            Some(2) => HwStop::Breakpoint,
            Some(3) => HwStop::Step,
            Some(4) if e.len() >= 7 => HwStop::Watch {
                addr: u32::from(u16::from_be_bytes([e[3], e[4]])),
                write: e[6] == 0,
                value: e[5],
            },
            Some(5) => HwStop::Reset,
            _ => HwStop::Halt,
        }))
    }
}

impl Hw for Fpga6809 {
    fn run(&mut self, bps: &[u32], phys: &[usize], watches: &[Watch]) -> Result<(), String> {
        if bps.len() + phys.len() > 8 || watches.len() > 4 {
            return Err(tr!(
                "FPGAのブレークポイントは8個、ウォッチポイントは4個まで",
                "the FPGA has 8 breakpoints and 4 watchpoints"
            )
            .into());
        }
        self.flush()?;
        let mut slots: Vec<(u8, u32)> = bps.iter().map(|&a| (1u8, a & 0xffff)).collect();
        slots.extend(phys.iter().map(|&p| (3u8, p as u32)));
        if slots.iter().map(|s| s.1).ne(self.bps.iter().copied()) || self.bps.len() != slots.len() {
            for i in 0..8u8 {
                let (f, a) = slots.get(usize::from(i)).copied().unwrap_or((0, 0));
                let [_, a2, a1, a0] = a.to_be_bytes();
                self.link.ok(b'B', &[i, f, a2, a1, a0])?;
            }
            self.bps = slots.iter().map(|s| s.1).collect();
        }
        for i in 0..4u8 {
            let (m, lo, hi) = watches
                .get(usize::from(i))
                .map_or((0, 0, 0), |w| (watch_mode(w.kind), w.start as u16, w.end as u16));
            let [lh, ll] = lo.to_be_bytes();
            let [hh, hl] = hi.to_be_bytes();
            self.link.ok(b'X', &[i, m, lh, ll, hh, hl])?;
        }
        self.mem.cache.clear();
        self.link.ok(b'G', &[])?;
        self.running = true;
        Ok(())
    }
    fn wait(&mut self, d: Duration) -> Result<Option<HwStop>, String> {
        self.wait_stop(d)
    }
    fn stop(&mut self) -> Result<(), String> {
        if self.running {
            self.link.ok(b'H', &[])?;
            self.wait_stop(REPLY)?;
        }
        Ok(())
    }
    fn running(&self) -> bool {
        self.running
    }
}

impl Target for Fpga6809 {
    type Snap = Regs;
    type CpuCopy = Cpu;
    const CPU: &'static str = "6809";
    const MAX_INSN: usize = 5;
    const CALL_LENS: &'static [u32] = &[2, 3, 4];

    fn mem(&self) -> &dyn Mem {
        &self.mem
    }
    fn poke(&mut self, addr: u32, v: u8) {
        let _ = self.mem.cache.put(0, addr & 0xffff, v);
        self.mem.cache.clear();
    }
    fn pc(&self) -> u32 {
        u32::from(self.cpu.pc)
    }
    fn set_pc(&mut self, v: u32) {
        self.cpu.pc = v as u16;
        self.dirty = true;
    }
    fn sp(&self) -> u32 {
        u32::from(self.cpu.s)
    }
    fn cycles(&self) -> u64 {
        // the board counts no cycles: time at the nominal 1 MHz
        self.t0.elapsed().as_micros() as u64
    }
    fn context(&self) -> u32 {
        u32::from(self.mem.task())
    }
    fn addr_bits(&self) -> u32 {
        16
    }
    fn ret_size(&self) -> u32 {
        2
    }
    fn executes(&self) -> bool {
        true
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
        <super::target::Mc6809 as Target>::snap_json(r)
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
        self.reg_list().into_iter().find(|r| r.0.eq_ignore_ascii_case(name)).map(|r| r.1)
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
        self.dirty = true;
        true
    }
    fn wait_state(&self) -> u8 {
        0
    }
    fn disasm(&self, read: &dyn Fn(u32) -> u8, addr: u32, svc: SvcHook) -> Line {
        super::target::disasm6809(read, addr, svc)
    }
    fn step(&mut self, _watches: &[Watch]) -> Result<Option<WatchHit>, String> {
        Hw::stop(self)?;
        self.flush()?;
        self.link.ok(b'T', &[])?;
        match self.wait_stop(REPLY)? {
            Some(HwStop::Watch { addr, write, value }) => Ok(Some((addr, write, value))),
            Some(_) => Ok(None),
            None => Err(tr!("FPGAのCPUが1命令で止まらない", "the CPU on the FPGA did not stop after one instruction").into()),
        }
    }
    fn step_logged(&mut self, _w: &[Watch], _log: &mut Vec<(usize, u8)>) -> Result<(Option<WatchHit>, bool), String> {
        Err(tr!("FPGAでは逆実行の記録ができない", "recording for stepping back is not available on the FPGA").into())
    }
    fn cpu_copy(&self) -> Cpu {
        self.cpu.clone()
    }
    fn set_cpu(&mut self, c: Cpu) {
        self.cpu = c;
        self.dirty = true;
    }
    fn restore_ram(&mut self, _p: usize, _v: u8) {}
    fn disk_error(&mut self) -> Option<io::Error> {
        None
    }
    fn console_take(&mut self) -> Vec<u8> {
        self.link.console_take()
    }
    fn console_ready(&self) -> bool {
        true
    }
    fn console_put(&mut self, b: u8) {
        self.link.console_send(&[b])
    }
    fn mmu_json(&self) -> String {
        let m = &self.mem;
        let maps: Vec<String> = m.mmu[..16].iter().map(|v| v.to_string()).collect();
        format!(
            "{{\"enabled\":{},\"task\":{},\"romdis\":{},\"frt\":{},\"maps\":[{}]}}",
            m.enabled(),
            m.task(),
            m.rom_off(),
            m.fixed_top(),
            maps.join(",")
        )
    }
    fn ram_len(&self) -> usize {
        self.mem.ram
    }
    fn mark_start(&mut self) {}
    fn reset(&mut self) {
        // The board's reset: the machine starts again from its ROM and
        // stops at the first instruction (the ICE runs it on if it ran).
        let _ = self.link.ok(b'Z', &[1]);
        self.running = true;
        let _ = self.wait_stop(REPLY);
        self.bps.clear();
    }
    fn machine_name(&self) -> &'static str {
        "Multicomp (FPGA)"
    }
    fn hw(&mut self) -> Option<&mut dyn Hw> {
        Some(self)
    }
}

// ======================================================================
// 68000 (Tiny68K on fx68k)
// ======================================================================

// The stub's mailbox (rs6809-fpga m68k/rom/stub.S), in space 3.
const MB_REGS: u32 = 0x00; // D0-D7, A0-A6
const MB_USP: usize = 0x3c;
const MB_SSP: usize = 0x40;
const MB_SR: usize = 0x44;
const MB_PC: usize = 0x48;
const MB_LEN: usize = 0x4c;
const MB_CMD: u32 = 0x51;
const MB_ARG: u32 = 0x54;

/// The 68000 machine's memory, read over the debug unit (RAM, ROM and the
/// DUART's registers; the IDE registers read $FF).
pub struct Board68Remote {
    cache: Cache,
    ram: usize,
}
impl Mem for Board68Remote {
    fn peek(&self, addr: u32) -> u8 {
        self.cache.get(0, addr & 0xff_ffff)
    }
    fn translate(&self, addr: u32) -> Option<usize> {
        let a = (addr & 0xff_ffff) as usize;
        (a < self.ram).then_some(a)
    }
    fn logical(&self, phys: usize) -> Option<u32> {
        (phys < self.ram).then_some(phys as u32)
    }
    fn phys(&self, p: usize) -> Option<u8> {
        (p < self.ram).then(|| self.peek(p as u32))
    }
    fn ram_size(&self) -> usize {
        self.ram
    }
}
// Only for Mc68000's register views: the CPU itself never runs here.
impl Bus68k for Board68Remote {
    fn read8(&mut self, a: u32) -> u8 {
        self.peek(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        let _ = self.cache.put(0, a & 0xff_ffff, v);
    }
}
impl Board68k for Board68Remote {
    const NAME: &'static str = "Tiny68K (FPGA)";
    fn poke(&mut self, addr: u32, v: u8) {
        let _ = self.cache.put(0, addr & 0xff_ffff, v);
    }
    fn console_take(&mut self) -> Vec<u8> {
        self.cache.link.console_take()
    }
    fn console_ready(&self) -> bool {
        true
    }
    fn console_put(&mut self, b: u8) {
        self.cache.link.console_send(&[b])
    }
    fn save_board(&self, _w: &mut Writer, _disk: bool) {}
    fn load_board(&mut self, _r: &mut Reader) -> Result<(), String> {
        Err(tr!("FPGAでは状態を読み込めない", "a state cannot be loaded onto the FPGA").into())
    }
}

/// The 68000 machine on the FPGA: the registers are the stub's mailbox.
pub struct Fpga68k {
    link: Arc<Link>,
    t: Mc68000<Board68Remote>,
    dirty: bool,
    running: bool,
    t0: Instant,
    bps: Vec<u32>,
}

impl Fpga68k {
    pub fn connect(spec: &str, baud: u32) -> Result<Self, String> {
        let link = Arc::new(Link::open(spec, baud)?);
        let ram = ident(&link, 0x68)?;
        let board = Board68Remote { cache: Cache { link: link.clone(), lines: RefCell::new(HashMap::new()) }, ram };
        let mut t = Self {
            t: Mc68000::start(board, 0, 0),
            link,
            dirty: false,
            running: true,
            t0: Instant::now(),
            bps: Vec::new(),
        };
        match t.link.cmd(b'Q', &[])? {
            (b'q', d) if d.first() == Some(&1) => t.running = false,
            _ => {
                t.link.ok(b'H', &[])?;
                t.wait_stop(REPLY)?.ok_or_else(|| tr!("FPGAのCPUが止まらない", "the CPU on the FPGA does not stop"))?;
            }
        }
        t.refresh()?;
        Ok(t)
    }
    fn refresh(&mut self) -> Result<(), String> {
        let now = self.cycles();
        self.t.machine.cache.clear();
        let m = self.link.read(3, MB_REGS, MB_LEN)?;
        let l = |i: usize| u32::from_be_bytes([m[i], m[i + 1], m[i + 2], m[i + 3]]);
        let c = &mut self.t.cpu;
        for i in 0..8 {
            c.d[i] = l(4 * i);
        }
        for i in 0..7 {
            c.a[i] = l(32 + 4 * i);
        }
        let sr = u16::from_be_bytes([m[MB_SR], m[MB_SR + 1]]);
        c.set_sr(0x2700);
        c.a[7] = l(MB_SSP);
        c.set_usp(l(MB_USP));
        c.set_sr(sr);
        c.pc = l(MB_PC) & 0xff_ffff;
        c.stopped = false;
        c.cycles = now;
        self.dirty = false;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let c = &self.t.cpu;
        let mut m = vec![0u8; MB_LEN];
        let mut put = |i: usize, v: u32| m[i..i + 4].copy_from_slice(&v.to_be_bytes());
        for i in 0..8 {
            put(4 * i, c.d[i]);
        }
        for i in 0..7 {
            put(32 + 4 * i, c.a[i]);
        }
        put(MB_USP, c.usp());
        put(MB_SSP, c.ssp());
        put(MB_PC, c.pc);
        m[MB_SR..MB_SR + 2].copy_from_slice(&c.sr.to_be_bytes());
        self.link.write(3, MB_REGS, &m)?;
        self.dirty = false;
        Ok(())
    }
    fn command(&mut self, cmd: u8) -> Result<(), String> {
        self.link.write(3, MB_CMD, &[cmd])
    }
    fn wait_stop(&mut self, d: Duration) -> Result<Option<HwStop>, String> {
        let until = Instant::now() + d;
        loop {
            let Some(e) = self.link.event(until.saturating_duration_since(Instant::now()))? else { return Ok(None) };
            self.running = false;
            self.refresh()?;
            let stop = match e.first() {
                Some(2) => HwStop::Breakpoint,
                Some(3) => HwStop::Step,
                Some(4) if e.len() >= 7 => {
                    let addr = u32::from_be_bytes([0, e[1], e[2], e[3]]);
                    let w = u16::from_be_bytes([e[4], e[5]]);
                    let value = if addr & 1 == 0 { (w >> 8) as u8 } else { w as u8 };
                    HwStop::Watch { addr, write: e[6] == 0, value }
                }
                Some(5) => HwStop::Reset,
                _ => HwStop::Halt,
            };
            // An ILLEGAL that is not one of our breakpoints: the program's
            // own, for its handler.
            if stop == HwStop::Breakpoint && !self.bps.contains(&self.t.pc()) {
                let v = self.t.machine.peek32(0x10);
                self.link.write(3, MB_ARG, &v.to_be_bytes())?;
                self.command(3)?;
                self.running = true;
                continue;
            }
            return Ok(Some(stop));
        }
    }
}

impl Hw for Fpga68k {
    fn run(&mut self, bps: &[u32], _phys: &[usize], watches: &[Watch]) -> Result<(), String> {
        if bps.len() > 8 || watches.len() > 4 {
            return Err(tr!(
                "FPGAのブレークポイントは8個、ウォッチポイントは4個まで",
                "the FPGA has 8 breakpoints and 4 watchpoints"
            )
            .into());
        }
        self.flush()?;
        let bps: Vec<u32> = bps.iter().map(|a| a & 0xff_fffe).collect();
        if bps != self.bps {
            for i in 0..8u8 {
                let (f, a) = bps.get(usize::from(i)).map_or((0, 0), |&a| (1u8, a));
                let [_, a2, a1, a0] = a.to_be_bytes();
                self.link.ok(b'B', &[i, f, a2, a1, a0])?;
            }
            self.bps = bps;
        }
        for i in 0..4u8 {
            let (m, lo, hi) = watches
                .get(usize::from(i))
                .map_or((0, 0, 0), |w| (watch_mode(w.kind), w.start, w.end));
            let [_, l2, l1, l0] = lo.to_be_bytes();
            let [_, h2, h1, h0] = hi.to_be_bytes();
            self.link.ok(b'X', &[i, m, l2, l1, l0, h2, h1, h0])?;
        }
        self.t.machine.cache.clear();
        self.command(1)?;
        self.running = true;
        Ok(())
    }
    fn wait(&mut self, d: Duration) -> Result<Option<HwStop>, String> {
        self.wait_stop(d)
    }
    fn stop(&mut self) -> Result<(), String> {
        if self.running {
            self.link.ok(b'H', &[])?;
            self.wait_stop(REPLY)?;
        }
        Ok(())
    }
    fn running(&self) -> bool {
        self.running
    }
}

impl Target for Fpga68k {
    type Snap = <Mc68000<Board68Remote> as Target>::Snap;
    type CpuCopy = M68k;
    const CPU: &'static str = "68000";
    const MAX_INSN: usize = 10;
    const CALL_LENS: &'static [u32] = &[2, 4, 6];

    fn mem(&self) -> &dyn Mem {
        &self.t.machine
    }
    fn poke(&mut self, addr: u32, v: u8) {
        Board68k::poke(&mut self.t.machine, addr, v)
    }
    fn pc(&self) -> u32 {
        self.t.pc()
    }
    fn set_pc(&mut self, v: u32) {
        self.t.set_pc(v);
        self.dirty = true;
    }
    fn sp(&self) -> u32 {
        self.t.sp()
    }
    fn cycles(&self) -> u64 {
        // time at the board's 8 MHz
        self.t0.elapsed().as_micros() as u64 * 8
    }
    fn context(&self) -> u32 {
        self.t.context()
    }
    fn addr_bits(&self) -> u32 {
        24
    }
    fn ret_size(&self) -> u32 {
        4
    }
    fn executes(&self) -> bool {
        true
    }
    fn returns(&self, b: &[u8]) -> (bool, bool) {
        self.t.returns(b)
    }
    fn cpu_state(&self) -> CpuState {
        self.t.cpu_state()
    }
    fn snap(&self) -> Self::Snap {
        self.t.snap()
    }
    fn snap_pc(s: &Self::Snap) -> u32 {
        <Mc68000<Board68Remote> as Target>::snap_pc(s)
    }
    fn snap_json(s: &Self::Snap) -> String {
        <Mc68000<Board68Remote> as Target>::snap_json(s)
    }
    fn regs_json(&self) -> String {
        self.t.regs_json()
    }
    fn reg_list(&self) -> Vec<(&'static str, u32, u8)> {
        self.t.reg_list()
    }
    fn reg(&self, name: &str) -> Option<u32> {
        self.t.reg(name)
    }
    fn set_reg(&mut self, name: &str, v: u32) -> bool {
        let ok = self.t.set_reg(name, v);
        self.dirty |= ok;
        ok
    }
    fn wait_state(&self) -> u8 {
        0
    }
    fn disasm(&self, read: &dyn Fn(u32) -> u8, addr: u32, svc: SvcHook) -> Line {
        self.t.disasm(read, addr, svc)
    }
    fn step(&mut self, _watches: &[Watch]) -> Result<Option<WatchHit>, String> {
        Hw::stop(self)?;
        self.flush()?;
        self.t.machine.cache.clear();
        self.command(2)?;
        self.running = true;
        match self.wait_stop(REPLY)? {
            Some(HwStop::Watch { addr, write, value }) => Ok(Some((addr, write, value))),
            Some(_) => Ok(None),
            None => Err(tr!("FPGAのCPUが1命令で止まらない", "the CPU on the FPGA did not stop after one instruction").into()),
        }
    }
    fn step_logged(&mut self, _w: &[Watch], _log: &mut Vec<(usize, u8)>) -> Result<(Option<WatchHit>, bool), String> {
        Err(tr!("FPGAでは逆実行の記録ができない", "recording for stepping back is not available on the FPGA").into())
    }
    fn cpu_copy(&self) -> M68k {
        self.t.cpu.clone()
    }
    fn set_cpu(&mut self, c: M68k) {
        self.t.cpu = c;
        self.dirty = true;
    }
    fn restore_ram(&mut self, _p: usize, _v: u8) {}
    fn disk_error(&mut self) -> Option<io::Error> {
        None
    }
    fn console_take(&mut self) -> Vec<u8> {
        self.link.console_take()
    }
    fn console_ready(&self) -> bool {
        true
    }
    fn console_put(&mut self, b: u8) {
        self.link.console_send(&[b])
    }
    fn mmu_json(&self) -> String {
        "null".into()
    }
    fn ram_len(&self) -> usize {
        self.t.machine.ram
    }
    fn mark_start(&mut self) {}
    fn reset(&mut self) {
        let _ = self.link.ok(b'Z', &[1]);
        self.running = true;
        let _ = self.wait_stop(REPLY);
        self.bps.clear();
    }
    fn clock_hz(&self) -> u64 {
        8_000_000
    }
    fn machine_name(&self) -> &'static str {
        "Tiny68K (FPGA)"
    }
    fn hw(&mut self) -> Option<&mut dyn Hw> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A board that answers one 'I', sends console bytes with a DLE in
    /// them and an event.
    #[test]
    fn link_framing() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let board = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut cmd = [0u8; 3];
            s.read_exact(&mut cmd).unwrap();
            assert_eq!(cmd, [DLE, b'I', 0]);
            s.write_all(&[b'h', DLE, DLE, b'i', DLE, b'S', 1, 3]).unwrap();
            s.write_all(&[DLE, b'i', 6, 0x09, 1, 8, 4, 0, 4]).unwrap();
            let mut con = [0u8; 3];
            s.read_exact(&mut con).unwrap();
            assert_eq!(con, [b'a', DLE, DLE]);
        });
        let link = Link::open(&format!("tcp:{addr}"), 0).unwrap();
        assert_eq!(ident(&link, 0x09).unwrap(), 1024 * 1024);
        assert_eq!(link.console_take(), vec![b'h', DLE, b'i']);
        assert_eq!(link.event(Duration::from_millis(100)).unwrap(), Some(vec![3]));
        link.console_send(&[b'a', DLE]);
        board.join().unwrap();
    }
}
