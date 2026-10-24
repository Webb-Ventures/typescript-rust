//! `data:` URL imports (`import "data:text/javascript,console.log(1)"`).
//!
//! Observed with esbuild 0.28.2 (`bundler_loader_test.go`, `TestLoaderDataURL*`): when
//! bundling, a `data:` URL whose MIME type is `text/javascript` or `application/json` is a
//! module of its own, loaded with the `js` or `json` loader. Parameters such as
//! `;charset=UTF-8` are allowed, `;base64` means base64 data, and otherwise the data is
//! percent-decoded (`+` stays `+`). Such a module cannot import relative paths ("Could not
//! resolve"): it has no directory. Other MIME types stay external imports.

use crate::options::Loader;

/// The module key prefix of a `data:` URL module (the key is the URL itself).
pub const PREFIX: &str = "data:";

/// The loader for a `data:` URL that is bundled as a module, if it is one.
pub fn loader(url: &str) -> Option<Loader> {
    let (mime, _, _) = split(url)?;
    match mime.to_ascii_lowercase().as_str() {
        "text/javascript" => Some(Loader::Js),
        "application/json" => Some(Loader::Json),
        _ => None,
    }
}

/// The decoded contents of a `data:` URL (`None` for malformed base64 or a missing `,`).
pub fn decode(url: &str) -> Option<Vec<u8>> {
    let (_, base64, data) = split(url)?;
    if base64 {
        base64_decode(data)
    } else {
        Some(percent_decode(data))
    }
}

/// `(MIME type, is base64, data)`.
fn split(url: &str) -> Option<(&str, bool, &str)> {
    let rest = url.strip_prefix(PREFIX)?;
    let (head, data) = rest.split_once(',')?;
    let mut params = head.split(';');
    let mime = params.next().unwrap_or("").trim();
    let base64 = params.any(|p| p.trim().eq_ignore_ascii_case("base64"));
    Some((mime, base64, data))
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|c| hex(*c)),
                b.get(i + 2).and_then(|c| hex(*c)),
            )
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(loader("data:text/javascript,x"), Some(Loader::Js));
        assert_eq!(
            loader("data:application/json;charset=UTF-8;base64,e30="),
            Some(Loader::Json)
        );
        assert_eq!(loader("data:some/thing;what,x"), None);
        assert_eq!(loader("data:text/javascript"), None);
        assert_eq!(
            decode("data:text/javascript,console.log('%31%32%33')").unwrap(),
            b"console.log('123')"
        );
        assert_eq!(decode("data:text/javascript,1+2").unwrap(), b"1+2");
        assert_eq!(decode("data:text/javascript,%zz%4").unwrap(), b"%zz%4");
        assert_eq!(
            decode("data:text/javascript;base64,Y29uc29sZS5sb2coMjM0KQ==").unwrap(),
            b"console.log(234)"
        );
        assert_eq!(decode("data:x;base64,!!"), None);
    }
}
