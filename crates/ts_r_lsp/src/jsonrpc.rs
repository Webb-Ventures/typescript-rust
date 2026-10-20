//! Hand-written JSON-RPC / LSP transport.
//!
//! * Framing: `Content-Length: N\r\n\r\n` + N bytes of UTF-8 JSON. The reader accepts any
//!   header order, ignores `Content-Type`, tolerates blank lines between frames (tsserver
//!   writes a trailing `\n` after each body) and rejects frames larger than [`MAX_FRAME`].
//! * tsserver *input* is newline-delimited JSON; [`read_line_or_frame`] accepts that and
//!   framed input on the same stream.
//! * Messages are kept as [`serde_json::Value`]; [`classify`] tells requests, notifications
//!   and responses apart, [`IdKey`] makes ids (number or string) usable as map keys.
//! * LSP error codes used by the daemon live in [`codes`].

use serde_json::{Value, json};
use std::fmt;
use std::io::{self, BufRead, Write};

/// Largest frame we accept (256 MiB). Anything bigger is treated as a broken stream.
pub const MAX_FRAME: usize = 256 * 1024 * 1024;

/// JSON-RPC / LSP error codes.
pub mod codes {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// A request arrived before `initialize`.
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    /// The request was cancelled by the client.
    pub const REQUEST_CANCELLED: i64 = -32800;
    /// The document changed under the request.
    pub const CONTENT_MODIFIED: i64 = -32801;
    /// The server could not answer (backend restarted, crashed, ...).
    pub const REQUEST_FAILED: i64 = -32803;
}

/// Reads one `Content-Length` frame. `Ok(None)` on clean EOF before any header byte.
pub fn read_frame<R: BufRead>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    let mut saw_header = false;
    loop {
        line.clear();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return if saw_header {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "eof inside frame header",
                ))
            } else {
                Ok(None)
            };
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if saw_header {
                break;
            }
            // Blank line between frames (tsserver's trailing newline): skip.
            continue;
        }
        saw_header = true;
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let v = value.trim().parse::<usize>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length")
                })?;
                if v > MAX_FRAME {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "frame too large",
                    ));
                }
                len = Some(v);
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed header line: {trimmed:.80}"),
            ));
        }
    }
    let len =
        len.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Writes one `Content-Length` frame and flushes.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(body)?;
    w.flush()
}

/// Serializes and writes a message as one frame.
pub fn write_message<W: Write>(w: &mut W, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, &body)
}

/// Reads tsserver-style input: either one JSON line, or a `Content-Length` frame.
/// Blank lines are skipped. `Ok(None)` on EOF.
pub fn read_line_or_frame<R: BufRead>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = trim_ascii(&line);
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > 15 && trimmed[..15].eq_ignore_ascii_case(b"content-length:") {
            let v = std::str::from_utf8(&trimmed[15..])
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .filter(|v| *v <= MAX_FRAME)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"))?;
            // Skip remaining header lines up to the blank line.
            loop {
                line.clear();
                if r.read_until(b'\n', &mut line)? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "eof in header",
                    ));
                }
                if trim_ascii(&line).is_empty() {
                    break;
                }
            }
            let mut body = vec![0u8; v];
            r.read_exact(&mut body)?;
            return Ok(Some(body));
        }
        return Ok(Some(trimmed.to_vec()));
    }
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &b[start..end]
}

/// A request/notification/response id usable as a map key (`1` and `"1"` differ).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IdKey {
    /// Numeric id.
    Num(i64),
    /// String id.
    Str(String),
}

impl IdKey {
    /// Converts a JSON id. `None` for null / non-scalar ids.
    pub fn from_value(v: &Value) -> Option<IdKey> {
        match v {
            Value::Number(n) => n.as_i64().map(IdKey::Num),
            Value::String(s) => Some(IdKey::Str(s.clone())),
            _ => None,
        }
    }

    /// Back to JSON.
    pub fn to_value(&self) -> Value {
        match self {
            IdKey::Num(n) => json!(n),
            IdKey::Str(s) => json!(s),
        }
    }
}

impl fmt::Debug for IdKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdKey::Num(n) => write!(f, "{n}"),
            IdKey::Str(s) => write!(f, "{s:?}"),
        }
    }
}

