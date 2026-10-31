use rs6809::{Bus, Machine};
use std::{fs::File, path::Path};

fn multicomp() -> Machine {
    // Mapping tests never read the backing SD file; overlay tests write a sector first.
    let file = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let mut rom = [0; 8192];
    for (i, b) in rom.iter_mut().enumerate() {
        *b = 0x80 | (i as u8 & 0x7f);
    }
    Machine::multicomp_with_ram(&rom, file, 64 * 1024).unwrap()
}

#[test]
fn flat_ram_all_addresses_except_configured_io() {
    let mut m = Machine::default();
    for address in 0..=u16::MAX {
        if [m.acia_base, m.acia_base + 1, m.timer_base].contains(&address) {
            continue;
        }
        let value = (address as u8)
            .wrapping_mul(37)
            .wrapping_add((address >> 8) as u8);
        m.write(address, value);
        assert_eq!(m.read(address), value, "${address:04x}");
    }
    // In a flat machine, Multicomp's high I/O addresses are ordinary RAM.
    assert_eq!(m.read(0xffd8), m.ram[0xffd8]);
}

#[test]
fn configurable_io_priority_and_address_wrap() {
    let mut m = Machine::default();
    m.acia_base = 0xffff;
    m.timer_base = 1;
    m.acia.rx.push_back(b'R');
    assert_eq!(m.read(0xffff), 3);
    assert_eq!(m.read(0), b'R');
    m.write(0, b'T');
    assert_eq!(m.acia.tx, b"T");
    m.write(1, 2);
    assert_eq!(m.read(1), 2);
    m.write(0x8018, 0x55);
    assert_eq!(m.read(0x8018), 0x55);

    let mut m = multicomp();
    m.acia_base = 0x4000;
    m.timer_base = 0x4002;
    m.write(0xffde, 0x80);
    m.write(0xffdd, 0x77); // Old timer address is now RAM, even on the rare I/O path.
    assert_eq!(m.read(0xffdd), 0x77);
    m.write(0x4002, 2);
    assert_eq!(m.read(0x4002), 2);
}

#[test]
fn rom_overlay_and_wrapping_vectors() {
    let mut m = multicomp();
    m.ram[0xe000] = 0x11;
    m.write(0xe000, 0x22);
    assert_eq!(m.read(0xe000), 0x80);
    assert_eq!(m.ram[0xe000], 0x11);
    m.write(0xffde, 0x80);
    assert_eq!(m.read(0xe000), 0x11);
    m.write(0xe000, 0x22);
    assert_eq!(m.read(0xe000), 0x22);
    m.write16(0xffff, 0x1234);
    assert_eq!(m.read16(0xffff), 0x1234);
    assert_eq!(m.ram[0], 0x34);
    m.write(0xffde, 0);
    assert_eq!(m.read(0xe000), 0x80);
}

#[test]
fn mapped_ram_protection_tasks_and_missing_banks() {
    let mut m = multicomp();
    for bank in 0..8 {
        m.ram[bank * 8192..(bank + 1) * 8192].fill(bank as u8);
    }
    for slot in 0..8u8 {
        m.write(0xffde, slot);
        m.write(0xffdf, 7 - slot);
        m.write(0xffde, slot + 8);
        m.write(0xffdf, slot);
    }
    m.write(0xffde, 0xa0);
    for slot in 0..8u16 {
        assert_eq!(m.read(slot * 8192), 7 - slot as u8);
        assert_eq!(m.read(slot * 8192 + 0x1fff), 7 - slot as u8);
    }
    m.write(0xffde, 0xa1);
    m.write(0xffdf, 0x86); // Slot 1 maps physical bank 6, read-only.
    m.write(0x2000, 99);
    assert_eq!(m.read(0x2000), 6);
    m.write(0xffde, 0xa2);
    m.write(0xffdf, 8);
    m.write(0x4000, 99);
    assert_eq!(m.read(0x4000), 0xff);
    m.write(0xffde, 0xe0); // Task 1 has identity mappings.
    assert_eq!(m.read(0x2000), 1);
    assert_eq!(m.read(0x4000), 2);
}

