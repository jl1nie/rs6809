//! Snapshots of the OS views at fixed stop points, to check that
//! refactoring the OS layer leaves the output unchanged.
//! `SNAPSHOT_DIR=dir cargo test --release --test os_snapshot snapshot -- --ignored`
//! writes `l1-*.json` and `l2-*.json`; take one before and one after a
//! change and compare them as JSON values. `API` selects the endpoint prefix.
use rs6809::ice::{Ice, Options};
use rs6809::{Cpu, Machine};
use std::collections::VecDeque;
use std::fs::{self, File};

fn boot(disk: &str, level2: bool) -> Ice {
    let kit = "reference/nitros9-basic09-starter-kit/nitros9-runtime";
    let mut m = Machine::multicomp_with_ram(
        &fs::read(format!("{kit}/6809M.bin")).unwrap(),
        File::open(disk).unwrap(),
        512 * 1024,
    )
    .unwrap();
    let mut c = Cpu::default();
    c.reset(&mut m);
    let mut actions = VecDeque::from([("CamelForth".to_string(), "NITROS9".to_string())]);
    if !level2 {
        actions.push_back(("Time ?".into(), "2026/10/31 12:00:00".into()));
        actions.push_back(("OS9:".into(), "echo traced".into()));
    }
    Ice::new(
        c,
        m,
        Options {
            actions,
            echo_console: false,
        },
    )
}

/// Run until the first F$Fork whose recent user calls mention `module`.
fn stop_at_fork(d: &mut Ice, api: &str, module: &str) {
    let ok = |d: &mut Ice, t: &str| assert_eq!(d.command("POST", t, b"").0, 200, "{t}");
    ok(d, &format!("{api}/break?code=F$Fork&when=return"));
    ok(d, "/api/run");
    for _ in 0..400 {
        d.run_steps(1_000_000);
        if !d.running {
            let (_, calls) = d.command("GET", &format!("{api}/calls?user=1&limit=20"), b"");
            if calls.contains(&format!(r#"モジュール\"{module}\""#)) {
                return;
            }
            ok(d, "/api/run");
        }
    }
    panic!("no fork of {module}");
}

fn dump(d: &mut Ice, api: &str, dir: &str, tag: &str) {
    for view in [
        "procs",
        "modules",
        "memmap",
        "calls?limit=100000",
        "services",
    ] {
        let (status, body) = d.command("GET", &format!("{api}/{view}"), b"");
        assert_eq!(status, 200, "{view}");
        let name = view.split('?').next().unwrap();
        fs::write(format!("{dir}/{tag}-{name}.json"), body).unwrap();
    }
    // The OS summary in the state, without the parts that are not OS views.
    let (_, state) = d.command("GET", "/api/state", b"");
    let os = state.find(r#""os":"#).unwrap();
    let end = state[os..].find("},").unwrap();
    fs::write(format!("{dir}/{tag}-state.json"), &state[os..os + end + 1]).unwrap();
    let (_, dis) = d.command(
        "GET",
        &format!("/api/disasm?addr={:04X}&back=8&n=24", d.cpu.pc),
        b"",
    );
    fs::write(format!("{dir}/{tag}-disasm.json"), dis).unwrap();
}

#[test]
#[ignore = "requires the reference kit and the Level 2 disk"]
fn snapshot_os_views() {
    let dir = std::env::var("SNAPSHOT_DIR").expect("SNAPSHOT_DIR");
    let api = std::env::var("API").unwrap_or("/api/os".into());
    fs::create_dir_all(&dir).unwrap();
    let mut d = boot(
        "reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img",
        false,
    );
    stop_at_fork(&mut d, &api, "echo");
    dump(&mut d, &api, &dir, "l1");
    let mut d = boot("artifacts/level2/multicomp09-level2.img", true);
    stop_at_fork(&mut d, &api, "echo");
    dump(&mut d, &api, &dir, "l2");
}

/// ICE speed with OS tracing at the Level 1 shell prompt (guest MHz).
#[test]
#[ignore = "requires the reference kit"]
fn ice_speed() {
    let mut d = boot(
        "reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img",
        false,
    );
    assert_eq!(d.command("POST", "/api/run", b"").0, 200);
    d.run_steps(20_000_000); // boot to the shell, then idle in the shell
    let mut best = 0.0f64;
    for _ in 0..5 {
        let (c0, t0) = (d.cpu.cycles, std::time::Instant::now());
        d.run_steps(10_000_000);
        best = best.max((d.cpu.cycles - c0) as f64 / t0.elapsed().as_secs_f64() / 1e6);
    }
    println!("ice_speed_mhz={best:.1}");
}
