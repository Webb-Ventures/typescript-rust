//! Where outputs go (observed in tsc 7.0.2; design record 0012, "Output paths").
//!
//! - `rootDir` is the explicit option, else the tsconfig's directory, else (no tsconfig) the
//!   common source directory of the emitted files.
//! - With `outDir` (JS, maps, JSON copies) and `declarationDir` (declarations, else `outDir`),
//!   an output is `<dir>/<path of the source relative to rootDir>`. A source outside
//!   `rootDir` is written **next to itself**, as if there were no `outDir` (tsc reports
//!   TS6059 and still does that).
//! - Extensions: `.ts`/`.tsx`/`.js`/`.jsx` → `.js` (`.tsx`/`.jsx` → `.jsx` under
//!   `jsx: preserve`), `.mts`/`.mjs` → `.mjs`, `.cts`/`.cjs` → `.cjs`; declarations
//!   `.d.ts`/`.d.mts`/`.d.cts`; JSON keeps its name.

use ts_r_config::path;

/// The extension of the JavaScript output of `source` (`None` for declaration files and
/// files that are not code).
pub fn js_extension(source: &str, preserve_jsx: bool) -> Option<&'static str> {
    let name = path::file_name(source);
    if path::ext::is_declaration(name) {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let ext = |e: &str| lower.len() > e.len() && lower.ends_with(e);
    Some(if ext(".tsx") || ext(".jsx") {
        if preserve_jsx { ".jsx" } else { ".js" }
    } else if ext(".mts") || ext(".mjs") {
        ".mjs"
    } else if ext(".cts") || ext(".cjs") {
        ".cjs"
    } else if ext(".ts") || ext(".js") {
        ".js"
    } else {
        return None;
    })
}

/// The declaration extension for `source` (`.d.ts`, `.d.mts`, `.d.cts`).
pub fn dts_extension(source: &str) -> Option<&'static str> {
    let name = path::file_name(source);
    if path::ext::is_declaration(name) {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let ext = |e: &str| lower.len() > e.len() && lower.ends_with(e);
    Some(if ext(".mts") || ext(".mjs") {
        ".d.mts"
    } else if ext(".cts") || ext(".cjs") {
        ".d.cts"
    } else if ext(".ts") || ext(".tsx") || ext(".js") || ext(".jsx") {
        ".d.ts"
    } else {
        return None;
    })
}

/// `source` without its extension (the last `.xxx` of the file name).
fn strip_extension(source: &str) -> &str {
    let name = path::file_name(source);
    match name.rfind('.') {
        Some(i) if i > 0 => &source[..source.len() - (name.len() - i)],
        _ => source,
    }
}

/// The longest directory containing every path in `files` (absolute, normalised). One file:
/// its directory. None without files, and when two files have different roots (drives):
/// tsc then has no common directory at all (`tsc: tsc/internal/outputpaths/commonsourcedirectory.go`).
pub fn common_source_dir<'a>(
    files: impl IntoIterator<Item = &'a str>,
    case_sensitive: bool,
) -> Option<String> {
    let mut common: Option<(String, Vec<String>)> = None;
    for f in files {
        let dir = path::directory_of(f);
        let (root, comps) = path::components(dir);
        match &mut common {
            None => {
                common = Some((
                    root.to_owned(),
                    comps.iter().map(|s| (*s).to_owned()).collect(),
                ))
            }
            Some((r, c)) => {
                if !path::eq(r, root, case_sensitive) {
                    return None;
                }
                let n = c
                    .iter()
                    .zip(&comps)
                    .take_while(|(a, b)| path::eq(a, b, case_sensitive))
                    .count();
                c.truncate(n);
            }
        }
    }
    common.map(|(root, comps)| {
        let mut s = root;
        s.push_str(&comps.join("/"));
        s
    })
}

/// How sources map to output paths.
#[derive(Clone, Debug)]
pub struct PathRules {
    /// The effective root directory (absolute).
    pub root_dir: String,
    pub out_dir: Option<String>,
    pub declaration_dir: Option<String>,
    /// `jsx: preserve`: `.tsx`/`.jsx` sources keep `.jsx`.
    pub preserve_jsx: bool,
    pub case_sensitive: bool,
}

impl PathRules {
    /// True when `source` lies inside `rootDir`.
    pub fn under_root(&self, source: &str) -> bool {
        path::contains(&self.root_dir, source, self.case_sensitive)
    }

