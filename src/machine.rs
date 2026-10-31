use crate::{Bus, Cpu, CpuError};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

/// A console-compatible subset of the MC6850 (instantaneous transmit).
#[derive(Default)]
pub struct Acia {
    pub control: u8,
    pub rx: VecDeque<u8>,
    pub tx: Vec<u8>,
    /// MC6850 DCD is active low. Local serial consoles keep carrier present.
    pub carrier_lost: bool,
    pub resets: u64,
    configured: bool,
    reset_streak: u8,
    session_ready: bool,
}
impl Acia {
    /// TSMon acknowledges a fresh login endpoint with two master resets.
    pub fn session_ready(&self) -> bool {
        self.session_ready
    }
    pub fn configured(&self) -> bool {
        self.configured
    }
    pub fn irq(&self) -> bool {
        self.control & 0x80 != 0 && !self.rx.is_empty() || self.control & 0x60 == 0x20
    }
    pub fn status(&self) -> u8 {
        2 | u8::from(!self.rx.is_empty())
            | if self.carrier_lost { 4 } else { 0 }
            | if self.irq() { 0x80 } else { 0 }
    }
    pub fn control(&mut self, v: u8) {
        if v & 3 == 3 {
            self.session_ready = false;
            self.reset_streak = self.reset_streak.saturating_add(1);
            self.configured = false;
            self.resets = self.resets.wrapping_add(1);
            self.control = 0;
            self.rx.clear()
        } else {
            self.session_ready |= self.reset_streak >= 2;
            self.reset_streak = 0;
            self.configured = true;
            self.control = v
        }
    }
}
struct Disk {
    file: File,
    persistent: bool,
    overlay: BTreeMap<u32, [u8; 512]>,
    lba: u32,
    state: u8,
    data: [u8; 512],
    index: usize,
    error: Option<io::Error>,
}
impl Disk {
    fn command(&mut self, v: u8) {
        if self.state != 0 {
            return;
        }
        self.index = 0;
        match v {
            0 => {
                self.data.fill(0xff);
                if let Some(data) = self.overlay.get(&self.lba) {
                    self.data = *data
                } else if let Err(e) = self
                    .file
                    .seek(SeekFrom::Start(self.lba as u64 * 512))
                    .and_then(|_| self.file.read_exact(&mut self.data))
                {
                    self.error = Some(e);
                }
                self.state = 1;
            }
            1 => {
                self.data.fill(0);
                self.state = 2
            }
            _ => (),
        }
    }
    #[inline]
    fn read(&mut self) -> u8 {
        if self.state != 1 {
            return 0xff;
        }
        let v = self.data[self.index];
        self.advance();
        v
    }
    #[inline]
    fn write(&mut self, v: u8) {
        if self.state != 2 {
            return;
        }
        self.data[self.index] = v;
        self.advance();
    }
    fn advance(&mut self) {
        self.index += 1;
        if self.index == 512 {
            if self.state == 2 {
                if self.persistent {
                    if let Err(error) = self
                        .file
                        .seek(SeekFrom::Start(u64::from(self.lba) * 512))
                        .and_then(|_| self.file.write_all(&self.data))
                        .and_then(|_| self.file.sync_data())
                    {
                        self.error = Some(error);
                    }
                } else {
                    self.overlay.insert(self.lba, self.data);
                }
            }
            self.state = 0;
        }
    }
}
struct Mapping {
    read_bases: [usize; 8],
    writable: [bool; 8],
}

/// Flat 64 KiB RAM or banked Multicomp RAM with an 8 KiB ROM overlay.
/// Disk writes use a volatile overlay unless `persist_disk_writes` is enabled.
pub struct Machine {
    pub ram: Box<[u8]>,
    pub acia: Acia,
    extra_acias: Vec<Acia>,
    pub acia_base: u16,
    pub timer_base: u16,
    pub timer: u8,
    pub tick_cycles: u64,
    elapsed: u64,
    rom: Option<Box<[u8; 8192]>>,
    rom_disabled: bool,
    mapper: u8,
    maps: [u8; 16],
    mapping: Box<Mapping>,
    fixed_top: bool,
    disk: Option<Disk>,
}
impl Default for Machine {
    fn default() -> Self {
        Self {
            ram: vec![0; 65536].into_boxed_slice(),
            acia: Acia::default(),
            extra_acias: Vec::new(),
            acia_base: 0x8018,
            timer_base: 0x8010,
            timer: 0,
            tick_cycles: 20000,
            elapsed: 0,
            rom: None,
            rom_disabled: false,
            mapper: 0,
            maps: [0; 16],
            mapping: Box::new(Mapping {
                read_bases: [0, 8192, 16384, 24576, 32768, 40960, 49152, 57344],
                writable: [true; 8],
            }),
            fixed_top: false,
            disk: None,
        }
    }
}
impl Machine {
    /// Multicomp with 512 KiB physical RAM (64 banks of 8 KiB).
    pub fn multicomp(rom: &[u8], disk: File) -> io::Result<Self> {
        Self::multicomp_with_ram(rom, disk, 512 * 1024)
    }

