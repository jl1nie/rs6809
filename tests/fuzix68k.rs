//! FUZIX 0.5 on the Tiny68K-compatible machine. Needs the toolchain and the
//! build (`sh scripts/build-m68k-elf.sh`, `sh scripts/build-fuzix-tiny68k.sh`):
//! `cargo test --release --test fuzix68k -- --ignored`.
use rs6809::ice::fuzix::Fuzix;
use rs6809::ice::json::Json;
use rs6809::ice::{Ice, Mc68000, Options};
use rs6809::m68k::tiny68k::{RAM_END, Tiny68k};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const DIR: &str = "artifacts/fuzix68k";

type Fx = Ice<Mc68000<Tiny68k>>;

/// Boot from the disk's boot sector, as the monitor would.
fn machine() -> Mc68000<Tiny68k> {
    let disk = std::fs::read(format!("{DIR}/disk.img")).unwrap();
    let mut m = Tiny68k::default();
    let code: Vec<u8> = disk[512..1024].chunks(2).flat_map(|w| [w[1], w[0]]).collect();
    m.load(0x15000, &code).unwrap();
    m.ide.attach(disk, None);
    Mc68000::start(m, 0x15000, RAM_END)
}

fn ice(commands: &[&str]) -> Fx {
    let mut actions: VecDeque<(String, String)> = VecDeque::from([
        ("bootdev:".to_string(), "hda1".to_string()),
        ("Enter new date:".into(), String::new()),
        ("Enter new time:".into(), String::new()),
        ("login:".into(), "root".into()),
    ]);
    actions.extend(commands.iter().map(|c| ("# ".to_string(), c.to_string())));
    let mut d = Ice::for_target(
        machine(),
        Options {
            actions,
            echo_console: false,
        },
        Box::new(Fuzix::default()),
    );
    d.load_symbols(&format!("{DIR}/fuzix.sym")).unwrap();
    d
}

fn ok(d: &mut Fx, t: &str) -> String {
    let (status, body) = d.command("POST", t, b"");
    assert_eq!(status, 200, "{t}: {body}");
    body
}
fn get(d: &mut Fx, t: &str) -> Json {
    let (status, body) = d.command("GET", t, b"");
    assert_eq!(status, 200, "{t}: {body}");
    Json::parse(&body).unwrap()
}
fn console(d: &Fx) -> String {
    String::from_utf8_lossy(&d.console).replace('\r', "")
}
/// Run until `text` shows up `times` times, or the CPU stops.
fn run_until(d: &mut Fx, text: &str, times: usize) {
    for _ in 0..20000 {
        d.run_steps(200_000);
        if console(d).matches(text).count() >= times || !d.running {
            return;
        }
    }
    panic!("no {text:?} in:\n{}", console(d));
}
/// "NAME ARGS → RESULT" of user calls, by process: "pid NAME ...".
fn calls(d: &mut Fx) -> Vec<String> {
    get(d, "/api/os/calls?user=1&limit=100000")
        .get("calls")
        .arr()
        .iter()
        .rev()
        .map(|c| {
            format!(
                "{} {} {} → {}",
                c.get("pid").int().unwrap_or(0),
                c.get("name").str().unwrap_or(""),
                c.get("args").str().unwrap_or(""),
                c.get("result").str().unwrap_or("")
            )
        })
        .collect()
}

#[test]
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn boots_to_the_shell_and_traces_fork_exec_wait() {
    let mut d = ice(&["ls /", "ps", "echo all-done"]);
    ok(&mut d, "/api/run");
    run_until(&mut d, "all-done\n", 1);
    let out = console(&d);
    assert!(out.contains("FUZIX version 0.5"), "{out}");
    assert!(out.contains("Motorola 68000 processor detected."));
    assert!(out.contains("\nbin\ndev\netc\n"), "ls /: {out}");
    assert!(out.contains("PID    TTY     TIME CMD"), "ps: {out}");

    let c = calls(&mut d);
    let find = |s: &str| c.iter().find(|x| x.contains(s)).cloned().unwrap_or_else(|| panic!("no {s} in {c:#?}"));
    // The shell forks, the child runs ps, the parent waits for it.
    let exec = find("execve \"/bin/ps\" [ps]");
    let child: i64 = exec.split(' ').next().unwrap().parse().unwrap();
    let fork = find(&format!("_fork フラグ$0 → 子のpid {child}"));
    let shell: i64 = fork.split(' ').next().unwrap().parse().unwrap();
    find(&format!("{child} open \"/dev/proc\""));
    find(&format!("{child} _exit 終了コード0"));
    find(&format!("{shell} waitpid pid -1"));
    assert!(c.iter().any(|x| x.starts_with(&format!("{shell} waitpid")) && x.contains(&format!("→ pid {child} "))), "{c:#?}");
    // File names learned from open, on later reads and writes.
    assert!(c.iter().any(|x| x.contains("write fd 9（/dev/tty1） 2バイト \"# \"")), "{c:#?}");

    // The timeline switched to the child and back.
    // Switches are [cycle, from, to].
    let sw = get(&mut d, "/api/os/calls?limit=100000").get("switches").clone();
    let to = |p: i64| sw.arr().iter().any(|s| s.arr().get(2).and_then(Json::int) == Some(p));
    assert!(to(child) && to(shell), "switches to {child} and back to {shell}: {sw}");
}

