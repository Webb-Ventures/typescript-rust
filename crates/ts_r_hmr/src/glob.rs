//! `import.meta.glob` (Vite's glob import, from its documentation and observed output; no
//! Vite code).
//!
//! `import.meta.glob(patterns, { eager, import, query, base, exhaustive })` with literal
//! arguments becomes an object of the matching files, keyed like the pattern (`./pages/a.js`
//! for relative patterns, `/src/pages/a.js` for root-absolute ones), sorted by key:
//!
//! - lazy (default): `{"./a.js": () => import("./a.js")}` (with `import: 'x'`:
//!   `.then(m => m["x"])`);
//! - `eager: true`: static imports prepended on line 1 (`import * as __vite_glob_0_0 from
//!   ...`, or `import { x as ... }` with `import`), values are the bindings.
//!
//! Patterns: `*`, `**`, `?`, `[abc]`, `{a,b}`, `!negated`; relative (`./`, `../`), root
//! absolute (`/src/...`) or through `resolve.alias` (the caller's `alias` callback). Dot
//! files and `node_modules` are skipped unless `exhaustive` (or the pattern names them).
//! The importing file itself is never included. The replacement keeps the call's line
//! count (padded with newlines) so source maps only shift columns.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::render::js_string;
use crate::scan::GlobCall;

/// Most files one glob may produce before it is cut off with an error.
const MAX_FILES: usize = 20_000;
/// Deepest directory walk.
const MAX_DEPTH: usize = 64;
/// Most patterns one pattern's `{a,b}` groups may expand to.
const MAX_ALTERNATIVES: usize = 1024;

/// Parsed `import.meta.glob` options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobOptions {
    pub eager: bool,
    /// `import: 'name'` (`'*'` = the namespace).
    pub import: Option<String>,
    /// The query without `?` (`raw`, `foo=bar&n=1`).
    pub query: String,
    pub base: Option<String>,
    pub exhaustive: bool,
}

/// A compiled pattern for matching paths later (watching for added/removed files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobWatch {
    positive: Vec<Vec<String>>,
    negative: Vec<Vec<String>>,
    exhaustive: bool,
    importer: PathBuf,
}

impl GlobWatch {
    /// Whether a file at `path` would be part of this glob's result.
    pub fn matches(&self, path: &Path) -> bool {
        if path == self.importer {
            return false;
        }
        let segs = path_segments(path);
        if !self.exhaustive && !self.allows_hidden(&segs) {
            return false;
        }
        self.positive.iter().any(|p| match_segments(p, &segs))
            && !self.negative.iter().any(|p| match_segments(p, &segs))
    }

    fn allows_hidden(&self, segs: &[String]) -> bool {
        segs.iter().enumerate().all(|(i, s)| {
            let hidden = s.starts_with('.') || s == "node_modules";
            !hidden
                || self.positive.iter().any(|p| {
                    p.get(i)
                        .is_some_and(|ps| ps == s || (ps.starts_with('.') && ps != "**"))
                })
        })
    }
}

/// One expanded module: code with the calls replaced, the line-1 prelude of eager imports,
/// and the edits (for source map shifting).
#[derive(Clone, Debug, Default)]
pub struct Expanded {
    pub code: String,
    /// Eager imports, prepended to `code` on line 1 (already included in `code`).
    pub prelude: String,
    /// `(start, end, replacement)` in the original code, sorted.
    pub edits: Vec<(usize, usize, String)>,
    /// Every specifier the expansion imports.
    pub specifiers: Vec<String>,
    pub watches: Vec<GlobWatch>,
}

/// Where patterns resolve from.
pub struct GlobContext<'a> {
    pub root: &'a Path,
    /// The importing file.
    pub importer: &'a Path,
    /// `resolve.alias` for patterns that are neither relative nor root-absolute; returns
    /// the rewritten pattern (an absolute file system path, `./`, `../` or `/`).
    pub alias: &'a dyn Fn(&str) -> Option<String>,
}

impl std::fmt::Debug for GlobContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobContext")
            .field("root", &self.root)
            .field("importer", &self.importer)
            .finish_non_exhaustive()
    }
}

