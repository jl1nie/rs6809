//! Breakpoint conditions: a small integer expression language over the
//! registers, memory and symbols.
//!
//! ```text
//! D0 == 3 && [A0].b != 0      registers by name, memory as [addr] (.b .w .l)
//! X >= $C000 || pid == 2      $hex, 0xhex, decimal, 'c'; pid is the OS process
//! (SR & $2000) == 0           + - * / % & | ^ << >> ~ ! unary -, comparisons
//! ```
//!
//! `[expr]` reads a byte, `[expr].w` a word and `[expr].l` a long (big-endian).
//! Names that are not registers are looked up as symbols. Values are 64-bit
//! signed; a condition is true when its value is not zero.

use crate::tr;

/// What an expression can see.
pub trait Env {
    fn reg(&self, name: &str) -> Option<i64>;
    fn sym(&self, name: &str) -> Option<i64>;
    fn mem(&self, addr: u32, size: u8) -> i64;
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Name(String),
    Op(&'static str),
}

const OPS: [&str; 26] = [
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "&", "|", "^", "~", "!", "<", ">", "(", ")", "[", "]", ".", "=",
];

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' && i + 2 < b.len() && b[i + 2] == b'\'' {
            out.push(Tok::Num(i64::from(b[i + 1])));
            i += 3;
            continue;
        }
        let hex = if c == '$' {
            Some(1)
        } else if s[i..].starts_with("0x") || s[i..].starts_with("0X") {
            Some(2)
        } else {
            None
        };
        if let Some(skip) = hex {
            let start = i + skip;
            let mut j = start;
            while j < b.len() && (b[j] as char).is_ascii_hexdigit() {
                j += 1;
            }
            let v = i64::from_str_radix(&s[start..j], 16).map_err(|_| tr!(format!("16進の数が正しくない: {}", &s[i..j]), format!("bad hex number: {}", &s[i..j])))?;
            out.push(Tok::Num(v));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < b.len() && (b[j] as char).is_ascii_digit() {
                j += 1;
            }
            out.push(Tok::Num(s[i..j].parse().map_err(|_| tr!("数が大きすぎる", "number too large").to_string())?));
            i = j;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == '@' {
            let mut j = i;
            while j < b.len() && ((b[j] as char).is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'@' || b[j] == b'$') {
                j += 1;
            }
            out.push(Tok::Name(s[i..j].to_string()));
            i = j;
            continue;
        }
        match OPS.iter().find(|o| s[i..].starts_with(**o)) {
            Some(o) => {
                out.push(Tok::Op(o));
                i += o.len();
            }
            None => return Err(tr!(format!("使えない文字: {c}"), format!("unexpected character: {c}"))),
        }
    }
    Ok(out)
}

/// A parsed expression, evaluated each time the breakpoint is reached.
#[derive(Debug, Clone)]
pub struct Expr {
    toks: Vec<Tok>,
}

impl Expr {
    pub fn parse(s: &str) -> Result<Self, String> {
        let toks = lex(s)?;
        if toks.is_empty() {
            return Err(tr!("式が空", "empty expression").into());
        }
        // Check the syntax once with an environment that knows every name.
        struct Any;
        impl Env for Any {
            fn reg(&self, _: &str) -> Option<i64> {
                Some(1)
            }
            fn sym(&self, _: &str) -> Option<i64> {
                Some(1)
            }
            fn mem(&self, _: u32, _: u8) -> i64 {
                1
            }
        }
        let e = Self { toks };
        e.eval(&Any)?;
        Ok(e)
    }
    pub fn eval(&self, env: &dyn Env) -> Result<i64, String> {
        let mut p = Parser { t: &self.toks, i: 0, env };
        let v = p.expr(0)?;
        if p.i != self.toks.len() {
            return Err(tr!("式の終わりに余分なものがある", "extra text after the expression").into());
        }
        Ok(v)
    }
}

struct Parser<'a> {
    t: &'a [Tok],
    i: usize,
    env: &'a dyn Env,
}

