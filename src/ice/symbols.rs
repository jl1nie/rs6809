//! Symbols and source lines from assembler listings and linker maps.
//!
//! Supported inputs:
//! - LWASM listings (`lwasm --list`): labels, code ranges and source text.
//!   A listing whose code starts with an OS-9 module header (`$87CD`) holds
//!   module-relative offsets; otherwise addresses are absolute.
//! - LWLINK maps (`Symbol: name (file) = XXXX`): absolute symbols.
//! - Microware RMA listings (`c.asm -l`) together with the `c.link -m -s`
//!   map of the same base name, and for C the `c.prep -l` output (`.m`).
//!   `c.prep -l` copies every source line into the assembly as a `* *`
//!   comment and records its file and line in the `.m` file.
//!
//! Module-relative tables are placed through the live OS-9 module directory,
//! as physical RAM ranges, so the same table works in Level 1 and in every
//! Level 2 address space that maps the module.
use super::debuginfo::DebugInfo;
use super::microware::{self, Bases, DataKind, DataLabel, Insn};
use super::os9::{Level, View};
use crate::Machine;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Source {
    /// The name recorded by the tool, e.g. `hello.c` or a truncated
    /// `s/source-echo.asm` from an LWASM listing.
    pub name: String,
    /// The host file, when one was found.
    pub path: Option<PathBuf>,
    pub text: BTreeMap<u32, String>,
}

/// Code bytes `[off, end)` generated for one source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRec {
    pub off: u16,
    pub end: u16,
    pub src: usize,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub path: PathBuf,
    pub kind: &'static str,
    /// OS-9 module these offsets belong to; `None` for absolute addresses.
    pub module: Option<String>,
    /// Sorted by offset.
    pub symbols: Vec<(u16, String)>,
    /// Sorted by offset.
    pub lines: Vec<LineRec>,
    pub sources: Vec<Source>,
    /// Variables and frames, when the compiler's output allows it.
    pub debug: Option<DebugInfo>,
}

impl Table {
    fn new(path: &Path, kind: &'static str) -> Self {
        Self {
            path: path.to_path_buf(),
            kind,
            module: None,
            symbols: Vec::new(),
            lines: Vec::new(),
            sources: Vec::new(),
            debug: None,
        }
    }
    fn source(&mut self, name: &str) -> usize {
        if let Some(i) = self.sources.iter().position(|s| s.name == name) {
            return i;
        }
        self.sources.push(Source {
            name: name.to_string(),
            path: None,
            text: BTreeMap::new(),
        });
        self.sources.len() - 1
    }
    fn finish(&mut self) {
        self.symbols.sort();
        self.symbols.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        self.lines.sort_by_key(|l| (l.off, l.end));
        self.lines.retain(|l| l.end > l.off);
    }
    pub fn line_at(&self, off: u16) -> Option<&LineRec> {
        let i = self.lines.partition_point(|l| l.off <= off);
        self.lines[..i].iter().rev().take(8).find(|l| off < l.end)
    }
    pub fn symbol_at(&self, off: u16) -> Option<(u16, &str)> {
        let i = self.symbols.partition_point(|s| s.0 <= off);
        i.checked_sub(1)
            .map(|i| (self.symbols[i].0, self.symbols[i].1.as_str()))
    }
    /// The nearest global symbol at or below `off`; local labels (`l@`,
    /// numeric, compiler `_123`) are only used when exact.
    pub fn base_symbol(&self, off: u16) -> Option<(u16, &str)> {
        let local = |n: &str| {
            n.contains('@')
                || n.starts_with(|c: char| c.is_ascii_digit())
                || (n.starts_with('_') && n[1..].bytes().all(|c| c.is_ascii_digit()))
        };
        let i = self.symbols.partition_point(|s| s.0 <= off);
        self.symbols[..i]
            .iter()
            .rev()
            .find(|s| s.0 == off || !local(&s.1))
            .map(|s| (s.0, s.1.as_str()))
    }
    /// Whether an absolute table describes this address.
    fn covers(&self, addr: u16) -> bool {
        if self.line_at(addr).is_some() {
            return true;
        }
        match (self.symbols.first(), self.symbols.last()) {
            (Some(a), Some(b)) => addr >= a.0 && u32::from(addr) < u32::from(b.0) + 256,
            _ => false,
        }
    }
}

fn text_of(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}
fn hex16(s: &str) -> Option<u16> {
    (s.len() == 4).then(|| u16::from_str_radix(s, 16).ok())?
}

