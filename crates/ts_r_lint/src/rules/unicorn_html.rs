//! HTML attribute text for the unicorn rules that read JSX attributes the way a browser
//! reads HTML: character-reference decoding in attribute values and `srcset` candidates.

use super::unicorn_util_h2::decode_jsx_entities;

/// Windows-1252 code points the HTML parser substitutes for numeric references 0x80-0x9F.
const C1_REMAP: [u32; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

/// The value of an HTML attribute written as `raw`, with character references decoded, or
/// `None` when that can't be told without the full HTML table of names: a `&name;` that is
/// not one of the HTML 4 names JSX knows, or `&name` without its `;` (a legacy reference
/// may apply). Numeric references follow the HTML parser (with or without `;`, 0 and
/// out-of-range values as U+FFFD, 0x80-0x9F through Windows-1252).
pub(crate) fn html_attr_decode(raw: &str) -> Option<String> {
    if !raw.contains('&') {
        return Some(raw.to_owned());
    }
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i..].chars().next()?;
        if c != '&' {
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        if b.get(i + 1) == Some(&b'#') {
            let hex = matches!(b.get(i + 2), Some(b'x' | b'X'));
            let start = i + 2 + usize::from(hex);
            let radix = if hex { 16 } else { 10 };
            let mut end = start;
            while end < b.len() && (b[end] as char).is_digit(radix) {
                end += 1;
            }
            if end == start {
                out.push('&');
                i += 1;
                continue;
            }
            let v = u32::from_str_radix(&raw[start..end.min(start + 8)], radix)
                .ok()
                .filter(|_| end - start <= 8)
                .unwrap_or(u32::MAX);
            let v = match v {
                0 => 0xFFFD,
                0x80..=0x9F => C1_REMAP[(v - 0x80) as usize],
                v if v > 0x10FFFF => 0xFFFD,
                v => v,
            };
            out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
            i = if b.get(end) == Some(&b';') {
                end + 1
            } else {
                end
            };
            continue;
        }
        let mut end = i + 1;
        while end < b.len() && b[end].is_ascii_alphanumeric() {
            end += 1;
        }
        if end == i + 1 {
            out.push('&');
            i += 1;
            continue;
        }
        if b.get(end) != Some(&b';') {
            return None;
        }
        let reference = &raw[i..=end];
        let decoded = decode_jsx_entities(reference);
        if decoded == reference {
            return None;
        }
        out.push_str(&decoded);
        i = end + 1;
    }
    Some(out)
}

/// HTML's attribute whitespace.
pub(crate) fn is_html_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

/// The byte ranges of the URL candidates of a `srcset` value that has no character
/// references: separators are commas and whitespace; a candidate's URL runs to the next
/// whitespace, minus trailing commas; after a URL without trailing commas the descriptors
/// run to the next comma outside parentheses.
pub(crate) fn srcset_candidates(value: &str) -> Vec<(usize, usize)> {
    let b = value.as_bytes();
    let space = |c: u8| is_html_space(c as char);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] == b',' || space(b[i])) {
            i += 1;
        }
        let start = i;
        let mut trailing_comma = None;
        while i < b.len() && !space(b[i]) {
            if b[i] == b',' {
                trailing_comma.get_or_insert(i);
            } else {
                trailing_comma = None;
            }
            i += 1;
        }
        let end = trailing_comma.unwrap_or(i);
        if end > start {
            out.push((start, end));
        }
        if trailing_comma.is_none() {
            let mut depth = 0u32;
            while i < b.len() {
                let c = b[i];
                i += 1;
                match c {
                    b'(' => depth += 1,
                    b')' if depth > 0 => depth -= 1,
                    b',' if depth == 0 => break,
                    _ => {}
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_attributes() {
        assert_eq!(html_attr_decode("a&amp;b").as_deref(), Some("a&b"));
        assert_eq!(html_attr_decode("&#63query").as_deref(), Some("?query"));
        assert_eq!(html_attr_decode("&#128;").as_deref(), Some("\u{20ac}"));
        assert_eq!(html_attr_decode("&quest;"), None);
        assert_eq!(html_attr_decode("a & b").as_deref(), Some("a & b"));
        assert_eq!(
            srcset_candidates(" a.png 1x, b.png 2x"),
            vec![(1, 6), (11, 16)]
        );
        assert_eq!(srcset_candidates("a.png,b.png"), vec![(0, 11)]);
    }
}
