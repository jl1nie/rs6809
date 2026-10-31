//! 68000 disassembler (Motorola syntax, `$` hexadecimal).
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Next,
    /// JSR/BSR/TRAP: returns to the following instruction.
    Call,
    Jump,
    /// Conditional branch (also falls through).
    Branch,
    Return,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub addr: u32,
    pub len: u32,
    pub text: String,
    /// Branch/call destination when known.
    pub target: Option<u32>,
    pub flow: Flow,
}

struct Rd<'a> {
    mem: &'a dyn Fn(u32) -> u8,
    pc: u32,
}
impl Rd<'_> {
    fn word(&mut self) -> u16 {
        let v = u16::from_be_bytes([(self.mem)(self.pc), (self.mem)(self.pc.wrapping_add(1))]);
        self.pc = self.pc.wrapping_add(2);
        v
    }
    fn long(&mut self) -> u32 {
        u32::from(self.word()) << 16 | u32::from(self.word())
    }
}

const CC: [&str; 16] = [
    "T", "F", "HI", "LS", "CC", "CS", "NE", "EQ", "VC", "VS", "PL", "MI", "GE", "LT", "GT", "LE",
];

fn sz(bits: u16) -> Option<(&'static str, u8)> {
    match bits & 3 {
        0 => Some((".B", 1)),
        1 => Some((".W", 2)),
        2 => Some((".L", 4)),
        _ => None,
    }
}
fn signed_hex(v: i32) -> String {
    if v < 0 {
        format!("-${:X}", -(v as i64))
    } else {
        format!("${v:X}")
    }
}

impl Rd<'_> {
    /// Effective address text; `size` in bytes for immediates.
    fn ea(&mut self, mode: u16, reg: u16, size: u8) -> Option<String> {
        let r = reg & 7;
        Some(match mode & 7 {
            0 => format!("D{r}"),
            1 => format!("A{r}"),
            2 => format!("(A{r})"),
            3 => format!("(A{r})+"),
            4 => format!("-(A{r})"),
            5 => format!("({},A{r})", signed_hex(i32::from(self.word() as i16))),
            6 => self.index(&format!("A{r}"), None),
            _ => match r {
                0 => format!("${:X}.W", self.word() as i16 as i32 as u32 & 0xff_ffff),
                1 => format!("${:X}.L", self.long()),
                2 => {
                    let base = self.pc;
                    let d = self.word() as i16 as i32;
                    format!("${:X}(PC)", base.wrapping_add(d as u32) & 0xff_ffff)
                }
                3 => {
                    let base = self.pc;
                    self.index("PC", Some(base))
                }
                4 => match size {
                    1 => format!("#${:X}", self.word() & 0xff),
                    4 => format!("#${:X}", self.long()),
                    _ => format!("#${:X}", self.word()),
                },
                _ => return None,
            },
        })
    }
    fn index(&mut self, base: &str, pc: Option<u32>) -> String {
        let ext = self.word();
        let x = format!(
            "{}{}.{}",
            if ext & 0x8000 != 0 { 'A' } else { 'D' },
            (ext >> 12) & 7,
            if ext & 0x800 != 0 { 'L' } else { 'W' }
        );
        let d = i32::from(ext as u8 as i8);
        match pc {
            Some(p) => format!("${:X}(PC,{x})", p.wrapping_add(d as u32) & 0xff_ffff),
            None => format!("({},{base},{x})", signed_hex(d)),
        }
    }
}

fn reglist(mask: u16, predec: bool) -> String {
    let regs: Vec<String> = (0..16)
        .filter(|&i| mask & (1 << if predec { 15 - i } else { i }) != 0)
        .map(|i| {
            if i < 8 {
                format!("D{i}")
            } else {
                format!("A{}", i - 8)
            }
        })
        .collect();
    // Collapse runs: D0-D3/A6.
    let mut out = String::new();
    let mut i = 0;
    while i < regs.len() {
        let mut j = i;
        while j + 1 < regs.len()
            && regs[j + 1].as_bytes()[0] == regs[i].as_bytes()[0]
            && regs[j + 1].as_bytes()[1] == regs[j].as_bytes()[1] + 1
        {
            j += 1;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&regs[i]);
        if j > i {
            let _ = write!(out, "-{}", regs[j]);
        }
        i = j + 1;
    }
    if out.is_empty() { "#0".into() } else { out }
}

