//! The OS layer with a made-up OS: the HTTP API and DAP must work through
//! `OsMonitor` alone, without anything from NitrOS-9.
use rs6809::ice::json::{Json, obj};
use rs6809::ice::osmon::{Call, Image, OsMonitor, PathNames, Process, Regs, Service, Svc};
use rs6809::ice::{Ice, Options};
use rs6809::{Cpu, Machine};
use std::collections::VecDeque;

/// ToyOS: `SWI` + code byte. Code 1 prints A, code 2 exits. The running
/// process handle is at $0010; its low byte is the process ID.
struct ToyOs;

impl OsMonitor for ToyOs {
    fn detect(&mut self, m: &Machine) -> bool {
        m.peek(0x10) != 0
    }
    fn info(&self) -> Json {
        obj([
            ("os", "toy".into()),
            ("name", "ToyOS".into()),
            ("title", "Toy OS 1.0".into()),
            ("current", "実行中".into()),
            (
                "queues",
                Json::Arr(vec![obj([
                    ("key", "ready".into()),
                    ("label", "準備完了".into()),
                ])]),
            ),
            ("errorLabel", "Toyエラー".into()),
            ("switchLabel", "Toy切り替え".into()),
        ])
    }
    fn summary(&self, _: &Machine) -> Vec<(&'static str, Json)> {
        vec![("ready", Json::Arr(Vec::new()))]
    }
    fn svc_at(&self, read: &dyn Fn(u16) -> u8, pc: u16) -> Option<Svc> {
        (read(pc) == 0x3f).then(|| {
            let code = read(pc.wrapping_add(1));
            Svc {
                code,
                len: 2,
                no_return: code == 2,
            }
        })
    }
    fn svc_prefix(&self) -> &'static [u8] {
        &[0x3f]
    }
    fn svc_mnemonic(&self) -> &'static str {
        "TOY"
    }
    fn service_name(&self, code: u8) -> String {
        match code {
            1 => "T_PUT".into(),
            2 => "T_EXIT".into(),
            c => format!("T_{c}"),
        }
    }
    fn service_code(&self, name: &str) -> Option<u8> {
        match name {
            "T_PUT" => Some(1),
            "T_EXIT" => Some(2),
            _ => None,
        }
    }
    fn explain(&self, code: u8) -> &'static str {
        if code == 1 {
            "Aの文字を出力する。"
        } else {
            ""
        }
    }
    fn error_name(&self, code: u8) -> String {
        format!("TE{code}")
    }
    fn services(&self) -> Vec<Service> {
        [1u8, 2]
            .iter()
            .map(|&c| Service {
                code: c,
                name: self.service_name(c),
                desc: String::new(),
                explain: self.explain(c),
            })
            .collect()
    }
    fn image_calls(&self) -> &'static [u8] {
        &[]
    }
    fn current(&self, m: &Machine) -> u16 {
        m.peek16(0x10)
    }
    fn current_slot(&self) -> Option<usize> {
        Some(0x10)
    }
    fn pid_of(&self, _: &Machine, key: u16) -> u8 {
        key as u8
    }
    fn in_system(&self, _: &Machine, _: u16) -> bool {
        false
    }
    fn describe_entry(&self, _: &Machine, _: &PathNames, _: u8, code: u8, r: &Regs) -> String {
        if code == 1 {
            format!("文字 '{}'", char::from(r.a))
        } else {
            String::new()
        }
    }
    fn describe_exit(&self, _: &Machine, _: &Call, _: &Regs) -> String {
        "OK".into()
    }
    fn error_of(&self, _: &Regs) -> Option<u8> {
        None
    }
    fn learn(&self, _: &Call, _: &Regs, _: u8, _: &mut PathNames) {}
    fn processes(&self, m: &Machine) -> Vec<Process> {
        vec![Process {
            id: 1,
            parent: 0,
            key: self.current(m),
            name: "toy".into(),
            queue: "running",
            fields: vec![("prio", 5.into())],
        }]
    }
    fn images(&self, _: &Machine) -> Vec<Image> {
        vec![Image {
            name: "toy".into(),
            fields: vec![("addr", 0x1000.into())],
            chunks: vec![(0x1000, 0x100, 0)],
        }]
    }
    fn memory_map(&self, _: &Machine) -> Json {
        obj([
            ("unit", 256.into()),
            ("map", Json::Arr(vec![1.into(), 0.into()])),
        ])
    }
    fn read_space(&self, m: &Machine, space: &str, addr: u16) -> Option<u8> {
        (space == "sys").then(|| m.peek(addr))
    }
}

const PROGRAM: &[u8] = &[
    0x86, b'H', // 1000 LDA #'H'
    0x3f, 0x01, // 1002 SWI T_PUT
    0x86, b'i', // 1004 LDA #'i'
    0x3f, 0x01, // 1006 SWI T_PUT
    0x3f, 0x02, // 1008 SWI T_EXIT
    0x20, 0xfe, // 100A BRA *
];
// A listing of the image, as module-relative offsets (placed with `@toy`).
const LISTING: &str = "0000 8648             (          toy.asm):00001         start   lda     #'H'
0002 3F01             (          toy.asm):00002                 swi
0004 8669             (          toy.asm):00003         again   lda     #'i'
";

fn toy() -> Ice {
    let mut m = Machine::default();
    m.load(0x1000, PROGRAM).unwrap();
    m.load(0x0010, &[0x01, 0x01]).unwrap(); // the running process
    m.load(0xfffa, &[0xf0, 0x00]).unwrap(); // SWI vector
    // Step the stacked PC past the code byte, like a kernel would.
    m.load(0xf000, &[0xae, 0x6a, 0x30, 0x01, 0xaf, 0x6a, 0x3b])
        .unwrap();
    let mut c = Cpu::default();
    c.pc = 0x1000;
    c.s = 0x7000;
    c.cc = 0x50;
    Ice::with_os(
        c,
        m,
        Options {
            actions: VecDeque::new(),
            echo_console: false,
        },
        Box::new(ToyOs),
    )
}

