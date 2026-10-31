//! A plain 68000 machine for programs and the ICE: RAM from address 0 and a
//! console port. (The machine of a real board comes in a later stage.)
//!
//! | Address   | Use |
//! |-----------|-----|
//! | `$000000` | RAM (vectors from 0) |
//! | `$FF0000` | console status: bit 0 = input byte ready, bit 1 = can send |
//! | `$FF0001` | console data: read takes an input byte, write sends one |
//!
//! Other addresses read `$FF` and ignore writes.
use super::Bus68k;
use std::collections::VecDeque;

pub const CONSOLE_STATUS: u32 = 0xff_0000;
pub const CONSOLE_DATA: u32 = 0xff_0001;

pub struct Sim68k {
    pub ram: Vec<u8>,
    /// Bytes sent to the console, not yet taken by the host.
    pub tx: Vec<u8>,
    /// Bytes waiting to be read from the console.
    pub rx: VecDeque<u8>,
}

impl Default for Sim68k {
    fn default() -> Self {
        Self::with_ram(1 << 20)
    }
}

impl Sim68k {
    pub fn with_ram(bytes: usize) -> Self {
        Self {
            ram: vec![0; bytes.min(0xff_0000)],
            tx: Vec::new(),
            rx: VecDeque::new(),
        }
    }
    /// Read without side effects (the console data port shows the next byte).
    pub fn peek(&self, a: u32) -> u8 {
        let a = a & 0xff_ffff;
        match a {
            CONSOLE_STATUS => self.status(),
            CONSOLE_DATA => self.rx.front().copied().unwrap_or(0),
            _ => self.ram.get(a as usize).copied().unwrap_or(0xff),
        }
    }
    /// Write to RAM only.
    pub fn poke(&mut self, a: u32, v: u8) {
        if let Some(b) = self.ram.get_mut((a & 0xff_ffff) as usize) {
            *b = v
        }
    }
    fn status(&self) -> u8 {
        u8::from(!self.rx.is_empty()) | 2
    }
    pub fn load(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let end = addr as usize + data.len();
        if end > self.ram.len() {
            return Err(format!("${addr:06X}+{} はRAM（{} バイト）の外", data.len(), self.ram.len()));
        }
        self.ram[addr as usize..end].copy_from_slice(data);
        Ok(())
    }
    /// Put the reset vectors: initial SSP and PC.
    pub fn set_vectors(&mut self, ssp: u32, pc: u32) {
        for (i, b) in ssp.to_be_bytes().into_iter().chain(pc.to_be_bytes()).enumerate() {
            self.poke(i as u32, b);
        }
    }
    /// Load Motorola S-records (S1/S2/S3 data); returns the start address
    /// of the S7/S8/S9 record, if any.
    pub fn load_srec(&mut self, text: &str) -> Result<Option<u32>, String> {
        let mut start = None;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let err = |m: &str| format!("S-record {}行目: {m}", n + 1);
            let kind = line.strip_prefix('S').and_then(|r| r.chars().next()).ok_or_else(|| err("Sで始まらない"))?;
            let hex = &line[2..];
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
                .collect::<Result<_, _>>()
                .map_err(|_| err("16進でない"))?;
            if bytes.is_empty() || bytes[0] as usize != bytes.len() - 1 {
                return Err(err("長さが合わない"));
            }
            if bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b)) != 0xff {
                return Err(err("チェックサムが合わない"));
            }
            let alen = match kind {
                '1' | '9' => 2,
                '2' | '8' => 3,
                '3' | '7' => 4,
                _ => continue,
            };
            let body = &bytes[1..bytes.len() - 1];
            if body.len() < alen {
                return Err(err("番地がない"));
            }
            let addr = body[..alen].iter().fold(0u32, |a, b| a << 8 | u32::from(*b));
            if matches!(kind, '1' | '2' | '3') {
                self.load(addr, &body[alen..]).map_err(|e| err(&e))?;
            } else {
                start = Some(addr);
            }
        }
        Ok(start)
    }
}

impl Bus68k for Sim68k {
    fn read8(&mut self, a: u32) -> u8 {
        let a = a & 0xff_ffff;
        if a == CONSOLE_DATA {
            return self.rx.pop_front().unwrap_or(0);
        }
        self.peek(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        let a = a & 0xff_ffff;
        if a == CONSOLE_DATA {
            self.tx.push(v);
        } else {
            self.poke(a, v)
        }
    }
}