/// LWASM `--list` output: `AAAA BYTES (file):NNNNN source`.
fn parse_lwasm(path: &Path, text: &str) -> Result<Table, String> {
    let mut t = Table::new(path, "lwasm");
    let mut bytes: HashMap<u16, u8> = HashMap::new();
    let mut last: Option<usize> = None;
    let mut seen = false;
    for line in text.lines() {
        let Some(open) = line.find(" (").map(|i| i + 1) else {
            // Continuation of the previous line's bytes.
            let h = line.trim();
            if let Some(i) = last
                && !h.is_empty()
                && h.len() % 2 == 0
                && h.bytes().all(|c| c.is_ascii_hexdigit())
                && line.starts_with("     ")
            {
                let l = &mut t.lines[i];
                for k in 0..h.len() / 2 {
                    if let Ok(b) = u8::from_str_radix(&h[k * 2..k * 2 + 2], 16) {
                        bytes.insert(l.end.wrapping_add(k as u16), b);
                    }
                }
                l.end = l.end.wrapping_add((h.len() / 2) as u16);
            }
            continue;
        };
        let rest = &line[open + 1..];
        let Some(close) = rest.find("):") else {
            continue;
        };
        let num = rest.get(close + 2..close + 7).unwrap_or("");
        let Ok(lineno) = num.parse::<u32>() else {
            continue;
        };
        seen = true;
        let file = rest[..close].trim().to_string();
        // `%05d ` is followed by an 8-column cycle-count field.
        let source = rest.get(close + 16..).unwrap_or("");
        let src = t.source(&file);
        t.sources[src].text.insert(lineno, source.to_string());
        last = None;
        let head = &line[..open];
        let addr = head.get(..4).and_then(hex16);
        let flag = head.as_bytes().get(4).copied().unwrap_or(b' ');
        let Some(addr) = addr.filter(|_| flag == b' ' || flag == b'.') else {
            continue;
        };
        if flag == b'.' {
            continue; // data offsets (rmb), not code addresses
        }
        let hexbytes = head.get(5..).unwrap_or("").trim();
        let n = (hexbytes.len() / 2) as u16;
        for k in 0..usize::from(n) {
            if let Ok(b) = u8::from_str_radix(&hexbytes[k * 2..k * 2 + 2], 16) {
                bytes.insert(addr.wrapping_add(k as u16), b);
            }
        }
        if n > 0 {
            t.lines.push(LineRec {
                off: addr,
                end: addr.wrapping_add(n),
                src,
                line: lineno,
            });
            last = Some(t.lines.len() - 1);
        }
        let first = source.chars().next().unwrap_or(' ');
        if !first.is_whitespace() && first != '*' && first != ';' {
            let mut words = source.split_whitespace();
            let label = words.next().unwrap_or("").trim_end_matches(':');
            let op = words.next().unwrap_or("").to_ascii_lowercase();
            // Code labels: lines that emit bytes, or a label alone on its line.
            // Structure definitions (`org 0` + `rmb`) give data offsets.
            if !label.is_empty() && (n > 0 || op.is_empty()) {
                t.symbols.push((addr, label.to_string()));
            }
        }
    }
    if !seen {
        return Err(format!("{}: not an LWASM listing", path.display()));
    }
    // An OS-9 module: header at offset 0, name at the offset in bytes 4-5.
    let b = |a: u16| bytes.get(&a).copied();
    if b(0) == Some(0x87) && b(1) == Some(0xcd) {
        let off = u16::from_be_bytes([b(4).unwrap_or(0), b(5).unwrap_or(0)]);
        let mut name = String::new();
        for i in 0..32 {
            let Some(c) = b(off + i) else { break };
            name.push(char::from(c & 0x7f));
            if c & 0x80 != 0 {
                break;
            }
        }
        if !name.is_empty() {
            t.module = Some(name);
        }
    }
    t.finish();
    Ok(t)
}

/// LWLINK map: `Symbol: name (file) = XXXX`.
fn parse_lwlink(path: &Path, text: &str) -> Table {
    let mut t = Table::new(path, "lwlink");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Symbol: ")
            && let Some((name, value)) = rest.rsplit_once(" = ")
            && let Ok(v) = u16::from_str_radix(value.trim(), 16)
        {
            let name = name.split(" (").next().unwrap_or(name).trim();
            t.symbols.push((v, name.to_string()));
        }
    }
    t.finish();
    t
}

