//! CP/M-68K 1.3 on the Tiny68K-compatible machine with the rs6809 BIOS.
//! Needs `python3 scripts/fetch-cpm68k.py` (the distribution is not kept in
//! the repository): `cargo test --release --test cpm68k -- --ignored`.
use rs6809::ice::cpm68k::Cpm68k;
use rs6809::ice::json::Json;
use rs6809::ice::{Ice, Mc68000, Options};
use rs6809::m68k::machine::parse_srec;
use rs6809::m68k::tiny68k::{RAM_END, Tiny68k};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const DIST: &str = "artifacts/cpm68k/dist";

type Cpm = Ice<Mc68000<Tiny68k>>;

/// A fresh disk with the distribution and `extra` host files.
fn disk(tag: &str, extra: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rs6809-cpm68k-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("a.img");
    let st = Command::new("python3")
        .args(["-I", "scripts/cpm68k-disk.py", "dist"])
        .arg(&img)
        .args(extra)
        .status()
        .unwrap();
    assert!(st.success(), "cpm68k-disk.py");
    img
}

fn machine(img: &PathBuf) -> Mc68000<Tiny68k> {
    let mut m = Tiny68k::default();
    m.ide.attach(std::fs::read(img).unwrap(), None);
    for f in [format!("{DIST}/DISK8/CPM15000.SR"), "examples/cpm68k/bios.sr".to_string()] {
        let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
        for (a, bytes) in parse_srec(&text).unwrap().0 {
            m.load(a, &bytes).unwrap();
        }
    }
    Mc68000::start(m, 0x15000, RAM_END)
}

fn boot(img: &PathBuf, commands: &[&str]) -> Cpm {
    let actions: VecDeque<(String, String)> = commands.iter().map(|c| ("A>".to_string(), c.to_string())).collect();
    let mut d = Ice::for_target(
        machine(img),
        Options {
            actions,
            echo_console: false,
        },
        Box::new(Cpm68k::default()),
    );
    ok(&mut d, "/api/run");
    d
}

fn ok(d: &mut Cpm, t: &str) -> String {
    let (status, body) = d.command("POST", t, b"");
    assert_eq!(status, 200, "{t}: {body}");
    body
}
fn get(d: &mut Cpm, t: &str) -> Json {
    let (status, body) = d.command("GET", t, b"");
    assert_eq!(status, 200, "{t}: {body}");
    Json::parse(&body).unwrap()
}
fn console(d: &Cpm) -> String {
    String::from_utf8_lossy(&d.console).replace('\r', "")
}

/// "user|sys NAME ARGS → RESULT" of each recorded call.
fn summary(calls: &Json) -> Vec<String> {
    calls
        .get("calls")
        .arr()
        .iter()
        .map(|c| {
            format!(
                "{} {} {} → {}",
                if c.get("system").bool() == Some(true) { "sys" } else { "user" },
                c.get("name").str().unwrap_or(""),
                c.get("args").str().unwrap_or(""),
                c.get("result").str().unwrap_or("")
            )
        })
        .collect()
}

/// Run until the console shows `text` `times` times (or the CPU stops).
fn run_until(d: &mut Cpm, text: &str, times: usize) {
    for _ in 0..4000 {
        d.run_steps(200_000);
        if console(d).matches(text).count() >= times || !d.running {
            if !d.running && !text.is_empty() && text != "never" && console(d).matches(text).count() < times {
                let (_, st) = d.command("GET", "/api/state", b"");
                panic!("stopped before {text:?}:\n{}\nstate {}", console(d), &st[..st.len().min(600)]);
            }
            return;
        }
    }
    let (_, st) = d.command("GET", "/api/state", b"");
    panic!("no {text:?} in:\n{}\nstate {}", console(d), &st[..st.len().min(600)]);
}

