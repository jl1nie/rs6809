use rs6809::{Cpu, Machine, ice, telnet::TelnetServer};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    sync::mpsc,
    thread,
    time::Instant,
};
fn number(s: &str) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(if let Some(s) = s.strip_prefix("0x") {
        u64::from_str_radix(s, 16)?
    } else {
        s.parse()?
    })
}
fn report(cycles: u64, started: Instant, measurement: Option<(u64, Instant)>) {
    eprintln!(
        "rs6809: total_cycles={cycles} host_seconds={:.6}",
        started.elapsed().as_secs_f64()
    );
    if let Some((first, start)) = measurement {
        let elapsed = cycles - first;
        eprintln!(
            "rs6809: measured_cycles={elapsed} host_seconds={:.6} seconds_at_1mhz={:.6}",
            start.elapsed().as_secs_f64(),
            elapsed as f64 / 1_000_000.0
        );
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1).peekable();
    let mut rom = None;
    let mut disk = None;
    let mut ram_kb = None;
    let mut acia_count = None;
    let mut telnet = None;
    let mut persist_disk = false;
    let mut volatile_disk = false;
    let mut bin = None;
    let mut addr: u64 = 0x1000;
    let mut pc: Option<u64> = None;
    let mut cpu_kind = String::from("6809");
    let mut bins: Vec<String> = Vec::new();
    let mut machine_kind: Option<String> = None;
    let mut boot_sector = false;
    let mut os_kind: Option<String> = None;
    let mut load_state: Option<String> = None;
    let mut tty2: Option<String> = None;
    let mut lang: Option<bool> = None;
    let mut max = 0;
    let mut script = None;
    let mut until = None;
    let mut boot = false;
    let mut stats = false;
    let mut measure_from = None;
    let mut ice_addr = None;
    let mut ice_halt = false;
    let mut dap_addr = None;
    let mut symbol_specs = Vec::new();
    let mut fpga: Option<String> = None;
    let mut fpga_baud: u32 = 115_200;
    let mut source_dirs = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--rom" => rom = Some(args.next().ok_or("missing ROM path")?),
            "--disk" => disk = Some(args.next().ok_or("missing disk path")?),
            "--acia-count" => {
                acia_count = Some(usize::try_from(number(
                    &args.next().ok_or("missing ACIA count")?,
                )?)?)
            }
            "--telnet" => {
                telnet = Some(if args.peek().is_some_and(|next| !next.starts_with("--")) {
                    args.next().unwrap()
                } else {
                    "127.0.0.1:0".to_string()
                });
            }
            "--persist-disk" => persist_disk = true,
            "--volatile-disk" => volatile_disk = true,
            "--ram-kb" => {
                ram_kb = Some(usize::try_from(number(
                    &args.next().ok_or("missing RAM size")?,
                )?)?)
            }
            "--bin" => {
                let path = args.next().ok_or("missing binary path")?;
                bins.push(path.clone());
                bin = Some(path)
            }
            "--machine" => machine_kind = Some(args.next().ok_or("missing machine")?),
            "--fpga" => fpga = Some(args.next().ok_or("--fpga requires a serial device or tcp:HOST:PORT")?),
            "--fpga-baud" => fpga_baud = args.next().ok_or("--fpga-baud requires a number")?.parse()?,
            "--boot-sector" => boot_sector = true,
            "--os" => os_kind = Some(args.next().ok_or("missing OS")?),
            "--load-state" => load_state = Some(args.next().ok_or("missing state file")?),
            "--lang" => match args.next().as_deref() {
                Some("ja") => lang = Some(false),
                Some("en") => lang = Some(true),
                _ => return Err("--lang takes ja or en".into()),
            },
            "--tty2" => {
                tty2 = Some(if args.peek().is_some_and(|next| !next.starts_with("--")) {
                    args.next().unwrap()
                } else {
                    "127.0.0.1:6812".to_string()
                })
            }
            "--load" => addr = number(&args.next().ok_or("missing load address")?)?,
            "--pc" => pc = Some(number(&args.next().ok_or("missing PC")?)?),
            "--cpu" => cpu_kind = args.next().ok_or("missing CPU")?,
            "--max-cycles" => max = number(&args.next().ok_or("missing cycle limit")?)?,
            "--script" => script = Some(args.next().ok_or("missing script path")?),
            "--until" => until = Some(args.next().ok_or("missing output marker")?),
            "--boot" => boot = true,
            "--ice" => {
                ice_addr = Some(if args.peek().is_some_and(|next| !next.starts_with("--")) {
                    args.next().unwrap()
                } else {
                    "127.0.0.1:6809".to_string()
                });
            }
            "--ice-halt" => ice_halt = true,
            "--dap" => {
                dap_addr = Some(if args.peek().is_some_and(|next| !next.starts_with("--")) {
                    args.next().unwrap()
                } else {
                    "127.0.0.1:6810".to_string()
                });
            }
            "--symbols" => symbol_specs.push(args.next().ok_or("missing symbols path")?),
            "--source-dir" => source_dirs.push(args.next().ok_or("missing source directory")?),
            "--stats" => stats = true,
            "--measure-from" => {
                measure_from = Some(args.next().ok_or("missing measurement marker")?);
                stats = true;
            }
            "--help" | "-h" => {
                println!(
                    "rs6809 [--cpu 6809|68000] [--bin FILE --load 0x1000 --pc 0x1000]\n       [--rom FILE --disk FILE --boot] [--ram-kb N] [--script FILE]\n       [--acia-count N] [--telnet [127.0.0.1:0]] [--persist-disk|--volatile-disk]\n       [--until TEXT] [--max-cycles N] [--stats] [--measure-from TEXT]\n       [--ice [127.0.0.1:6809]] [--ice-halt] [--dap [127.0.0.1:6810]]\n       [--symbols FILE|DIR[@MODULE]]... [--source-dir DIR]...\n       [--fpga DEVICE|tcp:HOST:PORT [--fpga-baud 115200]]\n\nDefault: 64 KiB RAM, ACIA $8018/$8019, timer $8010.\nROM+disk: 512 KiB RAM (select 64/128/256/512/1024 with --ram-kb).\nMulticomp console $FFD0/$FFD1, SD $FFD8-$FFDC, timer $FFDD.\n--acia-count selects 1..4 Multicomp ACIAs (default 1, or 4 with --telnet).\n--telnet binds localhost on a free TCP port by default; OS-9 opens the available ACIAs.\nOne client per active ACIA; host stdin is disabled. Use the multi-user Level 2 disk.\n--boot types NITROS9 at the CamelForth prompt.\nScripts: lines PROMPT<TAB>RESPONSE, matched sequentially; response ends with CR.\n  Lines starting with # are comments (68000: \\# starts a prompt with #,\n  and a ?PROMPT line is answered whenever PROMPT shows, out of order).\nTelnet disk writes persist by default; --persist-disk also enables this without Telnet.\n--volatile-disk keeps writes in RAM. Cycle limit without --until exits successfully.\n--stats reports guest cycles and host elapsed seconds to stderr.\n--measure-from starts an output-marker interval, ending at --until.\n--ice serves the debugger UI (registers, disassembly, breakpoints, OS-9 views)\n  over HTTP; open the printed URL. ACIA 1 becomes the browser console.\n--ice-halt starts the CPU stopped at the reset vector.\n--dap also serves the Debug Adapter Protocol (VS Code etc.); implies --ice.\n--symbols loads LWASM/RMA listings, LWLINK/c.link maps, GNU as listings,\n  nm output and CP/M-68K NM68 maps (repeatable).\n--source-dir adds a directory searched for source files.\n--fpga runs the ICE on the CPU of an rs6809-fpga machine (Tang Nano 20K)\n  over its USB serial port (or tcp:HOST:PORT, e.g. its simulation):\n  --cpu 6809 for the Multicomp09 machine, --cpu 68000 for Tiny68K (--os\n  fuzix by default). Breakpoints and watchpoints run in hardware (8 and 4).\n--lang ja|en sets the language of the ICE (default: Japanese if LANG says ja,\n  else English); the UI can switch it too.\n\n--cpu 68000 [--machine plain|tiny68k] (docs/m68k.md, docs/cpm68k.md):\n  plain: RAM from $000000 (1 MiB, --ram-kb up to 16320), console status $FF0000\n  (bit 0 input ready), data $FF0001. SSP = top of RAM.\n  tiny68k: 16 MiB RAM less 32 KiB (--ram-kb 8192: the 8 MiB of rs6809-fpga's\n  Tang Nano 20K machine), 68681 DUART $FFF000, IDE $FFE000 (--disk,\n  --persist-disk); starts in supervisor state at --pc without a monitor ROM;\n  --boot-sector loads disk LBA 1 at $15000 and starts there (FUZIX loader);\n  --os cpm68k|fuzix|none picks the OS monitor (default cpm68k); FUZIX needs\n  the kernel symbols (--symbols fuzix.sym).\n  --tty2 [127.0.0.1:6812] puts DUART channel B (FUZIX /dev/tty2) on TCP.\n  --load-state FILE continues from a state saved in the ICE (button or\n  /api/machine/save); the ICE's reset still returns to the boot.\n  --bin (repeatable) loads a raw binary at --load, or Motorola S-records.\n  Without --pc the start is the S-record entry, else the load address\n  (a raw image loaded at 0 keeps its own reset vectors).\n  Works with --ice, --ice-halt, --dap, --symbols, --script, --until, --max-cycles, --stats."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {a}").into()),
        }
    }
    // The ICE's text: --lang, else Japanese only when the environment asks.
    ice::lang::set_en(lang.unwrap_or_else(ice::lang::english_from_env));
    if let Some(spec) = fpga {
        if rom.is_some() || disk.is_some() || !bins.is_empty() || bin.is_some() || telnet.is_some() || until.is_some() || max != 0 {
            return Err("--fpga runs the machine on the board: no --rom, --disk, --bin, --telnet, --until or --max-cycles".into());
        }
        let mut actions = std::collections::VecDeque::<(String, String)>::new();
        if boot {
            actions.push_back(("CamelForth".into(), "NITROS9".into()));
        }
        actions.extend(script_actions(&script)?);
        return run_fpga(FpgaArgs {
            cpu: cpu_kind,
            spec,
            baud: fpga_baud,
            os: os_kind,
            actions,
            ice_addr: ice_addr.unwrap_or_else(|| "127.0.0.1:6809".into()),
            ice_halt,
            dap_addr,
            symbol_specs,
            source_dirs,
        });
    }
    match cpu_kind.as_str() {
        "6809" => {}
        "68000" | "68k" => {
            if rom.is_some() || telnet.is_some() || boot || measure_from.is_some() || acia_count.is_some() {
                return Err("--rom, --telnet, --boot, --acia-count and --measure-from are for the 6809".into());
            }
            return run68k(M68kArgs {
                machine: machine_kind.unwrap_or_else(|| "plain".into()),
                boot_sector,
                os: os_kind,
                load_state,
                tty2,
                bins,
                disk,
                persist_disk,
                volatile_disk,
                script,
                addr,
                pc,
                ram_kb,
                max,
                until,
                stats,
                ice_addr,
                ice_halt,
                dap_addr,
                symbol_specs,
                source_dirs,
            });
        }
        other => return Err(format!("unknown CPU: {other} (6809 or 68000)").into()),
    }
    let addr = u16::try_from(addr)?;
    let pc = pc.map(u16::try_from).transpose()?;
    if measure_from.as_ref().is_some_and(|s| s.is_empty())
        || (measure_from.is_some() && until.is_none())
    {
        return Err("--measure-from requires a nonempty marker and --until".into());
    }
    if persist_disk && volatile_disk {
        return Err("--persist-disk and --volatile-disk conflict".into());
    }
    persist_disk = (persist_disk || telnet.is_some()) && !volatile_disk;
    if persist_disk && disk.is_none() {
        return Err("persistent writes require --disk".into());
    }
    let mut machine = match (rom, disk) {
        (Some(r), Some(d)) => Machine::multicomp_with_ram(
            &fs::read(r)?,
            if persist_disk {
                OpenOptions::new().read(true).write(true).open(d)?
            } else {
                File::open(d)?
            },
            ram_kb
                .unwrap_or(512)
                .checked_mul(1024)
                .ok_or("RAM size overflow")?,
        )?,
        (None, None) => {
            if ram_kb.is_some() {
                return Err("--ram-kb requires --rom and --disk".into());
            }
            Machine::default()
        }
        _ => return Err("--rom and --disk must be specified together".into()),
    };
    if persist_disk {
        machine.persist_disk_writes()?;
    }
    if acia_count.is_some() || telnet.is_some() {
        machine.set_acia_count(acia_count.unwrap_or(if telnet.is_some() { 4 } else { 1 }))?;
    }
    let mut server = telnet
        .map(|address| TelnetServer::bind(&address, &mut machine))
        .transpose()?;
    let mut announced = false;
    let mut console_private = false;
    let mut cpu = Cpu::default();
    cpu.reset(&mut machine);
    if let Some(path) = bin {
        machine.load(addr, &fs::read(path)?)?;
        cpu.pc = pc.unwrap_or(addr)
    } else if let Some(pc) = pc {
        cpu.pc = pc
    }
    if machine_kind.is_some() || bins.len() > 1 || boot_sector || os_kind.is_some() || load_state.is_some() || tty2.is_some() {
        return Err("--machine and several --bin are for --cpu 68000".into());
    }
    let mut actions = std::collections::VecDeque::<(String, String)>::new();
    if boot {
        actions.push_back(("CamelForth".into(), "NITROS9".into()));
    }
    if let Some(path) = script {
        for line in fs::read_to_string(path)?.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (prompt, response) = line
                .split_once('\t')
                .ok_or("script requires PROMPT<TAB>RESPONSE")?;
            if prompt.is_empty() {
                return Err("empty script prompt".into());
            }
            actions.push_back((prompt.into(), response.into()));
        }
    }
    if ice_addr.is_none() && (dap_addr.is_some() || !symbol_specs.is_empty()) {
        ice_addr = Some("127.0.0.1:6809".to_string());
    }
    if let Some(address) = ice_addr {
        if until.is_some() || max != 0 || measure_from.is_some() {
            return Err("--ice cannot be combined with --until, --max-cycles or --measure-from".into());
        }
        let listener = std::net::TcpListener::bind(&address)?;
        let mut debugger = ice::Ice::new(
            cpu,
            machine,
            ice::Options {
                actions,
                echo_console: server.is_none(),
            },
        );
        if let Some(server) = server {
            eprintln!("rs6809: Telnet listening on {}", server.local_addr()?);
            debugger.set_telnet(server);
        }
        for dir in source_dirs {
            debugger.symbols.add_source_dir(dir.into());
        }
        for spec in &symbol_specs {
            let loaded = debugger.load_symbols(spec).map_err(|e| format!("--symbols: {e}"))?;
            for i in loaded {
                let t = &debugger.symbols.tables[i];
                eprintln!(
                    "rs6809: symbols {} ({}, {} symbols, {} lines{})",
                    t.path.display(),
                    t.kind,
                    t.symbols.len(),
                    t.lines.len(),
                    t.module.as_ref().map_or(String::new(), |m| format!(", module {m}"))
                );
            }
        }
        eprintln!("rs6809: ICE UI at http://{}/", listener.local_addr()?);
        let dap = match dap_addr {
            Some(a) => {
                let l = std::net::TcpListener::bind(&a)?;
                eprintln!("rs6809: DAP listening on {}", l.local_addr()?);
                Some(l)
            }
            None => None,
        };
        return Ok(debugger.serve(listener, dap, !ice_halt)?);
    }
    let (tx, rx) = mpsc::sync_channel(1024);
    if server.is_none() {
        thread::spawn(move || {
            let stdin = io::stdin();
            for byte in stdin.lock().bytes() {
                match byte {
                    Ok(b) => {
                        if tx.send(b).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    let mut out = io::stdout().lock();
    let mut seen = String::new();
    let mut pending = std::collections::VecDeque::new();
    let mut next_input = 0;
    let mut blocks = 0;
    let started = Instant::now();
    let mut measurement = None;
    loop {
        machine.step(&mut cpu)?;
        if let Some(e) = machine.disk_error() {
            return Err(e.into());
        }
        if !machine.acia.tx.is_empty() {
            let bytes = std::mem::take(&mut machine.acia.tx);
            if let Some(server) = &mut server {
                server.send(0, &bytes, &mut machine);
            }
            // Once ACIA 1 offers logins it belongs to a remote user; only the
            // boot log before that reaches the host console.
            if !console_private {
                out.write_all(&bytes)?;
                out.flush()?;
            }
            for b in bytes {
                seen.push(char::from(b));
            }
            if measurement.is_none() && measure_from.as_ref().is_some_and(|s| seen.contains(s)) {
                measurement = Some((cpu.cycles, Instant::now()));
            }
            if until.as_ref().is_some_and(|s| seen.contains(s)) {
                if stats {
                    report(cpu.cycles, started, measurement);
                }
                if measure_from.is_some() && measurement.is_none() {
                    return Err("stop marker reached before measurement marker".into());
                }
                return Ok(());
            }
            if let Some((prompt, _)) = actions.front()
                && seen.contains(prompt)
            {
                let (_, response) = actions.pop_front().unwrap();
                pending.extend(response.bytes());
                pending.push_back(13);
                seen.clear();
                next_input = cpu.cycles + 100000;
            }
            if seen.len() > 65536 {
                let cut = seen.char_indices().nth(32768).map(|(i, _)| i).unwrap_or(0);
                seen.drain(..cut);
            }
        }
        if cpu.cycles >= next_input
            && machine.acia.rx.is_empty()
            && let Some(b) = pending.pop_front()
        {
            machine.acia.rx.push_back(b);
            next_input = cpu.cycles + 30000;
        }
        blocks += 1;
        if blocks == 1024 {
            blocks = 0;
            if let Some(server) = &mut server {
                server.poll(&mut machine)?;
                console_private |= machine.serial(0).unwrap().session_ready();
                if !announced
                    && (0..machine.acia_count()).any(|i| machine.serial(i).unwrap().session_ready())
                {
                    eprintln!(
                        "rs6809: Telnet listening on {} ({} ACIA hardware ports; OS-9 controls active sessions)",
                        server.local_addr()?,
                        machine.acia_count()
                    );
                    announced = true;
                }
            }
            if pending.is_empty() {
                while let Ok(b) = rx.try_recv() {
                    machine.acia.rx.push_back(match b {
                        10 => 13,
                        127 => 8,
                        _ => b,
                    })
                }
            }
        }
        if max != 0 && cpu.cycles >= max {
            if stats {
                report(cpu.cycles, started, measurement);
            }
            if until.is_some() {
                return Err(
                    format!("cycle limit reached before output marker; CPU {cpu:?}").into(),
                );
            }
            return Ok(());
        }
    }
}
struct M68kArgs {
    machine: String,
    boot_sector: bool,
    os: Option<String>,
    load_state: Option<String>,
    tty2: Option<String>,
    bins: Vec<String>,
    disk: Option<String>,
    persist_disk: bool,
    volatile_disk: bool,
    script: Option<String>,
    addr: u64,
    pc: Option<u64>,
    ram_kb: Option<usize>,
    max: u64,
    until: Option<String>,
    stats: bool,
    ice_addr: Option<String>,
    ice_halt: bool,
    dap_addr: Option<String>,
    symbol_specs: Vec<String>,
    source_dirs: Vec<String>,
}

type Loader<'a> = dyn FnMut(u32, &[u8]) -> Result<(), String> + 'a;

/// Load each --bin (S-records, or raw at the load address) through `load`;
/// returns the start address they give.
fn load_images(
    bins: &[String],
    addr: u32,
    load: &mut Loader,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let mut start = None;
    for path in bins {
        let data = fs::read(path)?;
        if data.first() == Some(&b'S') && data.get(1).is_some_and(|c| c.is_ascii_digit()) {
            let text = String::from_utf8_lossy(&data);
            let (chunks, entry) = rs6809::m68k::machine::parse_srec(&text).map_err(|e| format!("{path}: {e}"))?;
            for (a, bytes) in chunks {
                load(a, &bytes).map_err(|e| format!("{path}: {e}"))?;
            }
            start = start.or(entry);
        } else {
            load(addr, &data).map_err(|e| format!("{path}: {e}"))?;
            if addr != 0 {
                start = start.or(Some(addr));
            }
        }
    }
    Ok(start)
}

fn script_actions(path: &Option<String>) -> Result<std::collections::VecDeque<(String, String)>, Box<dyn std::error::Error>> {
    let mut actions = std::collections::VecDeque::new();
    if let Some(path) = path {
        for line in fs::read_to_string(path)?.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (prompt, response) = line.split_once('\t').ok_or("script requires PROMPT<TAB>RESPONSE")?;
            // `\#` starts a prompt with `#` (a shell prompt), not a comment.
            let prompt = prompt.strip_prefix('\\').filter(|p| p.starts_with('#')).unwrap_or(prompt);
            if prompt.is_empty() {
                return Err("empty script prompt".into());
            }
            actions.push_back((prompt.into(), response.into()));
        }
    }
    Ok(actions)
}

struct FpgaArgs {
    cpu: String,
    spec: String,
    baud: u32,
    os: Option<String>,
    actions: std::collections::VecDeque<(String, String)>,
    ice_addr: String,
    ice_halt: bool,
    dap_addr: Option<String>,
    symbol_specs: Vec<String>,
    source_dirs: Vec<String>,
}

/// The ICE on the machine of rs6809-fpga (Tang Nano 20K): the CPU runs on
/// the board, the ICE talks to it over the serial port.
fn run_fpga(a: FpgaArgs) -> Result<(), Box<dyn std::error::Error>> {
    let options = ice::Options {
        actions: a.actions,
        echo_console: true,
    };
    let listener = std::net::TcpListener::bind(&a.ice_addr)?;
    let dap = match &a.dap_addr {
        Some(d) => Some(std::net::TcpListener::bind(d)?),
        None => None,
    };
    eprintln!("rs6809: connecting to the FPGA on {}", a.spec);
    macro_rules! go {
        ($t:expr, $os:expr) => {{
            let mut debugger = ice::Ice::for_target($t, options, $os);
            for dir in a.source_dirs {
                debugger.symbols.add_source_dir(dir.into());
            }
            for spec in &a.symbol_specs {
                debugger.load_symbols(spec).map_err(|e| format!("--symbols: {e}"))?;
            }
            eprintln!("rs6809: ICE UI at http://{}/ (CPU on the FPGA)", listener.local_addr()?);
            if let Some(l) = &dap {
                eprintln!("rs6809: DAP listening on {}", l.local_addr()?);
            }
            return Ok(debugger.serve(listener, dap, !a.ice_halt)?);
        }};
    }
    match a.cpu.as_str() {
        "6809" => {
            let t = ice::fpga::Fpga6809::connect(&a.spec, a.baud)?;
            go!(t, Box::new(ice::os9::Nitros9::default()))
        }
        "68000" | "68k" => {
            let t = ice::fpga::Fpga68k::connect(&a.spec, a.baud)?;
            let os: Box<dyn ice::osmon::OsMonitor> = match a.os.as_deref().unwrap_or("fuzix") {
                "cpm68k" => Box::new(ice::cpm68k::Cpm68k::default()),
                "fuzix" => Box::new(ice::fuzix::Fuzix::default()),
                "none" => Box::new(ice::osmon::NoOs),
                other => return Err(format!("unknown OS: {other} (cpm68k, fuzix or none)").into()),
            };
            go!(t, os)
        }
        other => Err(format!("unknown CPU: {other} (6809 or 68000)").into()),
    }
}

/// Where the Tiny68K monitor loads a boot sector (the CP/M boot loader).
const BOOT_SECTOR: u32 = 0x15000;

/// A 68000 machine: the plain one (RAM + console port) or Tiny68K.
fn run68k(a: M68kArgs) -> Result<(), Box<dyn std::error::Error>> {
    use rs6809::m68k::{machine::Sim68k, tiny68k::Tiny68k};
    let addr = u32::try_from(a.addr)?;
    let pc = a.pc.map(u32::try_from).transpose()?;
    match a.machine.as_str() {
        "plain" => {
            if a.disk.is_some() || a.tty2.is_some() {
                return Err("--disk and --tty2 need --machine tiny68k".into());
            }
            let kb = a.ram_kb.unwrap_or(1024);
            if kb == 0 || kb > 0xff_0000 / 1024 {
                return Err("--ram-kb for the 68000 is 1..16320".into());
            }
            let mut m = Sim68k::with_ram(kb * 1024);
            let start = load_images(&a.bins, addr, &mut |at, d| m.load(at, d))?;
            if let Some(pc) = pc.or(start) {
                m.set_vectors(m.ram.len() as u32 & !1, pc);
            }
            if a.os.is_some() {
                return Err("--os needs --machine tiny68k".into());
            }
            run_board(ice::Mc68000::new(m), Box::new(ice::osmon::NoOs), a)
        }
        "tiny68k" => {
            // 16 MiB less 32 KiB by default; --ram-kb 8192 is the Tang Nano
            // 20K machine (rs6809-fpga).
            let kb = a.ram_kb.unwrap_or(rs6809::m68k::tiny68k::RAM_END as usize / 1024);
            if kb == 0 || kb > rs6809::m68k::tiny68k::RAM_END as usize / 1024 {
                return Err("--ram-kb for Tiny68K is 1..16352".into());
            }
            if a.persist_disk && a.volatile_disk {
                return Err("--persist-disk and --volatile-disk conflict".into());
            }
            let mut m = Tiny68k::with_ram(kb * 1024);
            if let Some(addr) = &a.tty2 {
                let port = rs6809::m68k::serial::TcpPort::bind(addr.as_str())?;
                eprintln!("rs6809: tty2 (DUART channel B) on {} (telnet or nc)", port.local_addr()?);
                m.tty2 = Some(port);
            }
            if let Some(path) = &a.disk {
                let data = fs::read(path)?;
                let file = if a.persist_disk {
                    Some(OpenOptions::new().write(true).open(path)?)
                } else {
                    None
                };
                m.ide.attach(data, file);
            } else if a.persist_disk {
                return Err("persistent writes require --disk".into());
            }
            let mut start = load_images(&a.bins, addr, &mut |at, d| m.load(at, d))?;
            if a.boot_sector {
                // What the monitor's boot command does: LBA 1 to $15000, run
                // it. The disk holds byte-swapped words, as the bus reads them.
                let sector = m.ide.disk.get(512..1024).ok_or("--boot-sector needs a --disk")?;
                let code: Vec<u8> = sector.chunks(2).flat_map(|w| [w[1], w[0]]).collect();
                m.load(BOOT_SECTOR, &code)?;
                start = start.or(Some(BOOT_SECTOR));
            }
            // No monitor ROM: start the loaded program in supervisor state.
            let t = match pc.or(start) {
                Some(pc) => {
                    let ssp = m.ram_end();
                    ice::Mc68000::start(m, pc, ssp)
                }
                None => ice::Mc68000::new(m),
            };
            // The OS monitor stays idle until it finds its system in memory.
            let os: Box<dyn ice::osmon::OsMonitor> = match a.os.as_deref().unwrap_or("cpm68k") {
                "cpm68k" => Box::new(ice::cpm68k::Cpm68k::default()),
                "fuzix" => Box::new(ice::fuzix::Fuzix::default()),
                "none" => Box::new(ice::osmon::NoOs),
                other => return Err(format!("unknown OS: {other} (cpm68k, fuzix or none)").into()),
            };
            run_board(t, os, a)
        }
        other => Err(format!("unknown 68000 machine: {other} (plain or tiny68k)").into()),
    }
}

/// Run a 68000 machine: the ICE, or the console on stdin/stdout.
fn run_board<B: ice::Board68k + 'static>(
    mut t: ice::Mc68000<B>,
    os: Box<dyn ice::osmon::OsMonitor>,
    a: M68kArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut actions = script_actions(&a.script)?;
    // `?PROMPT` lines are answered whenever the prompt shows (the ICE splits
    // them itself).
    let optional: Vec<(String, String)> = actions
        .iter()
        .filter_map(|(p, r)| p.strip_prefix('?').map(|p| (p.to_string(), r.clone())))
        .collect();
    if let (Some(path), true) = (&a.load_state, a.ice_addr.is_none() && a.dap_addr.is_none() && a.symbol_specs.is_empty()) {
        use rs6809::ice::target::Target;
        t.load_state(&fs::read(path).map_err(|e| format!("{path}: {e}"))?)?;
    }
    let mut ice_addr = a.ice_addr;
    if ice_addr.is_none() && (a.dap_addr.is_some() || !a.symbol_specs.is_empty()) {
        ice_addr = Some("127.0.0.1:6809".to_string());
    }
    if let Some(address) = ice_addr {
        if a.until.is_some() || a.max != 0 {
            return Err("--ice cannot be combined with --until or --max-cycles".into());
        }
        let listener = std::net::TcpListener::bind(&address)?;
        let mut debugger = ice::Ice::for_target(
            t,
            ice::Options {
                actions,
                echo_console: true,
            },
            os,
        );
        if let Some(path) = &a.load_state {
            debugger.load_machine(path)?;
        }
        for dir in a.source_dirs {
            debugger.symbols.add_source_dir(dir.into());
        }
        for spec in &a.symbol_specs {
            debugger.load_symbols(spec).map_err(|e| format!("--symbols: {e}"))?;
        }
        eprintln!("rs6809: 68000 ICE UI at http://{}/", listener.local_addr()?);
        let dap = match a.dap_addr {
            Some(d) => {
                let l = std::net::TcpListener::bind(&d)?;
                eprintln!("rs6809: DAP listening on {}", l.local_addr()?);
                Some(l)
            }
            None => None,
        };
        return Ok(debugger.serve(listener, dap, !a.ice_halt)?);
    }
    actions.retain(|(p, _)| !p.starts_with('?'));
    let (tx, rx) = mpsc::sync_channel(1024);
    thread::spawn(move || {
        for byte in io::stdin().lock().bytes() {
            match byte {
                Ok(b) if tx.send(b).is_ok() => {}
                _ => break,
            }
        }
    });
    let mut out = io::stdout().lock();
    let mut seen = String::new();
    let mut pending = std::collections::VecDeque::new();
    let mut next_input = 0u64;
    let started = Instant::now();
    let mut blocks = 0u32;
    // Batch runs (--until, --max-cycles) skip waits at full speed; an
    // interactive console lets them pass in real time.
    let batch = a.until.is_some() || a.max != 0;
    let hz = rs6809::m68k::tiny68k::CPU_HZ;
    loop {
        if let Some(idle) = t.idle() {
            // STOP with nothing pending: jump to the next device event or
            // scripted input, and in real time take host input meanwhile.
            let mut n = idle;
            if !pending.is_empty() {
                n = n.min(next_input.saturating_sub(t.cpu.cycles).max(1));
            }
            if !batch {
                n = n.min(hz / 200); // 5 ms slices, to notice typing
                t.machine.poll_io();
                if pending.is_empty() && actions.is_empty() {
                    while let Ok(b) = rx.try_recv() {
                        t.machine.console_put(if b == 10 { 13 } else { b });
                    }
                }
                if t.idle().is_some() {
                    thread::sleep(std::time::Duration::from_secs_f64(n as f64 / hz as f64));
                }
            }
            if t.idle().is_some() {
                // At most a second of guest time per jump (no event: wait).
                n = n.min(hz);
                if a.max != 0 {
                    n = n.min(a.max.saturating_sub(t.cpu.cycles).max(1));
                }
                t.skip(n);
            }
        } else {
            t.run_one();
        }
        if t.cpu.halted {
            return Err(format!("double fault: CPU halted at ${:06X}", t.cpu.pc).into());
        }
        if let Some(e) = t.machine.disk_error() {
            return Err(e.into());
        }
        let bytes = t.machine.console_take();
        if !bytes.is_empty() {
            out.write_all(&bytes)?;
            out.flush()?;
            seen.extend(bytes.iter().map(|&b| char::from(b)));
            if a.until.as_ref().is_some_and(|m| seen.contains(m.as_str())) {
                if a.stats {
                    report(t.cpu.cycles, started, None);
                }
                return Ok(());
            }
            if let Some((prompt, _)) = actions.front()
                && seen.contains(prompt.as_str())
            {
                let (_, response) = actions.pop_front().unwrap();
                pending.extend(response.bytes());
                pending.push_back(13);
                seen.clear();
                next_input = t.cpu.cycles + 100_000;
            } else if let Some((_, response)) = optional.iter().find(|(p, _)| seen.contains(p.as_str())) {
                pending.extend(response.bytes());
                pending.push_back(13);
                seen.clear();
                next_input = t.cpu.cycles + 100_000;
            }
            if seen.len() > 65536 {
                let cut = seen.char_indices().nth(32768).map_or(0, |(i, _)| i);
                seen.drain(..cut);
            }
        }
        if t.cpu.cycles >= next_input
            && t.machine.console_ready()
            && let Some(b) = pending.pop_front()
        {
            t.machine.console_put(b);
            next_input = t.cpu.cycles + 30_000;
        }
        blocks += 1;
        if blocks == 1024 {
            blocks = 0;
            t.machine.poll_io();
            if pending.is_empty() && actions.is_empty() {
                while let Ok(b) = rx.try_recv() {
                    t.machine.console_put(if b == 10 { 13 } else { b });
                }
            }
        }
        if a.max != 0 && t.cpu.cycles >= a.max {
            if a.stats {
                report(t.cpu.cycles, started, None);
            }
            if a.until.is_some() {
                return Err("cycle limit reached before output marker".into());
            }
            return Ok(());
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("rs6809: {e}");
        std::process::exit(1)
    }
}
