//! `jsdoc/check-values`: the values of `@version`, `@since` (semver), `@kind`, `@variation`,
//! `@license` (SPDX expressions or an allowed list), `@author` and, in `typescript` mode,
//! `@import` (an import statement).

use super::jsdoc_no_undefined_types::{import_names, import_statement};
use super::prelude::*;
use crate::jsdoc_util::{each_block, preferred_tags, tag_description};
use crate::regex_exec::match_all;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-values",
        "This rule checks the values for a handful of tags: `@version`, `@since`, `@license` and `@author`.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<CheckValues>,
};

#[derive(Debug)]
enum Licenses {
    /// Check SPDX expressions.
    Spdx,
    /// Any non-empty text.
    Any,
    List(Vec<String>),
}

#[derive(Debug)]
struct CheckValues {
    allowed_authors: Option<Vec<String>>,
    licenses: Licenses,
    license_pattern: String,
    numeric_only_variation: bool,
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect()
    })
}

impl FromOptions for CheckValues {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first();
        let get = |k: &str| o.and_then(|o| o.get(k));
        Ok(CheckValues {
            allowed_authors: get("allowedAuthors").and_then(strings),
            licenses: match get("allowedLicenses") {
                Some(Value::Bool(true)) => Licenses::Any,
                Some(v @ Value::Array(_)) => Licenses::List(strings(v).unwrap_or_default()),
                _ => Licenses::Spdx,
            },
            license_pattern: get("licensePattern")
                .and_then(Value::as_str)
                .unwrap_or("/([^\n\r]*)/gv")
                .to_owned(),
            numeric_only_variation: get("numericOnlyVariation").and_then(Value::as_bool)
                == Some(true),
        })
    }
}

const KINDS: &[&str] = &[
    "class",
    "constant",
    "event",
    "external",
    "file",
    "function",
    "member",
    "mixin",
    "module",
    "namespace",
    "typedef",
];

/// `semver.valid` (strict): `v?MAJOR.MINOR.PATCH`, numbers without leading zeros and within
/// the safe integer range, then `-prerelease` and `+build` dot-separated identifiers; at
/// most 256 characters.
fn semver_valid(v: &str) -> bool {
    if v.len() > 256 {
        return false;
    }
    let v = v.strip_prefix('v').unwrap_or(v);
    let (rest, build) = match v.split_once('+') {
        Some((r, b)) => (r, Some(b)),
        None => (v, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let numeric = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
    };
    let alnum =
        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|p| numeric(p) && p.parse::<u64>().is_ok_and(|n| n <= 9_007_199_254_740_991))
    {
        return false;
    }
    if let Some(pre) = pre
        && !pre
            .split('.')
            .all(|id| numeric(id) || (alnum(id) && !id.bytes().all(|b| b.is_ascii_digit())))
    {
        return false;
    }
    if let Some(build) = build
        && !build.split('.').all(alnum)
    {
        return false;
    }
    true
}

/// JavaScript's `Number(text)` for a string.
fn js_number(text: &str) -> f64 {
    let t = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    if t.is_empty() {
        return 0.0;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = t.strip_prefix(prefix) {
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return digits.chars().fold(0.0, |n, c| {
                n * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
            });
        }
    }
    let b = t.as_bytes();
    let mut i = usize::from(matches!(b[0], b'+' | b'-'));
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - int_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - f;
    }
    if digits == 0 {
        return f64::NAN;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        i += 1;
        if i < b.len() && matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        let e = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e {
            return f64::NAN;
        }
    }
    if i != b.len() {
        return f64::NAN;
    }
    t.parse().unwrap_or(f64::NAN)
}

/// The SPDX license and exception identifiers.
static SPDX: &str = include_str!("jsdoc_check_values_spdx.txt");

