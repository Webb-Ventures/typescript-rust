//! `server.cors`, with the semantics Vite 8.3.4 shows (it uses the `cors` middleware's
//! options; observed, no code taken):
//!
//! - default: origins on `localhost`/`127.0.0.1`/`[::1]` (any port, http or https) are
//!   reflected in `Access-Control-Allow-Origin`; every response carries `Vary: Origin`;
//! - `true`: `Access-Control-Allow-Origin: *`; `false`: no CORS headers;
//! - an options object: `origin` (`*`, a string, `true` = reflect, or a list of strings and
//!   regexes), `methods`, `allowedHeaders` (default: reflect the request's), `exposedHeaders`,
//!   `credentials`, `maxAge`.
//!
//! `OPTIONS` requests are preflights answered with 204.

use crate::http::{Request, Response};

/// One allowed origin.
#[derive(Clone, Debug)]
pub enum OriginMatch {
    Exact(String),
    Regex(regex_lite::Regex),
}

impl OriginMatch {
    fn matches(&self, origin: &str) -> bool {
        match self {
            OriginMatch::Exact(s) => s == origin,
            OriginMatch::Regex(r) => r.is_match(origin),
        }
    }
}

/// Which origins are allowed.
#[derive(Clone, Debug)]
pub enum Origin {
    /// `Access-Control-Allow-Origin: *`.
    Any,
    /// Reflect the request's origin.
    Reflect,
    /// A fixed value (`origin: 'https://x'`).
    Fixed(String),
    /// Reflect when one matches.
    List(Vec<OriginMatch>),
    /// Vite's default: localhost-like origins.
    Localhost,
}

/// `server.cors`.
#[derive(Clone, Debug)]
pub enum Cors {
    Off,
    On(CorsOptions),
}

impl Default for Cors {
    fn default() -> Cors {
        Cors::On(CorsOptions {
            origin: Origin::Localhost,
            ..CorsOptions::default()
        })
    }
}

#[derive(Clone, Debug)]
pub struct CorsOptions {
    pub origin: Origin,
    /// Default `GET,HEAD,PUT,PATCH,POST,DELETE`.
    pub methods: Option<String>,
    /// `None` = reflect `Access-Control-Request-Headers`.
    pub allowed_headers: Option<String>,
    pub exposed_headers: Option<String>,
    pub credentials: bool,
    pub max_age: Option<u64>,
}

impl Default for CorsOptions {
    fn default() -> CorsOptions {
        CorsOptions {
            origin: Origin::Any,
            methods: None,
            allowed_headers: None,
            exposed_headers: None,
            credentials: false,
            max_age: None,
        }
    }
}

/// `^https?://(?:(?:[^:]+\.)?localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$`, by hand.
pub fn is_localhost_origin(origin: &str) -> bool {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let Some(rest) = rest else {
        return false;
    };
    let (host, port) = if let Some(r) = rest.strip_prefix("[::1]") {
        ("[::1]", r)
    } else {
        match rest.find(':') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        }
    };
    let port_ok = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let host_ok = host == "127.0.0.1"
        || host == "[::1]"
        || host == "localhost"
        || host
            .strip_suffix(".localhost")
            .is_some_and(|h| !h.is_empty());
    port_ok && host_ok
}

impl CorsOptions {
    /// The `Access-Control-Allow-Origin` value for `origin`, and whether `Vary: Origin` is due.
    fn allow_origin(&self, origin: Option<&str>) -> (Option<String>, bool) {
        match &self.origin {
            Origin::Any => (Some("*".to_owned()), false),
            Origin::Fixed(s) => (Some(s.clone()), true),
            Origin::Reflect => (origin.map(str::to_owned), true),
            Origin::List(l) => (
                origin
                    .filter(|o| l.iter().any(|m| m.matches(o)))
                    .map(str::to_owned),
                true,
            ),
            Origin::Localhost => (
                origin.filter(|o| is_localhost_origin(o)).map(str::to_owned),
                true,
            ),
        }
    }
}

