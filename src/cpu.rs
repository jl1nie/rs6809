use std::fmt;
pub const E: u8 = 0x80;
pub const F: u8 = 0x40;
pub const H: u8 = 0x20;
pub const I: u8 = 0x10;
pub const N: u8 = 8;
pub const Z: u8 = 4;
pub const V: u8 = 2;
pub const C: u8 = 1;
pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn read16(&mut self, addr: u16) -> u16 {
        let h = self.read(addr);
        u16::from_be_bytes([h, self.read(addr.wrapping_add(1))])
    }
    fn write16(&mut self, addr: u16, value: u16) {
        let [h, l] = value.to_be_bytes();
        self.write(addr, h);
        self.write(addr.wrapping_add(1), l);
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuError {
    pub pc: u16,
    pub opcode: u16,
}
impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported instruction ${:04X} at ${:04X}",
            self.opcode, self.pc
        )
    }
}
impl std::error::Error for CpuError {}
#[derive(Debug, Clone, Default)]
pub struct Cpu {
    pub a: u8,
    pub b: u8,
    pub dp: u8,
    pub cc: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
    pub cycles: u64,
    wait: u8,
    nmi_armed: bool,
}
impl Cpu {
    pub fn d(&self) -> u16 {
        u16::from_be_bytes([self.a, self.b])
    }
    pub fn set_d(&mut self, v: u16) {
        [self.a, self.b] = v.to_be_bytes();
    }
    /// 0 when executing, 1 while stopped in SYNC, 2 while stopped in CWAI.
    pub fn waiting(&self) -> u8 {
        self.wait
    }
    pub fn reset(&mut self, b: &mut impl Bus) {
        *self = Self {
            cc: I | F,
            pc: b.read16(0xfffe),
            ..Self::default()
        };
    }
    fn flag(&mut self, m: u8, on: bool) {
        if on { self.cc |= m } else { self.cc &= !m }
    }
    fn nz8(&mut self, v: u8) {
        self.flag(N, v & 0x80 != 0);
        self.flag(Z, v == 0);
    }
    fn nz16(&mut self, v: u16) {
        self.flag(N, v & 0x8000 != 0);
        self.flag(Z, v == 0);
    }
    fn fetch(&mut self, b: &mut impl Bus) -> u8 {
        let v = b.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn word(&mut self, b: &mut impl Bus) -> u16 {
        let h = self.fetch(b);
        u16::from_be_bytes([h, self.fetch(b)])
    }
    fn push8(b: &mut impl Bus, s: &mut u16, v: u8) {
        *s = s.wrapping_sub(1);
        b.write(*s, v);
    }
    fn push16(b: &mut impl Bus, s: &mut u16, v: u16) {
        Self::push8(b, s, v as u8);
        Self::push8(b, s, (v >> 8) as u8);
    }
    fn pop8(b: &mut impl Bus, s: &mut u16) -> u8 {
        let v = b.read(*s);
        *s = s.wrapping_add(1);
        v
    }
    fn pop16(b: &mut impl Bus, s: &mut u16) -> u16 {
        let h = Self::pop8(b, s);
        u16::from_be_bytes([h, Self::pop8(b, s)])
    }
    fn frame(&mut self, b: &mut impl Bus) {
        Self::push16(b, &mut self.s, self.pc);
        Self::push16(b, &mut self.s, self.u);
        Self::push16(b, &mut self.s, self.y);
        Self::push16(b, &mut self.s, self.x);
        Self::push8(b, &mut self.s, self.dp);
        Self::push8(b, &mut self.s, self.b);
        Self::push8(b, &mut self.s, self.a);
        Self::push8(b, &mut self.s, self.cc);
    }
    /// Sample interrupt pins before each instruction; NMI must be supplied as a pulse.
    pub fn interrupt(&mut self, b: &mut impl Bus, irq: bool, firq: bool, nmi: bool) -> bool {
        if (irq || firq || nmi) && self.wait == 1 {
            self.wait = 0;
        }
        let vector = if nmi && self.nmi_armed {
            0xfffc
        } else if firq && self.cc & F == 0 {
            0xfff6
        } else if irq && self.cc & I == 0 {
            0xfff8
        } else {
            return false;
        };
        if self.wait != 2 {
            if vector == 0xfff6 {
                self.cc &= !E;
                Self::push16(b, &mut self.s, self.pc);
                Self::push8(b, &mut self.s, self.cc);
                self.cycles += 10;
            } else {
                self.cc |= E;
                self.frame(b);
                self.cycles += 19;
            }
        } else {
            self.cycles += 7;
        }
        self.wait = 0;
        self.cc |= I;
        if vector != 0xfff8 {
            self.cc |= F;
        }
        self.pc = b.read16(vector);
        true
    }
    #[inline]
    fn reg(&self, r: u8) -> u16 {
        match r {
            0 => self.d(),
            1 => self.x,
            2 => self.y,
            3 => self.u,
            4 => self.s,
            5 => self.pc,
            8 => 0xff00 | self.a as u16,
            9 => 0xff00 | self.b as u16,
            10 => 0xff00 | self.cc as u16,
            11 => 0xff00 | self.dp as u16,
            _ => 0xffff,
        }
    }
    #[inline]
    fn set_reg(&mut self, r: u8, v: u16) {
        match r {
            0 => self.set_d(v),
            1 => self.x = v,
            2 => self.y = v,
            3 => self.u = v,
            4 => {
                self.s = v;
                self.nmi_armed = true
            }
            5 => self.pc = v,
            8 => self.a = v as u8,
            9 => self.b = v as u8,
            10 => self.cc = v as u8,
            11 => self.dp = v as u8,
            _ => (),
        }
    }
    #[inline]
    fn index(&mut self, b: &mut impl Bus) -> Result<(u16, u64), ()> {
        let p = self.fetch(b);
        let r = 1 + ((p >> 5) & 3);
        let base = self.reg(r);
        if p & 0x80 == 0 {
            return Ok((base.wrapping_add(((p as i8) << 3 >> 3) as u16), 1));
        }
        let mode = p & 15;
        let indirect = p & 16 != 0;
        let (mut a, mut c) = match mode {
            0 if !indirect => {
                self.set_reg(r, base.wrapping_add(1));
                (base, 2)
            }
            1 => {
                self.set_reg(r, base.wrapping_add(2));
                (base, 3)
            }
            2 if !indirect => {
                let v = base.wrapping_sub(1);
                self.set_reg(r, v);
                (v, 2)
            }
            3 => {
                let v = base.wrapping_sub(2);
                self.set_reg(r, v);
                (v, 3)
            }
            4 => (base, 0),
            5 => (base.wrapping_add(self.b as i8 as u16), 1),
            6 => (base.wrapping_add(self.a as i8 as u16), 1),
            8 => {
                let o = self.fetch(b) as i8 as u16;
                (base.wrapping_add(o), 1)
            }
            9 => {
                let o = self.word(b);
                (base.wrapping_add(o), 4)
            }
            11 => (base.wrapping_add(self.d()), 4),
            12 => {
                let o = self.fetch(b) as i8 as u16;
                (self.pc.wrapping_add(o), 1)
            }
            13 => {
                let o = self.word(b);
                (self.pc.wrapping_add(o), 5)
            }
            15 if indirect => (self.word(b), 2),
            _ => return Err(()),
        };
        if indirect {
            a = b.read16(a);
            c += 3;
        }
        Ok((a, c))
    }
    #[inline]
    fn ea(&mut self, b: &mut impl Bus, mode: u8) -> Result<(u16, u64), ()> {
        match mode {
            1 => Ok((((self.dp as u16) << 8) | self.fetch(b) as u16, 0)),
            2 => self.index(b),
            3 => Ok((self.word(b), 0)),
            _ => Err(()),
        }
    }
    fn add8(&mut self, a: u8, b: u8, carry: bool) -> u8 {
        let c = u16::from(carry);
        let t = a as u16 + b as u16 + c;
        let r = t as u8;
        self.nz8(r);
        self.flag(C, t > 255);
        self.flag(V, (!(a ^ b) & (a ^ r) & 128) != 0);
        self.flag(H, (a & 15) as u16 + (b & 15) as u16 + c > 15);
        r
    }
    fn sub8(&mut self, a: u8, b: u8, carry: bool) -> u8 {
        let t = b as u16 + u16::from(carry);
        let r = a.wrapping_sub(t as u8);
        self.nz8(r);
        self.flag(C, (a as u16) < t);
        self.flag(V, ((a ^ b) & (a ^ r) & 128) != 0);
        r
    }
    fn math16(&mut self, a: u16, b: u16, add: bool) -> u16 {
        let r = if add {
            a.wrapping_add(b)
        } else {
            a.wrapping_sub(b)
        };
        self.nz16(r);
        self.flag(
            C,
            if add {
                a as u32 + b as u32 > 65535
            } else {
                a < b
            },
        );
        self.flag(
            V,
            if add {
                (!(a ^ b) & (a ^ r) & 0x8000) != 0
            } else {
                ((a ^ b) & (a ^ r) & 0x8000) != 0
            },
        );
        r
    }
    fn unary(&mut self, op: u8, v: u8) -> Result<u8, ()> {
        let r = match op {
            0 => {
                let r = self.sub8(0, v, false);
                return Ok(r);
            }
            3 => {
                self.cc = (self.cc & !V) | C;
                !v
            }
            4 => {
                self.flag(C, v & 1 != 0);
                v >> 1
            }
            6 => {
                let c = self.cc & C;
                self.flag(C, v & 1 != 0);
                (v >> 1) | (c << 7)
            }
            7 => {
                self.flag(C, v & 1 != 0);
                ((v as i8) >> 1) as u8
            }
            8 => {
                self.flag(C, v & 128 != 0);
                let r = v << 1;
                self.flag(V, (v ^ r) & 128 != 0);
                r
            }
            9 => {
                let c = self.cc & C;
                self.flag(C, v & 128 != 0);
                let r = (v << 1) | c;
                self.flag(V, (v ^ r) & 128 != 0);
                r
            }
            10 => {
                self.flag(V, v == 128);
                v.wrapping_sub(1)
            }
            12 => {
                self.flag(V, v == 127);
                v.wrapping_add(1)
            }
            13 => {
                self.cc &= !V;
                v
            }
            15 => {
                self.cc &= !(N | V | C);
                0
            }
            _ => return Err(()),
        };
        self.nz8(r);
        Ok(r)
    }
    fn condition(&self, op: u8) -> bool {
        let c = self.cc & C != 0;
        let z = self.cc & Z != 0;
        let n = self.cc & N != 0;
        let v = self.cc & V != 0;
        match op & 15 {
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
    fn stack(&mut self, b: &mut impl Bus, user: bool, pull: bool, mask: u8) -> u64 {
        let mut sp = if user { self.u } else { self.s };
        let other = if user { self.s } else { self.u };
        let mut bytes = 0;
        if pull {
            for bit in 0..8 {
                if mask & (1 << bit) == 0 {
                    continue;
                }
                bytes += if bit >= 4 { 2 } else { 1 };
                match bit {
                    0 => self.cc = Self::pop8(b, &mut sp),
                    1 => self.a = Self::pop8(b, &mut sp),
                    2 => self.b = Self::pop8(b, &mut sp),
                    3 => self.dp = Self::pop8(b, &mut sp),
                    4 => self.x = Self::pop16(b, &mut sp),
                    5 => self.y = Self::pop16(b, &mut sp),
                    6 => {
                        let v = Self::pop16(b, &mut sp);
                        if user { self.s = v } else { self.u = v }
                    }
                    7 => self.pc = Self::pop16(b, &mut sp),
                    _ => (),
                }
            }
        } else {
            for bit in (0..8).rev() {
                if mask & (1 << bit) == 0 {
                    continue;
                }
                bytes += if bit >= 4 { 2 } else { 1 };
                match bit {
                    7 => Self::push16(b, &mut sp, self.pc),
                    6 => Self::push16(b, &mut sp, other),
                    5 => Self::push16(b, &mut sp, self.y),
                    4 => Self::push16(b, &mut sp, self.x),
                    3 => Self::push8(b, &mut sp, self.dp),
                    2 => Self::push8(b, &mut sp, self.b),
                    1 => Self::push8(b, &mut sp, self.a),
                    0 => Self::push8(b, &mut sp, self.cc),
                    _ => (),
                }
            }
        }
        if user {
            self.u = sp
        } else {
            self.s = sp
        }
        bytes
    }
    /// Execute one instruction, returning its cycle count. Undefined opcodes fail explicitly.
    pub fn step(&mut self, b: &mut impl Bus) -> Result<u64, CpuError> {
        if self.wait != 0 {
            self.cycles += 1;
            return Ok(1);
        }
        let pc = self.pc;
        let first = self.fetch(b);
        let page = if first == 0x10 || first == 0x11 {
            first
        } else {
            0
        };
        let op = if page != 0 { self.fetch(b) } else { first };
        let err = CpuError {
            pc,
            opcode: ((page as u16) << 8) | op as u16,
        };
        let result = self.execute(b, page, op).map_err(|()| err)?;
        self.cycles += result;
        Ok(result)
    }
    fn execute(&mut self, b: &mut impl Bus, page: u8, op: u8) -> Result<u64, ()> {
        if page == 0x10 && (0x21..=0x2f).contains(&op) {
            let o = self.word(b);
            let yes = self.condition(op);
            if yes {
                self.pc = self.pc.wrapping_add(o)
            }
            return Ok(if yes { 6 } else { 5 });
        }
        if page != 0 && op == 0x3f {
            self.cc |= E;
            self.frame(b);
            self.pc = b.read16(if page == 0x10 { 0xfff4 } else { 0xfff2 });
            return Ok(20);
        }
        if page == 0 && (0x20..=0x2f).contains(&op) {
            let o = self.fetch(b) as i8 as u16;
            if self.condition(op) {
                self.pc = self.pc.wrapping_add(o)
            }
            return Ok(3);
        }
        if page == 0 && ((op <= 0x0f) || (0x40..=0x7f).contains(&op)) {
            let lo = op & 15;
            let reg = (0x40..0x60).contains(&op);
            let (a, extra) = if reg {
                (0, 0)
            } else {
                self.ea(b, if op < 16 { 1 } else { (op >> 4) - 4 })?
            };
            if lo == 14 && !reg {
                self.pc = a;
                return Ok((if op < 0x70 { 3 } else { 4 }) + extra);
            }
            let v = if reg {
                if op & 16 == 0 { self.a } else { self.b }
            } else {
                b.read(a)
            };
            let r = self.unary(lo, v)?;
            if lo != 13 {
                if reg {
                    if op & 16 == 0 { self.a = r } else { self.b = r }
                } else {
                    b.write(a, r)
                }
            }
            return Ok((if reg {
                2
            } else if op >= 0x70 {
                7
            } else {
                6
            }) + extra);
        }
        if page == 0 {
            match op {
                0x12 => return Ok(2),
                0x13 => {
                    self.wait = 1;
                    return Ok(4);
                }
                0x16 | 0x17 => {
                    let o = self.word(b);
                    if op == 0x17 {
                        Self::push16(b, &mut self.s, self.pc)
                    }
                    self.pc = self.pc.wrapping_add(o);
                    return Ok(if op == 0x16 { 5 } else { 9 });
                }
                0x19 => {
                    let mut correction = 0;
                    let a = self.a;
                    if a & 15 > 9 || self.cc & H != 0 {
                        correction |= 6
                    }
                    if a > 0x99 || self.cc & C != 0 {
                        correction |= 0x60
                    }
                    let t = a as u16 + correction;
                    self.a = t as u8;
                    self.flag(C, self.cc & C != 0 || t > 255);
                    self.cc &= !V;
                    self.nz8(self.a);
                    return Ok(2);
                }
                0x1a => {
                    self.cc |= self.fetch(b);
                    return Ok(3);
                }
                0x1c => {
                    self.cc &= self.fetch(b);
                    return Ok(3);
                }
                0x1d => {
                    self.a = if self.b & 128 != 0 { 255 } else { 0 };
                    self.nz16(self.d());
                    self.cc &= !V;
                    return Ok(2);
                }
                0x1e | 0x1f => {
                    let p = self.fetch(b);
                    let v = self.reg(p >> 4);
                    let w = self.reg(p & 15);
                    self.set_reg(p & 15, v);
                    if op == 0x1e {
                        self.set_reg(p >> 4, w)
                    }
                    return Ok(if op == 0x1e { 8 } else { 6 });
                }
                0x30..=0x33 => {
                    let (a, c) = self.index(b)?;
                    match op {
                        0x30 => {
                            self.x = a;
                            self.flag(Z, a == 0)
                        }
                        0x31 => {
                            self.y = a;
                            self.flag(Z, a == 0)
                        }
                        0x32 => {
                            self.s = a;
                            self.nmi_armed = true
                        }
                        _ => self.u = a,
                    }
                    return Ok(4 + c);
                }
                0x34..=0x37 => {
                    let mask = self.fetch(b);
                    let c = self.stack(b, op & 2 != 0, op & 1 != 0, mask);
                    return Ok(5 + c);
                }
                0x39 => {
                    self.pc = Self::pop16(b, &mut self.s);
                    return Ok(5);
                }
                0x3a => {
                    self.x = self.x.wrapping_add(self.b as u16);
                    return Ok(3);
                }
                0x3b => {
                    self.cc = Self::pop8(b, &mut self.s);
                    let full = self.cc & E != 0;
                    if full {
                        self.a = Self::pop8(b, &mut self.s);
                        self.b = Self::pop8(b, &mut self.s);
                        self.dp = Self::pop8(b, &mut self.s);
                        self.x = Self::pop16(b, &mut self.s);
                        self.y = Self::pop16(b, &mut self.s);
                        self.u = Self::pop16(b, &mut self.s)
                    }
                    self.pc = Self::pop16(b, &mut self.s);
                    return Ok(if full { 15 } else { 6 });
                }
                0x3c => {
                    self.cc &= self.fetch(b);
                    self.cc |= E;
                    self.frame(b);
                    self.wait = 2;
                    return Ok(20);
                }
                0x3d => {
                    let r = self.a as u16 * self.b as u16;
                    self.set_d(r);
                    self.flag(Z, r == 0);
                    self.flag(C, r & 128 != 0);
                    return Ok(11);
                }
                0x3f => {
                    self.cc |= E;
                    self.frame(b);
                    self.cc |= I | F;
                    self.pc = b.read16(0xfffa);
                    return Ok(19);
                }
                _ => (),
            }
        }
        if op < 0x80 {
            return Err(());
        }
        let group = op >> 6;
        let mode = (op >> 4) & 3;
        let lo = op & 15;
        if page == 0 && lo == 13 && group == 2 {
            if mode == 0 {
                let o = self.fetch(b) as i8 as u16;
                Self::push16(b, &mut self.s, self.pc);
                self.pc = self.pc.wrapping_add(o);
                return Ok(7);
            }
            let (a, c) = self.ea(b, mode)?;
            Self::push16(b, &mut self.s, self.pc);
            self.pc = a;
            return Ok((if mode == 3 { 8 } else { 7 }) + c);
        }
        let wide = if page == 0 {
            matches!(lo, 3 | 12 | 14 | 15) || (lo == 13 && group == 3)
        } else {
            true
        };
        // Validate prefixed instructions before touching an operand.
        if page != 0 {
            let valid = match page {
                0x10 => {
                    (group == 2 && matches!(lo, 3 | 12 | 14 | 15))
                        || (group == 3 && matches!(lo, 14 | 15))
                }
                0x11 => group == 2 && matches!(lo, 3 | 12),
                _ => false,
            };
            if !valid {
                return Err(());
            }
        }
        let store = lo == 7 || lo == 15 || (lo == 13 && group == 3 && page == 0);
        if mode == 0 && store {
            return Err(());
        }
        let (addr, extra) = if mode == 0 { (0, 0) } else { self.ea(b, mode)? };
        let val = if store {
            0
        } else if mode == 0 {
            if wide {
                self.word(b)
            } else {
                self.fetch(b) as u16
            }
        } else if wide {
            b.read16(addr)
        } else {
            b.read(addr) as u16
        };
        let base = match mode {
            0 => 2,
            1 | 2 => 4,
            _ => 5,
        };
        if page != 0 {
            let reg = match (page, lo, group) {
                (0x10, 3, _) => 0,
                (0x10, 12, _) => 2,
                (0x11, 3, _) => 3,
                (0x11, 12, _) => 4,
                (0x10, 14 | 15, 2) => 2,
                (0x10, 14 | 15, 3) => 4,
                _ => return Err(()),
            };
            if lo == 3 || lo == 12 {
                self.math16(self.reg(reg), val, false);
                return Ok(base + 3 + extra);
            }
            if store {
                let v = self.reg(reg);
                b.write16(addr, v);
                self.nz16(v)
            } else {
                self.set_reg(reg, val);
                self.nz16(val)
            }
            self.cc &= !V;
            return Ok(base + 2 + extra);
        }
        if wide {
            match (group, lo) {
                (2, 3) => {
                    let r = self.math16(self.d(), val, false);
                    self.set_d(r)
                }
                (3, 3) => {
                    let r = self.math16(self.d(), val, true);
                    self.set_d(r)
                }
                (2, 12) => {
                    self.math16(self.x, val, false);
                }
                (3, 12) => {
                    self.set_d(val);
                    self.nz16(val);
                    self.cc &= !V
                }
                (3, 13) => {
                    b.write16(addr, self.d());
                    self.nz16(self.d());
                    self.cc &= !V
                }
                (_, 14) => {
                    if group == 2 {
                        self.x = val
                    } else {
                        self.u = val
                    }
                    self.nz16(val);
                    self.cc &= !V
                }
                (_, 15) => {
                    let v = if group == 2 { self.x } else { self.u };
                    b.write16(addr, v);
                    self.nz16(v);
                    self.cc &= !V
                }
                _ => return Err(()),
            }
            return Ok(base
                + if lo == 3 || (group == 2 && lo == 12) {
                    2
                } else {
                    1
                }
                + extra);
        }
        let a = if group == 2 { self.a } else { self.b };
        let v = val as u8;
        let carry = self.cc & C != 0;
        let r = match lo {
            0 => self.sub8(a, v, false),
            1 => {
                self.sub8(a, v, false);
                a
            }
            2 => self.sub8(a, v, carry),
            4 => {
                let r = a & v;
                self.nz8(r);
                self.cc &= !V;
                r
            }
            5 => {
                self.nz8(a & v);
                self.cc &= !V;
                a
            }
            6 => {
                self.nz8(v);
                self.cc &= !V;
                v
            }
            7 => {
                b.write(addr, a);
                self.nz8(a);
                self.cc &= !V;
                a
            }
            8 => {
                let r = a ^ v;
                self.nz8(r);
                self.cc &= !V;
                r
            }
            9 => self.add8(a, v, carry),
            10 => {
                let r = a | v;
                self.nz8(r);
                self.cc &= !V;
                r
            }
            11 => self.add8(a, v, false),
            _ => return Err(()),
        };
        if group == 2 {
            self.a = r
        } else {
            self.b = r
        }
        Ok(base + extra)
    }
}
