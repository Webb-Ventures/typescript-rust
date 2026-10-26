//! `unicorn/no-unnecessary-polyfills`: a polyfill for a feature every target already has.
//!
//! The checked strings are the source of an `import` declaration, the argument of
//! `import()` and the single argument of a plain `require(x)` call (a string or a template
//! without substitutions). Relative and absolute paths are skipped.
//!
//! A core-js entry point (`core-js-pure` read as `core-js`, `.js` optional) is reported when
//! every feature it loads is available: one feature reads "Use built-in instead.", several
//! name the module. Any other specifier is matched against polyfill names built from the
//! core-js feature names (`es6-promise`, `array.prototype.find`, `object-assign`,
//! `promise-polyfill`, `mdn-polyfills/Array.prototype.findIndex`, a few known packages);
//! the match with the fewest name segments wins, and it is reported when the `core-js/full`
//! entry of its namespace (and method) is available. A few packages that polyfill exactly
//! one feature (`es6-promise`, `es6-symbol`, ...) check only that feature.
//!
//! A feature is unavailable when some target engine lacks it or has a lower version than
//! core-js-compat records; an `esnext.*` feature is unavailable while its `es.*` version is.
//! Targets come from the `targets` option: an object of engine versions (`{node: "20"}`,
//! `browsers` as a query) or a Browserslist query. The query subset understood is Node.js
//! and browser versions, ranges and comparisons (`node >16`, `chrome 100`, `safari 15 - 16`)
//! joined with `,`, `or`, `and` and `not`; other queries (usage, `defaults`, `last 2
//! versions`), `esmodules` and `node: "current"` give no targets. Without the option the
//! plugin reads a Browserslist config or `engines` in package.json; the linter does not
//! read either, so the rule then reports nothing. An invalid query or version also turns
//! the rule off, as in the plugin.
//!
//! The data (core-js-compat features and entries, Node.js releases, browser versions) is
//! vendored from the pinned packages in `unicorn_no_unnecessary_polyfills_data.txt`.

use super::prelude::*;
use super::ts_no_require_imports::plain_string;
use rustc_hash::FxHashMap;
use std::sync::OnceLock;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-polyfills",
        "Enforce the use of built-in methods instead of unnecessary polyfills.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoUnnecessaryPolyfills>,
};

static DATA: &str = include_str!("unicorn_no_unnecessary_polyfills_data.txt");

/// A core-js-compat version: the first `major[.minor[.patch]]` in the string.
type Ver = (u64, u64, u64);

fn parse_ver(s: &str) -> Option<Ver> {
    let b = s.as_bytes();
    let start = b.iter().position(u8::is_ascii_digit)?;
    let mut parts = [0u64; 3];
    let mut i = start;
    for (k, part) in parts.iter_mut().enumerate() {
        if k > 0 {
            if b.get(i) != Some(&b'.') || !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                break;
            }
            i += 1;
        }
        let mut n = 0u64;
        while let Some(&c) = b.get(i).filter(|c| c.is_ascii_digit()) {
            n = n.saturating_mul(10).saturating_add(u64::from(c - b'0'));
            i += 1;
        }
        *part = n;
    }
    Some((parts[0], parts[1], parts[2]))
}

struct Feature {
    name: &'static str,
    /// (engine index, minimum version); empty for features only entries name.
    reqs: Vec<(u16, Ver)>,
    has_data: bool,
    /// For `esnext.x`: the index of `es.x`.
    es: Option<u32>,
}

struct Browser {
    name: &'static str,
    released: Vec<&'static str>,
    versions: Vec<&'static str>,
}

/// A polyfill name pattern for one core-js feature.
struct Polyfill {
    /// `None` for `es` (`es`, `es6`, `es2015`, ...).
    ecma: Option<&'static str>,
    ctor: [String; 2],
    method: Option<[String; 2]>,
    extra: &'static [&'static str],
    segments: usize,
}

struct Data {
    features: Vec<Feature>,
    by_name: FxHashMap<&'static str, u32>,
    engines: Vec<&'static str>,
    lists: Vec<Vec<u32>>,
    entries: FxHashMap<String, u32>,
    node: Vec<&'static str>,
    browsers: Vec<Browser>,
    polyfills: Vec<Polyfill>,
    tokens: Vec<String>,
    /// Polyfill indices per token.
    by_token: Vec<Vec<u32>>,
    /// Token indices by first byte, in first-seen order.
    tokens_by_first: FxHashMap<u8, Vec<u32>>,
    es_token: Option<u32>,
    es_ctor_tokens: Vec<String>,
}