fn add_vary(resp: &mut Response, value: &str) {
    if let Some((_, v)) = resp
        .headers
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case("vary"))
    {
        if !v.split(',').any(|p| p.trim().eq_ignore_ascii_case(value)) {
            v.push_str(", ");
            v.push_str(value);
        }
        return;
    }
    resp.headers
        .push((std::borrow::Cow::Borrowed("Vary"), value.to_owned()));
}

impl Cors {
    /// Adds CORS headers to a normal response.
    pub fn apply(&self, req: &Request, resp: &mut Response) {
        let Cors::On(o) = self else {
            return;
        };
        let (allow, vary) = o.allow_origin(req.header("origin"));
        if let Some(a) = allow {
            resp.headers
                .push((std::borrow::Cow::Borrowed("Access-Control-Allow-Origin"), a));
        }
        if vary {
            add_vary(resp, "Origin");
        }
        if o.credentials {
            resp.headers.push((
                std::borrow::Cow::Borrowed("Access-Control-Allow-Credentials"),
                "true".to_owned(),
            ));
        }
        if let Some(e) = &o.exposed_headers {
            resp.headers.push((
                std::borrow::Cow::Borrowed("Access-Control-Expose-Headers"),
                e.clone(),
            ));
        }
    }

    /// The answer to an `OPTIONS` preflight, when CORS is on.
    pub fn preflight(&self, req: &Request) -> Option<Response> {
        let Cors::On(o) = self else {
            return None;
        };
        if req.method != "OPTIONS" {
            return None;
        }
        let mut resp = Response {
            status: 204,
            headers: Vec::new(),
            body: Vec::new(),
        };
        let (allow, vary) = o.allow_origin(req.header("origin"));
        if let Some(a) = allow {
            resp = resp.header("Access-Control-Allow-Origin", a);
        }
        if vary {
            add_vary(&mut resp, "Origin");
        }
        if o.credentials {
            resp = resp.header("Access-Control-Allow-Credentials", "true");
        }
        resp = resp.header(
            "Access-Control-Allow-Methods",
            o.methods
                .clone()
                .unwrap_or_else(|| "GET,HEAD,PUT,PATCH,POST,DELETE".to_owned()),
        );
        match &o.allowed_headers {
            Some(h) => resp = resp.header("Access-Control-Allow-Headers", h.clone()),
            None => {
                add_vary(&mut resp, "Access-Control-Request-Headers");
                if let Some(h) = req.header("access-control-request-headers") {
                    resp = resp.header("Access-Control-Allow-Headers", h);
                }
            }
        }
        if let Some(m) = o.max_age {
            resp = resp.header("Access-Control-Max-Age", m.to_string());
        }
        if let Some(e) = &o.exposed_headers {
            resp = resp.header("Access-Control-Expose-Headers", e.clone());
        }
        Some(resp)
    }
}

