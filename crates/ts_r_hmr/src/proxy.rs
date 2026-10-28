//! `server.proxy`: forwards matching requests (and WebSocket upgrades with `ws: true`) to
//! another server, the way Vite 8.3.4 does from the outside (observed, no code taken):
//!
//! - rules are tried in order; a key starting with `^` is a regex tested against the
//!   request URL (path and query), any other key is a URL prefix;
//! - `rewrite` maps the URL; the target's own path is prepended (`/base` + `/api/x`);
//! - `changeOrigin` sends the target's host in `Host` (the string form implies it);
//! - `secure: false` accepts any certificate from an `https://`/`wss://` target;
//! - an unreachable target answers `502 Bad Gateway` with an empty body;
//! - proxied responses carry the CORS headers but not `server.headers`;
//! - `bypass(req, res)` runs first (`vite: packages/vite/src/node/server/middlewares/proxy.ts`):
//!   `false` answers an empty 404; a string serves that URL from the dev server instead
//!   (unless the function wrote a response itself); a throw is a server error (logged,
//!   500); anything else proxies. A rule may then have no target at all.
//!
//! The upstream connection is one-shot (`Connection: close`); bodies are streamed in both
//! directions with their own framing (Content-Length, chunked, or until close).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::thread;

use crate::http::{Request, reason};
use crate::net::{self, Duplex};

/// `rewrite(url)`, evaluated wherever the config lives (the Node sidecar for a Vite
/// config). `None` = leave the URL alone.
pub type Rewrite = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// What a rule's `bypass` function decided for one request.
#[derive(Debug)]
pub enum Bypass {
    /// Forward the request (the function returned nothing, or wasn't run).
    Proxy,
    /// `false`: an empty 404.
    NotFound,
    /// A URL: served by the dev server as if requested.
    Serve(String),
    /// The function wrote and ended the response itself.
    Respond(crate::http::Response),
    /// The function threw: logged, then a 500.
    Error(String),
}

/// `bypass(req, res)`, evaluated wherever the config lives (the Node sidecar).
pub type BypassFn = Arc<dyn Fn(&Request) -> Bypass + Send + Sync>;

/// Largest upstream response head we relay.
const MAX_HEAD: usize = 64 * 1024;

/// Where a rule sends requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyTarget {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path prefix (`""` for none), without a trailing `/`.
    pub path: String,
}

impl ProxyTarget {
    /// `http://host:port/path` (also `https`, `ws`, `wss`).
    pub fn parse(url: &str) -> Result<ProxyTarget, String> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| format!("proxy target {url:?} has no scheme"))?;
        let (tls, default_port) = match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => (false, 80),
            "https" | "wss" => (true, 443),
            other => return Err(format!("proxy target {url:?}: unsupported scheme {other}")),
        };
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']'))
        {
            let host = &authority[..end + 2];
            let port = authority[end + 2..].strip_prefix(':');
            (host, port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (authority, None),
            }
        };
        if host.is_empty() {
            return Err(format!("proxy target {url:?} has no host"));
        }
        let port = match port {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| format!("proxy target {url:?}: bad port"))?,
            None => default_port,
        };
        let path = path.split('?').next().unwrap_or("");
        Ok(ProxyTarget {
            tls,
            host: host.to_owned(),
            port,
            path: path.trim_end_matches('/').to_owned(),
        })
    }

    /// `Host` for `changeOrigin`.
    fn authority(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// One `server.proxy` entry.
#[derive(Clone)]
pub struct ProxyRule {
    pub key: String,
    regex: Option<regex_lite::Regex>,
    pub target: ProxyTarget,
    pub change_origin: bool,
    pub ws: bool,
    pub secure: bool,
    pub rewrite: Option<Rewrite>,
    pub bypass: Option<BypassFn>,
    /// The rule has no `target` (only a `bypass`): proxying fails with a 500.
    pub no_target: bool,
    /// Extra request headers (`headers` option).
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for ProxyRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRule")
            .field("key", &self.key)
            .field("target", &self.target)
            .field("change_origin", &self.change_origin)
            .field("ws", &self.ws)
            .field("secure", &self.secure)
            .field("rewrite", &self.rewrite.is_some())
            .field("bypass", &self.bypass.is_some())
            .field("no_target", &self.no_target)
            .finish_non_exhaustive()
    }
}

