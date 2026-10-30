use rs6809::cpu::{C, E, F, H, I, N, V, Z};
use rs6809::{Bus, Cpu, Machine};
fn program(code: &[u8]) -> (Cpu, Machine) {
    let mut m = Machine::default();
    m.load(0x1000, code).unwrap();
    let mut c = Cpu::default();
    c.pc = 0x1000;
    c.s = 0x7000;
    (c, m)
}
#[test]
fn arithmetic_flags() {
    let (mut c, mut m) = program(&[0x86, 0x7f, 0x8b, 1, 0x80, 1, 0x8b, 0x81, 0x89, 0xff]);
    c.step(&mut m).unwrap();
    c.step(&mut m).unwrap();
    assert_eq!(c.a, 0x80);
    assert_eq!(c.cc & (H | N | Z | V | C), H | N | V);
    c.step(&mut m).unwrap();
    assert_eq!(c.a, 0x7f);
    assert_eq!(c.cc & (N | Z | V | C), V);
    c.step(&mut m).unwrap();
    assert_eq!(c.a, 0);
    assert_eq!(c.cc & (N | Z | V | C), Z | C);
    c.step(&mut m).unwrap();
    assert_eq!(c.a, 0);
    assert_eq!(c.cc & (N | Z | V | C), Z | C);
}
#[test]
fn indexed_and_big_endian_wrap() {
    let (mut c, mut m) = program(&[
        0x8e, 0x20, 0x00, 0xa6, 0x1f, 0xa6, 0x81, 0xa6, 0x9f, 0x30, 0x00,
    ]);
    m.ram[0x1fff] = 0x42;
    m.ram[0x2000] = 0x55;
    m.write16(0x3000, 0x4000);
    m.ram[0x4000] = 0x66;
    c.step(&mut m).unwrap();
    c.step(&mut m).unwrap();
    assert_eq!(c.a, 0x42);
    c.step(&mut m).unwrap();
    assert_eq!((c.a, c.x), (0x55, 0x2002));
    c.step(&mut m).unwrap();
    assert_eq!(c.a, 0x66);
    m.write16(0xffff, 0x1234);
    assert_eq!(m.ram[0], 0x34);
    assert_eq!(m.read16(0xffff), 0x1234);
}
#[test]
fn swi2_rti_full_frame() {
    let (mut c, mut m) = program(&[0x10, 0x3f]);
    m.write16(0xfff4, 0x2000);
    m.ram[0x2000] = 0x3b;
    c.a = 12;
    c.b = 34;
    c.x = 0x1234;
    c.u = 0x4567;
    c.y = 0x5678;
    c.dp = 0x42;
    c.cc = C;
    let before = c.clone();
    assert_eq!(c.step(&mut m).unwrap(), 20);
    assert_eq!(c.pc, 0x2000);
    assert_eq!(c.s, before.s - 12);
    assert_eq!(c.cc, C | E);
    c.a = 0;
    c.x = 0;
    c.step(&mut m).unwrap();
    assert_eq!(
        (c.pc, c.s, c.a, c.x, c.cc),
        (0x1002, before.s, 12, 0x1234, C | E)
    );
}
#[test]
fn cwai_stacks_once_and_irq_wakes_sync() {
    let (mut c, mut m) = program(&[0x3c, 0xef]);
    m.write16(0xfff8, 0x2000);
    m.ram[0x2000] = 0x3b;
    c.cc = I;
    c.step(&mut m).unwrap();
    let sp = c.s;
    assert!(c.interrupt(&mut m, true, false, false));
    assert_eq!(c.s, sp);
    c.step(&mut m).unwrap();
    assert_eq!(c.s, 0x7000);
    let (mut c, mut m) = program(&[0x13, 0x12]);
    c.cc = I | F;
    c.step(&mut m).unwrap();
    assert!(!c.interrupt(&mut m, true, false, false));
    c.step(&mut m).unwrap();
    assert_eq!(c.pc, 0x1002);
}
#[test]
fn acia_and_timer_irq() {
    let mut m = Machine::default();
    m.acia.rx.push_back(b'A');
    m.write(0x8018, 0x80);
    assert_eq!(m.read(0x8018), 0x83);
    assert!(m.irq());
    assert_eq!(m.read(0x8019), b'A');
    assert!(!m.irq());
    m.write(0x8010, 2);
    m.tick_cycles = 2;
    let mut c = Cpu::default();
    c.pc = 0x1000;
    c.cc = I;
    m.ram[0x1000] = 0x12;
    m.step(&mut c).unwrap();
    assert_eq!(m.timer, 0x82);
    m.write(0x8010, 0x83);
    assert_eq!(m.timer, 2);
    assert!(!m.irq());
}
#[test]
fn invalid_opcode_reports_address() {
    let (mut c, mut m) = program(&[0x10, 0x12]);
    let e = c.step(&mut m).unwrap_err();
    assert_eq!((e.pc, e.opcode), (0x1000, 0x1012));
}

#[test]
fn exhaustive_add_subtract_with_carry() {
    let (mut c, mut m) = program(&[]);
    for op in [0x8b, 0x89, 0x80, 0x82] {
        for a in 0..=255u16 {
            for b in 0..=255u16 {
                for carry in 0..=1u8 {
                    c.pc = 0x1000;
                    c.a = a as u8;
                    c.cc = carry;
                    m.ram[0x1000] = op;
                    m.ram[0x1001] = b as u8;
                    c.step(&mut m).unwrap();
                    let cin = if op == 0x89 || op == 0x82 {
                        carry as i32
                    } else {
                        0
                    };
                    let subtract = op == 0x80 || op == 0x82;
                    let result = if subtract {
                        a as i32 - b as i32 - cin
                    } else {
                        a as i32 + b as i32 + cin
                    };
                    let signed = if subtract {
                        a as u8 as i8 as i32 - b as u8 as i8 as i32 - cin
                    } else {
                        a as u8 as i8 as i32 + b as u8 as i8 as i32 + cin
                    };
                    assert_eq!(c.a, result as u8);
                    assert_eq!(c.cc & C != 0, !(0..=255).contains(&result));
                    assert_eq!(c.cc & V != 0, !(-128..=127).contains(&signed));
                    assert_eq!(c.cc & N != 0, (result as u8) & 128 != 0);
                    assert_eq!(c.cc & Z != 0, result as u8 == 0);
                    if !subtract {
                        assert_eq!(c.cc & H != 0, (a & 15) + (b & 15) + cin as u16 > 15);
                    }
                }
            }
        }
    }
}

#[test]
fn firq_short_frame_and_nmi_after_lds() {
    let (mut c, mut m) = program(&[0x10, 0xce, 0x70, 0x00]);
    m.write16(0xfffc, 0x2000);
    m.write16(0xfff6, 0x3000);
    m.ram[0x2000] = 0x3b;
    m.ram[0x3000] = 0x3b;
    assert!(!c.interrupt(&mut m, false, false, true));
    c.step(&mut m).unwrap();
    c.cc = 0;
    c.a = 0x42;
    assert!(c.interrupt(&mut m, false, true, false));
    assert_eq!((c.pc, c.s, c.cc & E), (0x3000, 0x6ffd, 0));
    c.step(&mut m).unwrap();
    assert_eq!((c.pc, c.s, c.a), (0x1004, 0x7000, 0x42));
    c.cc = I | F;
    assert!(c.interrupt(&mut m, false, false, true));
    assert_eq!((c.pc, c.s), (0x2000, 0x6ff4));
    c.step(&mut m).unwrap();
    assert_eq!((c.pc, c.s), (0x1004, 0x7000));
}
