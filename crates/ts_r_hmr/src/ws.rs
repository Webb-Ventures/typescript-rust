//! RFC 6455 WebSocket framing over blocking streams, both roles.
//!
//! The server role carries the HMR channel. The client role is used by the end-to-end tests
//! and the latency benchmark to drive Chrome over the DevTools protocol. Only what those need
//! is implemented: text/binary messages (with fragmentation), ping/pong, close. No
//! extensions (`permessage-deflate` is never negotiated).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

use crate::http::Request;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Largest message we accept (CDP screenshots and large evaluate results fit).
const MAX_MESSAGE: usize = 64 << 20;

/// `Sec-WebSocket-Accept` for a client key.
pub fn accept_key(client_key: &str) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(client_key.trim().as_bytes());
    h.update(GUID.as_bytes());
    base64(&h.digest().bytes())
}

/// The 101 response for an upgrade request, or `None` if the request isn't a valid
/// WebSocket handshake. Echoes the first offered subprotocol (Vite's client offers
/// `vite-hmr`).
pub fn handshake_response(req: &Request) -> Option<String> {
    let key = req.header("sec-websocket-key")?;
    let mut resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n",
        accept_key(key)
    );
    if let Some(proto) = req
        .header("sec-websocket-protocol")
        .and_then(|p| p.split(',').next())
    {
        resp.push_str(&format!("Sec-WebSocket-Protocol: {}\r\n", proto.trim()));
    }
    resp.push_str("\r\n");
    Some(resp)
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn from_u8(v: u8) -> Option<Opcode> {
        Some(match v {
            0 => Opcode::Continuation,
            1 => Opcode::Text,
            2 => Opcode::Binary,
            8 => Opcode::Close,
            9 => Opcode::Ping,
            10 => Opcode::Pong,
            _ => return None,
        })
    }

    fn to_u8(self) -> u8 {
        match self {
            Opcode::Continuation => 0,
            Opcode::Text => 1,
            Opcode::Binary => 2,
            Opcode::Close => 8,
            Opcode::Ping => 9,
            Opcode::Pong => 10,
        }
    }
}

/// Encodes one unfragmented frame. `mask` is required for client → server frames.
pub fn encode_frame(op: Opcode, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut f = Vec::with_capacity(payload.len() + 14);
    f.push(0x80 | op.to_u8());
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => f.push(mask_bit | n as u8),
        n if n <= 0xffff => {
            f.push(mask_bit | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(mask_bit | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(m) => {
            f.extend_from_slice(&m);
            f.extend(payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]));
        }
        None => f.extend_from_slice(payload),
    }
    f
}

/// A complete message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

/// Reads frames until one complete message (control frames are returned as they arrive,
/// even between fragments).
pub fn read_message(
    r: &mut impl Read,
    partial: &mut Option<(Opcode, Vec<u8>)>,
) -> io::Result<Message> {
    loop {
        let mut h = [0u8; 2];
        r.read_exact(&mut h)?;
        let fin = h[0] & 0x80 != 0;
        let op = Opcode::from_u8(h[0] & 0x0f).ok_or_else(|| bad("unknown opcode"))?;
        let masked = h[1] & 0x80 != 0;
        let len = match h[1] & 0x7f {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b)?;
                u64::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                u64::from_be_bytes(b)
            }
            n => u64::from(n),
        };
        if len > MAX_MESSAGE as u64 {
            return Err(bad("frame too large"));
        }
        let mut mask = [0u8; 4];
        if masked {
            r.read_exact(&mut mask)?;
        }
        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload)?;
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        match op {
            Opcode::Close => return Ok(Message::Close),
            Opcode::Ping => return Ok(Message::Ping(payload)),
            Opcode::Pong => return Ok(Message::Pong(payload)),
            Opcode::Text | Opcode::Binary => {
                if fin {
                    return finish(op, payload);
                }
                *partial = Some((op, payload));
            }
            Opcode::Continuation => {
                let Some((first, mut data)) = partial.take() else {
                    return Err(bad("continuation without start"));
                };
                data.extend_from_slice(&payload);
                if data.len() > MAX_MESSAGE {
                    return Err(bad("message too large"));
                }
                if fin {
                    return finish(first, data);
                }
                *partial = Some((first, data));
            }
        }
    }
}