/// Packages the plugin knows besides the generated names.
fn extra_modules(feature: &str) -> &'static [&'static str] {
    match feature {
        "es.promise.finally" => &["p-finally"],
        "es.object.set-prototype-of" => &["setprototypeof"],
        "es.string.code-point-at" => &["code-point-at"],
        _ => &[],
    }
}

/// Packages that polyfill one feature: only that feature is checked.
const SINGLE_FEATURE: &[&str] = &[
    "es6-symbol",
    "promise-polyfill",
    "es6-promise",
    "weakmap-polyfill",
];

/// `copy-within` as camel case, lowercased.
fn joined(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn first_segment(s: &str) -> &str {
    s.split(['-', '.', '/']).next().unwrap_or("")
}

fn parse_data() -> Data {
    let mut d = Data {
        features: Vec::new(),
        by_name: FxHashMap::default(),
        engines: Vec::new(),
        lists: Vec::new(),
        entries: FxHashMap::default(),
        node: Vec::new(),
        browsers: Vec::new(),
        polyfills: Vec::new(),
        tokens: Vec::new(),
        by_token: Vec::new(),
        tokens_by_first: FxHashMap::default(),
        es_token: None,
        es_ctor_tokens: Vec::new(),
    };
    let mut previous = String::new();
    for line in DATA.lines() {
        let mut words = line.split(' ');
        let tag = words.next().unwrap_or("");
        match tag {
            "G" => d.engines.extend(words),
            "F" | "X" => {
                let Some(name) = words.next() else { continue };
                let reqs = words
                    .filter_map(|w| {
                        let (e, v) = w.split_once('=')?;
                        Some((e.parse().ok()?, parse_ver(v)?))
                    })
                    .collect();
                d.by_name.insert(name, d.features.len() as u32);
                d.features.push(Feature {
                    name,
                    reqs,
                    has_data: tag == "F",
                    es: None,
                });
            }
            "L" => {
                let mut list = Vec::new();
                for w in words {
                    if let Some((a, b)) = w.split_once('-') {
                        if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                            list.extend(a..=b.min(a.saturating_add(10_000)));
                        }
                    } else if let Ok(i) = w.parse() {
                        list.push(i);
                    }
                }
                d.lists.push(list);
            }
            "E" => {
                let (Some(shared), Some(suffix), Some(list)) = (
                    words.next().and_then(|w| w.parse::<usize>().ok()),
                    words.next(),
                    words.next().and_then(|w| w.parse::<u32>().ok()),
                ) else {
                    continue;
                };
                let mut name = previous.get(..shared).unwrap_or("").to_owned();
                name.push_str(suffix);
                d.entries.insert(name.clone(), list);
                previous = name;
            }
            "N" => d.node.extend(words),
            "B" => {
                let Some(name) = words.next() else { continue };
                let mut released = Vec::new();
                let mut versions = Vec::new();
                let mut unreleased = false;
                for w in words {
                    if w == "|" {
                        unreleased = true;
                        continue;
                    }
                    if !unreleased {
                        released.push(w);
                    }
                    versions.push(w);
                }
                d.browsers.push(Browser {
                    name,
                    released,
                    versions,
                });
            }
            _ => {}
        }
    }
    for i in 0..d.features.len() {
        if let Some(rest) = d.features[i].name.strip_prefix("esnext.") {
            d.features[i].es = d.by_name.get(format!("es.{rest}").as_str()).copied();
        }
    }
    build_polyfills(&mut d);
    d
}

fn build_polyfills(d: &mut Data) {
    let mut token_index: FxHashMap<String, u32> = FxHashMap::default();
    let mut es_ctor: Vec<String> = Vec::new();
    for (i, f) in d.features.iter().enumerate().filter(|(_, f)| f.has_data) {
        let mut parts = f.name.split('.');
        let ecma = parts.next().unwrap_or("");
        let ctor = parts.next().unwrap_or("");
        let method = parts.next().unwrap_or("");
        let extra = extra_modules(f.name);
        d.polyfills.push(Polyfill {
            ecma: (ecma != "es").then_some(ecma),
            ctor: [ctor.to_ascii_lowercase(), joined(ctor)],
            method: (!method.is_empty()).then(|| [method.to_ascii_lowercase(), joined(method)]),
            extra,
            segments: f.name.split('.').count(),
        });
        if ecma == "es" {
            for t in [ctor.to_ascii_lowercase(), joined(ctor)] {
                if !es_ctor.contains(&t) {
                    es_ctor.push(t);
                }
            }
        }
        // The polyfill's tokens, deduplicated in order.
        let mut tokens: Vec<String> = Vec::new();
        let add = |tokens: &mut Vec<String>, t: String| {
            if !tokens.contains(&t) {
                tokens.push(t);
            }
        };
        if ecma == "es" {
            add(&mut tokens, "es".to_owned());
        }
        let values = (ecma != "es")
            .then_some(ecma)
            .into_iter()
            .chain([ctor, method])
            .chain(extra.iter().copied());
        for v in values.filter(|v| !v.is_empty()) {
            let lower = v.to_ascii_lowercase();
            let camel = joined(v);
            let (lower_first, camel_first) = (
                first_segment(&lower).to_owned(),
                first_segment(&camel).to_owned(),
            );
            add(&mut tokens, lower);
            add(&mut tokens, lower_first);
            add(&mut tokens, camel);
            add(&mut tokens, camel_first);
        }
        for t in tokens {
            let ti = *token_index.entry(t.clone()).or_insert_with(|| {
                let ti = d.tokens.len() as u32;
                if let Some(&b) = t.as_bytes().first() {
                    d.tokens_by_first.entry(b).or_default().push(ti);
                }
                d.tokens.push(t);
                d.by_token.push(Vec::new());
                ti
            });
            d.by_token[ti as usize].push(i as u32);
        }
    }
    d.es_token = token_index.get("es").copied();
    d.es_ctor_tokens = es_ctor;
}

fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(parse_data)
}

// ---- Polyfill names ----

#[derive(Clone, Copy)]
enum El<'a> {
    Alts(&'a [&'a str]),
    /// `es` followed by digits.
    Es,
}

const PREFIX_OPT: &[&str] = &["", "mdn-polyfills/", "polyfill-"];
const PREFIX: &[&str] = &["mdn-polyfills/", "polyfill-"];
const DELIM: &[&str] = &["", ".", "-", ".prototype.", "/"];
const SUFFIX_OPT: &[&str] = &["", "-polyfill"];
const SUFFIX: &[&str] = &["-polyfill"];

/// Whether `m` is exactly a concatenation matching `els`.
fn seq_matches(m: &[u8], els: &[El<'_>]) -> bool {
    let mut at = vec![0usize];
    for el in els {
        let mut next = Vec::new();
        for &p in &at {
            match *el {
                El::Alts(alts) => {
                    for a in alts {
                        if m.get(p..).is_some_and(|r| r.starts_with(a.as_bytes())) {
                            next.push(p + a.len());
                        }
                    }
                }
                El::Es => {
                    if m.get(p..).is_some_and(|r| r.starts_with(b"es")) {
                        let mut q = p + 2;
                        next.push(q);
                        while m.get(q).is_some_and(u8::is_ascii_digit) {
                            q += 1;
                            next.push(q);
                        }
                    }
                }
            }
        }
        next.sort_unstable();
        next.dedup();
        if next.is_empty() {
            return false;
        }
        at = next;
    }
    at.contains(&m.len())
}

fn polyfill_matches(p: &Polyfill, m: &[u8]) -> bool {
    let ecma_lit = [p.ecma.unwrap_or("")];
    let ecma = match p.ecma {
        Some(_) => El::Alts(&ecma_lit),
        None => El::Es,
    };
    let ctor = [p.ctor[0].as_str(), p.ctor[1].as_str()];
    let c = El::Alts(&ctor);
    let d = El::Alts(DELIM);
    let (po, so) = (El::Alts(PREFIX_OPT), El::Alts(SUFFIX_OPT));
    let method = p.method.as_ref().map(|m| [m[0].as_str(), m[1].as_str()]);
    if let Some(method) = &method {
        let mm = El::Alts(method);
        if seq_matches(m, &[po, ecma, d, c, d, mm, so]) || seq_matches(m, &[po, c, d, mm, so]) {
            return true;
        }
    }
    if seq_matches(m, &[po, ecma, d, c, so]) {
        return true;
    }
    let x = El::Alts(method.as_ref().map_or(&ctor[..], |m| &m[..]));
    seq_matches(m, &[El::Alts(PREFIX), x]) || seq_matches(m, &[x, El::Alts(SUFFIX)]) || {
        p.extra.iter().any(|e| e.as_bytes() == m)
    }
}

fn strip_polyfill_prefix(m: &str) -> &str {
    m.strip_prefix("polyfill-")
        .or_else(|| m.strip_prefix("mdn-polyfills/"))
        .unwrap_or(m)
}

fn potential_es_prefix(d: &Data, m: &str) -> bool {
    let b = m.as_bytes();
    if !m.starts_with("es") {
        return false;
    }
    let mut i = 2;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if b[i..].starts_with(b".prototype.") {
        i += ".prototype.".len();
    } else if matches!(b.get(i), Some(b'.' | b'-' | b'/')) {
        i += 1;
    }
    let rest = &b[i..];
    d.es_ctor_tokens
        .iter()
        .any(|t| rest.starts_with(t.as_bytes()))
}

/// The polyfills whose tokens start the (lowercased) specifier, in the plugin's order.
fn candidates(d: &Data, m: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut seen = vec![false; d.polyfills.len()];
    let mut add = |out: &mut Vec<u32>, ps: &[u32]| {
        for &p in ps {
            if !std::mem::replace(&mut seen[p as usize], true) {
                out.push(p);
            }
        }
    };
    let stripped = strip_polyfill_prefix(m);
    let Some(first) = stripped.as_bytes().first() else {
        return out;
    };
    let Some(tokens) = d.tokens_by_first.get(first) else {
        return out;
    };
    let segment = first_segment(stripped);
    let es_prefix = potential_es_prefix(d, stripped);
    for &ti in tokens {
        let t = d.tokens[ti as usize].as_str();
        let keep = if segment == stripped {
            if t == "es" {
                es_prefix
            } else {
                stripped.starts_with(t)
            }
        } else {
            t != "es" && segment.starts_with(t)
        };
        if keep {
            add(&mut out, &d.by_token[ti as usize]);
        }
    }
    if es_prefix && let Some(es) = d.es_token {
        add(&mut out, &d.by_token[es as usize]);
    }
    out
}

fn best_match(d: &Data, m: &str) -> Option<u32> {
    let mut best = None;
    let mut best_segments = usize::MAX;
    for p in candidates(d, m) {
        let poly = &d.polyfills[p as usize];
        if poly.segments < best_segments && polyfill_matches(poly, m.as_bytes()) {
            best = Some(p);
            best_segments = poly.segments;
        }
    }
    best
}

// ---- Targets ----

/// The browser names Browserslist also accepts.
fn browserslist_alias(name: &str) -> &str {
    match name {
        "fx" | "ff" => "firefox",
        "ios" => "ios_saf",
        "explorer" => "ie",
        "blackberry" => "bb",
        "explorermobile" => "ie_mob",
        "operamini" => "op_mini",
        "operamobile" => "op_mob",
        "chromeandroid" => "and_chr",
        "firefoxandroid" => "and_ff",
        "ucandroid" => "and_uc",
        "qqandroid" => "and_qq",
        _ => name,
    }
}

/// The core-js-compat engine for a target name, if it knows one.
fn core_js_engine(name: &str) -> Option<&str> {
    let name = match browserslist_alias(name) {
        "and_chr" => "chrome-android",
        "and_ff" => "firefox-android",
        "ie_mob" => "ie",
        "ios_saf" => "ios",
        "oculus" => "quest",
        "op_mob" | "opera_mobile" => "opera-android",
        "react" | "reactnative" => "react-native",
        n => n,
    };
    const VALID: &[&str] = &[
        "android",
        "bun",
        "chrome",
        "chrome-android",
        "deno",
        "edge",
        "electron",
        "firefox",
        "firefox-android",
        "hermes",
        "ie",
        "ios",
        "node",
        "opera",
        "opera-android",
        "phantom",
        "quest",
        "react-native",
        "rhino",
        "safari",
        "samsung",
    ];
    VALID.contains(&name).then_some(name)
}

/// JavaScript `parseFloat` on a version (`4.2-4.3` is 4.2, `TP` is NaN).
fn parse_float(s: &str) -> f64 {
    let b = s.trim_start().as_bytes();
    let mut i = 0;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if b.get(i) == Some(&b'.') {
        let j = i + 1;
        let mut k = j;
        while b.get(k).is_some_and(u8::is_ascii_digit) {
            k += 1;
        }
        if k > j || i > 0 {
            i = k;
        }
    }
    std::str::from_utf8(&b[..i])
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(f64::NAN)
}

fn parse_latest_float(v: &str) -> f64 {
    parse_float(v.split('-').nth(1).filter(|s| !s.is_empty()).unwrap_or(v))
}

/// JavaScript `parseInt` (NaN without leading digits).
fn parse_int(s: &str) -> f64 {
    let b = s.trim_start().as_bytes();
    let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
    std::str::from_utf8(&b[..n])
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(f64::NAN)
}

fn cmp_num(a: f64, b: f64) -> i32 {
    if a > b {
        1
    } else if a < b {
        -1
    } else {
        0
    }
}

/// Node.js releases compared to a version (missing parts are 0).
fn node_semver_filter(op: &str, version: &str) -> impl Fn(&str) -> bool {
    let mut r: Vec<f64> = version.split('.').map(parse_int).collect();
    r.resize(3, 0.0);
    for x in &mut r[1..] {
        if x.is_nan() {
            *x = 0.0;
        }
    }
    let op = op.to_owned();
    move |v: &str| {
        let p: Vec<&str> = v.split('.').collect();
        let part = |k: usize| parse_int(p.get(k).copied().filter(|s| !s.is_empty()).unwrap_or("0"));
        let c = (0..3)
            .map(|k| cmp_num(part(k), r[k]))
            .find(|&c| c != 0)
            .unwrap_or(0);
        match op.as_str() {
            ">" => c > 0,
            ">=" => c >= 0,
            "<" => c < 0,
            _ => c <= 0,
        }
    }
}

/// Node.js releases against a range bound: the major, and the minor when given.
fn node_loose_filter(ge: bool, bound: &str) -> impl Fn(&str) -> bool {
    let parts: Vec<f64> = bound.split('.').map(parse_int).collect();
    let major = parts.first().copied().unwrap_or(f64::NAN);
    let minor = parts.get(1).copied();
    move |v: &str| {
        let p: Vec<f64> = v.split('.').map(parse_int).collect();
        let (vm, vn) = (p.first().copied().unwrap_or(f64::NAN), p.get(1).copied());
        let c = if vm != major {
            if vm < major { -1 } else { 1 }
        } else {
            match minor {
                None => 0,
                Some(minor) => {
                    let vn = vn.unwrap_or(f64::NAN);
                    if vn != minor {
                        if vn < minor { -1 } else { 1 }
                    } else {
                        0
                    }
                }
            }
        };
        if ge { c >= 0 } else { c <= 0 }
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_version(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit() || c == b'.')
}

/// One query, without `not`, resolved to `name version` pairs; `None` when it is invalid
/// or outside the understood subset.
fn resolve_query(d: &Data, q: &str) -> Option<Vec<(String, String)>> {
    let lower = q.to_ascii_lowercase();
    let b = lower.as_bytes();
    let word_end = b.iter().position(|&c| !is_word(c)).unwrap_or(b.len());
    let (word, rest) = lower.split_at(word_end);
    if word.is_empty() {
        return None;
    }
    let ws = |s: &str| {
        s.trim_start_matches(|c: char| c.is_ascii_whitespace())
            .len()
            < s.len()
    };
    let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    // `<a> - <b>`
    let range = ws(rest)
        .then(|| {
            let (from, tail) = trimmed.split_once('-')?;
            let (from, to) = (from.trim_end(), tail.trim_start());
            (is_version(from) && is_version(to)).then_some((from, to))
        })
        .flatten();
    // `<op> <version>`
    let ray = {
        let op_len = if trimmed.starts_with(">=") || trimmed.starts_with("<=") {
            2
        } else if trimmed.starts_with('>') || trimmed.starts_with('<') {
            1
        } else {
            0
        };
        (op_len > 0)
            .then(|| {
                let v = trimmed[op_len..].trim_start();
                (is_version(v) || v == "esr").then_some((&trimmed[..op_len], v))
            })
            .flatten()
    };
    let version = (ws(rest) && (trimmed == "tp" || is_version(trimmed))).then_some(trimmed);
    let pair = |name: &str, v: &str| (name.to_owned(), v.to_owned());
    if word == "node" {
        if let Some((from, to)) = range {
            let (ge, le) = (node_loose_filter(true, from), node_loose_filter(false, to));
            return Some(
                d.node
                    .iter()
                    .filter(|v| ge(v) && le(v))
                    .map(|v| pair("node", v))
                    .collect(),
            );
        }
        if let Some((op, v)) = ray
            && v != "esr"
        {
            let f = node_semver_filter(op, v);
            return Some(
                d.node
                    .iter()
                    .filter(|n| f(n))
                    .map(|n| pair("node", n))
                    .collect(),
            );
        }
        if let Some(v) = version {
            let parts: Vec<&str> = v.split('.').collect();
            if parts.len() <= 3 && parts.iter().all(|p| !p.is_empty() && is_version(p)) {
                let prefix = format!("{v}.");
                let last = d
                    .node
                    .iter()
                    .rev()
                    .find(|n| format!("{n}.").starts_with(&prefix))?;
                return Some(vec![pair("node", last)]);
            }
        }
        return None;
    }
    let name = browserslist_alias(word);
    let Some(data) = d.browsers.iter().find(|b| b.name == name) else {
        let opera_mini = lower.contains("op_mini") || lower.contains("operamini");
        return (opera_mini && range.is_none() && ray.is_none() && version.is_none() && {
            let at = lower.find("op_mini").or_else(|| lower.find("operamini"))?;
            let after = lower[at..].trim_start_matches(|c: char| is_word(c as u8));
            ws(after) && after.trim_start().starts_with("all")
        })
        .then(|| vec![pair("op_mini", "all")]);
    };
    let normalize = |v: &str| -> Option<&'static str> {
        if let Some(&full) = data.versions.iter().find(|&&x| x == v) {
            return Some(full);
        }
        if let Some(&full) = data
            .versions
            .iter()
            .find(|x| x.contains('-') && x.split('-').any(|p| p == v))
        {
            return Some(full);
        }
        (data.versions.len() == 1).then(|| data.versions[0])
    };
    if let Some((from, to)) = range {
        let from = normalize(from).map_or_else(|| parse_float(from), parse_float);
        let to = normalize(to).map_or_else(|| parse_float(to), parse_float);
        return Some(
            data.released
                .iter()
                .filter(|v| parse_latest_float(v) >= from && parse_float(v) <= to)
                .map(|v| pair(data.name, v))
                .collect(),
        );
    }
    if let Some((op, v)) = ray {
        if v == "esr" {
            return None;
        }
        let target = parse_float(v);
        let target_latest = parse_latest_float(v);
        return Some(
            data.released
                .iter()
                .filter(|x| match op {
                    ">" => parse_latest_float(x) > target_latest,
                    ">=" => parse_latest_float(x) >= target_latest,
                    "<" => parse_float(x) < target,
                    _ => parse_float(x) <= target,
                })
                .map(|x| pair(data.name, x))
                .collect(),
        );
    }
    let v = version?;
    let v = if v == "tp" { "TP" } else { v };
    let resolved = normalize(v).or_else(|| {
        let alias = if v.contains('.') {
            v.strip_suffix(".0").unwrap_or(v).to_owned()
        } else {
            format!("{v}.0")
        };
        normalize(&alias)
    })?;
    Some(vec![pair(data.name, resolved)])
}

/// Whether a query block ends with `,`, `and` or `or` (it continues in the next one).
fn ends_with_operator(block: &str) -> bool {
    let t = block.trim_end();
    if let Some(head) = t.strip_suffix(',') {
        return !head.trim().is_empty();
    }
    let lower = t.to_ascii_lowercase();
    for op in ["and", "or"] {
        if let Some(head) = lower.strip_suffix(op)
            && head.ends_with(|c: char| c.is_ascii_whitespace())
        {
            return !head.trim().is_empty();
        }
    }
    false
}

#[derive(Clone, Copy, PartialEq)]
enum Compose {
    Or,
    And,
}

/// Splits a block into clauses on `,`, ` and ` and ` or `.
fn split_block(block: &str, out: &mut Vec<(String, Compose)>) {
    let b = block.as_bytes();
    let mut start = 0;
    let mut compose = Compose::Or;
    let mut i = 0;
    let skip_ws = |mut k: usize| {
        while b.get(k).is_some_and(u8::is_ascii_whitespace) {
            k += 1;
        }
        k
    };
    while i < b.len() {
        if b[i] == b',' {
            if i != 0 {
                out.push((block[start..i].trim().to_owned(), compose));
            }
            i = skip_ws(i + 1);
            compose = Compose::Or;
            start = i;
            continue;
        }
        if b[i].is_ascii_whitespace() {
            let q = skip_ws(i);
            let word = |w: &[u8]| {
                b.get(q..q + w.len())
                    .is_some_and(|s| s.eq_ignore_ascii_case(w))
                    && b.get(q + w.len()).is_some_and(u8::is_ascii_whitespace)
            };
            let found = if word(b"and") {
                Some((Compose::And, 3))
            } else if word(b"or") {
                Some((Compose::Or, 2))
            } else {
                None
            };
            if let Some((c, len)) = found {
                if i != 0 {
                    out.push((block[start..i].trim().to_owned(), compose));
                }
                i = skip_ws(q + len);
                compose = c;
                start = i;
                continue;
            }
            i = q;
            continue;
        }
        i += 1;
    }
    out.push((block[start..].trim().to_owned(), compose));
}

/// A Browserslist query (the understood subset) resolved to `name version` pairs.
fn browserslist(d: &Data, queries: &[&str]) -> Option<Vec<(String, String)>> {
    let mut blocks: Vec<String> = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for &q in queries.iter().filter(|q| !q.is_empty()) {
        pending.push(q);
        if !ends_with_operator(q) {
            blocks.push(pending.join("\n"));
            pending.clear();
        }
    }
    if !pending.is_empty() {
        blocks.push(pending.join("\n"));
    }
    let mut clauses = Vec::new();
    for block in &blocks {
        split_block(block, &mut clauses);
    }
    let mut result: Vec<(String, String)> = Vec::new();
    for (index, (clause, compose)) in clauses.iter().enumerate() {
        let lower = clause.to_ascii_lowercase();
        let (not, query) = match lower.strip_prefix("not") {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_whitespace()) => {
                (true, clause[3..].trim_start())
            }
            _ => (false, clause.as_str()),
        };
        if not && index == 0 {
            return None;
        }
        let list = resolve_query(d, query)?;
        if !not && *compose == Compose::Or {
            result.extend(list);
        } else {
            result.retain(|j| list.contains(j) != not);
        }
    }
    Some(result)
}