/// Expands every call in `calls` (found in `code` by the scanner). Errors carry the byte
/// offset of the failing call.
pub fn expand(
    code: &str,
    calls: &[GlobCall],
    cx: &GlobContext<'_>,
) -> Result<Expanded, (usize, String)> {
    let mut out = Expanded::default();
    let importer_dir = cx.importer.parent().unwrap_or(cx.root);
    for (i, call) in calls.iter().enumerate() {
        let (s, e) = (call.start as usize, call.end as usize);
        let args = code
            .get(call.args_start as usize..call.args_end as usize)
            .ok_or((s, "invalid glob call".to_owned()))?;
        let (patterns, opts) = parse_args(args).map_err(|m| (s, m))?;
        let base_dir = match &opts.base {
            Some(b) => resolve_dir(b, importer_dir, cx).map_err(|m| (s, m))?,
            None => importer_dir.to_path_buf(),
        };
        let mut positive = Vec::new();
        let mut negative = Vec::new();
        let mut keyed_root = false;
        for p in &patterns {
            let (neg, body) = match p.strip_prefix('!') {
                Some(b) => (true, b),
                None => (false, p.as_str()),
            };
            let abs = resolve_pattern(body, &base_dir, cx).map_err(|m| (s, m))?;
            if !neg && body.starts_with('/') {
                keyed_root = true;
            }
            let mut expanded = Vec::new();
            expand_braces(&abs, 0, &mut expanded).map_err(|m| (s, m))?;
            for one in expanded {
                let segs = path_segments(Path::new(&one));
                if neg {
                    negative.push(segs);
                } else {
                    positive.push(segs);
                }
            }
        }
        let watch = GlobWatch {
            positive,
            negative,
            exhaustive: opts.exhaustive,
            importer: cx.importer.to_path_buf(),
        };
        let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
        for p in &watch.positive {
            collect(p, &watch, &mut files, cx.root, &base_dir, keyed_root).map_err(|m| (s, m))?;
        }
        let query = if opts.query.is_empty() {
            String::new()
        } else {
            format!("?{}", opts.query)
        };
        let mut entries = Vec::with_capacity(files.len());
        for (j, (key, file)) in files.iter().enumerate() {
            let spec = format!("{}{query}", relative_specifier(importer_dir, file));
            let value = if opts.eager {
                let name = format!("__vite_glob_{i}_{j}");
                match opts.import.as_deref() {
                    None | Some("*") => {
                        out.prelude
                            .push_str(&format!("import * as {name} from {};", js_string(&spec)));
                    }
                    Some(imp) => {
                        out.prelude.push_str(&format!(
                            "import {{ {} as {name} }} from {};",
                            if is_identifier(imp) {
                                imp.to_owned()
                            } else {
                                js_string(imp)
                            },
                            js_string(&spec)
                        ));
                    }
                }
                name
            } else {
                match opts.import.as_deref() {
                    None | Some("*") => format!("() => import({})", js_string(&spec)),
                    Some(imp) => format!(
                        "() => import({}).then(m => m[{}])",
                        js_string(&spec),
                        js_string(imp)
                    ),
                }
            };
            entries.push(format!("{}: {value}", js_string(key)));
            out.specifiers.push(spec);
        }
        let newlines = code[s..e].bytes().filter(|&b| b == b'\n').count();
        let mut text = format!("/* #__PURE__ */ Object.assign({{{}}})", entries.join(","));
        text.extend(std::iter::repeat_n('\n', newlines));
        out.edits.push((s, e, text));
        out.watches.push(watch);
    }
    let mut code_out = String::with_capacity(code.len() + out.prelude.len() + 64);
    code_out.push_str(&out.prelude);
    let mut at = 0;
    for (s, e, t) in &out.edits {
        if *s < at {
            continue;
        }
        code_out.push_str(&code[at..*s]);
        code_out.push_str(t);
        at = *e;
    }
    code_out.push_str(&code[at..]);
    out.code = code_out;
    Ok(out)
}