    /// `source` moved under `dir`, relative to `rootDir`; a source outside `rootDir` (or
    /// without `dir`) stays where it is.
    fn rebase(&self, source: &str, dir: Option<&str>) -> String {
        match dir {
            Some(d) if self.under_root(source) => {
                let rel = path::relative(&self.root_dir, source, self.case_sensitive);
                path::combine(d, &rel)
            }
            _ => source.to_owned(),
        }
    }

    /// The `.js`/`.mjs`/`.cjs`/`.jsx` output of `source`, `None` for declarations.
    pub fn js_output(&self, source: &str) -> Option<String> {
        let ext = js_extension(source, self.preserve_jsx)?;
        let moved = self.rebase(source, self.out_dir.as_deref());
        Some(format!("{}{ext}", strip_extension(&moved)))
    }

    /// The declaration output of `source` (`declarationDir`, else `outDir`).
    pub fn dts_output(&self, source: &str) -> Option<String> {
        let ext = dts_extension(source)?;
        let dir = self.declaration_dir.as_deref().or(self.out_dir.as_deref());
        let moved = self.rebase(source, dir);
        Some(format!("{}{ext}", strip_extension(&moved)))
    }

    /// The copy of a JSON source under `outDir`, `None` without `outDir` (it would be the
    /// input itself).
    pub fn json_output(&self, source: &str) -> Option<String> {
        let out = self.rebase(source, self.out_dir.as_deref());
        (!path::eq(&out, source, self.case_sensitive)).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(root: &str, out: Option<&str>) -> PathRules {
        PathRules {
            root_dir: root.to_owned(),
            out_dir: out.map(str::to_owned),
            declaration_dir: None,
            preserve_jsx: false,
            case_sensitive: true,
        }
    }

    #[test]
    fn extensions() {
        assert_eq!(js_extension("/a/b.ts", false), Some(".js"));
        assert_eq!(js_extension("/a/b.tsx", false), Some(".js"));
        assert_eq!(js_extension("/a/b.tsx", true), Some(".jsx"));
        assert_eq!(js_extension("/a/b.mts", false), Some(".mjs"));
        assert_eq!(js_extension("/a/b.cts", false), Some(".cjs"));
        assert_eq!(js_extension("/a/b.d.ts", false), None);
        assert_eq!(js_extension("/a/b.d.css.ts", false), None);
        assert_eq!(js_extension("/a/b.json", false), None);
        assert_eq!(dts_extension("/a/b.mts"), Some(".d.mts"));
        assert_eq!(dts_extension("/a/b.tsx"), Some(".d.ts"));
    }

    #[test]
    fn out_dir_rebases_relative_to_root() {
        let r = rules("/p", Some("/p/out"));
        assert_eq!(
            r.js_output("/p/src/a.ts").as_deref(),
            Some("/p/out/src/a.js")
        );
        assert_eq!(r.js_output("/p/b.mts").as_deref(), Some("/p/out/b.mjs"));
        // Outside rootDir: next to the source (tsc 7, with TS6059).
        assert_eq!(r.js_output("/x/a.ts").as_deref(), Some("/x/a.js"));
        assert_eq!(r.json_output("/p/d.json").as_deref(), Some("/p/out/d.json"));
        let none = rules("/p", None);
        assert_eq!(
            none.js_output("/p/src/a.ts").as_deref(),
            Some("/p/src/a.js")
        );
        assert_eq!(none.json_output("/p/d.json"), None);
    }

    #[test]
    fn common_dir() {
        let c = |f: &[&str]| common_source_dir(f.iter().copied(), true);
        assert_eq!(c(&["/p/src/a.ts"]).as_deref(), Some("/p/src"));
        assert_eq!(
            c(&["/p/src/a.ts", "/p/src/sub/b.ts"]).as_deref(),
            Some("/p/src")
        );
        assert_eq!(c(&["/p/src/a.ts", "/p/b.ts"]).as_deref(), Some("/p"));
        assert_eq!(c(&["/p/a.ts", "/q/b.ts"]).as_deref(), Some("/"));
        assert_eq!(c(&[]), None);
        assert_eq!(c(&["A:/foo/a.ts", "B:/foo/b.ts"]), None);
        assert_eq!(c(&["A:/foo/a.ts", "a:/foo/b.ts"]), None);
        let insensitive = common_source_dir(["A:/foo/a.ts", "a:/foo/b.ts"], false);
        assert_eq!(insensitive.as_deref(), Some("A:/foo"));
    }
}
