//! The ICE with the 68000: HTTP API and DAP on a small hand-assembled program.
use rs6809::ice::json::Json;
use rs6809::ice::{Ice, Mc68000, Options};
use rs6809::m68k::machine::Sim68k;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const PROGRAM: &[u8] = &[
    0x41, 0xf9, 0x00, 0x00, 0x20, 0x00, // 1000 LEA $2000.L,A0
    0x61, 0x00, 0x00, 0x0a, //             1006 BSR.W $1012
    0x13, 0xc0, 0x00, 0x00, 0x30, 0x00, // 100A MOVE.B D0,$3000.L
    0x60, 0xfe, //                         1010 BRA.S $1010
    0x70, 0x42, //                         1012 MOVEQ #$42,D0
    0x13, 0xc0, 0x00, 0xff, 0x00, 0x01, // 1014 MOVE.B D0,$FF0001.L (console)
    0x4e, 0x75, //                         101A RTS
];

fn target() -> Mc68000 {
    let mut m = Sim68k::default();
    m.set_vectors(0x8000, 0x1000);
    m.load(0x1000, PROGRAM).unwrap();
    Mc68000::new(m)
}
fn options() -> Options {
    Options {
        actions: VecDeque::new(),
        echo_console: false,
    }
}
fn ice() -> Ice<Mc68000> {
    Ice::m68000(target(), options())
}
fn ok(ice: &mut Ice<Mc68000>, target: &str) -> String {
    let (status, body) = ice.command("POST", target, b"");
    assert_eq!(status, 200, "{target}: {body}");
    body
}
fn get(ice: &mut Ice<Mc68000>, target: &str) -> Json {
    let (status, body) = ice.command("GET", target, b"");
    assert_eq!(status, 200, "{target}: {body}");
    Json::parse(&body).unwrap()
}

#[test]
fn step_over_and_out() {
    let mut d = ice();
    assert_eq!((d.t.cpu.pc, d.t.cpu.a[7]), (0x1000, 0x8000));
    ok(&mut d, "/api/step");
    assert_eq!((d.t.cpu.pc, d.t.cpu.a[0]), (0x1006, 0x2000));
    ok(&mut d, "/api/over");
    d.run_steps(100);
    assert_eq!((d.t.cpu.pc, d.t.cpu.d[0], d.t.cpu.a[7], d.running), (0x100a, 0x42, 0x8000, false));
    assert_eq!(d.console, b"B");

    d.t.cpu.pc = 0x1006;
    ok(&mut d, "/api/step");
    assert_eq!((d.t.cpu.pc, d.t.cpu.a[7]), (0x1012, 0x7ffc));
    let stack = get(&mut d, "/api/stack");
    assert!(stack.to_string().contains("4106"), "return address $100A on the stack: {stack}");
    ok(&mut d, "/api/out");
    d.run_steps(100);
    assert_eq!((d.t.cpu.pc, d.t.cpu.a[7]), (0x100a, 0x8000));
}

#[test]
fn breakpoints_watchpoints_and_registers() {
    let mut d = ice();
    ok(&mut d, "/api/bp?addr=1010");
    ok(&mut d, "/api/watch?addr=3000&len=1&kind=w");
    ok(&mut d, "/api/run");
    d.run_steps(100);
    assert_eq!(d.t.cpu.pc, 0x1010, "the watched write stops after MOVE.B");
    let st = get(&mut d, "/api/state");
    assert_eq!(st.get("cpu").str(), Some("68000"));
    assert!(st.get("stop").str().unwrap().contains("ウォッチ $3000"), "{st}");
    assert_eq!(st.get("regs").get("d0").int(), Some(0x42));
    assert_eq!(d.t.machine.ram[0x3000], 0x42);

    ok(&mut d, "/api/watch?op=clear");
    d.t.cpu.pc = 0x100a;
    ok(&mut d, "/api/run");
    d.run_steps(100);
    let st = get(&mut d, "/api/state");
    assert!(st.get("stop").str().unwrap().contains("ブレークポイント $1010"), "{st}");

    ok(&mut d, "/api/reg?name=d3&val=12345678");
    ok(&mut d, "/api/reg?name=A6&val=ABCDEF");
    ok(&mut d, "/api/reg?name=sr&val=0");
    assert_eq!((d.t.cpu.d[3], d.t.cpu.a[6], d.t.cpu.sr), (0x1234_5678, 0xab_cdef, 0));
    // Clearing S switches A7 to the (never set) user stack pointer.
    assert_eq!((d.t.cpu.a[7], d.t.cpu.usp(), d.t.cpu.ssp()), (0, 0, 0x8000));
}

