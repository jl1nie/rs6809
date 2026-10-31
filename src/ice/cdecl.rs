//! Declarations from preprocessed C (K&R and simple ANSI forms): globals,
//! functions with their parameters, and block-local variables with the
//! source lines of their blocks. Statements are skipped; only braces matter.
//! The result is compiler-neutral; an adapter assigns storage locations.
use super::debuginfo::{Field, StructDef, Type};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Auto,
    Register,
    Static,
    Extern,
    Typedef,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CDecl {
    pub name: String,
    pub ty: Type,
    pub storage: Storage,
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CBlock {
    pub decls: Vec<CDecl>,
    pub blocks: Vec<CBlock>,
    pub first: u32,
    pub last: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CFunc {
    pub name: String,
    pub file: String,
    pub params: Vec<CDecl>,
    pub body: CBlock,
}

#[derive(Debug, Default)]
pub struct CProgram {
    pub globals: Vec<CDecl>,
    pub functions: Vec<CFunc>,
    pub structs: Vec<StructDef>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(i64),
    Punct(&'static str),
    Str,
}

const PUNCT: [&str; 46] = [
    "...", "<<=", ">>=", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "{", "}", "(", ")", "[", "]", ";", ",", ":", "=",
    "*", "&", "+", "-", "/", "%", "<", ">", "!", "~", "^", "|", "?", ".",
];

fn tokenize(lines: &[(String, u32, String)]) -> Vec<(Tok, usize)> {
    let mut out = Vec::new();
    let mut in_comment = false;
    for (li, (_, _, text)) in lines.iter().enumerate() {
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if in_comment {
                if b[i..].starts_with(b"*/") {
                    in_comment = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            let c = b[i];
            if c.is_ascii_whitespace() {
                i += 1;
            } else if b[i..].starts_with(b"/*") {
                in_comment = true;
                i += 2;
            } else if c.is_ascii_alphabetic() || c == b'_' {
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push((Tok::Ident(text[s..i].to_string()), li));
            } else if c.is_ascii_digit() {
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                let t = text[s..i].trim_end_matches(['l', 'L', 'u', 'U']);
                let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    i64::from_str_radix(h, 16).unwrap_or(0)
                } else if t.len() > 1 && t.starts_with('0') {
                    i64::from_str_radix(&t[1..], 8).unwrap_or(0)
                } else {
                    t.parse().unwrap_or(0)
                };
                out.push((Tok::Num(v), li));
            } else if c == b'"' || c == b'\'' {
                let s = i;
                i += 1;
                while i < b.len() && b[i] != c {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                if c == b'\'' {
                    let inner = &b[s + 1..(i - 1).min(b.len())];
                    let v = match inner {
                        [b'\\', b'n'] => 13,
                        [b'\\', b'r'] => 13,
                        [b'\\', b'0'] => 0,
                        [b'\\', x] => i64::from(*x),
                        [x, ..] => i64::from(*x),
                        [] => 0,
                    };
                    out.push((Tok::Num(v), li));
                } else {
                    out.push((Tok::Str, li));
                }
            } else if let Some(p) = PUNCT.iter().find(|p| b[i..].starts_with(p.as_bytes())) {
                out.push((Tok::Punct(p), li));
                i += p.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// One declarator: pointers, an inner declarator or name, and suffixes.
struct Declarator {
    ptrs: usize,
    inner: Option<Box<Declarator>>,
    name: Option<String>,
    suffixes: Vec<Suffix>,
}
enum Suffix {
    Array(Option<u16>),
    Func(Vec<(String, Option<Type>, bool)>),
}

struct Parser<'a> {
    toks: Vec<(Tok, usize)>,
    lines: &'a [(String, u32, String)],
    pos: usize,
    typedefs: HashMap<String, Type>,
    tags: HashMap<String, usize>,
    structs: Vec<StructDef>,
}

const TYPE_WORDS: [&str; 14] = [
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "struct",
    "union", "enum", "const", "volatile",
];
const STORAGE_WORDS: [&str; 5] = ["typedef", "static", "extern", "register", "auto"];

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }
    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n).map(|t| &t.0)
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(x)) if *x == p)
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn ident(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::Ident(s)) => Some(s),
            _ => None,
        }
    }
    fn line(&self) -> (String, u32) {
        let li = self
            .toks
            .get(self.pos)
            .or(self.toks.last())
            .map_or(0, |t| t.1);
        self.lines
            .get(li)
            .map_or((String::new(), 0), |l| (l.0.clone(), l.1))
    }
    fn decl_start(&self) -> bool {
        match self.ident() {
            Some(w) if TYPE_WORDS.contains(&w) || STORAGE_WORDS.contains(&w) => true,
            Some(w) if self.typedefs.contains_key(w) => {
                // `name x` or `name *x`, not `name = ...` or `name(...)`.
                matches!(self.peek_at(1), Some(Tok::Ident(_)) | Some(Tok::Punct("*")))
            }
            _ => false,
        }
    }
    /// Skip to the matching close of the bracket at the cursor.
    fn skip_group(&mut self) {
        let mut depth = 0i32;
        while let Some(t) = self.peek() {
            if let Tok::Punct(p) = t {
                match *p {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    _ => {}
                }
            }
            self.pos += 1;
            if depth <= 0 {
                return;
            }
        }
    }
    /// Skip an initializer up to `,` or `;` at the top level.
    fn skip_init(&mut self) {
        while let Some(t) = self.peek() {
            match t {
                Tok::Punct("," | ";") => return,
                Tok::Punct("(" | "[" | "{") => self.skip_group(),
                Tok::Punct("}") => return,
                _ => self.pos += 1,
            }
        }
    }
    fn const_expr(&mut self, close: &str) -> Option<i64> {
        // Numbers combined with + - * / and parentheses.
        let start = self.pos;
        let mut depth = 0;
        while let Some(t) = self.peek() {
            match t {
                Tok::Punct(p) if *p == close && depth == 0 => break,
                Tok::Punct("(") => depth += 1,
                Tok::Punct(")") => depth -= 1,
                _ => {}
            }
            self.pos += 1;
        }
        let toks: Vec<Tok> = self.toks[start..self.pos]
            .iter()
            .map(|t| t.0.clone())
            .collect();
        fn expr(t: &[Tok], i: &mut usize) -> Option<i64> {
            let mut v = term(t, i)?;
            while let Some(Tok::Punct(p @ ("+" | "-"))) = t.get(*i) {
                *i += 1;
                let r = term(t, i)?;
                v = if *p == "+" { v + r } else { v - r };
            }
            Some(v)
        }
        fn term(t: &[Tok], i: &mut usize) -> Option<i64> {
            let mut v = atom(t, i)?;
            while let Some(Tok::Punct(p @ ("*" | "/"))) = t.get(*i) {
                *i += 1;
                let r = atom(t, i)?;
                v = if *p == "*" { v * r } else { v.checked_div(r)? };
            }
            Some(v)
        }
        fn atom(t: &[Tok], i: &mut usize) -> Option<i64> {
            match t.get(*i)? {
                Tok::Num(n) => {
                    *i += 1;
                    Some(*n)
                }
                Tok::Punct("-") => {
                    *i += 1;
                    atom(t, i).map(|v| -v)
                }
                Tok::Punct("(") => {
                    *i += 1;
                    let v = expr(t, i)?;
                    *i += 1;
                    Some(v)
                }
                _ => None,
            }
        }
        let mut i = 0;
        let v = expr(&toks, &mut i)?;
        (i == toks.len()).then_some(v)
    }

    fn struct_body(&mut self, idx: usize, union: bool) {
        // At '{'.
        self.pos += 1;
        let mut fields = Vec::new();
        let mut off: Option<u16> = Some(0);
        let mut size: Option<u16> = Some(0);
        while self.peek().is_some() && !self.is("}") {
            let Some((_, base)) = self.specifiers() else {
                self.skip_init();
                self.eat(";");
                continue;
            };
            loop {
                let d = self.declarator();
                if self.eat(":") {
                    self.const_expr(";");
                }
                let (name, ty, _) = apply(base.clone(), d);
                let fsize = self.size_of(&ty);
                if let Some(name) = name {
                    fields.push(Field {
                        name,
                        off: if union { 0 } else { off.unwrap_or(u16::MAX) },
                        ty,
                    });
                }
                if union {
                    size = match (size, fsize) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        _ => None,
                    };
                } else {
                    off = match (off, fsize) {
                        (Some(a), Some(b)) => Some(a + b),
                        _ => None,
                    };
                    size = off;
                }
                if !self.eat(",") {
                    break;
                }
            }
            if !self.eat(";") {
                self.skip_init();
                self.eat(";");
            }
        }
        self.eat("}");
        let s = &mut self.structs[idx];
        s.fields = fields;
        s.union = union;
        s.size = size;
    }

    fn size_of(&self, t: &Type) -> Option<u16> {
        match t {
            Type::Char { .. } => Some(1),
            Type::Int { size, .. } | Type::Float(size) => Some(u16::from(*size)),
            Type::Ptr(_) => Some(2),
            Type::Array(e, n) => Some(self.size_of(e)?.checked_mul((*n)?)?),
            Type::Struct(i) => self.structs.get(*i)?.size,
            _ => None,
        }
    }

    /// Storage class and base type; `None` when no specifier is present.
    fn specifiers(&mut self) -> Option<(Storage, Type)> {
        let mut storage = Storage::Auto;
        let (mut unsigned, mut short, mut long, mut seen) = (false, false, 0, false);
        // A typedef name is a type only before any other type word.
        let mut typed = false;
        let mut base: Option<Type> = None;
        while let Some(w) = self.ident().map(str::to_string) {
            match w.as_str() {
                "typedef" => storage = Storage::Typedef,
                "static" => storage = Storage::Static,
                "extern" => storage = Storage::Extern,
                "register" => storage = Storage::Register,
                "auto" | "const" | "volatile" | "signed" => {}
                "unsigned" => unsigned = true,
                "short" => short = true,
                "long" => long += 1,
                "int" => {}
                "char" => base = Some(Type::Char { signed: true }),
                "float" => base = Some(Type::Float(4)),
                "double" => base = Some(Type::Float(8)),
                "void" => base = Some(Type::Void),
                "struct" | "union" => {
                    self.pos += 1;
                    let union = w == "union";
                    let tag = self.ident().map(str::to_string);
                    if tag.is_some() {
                        self.pos += 1;
                    }
                    let idx = match tag.as_ref().and_then(|t| self.tags.get(t)) {
                        Some(&i) => i,
                        None => {
                            self.structs.push(StructDef {
                                tag: tag.clone().unwrap_or_default(),
                                union,
                                ..Default::default()
                            });
                            let i = self.structs.len() - 1;
                            if let Some(t) = tag {
                                self.tags.insert(t, i);
                            }
                            i
                        }
                    };
                    if self.is("{") {
                        self.struct_body(idx, union);
                    }
                    base = Some(Type::Struct(idx));
                    seen = true;
                    continue;
                }
                "enum" => {
                    self.pos += 1;
                    if self.ident().is_some() {
                        self.pos += 1;
                    }
                    if self.is("{") {
                        self.skip_group();
                    }
                    base = Some(Type::Int {
                        size: 2,
                        signed: true,
                    });
                    seen = true;
                    continue;
                }
                name if !typed
                    && base.is_none()
                    && !unsigned
                    && !short
                    && long == 0
                    && self.typedefs.contains_key(name) =>
                {
                    base = self.typedefs.get(name).cloned();
                }
                _ => break,
            }
            seen = true;
            typed |=
                !STORAGE_WORDS.contains(&w.as_str()) && !matches!(w.as_str(), "const" | "volatile");
            self.pos += 1;
        }
        if !seen {
            return None;
        }
        let ty = match base {
            Some(Type::Char { .. }) => Type::Char { signed: !unsigned },
            Some(Type::Float(_)) if long > 0 => Type::Float(8),
            Some(t) => t,
            None if long > 0 => Type::Int {
                size: 4,
                signed: !unsigned,
            },
            None => Type::Int {
                size: 2,
                signed: !unsigned,
            },
        };
        Some((storage, ty))
    }

    fn declarator(&mut self) -> Declarator {
        let mut d = Declarator {
            ptrs: 0,
            inner: None,
            name: None,
            suffixes: Vec::new(),
        };
        while self.eat("*") {
            d.ptrs += 1;
            while matches!(self.ident(), Some("const" | "volatile")) {
                self.pos += 1;
            }
        }
        if self.is("(")
            && !matches!(self.peek_at(1), Some(Tok::Punct(")")))
            && !self.param_list_ahead()
        {
            self.pos += 1;
            d.inner = Some(Box::new(self.declarator()));
            self.eat(")");
        } else if let Some(n) = self.ident().map(str::to_string) {
            d.name = Some(n);
            self.pos += 1;
        }
        loop {
            if self.eat("[") {
                let n = self.const_expr("]").and_then(|v| u16::try_from(v).ok());
                self.eat("]");
                d.suffixes.push(Suffix::Array(n));
            } else if self.is("(") {
                self.pos += 1;
                let params = self.params();
                d.suffixes.push(Suffix::Func(params));
            } else {
                break;
            }
        }
        d
    }
    /// After `(`: a parameter list rather than a parenthesized declarator?
    fn param_list_ahead(&self) -> bool {
        match self.peek_at(1) {
            Some(Tok::Ident(w)) => {
                TYPE_WORDS.contains(&w.as_str())
                    || STORAGE_WORDS.contains(&w.as_str())
                    || self.typedefs.contains_key(w)
            }
            _ => false,
        }
    }
    /// Parameter names (K&R) or typed parameters (ANSI), after `(`.
    fn params(&mut self) -> Vec<(String, Option<Type>, bool)> {
        let mut out = Vec::new();
        while self.peek().is_some() && !self.eat(")") {
            if self.eat(",") {
                continue;
            }
            if self.eat("...") {
                continue;
            }
            if self.decl_start() {
                let Some((storage, base)) = self.specifiers() else {
                    break;
                };
                let d = self.declarator();
                let (name, ty, _) = apply(base, d);
                if let Some(n) = name {
                    out.push((n, Some(param_type(ty)), storage == Storage::Register));
                } else if ty == Type::Void {
                    // f(void)
                }
            } else if let Some(n) = self.ident().map(str::to_string) {
                self.pos += 1;
                out.push((n, None, false));
            } else {
                self.pos += 1;
            }
        }
        out
    }

    /// A declaration list (`specifiers declarator [= init], ... ;`).
    fn declaration(&mut self) -> Vec<CDecl> {
        let (file, line) = self.line();
        let Some((storage, base)) = self.specifiers() else {
            self.skip_init();
            self.eat(";");
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            let d = self.declarator();
            let (name, ty, _) = apply(base.clone(), d);
            if self.eat("=") {
                self.skip_init();
            }
            if let Some(name) = name {
                if storage == Storage::Typedef {
                    self.typedefs.insert(name.clone(), ty.clone());
                }
                out.push(CDecl {
                    name,
                    ty,
                    storage,
                    file: file.clone(),
                    line,
                });
            }
            if !self.eat(",") {
                break;
            }
        }
        if !self.eat(";") {
            self.skip_init();
            self.eat(";");
        }
        out
    }

    fn block(&mut self) -> CBlock {
        let mut b = CBlock {
            first: self.line().1,
            ..Default::default()
        };
        self.eat("{");
        while self.decl_start() {
            let d = self.declaration();
            b.decls.extend(d);
        }
        loop {
            match self.peek() {
                None => break,
                Some(Tok::Punct("}")) => {
                    b.last = self.line().1;
                    self.pos += 1;
                    break;
                }
                Some(Tok::Punct("{")) => {
                    let inner = self.block();
                    b.blocks.push(inner);
                }
                _ => self.pos += 1,
            }
        }
        b
    }

    fn unit(&mut self) -> CProgram {
        let mut prog = CProgram::default();
        while self.peek().is_some() {
            if self.eat(";") {
                continue;
            }
            let start = self.pos;
            let (file, line) = self.line();
            // `main()` without specifiers is an int function.
            let (storage, base) = self.specifiers().unwrap_or((
                Storage::Auto,
                Type::Int {
                    size: 2,
                    signed: true,
                },
            ));
            let mut first = true;
            loop {
                let d = self.declarator();
                let knr = matches!(d.suffixes.first(), Some(Suffix::Func(_))) && d.inner.is_none();
                let (name, ty, params) = apply(base.clone(), d);
                if first && knr && !self.is(";") && !self.is(",") && !self.is("=") {
                    // A function definition: K&R parameter declarations, then the body.
                    let mut params: Vec<CDecl> = params
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(n, t, reg)| CDecl {
                            name: n,
                            ty: t.unwrap_or(Type::Int {
                                size: 2,
                                signed: true,
                            }),
                            storage: if reg {
                                Storage::Register
                            } else {
                                Storage::Auto
                            },
                            file: file.clone(),
                            line,
                        })
                        .collect();
                    while self.decl_start() {
                        for d in self.declaration() {
                            if let Some(p) = params.iter_mut().find(|p| p.name == d.name) {
                                p.ty = param_type(d.ty);
                                p.storage = d.storage;
                            }
                        }
                    }
                    if !self.is("{") {
                        break;
                    }
                    let body = self.block();
                    if let Some(name) = name {
                        prog.functions.push(CFunc {
                            name,
                            file: file.clone(),
                            params,
                            body,
                        });
                    }
                    break;
                }
                first = false;
                if self.eat("=") {
                    self.skip_init();
                }
                if let Some(name) = name {
                    if storage == Storage::Typedef {
                        self.typedefs.insert(name.clone(), ty.clone());
                    } else if !matches!(ty, Type::Func(_)) {
                        prog.globals.push(CDecl {
                            name,
                            ty,
                            storage,
                            file: file.clone(),
                            line,
                        });
                    }
                }
                if !self.eat(",") {
                    self.eat(";");
                    break;
                }
            }
            if self.pos == start {
                self.pos += 1;
            }
        }
        prog.structs = std::mem::take(&mut self.structs);
        prog
    }
}