fn prec(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" | "=" => 6,
        "<" | "<=" | ">" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn eat(&mut self, op: &str) -> bool {
        if self.peek() == Some(&Tok::Op(leak(op))) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self, min: u8) -> Result<i64, String> {
        let mut l = self.unary()?;
        while let Some(Tok::Op(op)) = self.peek() {
            let op = *op;
            let Some(p) = prec(op) else { break };
            if p < min {
                break;
            }
            self.i += 1;
            let r = self.expr(p + 1)?;
            l = match op {
                "||" => i64::from(l != 0 || r != 0),
                "&&" => i64::from(l != 0 && r != 0),
                "|" => l | r,
                "^" => l ^ r,
                "&" => l & r,
                "==" | "=" => i64::from(l == r),
                "!=" => i64::from(l != r),
                "<" => i64::from(l < r),
                "<=" => i64::from(l <= r),
                ">" => i64::from(l > r),
                ">=" => i64::from(l >= r),
                "<<" => l.wrapping_shl(r as u32),
                ">>" => l.wrapping_shr(r as u32),
                "+" => l.wrapping_add(r),
                "-" => l.wrapping_sub(r),
                "*" => l.wrapping_mul(r),
                "/" | "%" if r == 0 => return Err(tr!("0で割った", "division by zero").into()),
                "/" => l.wrapping_div(r),
                _ => l.wrapping_rem(r),
            };
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<i64, String> {
        if self.eat("-") {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat("~") {
            return Ok(!self.unary()?);
        }
        if self.eat("!") {
            return Ok(i64::from(self.unary()? == 0));
        }
        self.atom()
    }
    fn atom(&mut self) -> Result<i64, String> {
        let missing = || tr!("値が足りない", "a value is missing").to_string();
        match self.peek().cloned().ok_or_else(missing)? {
            Tok::Num(v) => {
                self.i += 1;
                Ok(v)
            }
            Tok::Name(n) => {
                self.i += 1;
                if n.eq_ignore_ascii_case("pid")
                    && let Some(v) = self.env.reg("pid")
                {
                    return Ok(v);
                }
                self.env
                    .reg(&n)
                    .or_else(|| self.env.sym(&n))
                    .ok_or_else(|| tr!(format!("知らない名前: {n}"), format!("unknown name: {n}")))
            }
            Tok::Op("(") => {
                self.i += 1;
                let v = self.expr(0)?;
                if !self.eat(")") {
                    return Err(tr!("「)」がない", "missing )").into());
                }
                Ok(v)
            }
            Tok::Op("[") => {
                self.i += 1;
                let a = self.expr(0)?;
                if !self.eat("]") {
                    return Err(tr!("「]」がない", "missing ]").into());
                }
                let mut size = 1;
                if self.eat(".") {
                    size = match self.peek() {
                        Some(Tok::Name(s)) if s.eq_ignore_ascii_case("b") => 1,
                        Some(Tok::Name(s)) if s.eq_ignore_ascii_case("w") => 2,
                        Some(Tok::Name(s)) if s.eq_ignore_ascii_case("l") => 4,
                        _ => return Err(tr!("大きさは .b .w .l のどれか", "the size is .b, .w or .l").into()),
                    };
                    self.i += 1;
                }
                Ok(self.env.mem(a as u32, size))
            }
            Tok::Op(o) => Err(tr!(format!("ここに「{o}」は置けない"), format!("unexpected {o}"))),
        }
    }
}

fn leak(op: &str) -> &'static str {
    OPS.iter().find(|o| **o == op).copied().unwrap_or("")
}

/// How often a breakpoint stops: `N` (the Nth time and after, as VS Code's
/// default), `=N`, `>=N`, `>N` or `%N` (every Nth time).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hits {
    AtLeast(u64),
    Exactly(u64),
    Every(u64),
}

impl Hits {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let num = |t: &str| t.trim().parse::<u64>().map_err(|_| tr!(format!("回数が正しくない: {s}"), format!("bad hit count: {s}")));
        Ok(if let Some(t) = s.strip_prefix(">=") {
            Hits::AtLeast(num(t)?)
        } else if let Some(t) = s.strip_prefix('>') {
            Hits::AtLeast(num(t)? + 1)
        } else if let Some(t) = s.strip_prefix("==").or_else(|| s.strip_prefix('=')) {
            Hits::Exactly(num(t)?)
        } else if let Some(t) = s.strip_prefix('%') {
            Hits::Every(num(t)?.max(1))
        } else {
            Hits::AtLeast(num(s)?)
        })
    }
    pub fn stops(self, n: u64) -> bool {
        match self {
            Hits::AtLeast(k) => n >= k,
            Hits::Exactly(k) => n == k,
            Hits::Every(k) => n.is_multiple_of(k),
        }
    }
}

