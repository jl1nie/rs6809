//! Minimal JSON values for the Debug Adapter Protocol (no external crates).
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn get(&self, key: &str) -> &Json {
        static NULL: Json = Json::Null;
        match self {
            Json::Obj(m) => m.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        self.num().map(|n| n as i64)
    }
    pub fn bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("trailing data at {}", p.i));
        }
        Ok(v)
    }
}

impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{b}"),
            Json::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
            Json::Num(n) => write!(f, "{n}"),
            Json::Str(s) => f.write_str(&super::json_str(s)),
            Json::Arr(v) => {
                f.write_char('[')?;
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{x}")?;
                }
                f.write_char(']')
            }
            Json::Obj(m) => {
                f.write_char('{')?;
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{}:{x}", super::json_str(k))?;
                }
                f.write_char('}')
            }
        }
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Self {
        Json::Str(s.to_string())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Self {
        Json::Str(s)
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Self {
        Json::Bool(b)
    }
}
macro_rules! from_num {
    ($($t:ty),*) => {$(
        impl From<$t> for Json {
            fn from(n: $t) -> Self {
                Json::Num(n as f64)
            }
        }
    )*};
}
from_num!(u8, u16, u32, u64, usize, i32, i64);
impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(v: Vec<T>) -> Self {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Self {
        v.map_or(Json::Null, Into::into)
    }
}

/// Build an object: `obj([("a", 1.into()), ...])`.
pub fn obj<const N: usize>(pairs: [(&str, Json); N]) -> Json {
    Json::Obj(
        pairs
            .into_iter()
            .filter(|(_, v)| *v != Json::Null)
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        let Some(&c) = self.s.get(self.i) else {
            return Err("unexpected end".into());
        };
        match c {
            b'{' => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.eat("}") {
                    return Ok(Json::Obj(m));
                }
                loop {
                    self.ws();
                    let Json::Str(k) = self.value()? else {
                        return Err("object key".into());
                    };
                    self.ws();
                    if !self.eat(":") {
                        return Err("expected ':'".into());
                    }
                    m.insert(k, self.value()?);
                    self.ws();
                    if self.eat("}") {
                        return Ok(Json::Obj(m));
                    }
                    if !self.eat(",") {
                        return Err("expected ','".into());
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.eat("]") {
                    return Ok(Json::Arr(v));
                }
                loop {
                    v.push(self.value()?);
                    self.ws();
                    if self.eat("]") {
                        return Ok(Json::Arr(v));
                    }
                    if !self.eat(",") {
                        return Err("expected ','".into());
                    }
                }
            }
            b'"' => self.string().map(Json::Str),
            b't' if self.eat("true") => Ok(Json::Bool(true)),
            b'f' if self.eat("false") => Ok(Json::Bool(false)),
            b'n' if self.eat("null") => Ok(Json::Null),
            _ => {
                let start = self.i;
                while self.i < self.s.len() && b"+-0123456789.eE".contains(&self.s[self.i]) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i])
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .map(Json::Num)
                    .ok_or_else(|| format!("bad value at {start}"))
            }
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.s.get(self.i..self.i + 4).ok_or("short \\u")?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).map_err(|e| e.to_string())?, 16)
            .map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return Err("unterminated string".into());
            };
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let e = *self.s.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.eat("\\u") {
                                let lo = self.hex4()?;
                                u = 0x10000
                                    + ((u - 0xd800) << 10)
                                    + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        other => char::from(other),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let v = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= n {
                s.push(char::from(B64[(v >> (18 - 6 * i) & 63) as usize]));
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = acc << 6 | B64.iter().position(|&b| b == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let v = Json::parse(r#"{"a":[1,2.5,"x\né"],"b":{"c":null,"d":true}}"#).unwrap();
        assert_eq!(v.get("a").arr()[2].str(), Some("x\né"));
        assert_eq!(v.get("b").get("d").bool(), Some(true));
        assert_eq!(
            v.to_string(),
            r#"{"a":[1,2.5,"x\né"],"b":{"c":null,"d":true}}"#
        );
        assert_eq!(base64(b"6809!"), "NjgwOSE=");
        assert_eq!(unbase64("NjgwOSE=").unwrap(), b"6809!");
    }
}
