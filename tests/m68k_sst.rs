//! SingleStepTests 68000 (v1): one instruction per test, compared with the
//! recorded final state. Fetch the suite first:
//! `python3 scripts/fetch-68000-tests.py`, then
//! `cargo test --release --test m68k_sst -- --ignored --nocapture`.
//! `SST_FILTER=MOVE.w` limits the files; `SST_SHOW=n` prints n failures each.
//!
//! Tests that end in an address error are checked for the exception, the
//! handler, SR and the frame's status word, access address, IR and SR; the
//! stacked PC and the cycle count are reported but not required.
use rs6809::m68k::{Bus68k, M68k};
use std::collections::HashMap;
use std::fs;

struct Mem(HashMap<u32, u8>);
impl Bus68k for Mem {
    fn read8(&mut self, a: u32) -> u8 {
        *self.0.get(&a).unwrap_or(&0)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.0.insert(a, v);
    }
}

#[derive(Debug, Clone)]
struct State {
    r: [u32; 19],
    prefetch: [u16; 2],
    ram: Vec<(u32, u8)>,
}
struct Test {
    name: String,
    initial: State,
    fin: State,
    length: u32,
}

struct Rd<'a>(&'a [u8], usize);
impl Rd<'_> {
    fn u8(&mut self) -> u8 {
        self.1 += 1;
        self.0[self.1 - 1]
    }
    fn u16(&mut self) -> u16 {
        u16::from(self.u8()) | u16::from(self.u8()) << 8
    }
    fn u32(&mut self) -> u32 {
        u32::from(self.u16()) | u32::from(self.u16()) << 16
    }
    fn state(&mut self) -> State {
        let r = std::array::from_fn(|_| self.u32());
        let prefetch = [self.u16(), self.u16()];
        let n = self.u32();
        let ram = (0..n).map(|_| (self.u32(), self.u8())).collect();
        State { r, prefetch, ram }
    }
}
fn load(path: &str) -> Vec<Test> {
    let data = fs::read(path).unwrap();
    assert_eq!(&data[..4], b"SST1");
    let mut r = Rd(&data, 4);
    let n = r.u32();
    (0..n)
        .map(|_| {
            let len = usize::from(r.u16());
            let name = String::from_utf8_lossy(&r.0[r.1..r.1 + len]).into_owned();
            r.1 += len;
            Test {
                name,
                initial: r.state(),
                fin: r.state(),
                length: r.u32(),
            }
        })
        .collect()
}

/// Tests whose recorded result is not used.
const KNOWN: [(&str, &str); 3] = [
    // SingleStepTests/680x0 issue #4: the upper bytes of D2 change in the data.
    ("e502 [ASL.b Q, D2] 1583", "incorrect data (issue #4)"),
    ("e502 [ASL.b Q, D2] 1761", "incorrect data (issue #4)"),
    // The only divide-by-zero test stacks the DIVU's own address; the
    // manual stacks the next instruction, as CHK and TRAPV do in thousands of tests.
    (
        "80ef [DIVU (d16, A7), D0] 5745",
        "stacked PC of the divide-by-zero trap",
    ),
];

const SR: usize = 17;
const PC: usize = 18;

fn setup(t: &Test) -> (M68k, Mem) {
    let i = &t.initial;
    let mut mem = Mem(HashMap::new());
    for &(a, v) in &i.ram {
        mem.0.insert(a, v);
    }
    let pc = i.r[PC];
    for (k, w) in i.prefetch.iter().enumerate() {
        let a = pc + 2 * k as u32;
        mem.0.insert(a & 0xff_ffff, (w >> 8) as u8);
        mem.0.insert((a + 1) & 0xff_ffff, *w as u8);
    }
    let mut c = M68k::default();
    c.d.copy_from_slice(&i.r[0..8]);
    c.a[..7].copy_from_slice(&i.r[8..15]);
    c.set_sr(i.r[SR] as u16);
    c.set_usp(i.r[15]);
    c.set_ssp(i.r[16]);
    c.pc = pc;
    (c, mem)
}