/// `c.link -m [-s]`: module name, section code offsets and code symbols.
struct LinkMap {
    module: Option<String>,
    sections: HashMap<String, Bases>,
    symbols: Vec<(u16, String)>,
    /// Data symbols (udat/idat/udpd/idpd) as offsets from the data base.
    data: HashMap<String, u16>,
}
fn parse_linkmap(text: &str) -> LinkMap {
    let mut m = LinkMap {
        module: None,
        sections: HashMap::new(),
        symbols: Vec::new(),
        data: HashMap::new(),
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Linkage map for ") {
            m.module = rest.split_whitespace().next().map(str::to_string);
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        if line.starts_with(' ') {
            if w.len() == 3
                && let Some(v) = hex16(w[2])
            {
                match w[1] {
                    "code" => m.symbols.push((v, w[0].to_string())),
                    "udat" | "idat" | "udpd" | "idpd" => {
                        m.data.insert(w[0].to_string(), v);
                    }
                    _ => {}
                }
            }
        } else if w.len() >= 7
            && w[1..6]
                .iter()
                .all(|x| x.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            let h = |x: &str| u16::from_str_radix(x, 16).unwrap_or(0);
            m.sections.insert(
                w[0].to_string(),
                Bases {
                    code: h(w[1]),
                    idat: h(w[2]),
                    udat: h(w[3]),
                    idpd: h(w[4]),
                    udpd: h(w[5]),
                },
            );
        }
    }
    m
}

/// `c.prep -l` output: `#7` file, `#6` source line (one per input line),
/// followed by the preprocessed text of that line.
struct Prep {
    /// (file, line, source text) per `#6` record.
    records: Vec<(String, u32, String)>,
    /// Preprocessed text: (file, line, text).
    expanded: Vec<(String, u32, String)>,
}
fn parse_prep(text: &str) -> Prep {
    let mut p = Prep {
        records: Vec::new(),
        expanded: Vec::new(),
    };
    let mut stack: Vec<(String, u32)> = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        match l {
            "#7" => {
                let name = lines.next().unwrap_or("").to_string();
                lines.next(); // section name
                if stack.len() >= 2 && stack[stack.len() - 2].0 == name {
                    stack.pop();
                } else {
                    stack.push((name, 0));
                }
            }
            "#6" => {
                let src = lines.next().unwrap_or("").to_string();
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                    p.records.push((top.0.clone(), top.1, src));
                }
            }
            "#5" => {
                lines.next();
            }
            "#P" => {
                lines.next();
                lines.next();
            }
            text => {
                if let Some(top) = stack.last() {
                    p.expanded.push((top.0.clone(), top.1, text.to_string()));
                }
            }
        }
    }
    p
}

