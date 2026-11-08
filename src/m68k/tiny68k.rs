//! A Tiny68K-compatible machine (Bill Shen's 68000 board, as FUZIX and the
//! EtchedPixels EmulatorKit see it), without the monitor ROM.
//!
//! | Address             | Device |
//! |---------------------|--------|
//! | `$000000`–`$FF7FFF` | RAM (16 MiB less the top 32 KiB; `with_ram` makes it smaller, e.g. the 8 MiB of the Tang Nano 20K machine) |
//! | `$FFE000`–`$FFEFFF` | IDE (CompactFlash): register = (address & 31) >> 1 |
//! | `$FFF000`–`$FFF01F` | MC68681 DUART on odd addresses: register = (address >> 1) & 15 |
//!
//! The DUART interrupts on level 2 with the vector in its IVR. The IDE data
//! register is 16 bits wide with the bytes swapped (the first byte of a
//! sector is the low byte of the word), as on the board.
use crate::tr;
use super::Bus68k;
use super::state::{Reader, Writer};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};

pub const RAM_END: u32 = 0xff_8000;
pub const IDE_BASE: u32 = 0xff_e000;
pub const DUART_BASE: u32 = 0xff_f000;
/// The DUART's crystal (X1).
pub const DUART_HZ: u64 = 3_686_400;
/// CPU clock used to run the DUART timer (the EmulatorKit runs about 10 MHz).
pub const CPU_HZ: u64 = 10_000_000;
/// Interrupt level of the DUART.
pub const DUART_LEVEL: u8 = 2;

pub struct Tiny68k {
    pub ram: Vec<u8>,
    pub duart: Duart,
    pub ide: Ide,
    /// DUART channel B over TCP (a second terminal), if enabled.
    pub tty2: Option<super::serial::TcpPort>,
}

impl Default for Tiny68k {
    fn default() -> Self {
        Self {
            ram: vec![0; RAM_END as usize],
            duart: Duart::default(),
            ide: Ide::default(),
            tty2: None,
        }
    }
}

impl Tiny68k {
    /// A machine with less RAM from `$000000` (the Tang Nano 20K's has
    /// 8 MiB); addresses above it up to `$FF7FFF` read `$FF`.
    pub fn with_ram(bytes: usize) -> Self {
        Self {
            ram: vec![0; bytes.min(RAM_END as usize)],
            ..Self::default()
        }
    }
    /// The end of RAM (the initial supervisor stack without a monitor).
    pub fn ram_end(&self) -> u32 {
        self.ram.len() as u32
    }
    /// Read without side effects.
    pub fn peek(&self, a: u32) -> u8 {
        let a = a & 0xff_ffff;
        if (a as usize) < self.ram.len() {
            self.ram[a as usize]
        } else if (IDE_BASE..IDE_BASE + 0x1000).contains(&a) {
            self.ide.peek(((a & 31) >> 1) as u8)
        } else if (DUART_BASE..DUART_BASE + 0x20).contains(&a) && a & 1 == 1 {
            self.duart.peek(((a >> 1) & 15) as u8)
        } else {
            0xff
        }
    }
    /// Write RAM only.
    pub fn poke(&mut self, a: u32, v: u8) {
        if let Some(b) = self.ram.get_mut((a & 0xff_ffff) as usize) {
            *b = v
        }
    }
    pub fn load(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let end = addr as usize + data.len();
        if end > self.ram.len() {
            return Err(tr!(format!("${addr:06X}+{} はRAMの外", data.len()), format!("${addr:06X}+{} is outside RAM", data.len())));
        }
        self.ram[addr as usize..end].copy_from_slice(data);
        Ok(())
    }
    /// Advance the devices by CPU cycles.
    pub fn tick(&mut self, cycles: u64) {
        self.duart.tick(cycles);
    }
    /// Exchange channel B with its TCP client (call now and then).
    pub fn poll_io(&mut self) {
        if let Some(p) = &mut self.tty2 {
            p.poll(&mut self.duart.b.rx, &mut self.duart.b.tx);
        }
    }
    /// RAM, the DUART, the IDE registers and, with `disk`, the disk.
    pub fn save_state(&self, w: &mut Writer, disk: bool) {
        w.bytes(&self.ram);
        self.duart.save(w);
        self.ide.save(w, disk);
    }
    /// Restore a `save_state`; without a saved disk, the current one stays.
    pub fn load_state(&mut self, r: &mut Reader) -> Result<(), String> {
        let ram = r.bytes()?;
        if ram.len() != self.ram.len() {
            return Err(tr!("RAMの大きさが違う", "the RAM size differs").into());
        }
        self.ram.copy_from_slice(ram);
        self.duart.load(r)?;
        self.ide.load(r)
    }
    /// The interrupt request: (level, vector).
    pub fn irq(&self) -> (u8, Option<u8>) {
        if self.duart.irq() {
            (DUART_LEVEL, Some(self.duart.ivr))
        } else {
            (0, None)
        }
    }
}