    /// Select 64, 128, 256, 512 or 1024 KiB of physical RAM.
    /// The CPU address space remains 64 KiB; MMU entries select physical banks.
    pub fn multicomp_with_ram(rom: &[u8], disk: File, ram_bytes: usize) -> io::Result<Self> {
        if !(65536..=1048576).contains(&ram_bytes) || !ram_bytes.is_power_of_two() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Multicomp RAM must be 64, 128, 256, 512 or 1024 KiB",
            ));
        }
        let rom: Box<[u8; 8192]> = rom.to_vec().into_boxed_slice().try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "boot ROM must be exactly 8192 bytes",
            )
        })?;
        Ok(Self {
            ram: vec![0; ram_bytes].into_boxed_slice(),
            acia_base: 0xffd0,
            timer_base: 0xffdd,
            rom: Some(rom),
            disk: Some(Disk {
                file: disk,
                persistent: false,
                overlay: BTreeMap::new(),
                lba: 0,
                state: 0,
                data: [0; 512],
                index: 0,
                error: None,
            }),
            ..Self::default()
        })
    }
    /// Configure the total number of Multicomp serial ports, including /term.
    /// Ports occupy $FFD0/$FFD1 through $FFD6/$FFD7. Call before CPU reset.
    pub fn set_acia_count(&mut self, count: usize) -> io::Result<()> {
        if self.rom.is_none() || !(1..=4).contains(&count) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Multicomp ACIA count must be 1 through 4",
            ));
        }
        self.extra_acias.resize_with(count - 1, Acia::default);
        Ok(())
    }
    pub fn acia_count(&self) -> usize {
        1 + self.extra_acias.len()
    }
    pub fn serial(&self, index: usize) -> Option<&Acia> {
        if index == 0 {
            Some(&self.acia)
        } else {
            self.extra_acias.get(index - 1)
        }
    }
    pub fn serial_mut(&mut self, index: usize) -> Option<&mut Acia> {
        if index == 0 {
            Some(&mut self.acia)
        } else {
            self.extra_acias.get_mut(index - 1)
        }
    }
    /// Persist completed SD sectors to a read/write backing file. Enable before
    /// running the CPU; an existing volatile overlay cannot be converted.
    pub fn persist_disk_writes(&mut self) -> io::Result<()> {
        let disk = self
            .disk
            .as_mut()
            .ok_or_else(|| io::Error::other("no SD disk configured"))?;
        if !disk.overlay.is_empty() {
            return Err(io::Error::other("disk already has volatile writes"));
        }
        disk.file
            .try_lock()
            .map_err(|error| io::Error::other(format!("cannot lock disk for writing: {error}")))?;
        disk.persistent = true;
        Ok(())
    }
    pub fn load(&mut self, addr: u16, data: &[u8]) -> io::Result<()> {
        let start = addr as usize;
        let end = start
            .checked_add(data.len())
            .filter(|&e| e <= 65536)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "program exceeds RAM"))?;
        self.ram[start..end].copy_from_slice(data);
        Ok(())
    }
    pub fn irq(&self) -> bool {
        self.timer & 0x80 != 0 || self.acia.irq() || self.extra_acias.iter().any(Acia::irq)
    }
    #[inline]
    pub fn step(&mut self, cpu: &mut Cpu) -> Result<u64, CpuError> {
        let old = cpu.cycles;
        let irq = self.irq();
        if !cpu.interrupt(self, irq, false, false) {
            cpu.step(self)?;
        }
        let count = cpu.cycles - old;
        self.tick(count);
        Ok(count)
    }
    /// Advance the periodic timer by `count` CPU cycles.
    #[inline]
    pub fn tick(&mut self, count: u64) {
        self.elapsed += count;
        if self.tick_cycles != 0 {
            while self.elapsed >= self.tick_cycles {
                self.elapsed -= self.tick_cycles;
                if self.timer & 2 != 0 {
                    self.timer |= 128;
                }
            }
        }
    }
    pub fn disk_error(&mut self) -> Option<io::Error> {
        self.disk.as_mut().and_then(|d| d.error.take())
    }
    // Recompute mapping invariants only when the private MMU registers change.
    fn refresh_mapping(&mut self) {
        let enabled = self.mapper & 0x20 != 0;
        let task = if self.mapper & 0x40 != 0 { 8 } else { 0 };
        for slot in 0..8 {
            let entry = if enabled {
                self.maps[slot | task]
            } else {
                slot as u8
            };
            let bank = usize::from(entry & 0x7f);
            self.mapping.read_bases[slot] = if bank < self.ram.len() >> 13 {
                bank << 13
            } else {
                self.ram.len()
            };
            self.mapping.writable[slot] = !enabled || entry & 0x80 == 0;
        }
    }
    #[inline]
    fn physical(&self, addr: u16) -> Option<usize> {
        if self.mapper & 0x20 == 0 || (self.fixed_top && addr >= 0xfe00) {
            return Some(usize::from(addr));
        }
        let translated =
            self.mapping.read_bases[usize::from(addr) >> 13] | (usize::from(addr) & 8191);
        // This range check also proves the following RAM access is in bounds.
        (translated < self.ram.len()).then_some(translated)
    }
}
impl Machine {
    #[cold]
    #[inline(never)]
    fn read_io(&mut self, addr: u16) -> u8 {
        if addr == self.acia_base {
            return self.acia.status();
        }
        if addr == self.acia_base.wrapping_add(1) {
            return self.acia.rx.pop_front().unwrap_or(0xff);
        }
        if addr == self.timer_base {
            return self.timer;
        }
        if self.rom.is_some() {
            if (0xffd2..=0xffd7).contains(&addr)
                && let Some(acia) = self.extra_acias.get_mut(usize::from(addr - 0xffd2) / 2)
            {
                return if addr & 1 == 0 {
                    acia.status()
                } else {
                    acia.rx.pop_front().unwrap_or(0xff)
                };
            }
            match addr {
                0xffd2 | 0xffd4 => return 2,
                0xffd3 | 0xffd5 | 0xffd6 | 0xffd7 => return 0xff,
                0xffd8 => return self.disk.as_mut().map_or(0xff, Disk::read),
                0xffd9 => {
                    return self.disk.as_ref().map_or(0x90, |d| match d.state {
                        1 => 0xe0,
                        2 => 0xa0,
                        _ => 0x80,
                    });
                }
                0xffda..=0xffdc | 0xffde..=0xffdf => return 0xff,
                _ => (),
            }
        }
        self.read_memory(addr)
    }
    #[cold]
    #[inline(never)]
    fn write_io(&mut self, addr: u16, v: u8) {
        if addr == self.acia_base {
            self.acia.control(v);
            return;
        }
        if addr == self.acia_base.wrapping_add(1) {
            self.acia.tx.push(v);
            return;
        }
        if addr == self.timer_base {
            self.timer = (self.timer & !2) | (v & 2);
            if v & 128 != 0 {
                self.timer &= 127
            }
            return;
        }
        if self.rom.is_some() {
            if (0xffd2..=0xffd7).contains(&addr)
                && let Some(acia) = self.extra_acias.get_mut(usize::from(addr - 0xffd2) / 2)
            {
                if addr & 1 == 0 {
                    acia.control(v);
                } else {
                    acia.tx.push(v);
                }
                return;
            }
            match addr {
                0xffd2..=0xffd7 => return,
                0xffd8 => {
                    if let Some(d) = &mut self.disk {
                        d.write(v)
                    }
                    return;
                }
                0xffd9 => {
                    if let Some(d) = &mut self.disk {
                        d.command(v)
                    }
                    return;
                }
                0xffda..=0xffdc => {
                    if let Some(d) = &mut self.disk {
                        let shift = (addr - 0xffda) * 8;
                        let mask = if shift == 16 { 0x7f } else { 0xff };
                        d.lba = (d.lba & !(0xff << shift)) | (((v as u32) & mask) << shift)
                    }
                    return;
                }
                0xffde => {
                    if v & 16 == 0 {
                        if v & 0x20 == 0 {
                            self.fixed_top = false
                        } else if self.mapper & 0xa0 == 0xa0 && v & 128 == 0 {
                            self.fixed_top = true
                        }
                        // ROMDIS stays set during the fixed-top handshake.
                        self.rom_disabled =
                            v & 128 != 0 || (self.mapper & 0xa0 == 0xa0 && v & 0x20 != 0);
                        self.mapper = v | if self.rom_disabled { 0x80 } else { 0 };
                        self.refresh_mapping();
                    }
                    return;
                }
                0xffdf => {
                    self.maps[(self.mapper & 15) as usize] = v;
                    self.refresh_mapping();
                    return;
                }
                _ => (),
            }
        }
        self.write_memory(addr, v);
    }
}