/// Microware RMA listing (`c.asm -l`): `NNNNN AAAA BYTES   label  op operand`.
fn parse_rma(path: &Path, text: &str) -> Result<Table, String> {
    let mut t = Table::new(path, "rma");
    let sibling = |ext: &str| path.with_extension(ext);
    let map = text_of(&sibling("map"))
        .map(|s| parse_linkmap(&s))
        .map_err(|_| {
            format!(
                "{}: needs the c.link -m -s map {}",
                path.display(),
                sibling("map").display()
            )
        })?;
    let prep = text_of(&sibling("m")).ok().map(|s| parse_prep(&s));
    t.module = map.module.clone();
    t.symbols = map.symbols.clone();
    if t.symbols.is_empty() {
        t.symbols = map
            .sections
            .iter()
            .map(|(n, b)| (b.code, n.clone()))
            .collect();
    }
    let listing = t.source(&path.file_name().unwrap_or_default().to_string_lossy());
    t.sources[listing].path = Some(path.to_path_buf());
    let mut base: Option<u16> = None;
    let mut bases = Bases::default();
    let mut data = false;
    let mut data_dp = false;
    // Inputs for variable information.
    let mut insns: Vec<Insn> = Vec::new();
    let mut functions: Vec<(String, u16)> = Vec::new();
    let mut data_labels: Vec<DataLabel> = Vec::new();
    let mut code_end = 0u16;
    let mut comment = 0usize;
    let mut current: Option<(usize, u32)> = None;
    // (offset, source, line) for each code line; ends come from the next one.
    let mut code: Vec<(u16, usize, u32)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let lst_line = n as u32 + 1;
        t.sources[listing].text.insert(lst_line, line.to_string());
        if line.len() < 6 || !line[..5].bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let rest = &line[6..];
        if let Some(c_text) = rest.strip_prefix("* *") {
            if let Some((file, ln, _)) = prep.as_ref().and_then(|p| p.records.get(comment)) {
                let src = t.source(file);
                t.sources[src].text.insert(*ln, c_text.to_string());
                current = Some((src, *ln));
            }
            comment += 1;
            continue;
        }
        let addr = rest.get(..4).and_then(hex16);
        let fields = rest.get(16..).unwrap_or("");
        let labelled = fields.starts_with(|c: char| !c.is_whitespace());
        let mut words = fields.split_whitespace();
        if labelled {
            words.next();
        }
        let label = labelled.then(|| fields.split_whitespace().next().unwrap_or("").to_string());
        let op = words.next().unwrap_or("").to_ascii_lowercase();
        let operand = words.next().unwrap_or("");
        let c_line = current.map(|(src, ln)| (t.sources[src].name.clone(), ln));
        if data {
            if let (Some(l), Some(a)) = (&label, addr)
                && !matches!(op.as_str(), "endsect" | "equ" | "set")
            {
                let kind = match (op.as_str(), data_dp) {
                    ("rmb", false) => DataKind::Udat,
                    ("rmb", true) => DataKind::Udpd,
                    (_, false) => DataKind::Idat,
                    (_, true) => DataKind::Idpd,
                };
                data_labels.push(DataLabel {
                    name: l.trim_end_matches(':').to_string(),
                    kind,
                    off: a,
                    line: c_line.clone(),
                });
            }
        } else if let (Some(b), Some(a)) = (base, addr)
            && !matches!(
                op.as_str(),
                "equ" | "set" | "psect" | "vsect" | "endsect" | "nam" | "opt"
            )
        {
            if op == "ttl" {
                functions.push((operand.to_string(), b.wrapping_add(a)));
            } else {
                insns.push(Insn {
                    off: b.wrapping_add(a),
                    label: label.clone(),
                    op: op.clone(),
                    operand: operand.to_string(),
                });
            }
        }
        match op.as_str() {
            "psect" => {
                let name = operand.split(',').next().unwrap_or("");
                bases = map.sections.get(name).copied().unwrap_or_default();
                base = map.sections.get(name).map(|b| b.code);
                data = false;
                continue;
            }
            "vsect" => {
                data = true;
                data_dp = operand.eq_ignore_ascii_case("dp");
                continue;
            }
            "endsect" => {
                if data {
                    data = false;
                } else {
                    if let Some(b) = base
                        && let Some(a) = addr
                    {
                        code.push((b.wrapping_add(a), usize::MAX, 0));
                        code_end = b.wrapping_add(a);
                    }
                    base = None;
                }
                continue;
            }
            "" | "equ" | "set" | "nam" | "ttl" | "opt" | "spc" | "pag" | "use" | "ifp1"
            | "endc" => {
                continue;
            }
            _ => {}
        }
        let (Some(b), Some(a), false) = (base, addr, data) else {
            continue;
        };
        if matches!(op.as_str(), "fcc" | "fcb" | "fdb" | "fcs" | "rzb" | "rmb") {
            // Constants in the code section end the previous line's range.
            code.push((b.wrapping_add(a), usize::MAX, 0));
            continue;
        }
        let (src, ln) = current.unwrap_or((listing, lst_line));
        code.push((
            b.wrapping_add(a),
            src,
            if current.is_some() { ln } else { lst_line },
        ));
    }
    if prep.is_none() && comment > 0 {
        // Without the .m file, C lines can only be shown as listing lines.
        for c in &mut code {
            if c.1 != usize::MAX {
                c.1 = listing;
            }
        }
    }
    loop_tails(&mut code, &insns);
    for w in code.windows(2) {
        let (off, src, line) = w[0];
        if src == usize::MAX || w[1].0 <= off {
            continue;
        }
        match t.lines.last_mut() {
            Some(l) if l.src == src && l.line == line && l.end == off => l.end = w[1].0,
            _ => t.lines.push(LineRec {
                off,
                end: w[1].0,
                src,
                line,
            }),
        }
    }
    t.finish();
    if let Some(p) = prep {
        let program = super::cdecl::parse(&p.expanded);
        t.debug = Some(microware::build(microware::Input {
            insns: &insns,
            functions: &functions,
            code_end,
            data: &data_labels,
            bases,
            map_data: &map.data,
            program,
        }));
    }
    Ok(t)
}

