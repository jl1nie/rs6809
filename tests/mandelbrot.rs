use std::process::{Command, Stdio};

#[test]
#[ignore = "requires python3 scripts/build-c-disk.py"]
fn compiles_renders_and_measures_mandelbrot() {
    let output = Command::new(env!("CARGO_BIN_EXE_rs6809"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "--rom",
            "reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin",
            "--disk",
            "artifacts/ccompiler/multicomp09-c.img",
            "--boot",
            "--script",
            "examples/mandelbrot.tsv",
            "--measure-from",
            "Mandelbrot / BASIC09 to C / 6809",
            "--until",
            "MANDELBROT-DONE\r\n\r\nOS9:",
            "--max-cycles",
            "1000000000",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let stats = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}\n{stats}");
    assert!(
        text.contains("OS9:cc mandel\r\nc.prep\r\nc.pass1\r\nc.pass2\r\nc.opt\r\nc.asm\r\nc.link"),
        "{text}"
    );
    assert!(!text.contains("compilation failed"), "{text}");
    let normalized = text.replace("\r\n", "\n");
    let picture = normalized
        .split_once("Mandelbrot / BASIC09 to C / 6809\n")
        .unwrap()
        .1
        .split_once("MANDELBROT-DONE")
        .unwrap()
        .0;
    let rows: Vec<&str> = picture.lines().collect();
    assert_eq!(rows.len(), 25, "{text}");
    for row in &rows {
        assert_eq!(row.len(), 79, "{row}");
        assert!(
            row.bytes().all(|b| b"0123456789ABCDEF ".contains(&b)),
            "{row}"
        );
    }
    // Real-axis symmetry and known points from the supplied BASIC09 grid.
    assert!(rows.iter().eq(rows.iter().rev()), "{text}");
    assert_eq!(rows[12].as_bytes()[39], b' '); // c=0 stays bounded.
    assert_eq!(rows[12].as_bytes()[0], b' '); // c=-39*.0458 lies in [-2,0].
    assert_eq!(rows[0].as_bytes()[0], b'0'); // Escapes after the first update.
    assert!(rows.iter().all(|row| row.as_bytes()[78] == b'0'));
    let cycles = |key: &str| -> u64 {
        stats
            .split_whitespace()
            .find_map(|s| s.strip_prefix(key))
            .unwrap()
            .parse()
            .unwrap()
    };
    let measured = cycles("measured_cycles=");
    assert!((100_000_000..1_000_000_000).contains(&measured), "{stats}");
    assert!(cycles("total_cycles=") > measured, "{stats}");
}
