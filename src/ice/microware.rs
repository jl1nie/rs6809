//! Microware C (OS-9/6809) adapter: storage locations and frame rules.
//!
//! The compiler leaves no symbol table, so this combines the declarations
//! parsed from the preprocessed source (`cdecl`) with layout rules checked
//! against its output (see docs/ice.md):
//!
//! - Prologue `pshs u`: the frame base F is S after it; the return address
//!   is at F+2 and the caller's U (the register variable) at F+0.
//! - Parameters start at F+4 in order, 2 bytes each (`char` widened, value
//!   in the low byte), `long` 4 and `double` 8.
//! - Locals are allocated downward from F in declaration order without
//!   padding; nested blocks continue below their parent, siblings reuse it.
//! - The first `register` variable of int or pointer size (parameters
//!   first) lives in U; later ones are ordinary stack variables.
//! - Statics and globals are Y-relative: section base (from the link map)
//!   plus the `vsect` offset.
//! - Addressing is S-relative, so the frame base at each instruction comes
//!   from tracking pushes, pops, `leas` and `,s++`-style operands.
use super::cdecl::{CBlock, CDecl, CProgram, Storage};
use super::debuginfo::{DebugInfo, Function, Loc, Type, Var, VarKind};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Insn {
    pub off: u16,
    pub label: Option<String>,
    pub op: String,
    pub operand: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    Udat,
    Idat,
    Udpd,
    Idpd,
}

#[derive(Debug, Clone)]
pub struct DataLabel {
    pub name: String,
    pub kind: DataKind,
    /// Section-relative offset.
    pub off: u16,
    /// The C source line being compiled when the label was emitted.
    pub line: Option<(String, u32)>,
}

/// Section bases from the link map: code, idat, udat, idpd, udpd.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bases {
    pub code: u16,
    pub idat: u16,
    pub udat: u16,
    pub idpd: u16,
    pub udpd: u16,
}
impl Bases {
    fn data(&self, k: DataKind) -> u16 {
        match k {
            DataKind::Udat => self.udat,
            DataKind::Idat => self.idat,
            DataKind::Udpd => self.udpd,
            DataKind::Idpd => self.idpd,
        }
    }
}

pub struct Input<'a> {
    pub insns: &'a [Insn],
    /// Function name and start offset, from `ttl` + label.
    pub functions: &'a [(String, u16)],
    /// End of the code section (table offset).
    pub code_end: u16,
    pub data: &'a [DataLabel],
    pub bases: Bases,
    /// Exported data symbols from the map: name → Y offset.
    pub map_data: &'a HashMap<String, u16>,
    pub program: CProgram,
}

/// Runtime routines that pop their stack argument (checked: `_lmove`).
/// Unknown effects are caught at run time by validating the return address.
const CALLEE_POPS: [(&str, i16); 1] = [("_lmove", 2)];

fn reg_bytes(list: &str) -> (i16, bool) {
    let mut n = 0;
    let mut pc = false;
    for r in list.split(',') {
        match r.trim().to_ascii_lowercase().as_str() {
            "cc" | "a" | "b" | "dp" => n += 1,
            "pc" => {
                n += 2;
                pc = true;
            }
            "d" | "x" | "y" | "u" | "s" => n += 2,
            _ => {}
        }
    }
    (n, pc)
}