impl Machine {
    #[inline]
    fn is_io(&self, addr: u16) -> bool {
        addr == self.acia_base
            || addr == self.acia_base.wrapping_add(1)
            || addr == self.timer_base
            || (self.rom.is_some() && (0xffd2..=0xffdf).contains(&addr))
    }

    // Keep ordinary RAM/ROM access small enough to inline into CPU execution.
    #[inline]
    fn read_memory(&self, addr: u16) -> u8 {
        if let Some(rom) = &self.rom
            && addr >= 0xe000
            && !self.rom_disabled
        {
            return rom[addr as usize - 0xe000];
        }
        self.physical(addr).map_or(0xff, |p| self.ram[p])
    }

    #[inline]
    fn write_memory(&mut self, addr: u16, v: u8) {
        if self.rom.is_some() && addr >= 0xe000 && !self.rom_disabled {
            return;
        }
        if self.mapper & 0x20 != 0
            && !(self.fixed_top && addr >= 0xfe00)
            && !self.mapping.writable[usize::from(addr) >> 13]
        {
            return;
        }
        if let Some(p) = self.physical(addr) {
            self.ram[p] = v;
        }
    }
}
/// MMU registers as seen by a debugger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmuView {
    pub enabled: bool,
    /// Hardware task (0 or 1) currently selected by `$FFDE` bit 6.
    pub task: u8,
    pub rom_disabled: bool,
    pub fixed_top: bool,
    /// Eight map entries per hardware task: bit7 write protect, bits 6-0 bank.
    pub maps: [u8; 16],
}

