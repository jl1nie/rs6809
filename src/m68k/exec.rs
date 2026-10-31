//! Instruction execution. Timings follow the MC68000 User's Manual tables
//! (section 8): a base time plus the effective address calculation time.
use super::{Bus68k, C, M68k, N, R, Size, Trap, V, X, Z};

/// A decoded effective address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ea {
    D(usize),
    A(usize),
    M(u32),
    /// A long predecrement operand: writes (and ADDX/SUBX reads) take the low word first.
    Pre(u32),
    Imm(u32),
}

const B: Size = Size::B;
const W: Size = Size::W;
const L: Size = Size::L;

/// A long access whose low word goes first faults reporting address + 2.
fn low_word_first(e: Trap) -> Trap {
    match e {
        Trap::Address(mut f) => {
            f.addr = f.addr.wrapping_add(2);
            Trap::Address(f)
        }
        t => t,
    }
}

/// Effective address calculation time.
pub(crate) fn ea_time(mode: u16, reg: u16, size: Size) -> u32 {
    let l = size == L;
    let t = |bw: u32, long: u32| if l { long } else { bw };
    match mode {
        0 | 1 => 0,
        2 | 3 => t(4, 8),
        4 => t(6, 10),
        5 => t(8, 12),
        6 => t(10, 14),
        _ => match reg {
            0 => t(8, 12),
            1 => t(12, 16),
            2 => t(8, 12),
            3 => t(10, 14),
            _ => t(4, 8),
        },
    }
}

fn size_of(bits: u16) -> Option<Size> {
    match bits & 3 {
        0 => Some(B),
        1 => Some(W),
        2 => Some(L),
        _ => None,
    }
}

impl M68k {
    // ---------- effective addresses ----------
    fn index(&mut self, bus: &mut dyn Bus68k, base: u32) -> R<u32> {
        let ext = self.fetch(bus)?;
        let r = usize::from((ext >> 12) & 7);
        let x = if ext & 0x8000 != 0 {
            self.a[r]
        } else {
            self.d[r]
        };
        let x = if ext & 0x0800 != 0 {
            x
        } else {
            x as u16 as i16 as i32 as u32
        };
        Ok(base
            .wrapping_add(x)
            .wrapping_add(ext as u8 as i8 as i32 as u32))
    }
    /// Decode an effective address, applying (An)+ / -(An) and reading
    /// extension words. Immediates are read here too.
    pub(crate) fn ea(&mut self, bus: &mut dyn Bus68k, mode: u16, reg: u16, size: Size) -> R<Ea> {
        let r = usize::from(reg & 7);
        // Byte steps of A7 keep the stack word-aligned.
        let step = if size == B && r == 7 { 2 } else { size.bytes() };
        Ok(match mode & 7 {
            0 => Ea::D(r),
            1 => Ea::A(r),
            2 => Ea::M(self.a[r]),
            3 => {
                let a = self.a[r];
                self.a[r] = a.wrapping_add(step);
                Ea::M(a)
            }
            4 => {
                self.elapsed += 2;
                self.a[r] = self.a[r].wrapping_sub(step);
                if size == L {
                    Ea::Pre(self.a[r])
                } else {
                    Ea::M(self.a[r])
                }
            }
            5 => {
                let d = self.fetch(bus)? as i16 as i32 as u32;
                Ea::M(self.a[r].wrapping_add(d))
            }
            6 => {
                self.elapsed += 2;
                let b = self.a[r];
                Ea::M(self.index(bus, b)?)
            }
            _ => match r {
                0 => Ea::M(self.fetch(bus)? as i16 as i32 as u32),
                1 => Ea::M(self.fetch32(bus)?),
                2 => {
                    let b = self.pc;
                    let d = self.fetch(bus)? as i16 as i32 as u32;
                    Ea::M(b.wrapping_add(d))
                }
                3 => {
                    self.elapsed += 2;
                    let b = self.pc;
                    Ea::M(self.index(bus, b)?)
                }
                4 => Ea::Imm(match size {
                    B => u32::from(self.fetch(bus)? & 0xff),
                    W => u32::from(self.fetch(bus)?),
                    L => self.fetch32(bus)?,
                }),
                _ => return Err(Trap::Vector(4)),
            },
        })
    }
    pub(crate) fn rd(&mut self, bus: &mut dyn Bus68k, ea: Ea, size: Size) -> R<u32> {
        Ok(match ea {
            Ea::D(r) => self.d[r] & size.mask(),
            Ea::A(r) => self.a[r] & size.mask(),
            Ea::M(a) => self.read(bus, a, size)?,
            // Reads of a predecremented long take the high word first.
            Ea::Pre(a) => self.read(bus, a, size)?,
            Ea::Imm(v) => v & size.mask(),
        })
    }
    pub(crate) fn wr(&mut self, bus: &mut dyn Bus68k, ea: Ea, size: Size, v: u32) -> R<()> {
        match ea {
            Ea::D(r) => self.d[r] = (self.d[r] & !size.mask()) | (v & size.mask()),
            Ea::A(r) => self.a[r] = v,
            Ea::M(a) => self.write(bus, a, size, v)?,
            Ea::Pre(a) => self.write(bus, a, size, v).map_err(low_word_first)?,
            Ea::Imm(_) => return Err(Trap::Vector(4)),
        }
        Ok(())
    }
    /// Data-alterable: Dn or memory except PC-relative and immediate.
    fn alterable(mode: u16, reg: u16) -> bool {
        mode != 1 && !(mode == 7 && reg > 1)
    }
    fn memory_alterable(mode: u16, reg: u16) -> bool {
        mode >= 2 && !(mode == 7 && reg > 1)
    }
    fn control(mode: u16, reg: u16) -> bool {
        matches!(mode, 2 | 5 | 6) || (mode == 7 && reg <= 3)
    }