#[test]
#[ignore = "requires scripts/fetch-cpm68k.py"]
fn boots_and_runs_the_utilities() {
    let img = disk("utils", &[]);
    let cmds = ["DIR *.SUB", "TYPE C.SUB", "STAT", "PIP X.SUB=C.SUB", "TYPE X.SUB", "DIR X.*"];
    let mut d = boot(&img, &cmds);
    run_until(&mut d, "A>", cmds.len() + 1);
    let out = console(&d);
    assert!(out.contains("rs6809 Tiny68K BIOS for CP/M-68K 1.3"), "{out}");
    assert!(out.contains(": C        SUB"), "DIR: {out}");
    assert_eq!(out.matches("c068 $1.i $1.1 $1.2 $1.3 -f").count(), 2, "TYPE before and after PIP: {out}");
    assert!(out.contains("A: RW, FREE SPACE:"), "STAT: {out}");
    assert!(out.contains("A: X        SUB"), "the copy: {out}");

    // The BDOS calls behind it, with names, arguments and results.
    let calls = summary(&get(&mut d, "/api/os/calls?user=1&limit=100000"));
    let has = |s: &str| calls.iter().any(|c| c.starts_with(s));
    assert!(has("user F_OPEN A:C.SUB (FCB"), "TYPE opens the file: {calls:?}");
    assert!(has("user F_MAKE A:X.$$$"), "PIP makes the copy under a temporary name: {:?}", calls.iter().filter(|c| c.contains("F_MAKE") || c.contains("F_RENAME")).collect::<Vec<_>>());
    assert!(has("user F_RENAME A:X.$$$ → A:X.SUB"), "and renames it");
    assert!(has("user P_LOAD A:PIP.68K TPA $020000–$FE0000"), "the CCP loads PIP");
    assert!(has("user F_READ A:C.SUB"), "{calls:?}");
    // The BIOS calls under them are internal.
    let all = summary(&get(&mut d, "/api/os/calls?limit=100000"));
    assert!(all.iter().any(|c| c.starts_with("sys BIOS_WRITE")), "BIOS calls are internal");
    let state = get(&mut d, "/api/state");
    assert_eq!(state.get("os").get("drive").str(), Some("A"));
    assert_eq!(state.get("os").get("tpa").arr().len(), 2, "learned from S_TPA");
}

