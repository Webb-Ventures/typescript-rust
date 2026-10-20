//! `files` / `include` / `exclude` matching, following tsc 7's observed behaviour:
//!
//! - Each `include` spec is walked in order from its literal base directory. A directory
//!   lists its files (ordinal order) before recursing into subdirectories (ordinal order).
//! - A spec whose last segment has no `.` and no wildcard names a directory (`src` means
//!   `src/**/*`).
//! - Wildcards (`*`, `?`, `**`) never match names starting with `.`, and wildcard directory
//!   segments never match `node_modules`, `bower_components` or `jspm_packages`. Literal
//!   segments match anything.
//! - `*.min.js` files are skipped when the file segment is a wildcard.
//! - Only supported extensions are included; `.json` only when the spec's last segment ends in
//!   `.json`. JSON matches are listed after all other matches.
//! - Within one base name, higher-priority extensions displace lower ones (`.ts` beats
//!   `.tsx`, `.d.ts`, `.js`, `.jsx`; `.d.ts` never displaces `.js`/`.jsx`), with the
//!   ordering quirks tsc shows for `.d.mts`/`.d.cts`.
//! - An `exclude` spec excludes matching paths and everything under matching directories.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::fs::{DirEntry, EntryKind, FileSystem};
use crate::path;

/// One segment of a compiled glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// No wildcard characters.
    Literal(String),
    /// Contains `*` or `?`.
    Wild(String),
    /// `**`.
    Globstar,
}

/// A compiled include or exclude pattern (absolute).
#[derive(Debug, Clone)]
pub struct Pattern {
    root: String,
    segs: Vec<Seg>,
    /// The literal directory to start walking from.
    pub base_dir: String,
    /// The last segment ends with `.json`.
    pub json: bool,
    /// The file segment contains a wildcard.
    wild_file: bool,
}

const IMPLICIT_EXCLUDED_DIRS: &[&str] = &["node_modules", "bower_components", "jspm_packages"];

fn has_wildcard(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

impl Pattern {
    /// Compiles an absolute spec. `is_include` applies the "last segment without extension is
    /// a directory" rule.
    pub fn new(abs_spec: &str, is_include: bool) -> Self {
        let spec = path::normalize(abs_spec);
        let (root, comps) = path::components(&spec);
        let mut segs: Vec<Seg> = comps
            .iter()
            .map(|c| {
                if *c == "**" {
                    Seg::Globstar
                } else if has_wildcard(c) {
                    Seg::Wild((*c).to_owned())
                } else {
                    Seg::Literal((*c).to_owned())
                }
            })
            .collect();
        if is_include {
            match segs.last() {
                Some(Seg::Literal(l)) if !l.contains('.') => {
                    segs.push(Seg::Globstar);
                    segs.push(Seg::Wild("*".into()));
                }
                Some(Seg::Globstar) => segs.push(Seg::Wild("*".into())),
                None => {
                    segs.push(Seg::Globstar);
                    segs.push(Seg::Wild("*".into()));
                }
                _ => {}
            }
        }
        let mut base = root.to_owned();
        let literal_prefix = segs
            .iter()
            .take(segs.len().saturating_sub(1))
            .take_while(|s| matches!(s, Seg::Literal(_)))
            .count();
        for s in &segs[..literal_prefix] {
            if let Seg::Literal(l) = s {
                base = path::combine(&base, l);
            }
        }
        let json = match segs.last() {
            Some(Seg::Literal(l)) | Some(Seg::Wild(l)) => l.to_ascii_lowercase().ends_with(".json"),
            _ => false,
        };
        let wild_file = matches!(segs.last(), Some(Seg::Wild(_)) | Some(Seg::Globstar));
        Pattern {
            root: root.to_owned(),
            segs,
            base_dir: base,
            json,
            wild_file,
        }
    }

    fn rel_components<'p>(&self, p: &'p str, cs: bool) -> Option<Vec<&'p str>> {
        let (root, comps) = path::components(p);
        if !path::eq(root, &self.root, cs) {
            return None;
        }
        Some(comps)
    }

    /// Full match of a file or directory path.
    pub fn matches(&self, p: &str, cs: bool) -> bool {
        let Some(comps) = self.rel_components(p, cs) else {
            return false;
        };
        match_segs(&self.segs, &comps, cs, Mode::Full)
    }

    /// The pattern matches `p` or one of its ancestors (exclude semantics).
    pub fn matches_self_or_ancestor(&self, p: &str, cs: bool) -> bool {
        let Some(comps) = self.rel_components(p, cs) else {
            return false;
        };
        match_segs(&self.segs, &comps, cs, Mode::Prefix)
    }

    /// Some descendant of directory `dir` could match (used to prune the walk).
    pub fn could_match_under(&self, dir: &str, cs: bool) -> bool {
        let Some(comps) = self.rel_components(dir, cs) else {
            return false;
        };
        match_segs(&self.segs, &comps, cs, Mode::DirPrefix)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Pattern and path must both be fully consumed.
    Full,
    /// Pattern fully consumed; path may have extra components (exclude of a directory).
    Prefix,
    /// Path fully consumed; pattern may have extra segments (directory could contain a match).
    DirPrefix,
}

