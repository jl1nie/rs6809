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
            "--bin" => bin = Some(args.next().ok_or("missing binary path")?),
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
                    "rs6809 [--cpu 6809|68000] [--bin FILE --load 0x1000 --pc 0x1000]\n       [--rom FILE --disk FILE --boot] [--ram-kb N] [--script FILE]\n       [--acia-count N] [--telnet [127.0.0.1:0]] [--persist-disk|--volatile-disk]\n       [--until TEXT] [--max-cycles N] [--stats] [--measure-from TEXT]\n       [--ice [127.0.0.1:6809]] [--ice-halt] [--dap [127.0.0.1:6810]]\n       [--symbols FILE|DIR[@MODULE]]... [--source-dir DIR]...\n\nDefault: 64 KiB RAM, ACIA $8018/$8019, timer $8010.\nROM+disk: 512 KiB RAM (select 64/128/256/512/1024 with --ram-kb).\nMulticomp console $FFD0/$FFD1, SD $FFD8-$FFDC, timer $FFDD.\n--acia-count selects 1..4 Multicomp ACIAs (default 1, or 4 with --telnet).\n--telnet binds localhost on a free TCP port by default; OS-9 opens the available ACIAs.\nOne client per active ACIA; host stdin is disabled. Use the multi-user Level 2 disk.\n--boot types NITROS9 at the CamelForth prompt.\nScripts: lines PROMPT<TAB>RESPONSE, matched sequentially; response ends with CR.\nTelnet disk writes persist by default; --persist-disk also enables this without Telnet.\n--volatile-disk keeps writes in RAM. Cycle limit without --until exits successfully.\n--stats reports guest cycles and host elapsed seconds to stderr.\n--measure-from starts an output-marker interval, ending at --until.\n--ice serves the debugger UI (registers, disassembly, breakpoints, OS-9 views)\n  over HTTP; open the printed URL. ACIA 1 becomes the browser console.\n--ice-halt starts the CPU stopped at the reset vector.\n--dap also serves the Debug Adapter Protocol (VS Code etc.); implies --ice.\n--symbols loads LWASM/RMA listings and LWLINK/c.link maps (repeatable).\n--source-dir adds a directory searched for source files.\n\n--cpu 68000: a plain 68000 machine (docs/m68k.md): RAM from $000000 (1 MiB,\n  --ram-kb up to 16320), console status $FF0000 (bit 0 input ready), data $FF0001.\n  --bin loads a raw binary at --load, or Motorola S-records (S0..S9 text).\n  Without --pc the start is the S-record entry, else the load address\n  (a raw image loaded at 0 keeps its own reset vectors). SSP = top of RAM.\n  Works with --ice, --ice-halt, --dap, --symbols, --until, --max-cycles, --stats."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {a}").into()),
        }
    }
    match cpu_kind.as_str() {
        "6809" => {}
        "68000" | "68k" => {
            if rom.is_some() || disk.is_some() || telnet.is_some() || boot || script.is_some() || measure_from.is_some() {
                return Err("--cpu 68000 takes --bin, --load, --pc, --ram-kb, --ice, --dap, --symbols, --until, --max-cycles, --stats".into());
            }
            return run68k(M68kArgs {
                bin,
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
    bin: Option<String>,
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

/// The plain 68000 machine: a program, the console on stdin/stdout, or the ICE.
fn run68k(a: M68kArgs) -> Result<(), Box<dyn std::error::Error>> {
    use rs6809::m68k::machine::Sim68k;
    let kb = a.ram_kb.unwrap_or(1024);
    if kb == 0 || kb > 0xff_0000 / 1024 {
        return Err("--ram-kb for the 68000 is 1..16320".into());
    }
    let mut m = Sim68k::with_ram(kb * 1024);
    let addr = u32::try_from(a.addr)?;
    let mut start = a.pc.map(u32::try_from).transpose()?;
    if let Some(path) = &a.bin {
        let data = fs::read(path)?;
        if data.first() == Some(&b'S') && data.get(1).is_some_and(|c| c.is_ascii_digit()) {
            let entry = m.load_srec(std::str::from_utf8(&data).map_err(|_| "S-record is not text")?)?;
            start = start.or(entry);
        } else {
            m.load(addr, &data)?;
            if addr != 0 {
                start = start.or(Some(addr));
            }
        }
    }
    if let Some(pc) = start {
        m.set_vectors(m.ram.len() as u32 & !1, pc);
    }
    let mut t = ice::Mc68000::new(m);
    let mut ice_addr = a.ice_addr;
    if ice_addr.is_none() && (a.dap_addr.is_some() || !a.symbol_specs.is_empty()) {
        ice_addr = Some("127.0.0.1:6809".to_string());
    }
    if let Some(address) = ice_addr {
        if a.until.is_some() || a.max != 0 {
            return Err("--ice cannot be combined with --until or --max-cycles".into());
        }
        let listener = std::net::TcpListener::bind(&address)?;
        let mut debugger = ice::Ice::m68000(
            t,
            ice::Options {
                actions: Default::default(),
                echo_console: true,
            },
        );
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
    let started = Instant::now();
    let mut blocks = 0u32;
    loop {
        t.cpu.step(&mut t.machine);
        if t.cpu.halted {
            return Err(format!("double fault: CPU halted at ${:06X}", t.cpu.pc).into());
        }
        if !t.machine.tx.is_empty() {
            let bytes = std::mem::take(&mut t.machine.tx);
            out.write_all(&bytes)?;
            out.flush()?;
            if let Some(marker) = &a.until {
                seen.extend(bytes.iter().map(|&b| char::from(b)));
                if seen.contains(marker.as_str()) {
                    if a.stats {
                        report(t.cpu.cycles, started, None);
                    }
                    return Ok(());
                }
                if seen.len() > 65536 {
                    let cut = seen.char_indices().nth(32768).map_or(0, |(i, _)| i);
                    seen.drain(..cut);
                }
            }
        }
        blocks += 1;
        if blocks == 1024 {
            blocks = 0;
            while let Ok(b) = rx.try_recv() {
                t.machine.rx.push_back(if b == 10 { 13 } else { b });
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
