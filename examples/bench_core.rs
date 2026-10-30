//! Fixed guest workloads for comparing emulator implementations without host terminal I/O.
use rs6809::{Bus, Cpu, Machine};
use std::{env, fs::File, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "indexed-flat".into());
    let limit: u64 = args
        .next()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(100_000_000);
    let mut machine = match mode.as_str() {
        "indexed-flat" | "serial" => Machine::default(),
        "indexed-mmu" => {
            let mut m = Machine::multicomp(&[0; 8192], File::open("Cargo.toml")?)?;
            for slot in 0..8 {
                m.write(0xffde, slot);
                m.write(0xffdf, slot);
            }
            m.write(0xffde, 0xa0);
            m
        }
        _ => return Err("mode must be indexed-flat, indexed-mmu, or serial".into()),
    };
    // LDA ,X; ADDA #1; STA ,X; LEAX 1,X; CMPX #$3000; BNE loop;
    // LDX #$2000; BRA loop. All guest instructions are real 6809 code.
    let indexed = [
        0xa6, 0x84, 0x8b, 1, 0xa7, 0x84, 0x30, 1, 0x8c, 0x30, 0, 0x26, 0xf3, 0x8e, 0x20, 0, 0x20,
        0xee,
    ];
    // LDA $8018; BITA #2; BEQ loop; STA $8019; BRA loop.
    let serial = [
        0xb6, 0x80, 0x18, 0x85, 2, 0x27, 0xf9, 0xb7, 0x80, 0x19, 0x20, 0xf4,
    ];
    machine.load(0x1000, if mode == "serial" { &serial } else { &indexed })?;
    let mut cpu = Cpu::default();
    cpu.pc = 0x1000;
    cpu.x = 0x2000;
    cpu.s = 0x7000;
    let mut steps = 0u64;
    let mut tx_bytes = 0u64;
    let started = Instant::now();
    while cpu.cycles < limit {
        machine.step(&mut cpu)?;
        steps += 1;
        if steps & 1023 == 0 {
            tx_bytes += machine.acia.tx.len() as u64;
            machine.acia.tx.clear();
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    tx_bytes += machine.acia.tx.len() as u64;
    let mut checksum = 0xcbf29ce484222325u64;
    for b in machine.ram.iter() {
        checksum = (checksum ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    for b in [cpu.a, cpu.b, cpu.cc, cpu.dp]
        .into_iter()
        .chain(cpu.pc.to_be_bytes())
        .chain(cpu.x.to_be_bytes())
        .chain(cpu.y.to_be_bytes())
        .chain(cpu.u.to_be_bytes())
        .chain(cpu.s.to_be_bytes())
    {
        checksum = (checksum ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    println!(
        "{{\"mode\":\"{mode}\",\"cycles\":{},\"steps\":{steps},\"seconds\":{seconds:.9},\"checksum\":{checksum},\"tx_bytes\":{tx_bytes}}}",
        cpu.cycles
    );
    Ok(())
}
