//! Compiler-neutral variable information: types, storage locations and
//! frame rules, plus value formatting and simple C expressions.
//!
//! A compiler adapter (e.g. `microware.rs`) fills a [`DebugInfo`] from
//! whatever its tools leave behind; the debugger front ends (DAP, browser)
//! only use this module.

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Void,
    Char {
        signed: bool,
    },
    Int {
        size: u8,
        signed: bool,
    },
    Float(u8),
    Ptr(Box<Type>),
    Array(Box<Type>, Option<u16>),
    /// Index into [`DebugInfo::structs`].
    Struct(usize),
    Func(Box<Type>),
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub off: u16,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructDef {
    pub tag: String,
    pub union: bool,
    pub fields: Vec<Field>,
    /// `None` while incomplete or when a member's size is unknown.
    pub size: Option<u16>,
}

/// Where a variable lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Loc {
    /// Relative to the frame base (CFA) of the function.
    Frame(i16),
    /// Kept in a register (the value, not an address).
    Reg(&'static str),
    /// Relative to the data base register of the program.
    Data(u16),
    Abs(u16),
    /// Placement not known; the reason is shown.
    Unknown(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Param,
    Local,
    Static,
    Global,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Var {
    pub name: String,
    pub ty: Type,
    pub loc: Loc,
    pub kind: VarKind,
    /// Source lines (inclusive) where a block-local variable is in scope.
    pub lines: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    /// Code range in table offsets.
    pub start: u16,
    pub end: u16,
    /// Source file name as recorded in the line table.
    pub file: String,
    pub vars: Vec<Var>,
    /// (offset, depth): before the instruction at `offset`, the frame base
    /// is S + depth. Sorted by offset.
    pub cfa: Vec<(u16, i16)>,
    /// Frame-base offset of the return address.
    pub ret_at: Option<i16>,
    /// Registers of the caller saved in this frame: (name, frame offset).
    pub saved: Vec<(&'static str, i16)>,
}

impl Function {
    pub fn depth_at(&self, off: u16) -> Option<i16> {
        let i = self.cfa.partition_point(|c| c.0 <= off);
        i.checked_sub(1).map(|i| self.cfa[i].1)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DebugInfo {
    pub functions: Vec<Function>,
    pub globals: Vec<Var>,
    pub structs: Vec<StructDef>,
    /// Register holding the base of `Loc::Data` (Microware C: Y).
    pub data_reg: &'static str,
}

impl DebugInfo {
    pub fn function_at(&self, off: u16) -> Option<&Function> {
        self.functions
            .iter()
            .find(|f| off >= f.start && off < f.end)
    }
    pub fn size_of(&self, t: &Type) -> Option<u16> {
        match t {
            Type::Void | Type::Func(_) | Type::Unknown => None,
            Type::Char { .. } => Some(1),
            Type::Int { size, .. } | Type::Float(size) => Some(u16::from(*size)),
            Type::Ptr(_) => Some(2),
            Type::Array(e, n) => Some(self.size_of(e)?.checked_mul((*n)?)?),
            Type::Struct(i) => self.structs.get(*i)?.size,
        }
    }
    pub fn type_name(&self, t: &Type) -> String {
        match t {
            Type::Void => "void".into(),
            Type::Char { signed: true } => "char".into(),
            Type::Char { signed: false } => "unsigned char".into(),
            Type::Int { size: 4, signed } => if *signed { "long" } else { "unsigned long" }.into(),
            Type::Int { signed, .. } => if *signed { "int" } else { "unsigned" }.into(),
            Type::Float(4) => "float".into(),
            Type::Float(_) => "double".into(),
            Type::Ptr(inner) => match **inner {
                Type::Func(_) => format!("{} (*)()", self.type_name(inner_ret(inner))),
                _ => format!("{} *", self.type_name(inner)),
            },
            Type::Array(e, n) => format!(
                "{}[{}]",
                self.type_name(e),
                n.map_or(String::new(), |n| n.to_string())
            ),
            Type::Struct(i) => {
                let s = &self.structs[*i];
                let kw = if s.union { "union" } else { "struct" };
                if s.tag.is_empty() {
                    kw.into()
                } else {
                    format!("{kw} {}", s.tag)
                }
            }
            Type::Func(r) => format!("{} ()", self.type_name(r)),
            Type::Unknown => "?".into(),
        }
    }
}

fn inner_ret(t: &Type) -> &Type {
    match t {
        Type::Func(r) => r,
        other => other,
    }
}

/// Where a value can be read from at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Mem(u16),
    /// A register value (register variables).
    Value(u16),
}

/// Machine access for formatting and expressions.
pub struct Ctx<'a> {
    pub read: &'a dyn Fn(u16) -> u8,
    /// Code label for an address (`name+$off`), for pointers.
    pub label: &'a dyn Fn(u16) -> Option<String>,
    pub info: &'a DebugInfo,
}

impl Ctx<'_> {
    fn bytes(&self, p: Place, n: u16) -> Vec<u8> {
        match p {
            Place::Mem(a) => (0..n).map(|i| (self.read)(a.wrapping_add(i))).collect(),
            Place::Value(v) => {
                let b = v.to_be_bytes();
                match n {
                    1 => vec![b[1]],
                    _ => b.to_vec(),
                }
            }
        }
    }
    fn word(&self, p: Place) -> u16 {
        let b = self.bytes(p, 2);
        u16::from_be_bytes([b[0], b[1]])
    }
    fn string_at(&self, a: u16, max: u16) -> String {
        let mut s = String::from("\"");
        for i in 0..max {
            let c = (self.read)(a.wrapping_add(i));
            match c {
                0 => {
                    s.push('"');
                    return s;
                }
                b'\r' => s.push_str("\\r"),
                b'\n' => s.push_str("\\n"),
                b'"' => s.push_str("\\\""),
                0x20..=0x7e => s.push(char::from(c)),
                _ => s.push_str(&format!("\\x{c:02X}")),
            }
        }
        s.push('…');
        s
    }

    /// The value as shown in a variables list.
    pub fn format(&self, ty: &Type, p: Place) -> String {
        self.format_depth(ty, p, 0)
    }
    fn format_depth(&self, ty: &Type, p: Place, depth: u8) -> String {
        match ty {
            Type::Char { signed } => {
                let b = self.bytes(p, 1)[0];
                let v = if *signed {
                    i32::from(b as i8)
                } else {
                    i32::from(b)
                };
                if (0x20..0x7f).contains(&b) {
                    format!("{v} '{}'", char::from(b))
                } else {
                    v.to_string()
                }
            }
            Type::Int { size: 4, signed } => {
                let b = self.bytes(p, 4);
                let u = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                if *signed {
                    format!("{} (${u:08X})", u as i32)
                } else {
                    format!("{u} (${u:08X})")
                }
            }
            Type::Int { signed, .. } => {
                let u = self.word(p);
                if *signed {
                    format!("{} (${u:04X})", u as i16)
                } else {
                    format!("{u} (${u:04X})")
                }
            }
            Type::Float(n) => {
                let hex: String = self
                    .bytes(p, u16::from(*n))
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                format!("${hex}（浮動小数点は未対応）")
            }
            Type::Ptr(inner) => {
                let a = self.word(p);
                let mut s = format!("${a:04X}");
                if a == 0 {
                    s.push_str(" NULL");
                } else if matches!(**inner, Type::Char { .. }) {
                    s.push(' ');
                    s.push_str(&self.string_at(a, 40));
                } else if let Some(l) = (self.label)(a) {
                    s.push_str(&format!(" <{l}>"));
                }
                s
            }
            Type::Array(e, n) => {
                let Place::Mem(a) = p else { return "?".into() };
                if matches!(**e, Type::Char { .. }) {
                    return self.string_at(a, n.unwrap_or(40).min(80));
                }
                if depth > 0 {
                    return format!("[{}]", n.map_or("?".into(), |n| n.to_string()));
                }
                let size = self.info.size_of(e).unwrap_or(0).max(1);
                let shown = n.unwrap_or(0).min(8);
                let items: Vec<String> = (0..shown)
                    .map(|i| self.format_depth(e, Place::Mem(a.wrapping_add(i * size)), depth + 1))
                    .collect();
                let more = if n.unwrap_or(0) > shown { ", …" } else { "" };
                format!("{{{}{more}}}", items.join(", "))
            }
            Type::Struct(i) => {
                let Place::Mem(a) = p else { return "?".into() };
                if depth > 0 {
                    return "{…}".into();
                }
                let s = &self.info.structs[*i];
                let items: Vec<String> = s
                    .fields
                    .iter()
                    .take(6)
                    .map(|f| {
                        format!(
                            "{}={}",
                            f.name,
                            self.format_depth(&f.ty, Place::Mem(a.wrapping_add(f.off)), depth + 1)
                        )
                    })
                    .collect();
                let more = if s.fields.len() > 6 { ", …" } else { "" };
                format!("{{{}{more}}}", items.join(", "))
            }
            Type::Func(_) => "関数".into(),
            Type::Void | Type::Unknown => "?".into(),
        }
    }

    /// Members of an aggregate, or the target of a pointer.
    pub fn children(&self, ty: &Type, p: Place) -> Vec<(String, Type, Place)> {
        match (ty, p) {
            (Type::Array(e, Some(n)), Place::Mem(a)) => {
                let size = self.info.size_of(e).unwrap_or(1).max(1);
                (0..(*n).min(256))
                    .map(|i| {
                        (
                            format!("[{i}]"),
                            (**e).clone(),
                            Place::Mem(a.wrapping_add(i * size)),
                        )
                    })
                    .collect()
            }
            (Type::Struct(i), Place::Mem(a)) => self.info.structs[*i]
                .fields
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        f.ty.clone(),
                        Place::Mem(a.wrapping_add(f.off)),
                    )
                })
                .collect(),
            (Type::Ptr(inner), _)
                if !matches!(**inner, Type::Void | Type::Func(_) | Type::Unknown) =>
            {
                let a = self.word(p);
                if a == 0 {
                    return Vec::new();
                }
                vec![(format!("*${a:04X}"), (**inner).clone(), Place::Mem(a))]
            }
            _ => Vec::new(),
        }
    }
    pub fn expandable(&self, ty: &Type) -> bool {
        match ty {
            Type::Array(_, Some(_)) | Type::Struct(_) => true,
            Type::Ptr(inner) => !matches!(
                **inner,
                Type::Void | Type::Func(_) | Type::Unknown | Type::Char { .. }
            ),
            _ => false,
        }
    }

    /// Evaluate `name`, `a.b`, `p->x`, `a[2]`, `*p` against visible variables.
    pub fn eval(
        &self,
        expr: &str,
        lookup: &dyn Fn(&str) -> Option<(Type, Place)>,
    ) -> Option<(Type, Place)> {
        let e = expr.trim();
        if let Some(rest) = e.strip_prefix('*') {
            let (t, p) = self.eval(rest, lookup)?;
            return self.deref(&t, p);
        }
        let end = e
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(e.len());
        let (mut t, mut p) = lookup(&e[..end])?;
        let mut rest = &e[end..];
        while !rest.is_empty() {
            rest = rest.trim_start();
            if let Some(r) = rest.strip_prefix("->") {
                (t, p) = self.deref(&t, p)?;
                rest = r;
                (t, p, rest) = self.field(&t, p, rest)?;
            } else if let Some(r) = rest.strip_prefix('.') {
                (t, p, rest) = self.field(&t, p, r)?;
            } else {
                let r = rest.strip_prefix('[')?;
                let close = r.find(']')?;
                let idx: i32 = r[..close].trim().parse().ok()?;
                let (elem, base) = match (&t, p) {
                    (Type::Array(e, _), Place::Mem(a)) => ((**e).clone(), a),
                    (Type::Ptr(e), _) => ((**e).clone(), self.word(p)),
                    _ => return None,
                };
                let size = self.info.size_of(&elem)? as i32;
                t = elem;
                p = Place::Mem(base.wrapping_add((idx * size) as u16));
                rest = &r[close + 1..];
            }
        }
        Some((t, p))
    }
    fn deref(&self, t: &Type, p: Place) -> Option<(Type, Place)> {
        match t {
            Type::Ptr(inner) => Some(((**inner).clone(), Place::Mem(self.word(p)))),
            Type::Array(e, _) => match p {
                Place::Mem(a) => Some(((**e).clone(), Place::Mem(a))),
                Place::Value(_) => None,
            },
            _ => None,
        }
    }
    fn field<'e>(&self, t: &Type, p: Place, rest: &'e str) -> Option<(Type, Place, &'e str)> {
        let rest = rest.trim_start();
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let (Type::Struct(i), Place::Mem(a)) = (t, p) else {
            return None;
        };
        let f = self.info.structs[*i]
            .fields
            .iter()
            .find(|f| f.name == rest[..end])?;
        Some((
            f.ty.clone(),
            Place::Mem(a.wrapping_add(f.off)),
            &rest[end..],
        ))
    }
}