impl ProxyRule {
    /// A rule for `key` → `target` with the object form's defaults (`changeOrigin: false`,
    /// `ws: false`, `secure: true`).
    pub fn new(key: &str, target: &str) -> Result<ProxyRule, String> {
        let regex = if key.starts_with('^') {
            Some(crate::cors::js_regex(key, "")?)
        } else {
            None
        };
        Ok(ProxyRule {
            key: key.to_owned(),
            regex,
            target: ProxyTarget::parse(target)?,
            change_origin: false,
            ws: false,
            secure: true,
            rewrite: None,
            bypass: None,
            no_target: false,
            headers: Vec::new(),
        })
    }

    /// A rule with only a `bypass` function: requests it doesn't answer can't be proxied.
    pub fn bypass_only(key: &str, bypass: BypassFn) -> Result<ProxyRule, String> {
        let mut rule = ProxyRule::new(key, "http://localhost")?;
        rule.bypass = Some(bypass);
        rule.no_target = true;
        Ok(rule)
    }

    /// Runs the `bypass` function, if there is one.
    pub fn bypass(&self, req: &Request) -> Bypass {
        match &self.bypass {
            Some(f) => f(req),
            None => Bypass::Proxy,
        }
    }

    pub fn matches(&self, url: &str) -> bool {
        match &self.regex {
            Some(r) => r.is_match(url),
            None => url.starts_with(&self.key),
        }
    }

    /// The upstream request target for `url`.
    pub fn upstream_path(&self, url: &str) -> String {
        let rewritten = match &self.rewrite {
            Some(f) => f(url).unwrap_or_else(|| url.to_owned()),
            None => url.to_owned(),
        };
        if self.target.path.is_empty() {
            if rewritten.starts_with('/') {
                rewritten
            } else {
                format!("/{rewritten}")
            }
        } else if rewritten.is_empty() || rewritten.starts_with('?') {
            format!("{}{rewritten}", self.target.path)
        } else {
            format!("{}/{}", self.target.path, rewritten.trim_start_matches('/'))
        }
    }
}

/// The first rule matching `url` (`ws` = only rules with `ws: true`).
pub fn find<'a>(rules: &'a [ProxyRule], url: &str, ws: bool) -> Option<&'a ProxyRule> {
    rules.iter().find(|r| (!ws || r.ws) && r.matches(url))
}

/// Request headers not forwarded as-is.
fn hop_by_hop(name: &str, ws: bool) -> bool {
    matches!(
        name,
        "host" | "keep-alive" | "proxy-connection" | "expect" | "te" | "trailer"
    ) || (!ws && matches!(name, "connection" | "upgrade"))
}

fn request_head(rule: &ProxyRule, req: &Request, ws: bool) -> String {
    let path = rule.upstream_path(&req.target);
    let mut head = format!("{} {path} HTTP/1.1\r\n", req.method);
    let host = if rule.change_origin {
        rule.target.authority()
    } else {
        req.header("host")
            .map_or_else(|| rule.target.authority(), str::to_owned)
    };
    head.push_str(&format!("host: {host}\r\n"));
    for (k, v) in &req.headers {
        if hop_by_hop(k, ws) || rule.headers.iter().any(|(h, _)| h.eq_ignore_ascii_case(k)) {
            continue;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    for (k, v) in &rule.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !ws {
        head.push_str("connection: close\r\n");
    }
    head.push_str("\r\n");
    head
}

fn bad_gateway(w: &mut dyn Write, extra: &[(String, String)]) -> io::Result<()> {
    let mut head = String::from("HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    w.write_all(head.as_bytes())?;
    w.flush()
}

/// Copies exactly `n` bytes.
fn copy_exact(r: &mut dyn Read, w: &mut dyn Write, n: u64) -> io::Result<()> {
    let copied = io::copy(&mut r.take(n), w)?;
    if copied < n {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "body ended early",
        ));
    }
    Ok(())
}

fn read_line(r: &mut dyn BufRead, out: &mut Vec<u8>) -> io::Result<()> {
    out.clear();
    let n = r.take(8 * 1024).read_until(b'\n', out)?;
    if n == 0 || !out.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated chunked body",
        ));
    }
    Ok(())
}

