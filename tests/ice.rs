use rs6809::ice::os9::{Level, View};
use rs6809::ice::{Ice, Options};
use rs6809::{Cpu, Machine};
use std::collections::VecDeque;
use std::fs::{self, File};

fn options() -> Options {
    Options {
        actions: VecDeque::new(),
        echo_console: false,
    }
}
fn ice(code: &[u8], at: u16, setup: impl FnOnce(&mut Machine)) -> Ice {
    let mut m = Machine::default();
    m.load(at, code).unwrap();
    setup(&mut m);
    let mut c = Cpu::default();
    c.pc = at;
    c.s = 0x7000;
    c.cc = 0x50;
    Ice::new(c, m, options())
}
fn ok(ice: &mut Ice, target: &str) -> String {
    let (status, body) = ice.command("POST", target, b"");
    assert_eq!(status, 200, "{target}: {body}");
    body
}

const PROGRAM: &[u8] = &[
    0x8e, 0x20, 0x00, // 1000 LDX #$2000
    0xbd, 0x10, 0x10, // 1003 JSR $1010
    0xb7, 0x30, 0x00, // 1006 STA $3000
    0x20, 0xfe, //       1009 BRA $1009
    0, 0, 0, 0, 0, //
    0x86, 0x42, //       1010 LDA #$42
    0x39, //             1012 RTS
];

#[test]
fn step_over_and_out() {
    let mut d = ice(PROGRAM, 0x1000, |_| ());
    ok(&mut d, "/api/step");
    assert_eq!(d.cpu.pc, 0x1003);
    ok(&mut d, "/api/over");
    d.run_steps(100);
    assert_eq!((d.cpu.pc, d.cpu.a, d.running), (0x1006, 0x42, false));

    d.cpu.pc = 0x1003;
    ok(&mut d, "/api/step");
    assert_eq!(d.cpu.pc, 0x1010);
    ok(&mut d, "/api/out");
    d.run_steps(100);
    assert_eq!((d.cpu.pc, d.cpu.s), (0x1006, 0x7000));
}

#[test]
fn breakpoints_and_watchpoints() {
    let mut d = ice(PROGRAM, 0x1000, |_| ());
    ok(&mut d, "/api/bp?addr=1009");
    ok(&mut d, "/api/watch?addr=3000&len=1&kind=w");
    ok(&mut d, "/api/run");
    d.run_steps(100);
    // The store hits the watchpoint first; the write itself has completed.
    assert_eq!(d.cpu.pc, 0x1009);
    assert_eq!(d.machine.peek(0x3000), 0x42);
    let state = ok(&mut d, "/api/state");
    assert!(state.contains("ウォッチ $3000 書き込み $42"), "{state}");

    // Resuming skips the breakpoint at the current PC only once.
    ok(&mut d, "/api/watch?op=clear");
    d.cpu.pc = 0x1000;
    ok(&mut d, "/api/run");
    d.run_steps(100);
    assert_eq!(d.cpu.pc, 0x1009);
    assert!(ok(&mut d, "/api/state").contains("ブレークポイント $1009"));
    ok(&mut d, "/api/run");
    d.run_steps(10);
    assert_eq!(d.cpu.pc, 0x1009);
    assert!(!d.running);
}

#[test]
fn disassembly_and_memory_api() {
    let mut d = ice(PROGRAM, 0x1000, |_| ());
    let (_, dis) = d.command("GET", "/api/disasm?addr=1003&n=2", b"");
    assert!(
        dis.contains("JSR $1010") && dis.contains("STA $3000"),
        "{dis}"
    );
    let (_, dis) = d.command("GET", "/api/disasm?addr=1006&back=2&n=1", b"");
    assert!(dis.contains("LDX #$2000"), "{dis}");
    ok(&mut d, "/api/mem?addr=2000&val=CAFE");
    let (_, mem) = d.command("GET", "/api/mem?addr=2000&len=2", b"");
    assert!(mem.contains("\"hex\":\"CAFE\""), "{mem}");
    ok(&mut d, "/api/reg?name=x&val=1234");
    assert_eq!(d.cpu.x, 0x1234);
}

