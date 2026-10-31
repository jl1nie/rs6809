//! 68000 behaviour not covered by the single-step suite: reset, interrupts,
//! STOP, trace, privilege violations and the illegal-instruction vectors.
use rs6809::m68k::{Bus68k, M68k, S, T};

struct Ram(Vec<u8>);
impl Bus68k for Ram {
    fn read8(&mut self, a: u32) -> u8 {
        self.0[a as usize % self.0.len()]
    }
    fn write8(&mut self, a: u32, v: u8) {
        let n = self.0.len();
        self.0[a as usize % n] = v;
    }
}
impl Ram {
    fn long(&mut self, a: u32, v: u32) {
        for (i, b) in v.to_be_bytes().iter().enumerate() {
            self.write8(a + i as u32, *b);
        }
    }
    fn words(&mut self, a: u32, w: &[u16]) {
        for (i, v) in w.iter().enumerate() {
            self.write8(a + 2 * i as u32, (v >> 8) as u8);
            self.write8(a + 2 * i as u32 + 1, *v as u8);
        }
    }
    fn word(&mut self, a: u32) -> u16 {
        u16::from_be_bytes([self.read8(a), self.read8(a + 1)])
    }
    fn read_long(&mut self, a: u32) -> u32 {
        u32::from(self.word(a)) << 16 | u32::from(self.word(a + 2))
    }
}

/// SSP $8000, PC $1000; every vector points at its own handler $2000 + 4n.
fn machine(code: &[u16]) -> (M68k, Ram) {
    let mut m = Ram(vec![0; 0x10000]);
    m.long(0, 0x8000);
    m.long(4, 0x1000);
    for v in 2..64 {
        m.long(v * 4, 0x2000 + v * 4);
    }
    m.words(0x1000, code);
    let mut c = M68k::default();
    c.reset(&mut m);
    (c, m)
}

#[test]
fn reset_loads_ssp_and_pc() {
    let (c, _) = machine(&[0x4e71]);
    assert_eq!((c.a[7], c.pc, c.sr), (0x8000, 0x1000, S | 0x0700));
}

#[test]
fn interrupts_respect_the_mask_and_use_autovectors() {
    // MOVE #$2300,SR ; NOP ; NOP
    let (mut c, mut m) = machine(&[0x46fc, 0x2300, 0x4e71, 0x4e71]);
    c.step(&mut m);
    c.irq = 3;
    c.step(&mut m);
    assert_eq!(c.pc, 0x1006, "level 3 is masked at mask 3");
    c.irq = 4;
    let before = c.cycles;
    c.step(&mut m);
    assert_eq!(c.pc, 0x2000 + 28 * 4, "autovector 28 for level 4");
    assert_eq!(c.cycles - before, 44);
    assert_eq!((c.sr >> 8) & 7, 4);
    assert_eq!(m.word(c.a[7]), 0x2300, "stacked SR");
    assert_eq!(m.read_long(c.a[7] + 2), 0x1006, "stacked PC");
    // Level 7 is not maskable.
    let (mut c, mut m) = machine(&[0x4e71]);
    c.irq = 7;
    c.step(&mut m);
    assert_eq!(c.pc, 0x2000 + 31 * 4);
    // A vectored interrupt.
    let (mut c, mut m) = machine(&[0x46fc, 0x2000, 0x4e71]);
    c.step(&mut m);
    c.irq = 2;
    c.irq_vector = Some(64 - 1);
    c.step(&mut m);
    assert_eq!(c.pc, 0x2000 + 63 * 4);
}

#[test]
fn stop_waits_for_an_interrupt() {
    // STOP #$2000 ; NOP
    let (mut c, mut m) = machine(&[0x4e72, 0x2000, 0x4e71]);
    c.step(&mut m);
    assert!(c.stopped);
    c.step(&mut m);
    c.step(&mut m);
    assert_eq!(c.pc, 0x1004);
    c.irq = 1;
    c.step(&mut m);
    assert!(!c.stopped);
    assert_eq!(m.read_long(c.a[7] + 2), 0x1004, "returns after STOP");
}

#[test]
fn trace_traps_after_each_instruction() {
    // MOVE #$A000,SR (trace on) ; NOP
    let (mut c, mut m) = machine(&[0x46fc, T | S, 0x4e71]);
    c.step(&mut m);
    // T is sampled before an instruction: the one that set it does not trap.
    assert_eq!(c.pc, 0x1004);
    c.step(&mut m);
    assert_eq!(c.pc, 0x2000 + 9 * 4, "trace after the next instruction");
    assert_eq!(c.sr & T, 0, "trace is off in the handler");
    assert_eq!(m.read_long(c.a[7] + 2), 0x1006);
}

#[test]
fn privilege_and_illegal_vectors_stack_the_instruction() {
    // MOVE #$0000,SR (to user) ; MOVE #$2700,SR (privileged in user mode)
    let (mut c, mut m) = machine(&[0x46fc, 0x0000, 0x46fc, 0x2700]);
    c.a[7] = 0x8000;
    c.step(&mut m);
    assert!(!c.supervisor());
    c.set_ssp(0x8000);
    c.a[7] = 0x6000; // user stack
    c.step(&mut m);
    assert_eq!(c.pc, 0x2000 + 8 * 4);
    assert!(c.supervisor());
    assert_eq!(
        m.read_long(c.a[7] + 2),
        0x1004,
        "address of the privileged instruction"
    );
    assert_eq!(c.usp(), 0x6000);
    for (op, vector) in [(0x4afcu16, 4u32), (0xa123, 10), (0xf123, 11)] {
        let (mut c, mut m) = machine(&[op]);
        c.step(&mut m);
        assert_eq!(c.pc, 0x2000 + vector * 4, "{op:04X}");
        assert_eq!(m.read_long(c.a[7] + 2), 0x1000);
    }
}

#[test]
fn reset_instruction_signals_the_machine() {
    let (mut c, mut m) = machine(&[0x4e70]);
    c.step(&mut m);
    assert!(c.reset_out);
    assert_eq!(c.cycles - 40, 132);
}

#[test]
fn trap_and_rte_round_trip() {
    // TRAP #5 ; NOP — the handler at vector 37 is RTE.
    let (mut c, mut m) = machine(&[0x4e45, 0x4e71]);
    m.words(0x2000 + 37 * 4, &[0x4e73]);
    c.step(&mut m);
    assert_eq!(c.pc, 0x2000 + 37 * 4);
    c.step(&mut m);
    assert_eq!((c.pc, c.a[7]), (0x1002, 0x8000));
}
