//! Minimal HTTP/1.1 for a localhost dev server: request head parsing, keep-alive, and
//! response writing. GET/HEAD only; request bodies are drained (bounded) or the connection
//! is closed.

use std::borrow::Cow;
use std::io::{self, BufRead, Read, Write};

/// Largest request head we accept.
const MAX_HEAD: usize = 64 * 1024;
/// Largest request body we drain to keep a connection alive.
const MAX_DRAIN: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// Raw request target, e.g. `/src/a.js?t=1`.
    pub target: String,
    /// Percent-decoded path without query.
    pub path: String,
    /// Raw query without `?` (empty if none).
    pub query: String,
    /// Header names lowercased, in arrival order.
    pub headers: Vec<(String, String)>,
    /// HTTP/1.0 (no default keep-alive).
    pub http10: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn keep_alive(&self) -> bool {
        match self.header("connection").map(str::to_ascii_lowercase) {
            Some(v) if v.contains("close") => false,
            Some(v) if v.contains("keep-alive") => true,
            _ => !self.http10,
        }
    }

    /// Whether `name` appears as a query parameter (`?import`, `?t=1&raw`).
    pub fn has_query(&self, name: &str) -> bool {
        query_param(&self.query, name).is_some()
    }

    pub fn query_value(&self, name: &str) -> Option<&str> {
        query_param(&self.query, name)
    }

    pub fn is_websocket_upgrade(&self) -> bool {
        self.header("upgrade")
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    }
}

/// `Some("")` for a bare flag, `Some(value)` for `name=value`.
pub fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == name).then_some(v)
    })
}

/// Reads one request head. `Ok(None)` on a clean EOF before any byte.
pub fn read_request(r: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut head = Vec::with_capacity(1024);
    loop {
        let before = head.len();
        let n = r
            .by_ref()
            .take((MAX_HEAD - head.len()) as u64 + 1)
            .read_until(b'\n', &mut head)?;
        if n == 0 {
            if head.is_empty() {
                return Ok(None);
            }
            return Err(bad("connection closed mid-request"));
        }
        if head.len() > MAX_HEAD {
            return Err(bad("request head too large"));
        }
        let line = &head[before..];
        if line == b"\r\n" || line == b"\n" {
            if before == 0 {
                // Tolerate stray blank lines between requests.
                head.clear();
                continue;
            }
            break;
        }
    }
    let text = std::str::from_utf8(&head).map_err(|_| bad("request head is not UTF-8"))?;
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r'));
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(bad("unsupported HTTP version"));
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            return Err(bad("malformed header"));
        };
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let (raw_path, query) = target.split_once('?').unwrap_or((target, ""));
    let raw_path = raw_path.split('#').next().unwrap_or(raw_path);
    let req = Request {
        method: method.to_owned(),
        target: target.to_owned(),
        path: percent_decode(raw_path),
        query: query.split('#').next().unwrap_or("").to_owned(),
        headers,
        http10: version == "HTTP/1.0",
    };
    Ok(Some(req))
}

/// Drains a request body so the connection can be reused. Returns `false` if the body
/// can't be drained (chunked or too large) and the connection must close.
pub fn drain_body(r: &mut impl Read, req: &Request) -> io::Result<bool> {
    if req.header("transfer-encoding").is_some() {
        return Ok(false);
    }
    let len: u64 = match req.header("content-length") {
        None => return Ok(true),
        Some(v) => v.parse().map_err(|_| bad("bad content-length"))?,
    };
    if len > MAX_DRAIN {
        return Ok(false);
    }
    io::copy(&mut r.by_ref().take(len), &mut io::sink())?;
    Ok(true)
}

