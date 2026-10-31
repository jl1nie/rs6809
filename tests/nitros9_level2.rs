use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
#[ignore = "requires reference starter kit and python3 scripts/build-level2-disk.py"]
fn level2_boots_runs_mfree_and_returns_from_child_commands() {
    // One disk; the kernel probes whether the emulator provides 512KB or 1MB.
    for (ram_kb, top) in [("512", "7FFFF"), ("1024", "FFFFF")] {
        level2_mfree(ram_kb, top);
    }
}

fn level2_mfree(ram_kb: &str, top: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_rs6809"))
        .current_dir(&root)
        .args([
            "--rom",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
            "--disk",
            "artifacts/level2/multicomp09-level2.img",
            "--ram-kb",
            ram_kb,
            "--boot",
            "--script",
            "examples/level2.tsv",
            // Include the output's line boundary and next prompt, so echoed input
            // cannot satisfy the stop condition before the command executes.
            "--until",
            "\r\nLEVEL2-DONE\r\n\r\nOS9:",
            "--max-cycles",
            "30000000",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("NitrOS-9/6809 Level 2"), "{text}");
    assert!(text.contains("LEVEL2-STARTED\r\n"), "{text}");
    assert!(text.contains("OS9:mfree\r\n"), "{text}");
    assert!(text.contains(&format!("  {top}  ")), "{text}");
    assert!(text.contains("Total:"), "{text}");
    assert!(text.ends_with("\r\nLEVEL2-DONE\r\n\r\nOS9:"), "{text}");
}

#[test]
#[ignore = "requires reference starter kit and python3 scripts/build-level2-disk.py"]
fn level2_original_cc1_compiles_and_runs_c() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_rs6809"))
        .current_dir(&root)
        .args([
            "--rom",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
            "--disk",
            "artifacts/level2/multicomp09-level2.img",
            "--ram-kb",
            "512",
            "--boot",
            "--script",
            "examples/level2-cc1.tsv",
            "--until",
            "\r\nHello from C: 42\r\n",
            "--max-cycles",
            "1000000000",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Microware's driver runs every pass; Level 1 could not fit cc1 and its passes.
    assert!(text.contains("CC1 VERSION RS 01.00.00"), "{text}");
    assert!(
        text.contains("c.prep:\r\nc.pass1:\r\nc.pass2:\r\nc.opt:\r\nc.asm:\r\nc.link:\r\n"),
        "{text}"
    );
}

#[test]
#[ignore = "requires reference starter kit and python3 scripts/build-level2-disk.py"]
fn level2_basic09_runs_programs_and_the_mandelbrot_sample() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_rs6809"))
        .current_dir(&root)
        .args([
            "--rom",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
            "--disk",
            "artifacts/level2/multicomp09-level2.img",
            "--ram-kb",
            "512",
            "--boot",
            "--script",
            "examples/level2-basic09.tsv",
            "--until",
            "\r\nBASIC09-DONE\r\n",
            "--max-cycles",
            "3000000000",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("B:run test\r\n42\r\n"), "{text}");
    let normalized = text.replace("\r\n", "\n");
    let rows: Vec<&str> = normalized
        .split_once("B:run mandel\n")
        .unwrap()
        .1
        .split_once("Ready")
        .unwrap()
        .0
        .lines()
        // BASIC09 emits a shift-out control byte before its Ready prompt.
        .filter(|row| !row.chars().all(char::is_control))
        .collect();
    // Same grid and escape mapping as examples/mandel.c.
    assert_eq!(rows.len(), 25, "{text}");
    assert!(rows.iter().all(|row| row.len() == 79), "{text}");
    assert!(rows.iter().eq(rows.iter().rev()), "{text}");
    assert_eq!(rows[12].as_bytes()[39], b' ');
}