#[test]
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn breaks_on_calls_returns_and_errors() {
    let mut d = ice(&["cat /etc/issue", "cat /nosuch"]);
    ok(&mut d, "/api/run");
    run_until(&mut d, "login:", 1);
    ok(&mut d, "/api/os/break?code=execve");
    // login also execs the shell: continue to the shell's child running cat.
    let mut st = Json::Null;
    for _ in 0..10 {
        run_until(&mut d, "never", 1);
        st = get(&mut d, "/api/state");
        let path = st.get("regs").get("d1").int().unwrap() as usize;
        if d.t.machine.ram[path..path + 9] == *b"/bin/cat\0" {
            break;
        }
        ok(&mut d, "/api/run");
    }
    assert!(st.get("stop").str().unwrap().contains("execve"), "{st}");
    assert_eq!(st.get("regs").get("d0").int().map(|v| v & 0xffff), Some(23));
    // The child of the shell is in the process table, about to exec.
    let procs = get(&mut d, "/api/os/procs");
    let pid = st.get("os").get("pid").int().unwrap();
    let me = procs.get("procs").arr().iter().find(|p| p.get("id").int() == Some(pid)).cloned().unwrap();
    assert_eq!(me.get("name").str(), Some("-sh"), "the forked shell: {procs}");
    assert_eq!(me.get("queue").str(), Some("running"));
    let parent = me.get("parent").int().unwrap();
    assert!(procs.get("procs").arr().iter().any(|p| p.get("id").int() == Some(parent) && p.get("queue").str() == Some("sleep")));

    ok(&mut d, "/api/os/break?op=clear");
    ok(&mut d, "/api/os/break?code=waitpid&when=return");
    ok(&mut d, "/api/run");
    run_until(&mut d, "never", 1);
    let st = get(&mut d, "/api/state");
    assert!(st.get("stop").str().unwrap().contains("waitpid"), "{st}");
    assert_eq!(st.get("regs").get("d0").int(), Some(pid), "waitpid returns the child");
    assert!(console(&d).contains("Fuzix"), "/etc/issue");

    // cat /nosuch: open returns -1 with ENOENT.
    ok(&mut d, "/api/os/break?op=clear&errors=1");
    ok(&mut d, "/api/run");
    run_until(&mut d, "never", 1);
    let st = get(&mut d, "/api/state");
    let stop = st.get("stop").str().unwrap().to_string();
    let c = calls(&mut d);
    assert!(c.iter().any(|x| x.contains("open \"/nosuch\"") && x.ends_with("-1 ENOENT")), "{stop}\n{c:#?}");
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
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn dap_breaks_on_fork_and_wait() {
    let d = ice(&["ps"]);
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let dap = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = dap.local_addr().unwrap().port();
    std::thread::spawn(move || d.serve(http, Some(dap), false));
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let mut c = Client {
        w: s.try_clone().unwrap(),
        r: BufReader::new(s),
        seq: 0,
        events: Vec::new(),
    };
    c.request("initialize", r#"{"adapterID":"rs6809","linesStartAt1":true}"#);
    c.event("initialized");
    c.request("attach", "{}");
    c.request(
        "setExceptionBreakpoints",
        r#"{"filters":[],"filterOptions":[{"filterId":"oscall","condition":"_fork"},{"filterId":"osreturn","condition":"waitpid"}]}"#,
    );
    c.request("configurationDone", "{}");
    assert_eq!(c.event("stopped").get("reason").str(), Some("pause"));
    // init forks while booting; continue to the shell's fork.
    let mut threads = String::new();
    for _ in 0..100 {
        c.request("continue", r#"{"threadId":1}"#);
        let stop = c.event("stopped");
        assert!(stop.get("text").str().unwrap_or("").contains("_fork") || stop.get("text").str().unwrap_or("").contains("waitpid"), "{stop}");
        threads = c.request("threads", "{}").to_string();
        if threads.contains("-sh)") && stop.get("text").str().unwrap_or("").contains("_fork") {
            break;
        }
    }
    assert!(threads.contains("68000 (FUZIX プロセス9 -sh)"), "{threads}");
    c.request("continue", r#"{"threadId":1}"#);
    let stop = c.event("stopped");
    assert!(stop.get("text").str().unwrap_or("").contains("waitpid"), "{stop}");
    let regs = c.request("variables", r#"{"variablesReference":1}"#).to_string();
    assert!(regs.contains(r#""name":"D0""#), "{regs}");
    c.request("disconnect", "{}");
}

#[test]
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn monitor_offsets_match_the_kernel_headers() {
    use rs6809::ice::fuzix::off;
    let text = std::fs::read_to_string(format!("{DIR}/offsets.txt")).unwrap();
    let want = [
        ("PTAB_SIZE", off::PTAB_SIZE),
        ("P_STATUS", off::P_STATUS),
        ("P_TTY", off::P_TTY),
        ("P_PID", off::P_PID),
        ("P_UID", off::P_UID),
        ("P_PPTR", off::P_PPTR),
        ("P_EXITVAL", off::P_EXITVAL),
        ("P_WAIT", off::P_WAIT),
        ("P_UDATA", off::P_UDATA),
        ("P_PRIORITY", off::P_PRIORITY),
        ("P_NAME", off::P_NAME),
        ("P_TIME", off::P_TIME),
        ("P_PGRP", off::P_PGRP),
        ("P_NICE", off::P_NICE),
        ("P_SIZE", off::P_SIZE),
        ("U_PTAB", off::U_PTAB),
        ("U_INSYS", off::U_INSYS),
        ("U_CALLNO", off::U_CALLNO),
        ("U_NAME", off::U_NAME),
        ("U_CODEBASE", off::U_CODEBASE),
        ("U_TOP", off::U_TOP),
    ];
    for (name, v) in want {
        let got = text
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(got.trim().parse::<u32>().unwrap(), v, "{name}");
    }
}

#[test]
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn breaks_inside_the_kernel_by_symbol() {
    let mut d = ice(&["ps"]);
    ok(&mut d, "/api/run");
    // Before the shell runs (a command is typed and forked within one
    // run_until slice once the prompt shows).
    run_until(&mut d, "login:", 1);
    ok(&mut d, "/api/bp?sym=dofork");
    run_until(&mut d, "never", 1);
    let st = get(&mut d, "/api/state");
    assert!(st.get("stop").str().unwrap().contains("ブレークポイント"), "{st}");
    let dis = get(&mut d, "/api/disasm?n=1");
    assert_eq!(dis.get("lines").arr()[0].get("label").str(), Some("dofork"), "{dis}");
    assert_eq!(st.get("os").get("pid").int(), Some(9), "the shell forks for ps");
}

#[test]
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn saved_state_resumes_at_the_shell() {
    let mut d = ice(&[]);
    ok(&mut d, "/api/run");
    run_until(&mut d, "# ", 1);
    for _ in 0..20 {
        d.run_steps(200_000);
    }
    ok(&mut d, "/api/pause");
    let path = std::env::temp_dir().join(format!("rs6809-fuzix-{}.state", std::process::id()));
    let p = path.to_string_lossy().into_owned();
    let saved = get_post(&mut d, &format!("/api/machine/save?path={p}"));
    // The disk is in memory here, so it is saved too.
    assert!(saved.get("bytes").int().unwrap() > 40_000_000, "{saved}");

    // A fresh ICE continues from the shell prompt: no boot, no login.
    let mut e = Ice::for_target(
        machine(),
        Options {
            actions: VecDeque::new(),
            echo_console: false,
        },
        Box::new(Fuzix::default()),
    );
    e.load_symbols(&format!("{DIR}/fuzix.sym")).unwrap();
    ok(&mut e, &format!("/api/machine/load?path={p}"));
    ok(&mut e, "/api/run");
    e.command("POST", "/api/console", b"echo resumed\r");
    run_until(&mut e, "resumed\n", 2);
    assert!(!console(&e).contains("FUZIX version"), "{}", console(&e));
    let procs = get(&mut e, "/api/os/procs").to_string();
    assert!(procs.contains("-sh"), "{procs}");

    // Reset goes back to the boot sector.
    ok(&mut e, "/api/reset");
    ok(&mut e, "/api/run");
    run_until(&mut e, "bootdev:", 1);
    std::fs::remove_file(&path).ok();
}

fn get_post(d: &mut Fx, t: &str) -> Json {
    Json::parse(&ok(d, t)).unwrap()
}

#[test]
#[ignore = "requires scripts/build-fuzix-tiny68k.sh"]
fn second_terminal_offers_a_login() {
    // getty runs on /dev/tty2 (DUART channel B; the build enables it).
    let mut d = ice(&[]);
    ok(&mut d, "/api/run");
    run_until(&mut d, "# ", 1);
    let mut seen = Vec::new();
    for _ in 0..200 {
        d.run_steps(200_000);
        seen.append(&mut d.t.machine.duart.b.tx);
        if seen.windows(6).any(|w| w == b"login:") {
            break;
        }
    }
    assert!(seen.windows(6).any(|w| w == b"login:"), "{}", String::from_utf8_lossy(&seen));
    // Log in there: a second shell appears in the process table.
    d.t.machine.duart.b.rx.extend(b"root\r");
    for _ in 0..200 {
        d.run_steps(200_000);
        seen.append(&mut d.t.machine.duart.b.tx);
        if seen.windows(2).any(|w| w == b"# ") {
            break;
        }
    }
    let procs = get(&mut d, "/api/os/procs");
    let shells = procs.get("procs").arr().iter().filter(|p| p.get("name").str() == Some("-sh")).count();
    assert_eq!(shells, 2, "{procs}");
}
