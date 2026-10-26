//! `unicorn/filename-case`: the linted file's name (and, for files inside ESLint's cwd,
//! the directory names on its relative path) must be in one of the chosen cases (default
//! kebab case), and its extension lowercase. Only runs of `[A-Za-z0-9_-]` are checked;
//! other characters stay as they are, a leading `_` run is kept, and names starting with `$`
//! pass. `index.{js,mjs,cjs,ts,tsx,vue}` are allowed. With `multipleFileExtensions` (the
//! default) everything after the first `.` of the name is extension. Every report is at 1:1;
//! the first bad directory wins over the filename. `ignore` patterns are tested against each
//! path segment; `directoryRoots` (exact `/`-joined relative paths or patterns) start the
//! directory check below the deepest root that matches.

use super::prelude::*;
use super::unicorn_names::{
    RegexOpt, camel_case, delimited_case, disjunction, pascal_case, regex_list,
};
use crate::diagnostic::Loc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/filename-case",
        "Enforce a case style for filenames and directory names.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<FilenameCase>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Camel,
    CamelAcronyms,
    Kebab,
    Snake,
    Pascal,
}

impl Case {
    fn parse(s: &str) -> Option<Case> {
        Some(match s {
            "camelCase" => Case::Camel,
            "camelCaseWithAcronyms" => Case::CamelAcronyms,
            "kebabCase" => Case::Kebab,
            "snakeCase" => Case::Snake,
            "pascalCase" => Case::Pascal,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            Case::Camel => "camel case",
            Case::CamelAcronyms => "camel case with acronyms",
            Case::Kebab => "kebab case",
            Case::Snake => "snake case",
            Case::Pascal => "pascal case",
        }
    }
    fn convert(self, s: &str) -> String {
        match self {
            Case::Camel => camel_case(&camel_case(s)),
            Case::CamelAcronyms => {
                if is_camel_with_acronyms(s) {
                    return s.to_owned();
                }
                let once = camel_case(s);
                if is_camel_with_acronyms(&once) {
                    once
                } else {
                    camel_case(&once)
                }
            }
            Case::Kebab => delimited_case(s, "-"),
            Case::Snake => delimited_case(s, "_"),
            Case::Pascal => {
                if has_valid_leading_acronym(s) {
                    return s.to_owned();
                }
                let once = pascal_case(s);
                if has_valid_leading_acronym(&once) {
                    once
                } else {
                    pascal_case(&once)
                }
            }
        }
    }
}

/// Lower camel case where an uppercase run (an acronym) is allowed when it ends the name
/// (digits may follow it), is followed by another uppercase letter after digits, or gives
/// its last letter to a following lowercase word (`getDOMRange`).
fn is_camel_with_acronyms(s: &str) -> bool {
    let b = s.as_bytes();
    if !b.first().is_some_and(u8::is_ascii_lowercase) {
        return false;
    }
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let mut i = 1;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            i += 1;
            continue;
        }
        if !c.is_ascii_uppercase() {
            return false;
        }
        let run_start = i;
        while at(i + 1).is_ascii_uppercase() {
            i += 1;
        }
        if i == run_start {
            i += 1;
            continue;
        }
        if at(i + 1).is_ascii_lowercase() {
            // The run's last letter starts the next word: look at it again.
            continue;
        }
        while at(i + 1).is_ascii_digit() {
            i += 1;
        }
        if i == b.len() - 1 {
            return true;
        }
        if !at(i + 1).is_ascii_uppercase() {
            return false;
        }
        i += 1;
    }
    true
}

/// `FAQPage`: an alphanumeric name whose leading run of three or more capitals (ending
/// where a capital, after optional digits, starts a lowercase word) is followed by a pascal
/// case remainder.
fn has_valid_leading_acronym(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || !b.iter().all(u8::is_ascii_alphanumeric) {
        return false;
    }
    let run = b.iter().take_while(|c| c.is_ascii_uppercase()).count();
    // The longest prefix of capitals (at least three) after which `\d*[A-Z](?:[a-z]|\d+[a-z])`
    // matches.
    let follows = |k: usize| {
        let mut j = k;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if !(j < b.len() && b[j].is_ascii_uppercase()) {
            return false;
        }
        j += 1;
        if j < b.len() && b[j].is_ascii_lowercase() {
            return true;
        }
        let d = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        j > d && j < b.len() && b[j].is_ascii_lowercase()
    };
    let Some(len) = (3..=run).rev().find(|&k| follows(k)) else {
        return false;
    };
    let suffix = &s[len..];
    !suffix.is_empty() && pascal_case(suffix) == suffix
}

