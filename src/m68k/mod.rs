//! Motorola 68000 interpreter: all instructions, exceptions and interrupts,
//! with instruction-level cycle counts (MC68000 User's Manual, section 8).
//!
//! Memory is accessed through [`Bus68k`] with 24-bit addresses. Word and
//! long accesses at odd addresses raise an address error, as on the chip.
//! Prefetch is not modelled: the PC points at the next unread word.
pub mod disasm;
mod exec;

/// The 68000's memory and I/O.
pub trait Bus68k {
    fn read8(&mut self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, value: u8);
    fn read16(&mut self, addr: u32) -> u16 {
        u16::from_be_bytes([self.read8(addr), self.read8(addr.wrapping_add(1))])
    }
    fn write16(&mut self, addr: u32, value: u16) {
        let [h, l] = value.to_be_bytes();
        self.write8(addr, h);
        self.write8(addr.wrapping_add(1), l);
    }
}

pub const C: u16 = 1;
pub const V: u16 = 2;
pub const Z: u16 = 4;
pub const N: u16 = 8;
pub const X: u16 = 16;
pub const S: u16 = 0x2000;
pub const T: u16 = 0x8000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Size {
    B,
    W,
    L,
}
impl Size {
    pub(crate) fn bytes(self) -> u32 {
        match self {
            Size::B => 1,
            Size::W => 2,
            Size::L => 4,
        }
    }
    pub(crate) fn mask(self) -> u32 {
        match self {
            Size::B => 0xff,
            Size::W => 0xffff,
            Size::L => 0xffff_ffff,
        }
    }
    pub(crate) fn msb(self) -> u32 {
        match self {
            Size::B => 0x80,
            Size::W => 0x8000,
            Size::L => 0x8000_0000,
        }
    }
    /// Sign-extend a value of this size to 32 bits.
    pub(crate) fn sext(self, v: u32) -> u32 {
        match self {
            Size::B => v as u8 as i8 as i32 as u32,
            Size::W => v as u16 as i16 as i32 as u32,
            Size::L => v,
        }
    }
}

/// An access that faulted: the address error exception reports these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fault {
    pub addr: u32,
    pub write: bool,
    /// An instruction (program) access rather than data.
    pub program: bool,
}

/// Why an instruction stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trap {
    Address(Fault),
    /// Illegal instruction, line A/F or privilege violation: vector, PC to stack.
    Vector(u8),
}
impl From<Fault> for Trap {
    fn from(f: Fault) -> Self {
        Trap::Address(f)
    }
}

pub(crate) type R<T> = Result<T, Trap>;

#[derive(Debug, Clone)]
pub struct M68k {
    pub d: [u32; 8],
    /// A0-A7; A7 is the active stack pointer (USP or SSP).
    pub a: [u32; 8],
    /// The inactive stack pointer (SSP in user mode, USP in supervisor mode).
    other_sp: u32,
    pub sr: u16,
    /// The address of the next instruction word to fetch.
    pub pc: u32,
    pub cycles: u64,
    /// Stopped by STOP, waiting for an interrupt.
    pub stopped: bool,
    /// Halted by a double fault.
    pub halted: bool,
    /// Requested interrupt level (0-7) from the machine.
    pub irq: u8,
    /// Vector for the requested interrupt; `None` uses the autovector.
    pub irq_vector: Option<u8>,
    /// Set by RESET for the machine to reset its devices.
    pub reset_out: bool,
    pub(crate) ir: u16,
    /// Address of the instruction being executed.
    pub(crate) inst_pc: u32,
    /// Cycles of the current instruction.
    pub(crate) clk: u32,
    /// Cycles spent in the current instruction before its last access,
    /// counted per bus access; an address error adds its 50 to this.
    pub(crate) elapsed: u32,
}

impl Default for M68k {
    fn default() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            other_sp: 0,
            sr: S | 0x0700,
            pc: 0,
            cycles: 0,
            stopped: false,
            halted: false,
            irq: 0,
            irq_vector: None,
            reset_out: false,
            ir: 0,
            inst_pc: 0,
            clk: 0,
            elapsed: 0,
        }
    }
}

