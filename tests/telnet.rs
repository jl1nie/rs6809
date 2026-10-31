use rs6809::{Machine, telnet::TelnetServer};
use std::{fs::File, net::TcpStream, time::Duration};
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