/// A minimal Level 1 layout: Init module, one user process, and an SWI2
/// handler that returns immediately, standing in for the kernel.
fn fake_level1(m: &mut Machine) {
    let module = |m: &mut Machine, at: u16, name: &[u8]| {
        let a = usize::from(at);
        m.ram[a..a + 9].copy_from_slice(&[0x87, 0xcd, 0x00, 0x20, 0x00, 0x0d, 0x11, 0x81, 0]);
        let n = a + 0x0d;
        m.ram[n..n + name.len()].copy_from_slice(name);
        m.ram[n + name.len() - 1] |= 0x80;
    };
    module(m, 0x0400, b"Init");
    module(m, 0x0700, b"Hello");
    m.load(0x2a, &[0x04, 0x00]).unwrap(); // D.Init
    m.load(0x49, &[0x05, 0x00, 0x06, 0x40]).unwrap(); // D.PrcDBT, D.Proc
    m.load(0x26, &[0x08, 0x00, 0x08, 0x08]).unwrap(); // D.ModDir..end
    m.load(0x0800, &[0x04, 0x00, 0x01, 0x00, 0x07, 0x00, 0x01, 0x00])
        .unwrap();
    m.ram[0x0500] = 0x06; // process 1 lives in page 6, slot 1
    m.ram[0x0640] = 1; // P$ID
    m.load(0x0652, &[0x07, 0x00]).unwrap(); // P$PModul
    m.load(0xfff4, &[0xf0, 0x00]).unwrap(); // SWI2 vector
    // Like the kernel, step the stacked PC past the service code byte.
    m.load(0xf000, &[0xae, 0x6a, 0x30, 0x01, 0xaf, 0x6a, 0x3b])
        .unwrap(); // LDX 10,S; LEAX 1,X; STX 10,S; RTI
    m.load(0x1020, b"hi\r").unwrap();
}
const WRITLN: &[u8] = &[
    0x86, 0x01, //             1000 LDA #1
    0x8e, 0x10, 0x20, //       1002 LDX #$1020
    0x10, 0x8e, 0x00, 0x03, // 1005 LDY #3
    0x10, 0x3f, 0x8c, //       1009 OS9 I$WritLn
    0x20, 0xfe, //             100C BRA *
];

