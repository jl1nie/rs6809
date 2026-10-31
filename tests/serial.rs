use rs6809::{Bus, Machine};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn machine(file: File) -> Machine {
    Machine::multicomp(&[0; 8192], file).unwrap()
}
fn dummy() -> Machine {
    machine(File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap())
}
fn temporary() -> PathBuf {
    std::env::temp_dir().join(format!(
        "rs6809-disk-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
#[test]
fn four_acias_have_independent_data_control_carrier_and_irq() {
    let mut m = dummy();
    m.set_acia_count(4).unwrap();
    for index in 0..4 {
        let base = 0xffd0 + index as u16 * 2;
        m.write(base, 0x80);
        m.serial_mut(index)
            .unwrap()
            .rx
            .push_back(0x40 + index as u8);
        m.write(base + 1, 0x60 + index as u8);
        assert_eq!(m.read(base) & 0x83, 0x83);
        assert_eq!(m.read(base + 1), 0x40 + index as u8);
        assert_eq!(m.serial(index).unwrap().tx, [0x60 + index as u8]);
        assert!(!m.irq());
    }
    m.serial_mut(3).unwrap().rx.push_back(9);
    assert!(m.irq());
    m.serial_mut(1).unwrap().carrier_lost = true;
    assert_eq!(m.read(0xffd2) & 4, 4);
    assert_eq!(m.read(0xffd0) & 4, 0);
    m.write(0xffd6, 3);
    assert!(!m.irq());
    assert_eq!(m.serial(3).unwrap().resets, 1);
    assert!(m.set_acia_count(0).is_err());
    assert!(m.set_acia_count(5).is_err());
    assert!(Machine::default().set_acia_count(2).is_err());
}
#[test]
fn sd_sector_writes_survive_reopening_and_lock_other_writers() {
    let path = temporary();
    fs::write(&path, vec![0xa5; 1024]).unwrap();
    {
        let mut m = machine(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );
        m.persist_disk_writes().unwrap();
        let mut other = machine(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );
        assert!(other.persist_disk_writes().is_err());
        m.write(0xffda, 1);
        m.write(0xffd9, 1);
        for i in 0..511 {
            m.write(0xffd8, i as u8);
        }
        assert_eq!(fs::read(&path).unwrap(), vec![0xa5; 1024]); // Partial sector is not committed.
        m.write(0xffd8, 0xff);
        assert!(m.disk_error().is_none());
    }
    let mut m = machine(File::open(&path).unwrap());
    m.write(0xffda, 1);
    m.write(0xffd9, 0);
    for i in 0..512 {
        assert_eq!(m.read(0xffd8), i as u8);
    }
    let mut first = [0; 512];
    File::open(&path).unwrap().read_exact(&mut first).unwrap();
    assert_eq!(first, [0xa5; 512]);
    fs::remove_file(path).unwrap();
}
#[test]
fn persistent_sd_reports_backing_file_errors() {
    let path = temporary();
    fs::write(&path, [0; 512]).unwrap();
    let mut m = machine(File::open(&path).unwrap());
    m.persist_disk_writes().unwrap();
    m.write(0xffd9, 1);
    for _ in 0..512 {
        m.write(0xffd8, 42);
    }
    assert!(m.disk_error().is_some());
    assert_eq!(fs::read(&path).unwrap(), [0; 512]);
    fs::remove_file(path).unwrap();
}

#[test]
fn login_readiness_requires_the_guest_monitor_reset_sequence() {
    let mut port = rs6809::machine::Acia::default();
    port.control(3);
    port.control(0x15); // Ordinary boot-ROM/driver initialization is insufficient.
    assert!(!port.session_ready());
    port.control(3);
    port.control(3);
    port.control(0); // Zero is also a valid MC6850 control word.
    assert!(port.session_ready());
    port.control(3);
    assert!(!port.session_ready());
}