/// Reads a request body (up to 1 MiB, `Content-Length` only). `None` when it can't be read
/// that way (chunked or too large); the connection must then close.
pub fn read_body(r: &mut impl Read, req: &Request) -> io::Result<Option<Vec<u8>>> {
    if req.header("transfer-encoding").is_some() {
        return Ok(None);
    }
    let len: u64 = match req.header("content-length") {
        None => return Ok(Some(Vec::new())),
        Some(v) => v.parse().map_err(|_| bad("bad content-length"))?,
    };
    if len > MAX_DRAIN {
        return Ok(None);
    }
    let mut body = Vec::with_capacity(len as usize);
    r.by_ref().take(len).read_to_end(&mut body)?;
    if (body.len() as u64) < len {
        return Err(bad("connection closed mid-body"));
    }
    Ok(Some(body))
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Decodes `%XX` escapes; invalid escapes and non-UTF-8 results are kept literally.
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Percent-encodes a URL path (keeps `/` and RFC 3986 path characters).
pub fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// JavaScript's `encodeURI`: percent-encodes everything but unreserved characters and the
/// URI delimiters (`;,/?:@&=+$#`).
pub fn encode_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-_.!~*'();,/?:@&=+$#".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Escapes text for HTML (`&`, `<`, `>`, `"`, `'`).
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A response ready to write.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(Cow<'static, str>, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            headers: vec![(Cow::Borrowed("Content-Type"), content_type.to_owned())],
            body: body.into(),
        }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Response {
        Response::new(status, "text/plain; charset=utf-8", body.into())
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Response {
        self.headers.push((Cow::Borrowed(name), value.into()));
        self
    }

    /// A header whose name is only known at run time (plugin middleware responses).
    pub fn header_owned(mut self, name: String, value: impl Into<String>) -> Response {
        self.headers.push((Cow::Owned(name), value.into()));
        self
    }

    pub fn not_modified(etag: &str) -> Response {
        Response {
            status: 304,
            headers: vec![(Cow::Borrowed("ETag"), etag.to_owned())],
            body: Vec::new(),
        }
    }

    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Writes status line, headers and (unless `head_only`) the body.
    pub fn write_to(
        &self,
        w: &mut impl Write,
        head_only: bool,
        keep_alive: bool,
    ) -> io::Result<()> {
        let mut head = String::with_capacity(256);
        head.push_str(&format!(
            "HTTP/1.1 {} {}\r\n",
            self.status,
            reason(self.status)
        ));
        for (k, v) in &self.headers {
            head.push_str(k.as_ref());
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        head.push_str(if keep_alive {
            "Connection: keep-alive\r\n\r\n"
        } else {
            "Connection: close\r\n\r\n"
        });
        if head_only || self.body.is_empty() {
            w.write_all(head.as_bytes())?;
        } else {
            let mut all = head.into_bytes();
            all.extend_from_slice(&self.body);
            w.write_all(&all)?;
        }
        w.flush()
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "",
    }
}

/// `If-None-Match` contains `etag` (or `*`).
pub fn etag_matches(req: &Request, etag: &str) -> bool {
    req.header("if-none-match").is_some_and(|v| {
        v.split(',').map(str::trim).any(|t| {
            t == etag || t == "*" || t.trim_start_matches("W/") == etag.trim_start_matches("W/")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn parses_requests_and_keep_alive() {
        let raw = b"GET /src/a%20b.js?t=12&import HTTP/1.1\r\nHost: x\r\nIf-None-Match: W/\"abc\"\r\n\r\nGET / HTTP/1.0\r\n\r\n";
        let mut r = BufReader::new(&raw[..]);
        let a = read_request(&mut r).unwrap().unwrap();
        assert_eq!(a.method, "GET");
        assert_eq!(a.path, "/src/a b.js");
        assert_eq!(a.query, "t=12&import");
        assert!(a.has_query("import") && a.has_query("t") && !a.has_query("raw"));
        assert_eq!(a.query_value("t"), Some("12"));
        assert_eq!(a.header("host"), Some("x"));
        assert!(a.keep_alive());
        assert!(etag_matches(&a, "W/\"abc\""));
        assert!(!etag_matches(&a, "W/\"abd\""));
        let b = read_request(&mut r).unwrap().unwrap();
        assert!(!b.keep_alive());
        assert!(read_request(&mut r).unwrap().is_none());
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        for raw in [
            &b"NONSENSE\r\n\r\n"[..],
            b"GET / SPDY/3\r\n\r\n",
            b"GET / HTTP/1.1\r\nno-colon\r\n\r\n",
        ] {
            assert!(read_request(&mut BufReader::new(raw)).is_err());
        }
        let huge = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD));
        assert!(read_request(&mut BufReader::new(huge.as_bytes())).is_err());
        assert!(read_request(&mut BufReader::new(&b"GET / HT"[..])).is_err());
    }

    #[test]
    fn percent_coding() {
        assert_eq!(percent_decode("/a%2Fb%zz%"), "/a/b%zz%");
        assert_eq!(percent_encode_path("/src/a b?.js"), "/src/a%20b%3F.js");
        assert_eq!(percent_decode(&percent_encode_path("/ü x")), "/ü x");
    }

    #[test]
    fn writes_responses() {
        let mut out = Vec::new();
        Response::text(200, "hi")
            .header("ETag", "\"1\"")
            .write_to(&mut out, false, true)
            .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Content-Length: 2\r\n") && s.ends_with("\r\n\r\nhi"));
        let mut out = Vec::new();
        Response::text(200, "hi")
            .write_to(&mut out, true, false)
            .unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .ends_with("Connection: close\r\n\r\n")
        );
    }
}