/// Expand `{expr}` in a log message.
pub fn log_text(msg: &str, env: &dyn Env) -> String {
    let mut out = String::new();
    let mut rest = msg;
    while let Some(a) = rest.find('{') {
        out.push_str(&rest[..a]);
        match rest[a..].find('}') {
            Some(b) => {
                let e = &rest[a + 1..a + b];
                match Expr::parse(e).and_then(|x| x.eval(env)) {
                    Ok(v) if v < 0 => out.push_str(&v.to_string()),
                    Ok(v) => out.push_str(&format!("${v:X}")),
                    Err(err) => out.push_str(&format!("<{err}>")),
                }
                rest = &rest[a + b + 1..];
            }
            None => {
                out.push_str(&rest[a..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    struct E;
    impl Env for E {
        fn reg(&self, n: &str) -> Option<i64> {
            match n.to_ascii_lowercase().as_str() {
                "d0" => Some(3),
                "a0" => Some(0x100),
                "x" => Some(0xC000),
                "pid" => Some(2),
                _ => None,
            }
        }
        fn sym(&self, n: &str) -> Option<i64> {
            (n == "main").then_some(0x1000)
        }
        fn mem(&self, a: u32, size: u8) -> i64 {
            // Each byte holds its address's low byte.
            (0..size).fold(0, |v, i| v << 8 | i64::from(a.wrapping_add(u32::from(i)) as u8))
        }
    }
    fn ev(s: &str) -> i64 {
        Expr::parse(s).unwrap().eval(&E).unwrap()
    }
    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ev("1 + 2 * 3"), 7);
        assert_eq!(ev("(1 + 2) * 3"), 9);
        assert_eq!(ev("$10 | 0x01"), 0x11);
        assert_eq!(ev("-1 + 2"), 1);
        assert_eq!(ev("~0 & $FF"), 0xFF);
        assert_eq!(ev("1 << 4 >> 2"), 4);
        assert_eq!(ev("'A'"), 65);
    }
    #[test]
    fn conditions() {
        assert_eq!(ev("D0 == 3 && x >= $C000"), 1);
        assert_eq!(ev("d0 != 3 || !pid"), 0);
        assert_eq!(ev("main + 2"), 0x1002);
        assert_eq!(ev("d0 = 3"), 1);
    }
    #[test]
    fn memory() {
        assert_eq!(ev("[a0+5]"), 5);
        assert_eq!(ev("[$1234].w"), 0x3435);
        assert_eq!(ev("[$10].l"), 0x10111213);
    }
    #[test]
    fn errors() {
        assert!(Expr::parse("").is_err());
        assert!(Expr::parse("1 +").is_err());
        assert!(Expr::parse("(1").is_err());
        assert!(Expr::parse("[1].q").is_err());
        assert!(Expr::parse("1 # 2").is_err());
        assert!(Expr::parse("nosuch").unwrap().eval(&E).is_err());
        assert!(Expr::parse("1 / 0").is_err());
    }
    #[test]
    fn hits_and_logs() {
        assert!(!Hits::parse("3").unwrap().stops(2));
        assert!(Hits::parse("3").unwrap().stops(4));
        assert!(Hits::parse("=3").unwrap().stops(3));
        assert!(!Hits::parse("=3").unwrap().stops(4));
        assert!(Hits::parse("%2").unwrap().stops(4));
        assert!(!Hits::parse(">3").unwrap().stops(3));
        assert!(Hits::parse("x").is_err());
        assert_eq!(log_text("d0={d0} at {x} {nosuch", &E), "d0=$3 at $C000 {nosuch");
    }
}
