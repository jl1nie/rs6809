use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
#[ignore = "requires reference starter kit and python3 scripts/build-level2-disk.py"]
fn level2_boots_runs_mfree_and_returns_from_child_commands() {
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
    assert!(text.contains("7FFFF") && text.contains("Total:"), "{text}");
    assert!(text.ends_with("\r\nLEVEL2-DONE\r\n\r\nOS9:"), "{text}");
}