/// Turns a pattern body (no `!`) into an absolute path pattern with `/` separators.
fn resolve_pattern(body: &str, base: &Path, cx: &GlobContext<'_>) -> Result<String, String> {
    if body.starts_with("./") || body.starts_with("../") {
        return Ok(join_pattern(base, body));
    }
    if let Some(rest) = body.strip_prefix('/') {
        // Already an absolute path inside the root (from an alias), else root-relative.
        if Path::new(body).starts_with(cx.root) {
            return Ok(body.to_owned());
        }
        return Ok(join_pattern(cx.root, &format!("./{rest}")));
    }
    if let Some(r) = (cx.alias)(body)
        && r != body
    {
        if Path::new(&r).is_absolute() && Path::new(&r).starts_with(cx.root) {
            return Ok(r.replace('\\', "/"));
        }
        if r.starts_with("./") || r.starts_with("../") || r.starts_with('/') {
            return resolve_pattern(&r, base, cx);
        }
    }
    Err(format!(
        "Invalid glob import pattern \"{body}\": it must start with ./, ../ or / (or an alias)"
    ))
}

fn resolve_dir(b: &str, importer_dir: &Path, cx: &GlobContext<'_>) -> Result<PathBuf, String> {
    let p = resolve_pattern(b, importer_dir, cx)?;
    Ok(PathBuf::from(p))
}

