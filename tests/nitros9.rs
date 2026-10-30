use rs6809::{Cpu, Machine};
use std::{
    collections::VecDeque,
    fs::{self, File},
    path::PathBuf,
};
#[test]
#[ignore = "requires reference starter kit; run cargo test --release --test nitros9 -- --ignored --nocapture"]
fn boot_to_basic09_and_run_program() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("reference/nitros9-basic09-starter-kit/nitros9-runtime");
    let mut m = Machine::multicomp(
        &fs::read(root.join("6809M.bin")).unwrap(),
        File::open(root.join("multicomp09_sd.img")).unwrap(),
    )
    .unwrap();
    let mut c = Cpu::default();
    c.reset(&mut m);
    let mut actions = VecDeque::from([
        ("CamelForth", "NITROS9"),
        ("Time ?", "2026/10/30 12:00:00"),
        ("OS9:", "basic09"),
        ("B:", "e test"),
        ("E:", "10 PRINT 6*7\r20 END\rq"),
        ("B:", "run test"),
    ]);
    let mut pending = VecDeque::new();
    let mut next = 0;
    let mut transcript = String::new();
    let mut stage = String::new();
    while c.cycles < 300_000_000 {
        m.step(&mut c)
            .unwrap_or_else(|e| panic!("{e}; {c:?}\n{transcript}"));
        assert!(m.disk_error().is_none(), "disk I/O error");
        for b in std::mem::take(&mut m.acia.tx) {
            let ch = char::from(b);
            print!("{ch}");
            transcript.push(ch);
            stage.push(ch);
        }
        if let Some((prompt, _)) = actions.front()
            && stage.contains(prompt)
        {
            let (_, response) = actions.pop_front().unwrap();
            pending.extend(response.bytes());
            pending.push_back(13);
            stage.clear();
            next = c.cycles + 100000;
        }
        if c.cycles >= next
            && m.acia.rx.is_empty()
            && let Some(b) = pending.pop_front()
        {
            m.acia.rx.push_back(b);
            next = c.cycles + 30000;
        }
        if actions.is_empty() && stage.contains("42") && stage.contains("B:") {
            assert!(transcript.contains("NitrOS-9"));
            return;
        }
    }
    panic!("boot timed out; CPU {c:?}\n{transcript}");
}

#[test]
#[ignore = "requires reference starter kit"]
fn cli_runs_basic09_script() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rs6809"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "--rom",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
            "--disk",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img",
            "--boot",
            "--script",
            "examples/basic09.tsv",
            "--until",
            "42",
            "--max-cycles",
            "300000000",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("OS9:"));
    assert!(text.contains("B:run test\r\n42"), "{text}");
}
