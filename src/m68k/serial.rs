//! A serial port reachable over TCP, for a second terminal (Tiny68K DUART
//! channel B). One client at a time. Made for `telnet`: the server asks for
//! character mode with remote echo, and telnet commands in the input are
//! dropped (`nc` works too, showing those two requests as a few bytes).
//! Output while no one is connected keeps its last 2 KiB for the next client.
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};

const IAC: u8 = 255;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const ECHO: u8 = 1;
const SGA: u8 = 3;

pub struct TcpPort {
    listener: TcpListener,
    client: Option<TcpStream>,
    /// Telnet command parsing state of the input.
    state: u8,
    last_cr: bool,
    /// The last output while no one was connected, sent on connecting
    /// (so a login prompt already printed shows up).
    backlog: VecDeque<u8>,
}

const BACKLOG: usize = 2048;

impl TcpPort {
    pub fn bind(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            client: None,
            state: 0,
            last_cr: false,
            backlog: VecDeque::new(),
        })
    }
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }
    pub fn connected(&self) -> bool {
        self.client.is_some()
    }
    /// Accept a client, move its input to `rx` and `tx` to it. While no one
    /// is connected, output is dropped (like a serial line with no terminal).
    pub fn poll(&mut self, rx: &mut VecDeque<u8>, tx: &mut Vec<u8>) {
        if self.client.is_none()
            && let Ok((mut s, _)) = self.listener.accept()
        {
            let _ = s.set_nonblocking(true);
            let _ = s.set_nodelay(true);
            let _ = s.write_all(&[IAC, WILL, ECHO, IAC, WILL, SGA]);
            let back: Vec<u8> = self.backlog.drain(..).collect();
            tx.splice(0..0, back);
            self.client = Some(s);
            self.state = 0;
        }
        let Some(s) = &mut self.client else {
            self.backlog.extend(tx.drain(..));
            let over = self.backlog.len().saturating_sub(BACKLOG);
            self.backlog.drain(..over);
            return;
        };
        let mut gone = false;
        let mut buf = [0u8; 256];
        let mut got = Vec::new();
        loop {
            match s.read(&mut buf) {
                Ok(0) => {
                    gone = true;
                    break;
                }
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => {
                    gone = true;
                    break;
                }
            }
        }
        if !tx.is_empty() {
            let mut out = Vec::with_capacity(tx.len());
            for &b in tx.iter() {
                out.push(b);
                if b == IAC {
                    out.push(IAC);
                }
            }
            tx.clear();
            if s.write_all(&out).is_err() {
                gone = true;
            }
        }
        if gone {
            self.client = None;
        }
        for b in got {
            self.input(b, rx);
        }
    }
    fn input(&mut self, b: u8, rx: &mut VecDeque<u8>) {
        match (self.state, b) {
            (0, IAC) => self.state = 1,
            (0, _) => {
                // Telnet sends CR LF or CR NUL for Return: one CR.
                if self.last_cr && (b == 0 || b == b'\n') {
                    self.last_cr = false;
                    return;
                }
                self.last_cr = b == b'\r';
                rx.push_back(if b == b'\n' { b'\r' } else { b });
            }
            (1, IAC) => {
                rx.push_back(IAC);
                self.state = 0;
            }
            (1, SB) => self.state = 3,
            (1, 251..=254) => self.state = 2,
            (1, _) | (2, _) => self.state = 0,
            (3, IAC) => self.state = 4,
            (3, _) => {}
            (4, SE) => self.state = 0,
            (4, _) => self.state = 3,
            _ => self.state = 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telnet_client_gets_the_backlog_and_sends_lines() {
        let mut p = TcpPort::bind("127.0.0.1:0").unwrap();
        let addr = p.local_addr().unwrap();
        let (mut rx, mut tx) = (VecDeque::new(), b"login: ".to_vec());
        p.poll(&mut rx, &mut tx); // no one yet: kept
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        // A telnet option reply, then "root" CR LF.
        c.write_all(&[IAC, 253, ECHO]).unwrap();
        c.write_all(b"root\r\n").unwrap();
        let mut got = Vec::new();
        for _ in 0..200 {
            p.poll(&mut rx, &mut tx);
            if rx.len() >= 5 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(rx.iter().copied().collect::<Vec<u8>>(), b"root\r");
        // The requests and the backlog may come in separate segments.
        let mut buf = [0u8; 64];
        while got.len() < 13 {
            let n = c.read(&mut buf).unwrap();
            assert!(n > 0, "closed after {got:?}");
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(&got[..6], &[IAC, WILL, ECHO, IAC, WILL, SGA]);
        assert!(got.ends_with(b"login: "), "{got:?}");
    }
}