/// `base` joined with a `./`/`../` pattern, normalized (`..` applied), `/` separators.
fn join_pattern(base: &Path, rel: &str) -> String {
    let mut parts: Vec<String> = path_segments(base);
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s.to_owned()),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The segments of an absolute path. A Windows prefix (`C:`, `\\srv\share`) is the first
/// segment, so patterns and paths on another drive never match and walks start on the
/// right drive; Unix paths have none.
fn path_segments(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            Component::Prefix(s) => Some(s.as_os_str().to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The absolute path for `segs` (the inverse of [`path_segments`]).
fn path_of(segs: &[String]) -> PathBuf {
    let mut p = PathBuf::from("/");
    for (i, s) in segs.iter().enumerate() {
        if i == 0 && matches!(Path::new(s).components().next(), Some(Component::Prefix(_))) {
            // `C:` alone is drive-relative: add the root.
            p = PathBuf::from(format!("{s}{}", std::path::MAIN_SEPARATOR));
        } else {
            p.push(s);
        }
    }
    p
}

/// `{a,b}` expansion (nested, bounded) into `out`; an error past [`MAX_ALTERNATIVES`].
fn expand_braces(p: &str, depth: u32, out: &mut Vec<String>) -> Result<(), String> {
    let Some(open) = p.find('{') else {
        return push_alternative(p, out);
    };
    let mut level = 0;
    let mut close = None;
    let mut commas = Vec::new();
    for (i, c) in p[open..].char_indices() {
        match c {
            '{' => level += 1,
            '}' => {
                level -= 1;
                if level == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            ',' if level == 1 => commas.push(open + i),
            _ => {}
        }
    }
    let Some(close) = close.filter(|_| depth < 8 && !commas.is_empty()) else {
        return push_alternative(p, out);
    };
    let mut alts = Vec::new();
    let mut start = open + 1;
    for c in commas.iter().copied().chain(std::iter::once(close)) {
        alts.push(&p[start..c]);
        start = c + 1;
    }
    let (pre, post) = (&p[..open], &p[close + 1..]);
    for a in alts {
        expand_braces(&format!("{pre}{a}{post}"), depth + 1, out)?;
    }
    Ok(())
}

fn push_alternative(p: &str, out: &mut Vec<String>) -> Result<(), String> {
    if out.len() >= MAX_ALTERNATIVES {
        return Err(format!(
            "glob pattern expands to more than {MAX_ALTERNATIVES} alternatives"
        ));
    }
    out.push(p.to_owned());
    Ok(())
}

fn has_magic(seg: &str) -> bool {
    seg.contains(['*', '?', '['])
}

/// Walks from the pattern's literal prefix and records matching files by key.
fn collect(
    pattern: &[String],
    watch: &GlobWatch,
    files: &mut BTreeMap<String, PathBuf>,
    root: &Path,
    base: &Path,
    keyed_root: bool,
) -> Result<(), String> {
    let prefix_len = pattern.iter().take_while(|s| !has_magic(s)).count();
    let dir = path_of(&pattern[..prefix_len.min(pattern.len().saturating_sub(1))]);
    if prefix_len == pattern.len() {
        // No wildcard at all: one file.
        let file = path_of(pattern);
        if file.is_file() && watch.matches(&file) {
            files.insert(key_for(&file, root, base, keyed_root), file);
        }
        return Ok(());
    }
    let globstar = pattern.iter().any(|s| s == "**");
    let max_depth = if globstar {
        MAX_DEPTH
    } else {
        pattern.len() - prefix_len
    };
    // The real path of every directory walked and its parent's index, so a symlink back
    // to a directory on the walk's own path (`node_modules/a -> .`) is not followed.
    let mut real: Vec<(PathBuf, Option<usize>)> = Vec::new();
    let Ok(top) = fs::canonicalize(&dir) else {
        return Ok(());
    };
    real.push((top, None));
    let mut stack = vec![(dir, 0usize, 0usize)];
    while let Some((d, depth, at)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let Ok(ft) = e.file_type() else {
                continue;
            };
            let is_dir = ft.is_dir() || (ft.is_symlink() && path.is_dir());
            if is_dir {
                // Nothing under an excluded directory can match.
                if depth + 1 >= max_depth
                    || (!watch.exhaustive && !watch.allows_hidden(&path_segments(&path)))
                {
                    continue;
                }
                let r = if ft.is_symlink() {
                    let Ok(r) = fs::canonicalize(&path) else {
                        continue;
                    };
                    r
                } else {
                    real[at].0.join(e.file_name())
                };
                let mut up = Some(at);
                let mut cycle = false;
                while let Some(i) = up {
                    cycle |= real[i].0 == r;
                    up = real[i].1;
                }
                if !cycle {
                    real.push((r, Some(at)));
                    stack.push((path, depth + 1, real.len() - 1));
                }
            } else if watch.matches(&path) {
                files.insert(key_for(&path, root, base, keyed_root), path);
                if files.len() > MAX_FILES {
                    return Err(format!("glob matched more than {MAX_FILES} files"));
                }
            }
        }
    }
    Ok(())
}

fn key_for(file: &Path, root: &Path, base: &Path, keyed_root: bool) -> String {
    if keyed_root && let Ok(rel) = file.strip_prefix(root) {
        return format!("/{}", path_segments(rel).join("/"));
    }
    relative_specifier(base, file)
}

/// `./x/y.js` or `../x.js` from directory `from` to `to`.
pub fn relative_specifier(from: &Path, to: &Path) -> String {
    let a = path_segments(from);
    let b = path_segments(to);
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let ups = a.len() - common;
    let rest = b[common..].join("/");
    if ups == 0 {
        format!("./{rest}")
    } else {
        format!("{}{rest}", "../".repeat(ups))
    }
}

/// Segment-wise match with `**` (any number of segments).
fn match_segments(pattern: &[String], path: &[String]) -> bool {
    // dp[j] = pattern[..i] matches path[..j]
    let mut dp = vec![false; path.len() + 1];
    dp[0] = true;
    for p in pattern {
        let mut next = vec![false; path.len() + 1];
        if p == "**" {
            let mut any = false;
            for j in 0..=path.len() {
                any |= dp[j];
                next[j] = any;
            }
        } else {
            for j in 1..=path.len() {
                next[j] = dp[j - 1] && match_segment(p.as_bytes(), path[j - 1].as_bytes());
            }
        }
        dp = next;
    }
    dp[path.len()]
}

/// One segment: `*`, `?`, `[abc]`/`[a-z]`/`[!x]`, literal text.
fn match_segment(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    star = Some(pi);
                    mark = si;
                    pi += 1;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                b'[' => {
                    if let Some((ok, len)) = class(&p[pi..], s[si])
                        && ok
                    {
                        pi += len;
                        si += 1;
                        continue;
                    }
                }
                b'\\' if pi + 1 < p.len() && p[pi + 1] == s[si] => {
                    pi += 2;
                    si += 1;
                    continue;
                }
                c if c == s[si] => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some(st) => {
                pi = st + 1;
                mark += 1;
                si = mark;
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// A `[...]` class at the start of `p`: (matches `c`, length of the class).
fn class(p: &[u8], c: u8) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = matches!(p.get(1), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == b']' && !first {
            return Some((hit != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            if p[i] <= c && c <= p[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                hit = true;
            }
            i += 1;
        }
    }
    None
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

// ---------------------------------------------------------------------------------------
// Arguments: a JS literal subset (strings, arrays, objects, numbers, booleans, null).

/// Parses `patterns[, options]`.
pub fn parse_args(text: &str) -> Result<(Vec<String>, GlobOptions), String> {
    let mut p = Lit {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let first = p.value()?;
    let patterns: Vec<String> = match &first {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "glob patterns must be string literals".to_owned())
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("import.meta.glob() needs a string or an array of strings".to_owned()),
    };
    if patterns.is_empty() {
        return Err("import.meta.glob() needs at least one pattern".to_owned());
    }
    let mut opts = GlobOptions::default();
    p.ws();
    if p.peek() == Some(b',') {
        p.i += 1;
        p.ws();
        if p.peek().is_some() {
            let o = p.value()?;
            let Value::Object(m) = o else {
                return Err("import.meta.glob() options must be an object literal".to_owned());
            };
            for (k, v) in m {
                match (k.as_str(), v) {
                    ("eager", Value::Bool(b)) => opts.eager = b,
                    ("exhaustive", Value::Bool(b)) => opts.exhaustive = b,
                    ("import", Value::String(s)) => opts.import = Some(s),
                    ("base", Value::String(s)) => opts.base = Some(s),
                    ("as", Value::String(s)) => {
                        opts.query = s;
                        opts.import = Some("default".to_owned());
                    }
                    ("query", Value::String(s)) => {
                        opts.query = s.trim_start_matches('?').to_owned()
                    }
                    ("query", Value::Object(q)) => {
                        opts.query = q
                            .iter()
                            .map(|(k, v)| match v {
                                Value::String(s) => format!("{k}={s}"),
                                other => format!("{k}={other}"),
                            })
                            .collect::<Vec<_>>()
                            .join("&");
                    }
                    (k, _) => {
                        return Err(format!("unsupported import.meta.glob() option `{k}`"));
                    }
                }
            }
            p.ws();
            if p.peek() == Some(b',') {
                p.i += 1;
            }
        }
    }
    p.ws();
    if p.peek().is_some() {
        return Err("import.meta.glob() takes at most two literal arguments".to_owned());
    }
    Ok((patterns, opts))
}

struct Lit<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

impl Lit<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.i += 1,
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'/') => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    while self.i < self.s.len()
                        && !(self.s[self.i] == b'*' && self.s.get(self.i + 1) == Some(&b'/'))
                    {
                        self.i += 1;
                    }
                    self.i = (self.i + 2).min(self.s.len());
                }
                _ => return,
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        if self.depth > 16 {
            return Err("glob options nest too deeply".to_owned());
        }
        match self.peek() {
            Some(b'"' | b'\'' | b'`') => self.string().map(Value::String),
            Some(b'[') => {
                self.i += 1;
                self.depth += 1;
                let mut out = Vec::new();
                loop {
                    self.ws();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        break;
                    }
                    out.push(self.value()?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return Err("expected , or ] in glob arguments".to_owned()),
                    }
                }
                self.depth -= 1;
                Ok(Value::Array(out))
            }
            Some(b'{') => {
                self.i += 1;
                self.depth += 1;
                let mut out = serde_json::Map::new();
                loop {
                    self.ws();
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        break;
                    }
                    let key = match self.peek() {
                        Some(b'"' | b'\'') => self.string()?,
                        _ => self.word(),
                    };
                    if key.is_empty() {
                        return Err("glob options must be literal properties".to_owned());
                    }
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(format!("expected : after `{key}` in glob options"));
                    }
                    self.i += 1;
                    let v = self.value()?;
                    out.insert(key, v);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {}
                        _ => return Err("expected , or } in glob options".to_owned()),
                    }
                }
                self.depth -= 1;
                Ok(Value::Object(out))
            }
            _ => {
                let w = self.word();
                match w.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Null),
                    _ => w
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(|n| {
                            if w.parse::<i64>().is_ok() {
                                Value::from(w.parse::<i64>().unwrap_or_default())
                            } else {
                                Value::Number(n)
                            }
                        })
                        .ok_or_else(|| {
                            format!(
                                "import.meta.glob() arguments must be literals (found `{}`)",
                                if w.is_empty() { "?" } else { &w }
                            )
                        }),
                }
            }
        }
    }

    fn word(&mut self) -> String {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'.' | b'-'))
        {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }

    fn string(&mut self) -> Result<String, String> {
        let q = self.s[self.i];
        self.i += 1;
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == q {
                return String::from_utf8(out).map_err(|_| "invalid UTF-8".to_owned());
            }
            if q == b'`' && c == b'$' && self.peek() == Some(b'{') {
                return Err("glob patterns can't use template substitutions".to_owned());
            }
            if c == b'\\' {
                let Some(n) = self.peek() else { break };
                self.i += 1;
                out.push(match n {
                    b'n' => b'\n',
                    b't' => b'\t',
                    other => other,
                });
            } else {
                out.push(c);
            }
        }
        Err("unterminated string in glob arguments".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_hot;
    use crate::testutil::TempDir;

    fn segs(p: &str) -> Vec<String> {
        p.split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn matches_segments_and_classes() {
        assert!(match_segments(&segs("/a/**/*.js"), &segs("/a/x.js")));
        assert!(match_segments(&segs("/a/**/*.js"), &segs("/a/b/c/x.js")));
        assert!(!match_segments(&segs("/a/*.js"), &segs("/a/b/x.js")));
        assert!(match_segment(b"[ab]?.t*", b"bx.ts"));
        assert!(!match_segment(b"[!ab]x", b"ax"));
        assert!(match_segment(b"[a-c]", b"b"));
        let mut out = Vec::new();
        expand_braces("/p/*.{js,ts}", 0, &mut out).unwrap();
        assert_eq!(out, vec!["/p/*.js", "/p/*.ts"]);
        assert_eq!(
            relative_specifier(Path::new("/r/src"), Path::new("/r/lib/a.js")),
            "../lib/a.js"
        );
    }

    #[test]
    fn brace_expansion_is_bounded() {
        // 10^8 alternatives if expanded in full.
        let mut out = Vec::new();
        let p = format!("/p/{}*.js", "{a,b,c,d,e,f,g,h,i,j}".repeat(8));
        let err = expand_braces(&p, 0, &mut out).unwrap_err();
        assert!(err.contains("more than 1024 alternatives"), "{err}");
        assert_eq!(out.len(), MAX_ALTERNATIVES);
    }

    /// Symlinks back up the walk's own path (`node_modules/a -> ..`) are not followed, so
    /// they neither loop nor branch the walk; other directory links are.
    #[cfg(unix)]
    #[test]
    fn walk_skips_symlink_cycles() {
        let dir = TempDir::new("glob-cycle");
        dir.write("src/a.js", "");
        dir.write("src/sub/b.js", "");
        dir.write("other/c.js", "");
        let src = dir.path().join("src");
        for (link, to) in [
            ("node_modules/a", "src"),
            ("node_modules/b", "src"),
            ("sub/up", "src"),
            ("sub/up2", "src"),
            ("linked", "other"),
        ] {
            let at = src.join(link);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(dir.path().join(to), at).unwrap();
        }
        let importer = dir.path().join("main.js");
        let code = "import.meta.glob('./src/**/*.js');\n\
                    import.meta.glob('./src/**/*.js', { exhaustive: true });\n";
        let no_alias = |_: &str| None;
        let cx = GlobContext {
            root: dir.path(),
            importer: &importer,
            alias: &no_alias,
        };
        let out = expand(code, &scan_hot(code).globs, &cx).unwrap();
        let want = ["./src/a.js", "./src/linked/c.js", "./src/sub/b.js"];
        assert_eq!(out.specifiers[..3], want);
        assert_eq!(out.specifiers[3..], want);
    }

    #[test]
    fn parses_arguments() {
        let (p, o) =
            parse_args("['./a/*.js', \"!./a/b.js\"], { eager: true, import: 'named', query: { foo: 'bar', n: 1 } }")
                .unwrap();
        assert_eq!(p, vec!["./a/*.js", "!./a/b.js"]);
        assert!(o.eager);
        assert_eq!(o.import.as_deref(), Some("named"));
        assert_eq!(o.query, "foo=bar&n=1");
        let (_, o) = parse_args("'./x/*.txt', { query: '?raw', import: 'default', }").unwrap();
        assert_eq!(o.query, "raw");
        assert!(parse_args("dir + '/*.js'").is_err());
        assert!(parse_args("`./${x}/*.js`").is_err());
        assert!(parse_args("'./a', { eager: maybe }").is_err());
        assert!(parse_args("'./a', { what: 1 }").is_err());
        let _ = parse_args("[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[");
    }

    /// The output shapes Vite 8.3.4 produced for the same calls (keys, sorting, eager
    /// bindings, `import` and `query` options, root-absolute keys, the importer excluded).
    #[test]
    fn expands_like_vite() {
        let dir = TempDir::new("glob");
        for f in [
            "src/pages/a.js",
            "src/pages/b.ts",
            "src/pages/sub/c.js",
            "src/pages/.hidden.js",
            "src/pages/node_modules/x.js",
            "src/assets/note.txt",
        ] {
            dir.write(f, "export default 1\n");
        }
        let importer = dir.path().join("src/main.js");
        let code = "const g1 = import.meta.glob('./pages/*.js')\n\
            const g2 = import.meta.glob(['./pages/**/*.{js,ts}', '!./pages/b.ts'], {\n  eager: true,\n})\n\
            const g4 = import.meta.glob('./pages/*.js', { import: 'default' })\n\
            const g5 = import.meta.glob('./assets/*.txt', { query: '?raw', import: 'default', eager: true })\n\
            const g6 = import.meta.glob('/src/pages/sub/*.js')\n\
            const g7 = import.meta.glob('./*.js')\n";
        let hot = scan_hot(code);
        assert_eq!(hot.globs.len(), 6);
        let no_alias = |_: &str| None;
        let cx = GlobContext {
            root: dir.path(),
            importer: &importer,
            alias: &no_alias,
        };
        let out = expand(code, &hot.globs, &cx).unwrap();
        let lines: Vec<&str> = out.code.lines().collect();
        assert_eq!(
            lines[0],
            "import * as __vite_glob_1_0 from \"./pages/a.js\";import * as __vite_glob_1_1 from \"./pages/sub/c.js\";import { default as __vite_glob_3_0 } from \"./assets/note.txt?raw\";const g1 = /* #__PURE__ */ Object.assign({\"./pages/a.js\": () => import(\"./pages/a.js\")})"
        );
        assert_eq!(
            lines[1],
            "const g2 = /* #__PURE__ */ Object.assign({\"./pages/a.js\": __vite_glob_1_0,\"./pages/sub/c.js\": __vite_glob_1_1})"
        );
        // Line count kept: the multi-line call left two empty lines.
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "");
        assert_eq!(
            lines[4],
            "const g4 = /* #__PURE__ */ Object.assign({\"./pages/a.js\": () => import(\"./pages/a.js\").then(m => m[\"default\"])})"
        );
        assert_eq!(
            lines[5],
            "const g5 = /* #__PURE__ */ Object.assign({\"./assets/note.txt\": __vite_glob_3_0})"
        );
        assert_eq!(
            lines[6],
            "const g6 = /* #__PURE__ */ Object.assign({\"/src/pages/sub/c.js\": () => import(\"./pages/sub/c.js\")})"
        );
        // The importer itself is never part of its own glob.
        assert_eq!(lines[7], "const g7 = /* #__PURE__ */ Object.assign({})");
        assert_eq!(out.code.lines().count(), code.lines().count());
        assert!(out.watches[0].matches(&dir.path().join("src/pages/new.js")));
        assert!(!out.watches[0].matches(&dir.path().join("src/pages/new.ts")));
        assert!(!out.watches[1].matches(&dir.path().join("src/pages/b.ts")));
        assert!(out.watches[1].matches(&dir.path().join("src/pages/x/y/z.ts")));
        assert!(!out.watches[0].matches(&dir.path().join("src/pages/.dot.js")));
        // Bad arguments point at the call.
        let bad = "x;\nimport.meta.glob(name)";
        let err = expand(bad, &scan_hot(bad).globs, &cx).unwrap_err();
        assert_eq!(err.0, 3);
    }
}