/// JavaScript truthiness of an option value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JavaScript `String(value)`.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    js_string(x)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn string_list(v: &Value) -> Option<Vec<&str>> {
    match v {
        Value::String(s) => Some(vec![s.as_str()]),
        Value::Array(a) => a.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

/// The core-js-compat targets: the lowest version per known engine. `None` turns the rule
/// off (no targets, an invalid or unsupported query, an invalid version).
fn core_js_targets(d: &Data, targets: &Value) -> Option<Vec<(u16, Ver)>> {
    let mut list: Vec<(String, String)> = Vec::new();
    match targets {
        Value::String(_) | Value::Array(_) => list = browserslist(d, &string_list(targets)?)?,
        Value::Object(o) => {
            let mut lowered: Vec<(String, &Value)> = Vec::new();
            for (k, v) in o {
                let k = k.to_lowercase();
                match lowered.iter_mut().find(|(e, _)| *e == k) {
                    Some(slot) => slot.1 = v,
                    None => lowered.push((k, v)),
                }
            }
            let get = |key: &str| lowered.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
            if get("esmodules").is_some_and(truthy) {
                return None;
            }
            for (k, v) in &lowered {
                if !matches!(k.as_str(), "browsers" | "esmodules" | "node") {
                    list.push((k.clone(), js_string(v)));
                }
            }
            if let Some(b) = get("browsers").filter(|b| truthy(b)) {
                match b {
                    Value::String(_) | Value::Array(_) => {
                        list.extend(browserslist(d, &string_list(b)?)?);
                    }
                    Value::Object(bo) => {
                        list.extend(bo.iter().map(|(k, v)| (k.clone(), js_string(v))));
                    }
                    _ => {}
                }
            }
            if let Some(n) = get("node").filter(|n| truthy(n)) {
                if n.as_str() == Some("current") {
                    return None;
                }
                list.push(("node".to_owned(), js_string(n)));
            }
        }
        _ => return None,
    }
    let mut normalized: Vec<(u16, Ver)> = Vec::new();
    for (engine, version) in &list {
        let Some(engine) = core_js_engine(engine) else {
            continue;
        };
        let v = parse_ver(version)?;
        // An engine no feature records lacks every feature: nothing is reported.
        let engine = d.engines.iter().position(|&e| e == engine)? as u16;
        match normalized.iter_mut().find(|(e, _)| *e == engine) {
            Some(slot) => slot.1 = slot.1.min(v),
            None => normalized.push((engine, v)),
        }
    }
    Some(normalized)
}

/// Per feature: (core-js-compat lists it as needed for the targets, it counts as available).
fn availability(d: &Data, targets: &[(u16, Ver)]) -> Vec<(bool, bool)> {
    let missing: Vec<bool> = d
        .features
        .iter()
        .map(|f| {
            // core-js-compat drops proposals that are already stable.
            f.has_data
                && f.es.is_none()
                && targets.iter().any(|&(e, v)| {
                    f.reqs
                        .iter()
                        .find(|(re, _)| *re == e)
                        .is_none_or(|&(_, min)| v < min)
                })
        })
        .collect();
    d.features
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let available = !missing[i] && !f.es.is_some_and(|es| missing[es as usize]);
            (missing[i], available)
        })
        .collect()
}