/// Microware C places a loop's increment and condition after the body,
/// after the comment of the next statement. Code that ends in a backward
/// conditional branch and is entered by a forward jump from the loop
/// header belongs to the header's line (`for`/`while`), not the next one.
fn loop_tails(code: &mut [(u16, usize, u32)], insns: &[Insn]) {
    let labels: HashMap<&str, u16> = insns
        .iter()
        .filter_map(|i| i.label.as_deref().map(|l| (l.trim_end_matches(':'), i.off)))
        .collect();
    let target = |i: &Insn| {
        labels
            .get(i.operand.split(',').next().unwrap_or(""))
            .copied()
    };
    let conditional = |op: &str| {
        (op.starts_with('b') || op.starts_with("lb"))
            && !matches!(op, "bra" | "lbra" | "bsr" | "lbsr" | "brn" | "lbrn")
            && !op.starts_with("bit")
    };
    for back in insns.iter().filter(|i| conditional(&i.op)) {
        let Some(t) = target(back).filter(|&t| t < back.off) else {
            continue;
        };
        let Some(b) = code.iter().position(|c| c.0 == back.off) else {
            continue;
        };
        let key = (code[b].1, code[b].2);
        let mut s = b;
        while s > 0 && (code[s - 1].1, code[s - 1].2) == key {
            s -= 1;
        }
        let start = code[s].0;
        if t >= start {
            continue;
        }
        let entry = insns.iter().find(|j| {
            matches!(j.op.as_str(), "bra" | "lbra")
                && j.off < t
                && target(j).is_some_and(|x| x >= start && x <= back.off)
        });
        let Some(header) = entry
            .and_then(|j| code.iter().find(|c| c.0 == j.off))
            .map(|c| (c.1, c.2))
        else {
            continue;
        };
        for c in &mut code[s..=b] {
            (c.1, c.2) = header;
        }
    }
}

/// Where a module-relative table lives right now, in physical RAM.
#[derive(Debug, Clone)]
pub struct Placement {
    pub table: usize,
    /// (physical start, length, module offset of the start)
    pub chunks: Vec<(usize, usize, u16)>,
}

#[derive(Debug, Default)]
pub struct Symbols {
    pub tables: Vec<Table>,
    pub source_dirs: Vec<PathBuf>,
    pub placements: Vec<Placement>,
    signature: u64,
}

/// A breakpoint target: CPU address (absolute tables) or physical RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Logical(u16),
    Phys(usize),
}

impl Symbols {
    /// Load a listing, map, or a directory of them. `PATH@MODULE` places an
    /// LWLINK map or LWASM listing as that module; `PATH@abs` forces absolute.
    pub fn load(&mut self, spec: &str) -> Result<Vec<usize>, String> {
        let (path, module) = match spec.rsplit_once('@') {
            Some((p, m)) if !m.contains('/') && !m.is_empty() => (PathBuf::from(p), Some(m)),
            _ => (PathBuf::from(spec), None),
        };
        let mut loaded = Vec::new();
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = fs::read_dir(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .flatten()
                .map(|e| e.path())
                .collect();
            entries.sort();
            for p in &entries {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                let has_lst = p.with_extension("lst").is_file();
                if ext == "lst" || (ext == "map" && !has_lst) {
                    match self.load_file(p, None) {
                        Ok(i) => loaded.push(i),
                        Err(e) if ext == "lst" => return Err(e),
                        Err(_) => {}
                    }
                }
            }
            if loaded.is_empty() {
                return Err(format!("{}: no .lst or .map files", path.display()));
            }
            if !self.source_dirs.contains(&path) {
                self.source_dirs.push(path);
            }
        } else {
            loaded.push(self.load_file(&path, module)?);
        }
        self.signature = 0;
        Ok(loaded)
    }

    fn load_file(&mut self, path: &Path, module: Option<&str>) -> Result<usize, String> {
        let text = text_of(path)?;
        let mut t = if text.contains("Microware OS-9 RMA") {
            parse_rma(path, &text)?
        } else if text.starts_with("Linkage map for") {
            let m = parse_linkmap(&text);
            let mut t = Table::new(path, "c.link");
            t.module = m.module;
            t.symbols = m.symbols;
            t.finish();
            t
        } else if text
            .lines()
            .any(|l| l.starts_with("Symbol: ") || l.starts_with("Section: "))
        {
            parse_lwlink(path, &text)
        } else {
            parse_lwasm(path, &text)?
        };
        match module {
            Some("abs") => t.module = None,
            Some(m) => t.module = Some(m.to_string()),
            None => {}
        }
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        for s in &mut t.sources {
            if s.path.is_none() {
                s.path = find_source(&s.name, &dir, &self.source_dirs);
            }
        }
        // Reloading the same file replaces it.
        if let Some(i) = self.tables.iter().position(|x| x.path == t.path) {
            self.tables[i] = t;
            Ok(i)
        } else {
            self.tables.push(t);
            Ok(self.tables.len() - 1)
        }
    }

