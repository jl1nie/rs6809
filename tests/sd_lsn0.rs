use rs6809::{Cpu, Machine, cpu::Z};
use std::fs;

#[test]
#[ignore = "requires python3 scripts/build-c-disk.py"]
fn driver_lsn0_detection_does_not_wrap_nonzero_sector_numbers_to_zero() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/artifacts/ccompiler/sd-driver/mc09sdc.dr"
    );
    let driver = fs::read(path).unwrap();
    // Execute the actual driver's PSHS X; ORB ,S+; ORB ,S+ sequence.
    let sequence = [0x34, 0x10, 0xea, 0xe0, 0xea, 0xe0, 0x26];
    let offset = driver
        .windows(sequence.len())
        .position(|b| b == sequence)
        .expect("corrected driver LSN0 test");
    let mut machine = Machine::default();
    machine.load(0x1000, &driver[offset..offset + 6]).unwrap();
    let mut cpu = Cpu::default();
    for high in [0, 1, 255u8] {
        for low in 0..=u16::MAX {
            cpu.pc = 0x1000;
            cpu.s = 0x7000;
            cpu.b = high;
            cpu.x = low;
            for _ in 0..3 {
                cpu.step(&mut machine).unwrap();
            }
            assert_eq!(
                cpu.cc & Z != 0,
                high == 0 && low == 0,
                "LSN {high:02x}{low:04x}"
            );
            assert_eq!(cpu.s, 0x7000);
            assert_eq!(cpu.x, low);
        }
    }
}
