//! Bounded, nonblocking Telnet connections, one per emulated ACIA.
use crate::Machine;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
};

const IAC: u8 = 255;
const LIMIT: usize = 65536;
const RX_LIMIT: usize = 4096;
const NEGOTIATE: &[u8] = &[IAC, 251, 1, IAC, 251, 3, IAC, 253, 3, IAC, 254, 34];

#[derive(Default)]
enum State {
    #[default]
    Data,
    Command,
    Option(u8),
    Sub,
    SubCommand,
}
#[derive(Default)]
struct Decoder {
    state: State,
    after_cr: bool,
}
impl Decoder {
    fn input(&mut self, b: u8, data: &mut VecDeque<u8>, replies: &mut VecDeque<u8>) {
        match self.state {
            State::Data if b == IAC => self.state = State::Command,
            State::Data => {
                if self.after_cr && (b == 0 || b == 10) {
                    self.after_cr = false;
                    return;
                }
                self.after_cr = b == 13;
                data.push_back(match b {
                    10 => 13,
                    127 => 8,
                    _ => b,
                });
            }
            State::Command => {
                self.state = match b {
                    251..=254 => State::Option(b),
                    250 => State::Sub,
                    IAC => {
                        self.after_cr = false;
                        data.push_back(IAC);
                        State::Data
                    }
                    243 | 244 => {
                        data.push_back(3);
                        State::Data
                    }
                    _ => State::Data,
                };
            }
            State::Option(command) => {
                // Supported options were offered on accept; do not re-offer
                // them in response to their acknowledgement (RFC 854).
                if command == 251 && b != 3 {
                    replies.extend([IAC, 254, b]);
                }
                if command == 253 && b != 1 && b != 3 {
                    replies.extend([IAC, 252, b]);
                }
                self.state = State::Data;
            }
            State::Sub => {
                if b == IAC {
                    self.state = State::SubCommand;
                }
            }
            State::SubCommand => {
                self.state = if b == 240 { State::Data } else { State::Sub };
            }
        }
    }
}
struct Client {
    stream: TcpStream,
    decoder: Decoder,
    output: VecDeque<u8>,
}
impl Client {
    fn queue(&mut self, bytes: &[u8]) -> bool {
        let needed = bytes.len() + bytes.iter().filter(|&&b| b == IAC).count();
        if needed > LIMIT.saturating_sub(self.output.len()) {
            return false;
        }
        for &b in bytes {
            self.output.push_back(b);
            if b == IAC {
                self.output.push_back(b);
            }
        }
        true
    }
    fn flush(&mut self) -> io::Result<()> {
        while !self.output.is_empty() {
            let (bytes, _) = self.output.as_slices();
            match self.stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.output.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
#[derive(Default)]
struct Slot {
    client: Option<Client>,
    wait_reset: Option<u64>,
}

pub struct TelnetServer {
    listener: TcpListener,
    slots: Vec<Slot>,
}
impl TelnetServer {
    pub fn bind(address: &str, machine: &mut Machine) -> io::Result<Self> {
        let addresses: Vec<_> = address.to_socket_addrs()?.collect();
        if addresses.is_empty() || addresses.iter().any(|a| !a.ip().is_loopback()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Telnet must listen on localhost",
            ));
        }
        let listener = TcpListener::bind(addresses.as_slice())?;
        listener.set_nonblocking(true)?;
        let mut slots = Vec::new();
        for index in 0..machine.acia_count() {
            machine.serial_mut(index).unwrap().carrier_lost = true;
            slots.push(Slot::default());
        }
        Ok(Self { listener, slots })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    pub fn connections(&self) -> usize {
        self.slots.iter().filter(|s| s.client.is_some()).count()
    }
    fn disconnect(&mut self, index: usize, machine: &mut Machine) {
        let port = machine.serial_mut(index).unwrap();
        port.carrier_lost = true;
        port.rx.clear();
        port.tx.clear();
        self.slots[index].client = None;
        // A new connection must not inherit the previous user's shell. The
        // guest monitor closes all paths after logout/hangup; driver Init then
        // resets the ACIA, acknowledging that the old session is finished.
        self.slots[index].wait_reset = Some(port.resets);
    }
    pub fn send(&mut self, index: usize, bytes: &[u8], machine: &mut Machine) {
        if let Some(client) = &mut self.slots[index].client
            && !client.queue(bytes)
        {
            self.disconnect(index, machine);
        }
    }
    /// Service bounded amounts of work; a slow or negotiating client never
    /// blocks CPU execution or another terminal.
    pub fn poll(&mut self, machine: &mut Machine) -> io::Result<()> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot
                .wait_reset
                .is_some_and(|reset| machine.serial(index).unwrap().resets != reset)
            {
                slot.wait_reset = None;
            }
        }
        for _ in 0..8 {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(true)?;
                    stream.set_nodelay(true)?;
                    if let Some(index) = self.slots.iter().enumerate().position(|(index, s)| {
                        s.client.is_none()
                            && s.wait_reset.is_none()
                            && machine.serial(index).unwrap().session_ready()
                    }) {
                        let port = machine.serial_mut(index).unwrap();
                        port.rx.clear();
                        port.tx.clear();
                        port.carrier_lost = false;
                        let mut client = Client {
                            stream,
                            decoder: Decoder::default(),
                            output: NEGOTIATE.iter().copied().collect(),
                        };
                        client.queue(
                            format!("\r\nrs6809 ACIA {}. Press Enter to log in.\r\n", index + 1)
                                .as_bytes(),
                        );
                        self.slots[index].client = Some(client);
                    } else {
                        let _ = stream.write(b"\r\nAll configured ACIA sessions are busy.\r\n");
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        for index in 0..self.slots.len() {
            if index != 0 {
                let bytes = std::mem::take(&mut machine.serial_mut(index).unwrap().tx);
                self.send(index, &bytes, machine);
            }
            let mut dead = false;
            if let Some(client) = &mut self.slots[index].client {
                let port = machine.serial_mut(index).unwrap();
                let mut buffer = [0; 1024];
                for _ in 0..4 {
                    let room = RX_LIMIT.saturating_sub(port.rx.len());
                    if room == 0 {
                        break;
                    }
                    let size = room.min(buffer.len());
                    match client.stream.read(&mut buffer[..size]) {
                        Ok(0) => {
                            dead = true;
                            break;
                        }
                        Ok(n) => {
                            for &b in &buffer[..n] {
                                client.decoder.input(b, &mut port.rx, &mut client.output);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            dead = true;
                            break;
                        }
                    }
                    if client.output.len() > LIMIT {
                        dead = true;
                        break;
                    }
                }
                if !dead && client.flush().is_err() {
                    dead = true;
                }
            }
            if dead {
                self.disconnect(index, machine);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_telnet_negotiation_and_nvt_input() {
        let mut decoder = Decoder::default();
        let mut input = VecDeque::new();
        let mut replies = VecDeque::new();
        // Feed byte by byte: options and subnegotiation must never enter the UART.
        for byte in [
            255, 251, 3, 255, 253, 1, 255, 251, 42, 255, 253, 42, 255, 250, 31, 0, 80, 255, 255, 0,
            25, 255, 240, b'A', 13, 10, b'B', 13, 0, 127, 255, 255, 255, 244,
        ] {
            decoder.input(byte, &mut input, &mut replies);
        }
        assert_eq!(
            input.into_iter().collect::<Vec<_>>(),
            [b'A', 13, b'B', 13, 8, 255, 3]
        );
        assert_eq!(
            replies.into_iter().collect::<Vec<_>>(),
            [255, 254, 42, 255, 252, 42]
        );
    }
}