fn wild_name_ok(name: &str, is_dir: bool) -> bool {
    if name.starts_with('.') {
        return false;
    }
    if is_dir && IMPLICIT_EXCLUDED_DIRS.contains(&name) {
        return false;
    }
    true
}

fn match_segs(segs: &[Seg], comps: &[&str], cs: bool, mode: Mode) -> bool {
    match (segs.first(), comps.first()) {
        (None, None) => true,
        (None, Some(_)) => mode == Mode::Prefix,
        (Some(_), None) => mode == Mode::DirPrefix,
        (Some(seg), Some(comp)) => {
            let is_last_comp = comps.len() == 1;
            // A component is a directory unless it is the last one in a full match.
            let comp_is_dir = !is_last_comp || mode == Mode::DirPrefix;
            match seg {
                Seg::Literal(l) => {
                    path::eq(l, comp, cs) && match_segs(&segs[1..], &comps[1..], cs, mode)
                }
                Seg::Wild(w) => {
                    wild_name_ok(comp, comp_is_dir)
                        && glob_component(w, comp, cs)
                        && match_segs(&segs[1..], &comps[1..], cs, mode)
                }
                Seg::Globstar => {
                    // Zero directories.
                    if match_segs(&segs[1..], comps, cs, mode) {
                        return true;
                    }
                    // One or more directories: consume `comp` as a directory.
                    if comps.len() > 1 || mode == Mode::DirPrefix {
                        return wild_name_ok(comp, true) && match_segs(segs, &comps[1..], cs, mode);
                    }
                    false
                }
            }
        }
    }
}

/// Matches one path component against `*`/`?` wildcards.
fn glob_component(pat: &str, name: &str, cs: bool) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = if cs {
        (pat.chars().collect(), name.chars().collect())
    } else {
        (
            pat.to_lowercase().chars().collect(),
            name.to_lowercase().chars().collect(),
        )
    };
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star_p, mut star_n) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = pi;
            star_n = ni;
            pi += 1;
        } else if star_p != usize::MAX {
            pi = star_p + 1;
            star_n += 1;
            ni = star_n;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Extension groups used for "higher priority extension" displacement.
fn extension_groups(allow_js: bool) -> Vec<Vec<&'static str>> {
    if allow_js {
        vec![
            vec![".ts", ".tsx", ".d.ts", ".js", ".jsx"],
            vec![".cts", ".d.cts", ".cjs"],
            vec![".mts", ".d.mts", ".mjs"],
        ]
    } else {
        vec![
            vec![".ts", ".tsx", ".d.ts"],
            vec![".cts", ".d.cts"],
            vec![".mts", ".d.mts"],
        ]
    }
}

/// The real extension of a file among the supported ones (longest match).
fn true_extension(name: &str) -> Option<&'static str> {
    const ORDER: &[&str] = &[
        ".d.ts", ".d.cts", ".d.mts", ".ts", ".tsx", ".cts", ".mts", ".js", ".jsx", ".cjs", ".mjs",
        ".json",
    ];
    ORDER.iter().copied().find(|e| path::ext::has(name, e))
}

/// True when `name` has an extension tsc picks up from `include`.
pub fn is_supported(name: &str, allow_js: bool) -> bool {
    match true_extension(name) {
        Some(".js" | ".jsx" | ".cjs" | ".mjs") => allow_js,
        Some(".json") | None => false,
        Some(_) => true,
    }
}

/// Inputs for [`match_files`].
#[derive(Debug, Clone)]
pub struct MatchInput<'a> {
    /// Absolute include specs, in order.
    pub include: &'a [String],
    /// Absolute exclude specs.
    pub exclude: &'a [String],
    /// `allowJs` (effective).
    pub allow_js: bool,
    /// Whether to include `.json` files matched by `*.json` specs.
    pub allow_json: bool,
}