#[test]
fn fixed_top_keeps_vectors_in_physical_bank_seven() {
    let mut m = multicomp();
    m.ram[0x4000] = 0x24;
    m.ram[0x5e00] = 0x25;
    m.ram[0xfe00] = 0x7e;
    m.ram[0xffff] = 0x7f;
    m.write(0xffde, 7);
    m.write(0xffdf, 2);
    m.write(0xffde, 0xa7);
    assert_eq!(m.read(0xe000), 0x24);
    assert_eq!(m.read(0xfe00), 0x25);
    m.write(0xffde, 0x27); // Fixed-top latch; ROM stays disabled.
    assert_eq!(m.read(0xe000), 0x24);
    assert_eq!(m.read(0xfe00), 0x7e);
    assert_eq!(m.read(0xffff), 0x7f);
}

#[test]
fn sd_io_sector_round_trip_uses_volatile_overlay() {
    let mut m = multicomp();
    assert_eq!(m.read(0xffd9), 0x80);
    m.write(0xffda, 0x23);
    m.write(0xffdb, 0x01);
    m.write(0xffdc, 0);
    m.write(0xffd9, 1);
    assert_eq!(m.read(0xffd9), 0xa0);
    for i in 0..512 {
        m.write(0xffd8, (i as u8).wrapping_mul(17));
    }
    assert_eq!(m.read(0xffd9), 0x80);
    m.write(0xffd9, 0);
    assert_eq!(m.read(0xffd9), 0xe0);
    for i in 0..512 {
        assert_eq!(m.read(0xffd8), (i as u8).wrapping_mul(17));
    }
    assert_eq!(m.read(0xffd9), 0x80);
    assert!(m.disk_error().is_none());
}

#[test]
fn every_mapping_register_value_updates_reads_and_write_protection() {
    let mut m = multicomp();
    for bank in 0..8 {
        m.ram[bank * 8192..(bank + 1) * 8192].fill(bank as u8);
    }
    for task in 0..2u8 {
        for slot in 0..8u8 {
            for entry in 0..=255u8 {
                let control = 0xa0 | (task << 6) | (task * 8 + slot);
                m.write(0xffde, control);
                m.write(0xffdf, entry);
                let address = u16::from(slot) * 8192 + 0x123;
                let bank = entry & 0x7f;
                assert_eq!(
                    m.read(address),
                    if bank < 8 { bank } else { 0xff },
                    "task={task} slot={slot} entry={entry:02x}"
                );
                m.write(address, 0xa5);
                if bank < 8 {
                    let physical = usize::from(bank) * 8192 + 0x123;
                    assert_eq!(m.ram[physical], if entry & 0x80 == 0 { 0xa5 } else { bank });
                    m.ram[physical] = bank;
                } else {
                    assert_eq!(m.read(address), 0xff);
                }
            }
        }
    }
}

#[test]
fn ignored_mapper_writes_and_disabled_mmu_keep_correct_mapping() {
    let mut m = multicomp();
    m.ram[0x123] = 0x11;
    m.ram[3 * 8192 + 0x123] = 0x33;
    m.write(0xffde, 0xa0);
    m.write(0xffdf, 0x83);
    assert_eq!(m.read(0x123), 0x33);
    m.write(0xffde, 0x90); // Bit 4 makes the entire control write ignored.
    assert_eq!(m.read(0x123), 0x33);
    m.write(0x123, 0x99);
    assert_eq!(m.read(0x123), 0x33);
    m.write(0xffde, 0x80); // A valid write disables MMU and its protection bits.
    assert_eq!(m.read(0x123), 0x11);
    m.write(0x123, 0x22);
    assert_eq!(m.read(0x123), 0x22);
}

fn banked(ram_kb: usize) -> Machine {
    let file = File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    Machine::multicomp_with_ram(&[0x12; 8192], file, ram_kb * 1024).unwrap()
}

#[test]
fn expanded_ram_banks_do_not_alias_and_missing_banks_are_open_bus() {
    for ram_kb in [64, 128, 256, 512, 1024] {
        let mut m = banked(ram_kb);
        let banks = ram_kb / 8;
        for bank in 0..128u8 {
            m.write(0xffde, 0xa2);
            m.write(0xffdf, bank);
            m.write(0x4123, bank ^ 0x5a);
            m.write(0x5fff, bank ^ 0xa5);
        }
        for bank in 0..128u8 {
            m.write(0xffdf, bank);
            assert_eq!(
                m.read(0x4123),
                if usize::from(bank) < banks {
                    bank ^ 0x5a
                } else {
                    0xff
                }
            );
            assert_eq!(
                m.read(0x5fff),
                if usize::from(bank) < banks {
                    bank ^ 0xa5
                } else {
                    0xff
                }
            );
            m.write(0xffdf, bank | 0x80);
            m.write(0x4123, 0);
            assert_eq!(
                m.read(0x4123),
                if usize::from(bank) < banks {
                    bank ^ 0x5a
                } else {
                    0xff
                }
            );
        }
    }
}