/// What kind of JSON-RPC message a value is.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind<'a> {
    /// Has `method` and `id`.
    Request {
        /// The id.
        id: &'a Value,
        /// The method.
        method: &'a str,
    },
    /// Has `method`, no `id`.
    Notification {
        /// The method.
        method: &'a str,
    },
    /// Has `id` and `result` or `error`, no `method`.
    Response {
        /// The id.
        id: &'a Value,
    },
}

/// Classifies a JSON-RPC message; `None` if it is none of the three.
pub fn classify(v: &Value) -> Option<Kind<'_>> {
    let obj = v.as_object()?;
    match (obj.get("method").and_then(Value::as_str), obj.get("id")) {
        (Some(method), Some(id)) if !id.is_null() => Some(Kind::Request { id, method }),
        (Some(method), _) => Some(Kind::Notification { method }),
        (None, Some(id)) if obj.contains_key("result") || obj.contains_key("error") => {
            Some(Kind::Response { id })
        }
        _ => None,
    }
}

/// A successful response.
pub fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// An error response.
pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// A request.
pub fn request(id: Value, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A notification.
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"a\":1}").unwrap();
        write_frame(&mut buf, "{\"b\":\"é\"}".as_bytes()).unwrap();
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(
            read_frame(&mut r).unwrap().unwrap(),
            "{\"b\":\"é\"}".as_bytes()
        );
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn tolerates_tsserver_trailing_newline_and_extra_headers() {
        let data = b"Content-Length: 2\r\n\r\n{}\nContent-Type: x\r\ncontent-length:3\r\n\r\n[1]";
        let mut r = Cursor::new(&data[..]);
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"{}");
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"[1]");
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn rejects_garbage_and_huge_frames() {
        assert!(read_frame(&mut Cursor::new(&b"hello\r\n\r\n"[..])).is_err());
        assert!(read_frame(&mut Cursor::new(&b"Content-Length: x\r\n\r\n"[..])).is_err());
        let huge = format!("Content-Length: {}\r\n\r\n", MAX_FRAME + 1);
        assert!(read_frame(&mut Cursor::new(huge.as_bytes())).is_err());
        assert!(read_frame(&mut Cursor::new(&b"X-Other: 1\r\n\r\n"[..])).is_err());
        // Truncated body.
        assert!(read_frame(&mut Cursor::new(&b"Content-Length: 10\r\n\r\n{}"[..])).is_err());
    }

    #[test]
    fn line_or_frame_input() {
        let data = b"{\"seq\":1}\r\n\n{\"seq\":2}\nContent-Length: 9\r\n\r\n{\"seq\":3}{\"seq\":4}";
        let mut r = Cursor::new(&data[..]);
        let mut got = Vec::new();
        while let Some(m) = read_line_or_frame(&mut r).unwrap() {
            got.push(String::from_utf8(m).unwrap());
        }
        assert_eq!(
            got,
            ["{\"seq\":1}", "{\"seq\":2}", "{\"seq\":3}", "{\"seq\":4}"]
        );
    }

    #[test]
    fn classify_messages() {
        let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let note = json!({"jsonrpc":"2.0","method":"initialized"});
        let resp = json!({"jsonrpc":"2.0","id":"x","result":null});
        let err = json!({"jsonrpc":"2.0","id":2,"error":{"code":1,"message":""}});
        assert!(matches!(
            classify(&req),
            Some(Kind::Request {
                method: "initialize",
                ..
            })
        ));
        assert!(matches!(
            classify(&note),
            Some(Kind::Notification {
                method: "initialized"
            })
        ));
        assert!(matches!(classify(&resp), Some(Kind::Response { .. })));
        assert!(matches!(classify(&err), Some(Kind::Response { .. })));
        assert_eq!(classify(&json!({"id":1})), None);
        assert_eq!(classify(&json!(3)), None);
    }

    #[test]
    fn id_keys() {
        assert_ne!(IdKey::from_value(&json!(1)), IdKey::from_value(&json!("1")));
        assert_eq!(IdKey::from_value(&json!(null)), None);
        assert_eq!(IdKey::Str("a".into()).to_value(), json!("a"));
    }
}