/// Frame-base depth (bytes from S up to F) before each instruction.
fn frame_depths(insns: &[Insn], start: u16, end: u16) -> Vec<(u16, i16)> {
    let body: Vec<&Insn> = insns
        .iter()
        .filter(|i| i.off >= start && i.off < end)
        .collect();
    let local: HashMap<&str, u16> = body
        .iter()
        .filter_map(|i| i.label.as_deref().map(|l| (l.trim_end_matches(':'), i.off)))
        .collect();
    let mut at_label: HashMap<String, i16> = HashMap::new();
    let mut out = Vec::new();
    let mut depth: i16 = if body
        .first()
        .is_some_and(|i| i.op == "pshs" && i.operand.eq_ignore_ascii_case("u"))
    {
        -2
    } else {
        0
    };
    let mut reachable = true;
    for i in body {
        if let Some(l) = &i.label {
            let l = l.trim_end_matches(':');
            if !reachable && let Some(&d) = at_label.get(l) {
                depth = d;
            }
            reachable = true;
        }
        if i.op.is_empty() {
            continue;
        }
        out.push((i.off, depth));
        let target = i.operand.split(',').next().unwrap_or("");
        let op = i.op.as_str();
        let s = i.operand.to_ascii_lowercase();
        match op {
            "pshs" => depth += reg_bytes(&i.operand).0,
            "puls" => {
                let (n, pc) = reg_bytes(&i.operand);
                depth -= n;
                if pc {
                    reachable = false;
                }
            }
            "leas" => {
                if let Some(k) = s.strip_suffix(",s").and_then(|k| k.parse::<i16>().ok()) {
                    depth -= k;
                }
            }
            // A call into this function itself is the literal trick
            // (`bsr _4; fdb ...; _4 puls x`): it pushes and never returns.
            "bsr" | "lbsr" if local.contains_key(target) => {
                at_label.entry(target.to_string()).or_insert(depth + 2);
                depth += 2;
                reachable = false;
            }
            // Runtime helpers that remove their stack argument themselves.
            "bsr" | "lbsr" | "jsr" if CALLEE_POPS.iter().any(|(n, _)| *n == target) => {
                depth -= CALLEE_POPS
                    .iter()
                    .find(|(n, _)| *n == target)
                    .map_or(0, |x| x.1);
            }
            "bra" | "lbra" | "jmp" => {
                at_label.entry(target.to_string()).or_insert(depth);
                reachable = false;
            }
            "rts" | "rti" => reachable = false,
            _ if (op.starts_with('b') || op.starts_with("lb"))
                && op != "bsr"
                && op != "lbsr"
                && !op.starts_with("bit") =>
            {
                at_label.entry(target.to_string()).or_insert(depth);
            }
            _ => {}
        }
        // Auto-increment/decrement of S in any operand.
        if s.contains(",s++") {
            depth -= 2;
        } else if s.contains(",s+") {
            depth -= 1;
        } else if s.contains(",--s") {
            depth += 2;
        } else if s.contains(",-s") {
            depth += 1;
        }
    }
    out
}

struct Layout<'a> {
    info: &'a mut DebugInfo,
    data: &'a [DataLabel],
    bases: Bases,
    used: Vec<bool>,
    u_taken: bool,
}

impl Layout<'_> {
    fn size(&self, t: &Type) -> Option<u16> {
        self.info.size_of(t)
    }
    fn register_ok(&self, t: &Type) -> bool {
        matches!(t, Type::Int { size: 2, .. } | Type::Ptr(_))
    }
    /// The vsect label emitted for a static declared on this line.
    fn static_loc(&mut self, d: &CDecl) -> Loc {
        let pos = self.data.iter().enumerate().position(|(i, l)| {
            !self.used[i]
                && (l.name == d.name
                    || l.line
                        .as_ref()
                        .is_some_and(|(f, n)| *n == d.line && f == &d.file))
        });
        match pos {
            Some(i) => {
                self.used[i] = true;
                let l = &self.data[i];
                Loc::Data(self.bases.data(l.kind).wrapping_add(l.off))
            }
            None => Loc::Unknown("静的変数の位置が見つかりません"),
        }
    }
    fn block(
        &mut self,
        b: &CBlock,
        mut cum: Option<u16>,
        lines: Option<(u32, u32)>,
        out: &mut Vec<Var>,
    ) {
        for d in &b.decls {
            let (loc, kind) = match d.storage {
                Storage::Typedef => continue,
                Storage::Extern => continue,
                Storage::Static => (self.static_loc(d), VarKind::Static),
                Storage::Register if !self.u_taken && self.register_ok(&d.ty) => {
                    self.u_taken = true;
                    (Loc::Reg("U"), VarKind::Local)
                }
                _ => match (cum, self.size(&d.ty)) {
                    (Some(c), Some(sz)) => {
                        let c = c + sz;
                        cum = Some(c);
                        (Loc::Frame(-(c as i16)), VarKind::Local)
                    }
                    _ => {
                        cum = None;
                        (
                            Loc::Unknown("サイズの分からない変数より後ろです"),
                            VarKind::Local,
                        )
                    }
                },
            };
            out.push(Var {
                name: d.name.clone(),
                ty: d.ty.clone(),
                loc,
                kind,
                lines,
            });
        }
        for inner in &b.blocks {
            self.block(inner, cum, Some((inner.first, inner.last)), out);
        }
    }
}