impl Bus68k for Tiny68k {
    fn read8(&mut self, a: u32) -> u8 {
        let a = a & 0xff_ffff;
        if (a as usize) < self.ram.len() {
            self.ram[a as usize]
        } else if (IDE_BASE..IDE_BASE + 0x1000).contains(&a) {
            self.ide.read(((a & 31) >> 1) as u8)
        } else if (DUART_BASE..DUART_BASE + 0x20).contains(&a) && a & 1 == 1 {
            self.duart.read(((a >> 1) & 15) as u8)
        } else {
            0xff
        }
    }
    fn write8(&mut self, a: u32, v: u8) {
        let a = a & 0xff_ffff;
        if (a as usize) < self.ram.len() {
            self.ram[a as usize] = v
        } else if (IDE_BASE..IDE_BASE + 0x1000).contains(&a) {
            self.ide.write(((a & 31) >> 1) as u8, v)
        } else if (DUART_BASE..DUART_BASE + 0x20).contains(&a) && a & 1 == 1 {
            self.duart.write(((a >> 1) & 15) as u8, v)
        }
    }
    fn read16(&mut self, a: u32) -> u16 {
        let a = a & 0xff_ffff;
        if (a as usize) + 1 < self.ram.len() {
            return u16::from_be_bytes([self.ram[a as usize], self.ram[a as usize + 1]]);
        }
        if (IDE_BASE..IDE_BASE + 0x1000).contains(&a) && (a & 31) >> 1 == 0 {
            let lo = self.ide.read(0);
            let hi = self.ide.read(0);
            return u16::from_le_bytes([lo, hi]);
        }
        u16::from_be_bytes([self.read8(a), self.read8(a.wrapping_add(1))])
    }
    fn write16(&mut self, a: u32, v: u16) {
        let a = a & 0xff_ffff;
        if (IDE_BASE..IDE_BASE + 0x1000).contains(&a) && (a & 31) >> 1 == 0 {
            let [lo, hi] = v.to_le_bytes();
            self.ide.write(0, lo);
            self.ide.write(0, hi);
            return;
        }
        let [h, l] = v.to_be_bytes();
        self.write8(a, h);
        self.write8(a.wrapping_add(1), l);
    }
}

// ---------- MC68681 DUART ----------

#[derive(Default)]
pub struct Channel {
    mr: [u8; 2],
    mr_ptr: usize,
    pub rx: VecDeque<u8>,
    pub tx: Vec<u8>,
    rx_on: bool,
    tx_on: bool,
}

impl Channel {
    fn save(&self, w: &mut Writer) {
        w.u8(self.mr[0]);
        w.u8(self.mr[1]);
        w.u8(self.mr_ptr as u8);
        w.bytes(&self.rx.iter().copied().collect::<Vec<u8>>());
        w.bool(self.rx_on);
        w.bool(self.tx_on);
    }
    fn load(&mut self, r: &mut Reader) -> Result<(), String> {
        self.mr = [r.u8()?, r.u8()?];
        self.mr_ptr = usize::from(r.u8()? & 1);
        self.rx = r.bytes()?.iter().copied().collect();
        self.rx_on = r.bool()?;
        self.tx_on = r.bool()?;
        self.tx.clear();
        Ok(())
    }
    fn status(&self) -> u8 {
        // RxRDY, (FIFO full), TxRDY and TxEMT: transmission is instant.
        u8::from(!self.rx.is_empty()) | if self.tx_on { 0x0c } else { 0 }
    }
    fn command(&mut self, v: u8) {
        match v & 3 {
            1 => self.rx_on = true,
            2 => self.rx_on = false,
            _ => {}
        }
        match (v >> 2) & 3 {
            1 => self.tx_on = true,
            2 => self.tx_on = false,
            _ => {}
        }
        match (v >> 4) & 7 {
            1 => self.mr_ptr = 0,
            2 => self.rx.clear(),
            _ => {}
        }
    }
}