    // ---------- flags ----------
    fn nz(&mut self, size: Size, r: u32) {
        let r = r & size.mask();
        self.set_flag(N, r & size.msb() != 0);
        self.set_flag(Z, r == 0);
    }
    fn logic_flags(&mut self, size: Size, r: u32) {
        self.nz(size, r);
        self.sr &= !(V | C);
    }
    fn add_flags(&mut self, size: Size, s: u32, d: u32, carry: u32, extend: bool) -> u32 {
        let m = size.mask();
        let (s, d) = (s & m, d & m);
        let wide = u64::from(s) + u64::from(d) + u64::from(carry);
        let r = (wide as u32) & m;
        let c = wide > u64::from(m);
        let v = (!(s ^ d) & (s ^ r)) & size.msb() != 0;
        self.set_flag(C, c);
        self.set_flag(X, c);
        self.set_flag(V, v);
        self.set_flag(N, r & size.msb() != 0);
        if extend {
            if r != 0 {
                self.sr &= !Z
            }
        } else {
            self.set_flag(Z, r == 0);
        }
        r
    }
    /// d - s - borrow, setting XNZVC (CMP leaves X alone: `x = false`).
    fn sub_flags(&mut self, size: Size, s: u32, d: u32, borrow: u32, extend: bool, x: bool) -> u32 {
        let m = size.mask();
        let (s, d) = (s & m, d & m);
        let r = d.wrapping_sub(s).wrapping_sub(borrow) & m;
        let c = u64::from(s) + u64::from(borrow) > u64::from(d);
        let v = ((s ^ d) & (d ^ r)) & size.msb() != 0;
        self.set_flag(C, c);
        if x {
            self.set_flag(X, c);
        }
        self.set_flag(V, v);
        self.set_flag(N, r & size.msb() != 0);
        if extend {
            if r != 0 {
                self.sr &= !Z
            }
        } else {
            self.set_flag(Z, r == 0);
        }
        r
    }
    pub(crate) fn cond(&self, cc: u16) -> bool {
        let (c, v, z, n) = (self.flag(C), self.flag(V), self.flag(Z), self.flag(N));
        match cc & 15 {
            0 => true,
            1 => false,
            2 => !c && !z,
            3 => c || z,
            4 => !c,
            5 => c,
            6 => !z,
            7 => z,
            8 => !v,
            9 => v,
            10 => !n,
            11 => n,
            12 => n == v,
            13 => n != v,
            14 => !z && n == v,
            _ => z || n != v,
        }
    }

    /// Continue at `target`; the prefetch there faults at an odd address.
    fn jump(&mut self, target: u32) -> R<()> {
        self.pc = target;
        if target & 1 != 0 {
            // The fault comes at the target's prefetch: everything but those two words was spent.
            self.elapsed = self.clk.saturating_sub(8);
            return Err(super::Fault {
                addr: target,
                write: false,
                program: true,
            }
            .into());
        }
        Ok(())
    }

    fn privileged(&self) -> R<()> {
        if self.supervisor() {
            Ok(())
        } else {
            Err(Trap::Vector(8))
        }
    }