/// Debugger access without I/O side effects: reads never pop ACIA data and
/// writes never reach device registers.
impl Machine {
    pub fn mmu(&self) -> MmuView {
        MmuView {
            enabled: self.mapper & 0x20 != 0,
            task: (self.mapper >> 6) & 1,
            rom_disabled: self.rom_disabled,
            fixed_top: self.fixed_top,
            maps: self.maps,
        }
    }
    pub fn has_rom(&self) -> bool {
        self.rom.is_some()
    }
    /// Physical RAM address for a CPU address under the current mapping,
    /// or `None` for ROM, I/O and unpopulated banks.
    pub fn translate(&self, addr: u16) -> Option<usize> {
        if self.is_io(addr) || (self.rom.is_some() && addr >= 0xe000 && !self.rom_disabled) {
            return None;
        }
        self.physical(addr)
    }
    /// Physical RAM address through a given hardware task map, ignoring
    /// the active task. Without the MMU this is the flat address.
    pub fn translate_task(&self, task: u8, addr: u16) -> Option<usize> {
        if self.mapper & 0x20 == 0 || (self.fixed_top && addr >= 0xfe00) {
            return Some(usize::from(addr)).filter(|&p| p < self.ram.len());
        }
        let entry = self.maps[usize::from(addr >> 13) | if task & 1 != 0 { 8 } else { 0 }];
        let p = (usize::from(entry & 0x7f) << 13) | (usize::from(addr) & 8191);
        (p < self.ram.len()).then_some(p)
    }
    pub fn peek(&self, addr: u16) -> u8 {
        if self.is_io(addr) {
            if addr == self.acia_base {
                return self.acia.status();
            }
            if addr == self.acia_base.wrapping_add(1) {
                return self.acia.rx.front().copied().unwrap_or(0xff);
            }
            if addr == self.timer_base {
                return self.timer;
            }
            if (0xffd2..=0xffd7).contains(&addr)
                && let Some(acia) = self.extra_acias.get(usize::from(addr - 0xffd2) / 2)
            {
                return if addr & 1 == 0 {
                    acia.status()
                } else {
                    acia.rx.front().copied().unwrap_or(0xff)
                };
            }
            return 0xff;
        }
        self.read_memory(addr)
    }
    pub fn peek16(&self, addr: u16) -> u16 {
        u16::from_be_bytes([self.peek(addr), self.peek(addr.wrapping_add(1))])
    }
    /// Store into RAM under the current mapping; ROM, I/O and protection
    /// behave as for the CPU, except device registers are never written.
    pub fn poke(&mut self, addr: u16, v: u8) {
        if !self.is_io(addr) {
            self.write_memory(addr, v);
        }
    }
}
impl Bus for Machine {
    #[inline]
    fn read(&mut self, addr: u16) -> u8 {
        if self.is_io(addr) {
            return self.read_io(addr);
        }
        self.read_memory(addr)
    }
    #[inline]
    fn write(&mut self, addr: u16, v: u8) {
        if self.is_io(addr) {
            self.write_io(addr, v);
            return;
        }
        self.write_memory(addr, v);
    }
}