pub struct Duart {
    pub a: Channel,
    pub b: Channel,
    acr: u8,
    pub imr: u8,
    isr_ct: bool,
    pub ivr: u8,
    ct_preload: u16,
    /// DUART clock ticks (scaled by CPU_HZ) left until the counter's next
    /// terminal count.
    ct_left: u64,
    ct_running: bool,
    opr: u8,
}

impl Default for Duart {
    fn default() -> Self {
        Self {
            a: Channel {
                tx_on: true,
                rx_on: true,
                ..Channel::default()
            },
            b: Channel {
                tx_on: true,
                rx_on: true,
                ..Channel::default()
            },
            acr: 0,
            imr: 0,
            isr_ct: false,
            ivr: 0x0f,
            ct_preload: 0xffff,
            ct_left: 0,
            ct_running: false,
            opr: 0,
        }
    }
}

impl Duart {
    fn save(&self, w: &mut Writer) {
        self.a.save(w);
        self.b.save(w);
        for v in [self.acr, self.imr, u8::from(self.isr_ct), self.ivr, self.opr] {
            w.u8(v);
        }
        w.u16(self.ct_preload);
        w.u64(self.ct_left);
        w.bool(self.ct_running);
    }
    fn load(&mut self, r: &mut Reader) -> Result<(), String> {
        self.a.load(r)?;
        self.b.load(r)?;
        self.acr = r.u8()?;
        self.imr = r.u8()?;
        self.isr_ct = r.bool()?;
        self.ivr = r.u8()?;
        self.opr = r.u8()?;
        self.ct_preload = r.u16()?;
        self.ct_left = r.u64()?;
        self.ct_running = r.bool()?;
        Ok(())
    }
    pub fn isr(&self) -> u8 {
        let a = self.a.status();
        let b = self.b.status();
        (a >> 2) & 1 | (a & 1) << 1 | u8::from(self.isr_ct) << 3 | ((b >> 2) & 1) << 4 | (b & 1) << 5
    }
    pub fn irq(&self) -> bool {
        self.isr() & self.imr != 0
    }
    /// Counter/timer clock in Hz, from ACR bits 6–4.
    fn ct_hz(&self) -> u64 {
        match (self.acr >> 4) & 7 {
            7 => DUART_HZ / 16,
            6 => DUART_HZ,
            3 => DUART_HZ / 16, // counter mode, X1/16
            _ => DUART_HZ / 16,
        }
    }
    /// CPU cycles between terminal counts (timer mode: each half of the
    /// square wave; counter ready is set once per full wave).
    fn ct_period(&self) -> u64 {
        let n = u64::from(self.ct_preload.max(1));
        let timer = self.acr & 0x40 != 0;
        let counts = if timer { 2 * n } else { n };
        (counts * CPU_HZ / self.ct_hz()).max(1)
    }
    fn restart_counter(&mut self) {
        self.ct_running = true;
        self.ct_left = self.ct_period();
    }
    /// CPU cycles until the counter/timer interrupts, when it can.
    pub fn next_event(&self) -> Option<u64> {
        let timer = self.acr & 0x40 != 0;
        let enabled = self.imr & 0x08 != 0 && !self.isr_ct;
        (enabled && (self.ct_running || timer)).then(|| if self.ct_left == 0 { self.ct_period() } else { self.ct_left })
    }
    pub fn tick(&mut self, cycles: u64) {
        let timer = self.acr & 0x40 != 0;
        if !(self.ct_running || timer) {
            return;
        }
        if self.ct_left == 0 {
            self.ct_left = self.ct_period();
        }
        if cycles >= self.ct_left {
            self.isr_ct = true;
            let p = self.ct_period();
            let over = (cycles - self.ct_left) % p;
            self.ct_left = p - over;
            if !timer {
                self.ct_running = false;
            }
        } else {
            self.ct_left -= cycles;
        }
    }
    pub fn peek(&self, r: u8) -> u8 {
        match r {
            0 => self.a.mr[self.a.mr_ptr],
            1 => self.a.status(),
            3 => self.a.rx.front().copied().unwrap_or(0),
            5 => self.isr(),
            8 => self.b.mr[self.b.mr_ptr],
            9 => self.b.status(),
            11 => self.b.rx.front().copied().unwrap_or(0),
            12 => self.ivr,
            13 => 0xff,
            _ => 0xff,
        }
    }
    pub fn read(&mut self, r: u8) -> u8 {
        match r {
            0 => {
                let v = self.a.mr[self.a.mr_ptr];
                self.a.mr_ptr = 1;
                v
            }
            3 => self.a.rx.pop_front().unwrap_or(0),
            8 => {
                let v = self.b.mr[self.b.mr_ptr];
                self.b.mr_ptr = 1;
                v
            }
            11 => self.b.rx.pop_front().unwrap_or(0),
            14 => {
                // Start counter command.
                self.restart_counter();
                0xff
            }
            15 => {
                // Stop counter command: clears counter ready.
                self.isr_ct = false;
                if self.acr & 0x40 == 0 {
                    self.ct_running = false;
                }
                0xff
            }
            _ => self.peek(r),
        }
    }
    pub fn write(&mut self, r: u8, v: u8) {
        match r {
            0 => {
                self.a.mr[self.a.mr_ptr] = v;
                self.a.mr_ptr = 1;
            }
            2 => self.a.command(v),
            3 => {
                if self.a.tx_on {
                    self.a.tx.push(v)
                }
            }
            4 => {
                self.acr = v;
                if v & 0x40 != 0 {
                    self.ct_left = self.ct_period();
                }
            }
            5 => self.imr = v,
            6 => self.ct_preload = (self.ct_preload & 0x00ff) | u16::from(v) << 8,
            7 => self.ct_preload = (self.ct_preload & 0xff00) | u16::from(v),
            8 => {
                self.b.mr[self.b.mr_ptr] = v;
                self.b.mr_ptr = 1;
            }
            10 => self.b.command(v),
            11 => {
                if self.b.tx_on {
                    self.b.tx.push(v)
                }
            }
            12 => self.ivr = v,
            14 => self.opr |= v,
            15 => self.opr &= !v,
            _ => {}
        }
    }
}