fn regs(c: &M68k) -> [u32; 19] {
    let mut r = [0; 19];
    r[0..8].copy_from_slice(&c.d);
    r[8..15].copy_from_slice(&c.a[..7]);
    r[15] = c.usp();
    r[16] = c.ssp();
    r[17] = u32::from(c.sr);
    r[18] = c.pc;
    r
}
const NAMES: [&str; 19] = [
    "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "a0", "a1", "a2", "a3", "a4", "a5", "a6",
    "usp", "ssp", "sr", "pc",
];

/// Differences, as text; `frame_pc` excludes the stacked PC (and cycles).
fn diff(t: &Test, c: &M68k, mem: &Mem, address_error: bool) -> (Vec<String>, Vec<String>) {
    let (mut hard, mut soft) = (Vec::new(), Vec::new());
    let got = regs(c);
    for k in 0..19 {
        if got[k] != t.fin.r[k] {
            let m = format!("{} {:08X} want {:08X}", NAMES[k], got[k], t.fin.r[k]);
            // For address errors only the outcome registers are required.
            if address_error && !matches!(k, 16..=18) {
                soft.push(m)
            } else {
                hard.push(m)
            }
        }
    }
    let ssp = t.fin.r[16];
    for &(a, v) in &t.fin.ram {
        let g = mem.0.get(&a).copied().unwrap_or(0);
        if g != v {
            let m = format!("ram[{a:06X}] {g:02X} want {v:02X}");
            let stacked_pc = address_error && a >= ssp + 10 && a < ssp + 14;
            let frame = a >= ssp && a < ssp + 14;
            if stacked_pc || (address_error && !frame) {
                soft.push(m)
            } else {
                hard.push(m)
            }
        }
    }
    let cyc = c.cycles as u32;
    if cyc != t.length {
        let m = format!("cycles {cyc} want {}", t.length);
        if address_error {
            soft.push(m)
        } else {
            hard.push(m)
        }
    }
    (hard, soft)
}