/// Copies a chunked body, framing included.
fn copy_chunked(r: &mut dyn BufRead, w: &mut dyn Write) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        read_line(r, &mut line)?;
        w.write_all(&line)?;
        let text = String::from_utf8_lossy(&line);
        let size = text.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            // Trailers, then the empty line.
            loop {
                read_line(r, &mut line)?;
                w.write_all(&line)?;
                if line == b"\r\n" || line == b"\n" {
                    return Ok(());
                }
            }
        }
        copy_exact(r, w, size)?;
        read_line(r, &mut line)?;
        w.write_all(&line)?;
    }
}

fn is_chunked(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    })
}

fn content_length(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
}

/// Forwards one HTTP request. `body` is the client connection (positioned at the request
/// body); `out` receives the response. `extra` headers (CORS) are added unless the
/// upstream sent the same header. Returns whether the client connection can stay open.
pub fn forward(
    rule: &ProxyRule,
    req: &Request,
    body: &mut dyn BufRead,
    out: &mut dyn Write,
    extra: &[(String, String)],
) -> io::Result<bool> {
    let up = match net::connect(
        &rule.target.host,
        rule.target.port,
        rule.target.tls,
        !rule.secure,
    ) {
        Ok(u) => u,
        Err(e) => {
            bad_gateway(out, extra)?;
            return Err(e);
        }
    };
    let Duplex {
        reader: up_reader,
        writer: mut up_writer,
        tcp: _tcp,
    } = up;
    if req
        .header("expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        out.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        out.flush()?;
    }
    up_writer.write_all(request_head(rule, req, false).as_bytes())?;
    let mut client_open = req.keep_alive();
    if is_chunked(&req.headers) {
        copy_chunked(body, &mut up_writer)?;
    } else if let Some(n) = content_length(&req.headers) {
        copy_exact(body, &mut up_writer, n)?;
    }
    up_writer.flush()?;

    let mut up_reader = BufReader::new(up_reader);
    let (status, status_text, headers) = match read_response_head(&mut up_reader) {
        Ok(h) => h,
        Err(e) => {
            bad_gateway(out, extra)?;
            return Err(e);
        }
    };
    let no_body = req.method == "HEAD" || status == 204 || status == 304 || status < 200;
    let chunked = !no_body && is_chunked(&headers);
    let length = if no_body {
        Some(0)
    } else {
        content_length(&headers)
    };
    if !chunked && length.is_none() {
        client_open = false; // body runs until the upstream closes
    }
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\n",
        if status_text.is_empty() {
            reason(status)
        } else {
            &status_text
        }
    );
    for (k, v) in &headers {
        if k.eq_ignore_ascii_case("connection") || k.eq_ignore_ascii_case("keep-alive") {
            continue;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    for (k, v) in extra {
        if !headers.iter().any(|(h, _)| h.eq_ignore_ascii_case(k)) {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    head.push_str(if client_open {
        "Connection: keep-alive\r\n\r\n"
    } else {
        "Connection: close\r\n\r\n"
    });
    out.write_all(head.as_bytes())?;
    if chunked {
        copy_chunked(&mut up_reader, out)?;
    } else if let Some(n) = length {
        copy_exact(&mut up_reader, out, n)?;
    } else {
        io::copy(&mut up_reader, out)?;
    }
    out.flush()?;
    Ok(client_open)
}

/// Status code, reason phrase and headers of an upstream response.
type ResponseHead = (u16, String, Vec<(String, String)>);

/// Reads an upstream response head.
fn read_response_head(r: &mut dyn BufRead) -> io::Result<ResponseHead> {
    let mut line = Vec::new();
    let mut total = 0usize;
    // Skip interim responses (`100 Continue`): the client already got ours.
    loop {
        read_line(r, &mut line)?;
        total += line.len();
        let text = String::from_utf8_lossy(&line).trim_end().to_owned();
        let mut parts = text.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        if !version.starts_with("HTTP/") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "upstream sent no HTTP status line",
            ));
        }
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad upstream status"))?;
        let status_text = parts.next().unwrap_or("").to_owned();
        let mut headers = Vec::new();
        loop {
            read_line(r, &mut line)?;
            total += line.len();
            if total > MAX_HEAD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "upstream response head too large",
                ));
            }
            let h = String::from_utf8_lossy(&line);
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        return Ok((status, status_text, headers));
    }
}