/// Bytes to store `text` into a scalar of type `ty`: decimal, `$hex`,
/// `0x`, or `'c'`.
pub fn encode(info: &DebugInfo, ty: &Type, text: &str) -> Option<Vec<u8>> {
    let s = text.trim();
    let v: i64 = if let Some(h) = s.strip_prefix('$').or_else(|| s.strip_prefix("0x")) {
        i64::from_str_radix(h, 16).ok()?
    } else if s.len() == 3 && s.starts_with('\'') && s.ends_with('\'') {
        i64::from(s.as_bytes()[1])
    } else {
        s.parse().ok()?
    };
    let n = match ty {
        Type::Char { .. } | Type::Int { .. } | Type::Ptr(_) => info.size_of(ty)?,
        _ => return None,
    };
    let b = (v as u32).to_be_bytes();
    Some(b[4 - usize::from(n)..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_evaluates() {
        let info = DebugInfo {
            structs: vec![StructDef {
                tag: "pt".into(),
                union: false,
                fields: vec![
                    Field {
                        name: "tag".into(),
                        off: 0,
                        ty: Type::Char { signed: true },
                    },
                    Field {
                        name: "x".into(),
                        off: 1,
                        ty: Type::Int {
                            size: 2,
                            signed: true,
                        },
                    },
                ],
                size: Some(3),
            }],
            ..Default::default()
        };
        let mem = [b'T', 0xff, 0xfe, 0x10, 0x00, b'h', b'i', 0];
        let read = |a: u16| mem.get(usize::from(a)).copied().unwrap_or(0);
        let ctx = Ctx {
            read: &read,
            label: &|_| None,
            info: &info,
        };
        let s = Type::Struct(0);
        assert_eq!(ctx.format(&s, Place::Mem(0)), "{tag=84 'T', x=-2 ($FFFE)}");
        let pstr = Type::Ptr(Box::new(Type::Char { signed: true }));
        assert_eq!(
            ctx.format(&Type::Ptr(Box::new(Type::Void)), Place::Value(0)),
            "$0000 NULL"
        );
        let lookup = |n: &str| match n {
            "s" => Some((s.clone(), Place::Mem(0))),
            "p" => Some((Type::Ptr(Box::new(s.clone())), Place::Value(0))),
            "q" => Some((pstr.clone(), Place::Value(5))),
            _ => None,
        };
        let (t, p) = ctx.eval("p->x", &lookup).unwrap();
        assert_eq!(
            (t, p),
            (
                Type::Int {
                    size: 2,
                    signed: true
                },
                Place::Mem(1)
            )
        );
        assert_eq!(ctx.format(&pstr, Place::Value(5)), "$0005 \"hi\"");
        let (t, p) = ctx.eval("q[1]", &lookup).unwrap();
        assert_eq!(ctx.format(&t, p), "105 'i'");
        assert_eq!(
            encode(
                &info,
                &Type::Int {
                    size: 2,
                    signed: true
                },
                "-2"
            ),
            Some(vec![0xff, 0xfe])
        );
        assert_eq!(
            info.type_name(&Type::Array(Box::new(pstr), Some(3))),
            "char *[3]"
        );
    }
}