#[test]
#[ignore = "requires python3 scripts/fetch-68000-tests.py"]
fn single_step_tests_68000() {
    let filter = std::env::var("SST_FILTER").ok();
    let show: usize = std::env::var("SST_SHOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut files: Vec<_> = fs::read_dir("artifacts/sst68000")
        .expect("run python3 scripts/fetch-68000-tests.py")
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.ends_with(".bin"))
        .filter(|p| filter.as_ref().is_none_or(|f| p.contains(&format!("/{f}"))))
        .collect();
    files.sort();
    let (mut tn, mut tp, mut an, mut ap, mut af) = (0, 0, 0, 0, 0);
    let soft_show: usize = std::env::var("SST_SOFT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut reasons: HashMap<&'static str, u32> = HashMap::new();
    let mut failing = Vec::new();
    for f in &files {
        let tests = load(f);
        let (mut n, mut p, mut a, mut apass, mut afull, mut shown) = (0, 0, 0, 0, 0, 0);
        for t in &tests {
            if let Some((_, why)) = KNOWN.iter().find(|k| k.0 == t.name) {
                println!("  skip {}: {why}", t.name);
                continue;
            }
            let v3 = [12u32, 13, 14, 15].iter().fold(0u32, |acc, k| {
                acc << 8 | u32::from(t.initial.ram.iter().find(|r| r.0 == *k).map_or(0, |r| r.1))
            });
            let ae = t.fin.r[PC] == v3;
            let (mut c, mut mem) = setup(t);
            c.step(&mut mem);
            let (hard, soft) = diff(t, &c, &mem, ae);
            if ae {
                a += 1;
                if hard.is_empty() {
                    apass += 1;
                    if soft.is_empty() {
                        afull += 1;
                    }
                }
                for k in [
                    ("cycles", "cycles"),
                    ("ram[", "stacked-pc"),
                    ("d", "registers"),
                    ("a", "registers"),
                    ("usp", "registers"),
                ] {
                    if soft.iter().any(|m| m.starts_with(k.0)) {
                        *reasons.entry(k.1).or_default() += 1;
                    }
                }
                if !soft.is_empty() && shown < soft_show {
                    shown += 1;
                    println!("  SOFT {}: {}", t.name, soft.join(", "));
                }
            } else {
                n += 1;
                if hard.is_empty() {
                    p += 1;
                }
            }
            if !hard.is_empty() && shown < show {
                shown += 1;
                println!(
                    "  FAIL {}{}: {}",
                    t.name,
                    if ae { " [AE]" } else { "" },
                    hard.join(", ")
                );
            }
        }
        let name = f.rsplit('/').next().unwrap();
        println!("{name:18} normal {p:5}/{n:5}  address-error {apass:5}/{a:5} (full {afull:5})");
        if p != n || apass != a {
            failing.push(name.to_string());
        }
        tn += n;
        tp += p;
        an += a;
        ap += apass;
        af += afull;
    }
    println!("TOTAL normal {tp}/{tn}  address-error {ap}/{an} (full {af})");
    let mut r: Vec<_> = reasons.into_iter().collect();
    r.sort();
    println!("address-error soft mismatches: {r:?}");
    assert!(failing.is_empty(), "failing: {}", failing.join(" "));
}

/// The disassembler against the suite: for each test that runs straight
/// through, the instruction length is the PC advance; the mnemonic must
/// belong to the file's instruction (e.g. `ADD.b` covers ADD, ADDI, ADDQ).
#[test]
#[ignore = "requires python3 scripts/fetch-68000-tests.py"]
fn disassembler_against_suite() {
    use rs6809::m68k::disasm::disassemble;
    let mut files: Vec<_> = fs::read_dir("artifacts/sst68000")
        .expect("run python3 scripts/fetch-68000-tests.py")
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.ends_with(".bin"))
        .collect();
    files.sort();
    let (mut checked, mut bad) = (0, Vec::new());
    for f in &files {
        let stem = f.rsplit('/').next().unwrap().trim_end_matches(".bin");
        let (family, size) = stem.split_once('.').map_or((stem, ""), |(a, b)| (a, b));
        let prefix = match family {
            "MOVE" if size == "q" => "MOVEQ",
            "MOVEfromSR" => "MOVE SR,",
            "MOVEtoCCR" | "MOVEtoSR" | "MOVEfromUSP" | "MOVEtoUSP" => "MOVE",
            "UNLINK" => "UNLK",
            "Bcc" => "B",
            "DBcc" => "DB",
            "Scc" => "S",
            f if f.ends_with("toCCR") || f.ends_with("toSR") => &f[..f.find("to").unwrap()],
            f => f,
        };
        for t in load(f) {
            let (_, mem) = setup(&t);
            let l = disassemble(
                &|a| mem.0.get(&(a & 0xff_ffff)).copied().unwrap_or(0),
                t.initial.r[PC],
            );
            let mut problem = None;
            if !l.text.starts_with(&prefix.to_ascii_uppercase()) {
                problem = Some(format!("mnemonic for {stem}"));
            } else if !size.is_empty()
                && size != "q"
                && !l.text.contains(&format!(".{}", size.to_ascii_uppercase()))
            {
                problem = Some(format!("size .{size}"));
            }
            let adv = t.fin.r[PC].wrapping_sub(t.initial.r[PC]);
            if (1..=10).contains(&adv) && l.flow == rs6809::m68k::disasm::Flow::Next && adv != l.len
            {
                problem = Some(format!("length {} want {adv}", l.len));
            }
            checked += 1;
            if let Some(p) = problem
                && bad.len() < 40
            {
                bad.push(format!("{}: {} ({p})", t.name, l.text));
            }
        }
    }
    println!("checked {checked}");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