    /// Execute the instruction whose first word is `op` (already fetched).
    pub(crate) fn exec(&mut self, bus: &mut dyn Bus68k, op: u16) -> R<()> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let rx = usize::from((op >> 9) & 7);
        match op >> 12 {
            0x0 => self.group0(bus, op, mode, reg, rx),
            0x1..=0x3 => self.op_move(bus, op, mode, reg),
            0x4 => self.group4(bus, op, mode, reg, rx),
            0x5 => self.group5(bus, op, mode, reg),
            0x6 => self.branch(bus, op),
            0x7 => {
                if op & 0x100 != 0 {
                    return Err(Trap::Vector(4));
                }
                let v = op as u8 as i8 as i32 as u32;
                self.d[rx] = v;
                self.logic_flags(L, v);
                self.clk += 4;
                Ok(())
            }
            0x8 => self.group8(bus, op, mode, reg, rx),
            0x9 | 0xd => self.add_sub(bus, op, mode, reg, rx),
            0xa => Err(Trap::Vector(10)),
            0xb => self.group_b(bus, op, mode, reg, rx),
            0xc => self.group_c(bus, op, mode, reg, rx),
            0xe => self.shift(bus, op, mode, reg, rx),
            _ => Err(Trap::Vector(11)),
        }
    }

    // ---------- 0: immediates, bit operations, MOVEP ----------
    fn group0(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        if op & 0x0138 == 0x0108 {
            return self.movep(bus, op, reg, rx);
        }
        if op & 0x0100 != 0 {
            let bit = self.d[rx];
            return self.bit_op(bus, op, mode, reg, bit, false);
        }
        let kind = (op >> 9) & 7;
        if kind == 4 {
            let bit = u32::from(self.fetch(bus)? & 0xff);
            return self.bit_op(bus, op, mode, reg, bit, true);
        }
        if kind == 7 {
            return Err(Trap::Vector(4));
        }
        let Some(size) = size_of(op >> 6) else {
            return Err(Trap::Vector(4));
        };
        // ORI/ANDI/EORI to CCR and SR.
        if mode == 7 && reg == 4 && matches!(kind, 0 | 1 | 5) {
            if size == L {
                return Err(Trap::Vector(4));
            }
            let imm = self.fetch(bus)?;
            let to_sr = size == W;
            if to_sr {
                self.privileged()?;
            }
            let cur = if to_sr { self.sr } else { self.ccr() };
            let v = match kind {
                0 => cur | imm,
                1 => cur & imm,
                _ => cur ^ imm,
            };
            if to_sr {
                self.set_sr(v)
            } else {
                self.set_ccr(v)
            }
            self.clk += 20;
            return Ok(());
        }
        if !Self::alterable(mode, reg) {
            return Err(Trap::Vector(4));
        }
        let imm = match size {
            L => self.fetch32(bus)?,
            _ => u32::from(self.fetch(bus)?) & size.mask(),
        };
        let ea = self.ea(bus, mode, reg, size)?;
        let d = self.rd(bus, ea, size)?;
        let reg_dst = mode == 0;
        let r = match kind {
            0 => {
                let r = d | imm;
                self.logic_flags(size, r);
                r
            }
            1 => {
                let r = d & imm;
                self.logic_flags(size, r);
                r
            }
            2 => self.sub_flags(size, imm, d, 0, false, true),
            3 => self.add_flags(size, imm, d, 0, false),
            5 => {
                let r = d ^ imm;
                self.logic_flags(size, r);
                r
            }
            _ => {
                // CMPI
                self.sub_flags(size, imm, d, 0, false, false);
                self.clk += match (size, reg_dst) {
                    (L, true) => 14,
                    (L, false) => 12 + ea_time(mode, reg, size),
                    (_, true) => 8,
                    (_, false) => 8 + ea_time(mode, reg, size),
                };
                return Ok(());
            }
        };
        self.wr(bus, ea, size, r)?;
        self.clk += match (size, reg_dst) {
            (L, true) => 16,
            (L, false) => 20 + ea_time(mode, reg, size),
            (_, true) => 8,
            (_, false) => 12 + ea_time(mode, reg, size),
        };
        Ok(())
    }

    fn bit_op(
        &mut self,
        bus: &mut dyn Bus68k,
        op: u16,
        mode: u16,
        reg: u16,
        bit: u32,
        imm: bool,
    ) -> R<()> {
        let kind = (op >> 6) & 3; // 0 BTST, 1 BCHG, 2 BCLR, 3 BSET
        if mode == 1
            || (kind != 0 && !Self::alterable(mode, reg))
            || (mode == 7 && reg == 4 && (imm || kind != 0))
        {
            return Err(Trap::Vector(4));
        }
        if mode == 0 {
            let n = bit & 31;
            let r = usize::from(reg);
            let set = self.d[r] & (1 << n) != 0;
            self.set_flag(Z, !set);
            match kind {
                1 => self.d[r] ^= 1 << n,
                2 => self.d[r] &= !(1 << n),
                3 => self.d[r] |= 1 << n,
                _ => {}
            }
            let hi = if n >= 16 { 2 } else { 0 };
            self.clk += match (kind, imm) {
                (0, false) => 6,
                (0, true) => 10,
                (2, false) => 8 + hi,
                (2, true) => 12 + hi,
                (_, false) => 6 + hi,
                (_, true) => 10 + hi,
            };
            return Ok(());
        }
        let n = bit & 7;
        let ea = self.ea(bus, mode, reg, B)?;
        let v = self.rd(bus, ea, B)?;
        self.set_flag(Z, v & (1 << n) == 0);
        let r = match kind {
            1 => v ^ (1 << n),
            2 => v & !(1 << n),
            3 => v | (1 << n),
            _ => v,
        };
        if kind != 0 {
            self.wr(bus, ea, B, r)?;
        }
        self.clk += match (kind, imm) {
            (0, false) if mode == 7 && reg == 4 => 6,
            (0, false) => 4,
            (0, true) => 8,
            (_, false) => 8,
            (_, true) => 12,
        } + ea_time(mode, reg, B);
        Ok(())
    }

    fn movep(&mut self, bus: &mut dyn Bus68k, op: u16, reg: u16, rx: usize) -> R<()> {
        let d = self.fetch(bus)? as i16 as i32 as u32;
        let mut a = self.a[usize::from(reg)].wrapping_add(d);
        let long = op & 0x40 != 0;
        let n = if long { 4 } else { 2 };
        if op & 0x80 != 0 {
            let v = self.d[rx];
            for i in (0..n).rev() {
                self.write(bus, a, B, v >> (i * 8))?;
                a = a.wrapping_add(2);
            }
        } else {
            let mut v = 0u32;
            for _ in 0..n {
                v = v << 8 | self.read(bus, a, B)?;
                a = a.wrapping_add(2);
            }
            self.d[rx] = if long {
                v
            } else {
                (self.d[rx] & 0xffff_0000) | v
            };
        }
        self.clk += if long { 24 } else { 16 };
        Ok(())
    }

    // ---------- 1-3: MOVE / MOVEA ----------
    fn op_move(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16) -> R<()> {
        let size = match op >> 12 {
            1 => B,
            3 => W,
            _ => L,
        };
        let dmode = (op >> 6) & 7;
        let dreg = (op >> 9) & 7;
        if dmode == 1 && size == B || (mode == 1 && size == B) {
            return Err(Trap::Vector(4));
        }
        if dmode == 7 && dreg > 1 {
            return Err(Trap::Vector(4));
        }
        let src = self.ea(bus, mode, reg, size)?;
        let v = self.rd(bus, src, size)?;
        if dmode == 1 {
            self.a[usize::from(dreg)] = size.sext(v);
            self.clk += 4 + ea_time(mode, reg, size);
            return Ok(());
        }
        self.logic_flags(size, v);
        if dmode == 4 {
            // -(An): the next opcode is prefetched before the write.
            let r = usize::from(dreg);
            self.a[r] = self.a[r].wrapping_sub(if size == B && r == 7 { 2 } else { size.bytes() });
            let ea = if size == L {
                Ea::Pre(self.a[r])
            } else {
                Ea::M(self.a[r])
            };
            if let Err(e) = self.wr(bus, ea, size, v) {
                self.pc = self.pc.wrapping_add(2);
                self.elapsed += 4;
                if size == L {
                    self.a[r] = self.a[r].wrapping_add(2);
                }
                return Err(e);
            }
        } else if dmode == 3 {
            // (An)+ advances only once the write has gone out.
            let r = usize::from(dreg);
            let a = self.a[r];
            self.wr(bus, Ea::M(a), size, v)?;
            self.a[r] = a.wrapping_add(if size == B && r == 7 { 2 } else { size.bytes() });
        } else {
            let dst = self.ea(bus, dmode, dreg, size)?;
            let w = self.wr(bus, dst, size, v);
            if dmode == 7 && dreg == 1 && mode >= 2 && !(mode == 7 && reg == 4) && w.is_err() {
                // After a memory source, the write overlaps the fetch of the address's low word.
                self.pc = self.pc.wrapping_sub(2);
                self.elapsed -= 4;
            }
            w?;
        }
        // A predecrementing destination costs the same as (An).
        let dt = if dmode == 4 {
            ea_time(2, 0, size)
        } else {
            ea_time(dmode, dreg, size)
        };
        self.clk += 4 + ea_time(mode, reg, size) + dt;
        Ok(())
    }

    // ---------- 4: miscellaneous ----------
    fn group4(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        if op & 0x01c0 == 0x01c0 {
            // LEA
            if !Self::control(mode, reg) {
                return Err(Trap::Vector(4));
            }
            let Ea::M(a) = self.ea(bus, mode, reg, L)? else {
                unreachable!()
            };
            self.a[rx] = a;
            self.clk += lea_time(mode, reg);
            return Ok(());
        }
        if op & 0x01c0 == 0x0180 {
            return self.chk(bus, op, mode, reg, rx);
        }
        match op {
            0x4afc => return Err(Trap::Vector(4)),
            0x4e70 => {
                self.privileged()?;
                self.reset_out = true;
                self.clk += 132;
                return Ok(());
            }
            0x4e71 => {
                self.clk += 4;
                return Ok(());
            }
            0x4e72 => {
                let imm = self.fetch(bus)?;
                self.privileged()?;
                self.set_sr(imm);
                self.stopped = true;
                self.clk += 4;
                return Ok(());
            }
            0x4e73 => {
                self.privileged()?;
                let sr = self.read(bus, self.a[7], W)? as u16;
                let pc = self.read(bus, self.a[7].wrapping_add(2), L)?;
                self.a[7] = self.a[7].wrapping_add(6);
                self.set_sr(sr);
                self.clk += 20;
                self.jump(pc)?;
                return Ok(());
            }
            0x4e75 => {
                let t = self.pop(bus, L)?;
                self.clk += 16;
                self.jump(t)?;
                return Ok(());
            }
            0x4e76 => {
                self.clk += 4;
                if self.flag(V) {
                    let pc = self.pc;
                    self.exception(bus, 7, pc, 30)?;
                }
                return Ok(());
            }
            0x4e77 => {
                let ccr = self.pop(bus, W)? as u16;
                self.set_ccr(ccr);
                let t = self.pop(bus, L)?;
                self.clk += 20;
                self.jump(t)?;
                return Ok(());
            }
            _ => {}
        }
        match op & 0xfff0 {
            0x4e40 => {
                let pc = self.pc;
                return self.exception(bus, 32 + (op & 15) as u8, pc, 34);
            }
            0x4e50 => {
                let r = usize::from(reg);
                if op & 8 == 0 {
                    // LINK
                    let d = self.fetch(bus)? as i16 as i32 as u32;
                    let v = self.a[r];
                    self.push(bus, L, if r == 7 { v.wrapping_sub(4) } else { v })?;
                    self.a[r] = self.a[7];
                    self.a[7] = self.a[7].wrapping_add(d);
                    self.clk += 16;
                } else {
                    // UNLK
                    let a = self.a[r];
                    let v = self.read(bus, a, L)?;
                    self.a[7] = a.wrapping_add(4);
                    self.a[r] = v;
                    self.clk += 12;
                }
                return Ok(());
            }
            0x4e60 => {
                self.privileged()?;
                let r = usize::from(reg);
                if op & 8 == 0 {
                    let v = self.a[r];
                    self.set_usp(v);
                } else {
                    self.a[r] = self.usp();
                }
                self.clk += 4;
                return Ok(());
            }
            _ => {}
        }
        match op & 0xffc0 {
            0x4e80 | 0x4ec0 => {
                // JSR / JMP
                if !Self::control(mode, reg) {
                    return Err(Trap::Vector(4));
                }
                let Ea::M(a) = self.ea(bus, mode, reg, L)? else {
                    unreachable!()
                };
                let jsr = op & 0x40 == 0;
                self.clk += jump_time(mode, reg, jsr);
                if a & 1 != 0 {
                    // The target is fetched before JSR stacks the return address.
                    self.clk = jump_time(mode, reg, false);
                    self.jump(a)?;
                }
                if jsr {
                    let pc = self.pc;
                    self.push(bus, L, pc)?;
                }
                self.jump(a)?;
                return Ok(());
            }
            0x40c0 => {
                // MOVE from SR
                if !Self::alterable(mode, reg) {
                    return Err(Trap::Vector(4));
                }
                let ea = self.ea(bus, mode, reg, W)?;
                if mode != 0 {
                    self.rd(bus, ea, W)?;
                }
                let sr = u32::from(self.sr);
                self.wr(bus, ea, W, sr)?;
                self.clk += if mode == 0 {
                    6
                } else {
                    8 + ea_time(mode, reg, W)
                };
                return Ok(());
            }
            0x44c0 | 0x46c0 => {
                // MOVE to CCR / SR
                if mode == 1 {
                    return Err(Trap::Vector(4));
                }
                let to_sr = op & 0x0200 != 0;
                if to_sr {
                    self.privileged()?;
                }
                let ea = self.ea(bus, mode, reg, W)?;
                let v = self.rd(bus, ea, W)? as u16;
                if to_sr {
                    self.set_sr(v)
                } else {
                    self.set_ccr(v)
                }
                self.clk += 12 + ea_time(mode, reg, W);
                return Ok(());
            }
            0x4840 => {
                if mode == 0 {
                    let r = usize::from(reg);
                    self.d[r] = self.d[r].rotate_left(16);
                    let v = self.d[r];
                    self.logic_flags(L, v);
                    self.clk += 4;
                } else {
                    // PEA
                    if !Self::control(mode, reg) {
                        return Err(Trap::Vector(4));
                    }
                    let Ea::M(a) = self.ea(bus, mode, reg, L)? else {
                        unreachable!()
                    };
                    self.push(bus, L, a)?;
                    self.clk += lea_time(mode, reg) + 8;
                }
                return Ok(());
            }
            0x4800 => return self.nbcd(bus, mode, reg),
            0x4ac0 => {
                // TAS
                if !Self::alterable(mode, reg) {
                    return Err(Trap::Vector(4));
                }
                let ea = self.ea(bus, mode, reg, B)?;
                let v = self.rd(bus, ea, B)?;
                self.logic_flags(B, v);
                self.wr(bus, ea, B, v | 0x80)?;
                self.clk += if mode == 0 {
                    4
                } else {
                    10 + ea_time(mode, reg, B)
                };
                return Ok(());
            }
            _ => {}
        }
        if op & 0xfb80 == 0x4880 {
            if mode == 0 {
                // EXT
                let r = usize::from(reg);
                if op & 0x40 == 0 {
                    let v = self.d[r] as u8 as i8 as i16 as u16 as u32;
                    self.d[r] = (self.d[r] & 0xffff_0000) | v;
                    self.logic_flags(W, v);
                } else {
                    self.d[r] = self.d[r] as u16 as i16 as i32 as u32;
                    let v = self.d[r];
                    self.logic_flags(L, v);
                }
                self.clk += 4;
                return Ok(());
            }
            return self.movem(bus, op, mode, reg);
        }
        // NEGX, CLR, NEG, NOT, TST
        let Some(size) = size_of(op >> 6) else {
            return Err(Trap::Vector(4));
        };
        let kind = (op >> 8) & 0xf;
        if !matches!(kind, 0 | 2 | 4 | 6 | 0xa) {
            return Err(Trap::Vector(4));
        }
        if kind == 0xa {
            if mode == 1 || (mode == 7 && reg == 4) {
                return Err(Trap::Vector(4));
            }
            let ea = self.ea(bus, mode, reg, size)?;
            let v = self.rd(bus, ea, size)?;
            self.logic_flags(size, v);
            self.clk += 4 + ea_time(mode, reg, size);
            return Ok(());
        }
        if !Self::alterable(mode, reg) {
            return Err(Trap::Vector(4));
        }
        let ea = self.ea(bus, mode, reg, size)?;
        let d = self.rd(bus, ea, size)?;
        let r = match kind {
            0 => {
                let x = u32::from(self.flag(X));
                self.sub_flags(size, d, 0, x, true, true)
            }
            2 => {
                self.sr = (self.sr & !(N | V | C)) | Z;
                0
            }
            4 => self.sub_flags(size, d, 0, 0, false, true),
            _ => {
                let r = !d & size.mask();
                self.logic_flags(size, r);
                r
            }
        };
        self.wr(bus, ea, size, r)?;
        self.clk += match (size, mode) {
            (L, 0) => 6,
            (_, 0) => 4,
            (L, _) => 12 + ea_time(mode, reg, size),
            _ => 8 + ea_time(mode, reg, size),
        };
        Ok(())
    }

    fn chk(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        if op & 0x40 != 0 || mode == 1 {
            return Err(Trap::Vector(4));
        }
        let ea = self.ea(bus, mode, reg, W)?;
        let bound = self.rd(bus, ea, W)? as u16 as i16;
        let v = self.d[rx] as u16 as i16;
        let et = ea_time(mode, reg, W);
        self.sr &= !(Z | V | C);
        // The bound is tested first: above it traps in 38, below zero in 40.
        let trap = if v > bound {
            Some(38)
        } else if v < 0 {
            Some(40)
        } else {
            None
        };
        match trap {
            Some(t) => {
                self.set_flag(N, v < 0);
                let pc = self.pc;
                self.exception(bus, 6, pc, t + et)?;
            }
            None => self.clk += 10 + et,
        }
        Ok(())
    }

    fn nbcd(&mut self, bus: &mut dyn Bus68k, mode: u16, reg: u16) -> R<()> {
        if !Self::alterable(mode, reg) {
            return Err(Trap::Vector(4));
        }
        let ea = self.ea(bus, mode, reg, B)?;
        let d = self.rd(bus, ea, B)?;
        let r = self.bcd_sub(d, 0);
        self.wr(bus, ea, B, r)?;
        self.clk += if mode == 0 {
            6
        } else {
            8 + ea_time(mode, reg, B)
        };
        Ok(())
    }
    /// BCD add, including invalid digits and the undocumented V and N
    /// (flamewing's analysis of the 68000).
    fn bcd_add(&mut self, s: u32, d: u32) -> u32 {
        let x = u32::from(self.flag(X));
        let ss = (s + d + x) & 0xff;
        let bc = ((s & d) | (!ss & s) | (!ss & d)) & 0x88;
        let dc = (((ss + 0x66) ^ ss) & 0x110) >> 1;
        let corf = (bc | dc) - ((bc | dc) >> 2);
        let rr = (ss + corf) & 0xff;
        let c = (bc | (ss & !rr)) & 0x80 != 0;
        self.bcd_flags(c, (!ss & rr) & 0x80 != 0, rr);
        rr
    }
    /// BCD subtract d - s - X (SBCD, NBCD with d = 0).
    fn bcd_sub(&mut self, s: u32, d: u32) -> u32 {
        let x = u32::from(self.flag(X));
        let dd = d.wrapping_sub(s).wrapping_sub(x) & 0xff;
        let bc = ((!d & s) | (dd & !d) | (dd & s)) & 0x88;
        let corf = bc - (bc >> 2);
        let rr = dd.wrapping_sub(corf) & 0xff;
        let c = (bc | (!dd & rr)) & 0x80 != 0;
        self.bcd_flags(c, (dd & !rr) & 0x80 != 0, rr);
        rr
    }
    fn bcd_flags(&mut self, c: bool, v: bool, r: u32) {
        self.set_flag(C, c);
        self.set_flag(X, c);
        self.set_flag(V, v);
        self.set_flag(N, r & 0x80 != 0);
        if r != 0 {
            self.sr &= !Z;
        }
    }

    fn movem(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16) -> R<()> {
        let mask = self.fetch(bus)?;
        let size = if op & 0x40 != 0 { L } else { W };
        let to_regs = op & 0x0400 != 0;
        let n = mask.count_ones();
        let per = if size == L { 8 } else { 4 };
        let r = usize::from(reg);
        if to_regs {
            if mode < 2 || mode == 4 || (mode == 7 && reg == 4) {
                return Err(Trap::Vector(4));
            }
            let Ea::M(mut a) = (if mode == 3 {
                Ea::M(self.a[r])
            } else {
                self.ea(bus, mode, reg, size)?
            }) else {
                unreachable!()
            };
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let v = self.read(bus, a, size);
                    if v.is_err() && mode == 3 {
                        self.a[r] = a.wrapping_add(2);
                    }
                    let v = size.sext(v?);
                    if i < 8 {
                        self.d[i] = v
                    } else {
                        self.a[i - 8] = v
                    }
                    a = a.wrapping_add(size.bytes());
                }
            }
            // The 68000 reads one extra word past the list.
            self.read(bus, a, W)?;
            if mode == 3 {
                self.a[r] = a;
            }
            self.clk += 12
                + n * per
                + match mode {
                    5 => 4,
                    6 => 6,
                    7 => match reg {
                        0 | 2 => 4,
                        3 => 6,
                        _ => 8,
                    },
                    _ => 0,
                };
        } else {
            if !Self::memory_alterable(mode, reg) || mode == 3 {
                return Err(Trap::Vector(4));
            }
            if mode == 4 {
                // Predecrement: the mask is reversed (bit 0 = A7).
                let mut a = self.a[r];
                for i in 0..16 {
                    if mask & (1 << i) != 0 {
                        let k = 15 - i;
                        a = a.wrapping_sub(size.bytes());
                        let v = if k < 8 { self.d[k] } else { self.a[k - 8] };
                        let w = self.write(bus, a, size, v);
                        if size == L {
                            w.map_err(low_word_first)?
                        } else {
                            w?
                        }
                    }
                }
                self.a[r] = a;
            } else {
                let Ea::M(mut a) = self.ea(bus, mode, reg, size)? else {
                    unreachable!()
                };
                for i in 0..16 {
                    if mask & (1 << i) != 0 {
                        let v = if i < 8 { self.d[i] } else { self.a[i - 8] };
                        self.write(bus, a, size, v)?;
                        a = a.wrapping_add(size.bytes());
                    }
                }
            }
            self.clk += 8
                + n * per
                + match mode {
                    5 => 4,
                    6 => 6,
                    7 => {
                        if reg == 0 {
                            4
                        } else {
                            8
                        }
                    }
                    _ => 0,
                };
        }
        Ok(())
    }

    // ---------- 5: ADDQ, SUBQ, Scc, DBcc ----------
    fn group5(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16) -> R<()> {
        let cc = (op >> 8) & 15;
        let Some(size) = size_of(op >> 6) else {
            if mode == 1 {
                // DBcc
                let disp = self.fetch(bus)? as i16 as i32 as u32;
                if self.cond(cc) {
                    self.clk += 12;
                    return Ok(());
                }
                let r = usize::from(reg);
                let c = (self.d[r] as u16).wrapping_sub(1);
                self.d[r] = (self.d[r] & 0xffff_0000) | u32::from(c);
                if c == 0xffff {
                    self.clk += 14;
                } else {
                    self.clk += 10;
                    self.jump(self.inst_pc.wrapping_add(2).wrapping_add(disp))?;
                }
                return Ok(());
            }
            // Scc
            if !Self::alterable(mode, reg) {
                return Err(Trap::Vector(4));
            }
            let t = self.cond(cc);
            let ea = self.ea(bus, mode, reg, B)?;
            if mode != 0 {
                self.rd(bus, ea, B)?;
            }
            self.wr(bus, ea, B, if t { 0xff } else { 0 })?;
            self.clk += if mode == 0 {
                if t { 6 } else { 4 }
            } else {
                8 + ea_time(mode, reg, B)
            };
            return Ok(());
        };
        let q = match (op >> 9) & 7 {
            0 => 8,
            n => u32::from(n),
        };
        let sub = op & 0x100 != 0;
        if mode == 1 {
            if size == B {
                return Err(Trap::Vector(4));
            }
            let r = usize::from(reg);
            self.a[r] = if sub {
                self.a[r].wrapping_sub(q)
            } else {
                self.a[r].wrapping_add(q)
            };
            self.clk += if size == L { 6 } else { 8 };
            return Ok(());
        }
        if !Self::alterable(mode, reg) {
            return Err(Trap::Vector(4));
        }
        let ea = self.ea(bus, mode, reg, size)?;
        let d = self.rd(bus, ea, size)?;
        let r = if sub {
            self.sub_flags(size, q, d, 0, false, true)
        } else {
            self.add_flags(size, q, d, 0, false)
        };
        self.wr(bus, ea, size, r)?;
        self.clk += match (size, mode) {
            (L, 0) => 8,
            (_, 0) => 4,
            (L, _) => 12 + ea_time(mode, reg, size),
            _ => 8 + ea_time(mode, reg, size),
        };
        Ok(())
    }

    // ---------- 6: Bcc, BRA, BSR ----------
    fn branch(&mut self, bus: &mut dyn Bus68k, op: u16) -> R<()> {
        let cc = (op >> 8) & 15;
        let base = self.pc;
        let (disp, word) = match op & 0xff {
            0 => (self.fetch(bus)? as i16 as i32 as u32, true),
            d => (d as u8 as i8 as i32 as u32, false),
        };
        let target = base.wrapping_add(disp);
        if cc == 1 {
            let pc = self.pc;
            self.push(bus, L, pc)?;
            self.clk += 18;
            self.jump(target)?;
        } else if self.cond(cc) {
            self.clk += 10;
            self.jump(target)?;
        } else {
            self.clk += if word { 12 } else { 8 };
        }
        Ok(())
    }

    // ---------- 8: OR, DIVU, DIVS, SBCD ----------
    fn group8(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        if op & 0x00c0 == 0x00c0 {
            return self.divide(bus, op & 0x100 != 0, mode, reg, rx);
        }
        if op & 0x01f0 == 0x0100 {
            return self.bcd_x(bus, op, mode, reg, rx, false);
        }
        self.logic(bus, op, mode, reg, rx, |a, b| a | b)
    }

    fn divide(
        &mut self,
        bus: &mut dyn Bus68k,
        signed: bool,
        mode: u16,
        reg: u16,
        rx: usize,
    ) -> R<()> {
        if mode == 1 {
            return Err(Trap::Vector(4));
        }
        let ea = self.ea(bus, mode, reg, W)?;
        let s = self.rd(bus, ea, W)?;
        let et = ea_time(mode, reg, W);
        let dividend = self.d[rx];
        if s == 0 {
            self.sr &= !(N | Z | V | C);
            let pc = self.pc;
            return self.exception(bus, 5, pc, 38 + et);
        }
        self.sr &= !C;
        if signed {
            let divisor = s as u16 as i16 as i32;
            let dv = dividend as i32;
            self.clk += divs_time(dv, divisor as i16) + et;
            let q = i64::from(dv) / i64::from(divisor);
            let rem = i64::from(dv) % i64::from(divisor);
            if !(-32768..=32767).contains(&q) {
                // Overflow: V set, C clear, N and Z unchanged.
                self.sr |= V;
                return Ok(());
            }
            let q = q as i16 as u16 as u32;
            self.d[rx] = (rem as i16 as u16 as u32) << 16 | q;
            self.sr &= !V;
            self.nz(W, q);
        } else {
            self.clk += divu_time(dividend, s as u16) + et;
            let q = dividend / s;
            if q > 0xffff {
                self.sr |= V;
                return Ok(());
            }
            self.d[rx] = (dividend % s) << 16 | q;
            self.sr &= !V;
            self.nz(W, q);
        }
        Ok(())
    }

    /// ABCD / SBCD: Dy,Dx or -(Ay),-(Ax).
    fn bcd_x(
        &mut self,
        bus: &mut dyn Bus68k,
        op: u16,
        _mode: u16,
        reg: u16,
        rx: usize,
        add: bool,
    ) -> R<()> {
        let ry = usize::from(reg);
        if op & 8 == 0 {
            let (s, d) = (self.d[ry] & 0xff, self.d[rx] & 0xff);
            let r = if add {
                self.bcd_add(s, d)
            } else {
                self.bcd_sub(s, d)
            };
            self.d[rx] = (self.d[rx] & !0xff) | r;
            self.clk += 6;
        } else {
            let sa = self.ea(bus, 4, reg, B)?;
            let s = self.rd(bus, sa, B)?;
            let da = self.ea(bus, 4, rx as u16, B)?;
            let d = self.rd(bus, da, B)?;
            let r = if add {
                self.bcd_add(s, d)
            } else {
                self.bcd_sub(s, d)
            };
            self.wr(bus, da, B, r)?;
            self.clk += 18;
        }
        Ok(())
    }

    /// OR / AND: <ea>,Dn or Dn,<ea>.
    fn logic(
        &mut self,
        bus: &mut dyn Bus68k,
        op: u16,
        mode: u16,
        reg: u16,
        rx: usize,
        f: fn(u32, u32) -> u32,
    ) -> R<()> {
        let Some(size) = size_of(op >> 6) else {
            return Err(Trap::Vector(4));
        };
        if op & 0x100 == 0 {
            if mode == 1 {
                return Err(Trap::Vector(4));
            }
            let ea = self.ea(bus, mode, reg, size)?;
            let s = self.rd(bus, ea, size)?;
            let r = f(s, self.d[rx]) & size.mask();
            self.d[rx] = (self.d[rx] & !size.mask()) | r;
            self.logic_flags(size, r);
            self.clk += match size {
                L => {
                    if mode == 0 || (mode == 7 && reg == 4) {
                        8
                    } else {
                        6
                    }
                }
                _ => 4,
            } + ea_time(mode, reg, size);
        } else {
            if !Self::memory_alterable(mode, reg) {
                return Err(Trap::Vector(4));
            }
            let ea = self.ea(bus, mode, reg, size)?;
            let d = self.rd(bus, ea, size)?;
            let r = f(self.d[rx], d) & size.mask();
            self.logic_flags(size, r);
            self.wr(bus, ea, size, r)?;
            self.clk += if size == L { 12 } else { 8 } + ea_time(mode, reg, size);
        }
        Ok(())
    }

    // ---------- 9 / D: SUB, SUBA, SUBX, ADD, ADDA, ADDX ----------
    fn add_sub(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        let add = op >> 12 == 0xd;
        if op & 0x00c0 == 0x00c0 {
            // ADDA / SUBA
            let size = if op & 0x100 != 0 { L } else { W };
            let ea = self.ea(bus, mode, reg, size)?;
            let s = size.sext(self.rd(bus, ea, size)?);
            self.a[rx] = if add {
                self.a[rx].wrapping_add(s)
            } else {
                self.a[rx].wrapping_sub(s)
            };
            self.clk += match size {
                L => {
                    if mode <= 1 || (mode == 7 && reg == 4) {
                        8
                    } else {
                        6
                    }
                }
                _ => 8,
            } + ea_time(mode, reg, size);
            return Ok(());
        }
        let Some(size) = size_of(op >> 6) else {
            unreachable!()
        };
        if op & 0x0130 == 0x0100 {
            // ADDX / SUBX
            let x = u32::from(self.flag(X));
            let ry = usize::from(reg);
            if op & 8 == 0 {
                let (s, d) = (self.d[ry], self.d[rx]);
                let r = if add {
                    self.add_flags(size, s, d, x, true)
                } else {
                    self.sub_flags(size, s, d, x, true, true)
                };
                self.d[rx] = (self.d[rx] & !size.mask()) | r;
                self.clk += if size == L { 8 } else { 4 };
            } else {
                // ADDX/SUBX -(Ay),-(Ax) read the low word of a long first.
                let lw = |r: R<u32>| {
                    if size == L {
                        r.map_err(low_word_first)
                    } else {
                        r
                    }
                };
                let sa = self.ea(bus, 4, reg, size)?;
                let s = lw(self.rd(bus, sa, size));
                if s.is_err() && size == L {
                    self.a[ry] = self.a[ry].wrapping_add(2);
                }
                let s = s?;
                let da = self.ea(bus, 4, rx as u16, size)?;
                self.elapsed -= 2;
                let d = lw(self.rd(bus, da, size));
                if d.is_err() && size == L {
                    self.a[rx] = self.a[rx].wrapping_add(2);
                }
                let d = d?;
                let r = if add {
                    self.add_flags(size, s, d, x, true)
                } else {
                    self.sub_flags(size, s, d, x, true, true)
                };
                self.wr(bus, da, size, r)?;
                self.clk += if size == L { 30 } else { 18 };
            }
            return Ok(());
        }
        if op & 0x100 == 0 {
            if mode == 1 && size == B {
                return Err(Trap::Vector(4));
            }
            let ea = self.ea(bus, mode, reg, size)?;
            let s = self.rd(bus, ea, size)?;
            let d = self.d[rx];
            let r = if add {
                self.add_flags(size, s, d, 0, false)
            } else {
                self.sub_flags(size, s, d, 0, false, true)
            };
            self.d[rx] = (self.d[rx] & !size.mask()) | r;
            self.clk += match size {
                L => {
                    if mode <= 1 || (mode == 7 && reg == 4) {
                        8
                    } else {
                        6
                    }
                }
                _ => 4,
            } + ea_time(mode, reg, size);
        } else {
            if !Self::memory_alterable(mode, reg) {
                return Err(Trap::Vector(4));
            }
            let ea = self.ea(bus, mode, reg, size)?;
            let d = self.rd(bus, ea, size)?;
            let s = self.d[rx];
            let r = if add {
                self.add_flags(size, s, d, 0, false)
            } else {
                self.sub_flags(size, s, d, 0, false, true)
            };
            self.wr(bus, ea, size, r)?;
            self.clk += if size == L { 12 } else { 8 } + ea_time(mode, reg, size);
        }
        Ok(())
    }

    // ---------- B: CMP, CMPA, CMPM, EOR ----------
    fn group_b(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        if op & 0x00c0 == 0x00c0 {
            let size = if op & 0x100 != 0 { L } else { W };
            let ea = self.ea(bus, mode, reg, size)?;
            let s = size.sext(self.rd(bus, ea, size)?);
            let d = self.a[rx];
            self.sub_flags(L, s, d, 0, false, false);
            self.clk += 6 + ea_time(mode, reg, size);
            return Ok(());
        }
        let Some(size) = size_of(op >> 6) else {
            unreachable!()
        };
        if op & 0x100 == 0 {
            if mode == 1 && size == B {
                return Err(Trap::Vector(4));
            }
            let ea = self.ea(bus, mode, reg, size)?;
            let s = self.rd(bus, ea, size)?;
            let d = self.d[rx];
            self.sub_flags(size, s, d, 0, false, false);
            self.clk += if size == L { 6 } else { 4 } + ea_time(mode, reg, size);
            return Ok(());
        }
        if mode == 1 {
            // CMPM (Ay)+,(Ax)+
            let sa = self.ea(bus, 3, reg, size)?;
            let s = self.rd(bus, sa, size)?;
            let da = self.ea(bus, 3, rx as u16, size)?;
            let d = self.rd(bus, da, size)?;
            self.sub_flags(size, s, d, 0, false, false);
            self.clk += if size == L { 20 } else { 12 };
            return Ok(());
        }
        // EOR Dn,<ea>
        if !Self::alterable(mode, reg) {
            return Err(Trap::Vector(4));
        }
        let ea = self.ea(bus, mode, reg, size)?;
        let d = self.rd(bus, ea, size)?;
        let r = (self.d[rx] ^ d) & size.mask();
        self.logic_flags(size, r);
        self.wr(bus, ea, size, r)?;
        self.clk += match (size, mode) {
            (L, 0) => 8,
            (_, 0) => 4,
            (L, _) => 12 + ea_time(mode, reg, size),
            _ => 8 + ea_time(mode, reg, size),
        };
        Ok(())
    }

    // ---------- C: AND, MULU, MULS, ABCD, EXG ----------
    fn group_c(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        if op & 0x00c0 == 0x00c0 {
            if mode == 1 {
                return Err(Trap::Vector(4));
            }
            let signed = op & 0x100 != 0;
            let ea = self.ea(bus, mode, reg, W)?;
            let s = self.rd(bus, ea, W)?;
            let r = if signed {
                (s as u16 as i16 as i32).wrapping_mul(self.d[rx] as u16 as i16 as i32) as u32
            } else {
                s * (self.d[rx] & 0xffff)
            };
            self.d[rx] = r;
            self.logic_flags(L, r);
            // MULU: one step per 1 bit; MULS: per 01/10 pair in the source with a 0 appended.
            let bits = if signed {
                (((s << 1) ^ s) & 0xffff).count_ones()
            } else {
                s.count_ones()
            };
            self.clk += 38 + 2 * bits + ea_time(mode, reg, W);
            return Ok(());
        }
        if op & 0x01f0 == 0x0100 {
            return self.bcd_x(bus, op, mode, reg, rx, true);
        }
        if op & 0x0130 == 0x0100 {
            // EXG
            let ry = usize::from(reg);
            match (op >> 3) & 0x1f {
                0x08 => self.d.swap(rx, ry),
                0x09 => self.a.swap(rx, ry),
                0x11 => std::mem::swap(&mut self.d[rx], &mut self.a[ry]),
                _ => return Err(Trap::Vector(4)),
            }
            self.clk += 6;
            return Ok(());
        }
        self.logic(bus, op, mode, reg, rx, |a, b| a & b)
    }

    // ---------- E: shifts and rotates ----------
    fn shift(&mut self, bus: &mut dyn Bus68k, op: u16, mode: u16, reg: u16, rx: usize) -> R<()> {
        let left = op & 0x100 != 0;
        let Some(size) = size_of(op >> 6) else {
            // Memory, word, by one.
            if op & 0x0800 != 0 || !Self::memory_alterable(mode, reg) {
                return Err(Trap::Vector(4));
            }
            let kind = (op >> 9) & 3;
            let ea = self.ea(bus, mode, reg, W)?;
            let v = self.rd(bus, ea, W)?;
            let r = self.shift_value(kind, left, W, v, 1);
            self.wr(bus, ea, W, r)?;
            self.clk += 8 + ea_time(mode, reg, W);
            return Ok(());
        };
        let kind = (op >> 3) & 3;
        let count = if op & 0x20 != 0 {
            self.d[rx] & 63
        } else {
            match rx {
                0 => 8,
                n => n as u32,
            }
        };
        let r = usize::from(reg);
        let v = self.d[r] & size.mask();
        let res = self.shift_value(kind, left, size, v, count);
        self.d[r] = (self.d[r] & !size.mask()) | res;
        self.clk += if size == L { 8 } else { 6 } + 2 * count;
        Ok(())
    }

    /// One shift/rotate of `count` steps: kind 0 AS, 1 LS, 2 ROX, 3 RO.
    fn shift_value(&mut self, kind: u16, left: bool, size: Size, v: u32, count: u32) -> u32 {
        let bits = size.bytes() * 8;
        let msb = size.msb();
        let m = size.mask();
        let mut r = v & m;
        let mut c = false;
        let mut vflag = false;
        let mut x = self.flag(X);
        for _ in 0..count {
            match (kind, left) {
                (0, true) | (1, true) => {
                    c = r & msb != 0;
                    let nr = (r << 1) & m;
                    if kind == 0 && (nr ^ r) & msb != 0 {
                        vflag = true;
                    }
                    r = nr;
                    x = c;
                }
                (0, false) => {
                    c = r & 1 != 0;
                    r = (r >> 1) | (r & msb);
                    x = c;
                }
                (1, false) => {
                    c = r & 1 != 0;
                    r >>= 1;
                    x = c;
                }
                (2, true) => {
                    c = r & msb != 0;
                    r = ((r << 1) & m) | u32::from(x);
                    x = c;
                }
                (2, false) => {
                    c = r & 1 != 0;
                    r = (r >> 1) | if x { msb } else { 0 };
                    x = c;
                }
                (3, true) => {
                    c = r & msb != 0;
                    r = ((r << 1) & m) | u32::from(c);
                }
                _ => {
                    c = r & 1 != 0;
                    r = (r >> 1) | if c { msb } else { 0 };
                }
            }
        }
        if kind == 0 && !left && count > bits {
            // Shifted right past the width: the reference leaves C and X clear.
            c = false;
            x = false;
        }
        if count == 0 {
            // No shift: C is cleared, or copies X for ROX.
            c = kind == 2 && self.flag(X);
        } else if kind != 3 {
            self.set_flag(X, x);
        }
        self.set_flag(C, c);
        self.set_flag(V, vflag);
        self.nz(size, r);
        r
    }
}

