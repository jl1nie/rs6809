//! The ICE in English (a test binary of its own: the language is one
//! setting for the process, and the other tests expect Japanese).
use rs6809::ice::json::Json;
use rs6809::ice::{Ice, Mc68000, Options};
use rs6809::m68k::machine::Sim68k;
use std::collections::VecDeque;

#[test]
fn english_text_from_the_ice_and_the_os_layers() {
    let mut m = Sim68k::default();
    m.set_vectors(0x8000, 0x1000);
    m.load(0x1000, &[0x4e, 0x71, 0x60, 0xfe]).unwrap(); // NOP ; BRA *
    let mut d = Ice::m68000(Mc68000::new(m), Options { actions: VecDeque::new(), echo_console: false });

    // Japanese by default; the API switches it.
    let st = Json::parse(&d.command("GET", "/api/state", b"").1).unwrap();
    assert_eq!(st.get("lang").str(), Some("ja"));
    let (status, body) = d.command("POST", "/api/lang?lang=en", b"");
    assert_eq!((status, body.as_str()), (200, r#"{"lang":"en"}"#));

    d.command("POST", "/api/step", b"");
    let st = Json::parse(&d.command("GET", "/api/state", b"").1).unwrap();
    assert_eq!(st.get("lang").str(), Some("en"));
    assert_eq!(st.get("stop").str(), Some("step done"));
    d.command("POST", "/api/bp?addr=1002", b"");
    d.t.cpu.pc = 0x1000;
    d.command("POST", "/api/run", b"");
    d.run_steps(10);
    let st = Json::parse(&d.command("GET", "/api/state", b"").1).unwrap();
    assert_eq!(st.get("stop").str(), Some("breakpoint $1002"));

    // The OS layers' explanations.
    assert!(rs6809::ice::os9::explain(0x03).starts_with("Allocates a new process descriptor"));
    let fuzix = rs6809::ice::fuzix::Fuzix::default();
    use rs6809::ice::osmon::OsMonitor;
    assert!(fuzix.explain(32).starts_with("Duplicates the process")); // _fork
    let cpm = rs6809::ice::cpm68k::Cpm68k::default();
    assert!(cpm.explain(15).starts_with("Looks for the file")); // F_OPEN
    assert_eq!(cpm.services().iter().find(|s| s.name == "F_OPEN").map(|s| s.desc.clone()), Some("Open file".into()));

    // Back to Japanese.
    d.command("POST", "/api/lang?lang=ja", b"");
    assert!(rs6809::ice::os9::explain(0x03).starts_with("新しいプロセス記述子"));
    let (status, _) = d.command("POST", "/api/lang?lang=fr", b"");
    assert_eq!(status, 400);
}
