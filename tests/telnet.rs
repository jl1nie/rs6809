use rs6809::{Machine, telnet::TelnetServer};
use std::{
    fs::File,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    time::Duration,
};
fn machine() -> Machine {
    let mut m = Machine::multicomp(
        &[0; 8192],
        File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap(),
    )
    .unwrap();
    m.set_acia_count(4).unwrap();
    m
}
fn connect(server: &TelnetServer) -> TcpStream {
    let s = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    s
}
fn read(s: &mut TcpStream) -> Vec<u8> {
    let mut b = [0; 1024];
    let n = s.read(&mut b).unwrap();
    b[..n].to_vec()
}
#[test]
fn os_opened_ports_set_capacity_and_sessions_do_not_mix_or_inherit_data() {
    let mut m = machine();
    m.serial_mut(0).unwrap().control(3);
    m.serial_mut(0).unwrap().control(3);
    m.serial_mut(0).unwrap().control(0x15);
    m.serial_mut(1).unwrap().control(3);
    m.serial_mut(1).unwrap().control(3);
    m.serial_mut(1).unwrap().control(0x15);
    let mut server = TelnetServer::bind("127.0.0.1:0", &mut m).unwrap();
    let mut a = connect(&server);
    let mut b = connect(&server);
    let mut full = connect(&server);
    server.poll(&mut m).unwrap();
    assert!(String::from_utf8_lossy(&read(&mut a)).contains("ACIA 1"));
    assert!(String::from_utf8_lossy(&read(&mut b)).contains("ACIA 2"));
    assert!(String::from_utf8_lossy(&read(&mut full)).contains("busy"));
    assert_eq!(server.connections(), 2); // Hardware has four ports; OS opened only two.
    a.write_all(b"A\r\n").unwrap();
    b.write_all(b"B\r\0").unwrap();
    server.poll(&mut m).unwrap();
    assert_eq!(
        m.serial(0).unwrap().rx.iter().copied().collect::<Vec<_>>(),
        b"A\r"
    );
    assert_eq!(
        m.serial(1).unwrap().rx.iter().copied().collect::<Vec<_>>(),
        b"B\r"
    );
    server.send(0, b"only-a\xff", &mut m);
    server.send(1, b"only-b", &mut m);
    server.poll(&mut m).unwrap();
    assert_eq!(read(&mut a), b"only-a\xff\xff");
    assert_eq!(read(&mut b), b"only-b");
    a.shutdown(Shutdown::Both).unwrap();
    server.poll(&mut m).unwrap();
    assert!(m.serial(0).unwrap().carrier_lost);
    assert!(m.serial(0).unwrap().rx.is_empty());
    let mut blocked = connect(&server);
    server.poll(&mut m).unwrap();
    assert!(String::from_utf8_lossy(&read(&mut blocked)).contains("busy"));
    // OS-9 closes/reopens the old terminal and acknowledges its readiness.
    m.serial_mut(0).unwrap().control(3);
    m.serial_mut(0).unwrap().control(3);
    m.serial_mut(0).unwrap().control(0x15);
    let mut fresh = connect(&server);
    server.poll(&mut m).unwrap();
    assert!(String::from_utf8_lossy(&read(&mut fresh)).contains("ACIA 1"));
    assert!(m.serial(0).unwrap().rx.is_empty());
    assert!(!m.serial(0).unwrap().carrier_lost);
    // B remains independently connected throughout.
    server.send(1, b"still-b", &mut m);
    server.poll(&mut m).unwrap();
    assert_eq!(read(&mut b), b"still-b");
}
#[test]
fn slow_client_is_disconnected_without_blocking_other_ports() {
    let mut m = machine();
    m.acia.control(3);
    m.acia.control(3);
    m.acia.control(0x15);
    let mut server = TelnetServer::bind("127.0.0.1:0", &mut m).unwrap();
    let _client = connect(&server);
    server.poll(&mut m).unwrap();
    server.send(0, &vec![255; 65536], &mut m);
    assert_eq!(server.connections(), 0);
    assert!(m.acia.carrier_lost);
    assert!(TelnetServer::bind("0.0.0.0:0", &mut m).is_err());
}
