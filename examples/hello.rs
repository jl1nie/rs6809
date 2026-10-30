//! Run with `cargo run --example hello`.
use rs6809::{Bus, Cpu, Machine};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = Machine::default();
    // LDX #$1010; LDA ,X+; BEQ done; STA $8019; BRA loop; done: SYNC.
    machine.load(
        0x1000,
        &[
            0x8e, 0x10, 0x10, 0xa6, 0x80, 0x27, 0x05, 0xb7, 0x80, 0x19, 0x20, 0xf7, 0x13,
        ],
    )?;
    machine.load(0x1010, b"Hello, 6809!\r\n\0")?;
    machine.write16(0xfffe, 0x1000);
    let mut cpu = Cpu::default();
    cpu.reset(&mut machine);
    while cpu.pc != 0x100c {
        machine.step(&mut cpu)?;
    }
    print!("{}", String::from_utf8_lossy(&machine.acia.tx));
    Ok(())
}