struct Collector {
    files: Vec<Option<String>>,
    keys: std::collections::HashMap<String, usize>,
    json: Vec<String>,
    json_keys: HashSet<String>,
    groups: Vec<Vec<&'static str>>,
    cs: bool,
}

impl Collector {
    fn key(&self, p: &str) -> String {
        path::key(p, self.cs)
    }

    fn add(&mut self, file: &str) {
        let k = self.key(file);
        if self.keys.contains_key(&k) {
            return;
        }
        let name = path::file_name(file);
        let Some(ext) = true_extension(name) else {
            return;
        };
        let group = self.groups.iter().find(|g| g.contains(&ext)).cloned();
        if let Some(group) = group {
            let stem = &file[..file.len() - ext.len()];
            // Higher-priority sibling already present?
            for e in &group {
                let is_this = if *e == ".ts" {
                    path::ext::has(name, ".ts") && !path::ext::has(name, ".d.ts")
                } else {
                    path::ext::has(name, e)
                };
                if is_this {
                    break;
                }
                let sibling = self.key(&format!("{stem}{e}"));
                if self.keys.contains_key(&sibling) {
                    if *e == ".d.ts" && matches!(ext, ".js" | ".jsx") {
                        continue;
                    }
                    return;
                }
            }
            // Displace lower-priority siblings.
            if let Some(idx) = group.iter().position(|e| *e == ext) {
                for e in &group[idx + 1..] {
                    let sibling = self.key(&format!("{stem}{e}"));
                    if let Some(i) = self.keys.remove(&sibling) {
                        self.files[i] = None;
                    }
                }
            }
        }
        self.keys.insert(k, self.files.len());
        self.files.push(Some(file.to_owned()));
    }
}

/// Expands include/exclude specs against the file system. Returns absolute file paths in
/// tsc's order (wildcard matches, then JSON matches).
pub fn match_files(fs: &dyn FileSystem, input: &MatchInput<'_>) -> Vec<String> {
    let cs = fs.case_sensitive();
    let excludes: Vec<Pattern> = input
        .exclude
        .iter()
        .map(|e| Pattern::new(e, false))
        .collect();
    let mut col = Collector {
        files: Vec::new(),
        keys: Default::default(),
        json: Vec::new(),
        json_keys: HashSet::new(),
        groups: extension_groups(input.allow_js),
        cs,
    };
    for spec in input.include {
        let mut pat = Pattern::new(spec, true);
        if !cs {
            pat.base_dir = disk_case(fs, &pat.base_dir);
        }
        let mut visited = HashSet::new();
        let base = pat.base_dir.clone();
        let real = fs.realpath(&base);
        let tree = prefetch(fs, &pat, (&base, real.clone()), &excludes, cs);
        let mut w = Walk {
            fs,
            pat: &pat,
            excludes: &excludes,
            input,
            tree,
            cs,
        };
        w.walk((&base, real), &mut col, &mut visited);
    }
    let mut out: Vec<String> = col.files.into_iter().flatten().collect();
    out.extend(col.json);
    out
}

/// Rewrites `dir` with the case stored on disk (case-insensitive file systems only).
fn disk_case(fs: &dyn FileSystem, dir: &str) -> String {
    let (root, comps) = path::components(dir);
    let mut cur = root.to_owned();
    for c in comps {
        let name = fs.disk_name(&cur, c);
        cur = path::combine(&cur, name.as_deref().unwrap_or(c));
    }
    cur
}

/// Directories read ahead of the walk, on several threads.
#[derive(Default)]
struct Tree {
    /// Directory -> its entries.
    dirs: HashMap<String, Vec<DirEntry>>,
    /// Subdirectory -> its real path.
    reals: HashMap<String, String>,
}

/// Directories of one level the prefetch reads on a single thread.
const PREFETCH_PARALLEL_MIN: usize = 8;

/// Whether the walk descends into directory `sub`.
fn descends(pat: &Pattern, excludes: &[Pattern], sub: &str, cs: bool) -> bool {
    pat.could_match_under(sub, cs) && !excludes.iter().any(|x| x.matches_self_or_ancestor(sub, cs))
}

/// The real path of directory `sub` (entry `name` of a directory whose real path is
/// `parent_real`). Resolving every directory is the walk's main cost; only symlinks need it.
fn sub_real(fs: &dyn FileSystem, sub: &str, parent_real: &str, name: &str) -> String {
    if fs.may_be_symlink(sub) {
        fs.realpath(sub)
    } else {
        path::combine(parent_real, name)
    }
}

/// What the prefetch reads of one directory: its entries and the subdirectories the walk
/// descends into, with their real paths.
type Read = (Vec<DirEntry>, Vec<(String, String)>);