#[test]
fn independent_task_maps_share_physical_ram_and_keep_common_vectors() {
    let mut m = banked(512);
    for slot in 0..8u8 {
        m.write(0xffde, slot);
        m.write(0xffdf, 8 + slot);
        m.write(0xffde, 8 + slot);
        m.write(0xffdf, 56 + slot);
    }
    m.ram[0xfff8..0xfffa].copy_from_slice(&[0xfe, 0x10]);
    m.write(0xffde, 0xa0);
    m.write(0xffde, 0x20); // Enable fixed-top while keeping ROM disabled.
    for slot in 0..8u16 {
        m.write(slot * 8192 + 0x100, slot as u8 + 0x10);
    }
    m.write(0xffde, 0xe0);
    for slot in 0..8u16 {
        assert_eq!(m.read(slot * 8192 + 0x100), 0);
        m.write(slot * 8192 + 0x100, slot as u8 + 0x80);
    }
    assert_eq!(m.read16(0xfff8), 0xfe10);
    m.write(0xffde, 0xa0);
    for slot in 0..8u16 {
        assert_eq!(m.read(slot * 8192 + 0x100), slot as u8 + 0x10);
    }
    assert_eq!(m.read16(0xfff8), 0xfe10);
    // Map task 0's first physical bank into task 1, proving shared storage.
    m.write(0xffde, 0xa8);
    m.write(0xffdf, 8);
    assert_eq!(m.read(0x100), 0x10);
    m.write(0xffde, 0xe0);
    assert_eq!(m.read(0x100), 0x10);
}

#[test]
fn fixed_top_is_writable_even_with_protected_or_missing_slot_seven() {
    for entry in [0x83, 0x7f, 0xff] {
        let mut m = banked(512);
        m.write(0xffde, 0xa7);
        m.write(0xffdf, entry);
        m.write(0xffde, 0x27);
        for address in [0xfe00, 0xff7f, 0xff80, 0xffcf, 0xffe0, 0xffff] {
            m.write(address, 0x42);
            assert_eq!(m.read(address), 0x42);
            assert_eq!(m.ram[usize::from(address)], 0x42);
        }
        m.write(0xffd1, b'X'); // I/O remains visible over the common RAM.
        assert_eq!(m.acia.tx, b"X");
        assert_eq!(m.ram[0xffd1], 0);
        m.write(0xffde, 0xa7); // FRT stays latched even with ROMDIS set.
        assert_eq!(m.read(0xfe00), 0x42);
        m.write(0xffde, 0x80); // Disabling MMU clears FRT.
        m.write(0xffde, 0xa7);
        assert_eq!(m.read(0xfe00), if entry & 0x7f < 64 { 0 } else { 0xff });
    }
}

#[test]
fn fixed_top_handshake_requires_previously_enabled_mmu() {
    let mut m = banked(512);
    m.write(0xffde, 7);
    m.write(0xffdf, 8);
    m.write(0xffde, 0x80); // ROM off, MMU still disabled.
    m.write(0xffde, 0x27); // Not a FRT handshake: previous MMUEN was zero.
    assert_eq!(m.read(0xfe00), 0x12); // ROM is re-enabled.
    m.write(0xffde, 0xa7);
    m.write(0xfe00, 0x88);
    assert_eq!(m.ram[8 * 8192 + 0x1e00], 0x88);
    assert_eq!(m.ram[0xfe00], 0);
}

#[test]
fn multicomp_defaults_to_512k_and_rejects_invalid_sizes() {
    let file = || File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    assert_eq!(
        Machine::multicomp(&[0; 8192], file()).unwrap().ram.len(),
        512 * 1024
    );
    for size in [
        0,
        8192,
        65535,
        65537,
        96 * 1024,
        2 * 1024 * 1024,
        usize::MAX,
    ] {
        assert!(Machine::multicomp_with_ram(&[0; 8192], file(), size).is_err());
    }
    let mut m = banked(512);
    assert!(m.load(0xffff, &[1, 2]).is_err()); // load() keeps its 16-bit address contract.
    m.load(0xffff, &[1]).unwrap();
    assert_eq!(m.ram[0xffff], 1);
}