/// Disassemble one instruction at `addr`.
pub fn disassemble(mem: &dyn Fn(u32) -> u8, addr: u32) -> Line {
    let mut r = Rd { mem, pc: addr };
    let op = r.word();
    let (text, target, flow) =
        decode(&mut r, op).unwrap_or_else(|| (format!("DC.W ${op:04X}"), None, Flow::Next));
    Line {
        addr,
        len: r.pc.wrapping_sub(addr),
        text,
        target,
        flow,
    }
}

fn decode(r: &mut Rd, op: u16) -> Option<(String, Option<u32>, Flow)> {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let rx = (op >> 9) & 7;
    let next = |t: String| Some((t, None, Flow::Next));
    match op >> 12 {
        0x0 => {
            if op & 0x0138 == 0x0108 {
                let d = signed_hex(i32::from(r.word() as i16));
                let s = if op & 0x40 != 0 { ".L" } else { ".W" };
                return next(if op & 0x80 != 0 {
                    format!("MOVEP{s} D{rx},({d},A{reg})")
                } else {
                    format!("MOVEP{s} ({d},A{reg}),D{rx}")
                });
            }
            let bitname = ["BTST", "BCHG", "BCLR", "BSET"][usize::from((op >> 6) & 3)];
            if op & 0x0100 != 0 {
                let e = r.ea(mode, reg, 1)?;
                return next(format!("{bitname} D{rx},{e}"));
            }
            let kind = (op >> 9) & 7;
            if kind == 4 {
                let n = r.word() & 0xff;
                let e = r.ea(mode, reg, 1)?;
                return next(format!("{bitname} #{n},{e}"));
            }
            let name = ["ORI", "ANDI", "SUBI", "ADDI", "", "EORI", "CMPI", ""][usize::from(kind)];
            if name.is_empty() {
                return None;
            }
            let (s, n) = sz(op >> 6)?;
            if mode == 7 && reg == 4 && matches!(kind, 0 | 1 | 5) && n != 4 {
                let v = r.word();
                return next(if n == 1 {
                    format!("{name} #${:X},CCR", v & 0xff)
                } else {
                    format!("{name} #${v:X},SR")
                });
            }
            let imm = r.ea(7, 4, n)?;
            let e = r.ea(mode, reg, n)?;
            next(format!("{name}{s} {imm},{e}"))
        }
        0x1..=0x3 => {
            let (s, n) = match op >> 12 {
                1 => (".B", 1),
                3 => (".W", 2),
                _ => (".L", 4),
            };
            let src = r.ea(mode, reg, n)?;
            let dmode = (op >> 6) & 7;
            let dst = r.ea(dmode, rx, n)?;
            next(format!(
                "{}{s} {src},{dst}",
                if dmode == 1 { "MOVEA" } else { "MOVE" }
            ))
        }
        0x4 => decode4(r, op, mode, reg, rx),
        0x5 => {
            let cc = usize::from((op >> 8) & 15);
            if (op >> 6) & 3 == 3 {
                if mode == 1 {
                    let base = r.pc;
                    let d = r.word() as i16 as i32 as u32;
                    let t = base.wrapping_add(d);
                    let name = if cc == 1 {
                        "DBRA".to_string()
                    } else {
                        format!("DB{}", CC[cc])
                    };
                    return Some((
                        format!("{name} D{reg},${:X}", t & 0xff_ffff),
                        Some(t),
                        Flow::Branch,
                    ));
                }
                let e = r.ea(mode, reg, 1)?;
                return next(format!("S{} {e}", CC[cc]));
            }
            let (s, n) = sz(op >> 6)?;
            let q = if rx == 0 { 8 } else { rx };
            let e = r.ea(mode, reg, n)?;
            next(format!(
                "{}{s} #{q},{e}",
                if op & 0x100 != 0 { "SUBQ" } else { "ADDQ" }
            ))
        }
        0x6 => {
            let cc = usize::from((op >> 8) & 15);
            let base = r.pc;
            let (d, s) = match op & 0xff {
                0 => (r.word() as i16 as i32 as u32, ".W"),
                d => (d as u8 as i8 as i32 as u32, ".S"),
            };
            let t = base.wrapping_add(d);
            let (name, flow) = match cc {
                0 => ("BRA".to_string(), Flow::Jump),
                1 => ("BSR".to_string(), Flow::Call),
                _ => (format!("B{}", CC[cc]), Flow::Branch),
            };
            Some((format!("{name}{s} ${:X}", t & 0xff_ffff), Some(t), flow))
        }
        0x7 => {
            if op & 0x100 != 0 {
                return None;
            }
            next(format!(
                "MOVEQ #{},D{rx}",
                signed_hex(i32::from(op as u8 as i8))
            ))
        }
        0x8 | 0xc => {
            let and = op >> 12 == 0xc;
            if op & 0xc0 == 0xc0 {
                let e = r.ea(mode, reg, 2)?;
                let name = match (and, op & 0x100 != 0) {
                    (false, false) => "DIVU",
                    (false, true) => "DIVS",
                    (true, false) => "MULU",
                    (true, true) => "MULS",
                };
                return next(format!("{name}.W {e},D{rx}"));
            }
            if op & 0x1f0 == 0x100 {
                let name = if and { "ABCD" } else { "SBCD" };
                return next(if op & 8 == 0 {
                    format!("{name} D{reg},D{rx}")
                } else {
                    format!("{name} -(A{reg}),-(A{rx})")
                });
            }
            if and && op & 0x130 == 0x100 {
                return next(match (op >> 3) & 0x1f {
                    0x08 => format!("EXG D{rx},D{reg}"),
                    0x09 => format!("EXG A{rx},A{reg}"),
                    0x11 => format!("EXG D{rx},A{reg}"),
                    _ => return None,
                });
            }
            let (s, n) = sz(op >> 6)?;
            let name = if and { "AND" } else { "OR" };
            let e = r.ea(mode, reg, n)?;
            next(if op & 0x100 != 0 {
                format!("{name}{s} D{rx},{e}")
            } else {
                format!("{name}{s} {e},D{rx}")
            })
        }
        0x9 | 0xd => {
            let name = if op >> 12 == 0xd { "ADD" } else { "SUB" };
            if op & 0xc0 == 0xc0 {
                let (s, n) = if op & 0x100 != 0 {
                    (".L", 4)
                } else {
                    (".W", 2)
                };
                let e = r.ea(mode, reg, n)?;
                return next(format!("{name}A{s} {e},A{rx}"));
            }
            let (s, n) = sz(op >> 6)?;
            if op & 0x130 == 0x100 {
                return next(if op & 8 == 0 {
                    format!("{name}X{s} D{reg},D{rx}")
                } else {
                    format!("{name}X{s} -(A{reg}),-(A{rx})")
                });
            }
            let e = r.ea(mode, reg, n)?;
            next(if op & 0x100 != 0 {
                format!("{name}{s} D{rx},{e}")
            } else {
                format!("{name}{s} {e},D{rx}")
            })
        }
        0xb => {
            if op & 0xc0 == 0xc0 {
                let (s, n) = if op & 0x100 != 0 {
                    (".L", 4)
                } else {
                    (".W", 2)
                };
                let e = r.ea(mode, reg, n)?;
                return next(format!("CMPA{s} {e},A{rx}"));
            }
            let (s, n) = sz(op >> 6)?;
            if op & 0x100 == 0 {
                let e = r.ea(mode, reg, n)?;
                return next(format!("CMP{s} {e},D{rx}"));
            }
            if mode == 1 {
                return next(format!("CMPM{s} (A{reg})+,(A{rx})+"));
            }
            let e = r.ea(mode, reg, n)?;
            next(format!("EOR{s} D{rx},{e}"))
        }
        0xe => {
            let dir = if op & 0x100 != 0 { "L" } else { "R" };
            if (op >> 6) & 3 == 3 {
                let name = ["AS", "LS", "ROX", "RO"][usize::from((op >> 9) & 3)];
                let e = r.ea(mode, reg, 2)?;
                return next(format!("{name}{dir}.W {e}"));
            }
            let (s, _) = sz(op >> 6)?;
            let name = ["AS", "LS", "ROX", "RO"][usize::from((op >> 3) & 3)];
            let count = if op & 0x20 != 0 {
                format!("D{rx}")
            } else {
                format!("#{}", if rx == 0 { 8 } else { rx })
            };
            next(format!("{name}{dir}{s} {count},D{reg}"))
        }
        0xa => next(format!("LINEA ${op:04X}")),
        _ => next(format!("LINEF ${op:04X}")),
    }
}