/// Reads the directories the walk from `dir` can reach, one level at a time, the levels
/// with many directories on up to 4 threads (reading directories one after the other was
/// most of loading a project with thousands of them). Only a cache: the walk decides what
/// is listed and in which order, and reads anything missing itself.
fn prefetch(
    fs: &dyn FileSystem,
    pat: &Pattern,
    (dir, real): (&str, String),
    excludes: &[Pattern],
    cs: bool,
) -> Tree {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let mut tree = Tree::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut level: Vec<(String, String)> = vec![(dir.to_owned(), real)];
    let read = |(dir, real): &(String, String)| -> Read {
        let entries = fs.read_dir(dir);
        let subs = entries
            .iter()
            .filter(|e| e.kind == EntryKind::Directory)
            .filter_map(|e| {
                let sub = path::combine(dir, &e.name);
                if !descends(pat, excludes, &sub, cs) {
                    return None;
                }
                let r = sub_real(fs, &sub, real, &e.name);
                Some((sub, r))
            })
            .collect();
        (entries, subs)
    };
    while !level.is_empty() {
        level.retain(|(_, r)| seen.insert(path::key(r, cs)));
        let results: Vec<Read> = if threads <= 1 || level.len() < PREFETCH_PARALLEL_MIN {
            level.iter().map(read).collect()
        } else {
            let next = AtomicUsize::new(0);
            let out: Vec<Mutex<Option<Read>>> = level.iter().map(|_| Mutex::new(None)).collect();
            std::thread::scope(|s| {
                for _ in 0..threads.min(level.len()) {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(d) = level.get(i) else { break };
                            let r = read(d);
                            *out[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        }
                    });
                }
            });
            out.into_iter()
                .map(|m| {
                    m.into_inner()
                        .unwrap_or_else(|e| e.into_inner())
                        .unwrap_or_default()
                })
                .collect()
        };
        let mut below = Vec::new();
        for ((d, _), (entries, subs)) in level.into_iter().zip(results) {
            for (sub, r) in &subs {
                tree.reals.insert(sub.clone(), r.clone());
            }
            below.extend(subs);
            tree.dirs.insert(d, entries);
        }
        level = below;
    }
    tree
}

/// One include spec's walk over its prefetched [`Tree`].
struct Walk<'x, 'i> {
    fs: &'x dyn FileSystem,
    pat: &'x Pattern,
    excludes: &'x [Pattern],
    input: &'x MatchInput<'i>,
    tree: Tree,
    cs: bool,
}