fn spdx_lists() -> (Vec<&'static str>, Vec<&'static str>) {
    let mut licenses = Vec::new();
    let mut exceptions = Vec::new();
    let mut in_exceptions = false;
    for line in SPDX.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line == "%exceptions" {
            in_exceptions = true;
        } else if in_exceptions {
            exceptions.push(line);
        } else {
            licenses.push(line);
        }
    }
    (licenses, exceptions)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Op(&'static str),
    DocRef,
    LicenseRef,
    License,
    Exception,
}

/// Splits an SPDX expression into tokens; `None` on text that is no token.
fn spdx_tokens(src: &str, licenses: &[&str], exceptions: &[&str]) -> Option<Vec<Tok>> {
    let mut toks = Vec::new();
    let mut i = 0;
    let id_char = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'.';
    // The next run of identifier characters at or after `i` (the scanner searches rather
    // than anchors), and how far it moves `i`.
    let idstring = |i: usize| -> Option<&str> {
        let rest = &src.as_bytes()[i..];
        let s = rest.iter().position(|&c| id_char(c))?;
        let e = rest[s..]
            .iter()
            .position(|&c| !id_char(c))
            .map_or(rest.len(), |e| s + e);
        Some(&src[i + s..i + e])
    };
    let b = src.as_bytes();
    let mut guard = 0;
    while i < b.len() {
        guard += 1;
        if guard > 100_000 {
            return None;
        }
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let rest = &src[i..];
        let starts_ci = |w: &str| rest.len() >= w.len() && rest[..w.len()].eq_ignore_ascii_case(w);
        let op = ["WITH", "AND", "OR"]
            .into_iter()
            .find(|w| rest.is_char_boundary(w.len()) && starts_ci(w))
            .or_else(|| {
                ["(", ")", ":", "+"]
                    .into_iter()
                    .find(|w| rest.starts_with(w))
            });
        if let Some(op) = op {
            i += op.len();
            if op == "+" && i > 1 && b[i - 2] == b' ' {
                return None;
            }
            toks.push(Tok::Op(op));
            continue;
        }
        if let Some((prefix, tok)) = [
            ("DocumentRef-", Tok::DocRef),
            ("LicenseRef-", Tok::LicenseRef),
        ]
        .into_iter()
        .find(|(p, _)| rest.starts_with(p))
        {
            i += prefix.len();
            i += idstring(i)?.len();
            if !src.is_char_boundary(i) {
                return None;
            }
            toks.push(tok);
            continue;
        }
        let s = idstring(i)?;
        if licenses.contains(&s) {
            toks.push(Tok::License);
        } else if exceptions.contains(&s) {
            toks.push(Tok::Exception);
        } else {
            return None;
        }
        i += s.len();
        if !src.is_char_boundary(i) {
            return None;
        }
    }
    Some(toks)
}

/// Whether `src` is a valid SPDX license expression.
fn spdx_valid(src: &str) -> bool {
    let (licenses, exceptions) = spdx_lists();
    let Some(toks) = spdx_tokens(src, &licenses, &exceptions) else {
        return false;
    };
    let mut p = SpdxParser { toks: &toks, at: 0 };
    p.expr(0) && p.at == toks.len()
}

struct SpdxParser<'t> {
    toks: &'t [Tok],
    at: usize,
}

impl SpdxParser<'_> {
    fn eat(&mut self, t: &Tok) -> bool {
        if self.toks.get(self.at) == Some(t) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// An optional `WITH exception`.
    fn with(&mut self) -> bool {
        if self.eat(&Tok::Op("WITH")) {
            return self.eat(&Tok::Exception);
        }
        true
    }

    fn atom(&mut self, depth: u32) -> bool {
        if self.eat(&Tok::Op("(")) {
            return self.expr(depth + 1) && self.eat(&Tok::Op(")"));
        }
        let start = self.at;
        if self.eat(&Tok::DocRef) && !self.eat(&Tok::Op(":")) {
            return false;
        }
        if self.eat(&Tok::LicenseRef) {
            return self.with();
        }
        self.at = start;
        if self.eat(&Tok::License) {
            self.eat(&Tok::Op("+"));
            return self.with();
        }
        false
    }

    /// `and (OR expr)?` at `depth` 0, `atom (AND and)?` at 1.
    fn binary(&mut self, op: &'static str, depth: u32) -> bool {
        if depth > 200 {
            return false;
        }
        let left = if op == "OR" {
            self.binary("AND", depth + 1)
        } else {
            self.atom(depth + 1)
        };
        if !left {
            return false;
        }
        if !self.eat(&Tok::Op(op)) {
            return true;
        }
        self.binary(op, depth + 1)
    }

    fn expr(&mut self, depth: u32) -> bool {
        self.binary("OR", depth)
    }
}