// ---------- IDE (LBA, PIO) ----------

pub const ST_ERR: u8 = 0x01;
pub const ST_DRQ: u8 = 0x08;
pub const ST_DRDY: u8 = 0x40;

#[derive(Default)]
pub struct Ide {
    pub disk: Vec<u8>,
    /// Writes go through to this file as well.
    pub file: Option<File>,
    pub error: Option<io::Error>,
    regs: [u8; 8],
    status: u8,
    buf: Vec<u8>,
    pos: usize,
    /// Sectors left in the current command, and whether it writes.
    left: u32,
    writing: bool,
    lba: u32,
}

impl Ide {
    fn save(&self, w: &mut Writer, disk: bool) {
        w.bytes(&self.regs);
        w.u8(self.status);
        w.bytes(&self.buf);
        w.u64(self.pos as u64);
        w.u32(self.left);
        w.bool(self.writing);
        w.u32(self.lba);
        w.bool(disk);
        if disk {
            w.bytes(&self.disk);
        }
    }
    fn load(&mut self, r: &mut Reader) -> Result<(), String> {
        self.regs.copy_from_slice(r.bytes()?.get(..8).ok_or(tr!("IDEのレジスタが足りない", "too few IDE registers"))?);
        self.status = r.u8()?;
        self.buf = r.bytes()?.to_vec();
        self.pos = r.u64()? as usize;
        self.left = r.u32()?;
        self.writing = r.bool()?;
        self.lba = r.u32()?;
        if r.bool()? {
            let d = r.bytes()?;
            if self.file.is_some() && d != self.disk.as_slice() {
                return Err(tr!("書き込みを残すディスク（--persist-disk）には、ディスクを含む状態を読み込めない", "a state with a disk cannot be loaded onto a disk whose writes are kept (--persist-disk)").into());
            }
            self.disk = d.to_vec();
        }
        Ok(())
    }
    pub fn attach(&mut self, disk: Vec<u8>, file: Option<File>) {
        self.disk = disk;
        self.file = file;
        self.status = ST_DRDY;
    }
    fn sectors(&self) -> u32 {
        (self.disk.len() / 512) as u32
    }
    fn reg_lba(&self) -> u32 {
        u32::from(self.regs[3]) | u32::from(self.regs[4]) << 8 | u32::from(self.regs[5]) << 16 | u32::from(self.regs[6] & 15) << 24
    }
    /// Device 1 (the DEV bit of the drive/head register) is not fitted.
    fn absent(&self) -> bool {
        self.disk.is_empty() || self.regs[6] & 0x10 != 0
    }
    fn status(&self) -> u8 {
        if self.absent() { 0 } else { self.status }
    }
    pub fn peek(&self, r: u8) -> u8 {
        match r & 15 {
            0 => self.buf.get(self.pos).copied().unwrap_or(0),
            1 => self.regs[1],
            7 | 14 => self.status(),
            r @ 2..=6 => self.regs[r as usize],
            _ => 0xff,
        }
    }
    pub fn read(&mut self, r: u8) -> u8 {
        if r & 15 != 0 {
            return self.peek(r);
        }
        if self.writing || self.pos >= self.buf.len() {
            return 0xff;
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.left -= 1;
            if self.left > 0 {
                self.lba += 1;
                self.fill();
            } else {
                self.status = ST_DRDY;
            }
        }
        v
    }
    pub fn write(&mut self, r: u8, v: u8) {
        match r & 15 {
            0 => {
                if self.writing && self.pos < self.buf.len() {
                    self.buf[self.pos] = v;
                    self.pos += 1;
                    if self.pos == self.buf.len() {
                        self.flush_sector();
                    }
                }
            }
            7 => self.command(v),
            r @ 1..=6 => self.regs[r as usize] = v,
            _ => {}
        }
    }
    fn count(&self) -> u32 {
        if self.regs[2] == 0 { 256 } else { u32::from(self.regs[2]) }
    }
    fn fail(&mut self) {
        self.status = ST_DRDY | ST_ERR;
        self.regs[1] = 0x04; // aborted
        self.buf.clear();
        self.left = 0;
        self.writing = false;
    }
    fn fill(&mut self) {
        let at = self.lba as usize * 512;
        self.buf = self.disk[at..at + 512].to_vec();
        self.pos = 0;
        self.status = ST_DRDY | ST_DRQ;
    }
    fn flush_sector(&mut self) {
        let at = self.lba as usize * 512;
        self.disk[at..at + 512].copy_from_slice(&self.buf);
        if let Some(f) = &mut self.file
            && let Err(e) = f.seek(SeekFrom::Start(at as u64)).and_then(|_| f.write_all(&self.buf))
        {
            self.error.get_or_insert(e);
        }
        self.left -= 1;
        if self.left > 0 {
            self.lba += 1;
            self.pos = 0;
            self.status = ST_DRDY | ST_DRQ;
        } else {
            self.writing = false;
            self.buf.clear();
            self.status = ST_DRDY;
        }
    }
    fn command(&mut self, c: u8) {
        if self.absent() {
            return;
        }
        self.regs[1] = 0;
        match c {
            0x20 | 0x21 | 0x30 | 0x31 => {
                let lba = self.reg_lba();
                let n = self.count();
                if self.regs[6] & 0x40 == 0 || lba + n > self.sectors() {
                    return self.fail();
                }
                self.lba = lba;
                self.left = n;
                if c >= 0x30 {
                    self.writing = true;
                    self.buf = vec![0; 512];
                    self.pos = 0;
                    self.status = ST_DRDY | ST_DRQ;
                } else {
                    self.writing = false;
                    self.fill();
                }
            }
            0xec => {
                // IDENTIFY DEVICE: words, little-endian on the wire.
                let mut id = [0u16; 256];
                id[0] = 0x0040;
                id[1] = 1024;
                id[3] = 16;
                id[6] = 63;
                id[49] = 0x0200; // LBA
                let n = self.sectors();
                id[60] = n as u16;
                id[61] = (n >> 16) as u16;
                for (i, c) in b"rs6809 IDE disk                         ".chunks(2).enumerate() {
                    id[27 + i] = u16::from(c[0]) << 8 | u16::from(c[1]);
                }
                self.buf = id.iter().flat_map(|w| w.to_le_bytes()).collect();
                self.pos = 0;
                self.left = 1;
                self.writing = false;
                self.status = ST_DRDY | ST_DRQ;
            }
            // SET FEATURES, IDLE and friends: accepted.
            0xef | 0xe0..=0xe7 | 0x91 | 0x10..=0x1f => self.status = ST_DRDY,
            _ => self.fail(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_disk() -> Tiny68k {
        let mut m = Tiny68k::default();
        let mut disk = vec![0u8; 512 * 4];
        disk[512] = 0x12;
        disk[513] = 0x34;
        m.ide.attach(disk, None);
        m
    }
    fn cmd(m: &mut Tiny68k, lba: u8, c: u8) {
        m.write8(IDE_BASE + 0x05, 1); // count
        m.write8(IDE_BASE + 0x07, lba);
        m.write8(IDE_BASE + 0x09, 0);
        m.write8(IDE_BASE + 0x0b, 0);
        m.write8(IDE_BASE + 0x0d, 0xe0);
        m.write8(IDE_BASE + 0x0f, c);
    }

    #[test]
    fn ide_words_are_byte_swapped() {
        let mut m = with_disk();
        cmd(&mut m, 1, 0x20);
        assert_eq!(m.read8(IDE_BASE + 0x0f) & (ST_DRQ | ST_DRDY), ST_DRQ | ST_DRDY);
        // The first byte of the sector is the low byte of the word.
        assert_eq!(m.read16(IDE_BASE), 0x3412);
        for _ in 1..256 {
            m.read16(IDE_BASE);
        }
        assert_eq!(m.read8(IDE_BASE + 0x0f), ST_DRDY, "done");

        cmd(&mut m, 2, 0x30);
        m.write16(IDE_BASE, 0xbbaa);
        for _ in 1..256 {
            m.write16(IDE_BASE, 0);
        }
        assert_eq!(&m.ide.disk[1024..1026], &[0xaa, 0xbb]);
        // Out of range: aborted.
        cmd(&mut m, 9, 0x20);
        assert_eq!(m.read8(IDE_BASE + 0x0f) & ST_ERR, ST_ERR);
    }

    #[test]
    fn duart_console_and_timer_interrupt() {
        let mut m = Tiny68k::default();
        m.write8(DUART_BASE + 0x07, b'A');
        assert_eq!(m.duart.a.tx, b"A");
        assert_eq!(m.read8(DUART_BASE + 0x03) & 1, 0, "no input");
        m.duart.a.rx.push_back(b'x');
        assert_eq!(m.read8(DUART_BASE + 0x03) & 1, 1);
        assert_eq!(m.read8(DUART_BASE + 0x07), b'x');

        // FUZIX's 100 Hz tick: timer mode from X1/16, preload $480.
        m.write8(DUART_BASE + 0x09, 0x70);
        m.write8(DUART_BASE + 0x0d, 0x04);
        m.write8(DUART_BASE + 0x0f, 0x80);
        m.write8(DUART_BASE + 0x19, 0x81);
        m.write8(DUART_BASE + 0x0b, 0x08);
        m.write8(DUART_BASE + 0x09, 0x70); // reload with the preload
        m.tick(CPU_HZ / 100 - 1000);
        assert_eq!(m.irq(), (0, None));
        m.tick(2000);
        assert_eq!(m.irq(), (DUART_LEVEL, Some(0x81)));
        m.read8(DUART_BASE + 0x1f); // stop counter: acknowledge
        assert_eq!(m.irq(), (0, None));
    }
}