fn decode4(
    r: &mut Rd,
    op: u16,
    mode: u16,
    reg: u16,
    rx: u16,
) -> Option<(String, Option<u32>, Flow)> {
    let next = |t: String| Some((t, None, Flow::Next));
    if op & 0x1c0 == 0x1c0 {
        let e = r.ea(mode, reg, 4)?;
        return next(format!("LEA {e},A{rx}"));
    }
    if op & 0x1c0 == 0x180 {
        let e = r.ea(mode, reg, 2)?;
        return next(format!("CHK.W {e},D{rx}"));
    }
    match op {
        0x4afc => return next("ILLEGAL".into()),
        0x4e70 => return next("RESET".into()),
        0x4e71 => return next("NOP".into()),
        0x4e72 => return next(format!("STOP #${:X}", r.word())),
        0x4e73 => return Some(("RTE".into(), None, Flow::Return)),
        0x4e75 => return Some(("RTS".into(), None, Flow::Return)),
        0x4e76 => return next("TRAPV".into()),
        0x4e77 => return Some(("RTR".into(), None, Flow::Return)),
        _ => {}
    }
    match op & 0xfff0 {
        0x4e40 => return Some((format!("TRAP #{}", op & 15), None, Flow::Call)),
        0x4e50 => {
            return next(if op & 8 == 0 {
                format!("LINK A{reg},#{}", signed_hex(i32::from(r.word() as i16)))
            } else {
                format!("UNLK A{reg}")
            });
        }
        0x4e60 => {
            return next(if op & 8 == 0 {
                format!("MOVE A{reg},USP")
            } else {
                format!("MOVE USP,A{reg}")
            });
        }
        _ => {}
    }
    match op & 0xffc0 {
        0x4e80 | 0x4ec0 => {
            let jsr = op & 0x40 == 0;
            let start = r.pc;
            let e = r.ea(mode, reg, 4)?;
            // Absolute and PC-relative targets are known statically.
            let mut t = Rd {
                mem: r.mem,
                pc: start,
            };
            let target = match (mode, reg) {
                (7, 0) => Some(t.word() as i16 as i32 as u32),
                (7, 1) => Some(t.long()),
                (7, 2) => Some(start.wrapping_add(t.word() as i16 as i32 as u32)),
                _ => None,
            };
            return Some((
                format!("{} {e}", if jsr { "JSR" } else { "JMP" }),
                target,
                if jsr { Flow::Call } else { Flow::Jump },
            ));
        }
        0x40c0 => return next(format!("MOVE SR,{}", r.ea(mode, reg, 2)?)),
        0x44c0 => return next(format!("MOVE {},CCR", r.ea(mode, reg, 2)?)),
        0x46c0 => return next(format!("MOVE {},SR", r.ea(mode, reg, 2)?)),
        0x4840 => {
            return next(if mode == 0 {
                format!("SWAP D{reg}")
            } else {
                format!("PEA {}", r.ea(mode, reg, 4)?)
            });
        }
        0x4800 => return next(format!("NBCD {}", r.ea(mode, reg, 1)?)),
        0x4ac0 => return next(format!("TAS {}", r.ea(mode, reg, 1)?)),
        _ => {}
    }
    if op & 0xfb80 == 0x4880 {
        let s = if op & 0x40 != 0 { ".L" } else { ".W" };
        if mode == 0 {
            return next(format!("EXT{s} D{reg}"));
        }
        let mask = r.word();
        let n = if op & 0x40 != 0 { 4 } else { 2 };
        let e = r.ea(mode, reg, n)?;
        let list = reglist(mask, mode == 4);
        return next(if op & 0x400 != 0 {
            format!("MOVEM{s} {e},{list}")
        } else {
            format!("MOVEM{s} {list},{e}")
        });
    }
    let (s, n) = sz(op >> 6)?;
    let name = match (op >> 8) & 15 {
        0 => "NEGX",
        2 => "CLR",
        4 => "NEG",
        6 => "NOT",
        0xa => "TST",
        _ => return None,
    };
    next(format!("{name}{s} {}", r.ea(mode, reg, n)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dis(bytes: &[u8]) -> (String, u32) {
        let v = bytes.to_vec();
        let f = move |a: u32| v.get((a - 0x1000) as usize).copied().unwrap_or(0);
        let l = disassemble(&f, 0x1000);
        (l.text, l.len)
    }
    #[test]
    fn common_forms() {
        assert_eq!(dis(&[0x4e, 0x71]), ("NOP".into(), 2));
        assert_eq!(dis(&[0x30, 0x18]), ("MOVE.W (A0)+,D0".into(), 2));
        assert_eq!(
            dis(&[0x23, 0xfc, 0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x40, 0x00]),
            ("MOVE.L #$12345678,$4000.L".into(), 10)
        );
        assert_eq!(
            dis(&[0x41, 0xe8, 0xff, 0xf0]),
            ("LEA (-$10,A0),A0".into(), 4)
        );
        assert_eq!(
            dis(&[0x20, 0x31, 0x18, 0x04]),
            ("MOVE.L ($4,A1,D1.L),D0".into(), 4)
        );
        assert_eq!(dis(&[0x61, 0x00, 0x00, 0x10]), ("BSR.W $1012".into(), 4));
        assert_eq!(dis(&[0x67, 0xfe]), ("BEQ.S $1000".into(), 2));
        assert_eq!(dis(&[0x51, 0xc9, 0xff, 0xfe]), ("DBRA D1,$1000".into(), 4));
        assert_eq!(
            dis(&[0x48, 0xe7, 0xc0, 0xc0]),
            ("MOVEM.L D0-D1/A0-A1,-(A7)".into(), 4)
        );
        assert_eq!(
            dis(&[0x4c, 0xdf, 0x03, 0x03]),
            ("MOVEM.L (A7)+,D0-D1/A0-A1".into(), 4)
        );
        assert_eq!(dis(&[0x4e, 0x42]), ("TRAP #2".into(), 2));
        assert_eq!(dis(&[0x70, 0xff]), ("MOVEQ #-$1,D0".into(), 2));
        assert_eq!(dis(&[0xe3, 0x48]), ("LSL.W #1,D0".into(), 2));
        assert_eq!(dis(&[0x02, 0x7c, 0xf8, 0xff]), ("ANDI #$F8FF,SR".into(), 4));
        assert_eq!(
            dis(&[0x4e, 0xb9, 0x00, 0x00, 0x20, 0x00]),
            ("JSR $2000.L".into(), 6)
        );
        assert_eq!(dis(&[0xc1, 0x41]), ("EXG D0,D1".into(), 2));
    }
}