#[test]
#[ignore = "requires scripts/fetch-cpm68k.py"]
fn breaks_on_calls_and_returns_by_name() {
    let img = disk("break", &[]);
    let mut d = boot(&img, &["STAT"]);
    ok(&mut d, "/api/os/break?code=F_OPEN");
    run_until(&mut d, "never", 1);
    let st = get(&mut d, "/api/state");
    assert!(st.get("stop").str().unwrap().contains("F_OPEN"), "{st}");
    assert_eq!(st.get("regs").get("d0").int().map(|v| v & 0xffff), Some(15));
    // D1 points at the CCP's FCB for STAT.68K.
    let fcb = st.get("regs").get("d1").int().unwrap() as usize;
    assert_eq!(&d.t.machine.ram[fcb + 1..fcb + 12], b"STAT    68K");

    ok(&mut d, "/api/os/break?op=clear");
    ok(&mut d, "/api/os/break?code=P_LOAD&when=return");
    ok(&mut d, "/api/run");
    run_until(&mut d, "never", 1);
    let st = get(&mut d, "/api/state");
    assert!(st.get("stop").str().unwrap().contains("復帰 P_LOAD"), "{st}");
    assert_eq!(st.get("os").get("program").str(), Some("A:STAT.68K"));
    let procs = get(&mut d, "/api/os/procs").to_string();
    assert!(procs.contains(r#""basepage":"$020000""#), "{procs}");
    let mods = get(&mut d, "/api/os/modules").to_string();
    assert!(mods.contains(r#""name":"STAT""#) && mods.contains(r#""name":"BDOS""#), "{mods}");

    // Errors: opening a file that is not there returns $FF.
    ok(&mut d, "/api/os/break?op=clear&errors=1");
    ok(&mut d, "/api/run");
    run_until(&mut d, "A>", 2);
    let (_, _) = d.command("POST", "/api/console", b"TYPE NOSUCH.TXT\r");
    run_until(&mut d, "never", 1);
    let st = get(&mut d, "/api/state");
    let calls: Vec<String> = get(&mut d, "/api/os/calls?user=1&limit=40")
        .get("calls")
        .arr()
        .iter()
        .map(|c| format!("{} {} → {} {}", c.get("name").str().unwrap_or(""), c.get("args").str().unwrap_or(""), c.get("result").str().unwrap_or(""), c.get("error")))
        .collect();
    let calls = calls.join("\n");
    assert!(st.get("stop").str().unwrap().contains("F_OPEN"), "{st}\n{calls}");
    assert!(calls.contains("F_OPEN A:NOSUCH.TXT"), "{calls}");
}

#[test]
#[ignore = "requires scripts/fetch-cpm68k.py"]
fn alcyon_c_compiles_and_runs_hello() {
    let img = disk("cc", &["examples/cpm68k/hello.c"]);
    let cmds = ["AS68 -I AS68INIT", "SUBMIT C HELLO", "SUBMIT CLINK HELLO", "HELLO"];
    let mut d = boot(&img, &cmds);
    run_until(&mut d, "1+2+...+10 = 55", 1);
    let out = console(&d);
    assert!(out.contains("Hello from Alcyon C on CP/M-68K"), "{out}");
}

// ---------- DAP ----------

struct Client {
    w: TcpStream,
    r: BufReader<TcpStream>,
    seq: i64,
    events: Vec<Json>,
}
impl Client {
    fn read(&mut self) -> Json {
        let mut len = 0;
        loop {
            let mut line = String::new();
            self.r.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        self.r.read_exact(&mut body).unwrap();
        Json::parse(std::str::from_utf8(&body).unwrap()).unwrap()
    }
    fn request(&mut self, command: &str, args: &str) -> Json {
        self.seq += 1;
        let msg = format!(r#"{{"seq":{},"type":"request","command":"{command}","arguments":{args}}}"#, self.seq);
        write!(self.w, "Content-Length: {}\r\n\r\n{msg}", msg.len()).unwrap();
        loop {
            let m = self.read();
            if m.get("type").str() == Some("response") && m.get("request_seq").int() == Some(self.seq) {
                assert_eq!(m.get("success").bool(), Some(true), "{command}: {m}");
                return m.get("body").clone();
            }
            self.events.push(m);
        }
    }
    fn event(&mut self, name: &str) -> Json {
        if let Some(i) = self.events.iter().position(|e| e.get("event").str() == Some(name)) {
            return self.events.remove(i).get("body").clone();
        }
        loop {
            let m = self.read();
            if m.get("event").str() == Some(name) {
                return m.get("body").clone();
            }
            self.events.push(m);
        }
    }
}

#[test]
#[ignore = "requires scripts/fetch-cpm68k.py"]
fn dap_breaks_on_bdos_calls_with_bios_source() {
    let img = disk("dap", &[]);
    let d = Ice::for_target(
        machine(&img),
        Options {
            actions: VecDeque::from([("A>".to_string(), "DIR".to_string())]),
            echo_console: false,
        },
        Box::new(Cpm68k::default()),
    );
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let dap = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = dap.local_addr().unwrap().port();
    std::thread::spawn(move || d.serve(http, Some(dap), false));
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut c = Client {
        w: s.try_clone().unwrap(),
        r: BufReader::new(s),
        seq: 0,
        events: Vec::new(),
    };
    let caps = c.request("initialize", r#"{"adapterID":"rs6809","linesStartAt1":true}"#);
    assert_eq!(caps.get("supportsExceptionFilterOptions").bool(), Some(true));
    c.event("initialized");
    c.request("attach", r#"{"symbols":["examples/cpm68k/bios.lst","artifacts/cpm68k/dist/DISK8/CPM15000.MAP"]}"#);
    c.request(
        "setExceptionBreakpoints",
        r#"{"filters":[],"filterOptions":[{"filterId":"oscall","condition":"F_SFIRST"}]}"#,
    );
    c.request("configurationDone", "{}");
    assert_eq!(c.event("stopped").get("reason").str(), Some("pause"));
    c.request("continue", r#"{"threadId":1}"#);
    let stop = c.event("stopped");
    assert!(stop.get("text").str().unwrap_or("").contains("F_SFIRST"), "{stop}");
    let threads = c.request("threads", "{}").to_string();
    assert!(threads.contains("68000 (CP/M-68K"), "{threads}");

    // Then stop in the BIOS console output, with its source line.
    let src = std::fs::canonicalize("examples/cpm68k/bios.s").unwrap();
    let path = Json::from(src.to_string_lossy().into_owned());
    let text = std::fs::read_to_string(&src).unwrap();
    let line = text.lines().position(|l| l.starts_with("conout:")).unwrap() + 1;
    let bps = c.request(
        "setBreakpoints",
        &format!(r#"{{"source":{{"path":{path}}},"breakpoints":[{{"line":{line}}}]}}"#),
    );
    assert_eq!(bps.get("breakpoints").arr()[0].get("verified").bool(), Some(true), "{bps}");
    c.request("setExceptionBreakpoints", r#"{"filters":[]}"#);
    c.request("continue", r#"{"threadId":1}"#);
    assert_eq!(c.event("stopped").get("reason").str(), Some("breakpoint"));
    let frames = c.request("stackTrace", r#"{"threadId":1}"#);
    let top = &frames.get("stackFrames").arr()[0];
    assert_eq!(top.get("name").str(), Some("conout"));
    assert_eq!(top.get("line").int(), Some(line as i64));
    c.request("next", r#"{"threadId":1}"#);
    c.event("stopped");
    let frames = c.request("stackTrace", r#"{"threadId":1}"#);
    assert_eq!(frames.get("stackFrames").arr()[0].get("line").int(), Some(line as i64 + 1));
    c.request("disconnect", "{}");
}