#[derive(Debug)]
struct NoUnnecessaryPolyfills {
    /// Per core-js feature: (in core-js-compat's unavailable list, available); `None` when
    /// there are no targets.
    features: Option<Vec<(bool, bool)>>,
}

impl FromOptions for NoUnnecessaryPolyfills {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        if let Some(k) = o.keys().find(|k| *k != "targets") {
            return Err(format!("unknown option `{k}`"));
        }
        let targets = o.get("targets");
        match targets {
            None | Some(Value::String(_) | Value::Object(_)) => {}
            Some(Value::Array(a)) if a.iter().all(Value::is_string) => {}
            Some(_) => {
                return Err("targets must be a string, an array of strings or an object".into());
            }
        }
        let Some(targets) = targets.filter(|t| truthy(t)) else {
            return Ok(Self { features: None });
        };
        let d = data();
        let features = core_js_targets(d, targets).map(|t| availability(d, &t));
        Ok(Self { features })
    }
}

const POLYFILL: &str = "Use built-in instead.";

impl Rule for NoUnnecessaryPolyfills {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl, NodeKind::ImportCall, NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(features) = &self.features else {
            return;
        };
        let ast = cx.ast();
        let source = match *ast.node(node) {
            Node::ImportDecl { source, .. } => source,
            Node::ImportCall { arg, .. } => arg,
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let [arg] = ast.list(args) else {
                    return;
                };
                if ident_name(ast, skip_parens(ast, callee)) != Some("require") {
                    return;
                }
                *arg
            }
            _ => return,
        };
        let source = skip_parens(ast, source);
        let Some(value) = plain_string(cx, source) else {
            return;
        };
        if value.starts_with(['.', '/']) {
            return;
        }
        let d = data();
        let all_available = |list: &[u32]| list.iter().all(|&f| features[f as usize].1);
        let key = value.replacen("core-js-pure", "core-js", 1);
        let key = key.strip_suffix(".js").unwrap_or(&key);
        if let Some(&list) = d.entries.get(key) {
            let list = &d.lists[list as usize];
            if !all_available(list) {
                return;
            }
            let message = if list.len() > 1 {
                format!(
                    "All polyfilled features imported from `{value}` are available as built-ins. Use the built-ins instead."
                )
            } else {
                POLYFILL.to_owned()
            };
            let id = if list.len() > 1 {
                "unnecessaryCoreJsModule"
            } else {
                "unnecessaryPolyfill"
            };
            cx.report(cx.span(source), message).id(id);
            return;
        }
        let lower = value.to_lowercase();
        let Some(p) = best_match(d, &lower) else {
            return;
        };
        let feature = d.features[p as usize].name;
        let report = if SINGLE_FEATURE.contains(&lower.as_str()) {
            !features[p as usize].0
        } else {
            let mut parts = feature.split('.').skip(1);
            let ns = parts.next().unwrap_or("");
            let entry = match parts.next() {
                Some(m) => format!("core-js/full/{ns}/{m}"),
                None => format!("core-js/full/{ns}"),
            };
            d.entries
                .get(entry.as_str())
                .is_some_and(|&l| all_available(&d.lists[l as usize]))
        };
        if report {
            cx.report(cx.span(source), POLYFILL)
                .id("unnecessaryPolyfill");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_parses() {
        let d = data();
        assert_eq!(d.polyfills.len(), 544);
        assert!(d.entries.contains_key("core-js/full/array/flat"));
        assert!(d.node.len() > 300);
    }

    #[test]
    fn names_match() {
        let d = data();
        let name = |m: &str| best_match(d, m).map(|p| d.features[p as usize].name);
        assert_eq!(name("object-assign"), Some("es.object.assign"));
        assert_eq!(name("es6-promise"), Some("es.promise"));
        assert_eq!(name("this-is-not-a-polyfill"), None);
    }

    #[test]
    fn queries() {
        let d = data();
        assert!(browserslist(d, &["node >3"]).is_some_and(|l| !l.is_empty()));
        assert!(browserslist(d, &["invalid browserslist query"]).is_none());
        assert_eq!(
            browserslist(d, &["chrome 100"]),
            Some(vec![("chrome".to_owned(), "100".to_owned())])
        );
        assert!(browserslist(d, &["not chrome 100"]).is_none());
    }

    /// Expected values from core-js-compat's target parser over browserslist.
    #[test]
    fn targets() {
        let d = data();
        let resolve = |q: &str| {
            core_js_targets(d, &Value::String(q.to_owned())).map(|t| {
                t.into_iter()
                    .map(|(e, v)| format!("{} {}.{}.{}", d.engines[e as usize], v.0, v.1, v.2))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        };
        for (q, expected) in [
            ("node >16", "node 16.1.0"),
            ("node 10 - 12", "node 10.0.0"),
            ("node 6", "node 6.17.0"),
            ("node 6.9", "node 6.9.0"),
            (
                "chrome >= 100, firefox 120",
                "chrome 100.0.0, firefox 120.0.0",
            ),
            ("safari 15 - 16", "safari 15.0.0"),
            ("ios 14", "ios 14.0.0"),
            ("iOS 14, not ios 14", ""),
            ("chrome 100 and chrome >99", "chrome 100.0.0"),
            ("and_chr 100", "chrome-android 156.0.0"),
            ("op_mini all, node 18", "node 18.20.0"),
            ("edge >120", "edge 121.0.0"),
            ("Samsung 20", "samsung 20.0.0"),
        ] {
            assert_eq!(resolve(q).as_deref(), Some(expected), "{q}");
        }
        assert_eq!(resolve("defaults"), None);
        assert_eq!(resolve("> 0.5%"), None);
    }
}
