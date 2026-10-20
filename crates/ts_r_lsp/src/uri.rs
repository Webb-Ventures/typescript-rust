//! `file:` URI <-> path conversion, and the document keys the daemon uses.
//!
//! LSP clients name documents by URI, tsserver clients by path. The daemon keys documents by
//! path so both kinds of client share one view. Non-`file:` URIs (`untitled:`) keep the URI
//! itself as the key. When answering an LSP client the daemon prefers the exact URI string
//! that client used, because clients compare URIs as strings and encode them differently.

/// Decodes `%XX` escapes. Invalid escapes are kept literally; invalid UTF-8 is replaced.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `file:///a/b%20c.ts` -> `/a/b c.ts`; `file:///c%3A/x` -> `c:/x` on Windows-style URIs.
/// `None` for other schemes or file URIs with a remote authority.
pub fn uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // Authority (usually empty, sometimes "localhost").
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let decoded = percent_decode(path);
    // "/c:/x" -> "c:/x"
    let b = decoded.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(decoded[1..].to_string());
    }
    Some(decoded)
}

/// Path -> `file:` URI. Encodes everything outside the RFC 3986 unreserved set and `/`.
/// Windows paths (`c:\x` or `c:/x`) become `file:///c%3A/x`, like VS Code.
pub fn path_to_uri(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut out = String::from("file://");
    if !path.starts_with('/') {
        out.push('/');
    }
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// The daemon's document key for an LSP URI.
pub fn key_for_uri(uri: &str) -> String {
    uri_to_path(uri).unwrap_or_else(|| uri.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_and_convert() {
        assert_eq!(
            uri_to_path("file:///a/b%20c.ts").as_deref(),
            Some("/a/b c.ts")
        );
        assert_eq!(uri_to_path("file://localhost/a").as_deref(), Some("/a"));
        assert_eq!(
            uri_to_path("file:///c%3A/x/y.ts").as_deref(),
            Some("c:/x/y.ts")
        );
        assert_eq!(uri_to_path("file://server/share/x"), None);
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(percent_decode("%e2%82%ac%zz%"), "€%zz%");
    }

    #[test]
    fn encode_round_trips() {
        for p in ["/a/b c.ts", "/x/@scope/[id].tsx", "/ü/€.ts", "c:/x/y.ts"] {
            let uri = path_to_uri(p);
            assert_eq!(uri_to_path(&uri).as_deref(), Some(p), "{uri}");
        }
        assert_eq!(path_to_uri("c:\\x\\y.ts"), "file:///c%3A/x/y.ts");
        assert_eq!(path_to_uri("/a b"), "file:///a%20b");
    }

    #[test]
    fn keys() {
        assert_eq!(key_for_uri("file:///a/b.ts"), "/a/b.ts");
        assert_eq!(key_for_uri("untitled:Untitled-1"), "untitled:Untitled-1");
    }
}