#[derive(Debug)]
struct FilenameCase {
    cases: Vec<Case>,
    ignore: Vec<RegexOpt>,
    check_directories: bool,
    directory_roots: Vec<Value>,
    multiple_extensions: bool,
}

impl FromOptions for FilenameCase {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut cases = Vec::new();
        if let Some(c) = o.get("case").and_then(Value::as_str) {
            cases.push(Case::parse(c).ok_or("unknown `case`")?);
        } else if let Some(m) = o.get("cases").and_then(Value::as_object) {
            for (k, v) in m {
                if v.as_bool() == Some(true) {
                    cases.push(Case::parse(k).ok_or("unknown case in `cases`")?);
                }
            }
        }
        if cases.is_empty() {
            cases.push(Case::Kebab);
        }
        let ignore = regex_list(o.get("ignore"), "ignore")?;
        // Validated like `ignore`; strings stay exact paths.
        regex_list(o.get("directoryRoots"), "directoryRoots")?;
        let directory_roots = o
            .get("directoryRoots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(FilenameCase {
            cases,
            ignore,
            check_directories: o.get("checkDirectories").and_then(Value::as_bool) != Some(false),
            directory_roots,
            multiple_extensions: o.get("multipleFileExtensions").and_then(Value::as_bool)
                != Some(false),
        })
    }
}

/// A name split into runs of checked (`[A-Za-z0-9_-]`) and kept characters, after its
/// leading underscores.
struct Split<'s> {
    leading: &'s str,
    words: Vec<(&'s str, bool)>,
}

fn split_name(name: &str) -> Split<'_> {
    let lead = name.len() - name.trim_start_matches('_').len();
    let (leading, rest) = name.split_at(lead);
    let mut words: Vec<(&str, bool)> = Vec::new();
    let mut start = 0;
    let mut kind: Option<bool> = None;
    for (i, c) in rest.char_indices() {
        let ignored = !(c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if kind != Some(ignored) {
            if let Some(k) = kind {
                words.push((&rest[start..i], k));
            }
            start = i;
            kind = Some(ignored);
        }
    }
    if let Some(k) = kind {
        words.push((&rest[start..], k));
    }
    Split { leading, words }
}

impl FilenameCase {
    fn valid(&self, s: &Split<'_>) -> bool {
        s.words
            .iter()
            .filter(|(_, ignored)| !ignored)
            .all(|(w, _)| self.cases.iter().any(|c| c.convert(w) == *w))
    }
    fn renamed(&self, s: &Split<'_>, trailing: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.cases {
            let mut name = s.leading.to_owned();
            for (w, ignored) in &s.words {
                if *ignored {
                    name.push_str(w);
                } else {
                    name.push_str(&c.convert(w));
                }
            }
            name.push_str(trailing);
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }
    fn case_names(&self) -> String {
        let names: Vec<String> = self.cases.iter().map(|c| c.name().to_owned()).collect();
        disjunction(&names)
    }
    fn is_root(&self, dir: &str) -> bool {
        self.directory_roots.iter().any(|r| match r {
            Value::String(s) => s == dir,
            other => RegexOpt::from_value(other).is_some_and(|re| re.test(dir)),
        })
    }
}

/// The path's segments relative to `cwd` when the file is inside it, else its basename.
fn path_segments(path: &str, cwd: Option<&str>) -> Vec<String> {
    let path = path.replace('\\', "/");
    let base = || vec![path.rsplit('/').next().unwrap_or(&path).to_owned()];
    let Some(cwd) = cwd else {
        return base();
    };
    let cwd = cwd.replace('\\', "/");
    let cwd = cwd.trim_end_matches('/');
    let rel = if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        match path.strip_prefix(cwd).and_then(|r| r.strip_prefix('/')) {
            Some(r) => r.to_owned(),
            None => return base(),
        }
    } else {
        path.clone()
    };
    let segs: Vec<String> = rel
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_owned)
        .collect();
    if segs.is_empty() || segs.iter().any(|s| s == "..") {
        return base();
    }
    segs
}

