//! Minimal HTTP/1.1 server for the ICE UI. Each connection is served on its
//! own thread; API requests are forwarded to the emulator thread.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub reply: Sender<Response>,
}
impl Request {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json(body: String) -> Self {
        Self {
            status: 200,
            content_type: "application/json; charset=utf-8",
            body: body.into_bytes(),
        }
    }
    pub fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: format!("{{\"error\":{}}}", super::json_str(message)).into_bytes(),
        }
    }
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn parse_target(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (unescape(k), unescape(v))
        })
        .collect();
    (path.to_string(), query)
}

fn serve(stream: TcpStream, api: Sender<super::Inbox>, ui: &'static str) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
            return Ok(());
        };
        let method = method.to_string();
        let (path, query) = parse_target(target);
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = v.trim().parse().unwrap_or(0).min(1 << 20);
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        let response = if path == "/" || path == "/index.html" {
            Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: ui.as_bytes().to_vec(),
            }
        } else if path.starts_with("/api/") {
            let (tx, rx) = mpsc::channel();
            let req = Request {
                method,
                path,
                query,
                body,
                reply: tx,
            };
            if api.send(super::Inbox::Http(req)).is_err() {
                return Ok(());
            }
            rx.recv()
                .unwrap_or_else(|_| Response::error(503, "emulator stopped"))
        } else {
            Response::error(404, "not found")
        };
        let reason = match response.status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "Error",
        };
        write!(
            stream,
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
            response.status,
            response.content_type,
            response.body.len()
        )?;
        stream.write_all(&response.body)?;
        stream.flush()?;
    }
}

/// Accept connections in the background, forwarding API requests to `api`.
pub fn start(listener: TcpListener, api: Sender<super::Inbox>, ui: &'static str) {
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let api = api.clone();
            let _ = stream.set_nodelay(true);
            thread::spawn(move || {
                let _ = serve(stream, api, ui);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_parsing() {
        let (p, q) = parse_target("/api/mem?addr=0x10&text=a%20b+c");
        assert_eq!(p, "/api/mem");
        assert_eq!(q[0], ("addr".into(), "0x10".into()));
        assert_eq!(q[1], ("text".into(), "a b c".into()));
    }
}