#[test]
fn disassembly_memory_and_trace() {
    let mut d = ice();
    let dis = get(&mut d, "/api/disasm?addr=1000&n=7");
    let lines = dis.get("lines").arr();
    let text: Vec<_> = lines.iter().map(|l| l.get("text").str().unwrap()).collect();
    assert_eq!(
        text,
        [
            "LEA $2000.L,A0",
            "BSR.W $1012",
            "MOVE.B D0,$3000.L",
            "BRA.S $1010",
            "MOVEQ #$42,D0",
            "MOVE.B D0,$FF0001.L",
            "RTS"
        ]
    );
    assert_eq!(lines[1].get("addr").int(), Some(0x1006));

    ok(&mut d, "/api/mem?addr=4000&val=CAFE");
    let mem = get(&mut d, "/api/mem?addr=3FFF&len=4");
    assert_eq!(&d.t.machine.ram[0x4000..0x4002], &[0xca, 0xfe]);
    assert_eq!(mem.get("hex").str(), Some("00CAFE00"));

    // Memory above 64 KiB.
    ok(&mut d, "/api/mem?addr=0F0000&val=11");
    assert_eq!(d.t.machine.ram[0xf_0000], 0x11);

    ok(&mut d, "/api/step");
    ok(&mut d, "/api/step");
    let trace = get(&mut d, "/api/trace?n=2");
    assert!(trace.to_string().contains("BSR.W"), "{trace}");
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
        let msg = format!(
            r#"{{"seq":{},"type":"request","command":"{command}","arguments":{args}}}"#,
            self.seq
        );
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
fn dap_session() {
    let ice = ice();
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let dap = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = dap.local_addr().unwrap().port();
    std::thread::spawn(move || ice.serve(http, Some(dap), false));
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut d = Client {
        w: s.try_clone().unwrap(),
        r: BufReader::new(s),
        seq: 0,
        events: Vec::new(),
    };
    d.request("initialize", r#"{"adapterID":"rs6809","linesStartAt1":true}"#);
    d.event("initialized");
    d.request("attach", "{}");
    let bps = d.request(
        "setInstructionBreakpoints",
        r#"{"breakpoints":[{"instructionReference":"0x1014"}]}"#,
    );
    assert_eq!(bps.get("breakpoints").arr()[0].get("verified").bool(), Some(true));
    d.request("configurationDone", "{}");
    assert_eq!(d.event("stopped").get("reason").str(), Some("pause"));

    d.request("continue", r#"{"threadId":1}"#);
    let stop = d.event("stopped");
    assert_eq!(stop.get("reason").str(), Some("breakpoint"), "{stop}");
    let frames = d.request("stackTrace", r#"{"threadId":1}"#);
    let frames = frames.get("stackFrames").arr();
    assert_eq!(frames[0].get("instructionPointerReference").str(), Some("0x1014"));
    // The BSR's return address gives the caller's frame.
    assert_eq!(frames[1].get("instructionPointerReference").str(), Some("0x100A"), "{frames:?}");

    let regs = d.request("variables", r#"{"variablesReference":1}"#).to_string();
    assert!(regs.contains(r#""name":"D0","value":"$00000042""#), "{regs}");
    assert!(regs.contains(r#""name":"A7","value":"$00007FFC""#), "{regs}");
    assert!(regs.contains(r#""name":"SR","value":"$2700""#), "{regs}");
    d.request("setVariable", r#"{"variablesReference":1,"name":"D0","value":"$41"}"#);
    let stack = d.request("variables", r#"{"variablesReference":2}"#).to_string();
    assert!(stack.contains(r#""value":"$0000100A ↩""#), "{stack}");

    let dis = d.request(
        "disassemble",
        r#"{"memoryReference":"0x1012","instructionCount":3}"#,
    );
    let ins: Vec<_> = dis
        .get("instructions")
        .arr()
        .iter()
        .map(|i| i.get("instruction").str().unwrap().to_string())
        .collect();
    assert_eq!(ins, ["MOVEQ #$42,D0", "MOVE.B D0,$FF0001.L", "RTS"]);

    let ev = d.request("evaluate", r#"{"expression":"d0"}"#);
    assert!(ev.get("result").str().unwrap().starts_with("$000041"), "{ev}");

    d.request("stepOut", r#"{"threadId":1}"#);
    d.event("stopped");
    let frames = d.request("stackTrace", r#"{"threadId":1}"#);
    assert_eq!(
        frames.get("stackFrames").arr()[0].get("instructionPointerReference").str(),
        Some("0x100A")
    );
    d.request("next", r#"{"threadId":1}"#);
    d.event("stopped");
    let mem = d.request("readMemory", r#"{"memoryReference":"0x3000","count":1}"#);
    assert_eq!(mem.get("data").str(), Some("QQ=="), "'A' written by the program: {mem}");
    let out = d.event("output");
    assert!(out.get("output").str().unwrap().contains('A'), "{out}");
    d.request("disconnect", "{}");
}