    pub fn add_source_dir(&mut self, dir: PathBuf) {
        if !self.source_dirs.contains(&dir) {
            self.source_dirs.push(dir);
        }
        for t in &mut self.tables {
            let base = t.path.parent().map(Path::to_path_buf).unwrap_or_default();
            for s in &mut t.sources {
                if s.path.is_none() {
                    s.path = find_source(&s.name, &base, &self.source_dirs);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.tables.clear();
        self.placements.clear();
        self.signature = 0;
    }

    /// Recompute where modules live. Returns true when placements changed.
    pub fn place(&mut self, m: &Machine, level: Option<Level>) -> bool {
        if !self.tables.iter().any(|t| t.module.is_some()) {
            let changed = !self.placements.is_empty();
            self.placements.clear();
            return changed;
        }
        let Some(level) = level else {
            return false;
        };
        let v = View::new(m, level);
        let mods = v.modules();
        let mut sig: u64 = 0xcbf29ce484222325;
        for md in &mods {
            for b in md
                .name
                .bytes()
                .chain(md.addr.to_be_bytes())
                .chain(md.size.to_be_bytes())
            {
                sig = (sig ^ u64::from(b)).wrapping_mul(0x100000001b3);
            }
            for &b in &md.blocks {
                sig = (sig ^ u64::from(b)).wrapping_mul(0x100000001b3);
            }
        }
        if sig == self.signature {
            return false;
        }
        self.signature = sig;
        self.placements.clear();
        for (ti, t) in self.tables.iter().enumerate() {
            let Some(name) = &t.module else { continue };
            for md in mods.iter().filter(|md| md.name.eq_ignore_ascii_case(name)) {
                let mut chunks = Vec::new();
                let (start, end) = (u32::from(md.addr), u32::from(md.addr) + u32::from(md.size));
                let mut a = start;
                while a < end {
                    let slot_end = ((a >> 13) + 1) << 13;
                    let stop = slot_end.min(end);
                    let phys = match level {
                        Level::One => m.translate(a as u16),
                        Level::Two => md
                            .blocks
                            .get(((a >> 13) - (start >> 13)) as usize)
                            .map(|&b| usize::from(b) * 8192 + (a as usize & 8191)),
                    };
                    if let Some(p) = phys {
                        chunks.push((p, (stop - a) as usize, (a - start) as u16));
                    }
                    a = stop;
                }
                self.placements.push(Placement { table: ti, chunks });
            }
        }
        true
    }

    /// The table and offset describing a CPU address under the current map.
    pub fn locate(&self, m: &Machine, addr: u16) -> Option<(usize, u16)> {
        if let Some(p) = m.translate(addr) {
            for pl in &self.placements {
                for &(start, len, off) in &pl.chunks {
                    if p >= start && p < start + len {
                        return Some((pl.table, off + (p - start) as u16));
                    }
                }
            }
        }
        self.tables
            .iter()
            .position(|t| t.module.is_none() && t.covers(addr))
            .map(|i| (i, addr))
    }

    /// `name` or `name+$off` for an address, if any table covers it.
    pub fn label(&self, m: &Machine, addr: u16) -> Option<String> {
        let (ti, off) = self.locate(m, addr)?;
        let t = &self.tables[ti];
        match t.base_symbol(off) {
            Some((at, name)) if off == at => Some(name.to_string()),
            Some((at, name)) if off - at < 0x1000 => Some(format!("{name}+${:X}", off - at)),
            _ => t.module.as_ref().map(|md| format!("{md}+${off:04X}")),
        }
    }
    /// A symbol defined exactly at this address.
    pub fn exact(&self, m: &Machine, addr: u16) -> Option<&str> {
        let (ti, off) = self.locate(m, addr)?;
        match self.tables[ti].symbol_at(off) {
            Some((at, name)) if at == off => Some(name),
            _ => None,
        }
    }
    pub fn line(&self, m: &Machine, addr: u16) -> Option<(usize, LineRec)> {
        let (ti, off) = self.locate(m, addr)?;
        self.tables[ti].line_at(off).map(|l| (ti, *l))
    }

    fn target(&self, ti: usize, off: u16) -> Vec<Target> {
        if self.tables[ti].module.is_none() {
            return vec![Target::Logical(off)];
        }
        let mut out = Vec::new();
        for pl in self.placements.iter().filter(|p| p.table == ti) {
            for &(start, len, o) in &pl.chunks {
                if off >= o && usize::from(off - o) < len {
                    out.push(Target::Phys(start + usize::from(off - o)));
                }
            }
        }
        out
    }

    /// Breakpoint targets for a symbol name (all tables).
    pub fn symbol_targets(&self, name: &str) -> Vec<Target> {
        let mut out = Vec::new();
        for (ti, t) in self.tables.iter().enumerate() {
            for (off, n) in &t.symbols {
                if n == name || n.trim_start_matches('_') == name {
                    out.extend(self.target(ti, *off));
                }
            }
        }
        out
    }

    /// Targets for a source line: the first line at or after `line` that has
    /// code, in every table that compiled the file. Returns the actual line.
    pub fn line_targets(&self, file: &str, line: u32) -> (Option<u32>, Vec<Target>) {
        let mut best: Option<u32> = None;
        let mut found: Vec<(usize, u16, u32)> = Vec::new();
        for (ti, t) in self.tables.iter().enumerate() {
            for (si, s) in t.sources.iter().enumerate() {
                if !source_matches(s, file) {
                    continue;
                }
                let l = t
                    .lines
                    .iter()
                    .filter(|r| r.src == si && r.line >= line)
                    .map(|r| r.line)
                    .min();
                let Some(l) = l else { continue };
                if l > line + 20 {
                    continue;
                }
                if best.is_none_or(|b| l < b) {
                    best = Some(l);
                    found.clear();
                }
                if best == Some(l) {
                    // The lowest address of the line, its natural entry.
                    if let Some(r) = t
                        .lines
                        .iter()
                        .filter(|r| r.src == si && r.line == l)
                        .min_by_key(|r| r.off)
                    {
                        found.push((ti, r.off, l));
                    }
                }
            }
        }
        let targets = found
            .iter()
            .flat_map(|&(ti, off, _)| self.target(ti, off))
            .collect();
        (best, targets)
    }

    /// The CPU address for a target under the current map.
    pub fn logical(&self, m: &Machine, t: Target) -> Option<u16> {
        match t {
            Target::Logical(a) => Some(a),
            Target::Phys(p) => (0..8u16).find_map(|slot| {
                let base = m.translate(slot << 13)?;
                (p >= base && p < base + 8192).then(|| (slot << 13) | (p - base) as u16)
            }),
        }
    }

    /// Resolve an expression: hex/decimal number or symbol name.
    pub fn resolve(&self, m: &Machine, expr: &str) -> Option<u16> {
        let e = expr.trim();
        if let Some(h) = e.strip_prefix('$').or_else(|| e.strip_prefix("0x")) {
            return u16::from_str_radix(h, 16).ok();
        }
        if let Some(t) = self
            .symbol_targets(e)
            .into_iter()
            .find_map(|t| self.logical(m, t))
        {
            return Some(t);
        }
        u16::from_str_radix(e, 16).ok()
    }
}

fn source_matches(s: &Source, file: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/");
    let file = norm(file);
    if let Some(p) = &s.path {
        let host = norm(&p.to_string_lossy());
        if host == file {
            return true;
        }
        if let (Ok(a), Ok(b)) = (fs::canonicalize(p), fs::canonicalize(&file))
            && a == b
        {
            return true;
        }
    }
    // Otherwise by the recorded name, so a copy of the file also matches.
    let name = norm(&s.name);
    name == file
        || file.ends_with(&format!("/{name}"))
        || file.rsplit('/').next() == Some(name.as_str())
}

/// Find a host file for a recorded source name: by its trailing path in the
/// listing's directory and the configured source directories.
fn find_source(name: &str, dir: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = name.trim().trim_start_matches("./");
    if name.is_empty() {
        return None;
    }
    let base = name.rsplit('/').next().unwrap_or(name);
    let mut candidates: Vec<PathBuf> = std::iter::once(dir.to_path_buf())
        .chain(dirs.iter().cloned())
        .collect();
    // One level of subdirectories, e.g. the `sources/` of a build directory.
    for d in candidates.clone() {
        if let Ok(rd) = fs::read_dir(&d) {
            candidates.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    for d in candidates {
        let direct = d.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        // LWASM truncates long names from the left; match by suffix.
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                let s = p.to_string_lossy().replace('\\', "/");
                if p.is_file()
                    && p.file_name()
                        .is_some_and(|f| f.to_string_lossy().ends_with(base))
                    && s.ends_with(name)
                {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const LWASM: &str = "\
                      (s/source-echo.asm):00019         tylg                set       Prgrm+Objct
0000 87CD0022000D1181 (s/source-echo.asm):00024                             mod       eom,name,tylg,atrv,start,size
     0A001201C2
0000.                 (s/source-echo.asm):00027         u0000               rmb       450
000D 456368EF         (s/source-echo.asm):00030         name                fcs       /Echo/
0011 05               (s/source-echo.asm):00031                             fcb       edition
0012 1F02             (s/source-echo.asm):00033         start               tfr       d,y
0014 8601             (s/source-echo.asm):00034                             lda       #1
0016 103F8C           (s/source-echo.asm):00035                             os9       I$WritLn
0019 2501             (s/source-echo.asm):00036                             bcs       Exit
001B 5F               (s/source-echo.asm):00037                             clrb
001C 103F06           (s/source-echo.asm):00038         Exit                os9       F$Exit
001F F5FF9A           (s/source-echo.asm):00040                             emod
     0022             (s/source-echo.asm):00041         eom                 equ       *
";

    #[test]
    fn lwasm_module_listing() {
        let t = parse_lwasm(Path::new("echo.lst"), LWASM).unwrap();
        assert_eq!(t.module.as_deref(), Some("Echo"));
        let names: Vec<_> = t.symbols.iter().map(|(a, n)| (*a, n.as_str())).collect();
        // `u0000` is a data offset and `eom` an equate: neither labels code.
        assert_eq!(names, [(0x0d, "name"), (0x12, "start"), (0x1c, "Exit")]);
        assert_eq!(
            t.lines[0],
            LineRec {
                off: 0,
                end: 0x0d,
                src: 0,
                line: 24
            }
        );
        let l = t.line_at(0x17).unwrap();
        assert_eq!((l.off, l.line), (0x16, 35));
        assert!(t.sources[0].text[&35].contains("I$WritLn"));
    }

    #[test]
    fn microware_c_listing_map_and_prep() {
        let dir = std::env::temp_dir().join(format!("rs6809-sym-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("t.m"),
            "#7\nt.c\nt_c\n#P\nt_c\n0\n#6\n#include <x.h>\n#7\n/dd/defs/x.h\nx_h\n#6\nint x;\n#7\nt.c\nt_c\n#6\nint f()\n#6\n{\n#6\n    return 1;\n#6\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("t.map"),
            "Linkage map for t  File - t\n\nSection          Code IDat UDat IDpD UDpD File\n\ncstart_a         0010 0001 015f 00   01 /dd/lib/cstart.r\nt_c              0178 0001 01af 01   01 t.r\n     f         code 0178\n",
        )
        .unwrap();
        let lst = "Microware OS-9 RMA - V1.0  126/10/30  12:00   t.a                 Page     1\nt_c - f\n\n\
00001                          psect t_c,0,0,0,0,0 \n\
00002 * *#include <x.h>\n\
00003 * *int x;\n\
00004 * *int f()\n\
00005 * *{\n\
00006 0000                     ttl   f        \n\
00007 0000            f:                      \n\
00008 0000 3440                pshs  u          \n\
00009 * *    return 1;\n\
00010 0002 cc0001              ldd   #1         \n\
00011 0005 35c0                puls  u,pc       \n\
00012 * *}\n\
00013 0007                     endsect            \n";
        fs::write(dir.join("t.lst"), lst).unwrap();
        let mut s = Symbols::default();
        s.load(dir.join("t.lst").to_str().unwrap()).unwrap();
        let t = &s.tables[0];
        assert_eq!(t.module.as_deref(), Some("t"));
        assert_eq!(t.symbols, [(0x178, "f".to_string())]);
        let lines: Vec<_> = t
            .lines
            .iter()
            .map(|l| (l.off, l.end, t.sources[l.src].name.as_str(), l.line))
            .collect();
        // `{` covers the prologue; `return 1;` the rest. `}` has no code.
        assert_eq!(lines, [(0x178, 0x17a, "t.c", 3), (0x17a, 0x17f, "t.c", 4)]);
        assert_eq!(s.line_targets("t.c", 1).0, Some(3));
        assert_eq!(s.line_targets("t.c", 5).0, None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