fn lea_time(mode: u16, reg: u16) -> u32 {
    match mode {
        2 => 4,
        5 => 8,
        6 => 12,
        _ => match reg {
            0 => 8,
            1 => 12,
            2 => 8,
            _ => 12,
        },
    }
}

fn jump_time(mode: u16, reg: u16, jsr: bool) -> u32 {
    let base = match mode {
        2 => 8,
        5 => 10,
        6 => 14,
        _ => match reg {
            0 => 10,
            1 => 12,
            2 => 10,
            _ => 14,
        },
    };
    base + if jsr { 8 } else { 0 }
}

/// DIVU timing, after Jorge Cwik's analysis of the 68000 microcode.
fn divu_time(dividend: u32, divisor: u16) -> u32 {
    let hdivisor = u32::from(divisor) << 16;
    if dividend >= hdivisor {
        return 10;
    }
    let mut d = dividend;
    let mut mcycles = 38u32;
    for _ in 0..15 {
        let top = d & 0x8000_0000 != 0;
        d <<= 1;
        if top {
            d = d.wrapping_sub(hdivisor);
        } else {
            mcycles += 2;
            if d >= hdivisor {
                d -= hdivisor;
                mcycles -= 1;
            }
        }
    }
    mcycles * 2
}

/// DIVS timing, after Jorge Cwik's analysis of the 68000 microcode.
fn divs_time(dividend: i32, divisor: i16) -> u32 {
    let mut mcycles = 6u32;
    if dividend < 0 {
        mcycles += 1;
    }
    let ad = dividend.unsigned_abs();
    let ds = u32::from(divisor.unsigned_abs());
    let q = i64::from(dividend) / i64::from(divisor);
    // Any signed overflow ends early, as in the reference.
    if (ad >> 16) >= ds || !(-32768..=32767).contains(&q) {
        return (mcycles + 2) * 2;
    }
    mcycles += 55;
    if divisor >= 0 {
        if dividend >= 0 {
            mcycles -= 1;
        } else {
            mcycles += 1;
        }
    }
    let mut aquot = ad / ds;
    for _ in 0..15 {
        if aquot & 0x8000 == 0 {
            mcycles += 1;
        }
        aquot <<= 1;
    }
    mcycles * 2
}
