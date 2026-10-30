use crate::{Bus, Cpu, CpuError};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

/// A console-compatible subset of the MC6850 (instantaneous transmit).
#[derive(Default)]
pub struct Acia {
    pub control: u8,
    pub rx: VecDeque<u8>,
    pub tx: Vec<u8>,
}
impl Acia {
    pub fn irq(&self) -> bool {
        self.control & 0x80 != 0 && !self.rx.is_empty() || self.control & 0x60 == 0x20
    }
    pub fn status(&self) -> u8 {
        2 | u8::from(!self.rx.is_empty()) | if self.irq() { 0x80 } else { 0 }
    }
    pub fn control(&mut self, v: u8) {
        if v & 3 == 3 {
            self.control = 0;
            self.rx.clear()
        } else {
            self.control = v
        }
    }
}
struct Disk {
    file: File,
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
                self.overlay.insert(self.lba, self.data);
            }
            self.state = 0;
        }
    }
}
struct Mapping {
    read_bases: [usize; 8],
    writable: [bool; 8],
}

/// 64 KiB physical RAM; optional 8 KiB ROM overlay, timer and SD interface.
/// Disk writes are kept in memory, leaving the source image intact.
pub struct Machine {
    pub ram: Box<[u8; 65536]>,
    pub acia: Acia,
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
            ram: Box::new([0; 65536]),
            acia: Acia::default(),
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
    pub fn multicomp(rom: &[u8], disk: File) -> io::Result<Self> {
        let rom: Box<[u8; 8192]> = rom.to_vec().into_boxed_slice().try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "boot ROM must be exactly 8192 bytes",
            )
        })?;
        Ok(Self {
            acia_base: 0xffd0,
            timer_base: 0xffdd,
            rom: Some(rom),
            disk: Some(Disk {
                file: disk,
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
        self.timer & 0x80 != 0 || self.acia.irq()
    }
    #[inline]
    pub fn step(&mut self, cpu: &mut Cpu) -> Result<u64, CpuError> {
        let old = cpu.cycles;
        let irq = self.irq();
        if !cpu.interrupt(self, irq, false, false) {
            cpu.step(self)?;
        }
        let count = cpu.cycles - old;
        self.elapsed += count;
        if self.tick_cycles != 0 {
            while self.elapsed >= self.tick_cycles {
                self.elapsed -= self.tick_cycles;
                if self.timer & 2 != 0 {
                    self.timer |= 128;
                }
            }
        }
        Ok(count)
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
            self.mapping.read_bases[slot] = if bank < 8 { bank << 13 } else { 65536 };
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
        (translated < 65536).then_some(translated)
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
                        } else if self.rom_disabled && v & 128 == 0 {
                            self.fixed_top = true
                        }
                        self.mapper = v;
                        self.rom_disabled = v & 128 != 0 || (self.rom_disabled && v & 0x20 != 0);
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
        if self.mapper & 0x20 != 0 && !self.mapping.writable[usize::from(addr) >> 13] {
            return;
        }
        if let Some(p) = self.physical(addr) {
            self.ram[p] = v;
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
