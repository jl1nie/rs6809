//! 6809 disassembler. OS-9 service requests (`SWI2` + code byte) decode as `OS9 F$xxx`.
use super::os9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub addr: u16,
    pub len: u16,
    pub text: String,
    /// Branch/call destination or OS-9 service code, for the UI.
    pub target: Option<u16>,
    pub flow: Flow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Next,
    /// JSR/BSR/LBSR/SWI*/OS9: returns to the following instruction.
    Call,
    Jump,
    Return,
}

const UNARY: [&str; 16] = [
    "NEG", "", "", "COM", "LSR", "", "ROR", "ASR", "ASL", "ROL", "DEC", "", "INC", "TST", "JMP",
    "CLR",
];
const BRANCH: [&str; 16] = [
    "BRA", "BRN", "BHI", "BLS", "BCC", "BCS", "BNE", "BEQ", "BVC", "BVS", "BPL", "BMI", "BGE",
    "BLT", "BGT", "BLE",
];
const OPS_A: [&str; 16] = [
    "SUBA", "CMPA", "SBCA", "SUBD", "ANDA", "BITA", "LDA", "STA", "EORA", "ADCA", "ORA", "ADDA",
    "CMPX", "JSR", "LDX", "STX",
];
const OPS_B: [&str; 16] = [
    "SUBB", "CMPB", "SBCB", "ADDD", "ANDB", "BITB", "LDB", "STB", "EORB", "ADCB", "ORB", "ADDB",
    "LDD", "STD", "LDU", "STU",
];
const IDX_REG: [&str; 4] = ["X", "Y", "U", "S"];

fn reg_name(r: u8) -> &'static str {
    match r {
        0 => "D",
        1 => "X",
        2 => "Y",
        3 => "U",
        4 => "S",
        5 => "PC",
        8 => "A",
        9 => "B",
        10 => "CC",
        11 => "DP",
        _ => "?",
    }
}