impl M68k {
    pub fn supervisor(&self) -> bool {
        self.sr & S != 0
    }
    pub fn usp(&self) -> u32 {
        if self.supervisor() {
            self.other_sp
        } else {
            self.a[7]
        }
    }
    pub fn ssp(&self) -> u32 {
        if self.supervisor() {
            self.a[7]
        } else {
            self.other_sp
        }
    }
    pub fn set_usp(&mut self, v: u32) {
        if self.supervisor() {
            self.other_sp = v
        } else {
            self.a[7] = v
        }
    }
    pub fn set_ssp(&mut self, v: u32) {
        if self.supervisor() {
            self.a[7] = v
        } else {
            self.other_sp = v
        }
    }
    /// Set SR, switching stack pointers when the S bit changes.
    pub fn set_sr(&mut self, v: u16) {
        let v = v & 0xa71f;
        if (v ^ self.sr) & S != 0 {
            std::mem::swap(&mut self.a[7], &mut self.other_sp);
        }
        self.sr = v;
    }
    pub(crate) fn ccr(&self) -> u16 {
        self.sr & 0x1f
    }
    pub(crate) fn set_ccr(&mut self, v: u16) {
        self.sr = (self.sr & 0xff00) | (v & 0x1f);
    }
    pub(crate) fn flag(&self, f: u16) -> bool {
        self.sr & f != 0
    }
    pub(crate) fn set_flag(&mut self, f: u16, on: bool) {
        if on { self.sr |= f } else { self.sr &= !f }
    }

    /// Power-on reset: SSP and PC from vectors 0 and 1.
    pub fn reset(&mut self, bus: &mut dyn Bus68k) {
        self.sr = S | 0x0700;
        self.stopped = false;
        self.halted = false;
        let sp = read32(bus, 0);
        self.a[7] = sp;
        self.pc = read32(bus, 4);
        self.cycles += 40;
    }

    // ---------- memory ----------
    pub(crate) fn fc(&self, program: bool) -> u16 {
        (if self.supervisor() { 4 } else { 0 }) | if program { 2 } else { 1 }
    }
    pub(crate) fn read(&mut self, bus: &mut dyn Bus68k, addr: u32, size: Size) -> R<u32> {
        let a = addr & 0x00ff_ffff;
        if size != Size::B && addr & 1 != 0 {
            return Err(Fault {
                addr,
                write: false,
                program: false,
            }
            .into());
        }
        self.elapsed += if size == Size::L { 8 } else { 4 };
        Ok(match size {
            Size::B => u32::from(bus.read8(a)),
            Size::W => u32::from(bus.read16(a)),
            Size::L => {
                u32::from(bus.read16(a)) << 16 | u32::from(bus.read16((a + 2) & 0x00ff_ffff))
            }
        })
    }
    pub(crate) fn write(&mut self, bus: &mut dyn Bus68k, addr: u32, size: Size, v: u32) -> R<()> {
        let a = addr & 0x00ff_ffff;
        if size != Size::B && addr & 1 != 0 {
            return Err(Fault {
                addr,
                write: true,
                program: false,
            }
            .into());
        }
        self.elapsed += if size == Size::L { 8 } else { 4 };
        match size {
            Size::B => bus.write8(a, v as u8),
            Size::W => bus.write16(a, v as u16),
            Size::L => {
                bus.write16(a, (v >> 16) as u16);
                bus.write16((a + 2) & 0x00ff_ffff, v as u16);
            }
        }
        Ok(())
    }
    /// The next instruction word.
    pub(crate) fn fetch(&mut self, bus: &mut dyn Bus68k) -> R<u16> {
        if self.pc & 1 != 0 {
            return Err(Fault {
                addr: self.pc,
                write: false,
                program: true,
            }
            .into());
        }
        let v = bus.read16(self.pc & 0x00ff_ffff);
        self.pc = self.pc.wrapping_add(2);
        self.elapsed += 4;
        Ok(v)
    }
    pub(crate) fn fetch32(&mut self, bus: &mut dyn Bus68k) -> R<u32> {
        let h = self.fetch(bus)?;
        Ok(u32::from(h) << 16 | u32::from(self.fetch(bus)?))
    }
    pub(crate) fn push(&mut self, bus: &mut dyn Bus68k, size: Size, v: u32) -> R<()> {
        self.a[7] = self.a[7].wrapping_sub(size.bytes());
        self.write(bus, self.a[7], size, v)
    }
    pub(crate) fn pop(&mut self, bus: &mut dyn Bus68k, size: Size) -> R<u32> {
        let v = self.read(bus, self.a[7], size)?;
        self.a[7] = self.a[7].wrapping_add(size.bytes());
        Ok(v)
    }

