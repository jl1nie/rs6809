//! Debug Adapter Protocol over TCP against a small LWASM-assembled program.
use rs6809::ice::json::Json;
use rs6809::ice::{Ice, Options};
use rs6809::{Cpu, Machine};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

// `lwasm --6809 --format=raw --list=demo.lst demo.asm` (LWTOOLS 4.25).
const SOURCE: &str = "* DAP test program
        org     $1000
start   lds     #$7000
        ldx     #$2000
loop    jsr     count
        leax    1,x
        bra     loop

count   lda     #$42
        sta     $3000
        rts
";
const LISTING: &str = "                      (         demo.asm):00001         * DAP test program
                      (         demo.asm):00002                 org     $1000
1000 10CE7000         (         demo.asm):00003         start   lds     #$7000
1004 8E2000           (         demo.asm):00004                 ldx     #$2000
1007 BD100E           (         demo.asm):00005         loop    jsr     count
100A 3001             (         demo.asm):00006                 leax    1,x
100C 20F9             (         demo.asm):00007                 bra     loop
                      (         demo.asm):00008
100E 8642             (         demo.asm):00009         count   lda     #$42
1010 B73000           (         demo.asm):00010                 sta     $3000
1013 39               (         demo.asm):00011                 rts
";
const CODE: &[u8] = &[
    0x10, 0xce, 0x70, 0x00, 0x8e, 0x20, 0x00, 0xbd, 0x10, 0x0e, 0x30, 0x01, 0x20, 0xf9, 0x86, 0x42,
    0xb7, 0x30, 0x00, 0x39,
];

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
            if m.get("type").str() == Some("response")
                && m.get("request_seq").int() == Some(self.seq)
            {
                assert_eq!(m.get("success").bool(), Some(true), "{command}: {m}");
                return m.get("body").clone();
            }
            self.events.push(m);
        }
    }
    fn event(&mut self, name: &str) -> Json {
        if let Some(i) = self
            .events
            .iter()
            .position(|e| e.get("event").str() == Some(name))
        {
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
fn vscode_style_session() {
    let dir = std::env::temp_dir().join(format!("rs6809-dap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("demo.asm"), SOURCE).unwrap();
    std::fs::write(dir.join("demo.lst"), LISTING).unwrap();
    let asm: PathBuf = std::fs::canonicalize(dir.join("demo.asm")).unwrap();

    let mut m = Machine::default();
    m.load(0x1000, CODE).unwrap();
    let mut c = Cpu::default();
    c.pc = 0x1000;
    let ice = Ice::new(
        c,
        m,
        Options {
            actions: VecDeque::new(),
            echo_console: false,
        },
    );
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

    let caps = d.request(
        "initialize",
        r#"{"adapterID":"rs6809","linesStartAt1":true}"#,
    );
    assert_eq!(caps.get("supportsDisassembleRequest").bool(), Some(true));
    d.event("initialized");
    let lst = dir.join("demo.lst");
    d.request(
        "attach",
        &format!(
            r#"{{"symbols":[{}]}}"#,
            Json::from(lst.to_string_lossy().into_owned())
        ),
    );
    let path = Json::from(asm.to_string_lossy().into_owned());
    let bps = d.request(
        "setBreakpoints",
        &format!(r#"{{"source":{{"path":{path}}},"breakpoints":[{{"line":10}},{{"line":8}}]}}"#),
    );
    let bps = bps.get("breakpoints").arr();
    assert_eq!(
        (bps[0].get("verified").bool(), bps[0].get("line").int()),
        (Some(true), Some(10))
    );
    // Line 8 is blank: the breakpoint moves to the next line with code.
    assert_eq!(bps[1].get("line").int(), Some(9));
    d.request(
        "setBreakpoints",
        &format!(r#"{{"source":{{"path":{path}}},"breakpoints":[{{"line":10}}]}}"#),
    );
    d.request("configurationDone", "{}");
    assert_eq!(d.event("stopped").get("reason").str(), Some("pause"));

    d.request("continue", r#"{"threadId":1}"#);
    let stop = d.event("stopped");
    assert_eq!(stop.get("reason").str(), Some("breakpoint"), "{stop}");
    let frames = d.request("stackTrace", r#"{"threadId":1}"#);
    let frames = frames.get("stackFrames").arr();
    assert_eq!(frames[0].get("name").str(), Some("count+$2"));
    assert_eq!(frames[0].get("line").int(), Some(10));
    assert_eq!(frames[0].get("source").get("path"), &path);
    // The JSR's return address identifies the caller's line.
    assert_eq!(
        (frames[1].get("name").str(), frames[1].get("line").int()),
        (Some("loop+$2"), Some(5))
    );

    let regs = d.request("variables", r#"{"variablesReference":1}"#);
    assert!(
        regs.to_string()
            .contains(r#""name":"PC","value":"$1010  <count+$2>""#),
        "{regs}"
    );
    d.request(
        "setVariable",
        r#"{"variablesReference":1,"name":"A","value":"$55"}"#,
    );

    d.request("next", r#"{"threadId":1}"#);
    d.event("stopped");
    let frames = d.request("stackTrace", r#"{"threadId":1}"#);
    assert_eq!(
        frames.get("stackFrames").arr()[0].get("line").int(),
        Some(11)
    );
    let mem = d.request("readMemory", r#"{"memoryReference":"0x3000","count":1}"#);
    assert_eq!(
        mem.get("data").str(),
        Some("VQ=="),
        "STA stored the edited A"
    );

    d.request("stepOut", r#"{"threadId":1}"#);
    d.event("stopped");
    let frames = d.request("stackTrace", r#"{"threadId":1}"#);
    assert_eq!(
        frames.get("stackFrames").arr()[0].get("line").int(),
        Some(6)
    );

    let e = d.request("evaluate", r#"{"expression":"count","context":"watch"}"#);
    assert!(e.get("result").str().unwrap().starts_with("$100E"), "{e}");
    let dis = d.request(
        "disassemble",
        r#"{"memoryReference":"0x1007","instructionCount":2}"#,
    );
    let ins = dis.get("instructions").arr();
    assert!(
        ins[0].get("instruction").str().unwrap().contains("<count>"),
        "{dis}"
    );
    assert_eq!(ins[0].get("symbol").str(), Some("loop"));
    assert_eq!(ins[0].get("line").int(), Some(5));

    d.request(
        "setFunctionBreakpoints",
        r#"{"breakpoints":[{"name":"count"}]}"#,
    );
    d.request(
        "setBreakpoints",
        &format!(r#"{{"source":{{"path":{path}}},"breakpoints":[]}}"#),
    );
    d.request("continue", r#"{"threadId":1}"#);
    let stop = d.event("stopped");
    assert_eq!(stop.get("reason").str(), Some("breakpoint"));
    let frames = d.request("stackTrace", r#"{"threadId":1}"#);
    assert_eq!(
        frames.get("stackFrames").arr()[0].get("name").str(),
        Some("count")
    );
    d.request("disconnect", "{}");
    std::fs::remove_dir_all(&dir).unwrap();
}