fn finish(op: Opcode, data: Vec<u8>) -> io::Result<Message> {
    match op {
        Opcode::Text => String::from_utf8(data)
            .map(Message::Text)
            .map_err(|_| bad("text frame is not UTF-8")),
        _ => Ok(Message::Binary(data)),
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// A blocking WebSocket client (tests, benchmarks, CDP).
#[derive(Debug)]
pub struct WsClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    partial: Option<(Opcode, Vec<u8>)>,
    seed: u32,
}

impl WsClient {
    /// Connects to `ws://<addr><path>`.
    pub fn connect(addr: &str, path: &str) -> io::Result<WsClient> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let key = base64(&std::process::id().to_le_bytes().repeat(4));
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        let mut writer = stream.try_clone()?;
        writer.write_all(req.as_bytes())?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status)?;
        if !status.starts_with("HTTP/1.1 101") {
            return Err(bad(&format!(
                "websocket handshake failed: {}",
                status.trim()
            )));
        }
        let expected = accept_key(&key);
        let mut ok = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(bad("eof in handshake"));
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("sec-websocket-accept")
                && v.trim() == expected
            {
                ok = true;
            }
        }
        if !ok {
            return Err(bad("bad Sec-WebSocket-Accept"));
        }
        Ok(WsClient {
            reader,
            writer,
            partial: None,
            seed: 0x9e37_79b9,
        })
    }

    pub fn set_read_timeout(&self, t: Option<std::time::Duration>) -> io::Result<()> {
        self.writer.set_read_timeout(t)
    }

    fn next_mask(&mut self) -> [u8; 4] {
        // xorshift: masks only need to be unpredictable to proxies, not cryptographic here.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed.to_le_bytes()
    }

    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let mask = self.next_mask();
        self.writer
            .write_all(&encode_frame(Opcode::Text, text.as_bytes(), Some(mask)))
    }

    /// Next text message (answers pings, skips binary/pong). Errors on close.
    pub fn recv_text(&mut self) -> io::Result<String> {
        loop {
            match read_message(&mut self.reader, &mut self.partial)? {
                Message::Text(t) => return Ok(t),
                Message::Ping(p) => {
                    let mask = self.next_mask();
                    self.writer
                        .write_all(&encode_frame(Opcode::Pong, &p, Some(mask)))?;
                }
                Message::Close => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_rfc_example() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn frames_round_trip_all_lengths_and_fragments() {
        for len in [0usize, 5, 125, 126, 65535, 65536, 200_000] {
            let payload = "x".repeat(len);
            for mask in [None, Some([1, 2, 3, 4])] {
                let f = encode_frame(Opcode::Text, payload.as_bytes(), mask);
                let msg = read_message(&mut &f[..], &mut None).unwrap();
                assert_eq!(msg, Message::Text(payload.clone()));
            }
        }
        // Fragmented "hello" with a ping in the middle.
        let mut data = vec![0x01, 0x03];
        data.extend_from_slice(b"hel");
        data.extend_from_slice(&encode_frame(Opcode::Ping, b"p", None));
        data.extend_from_slice(&[0x80, 0x02]);
        data.extend_from_slice(b"lo");
        let mut r = &data[..];
        let mut partial = None;
        assert_eq!(
            read_message(&mut r, &mut partial).unwrap(),
            Message::Ping(b"p".to_vec())
        );
        assert_eq!(
            read_message(&mut r, &mut partial).unwrap(),
            Message::Text("hello".into())
        );
    }

    #[test]
    fn malformed_frames_error() {
        assert!(read_message(&mut &[0x83u8, 0][..], &mut None).is_err()); // opcode 3
        assert!(read_message(&mut &[0x80u8, 0][..], &mut None).is_err()); // lone continuation
        assert!(read_message(&mut &[0x81u8, 5, b'a'][..], &mut None).is_err()); // truncated
        let huge = [0x81u8, 127, 0xff, 0, 0, 0, 0, 0, 0, 0];
        assert!(read_message(&mut &huge[..], &mut None).is_err());
    }
}