impl Walk<'_, '_> {
    fn walk(
        &mut self,
        (dir, real): (&str, String),
        col: &mut Collector,
        visited: &mut HashSet<String>,
    ) {
        let (pat, excludes, input, cs) = (self.pat, self.excludes, self.input, self.cs);
        // `real` is `dir` with symlinks resolved: directories reached twice (symlink cycles)
        // are walked once.
        if !visited.insert(path::key(&real, cs)) {
            return;
        }
        let entries = match self.tree.dirs.remove(dir) {
            Some(e) => e,
            None => self.fs.read_dir(dir),
        };
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            let file = path::combine(dir, &e.name);
            if !pat.matches(&file, cs) {
                continue;
            }
            if excludes
                .iter()
                .any(|x| x.matches_self_or_ancestor(&file, cs))
            {
                continue;
            }
            if pat.wild_file && e.name.to_ascii_lowercase().ends_with(".min.js") {
                continue;
            }
            if pat.json {
                if input.allow_json && path::ext::has(&e.name, ".json") {
                    let k = path::key(&file, cs);
                    if col.json_keys.insert(k) {
                        col.json.push(file);
                    }
                }
                continue;
            }
            if is_supported(&e.name, input.allow_js) {
                col.add(&file);
            }
        }
        for e in entries.iter().filter(|e| e.kind == EntryKind::Directory) {
            let sub = path::combine(dir, &e.name);
            if !descends(pat, excludes, &sub, cs) {
                continue;
            }
            let r = match self.tree.reals.remove(&sub) {
                Some(r) => r,
                None => sub_real(self.fs, &sub, &real, &e.name),
            };
            self.walk((&sub, r), col, visited);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemoryFs;

    fn run(fs: &MemoryFs, include: &[&str], exclude: &[&str], allow_js: bool) -> Vec<String> {
        let inc: Vec<String> = include.iter().map(|s| path::resolve("/p", s)).collect();
        let exc: Vec<String> = exclude.iter().map(|s| path::resolve("/p", s)).collect();
        match_files(
            fs,
            &MatchInput {
                include: &inc,
                exclude: &exc,
                allow_js,
                allow_json: true,
            },
        )
        .into_iter()
        .map(|f| f.trim_start_matches("/p/").to_owned())
        .collect()
    }

    fn fs_with(files: &[&str]) -> MemoryFs {
        let mut fs = MemoryFs::new();
        for f in files {
            fs.add_file(&format!("/p/{f}"), "");
        }
        fs
    }

    #[test]
    fn component_globs() {
        assert!(glob_component("*.ts", "a.ts", true));
        assert!(glob_component("?.ts", "a.ts", true));
        assert!(!glob_component("?.ts", "ab.ts", true));
        assert!(glob_component("a*b*c", "aXbYc", true));
        assert!(glob_component("*.TS", "a.ts", false));
        assert!(!glob_component("*.TS", "a.ts", true));
    }

    #[test]
    fn files_before_subdirectories_in_ordinal_order() {
        let fs = fs_with(&["z.ts", "b/x.ts", "a/y.ts", "Z/q.ts", "_a.ts", "1.ts"]);
        assert_eq!(
            run(&fs, &["**/*"], &[], false),
            ["1.ts", "_a.ts", "z.ts", "Z/q.ts", "a/y.ts", "b/x.ts"]
        );
    }

    #[test]
    fn implicit_excludes() {
        let fs = fs_with(&[
            "a.ts",
            "node_modules/x/i.ts",
            ".hidden/h.ts",
            ".e.ts",
            "bower_components/b.ts",
            "jspm_packages/j.ts",
            "src/node_modules/y.ts",
        ]);
        assert_eq!(run(&fs, &["**/*"], &[], false), ["a.ts"]);
        assert_eq!(
            run(
                &fs,
                &["node_modules/**/*", ".hidden/*", ".e.ts"],
                &[],
                false
            ),
            ["node_modules/x/i.ts", ".hidden/h.ts", ".e.ts"]
        );
    }

    #[test]
    fn extension_priority() {
        let fs = fs_with(&[
            "a.ts", "a.d.ts", "c.ts", "c.js", "d.d.ts", "d.js", "e.tsx", "e.ts", "f.d.mts",
            "f.mts", "g.cts", "g.d.cts", "h.cjs", "h.d.cts", "i.js", "i.jsx", "j.d.mts", "j.mjs",
        ]);
        let got = run(&fs, &["*"], &[], true);
        assert_eq!(
            got,
            [
                "a.ts", "c.ts", "d.d.ts", "d.js", "e.ts", "f.mts", "g.cts", "g.d.cts", "h.d.cts",
                "i.js", "j.d.mts"
            ]
        );
    }

    #[test]
    fn per_spec_walk_order_and_json_last() {
        let fs = fs_with(&[
            "src/q.mts",
            "src/r.cts",
            "src/a/c2.cts",
            "src/b/m2.mts",
            "src/t.json",
            "src/a/k.json",
        ]);
        assert_eq!(
            run(&fs, &["src/**/*.mts", "src/**/*.cts"], &[], false),
            ["src/q.mts", "src/b/m2.mts", "src/r.cts", "src/a/c2.cts"]
        );
        assert_eq!(
            run(&fs, &["src/**/*.json", "src/a/**/*"], &[], false),
            ["src/a/c2.cts", "src/t.json", "src/a/k.json"]
        );
    }

    #[test]
    fn directory_specs_and_excludes() {
        let fs = fs_with(&[
            "src/a.ts",
            "src/a/q.ts",
            "src/b/x.ts",
            "out/k.ts",
            "src/x.min.js",
            "src/y.js",
        ]);
        assert_eq!(
            run(&fs, &["src"], &["src/a"], true),
            ["src/a.ts", "src/y.js", "src/b/x.ts"]
        );
        assert_eq!(
            run(&fs, &["**/*"], &["**/a*"], false),
            ["out/k.ts", "src/b/x.ts"]
        );
        assert_eq!(run(&fs, &["src/x.min.js"], &[], true), ["src/x.min.js"]);
    }

    #[test]
    fn case_insensitive_literal_match() {
        let mut fs = MemoryFs::case_insensitive();
        fs.add_file("/p/src/Z/a.ts", "");
        let inc = vec!["/p/SRC/z/*".to_owned()];
        let got = match_files(
            &fs,
            &MatchInput {
                include: &inc,
                exclude: &[],
                allow_js: false,
                allow_json: false,
            },
        );
        assert_eq!(got, ["/p/src/Z/a.ts"]);
    }
}