/// `path.extname`: from the last `.` that is not the name's first character.
pub(super) fn extname(base: &str) -> &str {
    match base.rfind('.') {
        Some(i) if i > 0 && base[..i].bytes().any(|b| b != b'.') => &base[i..],
        _ => "",
    }
}

impl Rule for FilenameCase {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let path = cx.file.path;
        if path == "<input>" || path == "<text>" {
            return;
        }
        let segs = path_segments(path, cx.env().cwd.as_deref());
        let Some(basename) = segs.last().cloned() else {
            return;
        };
        if segs.iter().any(|s| self.ignore.iter().any(|re| re.test(s))) {
            return;
        }
        let at = Loc::at(1, 1);
        if self.check_directories {
            let dirs = &segs[..segs.len() - 1];
            let mut from = 0;
            let mut joined = String::new();
            for (i, d) in dirs.iter().enumerate() {
                if !joined.is_empty() {
                    joined.push('/');
                }
                joined.push_str(d);
                if self.is_root(&joined) {
                    from = i + 1;
                }
            }
            for d in &dirs[from..] {
                let s = split_name(d);
                if d.starts_with('$') || self.valid(&s) {
                    continue;
                }
                let renamed: Vec<String> = self
                    .renamed(&s, "")
                    .into_iter()
                    .map(|n| format!("`{n}`"))
                    .collect();
                let msg = format!(
                    "Directory name `{d}` is not in {}. Rename it to {}.",
                    self.case_names(),
                    disjunction(&renamed)
                );
                cx.report(Span::at(0), msg).id("directory-case").loc(at);
                return;
            }
        }
        if matches!(
            basename.as_str(),
            "index.js" | "index.mjs" | "index.cjs" | "index.ts" | "index.tsx" | "index.vue"
        ) {
            return;
        }
        let ext = extname(&basename);
        let stem = &basename[..basename.len() - ext.len()];
        let (filename, extra) = if self.multiple_extensions {
            let first = stem.split('.').next().unwrap_or(stem);
            (first, &stem[first.len()..])
        } else {
            (stem, "")
        };
        let s = split_name(filename);
        if filename.starts_with('$') || self.valid(&s) {
            if ext != ext.to_lowercase() {
                let msg = format!(
                    "File extension `{ext}` is not in lowercase. Rename it to `{filename}{extra}{}`.",
                    ext.to_lowercase()
                );
                cx.report(Span::at(0), msg).id("filename-extension").loc(at);
            }
            return;
        }
        let trailing = format!("{extra}{}", ext.to_lowercase());
        let renamed: Vec<String> = self
            .renamed(&s, &trailing)
            .into_iter()
            .map(|n| format!("`{n}`"))
            .collect();
        let msg = format!(
            "Filename is not in {}. Rename it to {}.",
            self.case_names(),
            disjunction(&renamed)
        );
        cx.report(Span::at(0), msg).id("filename-case").loc(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acronyms() {
        assert!(is_camel_with_acronyms("getDOMRangeRect"));
        assert!(is_camel_with_acronyms("apiURL"));
        assert!(!is_camel_with_acronyms("HTMLParser"));
        assert!(has_valid_leading_acronym("FAQPage"));
        assert!(!has_valid_leading_acronym("FAQpage"));
    }

    #[test]
    fn segments() {
        assert_eq!(path_segments("/p/src/a.js", Some("/p")), ["src", "a.js"]);
        assert_eq!(path_segments("/q/src/a.js", Some("/p")), ["a.js"]);
        assert_eq!(path_segments("/q/a.js", None), ["a.js"]);
        assert_eq!(extname(".eslintrc"), "");
        assert_eq!(extname("a.b.JS"), ".JS");
    }
}