#[test]
fn traces_and_breaks_on_os9_calls() {
    let mut d = ice(WRITLN, 0x1000, fake_level1);
    let (_, dis) = d.command("GET", "/api/disasm?addr=1009&n=1", b"");
    assert!(
        dis.contains("OS9 I$WritLn") && dis.contains("パスAへ1行"),
        "{dis}"
    );

    ok(&mut d, "/api/os9break?code=I$WritLn&when=call");
    ok(&mut d, "/api/run");
    d.run_steps(100);
    assert_eq!(d.cpu.pc, 0x1009);
    assert!(ok(&mut d, "/api/state").contains("システムコール呼び出し I$WritLn (プロセス1)"));

    ok(&mut d, "/api/os9break?code=I$WritLn&op=del");
    ok(&mut d, "/api/os9break?code=I$WritLn&when=return");
    ok(&mut d, "/api/run");
    d.run_steps(100);
    assert_eq!(d.cpu.pc, 0x100c);
    assert!(ok(&mut d, "/api/state").contains("システムコール復帰 I$WritLn"));

    let (_, calls) = d.command("GET", "/api/os9/calls?user=1", b"");
    assert!(calls.contains(r#""name":"I$WritLn""#), "{calls}");
    assert!(
        calls.contains(r#"パス1(stdout) 3バイト \"hi\\r\""#),
        "{calls}"
    );
    assert!(calls.contains("3バイト書いた"), "{calls}");
    let (_, procs) = d.command("GET", "/api/os9/procs", b"");
    assert!(procs.contains(r#""module":"Hello""#), "{procs}");
    let (_, mods) = d.command("GET", "/api/os9/modules", b"");
    assert!(
        mods.contains(r#""name":"Init""#) && mods.contains(r#""name":"Hello""#),
        "{mods}"
    );
}

/// Level 2 structures laid out per defs/os9.d. No Level 2 kernel image is
/// available offline, so this checks the decoder against the definitions.
#[test]
fn decodes_level2_structures() {
    let mut m =
        Machine::multicomp_with_ram(&[0; 8192], File::open("Cargo.toml").unwrap(), 512 * 1024)
            .unwrap();
    let put = |m: &mut Machine, at: usize, bytes: &[u8]| {
        m.ram[at..at + bytes.len()].copy_from_slice(bytes)
    };
    let be = |v: u16| v.to_be_bytes();
    // System DAT image at $0300: slot 2 maps block $20 (physical $40000).
    let sysdat: Vec<u8> = [0u16, 1, 0x20, 3, 4, 5, 6, 0x3f]
        .iter()
        .flat_map(|&b| be(b))
        .collect();
    put(&mut m, 0x0300, &sysdat);
    put(&mut m, 0x4c, &be(0x0300)); // D.SysDAT
    put(
        &mut m,
        0x0400,
        &[0x87, 0xcd, 0x00, 0x20, 0x00, 0x0d, 0xc0, 0x81, 0],
    );
    put(&mut m, 0x040d, b"Ini\xf4");
    put(&mut m, 0x24, &be(0x0400)); // D.Init
    put(&mut m, 0x48, &be(0x0500)); // D.PrcDBT
    m.ram[0x0502] = 0x08; // process 2 at $0800
    put(&mut m, 0x50, &be(0x0800)); // D.Proc
    put(&mut m, 0x0800, &[2, 1, 0, 0, 0x1f, 0xf0, 3]); // ID, PID, SID, CID, SP, Task
    put(&mut m, 0x0811, &be(0x0010)); // P$PModul in the process' own space
    let image: Vec<u8> = [
        0x10u16, 0x333e, 0x333e, 0x333e, 0x333e, 0x333e, 0x333e, 0x333e,
    ]
    .iter()
    .flat_map(|&b| be(b))
    .collect();
    put(&mut m, 0x0840, &image); // P$DATImg
    put(
        &mut m,
        0x20010,
        &[0x87, 0xcd, 0x00, 0x20, 0x00, 0x0d, 0x11, 0x81, 0],
    );
    put(&mut m, 0x2001d, b"Shel\xec");
    put(&mut m, 0x44, &[0x07, 0x00, 0x07, 0x08]); // D.ModDir..D.ModEnd
    put(
        &mut m,
        0x0700,
        &[0x03, 0x00, 0x20, 0x00, 0x04, 0x00, 0x00, 0x02],
    );
    put(&mut m, 0x40, &[0x09, 0x00, 0x09, 0x40]); // D.BlkMap
    m.ram[0x0910] = 1;
    m.ram[0x40000] = 0x5a;

    let v = View::detect(&m).expect("Level 2 detected");
    assert_eq!(v.level, Level::Two);
    assert_eq!(v.sys(0x4000), 0x5a);
    assert_eq!(v.pid_of(v.current()), 2);
    let procs = v.processes();
    assert_eq!(procs.len(), 1);
    assert_eq!(
        (procs[0].module.as_str(), procs[0].task, procs[0].dat[0]),
        ("Shell", Some(3), 0x10)
    );
    let mods = v.modules();
    assert_eq!((mods[0].name.as_str(), mods[0].links), ("Init", 2));
    assert_eq!(mods[0].blocks, vec![0]);
    let (unit, map) = v.memory_map();
    assert_eq!((unit, map.len(), map[0x10]), (8192, 64, 1));
}

/// Boot the reference NitrOS-9 Level 1 under the ICE and watch the shell run
/// `echo`: requires the kit described in README.
#[test]
#[ignore]
fn traces_real_nitros9_shell() {
    let kit = "reference/nitros9-basic09-starter-kit/nitros9-runtime";
    let m = Machine::multicomp(
        &fs::read(format!("{kit}/6809M.bin")).unwrap(),
        File::open(format!("{kit}/multicomp09_sd.img")).unwrap(),
    )
    .unwrap();
    let mut c = Cpu::default();
    let mut m = m;
    c.reset(&mut m);
    let actions = VecDeque::from([
        ("CamelForth".to_string(), "NITROS9".to_string()),
        ("Time ?".into(), "2026/10/31 12:00:00".into()),
        ("OS9:".into(), "echo traced".into()),
    ]);
    let mut d = Ice::new(
        c,
        m,
        Options {
            actions,
            echo_console: false,
        },
    );
    ok(&mut d, "/api/run");
    for _ in 0..400 {
        d.run_steps(1_000_000);
        // The shell echoes the typed line; the second copy is echo's output.
        let (_, con) = d.command("GET", "/api/console?since=0", b"");
        if con.matches("traced\\r\\n").count() >= 2 {
            break;
        }
    }
    let (_, con) = d.command("GET", "/api/console?since=0", b"");
    assert!(con.contains("traced\\r\\n"), "{con}");
    let (_, calls) = d.command("GET", "/api/os9/calls?user=1&limit=100000", b"");
    assert!(
        calls.contains(r#"モジュール\"echo\" パラメータ\"traced\\r\""#),
        "{calls}"
    );
    assert!(
        calls.contains(r#"パス1(stdout) 7バイト \"traced\\r\""#),
        "{calls}"
    );
    let (_, procs) = d.command("GET", "/api/os9/procs", b"");
    assert!(
        procs.contains(r#""module":"Shell""#) && procs.contains(r#""module":"SysGo""#),
        "{procs}"
    );
    let state = ok(&mut d, "/api/state");
    assert!(state.contains("\"level\":1"), "{state}");
}