struct Reader<'a> {
    mem: &'a dyn Fn(u16) -> u8,
    pc: u16,
    start: u16,
}
impl Reader<'_> {
    fn byte(&mut self) -> u8 {
        let v = (self.mem)(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn word(&mut self) -> u16 {
        let h = self.byte();
        u16::from_be_bytes([h, self.byte()])
    }
    fn indexed(&mut self) -> String {
        let p = self.byte();
        let r = IDX_REG[usize::from((p >> 5) & 3)];
        if p & 0x80 == 0 {
            let o = ((p as i8) << 3 >> 3) as i16;
            return format!("{o},{r}");
        }
        let ind = p & 0x10 != 0;
        let body = match p & 15 {
            0 => format!(",{r}+"),
            1 => format!(",{r}++"),
            2 => format!(",-{r}"),
            3 => format!(",--{r}"),
            4 => format!(",{r}"),
            5 => format!("B,{r}"),
            6 => format!("A,{r}"),
            8 => format!("{},{r}", self.byte() as i8),
            9 => format!("${:04X},{r}", self.word()),
            11 => format!("D,{r}"),
            12 => {
                let o = self.byte() as i8 as u16;
                format!("${:04X},PCR", self.pc.wrapping_add(o))
            }
            13 => {
                let o = self.word();
                format!("${:04X},PCR", self.pc.wrapping_add(o))
            }
            15 => format!("${:04X}", self.word()),
            _ => "??".into(),
        };
        if ind { format!("[{body}]") } else { body }
    }
    fn operand(&mut self, mode: u8, wide: bool) -> String {
        match mode {
            0 => {
                if wide {
                    format!("#${:04X}", self.word())
                } else {
                    format!("#${:02X}", self.byte())
                }
            }
            1 => format!("<${:02X}", self.byte()),
            2 => self.indexed(),
            _ => format!("${:04X}", self.word()),
        }
    }
    fn stack_mask(mask: u8, user: bool) -> String {
        let names = [
            "CC",
            "A",
            "B",
            "DP",
            "X",
            "Y",
            if user { "S" } else { "U" },
            "PC",
        ];
        let v: Vec<_> = (0..8)
            .filter(|b| mask & (1 << b) != 0)
            .map(|b| names[b])
            .collect();
        v.join(",")
    }
}

/// Disassemble one instruction at `addr` using a side-effect-free reader.
pub fn disassemble(mem: &dyn Fn(u16) -> u8, addr: u16) -> Line {
    let mut r = Reader {
        mem,
        pc: addr,
        start: addr,
    };
    let (text, target, flow) = decode(&mut r);
    Line {
        addr,
        len: r.pc.wrapping_sub(r.start),
        text,
        target,
        flow,
    }
}

fn decode(r: &mut Reader) -> (String, Option<u16>, Flow) {
    let first = r.byte();
    let page = if first == 0x10 || first == 0x11 {
        first
    } else {
        0
    };
    let op = if page != 0 { r.byte() } else { first };
    let bad = || (format!("FCB ${first:02X}"), None, Flow::Next);
    if page == 0x10 && op == 0x3f {
        let code = r.byte();
        return (
            format!("OS9 {}", os9::service_name(code)),
            Some(u16::from(code)),
            Flow::Call,
        );
    }
    if page == 0x11 && op == 0x3f {
        return ("SWI3".into(), None, Flow::Call);
    }
    if page == 0x10 && (0x21..=0x2f).contains(&op) {
        let o = r.word();
        let t = r.pc.wrapping_add(o);
        return (
            format!("L{} ${t:04X}", BRANCH[usize::from(op & 15)]),
            Some(t),
            Flow::Jump,
        );
    }
    if page == 0 {
        if op <= 0x0f || (0x40..=0x7f).contains(&op) {
            let name = UNARY[usize::from(op & 15)];
            if name.is_empty() {
                return bad();
            }
            if (0x40..0x60).contains(&op) {
                if op & 15 == 14 {
                    return bad();
                }
                let reg = if op < 0x50 { "A" } else { "B" };
                return (format!("{name}{reg}"), None, Flow::Next);
            }
            let mode = if op < 0x10 { 1 } else { (op >> 4) - 4 };
            let o = r.operand(mode, false);
            let flow = if op & 15 == 14 {
                Flow::Jump
            } else {
                Flow::Next
            };
            return (format!("{name} {o}"), None, flow);
        }
        if (0x20..=0x2f).contains(&op) {
            let o = r.byte() as i8 as u16;
            let t = r.pc.wrapping_add(o);
            return (
                format!("{} ${t:04X}", BRANCH[usize::from(op & 15)]),
                Some(t),
                Flow::Jump,
            );
        }
        match op {
            0x12 => return ("NOP".into(), None, Flow::Next),
            0x13 => return ("SYNC".into(), None, Flow::Next),
            0x16 | 0x17 => {
                let o = r.word();
                let t = r.pc.wrapping_add(o);
                let (n, f) = if op == 0x16 {
                    ("LBRA", Flow::Jump)
                } else {
                    ("LBSR", Flow::Call)
                };
                return (format!("{n} ${t:04X}"), Some(t), f);
            }
            0x19 => return ("DAA".into(), None, Flow::Next),
            0x1a => return (format!("ORCC #${:02X}", r.byte()), None, Flow::Next),
            0x1c => return (format!("ANDCC #${:02X}", r.byte()), None, Flow::Next),
            0x1d => return ("SEX".into(), None, Flow::Next),
            0x1e | 0x1f => {
                let p = r.byte();
                let n = if op == 0x1e { "EXG" } else { "TFR" };
                let flow = if p & 15 == 5 { Flow::Jump } else { Flow::Next };
                return (
                    format!("{n} {},{}", reg_name(p >> 4), reg_name(p & 15)),
                    None,
                    flow,
                );
            }
            0x30..=0x33 => {
                let n = ["LEAX", "LEAY", "LEAS", "LEAU"][usize::from(op & 3)];
                return (format!("{n} {}", r.indexed()), None, Flow::Next);
            }
            0x34..=0x37 => {
                let n = ["PSHS", "PULS", "PSHU", "PULU"][usize::from(op & 3)];
                let m = r.byte();
                let flow = if op & 1 == 1 && m & 0x80 != 0 {
                    Flow::Return
                } else {
                    Flow::Next
                };
                return (
                    format!("{n} {}", Reader::stack_mask(m, op & 2 != 0)),
                    None,
                    flow,
                );
            }
            0x39 => return ("RTS".into(), None, Flow::Return),
            0x3a => return ("ABX".into(), None, Flow::Next),
            0x3b => return ("RTI".into(), None, Flow::Return),
            0x3c => return (format!("CWAI #${:02X}", r.byte()), None, Flow::Next),
            0x3d => return ("MUL".into(), None, Flow::Next),
            0x3f => return ("SWI".into(), None, Flow::Call),
            _ => (),
        }
        if op < 0x80 {
            return bad();
        }
        let lo = op & 15;
        let mode = (op >> 4) & 3;
        let group = op >> 6;
        if op == 0x8d {
            let o = r.byte() as i8 as u16;
            let t = r.pc.wrapping_add(o);
            return (format!("BSR ${t:04X}"), Some(t), Flow::Call);
        }
        let store = lo == 7 || lo == 15 || (lo == 13 && group == 3);
        if mode == 0 && (store || (lo == 13 && group == 2)) {
            return bad();
        }
        let name = if group == 2 {
            OPS_A[usize::from(lo)]
        } else {
            OPS_B[usize::from(lo)]
        };
        let wide = matches!(lo, 3 | 12 | 14 | 15) || (lo == 13 && group == 3);
        let target =
            (mode == 3).then(|| (r.mem)(r.pc) as u16 * 256 + (r.mem)(r.pc.wrapping_add(1)) as u16);
        let o = r.operand(mode, wide);
        let flow = if group == 2 && lo == 13 {
            Flow::Call
        } else {
            Flow::Next
        };
        return (
            format!("{name} {o}"),
            target.filter(|_| flow == Flow::Call),
            flow,
        );
    }
    // Prefixed register forms.
    let lo = op & 15;
    let mode = (op >> 4) & 3;
    let group = op >> 6;
    let name = match (page, group, lo) {
        (0x10, 2, 3) => "CMPD",
        (0x10, 2, 12) => "CMPY",
        (0x10, 2, 14) => "LDY",
        (0x10, 2, 15) => "STY",
        (0x10, 3, 14) => "LDS",
        (0x10, 3, 15) => "STS",
        (0x11, 2, 3) => "CMPU",
        (0x11, 2, 12) => "CMPS",
        _ => return (format!("FCB ${first:02X},${op:02X}"), None, Flow::Next),
    };
    if op < 0x80 || (mode == 0 && lo == 15) {
        return (format!("FCB ${first:02X},${op:02X}"), None, Flow::Next);
    }
    let o = r.operand(mode, true);
    (format!("{name} {o}"), None, Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dis(bytes: &[u8]) -> (String, u16) {
        let v = bytes.to_vec();
        let f = move |a: u16| {
            v.get(usize::from(a.wrapping_sub(0x1000)))
                .copied()
                .unwrap_or(0)
        };
        let l = disassemble(&f, 0x1000);
        (l.text, l.len)
    }
    #[test]
    fn decodes_common_forms() {
        assert_eq!(dis(&[0x86, 0x41]), ("LDA #$41".into(), 2));
        assert_eq!(dis(&[0xcc, 0x12, 0x34]), ("LDD #$1234".into(), 3));
        assert_eq!(dis(&[0x10, 0x8e, 0x00, 0x10]), ("LDY #$0010".into(), 4));
        assert_eq!(dis(&[0xa6, 0x84]), ("LDA ,X".into(), 2));
        assert_eq!(dis(&[0xa6, 0x9f, 0xff, 0xfe]), ("LDA [$FFFE]".into(), 4));
        assert_eq!(dis(&[0xa6, 0x1f]), ("LDA -1,X".into(), 2));
        assert_eq!(dis(&[0x30, 0x8d, 0x00, 0x10]), ("LEAX $1014,PCR".into(), 4));
        assert_eq!(dis(&[0x34, 0x16]), ("PSHS A,B,X".into(), 2));
        assert_eq!(dis(&[0x35, 0x80]), ("PULS PC".into(), 2));
        assert_eq!(dis(&[0x1f, 0x89]), ("TFR A,B".into(), 2));
        assert_eq!(dis(&[0x20, 0xfe]), ("BRA $1000".into(), 2));
        assert_eq!(dis(&[0x10, 0x27, 0x00, 0x00]), ("LBEQ $1004".into(), 4));
        assert_eq!(dis(&[0xbd, 0xc0, 0x00]), ("JSR $C000".into(), 3));
        assert_eq!(dis(&[0x10, 0x3f, 0x8a]), ("OS9 I$Write".into(), 3));
        assert_eq!(dis(&[0x10, 0x3f, 0x03]), ("OS9 F$Fork".into(), 3));
        assert_eq!(dis(&[0x4f]), ("CLRA".into(), 1));
        assert_eq!(dis(&[0x0f, 0x20]), ("CLR <$20".into(), 2));
        assert_eq!(dis(&[0x01]), ("FCB $01".into(), 1));
    }
}