    // ---------- exceptions ----------
    /// Group 1/2 exception: push PC and SR, enter supervisor mode, jump
    /// through `vector`. `pc` is the PC to stack.
    pub(crate) fn exception(
        &mut self,
        bus: &mut dyn Bus68k,
        vector: u8,
        pc: u32,
        cycles: u32,
    ) -> R<()> {
        let old = self.sr;
        self.set_sr((self.sr | S) & !T);
        self.push(bus, Size::L, pc)?;
        self.push(bus, Size::W, u32::from(old))?;
        self.pc = self.read(bus, u32::from(vector) * 4, Size::L)?;
        self.clk += cycles;
        Ok(())
    }
    /// Address error: the 14-byte group 0 frame.
    pub(crate) fn address_error(&mut self, bus: &mut dyn Bus68k, f: Fault, pc: u32, cycles: u32) {
        let old = self.sr;
        // The function code is the one of the faulting access (before entering supervisor mode).
        let status = (self.ir & 0xffe0)
            | if f.write { 0 } else { 0x10 }
            | if f.program { 8 } else { 0 }
            | self.fc(f.program);
        self.set_sr((self.sr | S) & !T);
        let frame = (|| -> R<()> {
            self.push(bus, Size::L, pc)?;
            self.push(bus, Size::W, u32::from(old))?;
            self.push(bus, Size::W, u32::from(self.ir))?;
            self.push(bus, Size::L, f.addr)?;
            self.push(bus, Size::W, u32::from(status))?;
            self.pc = self.read(bus, 3 * 4, Size::L)?;
            Ok(())
        })();
        if frame.is_err() {
            self.halted = true;
        }
        self.clk += cycles;
    }

    /// Execute one instruction (or take a pending interrupt).
    pub fn step(&mut self, bus: &mut dyn Bus68k) {
        if self.halted {
            self.cycles += 4;
            return;
        }
        self.clk = 0;
        let level = self.irq & 7;
        if level > 0 && (level == 7 || level > ((self.sr >> 8) & 7) as u8) {
            self.stopped = false;
            self.interrupt(bus, level);
            self.cycles += u64::from(self.clk);
            return;
        }
        if self.stopped {
            self.cycles += 4;
            return;
        }
        let trace = self.sr & T != 0;
        self.inst_pc = self.pc;
        let result = self.fetch(bus).and_then(|op| {
            self.ir = op;
            // The opcode word was prefetched by the previous instruction.
            self.elapsed = 0;
            self.exec(bus, op)
        });
        match result {
            Err(Trap::Address(f)) => {
                let pc = self.fault_pc(f);
                self.clk = 0;
                let spent = self.elapsed;
                self.address_error(bus, f, pc, 50 + spent);
                return self.finish();
            }
            Err(Trap::Vector(v)) => {
                // Illegal, line A/F and privilege violations stack the instruction's address.
                let pc = self.inst_pc;
                if let Err(e) = self.exception(bus, v, pc, 34) {
                    self.double(bus, e);
                }
                return self.finish();
            }
            Ok(()) => {}
        }
        if trace && !self.halted {
            let pc = self.pc;
            if let Err(e) = self.exception(bus, 9, pc, 34) {
                self.double(bus, e);
            }
        }
        self.finish();
    }
    fn finish(&mut self) {
        self.cycles += u64::from(self.clk);
    }
    /// A fault while stacking a group 1/2 exception becomes an address error.
    fn double(&mut self, bus: &mut dyn Bus68k, e: Trap) {
        match e {
            Trap::Address(f) => {
                let pc = self.pc;
                self.address_error(bus, f, pc, 50);
            }
            Trap::Vector(_) => self.halted = true,
        }
    }

    fn interrupt(&mut self, bus: &mut dyn Bus68k, level: u8) {
        let old = self.sr;
        let vector = self.irq_vector.unwrap_or(24 + level);
        self.set_sr(((self.sr | S) & !T & !0x0700) | (u16::from(level) << 8));
        let pc = self.pc;
        let r = (|| -> R<()> {
            self.push(bus, Size::L, pc)?;
            self.push(bus, Size::W, u32::from(old))?;
            self.pc = self.read(bus, u32::from(vector) * 4, Size::L)?;
            Ok(())
        })();
        if r.is_err() {
            self.halted = true;
        }
        self.clk += 44;
    }

    /// The PC stacked by an address error: two less than the PC at the
    /// faulting access (for a branch, the target), as the prefetch leaves it.
    fn fault_pc(&self, f: Fault) -> u32 {
        // A branch faults while prefetching its target: two words further back.
        self.pc.wrapping_sub(if f.program { 4 } else { 2 })
    }
}

pub(crate) fn read32(bus: &mut dyn Bus68k, a: u32) -> u32 {
    u32::from(bus.read16(a & 0xff_ffff)) << 16 | u32::from(bus.read16((a + 2) & 0xff_ffff))
}