pub fn build(input: Input) -> DebugInfo {
    let mut info = DebugInfo {
        structs: input.program.structs.clone(),
        data_reg: "Y",
        ..Default::default()
    };
    let mut layout_used = vec![false; input.data.len()];
    let mut starts: Vec<(String, u16)> = input.functions.to_vec();
    starts.sort_by_key(|f| f.1);
    for (i, (name, start)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(input.code_end, |n| n.1);
        let cfa = frame_depths(input.insns, *start, end);
        let prologue = input
            .insns
            .iter()
            .find(|x| x.off >= *start && !x.op.is_empty())
            .is_some_and(|x| x.op == "pshs" && x.operand.eq_ignore_ascii_case("u"));
        let mut vars = Vec::new();
        let mut file = String::new();
        if let Some(cf) = input.program.functions.iter().find(|f| &f.name == name) {
            file = cf.file.clone();
            let mut l = Layout {
                info: &mut info,
                data: input.data,
                bases: input.bases,
                used: std::mem::take(&mut layout_used),
                u_taken: false,
            };
            let mut off: Option<u16> = if prologue { Some(4) } else { None };
            for p in &cf.params {
                let slot = match &p.ty {
                    Type::Char { .. } | Type::Int { size: 2, .. } | Type::Ptr(_) => Some(2),
                    Type::Int { size: 4, .. } => Some(4),
                    Type::Float(_) => Some(8),
                    _ => None,
                };
                let loc = if p.storage == Storage::Register && !l.u_taken && l.register_ok(&p.ty) {
                    l.u_taken = true;
                    Loc::Reg("U")
                } else {
                    match (off, slot) {
                        // A widened char keeps its value in the low byte.
                        (Some(o), Some(_)) if matches!(p.ty, Type::Char { .. }) => {
                            Loc::Frame(o as i16 + 1)
                        }
                        (Some(o), Some(_)) => Loc::Frame(o as i16),
                        _ => Loc::Unknown("引数の位置が分かりません"),
                    }
                };
                off = match (off, slot) {
                    (Some(o), Some(s)) => Some(o + s),
                    _ => None,
                };
                vars.push(Var {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    loc,
                    kind: VarKind::Param,
                    lines: None,
                });
            }
            l.block(
                &cf.body,
                if prologue { Some(0) } else { None },
                None,
                &mut vars,
            );
            layout_used = l.used;
        }
        info.functions.push(Function {
            name: name.clone(),
            start: *start,
            end,
            file,
            vars,
            cfa,
            ret_at: prologue.then_some(2),
            saved: if prologue { vec![("U", 0)] } else { Vec::new() },
        });
    }
    for g in &input.program.globals {
        if g.storage == Storage::Typedef {
            continue;
        }
        let loc = if let Some(&v) = input.map_data.get(&g.name) {
            Loc::Data(v)
        } else if let Some(i) = input
            .data
            .iter()
            .enumerate()
            .position(|(i, l)| !layout_used[i] && l.name == g.name)
        {
            layout_used[i] = true;
            let l = &input.data[i];
            Loc::Data(input.bases.data(l.kind).wrapping_add(l.off))
        } else {
            Loc::Unknown("他のファイルで定義されています")
        };
        info.globals.push(Var {
            name: g.name.clone(),
            ty: g.ty.clone(),
            loc,
            kind: if g.storage == Storage::Static {
                VarKind::Static
            } else {
                VarKind::Global
            },
            lines: None,
        });
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn(off: u16, label: Option<&str>, op: &str, operand: &str) -> Insn {
        Insn {
            off,
            label: label.map(str::to_string),
            op: op.into(),
            operand: operand.into(),
        }
    }

    #[test]
    fn tracks_the_frame_base() {
        // From the listing of `locals` in the probe program.
        let code = [
            insn(0x1d, Some("locals:"), "pshs", "u"),
            insn(0x1f, None, "ldd", "#_3"),
            insn(0x22, None, "lbsr", "_stkcheck"),
            insn(0x25, None, "leas", "-21,s"),
            insn(0x34, None, "leax", "14,s"),
            insn(0x36, None, "pshs", "x"),
            insn(0x38, None, "bsr", "_4"),
            insn(0x3a, None, "fdb", "4386,13124"),
            insn(0x3e, Some("_4"), "puls", "x"),
            insn(0x40, None, "lbsr", "_lmove"),
            insn(0x78, None, "lbra", "_8"),
            insn(0x7b, Some("_6"), "leas", "-2,s"),
            insn(0x7c, None, "pshs", "d"),
            insn(0x7d, None, "addd", ",s++"),
            insn(0x91, None, "leas", "2,s"),
            insn(0x9c, Some("_8"), "ldd", "18,s"),
            insn(0xa3, None, "lblt", "_6"),
            insn(0xc1, None, "leas", "21,s"),
            insn(0xc4, None, "puls", "u,pc"),
        ];
        let d: HashMap<u16, i16> = frame_depths(&code, 0x1d, 0xc6).into_iter().collect();
        assert_eq!(d[&0x1d], -2);
        assert_eq!(d[&0x25], 0);
        assert_eq!(d[&0x34], 21);
        assert_eq!(d[&0x38], 23);
        assert_eq!(d[&0x3e], 25, "the literal's address was pushed by bsr");
        assert_eq!(d[&0x40], 23);
        assert_eq!(d[&0x7b], 21);
        assert_eq!(d[&0x7d], 25);
        assert_eq!(d[&0x91], 23);
        assert_eq!(d[&0x9c], 21);
        assert_eq!(d[&0xc4], 0);
    }
}