/// Tunnels a WebSocket upgrade: sends the (rewritten) handshake upstream, then pipes bytes
/// both ways until either side closes. `client_tcp` is shut down when the upstream ends.
pub fn tunnel(
    rule: &ProxyRule,
    req: &Request,
    client_reader: &mut dyn Read,
    mut client_writer: Box<dyn Write + Send>,
    client_tcp: &TcpStream,
) -> io::Result<()> {
    let up = net::connect(
        &rule.target.host,
        rule.target.port,
        rule.target.tls,
        !rule.secure,
    )?;
    let Duplex {
        reader: mut up_reader,
        writer: mut up_writer,
        tcp: up_tcp,
    } = up;
    up_writer.write_all(request_head(rule, req, true).as_bytes())?;
    up_writer.flush()?;
    let _ = client_tcp.set_read_timeout(None);
    let client_tcp2 = client_tcp.try_clone()?;
    let back = thread::Builder::new()
        .name("ts-r-dev-ws-proxy".into())
        .spawn(move || {
            let _ = io::copy(&mut up_reader, &mut client_writer);
            let _ = client_writer.flush();
            let _ = client_tcp2.shutdown(Shutdown::Both);
        })?;
    let mut buf = [0u8; 16 * 1024];
    loop {
        match client_reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if up_writer.write_all(&buf[..n]).is_err() || up_writer.flush().is_err() {
                    break;
                }
            }
        }
    }
    let _ = up_tcp.shutdown(Shutdown::Both);
    let _ = back.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_paths() {
        let t = ProxyTarget::parse("http://localhost:4000").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.path.as_str(), t.tls),
            ("localhost", 4000, "", false)
        );
        let t = ProxyTarget::parse("https://example.com/base/").unwrap();
        assert_eq!((t.port, t.path.as_str(), t.tls), (443, "/base", true));
        assert_eq!(t.authority(), "example.com");
        let t = ProxyTarget::parse("ws://[::1]:9000").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("[::1]", 9000));
        assert!(ProxyTarget::parse("localhost:4000").is_err());
        assert!(ProxyTarget::parse("ftp://x").is_err());

        let mut r = ProxyRule::new("/api", "http://localhost:4000/base").unwrap();
        assert!(r.matches("/api/x?y=1") && !r.matches("/apx"));
        assert_eq!(r.upstream_path("/api/x?y=1"), "/base/api/x?y=1");
        r.rewrite = Some(Arc::new(|u: &str| Some(u.replacen("/api", "", 1))));
        assert_eq!(r.upstream_path("/api/x"), "/base/x");
        assert_eq!(r.upstream_path("/api"), "/base");
        let re = ProxyRule::new("^/fallback/.*", "http://localhost:4000").unwrap();
        assert!(re.matches("/fallback/a") && !re.matches("/x/fallback/a"));
        assert_eq!(re.upstream_path("/fallback/a"), "/fallback/a");
    }

    #[test]
    fn chunked_bodies_copy_verbatim() {
        let body = b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\nX-T: 1\r\n\r\nrest";
        let mut r = &body[..];
        let mut out = Vec::new();
        copy_chunked(&mut r, &mut out).unwrap();
        assert_eq!(out, &body[..body.len() - 4]);
        assert_eq!(r, b"rest");
        let mut bad = &b"zz\r\n"[..];
        assert!(copy_chunked(&mut bad, &mut Vec::new()).is_err());
    }
}