/// Messages for a block, by tag index.
type Reports = Vec<(usize, String)>;

/// `getRegexFromString`: `/source/flags` or a bare source (flags `v`), plus `g`.
fn license_regex(pattern: &str) -> (String, String) {
    let (source, mut flags) = match pattern
        .strip_prefix('/')
        .and_then(|p| p.rfind('/').map(|at| (&p[..at], &p[at + 1..])))
        .filter(|(_, f)| f.chars().all(|c| "gimyvus".contains(c)))
    {
        Some((s, f)) => (
            s.to_owned(),
            if f.is_empty() {
                "v".to_owned()
            } else {
                f.to_owned()
            },
        ),
        None => (pattern.to_owned(), "v".to_owned()),
    };
    if !flags.contains('g') {
        flags.push('g');
    }
    (source, flags)
}

impl Rule for CheckValues {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let (source, flags) = license_regex(&self.license_pattern);
        each_block(cx, |cx, finder, doc| {
            let mut reports: Reports = Vec::new();
            let mut each =
                |cx: &mut Ctx<'_, '_>, name: &str, f: &mut dyn FnMut(usize, &str, &mut Reports)| {
                    if let Some((target, ids)) = preferred_tags(cx, finder, doc, name) {
                        for i in ids {
                            f(i, &target, &mut reports);
                        }
                    }
                };
            let block = &doc.block;
            let semver_check = |i: usize, t: &str, out: &mut Reports| {
                let desc = tag_description(block, i);
                let v = desc.trim();
                if v.is_empty() {
                    out.push((i, format!("Missing JSDoc @{t} value.")));
                } else if !semver_valid(v) {
                    out.push((i, format!("Invalid JSDoc @{t}: \"{desc}\".")));
                }
            };
            each(cx, "version", &mut |i, t, out| semver_check(i, t, out));
            each(cx, "kind", &mut |i, t, out| {
                let desc = tag_description(block, i);
                let k = desc.trim();
                if k.is_empty() {
                    out.push((i, format!("Missing JSDoc @{t} value.")));
                } else if !KINDS.contains(&k) {
                    out.push((
                        i,
                        format!(
                            "Invalid JSDoc @{t}: \"{desc}\"; must be one of: {}.",
                            KINDS.join(", ")
                        ),
                    ));
                }
            });
            if self.numeric_only_variation {
                each(cx, "variation", &mut |i, t, out| {
                    let desc = tag_description(block, i);
                    let v = desc.trim();
                    let n = js_number(v);
                    if v.is_empty() {
                        out.push((i, format!("Missing JSDoc @{t} value.")));
                    } else if !n.is_finite() || n.trunc() != n || n <= 0.0 {
                        out.push((i, format!("Invalid JSDoc @{t}: \"{desc}\".")));
                    }
                });
            }
            each(cx, "since", &mut |i, t, out| semver_check(i, t, out));
            each(cx, "license", &mut |i, t, out| {
                let desc = tag_description(block, i);
                let Some(matches) = match_all(&source, &flags, &desc) else {
                    return;
                };
                let mut positive = false;
                for groups in matches {
                    let license = groups
                        .get(1)
                        .cloned()
                        .flatten()
                        .filter(|g| !g.is_empty())
                        .or_else(|| groups.first().cloned().flatten())
                        .unwrap_or_default();
                    if !license.is_empty() {
                        positive = true;
                    }
                    if license.trim().is_empty() {
                        if positive {
                            return;
                        }
                        out.push((i, format!("Missing JSDoc @{t} value.")));
                        continue;
                    }
                    match &self.licenses {
                        Licenses::Any => {}
                        Licenses::List(allowed) => {
                            if !allowed.contains(&license) {
                                out.push((
                                    i,
                                    format!(
                                        "Invalid JSDoc @{t}: \"{license}\"; expected one of {}.",
                                        allowed.join(", ")
                                    ),
                                ));
                            }
                        }
                        Licenses::Spdx => {
                            if !spdx_valid(&license) {
                                out.push((i, format!(
                                    "Invalid JSDoc @{t}: \"{license}\"; expected SPDX expression: https://spdx.org/licenses/."
                                )));
                            }
                        }
                    }
                }
            });
            if finder.settings.mode == "typescript" {
                each(cx, "import", &mut |i, _, out| {
                    if import_names(&import_statement(&block.tags[i])).is_none() {
                        out.push((i, "Bad @import tag".to_owned()));
                    }
                });
            }
            each(cx, "author", &mut |i, t, out| {
                let desc = tag_description(block, i);
                let a = desc.trim();
                if a.is_empty() {
                    out.push((i, format!("Missing JSDoc @{t} value.")));
                } else if let Some(allowed) = &self.allowed_authors
                    && !allowed.iter().any(|x| x == a)
                {
                    out.push((
                        i,
                        format!(
                            "Invalid JSDoc @{t}: \"{desc}\"; expected one of {}.",
                            allowed.join(", ")
                        ),
                    ));
                }
            });
            for (i, msg) in reports {
                let span = doc.line_span(finder, doc.block.tag_line(i));
                cx.report(span, msg);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver() {
        for v in [
            "1.2.3",
            "v1.2.3",
            "1.2.3-0a",
            "1.2.3+01",
            "1.2.3-x.9007199254740992",
        ] {
            assert!(semver_valid(v), "{v}");
        }
        for v in [
            "=1.2.3",
            "1.2.3-01",
            "01.2.3",
            "1.2.3-",
            "V1.2.3",
            "1.2.3-a..b",
            "1.2.3+a+b",
            "9007199254740992.0.0",
            "3.1",
        ] {
            assert!(!semver_valid(v), "{v}");
        }
    }

    #[test]
    fn spdx() {
        for s in [
            "MIT",
            "(MIT OR GPL-2.0)",
            "GPL-2.0+",
            "Apache-2.0 WITH LLVM-exception",
            "LicenseRef-x",
            "DocumentRef-a:LicenseRef-b",
            "MIT and Apache-2.0",
        ] {
            assert!(spdx_valid(s), "{s}");
        }
        for s in [
            "FOO",
            "MIT-7",
            "(MIT OR GPL-2.5)",
            "MIT AND",
            "GPL-2.0 +",
            "(MIT",
            "",
        ] {
            assert!(!spdx_valid(s), "{s}");
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(js_number(" 3 "), 3.0);
        assert_eq!(js_number("0x10"), 16.0);
        assert_eq!(js_number("1e3"), 1000.0);
        assert!(js_number("abc").is_nan());
        assert!(js_number("1_0").is_nan());
        assert_eq!(js_number("-3"), -3.0);
    }

    #[test]
    fn license_patterns() {
        assert_eq!(
            license_regex("/([^\n\r]*)/gv"),
            ("([^\n\r]*)".to_owned(), "gv".to_owned())
        );
        assert_eq!(
            license_regex("[^\n]*"),
            ("[^\n]*".to_owned(), "vg".to_owned())
        );
        assert_eq!(
            license_regex("/^mit$/i"),
            ("^mit$".to_owned(), "ig".to_owned())
        );
    }
}
