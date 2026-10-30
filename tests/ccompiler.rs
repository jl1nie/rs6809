use std::{
    fs,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
#[ignore = "requires python3 scripts/build-c-disk.py"]
fn native_c_compiler_rebuilds_and_recovers_from_errors() {
    let root = env!("CARGO_MANIFEST_DIR");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let script = std::env::temp_dir().join(format!("rs6809-c-{}-{nonce}.tsv", std::process::id()));
    fs::write(
        &script,
        concat!(
            "Time ?\t2026/10/31 12:00:00\n",
            "OS9:\tchd /dd/SOURCES\n",
            "OS9:\tcc hello\n",
            "OS9:\t./hello\n",
            "OS9:\tcc hello.c\n",
            "OS9:\t./hello\n",
            "OS9:\tcc missing\n",
            "OS9:\tcc ../hello\n",
            "OS9:\techo CC-TEST-OK\n",
        ),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_rs6809"))
        .current_dir(root)
        .args([
            "--rom",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
            "--disk",
            "artifacts/ccompiler/multicomp09-c.img",
            "--boot",
            "--script",
        ])
        .arg(&script)
        .args(["--until", "\r\nCC-TEST-OK\r\n", "--max-cycles", "300000000"])
        .stdin(Stdio::null())
        .output();
    fs::remove_file(script).unwrap();
    let output = result.unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(text.matches("Hello from C: 42").count(), 2, "{text}");
    assert!(text.contains("cc: compilation failed"), "{text}");
    assert!(text.contains("Usage: cc basename[.c]"), "{text}");
    assert!(!text.contains("out of memory"), "{text}");
}