/// A JS regex (`source`, `flags`) as a [`regex_lite::Regex`]: `i` maps to case-insensitive,
/// `s` to dot-matches-newline, `m` to multi-line; other flags don't change matching here.
pub fn js_regex(source: &str, flags: &str) -> Result<regex_lite::Regex, String> {
    let mut prefix = String::new();
    for (f, c) in [('i', 'i'), ('s', 's'), ('m', 'm')] {
        if flags.contains(f) {
            prefix.push(c);
        }
    }
    // JS allows `\/` and `\-` escapes the regex crate rejects outside classes.
    let mut src = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && matches!(chars.peek(), Some('/')) {
            src.push('/');
            chars.next();
        } else {
            src.push(c);
            if c == '\\'
                && let Some(n) = chars.next()
            {
                src.push(n);
            }
        }
    }
    let pattern = if prefix.is_empty() {
        src
    } else {
        format!("(?{prefix}){src}")
    };
    regex_lite::Regex::new(&pattern).map_err(|e| format!("/{source}/{flags}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, origin: Option<&str>, extra: &[(&str, &str)]) -> Request {
        let mut headers: Vec<(String, String)> = extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(o) = origin {
            headers.push(("origin".into(), o.into()));
        }
        Request {
            method: method.into(),
            target: "/".into(),
            path: "/".into(),
            query: String::new(),
            headers,
            http10: false,
        }
    }

    fn hdr<'a>(r: &'a Response, k: &str) -> Option<&'a str> {
        r.get_header(k)
    }

    #[test]
    fn localhost_origins() {
        for ok in [
            "http://localhost:3000",
            "https://localhost",
            "http://127.0.0.1:9999",
            "http://[::1]:5173",
            "http://app.localhost:1",
        ] {
            assert!(is_localhost_origin(ok), "{ok}");
        }
        for bad in [
            "http://evil.com",
            "http://localhost.evil.com",
            "ftp://localhost",
            "http://localhost:",
            "http://127.0.0.2",
        ] {
            assert!(!is_localhost_origin(bad), "{bad}");
        }
    }

    /// Headers Vite 8.3.4 sent for the same requests (default and custom options).
    #[test]
    fn headers_like_vite() {
        let mut r = Response::text(200, "x");
        Cors::default().apply(&req("GET", Some("http://localhost:3000"), &[]), &mut r);
        assert_eq!(
            hdr(&r, "access-control-allow-origin"),
            Some("http://localhost:3000")
        );
        assert_eq!(hdr(&r, "vary"), Some("Origin"));
        let mut r = Response::text(200, "x");
        Cors::default().apply(&req("GET", Some("http://evil.com"), &[]), &mut r);
        assert_eq!(hdr(&r, "access-control-allow-origin"), None);
        assert_eq!(hdr(&r, "vary"), Some("Origin"));
        let pre = Cors::default()
            .preflight(&req(
                "OPTIONS",
                Some("http://localhost:3000"),
                &[("access-control-request-headers", "x-a, content-type")],
            ))
            .unwrap();
        assert_eq!(pre.status, 204);
        assert_eq!(
            hdr(&pre, "vary"),
            Some("Origin, Access-Control-Request-Headers")
        );
        assert_eq!(
            hdr(&pre, "access-control-allow-methods"),
            Some("GET,HEAD,PUT,PATCH,POST,DELETE")
        );
        assert_eq!(
            hdr(&pre, "access-control-allow-headers"),
            Some("x-a, content-type")
        );

        let custom = Cors::On(CorsOptions {
            origin: Origin::List(vec![
                OriginMatch::Exact("http://a.test".into()),
                OriginMatch::Regex(js_regex("\\.b\\.test$", "").unwrap()),
            ]),
            methods: Some("GET,POST".into()),
            allowed_headers: None,
            exposed_headers: Some("X-E".into()),
            credentials: true,
            max_age: Some(60),
        });
        let mut r = Response::text(200, "x");
        custom.apply(&req("GET", Some("http://x.b.test"), &[]), &mut r);
        assert_eq!(
            hdr(&r, "access-control-allow-origin"),
            Some("http://x.b.test")
        );
        assert_eq!(hdr(&r, "access-control-allow-credentials"), Some("true"));
        assert_eq!(hdr(&r, "access-control-expose-headers"), Some("X-E"));
        let pre = custom
            .preflight(&req(
                "OPTIONS",
                Some("http://a.test"),
                &[("access-control-request-headers", "x-a")],
            ))
            .unwrap();
        assert_eq!(hdr(&pre, "access-control-max-age"), Some("60"));
        assert_eq!(hdr(&pre, "access-control-allow-methods"), Some("GET,POST"));
        let mut r = Response::text(200, "x");
        Cors::On(CorsOptions::default()).apply(&req("GET", Some("http://z"), &[]), &mut r);
        assert_eq!(hdr(&r, "access-control-allow-origin"), Some("*"));
        assert_eq!(hdr(&r, "vary"), None);
        assert!(Cors::Off.preflight(&req("OPTIONS", None, &[])).is_none());
    }

    #[test]
    fn js_regexes() {
        assert!(
            js_regex("^https?:\\/\\/x\\.test$", "")
                .unwrap()
                .is_match("https://x.test")
        );
        assert!(js_regex("^/API", "i").unwrap().is_match("/api/x"));
        assert!(js_regex("(", "").is_err());
    }
}