/// Arrays decay to pointers and `char` widens in parameter lists; the
/// declared type is kept, the adapter decides the slot size.
fn param_type(t: Type) -> Type {
    match t {
        Type::Array(e, _) => Type::Ptr(e),
        other => other,
    }
}

/// Parameters: name, declared type (ANSI), `register`.
type Params = Vec<(String, Option<Type>, bool)>;

fn apply(base: Type, d: Declarator) -> (Option<String>, Type, Option<Params>) {
    let mut t = base;
    for _ in 0..d.ptrs {
        t = Type::Ptr(Box::new(t));
    }
    let mut params = None;
    for (i, s) in d
        .suffixes
        .into_iter()
        .enumerate()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        t = match s {
            Suffix::Array(n) => Type::Array(Box::new(t), n),
            Suffix::Func(p) => {
                if i == 0 {
                    params = Some(p);
                }
                Type::Func(Box::new(t))
            }
        };
    }
    match d.inner {
        Some(inner) => {
            let (n, t, _) = apply(t, *inner);
            (n, t, None)
        }
        None => (d.name, t, params),
    }
}

/// Parse preprocessed lines `(file, line, text)`.
pub fn parse(lines: &[(String, u32, String)]) -> CProgram {
    let mut p = Parser {
        toks: tokenize(lines),
        lines,
        pos: 0,
        typedefs: HashMap::new(),
        tags: HashMap::new(),
        structs: Vec::new(),
    };
    p.unit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<(String, u32, String)> {
        src.lines()
            .enumerate()
            .map(|(i, l)| ("t.c".to_string(), i as u32 + 1, l.to_string()))
            .collect()
    }

    #[test]
    fn knr_program() {
        let p = parse(&lines(
            "typedef struct _iobuf { char *_ptr; int _flag; } FILE;
extern FILE _iob[16];
long ftell();
int gcount, garr[2*2];
static char *gname = \"x{\";
struct pt { char tag; int x; long y; };
int f(a, b, s)
char a;
register int b;
struct pt *s;
{
    char c1;
    struct pt v;
    int (*fp)();
    {
        long inner;
    }
    if (a) { int x; }
    return b;
}
main()
{
    FILE *fp;
}",
        ));
        let names: Vec<_> = p
            .globals
            .iter()
            .map(|g| (g.name.as_str(), g.storage))
            .collect();
        assert_eq!(
            names,
            [
                ("_iob", Storage::Extern),
                ("gcount", Storage::Auto),
                ("garr", Storage::Auto),
                ("gname", Storage::Static)
            ]
        );
        assert_eq!(
            p.globals[2].ty,
            Type::Array(
                Box::new(Type::Int {
                    size: 2,
                    signed: true
                }),
                Some(4)
            )
        );
        let f = &p.functions[0];
        assert_eq!(
            f.params.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "s"]
        );
        assert_eq!(f.params[0].ty, Type::Char { signed: true });
        assert_eq!(f.params[1].storage, Storage::Register);
        assert!(matches!(&f.params[2].ty, Type::Ptr(t) if **t == Type::Struct(1)));
        assert_eq!(
            f.body
                .decls
                .iter()
                .map(|x| x.name.as_str())
                .collect::<Vec<_>>(),
            ["c1", "v", "fp"]
        );
        assert!(matches!(&f.body.decls[2].ty, Type::Ptr(t) if matches!(**t, Type::Func(_))));
        assert_eq!(f.body.blocks.len(), 2);
        assert_eq!((f.body.blocks[0].first, f.body.blocks[0].last), (15, 17));
        assert_eq!(f.body.blocks[1].decls[0].name, "x");
        assert_eq!(p.structs[1].size, Some(7));
        assert_eq!(p.structs[1].fields[2].off, 3);
        assert_eq!(p.functions[1].name, "main");
        assert_eq!(
            p.functions[1].body.decls[0].ty,
            Type::Ptr(Box::new(Type::Struct(0)))
        );
    }
}