fn get(d: &mut Ice, t: &str) -> Json {
    let (status, body) = d.command("GET", t, b"");
    assert_eq!(status, 200, "{t}: {body}");
    Json::parse(&body).unwrap()
}

#[test]
fn api_through_the_os_layer() {
    let mut d = toy();
    let info = get(&mut d, "/api/os/info");
    assert_eq!(info.get("name").str(), Some("ToyOS"));

    // Break on a request by the OS's own name.
    assert_eq!(
        d.command("POST", "/api/os/break?code=T_PUT&when=call", b"")
            .0,
        200
    );
    d.command("POST", "/api/run", b"");
    d.run_steps(1000);
    assert_eq!(d.cpu.pc, 0x1002);
    let state = get(&mut d, "/api/state");
    assert_eq!(
        state.get("stop").str(),
        Some("システムコール呼び出し T_PUT (プロセス1)")
    );
    assert_eq!(state.get("os").get("pid").int(), Some(1));
    assert_eq!(
        state.get("osbreak").get("calls").arr()[0].str(),
        Some("T_PUT")
    );

    let dis = get(&mut d, "/api/disasm?addr=1002&n=1");
    let l = &dis.get("lines").arr()[0];
    assert_eq!(
        (l.get("text").str(), l.get("svc").bool()),
        (Some("TOY T_PUT"), Some(true))
    );
    assert_eq!(l.get("note").str(), Some("Aの文字を出力する。"));

    d.command("POST", "/api/os/break?op=clear", b"");
    d.command("POST", "/api/run", b"");
    d.run_steps(1000);
    let calls = get(&mut d, "/api/os/calls");
    let calls = calls.get("calls").arr();
    let names: Vec<_> = calls
        .iter()
        .rev()
        .map(|c| {
            (
                c.get("name").str().unwrap(),
                c.get("args").str().unwrap(),
                c.get("result").str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("T_PUT", "文字 'H'", "OK"),
            ("T_PUT", "文字 'i'", "OK"),
            ("T_EXIT", "", "(戻らない)")
        ]
    );

    let procs = get(&mut d, "/api/os/procs");
    let p = &procs.get("procs").arr()[0];
    assert_eq!(
        (p.get("id").int(), p.get("queue").str(), p.get("prio").int()),
        (Some(1), Some("running"), Some(5))
    );
    assert_eq!(
        get(&mut d, "/api/os/modules").get("modules").arr()[0]
            .get("name")
            .str(),
        Some("toy")
    );
    assert_eq!(get(&mut d, "/api/os/memmap").get("unit").int(), Some(256));
    let mem = get(&mut d, "/api/mem?addr=1000&len=2&space=sys");
    assert_eq!(mem.get("hex").str(), Some("8648"));
    assert_eq!(
        get(&mut d, "/api/os/services").get("services").arr().len(),
        2
    );

    // Symbols for the image are placed through `images`.
    let dir = std::env::temp_dir().join(format!("rs6809-toy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("toy.lst"), LISTING).unwrap();
    d.load_symbols(&format!("{}@toy", dir.join("toy.lst").display()))
        .unwrap();
    let dis = get(&mut d, "/api/disasm?addr=1004&n=1");
    assert_eq!(dis.get("lines").arr()[0].get("label").str(), Some("again"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn dap_through_the_os_layer() {
    let mut d = toy();
    d.command("POST", "/api/os/break?code=T_PUT&when=call", b"");
    d.command("POST", "/api/run", b"");
    d.run_steps(1000);
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut seq = 0;
    let mut dap = |d: &mut Ice, command: &str, args: &str| -> Json {
        seq += 1;
        let msg = Json::parse(&format!(
            r#"{{"seq":{seq},"type":"request","command":"{command}","arguments":{args}}}"#
        ))
        .unwrap();
        d.dap_message(1, &msg, tx.clone());
        loop {
            let m = Json::parse(&rx.recv().unwrap()).unwrap();
            if m.get("type").str() == Some("response") {
                assert_eq!(m.get("success").bool(), Some(true), "{command}: {m}");
                return m.get("body").clone();
            }
        }
    };
    let caps = dap(&mut d, "initialize", "{}");
    let filters = caps.get("exceptionBreakpointFilters").arr();
    assert_eq!(filters[0].get("label").str(), Some("Toyエラー"));
    let threads = dap(&mut d, "threads", "{}");
    assert_eq!(
        threads.get("threads").arr()[0].get("name").str(),
        Some("6809 (ToyOS プロセス1 toy)")
    );
    let scopes = dap(&mut d, "scopes", r#"{"frameId":0}"#);
    let os = scopes
        .get("scopes")
        .arr()
        .iter()
        .find(|s| s.get("name").str() == Some("ToyOS"))
        .unwrap()
        .clone();
    let vars = dap(
        &mut d,
        "variables",
        &format!(
            r#"{{"variablesReference":{}}}"#,
            os.get("variablesReference")
        ),
    );
    let names: Vec<_> = vars
        .get("variables")
        .arr()
        .iter()
        .map(|v| v.get("name").str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"prio".to_string()) && names.contains(&"ready".to_string()),
        "{names:?}"
    );
    dap(
        &mut d,
        "setExceptionBreakpoints",
        r#"{"filters":["osswitch"]}"#,
    );
    assert!(d.break_switch);
}
