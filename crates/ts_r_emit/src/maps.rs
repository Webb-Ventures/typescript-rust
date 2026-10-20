//! Source-map paths and JSON (observed in tsc 7.0.2; design record 0012, "Source maps").
//!
//! A map has a *location*: the directory it describes itself from. Without `mapRoot` that is
//! the output's directory (the `.map` file sits next to the `.js`). With `mapRoot` it is
//! `<mapRoot>/<directory of the source relative to rootDir>`, where a relative `mapRoot` is
//! resolved against `rootDir`. Then:
//!
//! - `sources`: the source relative to the location, or, with `sourceRoot`, relative to
//!   `rootDir` (and `sourceRoot` gains a trailing `/`). A URL `mapRoot` (`https://...`) has
//!   no file-system location: sources are `file:///` URLs.
//! - The `//# sourceMappingURL=` comment (no trailing newline): the map's name, or with
//!   `mapRoot` the location relative to the output's directory (an absolute `mapRoot` stays
//!   absolute, a URL is used as is) plus the name; `inlineSourceMap` writes a base64 data
//!   URL instead of a file.
//! - JSON keys in tsc's order: `version`, `file`, `sourceRoot` (`""` when unset), `sources`,
//!   `names`, `mappings`, then `sourcesContent` with `inlineSources`.

use ts_r_codegen::SourceMap;
use ts_r_config::path;

/// The map settings of a program.
#[derive(Clone, Debug, Default)]
pub struct MapRules {
    /// The effective root directory (absolute).
    pub root_dir: String,
    /// `mapRoot` as written.
    pub map_root: Option<String>,
    /// `sourceRoot` as written.
    pub source_root: Option<String>,
    pub inline: bool,
    pub inline_sources: bool,
    pub case_sensitive: bool,
}

fn is_url(s: &str) -> bool {
    s.find("://")
        .is_some_and(|i| i > 1 && s[..i].bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Where a map describes itself from.
enum Location {
    Dir(String),
    Url(String),
}

/// What the map of one output says about paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPaths {
    /// `file`: the output's base name.
    pub file: String,
    /// `sourceRoot` (`""` when unset).
    pub source_root: String,
    /// The one `sources` entry.
    pub source: String,
    /// The URL in the `sourceMappingURL` comment (for a map file; inline maps use a data
    /// URL).
    pub url: String,
}

impl MapRules {
    fn location(&self, source: &str, output: &str) -> Location {
        let out_dir = path::directory_of(output).to_owned();
        let Some(root) = self.map_root.as_deref().filter(|_| !self.inline) else {
            return Location::Dir(out_dir);
        };
        let sub = if path::contains(&self.root_dir, source, self.case_sensitive) {
            path::relative(
                &self.root_dir,
                path::directory_of(source),
                self.case_sensitive,
            )
        } else {
            String::new()
        };
        if is_url(root) {
            let mut u = root.to_owned();
            if !u.ends_with('/') {
                u.push('/');
            }
            if !sub.is_empty() {
                u.push_str(&sub);
                u.push('/');
            }
            return Location::Url(u);
        }
        let base = path::resolve(&self.root_dir, &path::normalize_slashes(root));
        Location::Dir(if sub.is_empty() {
            base
        } else {
            path::combine(&base, &sub)
        })
    }

    /// The paths in the map of `output` (the `.js`), compiled from `source`.
    pub fn paths(&self, source: &str, output: &str) -> MapPaths {
        let file = path::file_name(output).to_owned();
        let location = self.location(source, output);
        let (source_root, rel) = match &self.source_root {
            Some(r) => {
                let mut r = r.clone();
                if !r.is_empty() && !r.ends_with('/') {
                    r.push('/');
                }
                (
                    r,
                    path::relative(&self.root_dir, source, self.case_sensitive),
                )
            }
            None => (
                String::new(),
                match &location {
                    Location::Dir(d) => path::relative(d, source, self.case_sensitive),
                    Location::Url(_) => format!("file:///{}", source.trim_start_matches('/')),
                },
            ),
        };
        let name = format!("{file}.map");
        let url = match (&location, self.map_root.as_deref().filter(|_| !self.inline)) {
            (Location::Url(u), _) => format!("{u}{name}"),
            (Location::Dir(d), Some(root)) if path::is_rooted(&path::normalize_slashes(root)) => {
                path::combine(d, &name)
            }
            (Location::Dir(d), Some(_)) => {
                let rel = path::relative(path::directory_of(output), d, self.case_sensitive);
                if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                }
            }
            (Location::Dir(_), None) => name,
        };
        MapPaths {
            file,
            source_root,
            source: rel,
            url,
        }
    }
}

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The map JSON as tsc writes it.
pub fn map_json(map: &SourceMap, paths: &MapPaths, content: Option<&str>) -> String {
    let mut s = String::with_capacity(map.mappings.len() + 128);
    s.push_str("{\"version\":3,\"file\":");
    json_string(&mut s, &paths.file);
    s.push_str(",\"sourceRoot\":");
    json_string(&mut s, &paths.source_root);
    s.push_str(",\"sources\":[");
    json_string(&mut s, &paths.source);
    s.push_str("],\"names\":[");
    for (i, n) in map.names.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_string(&mut s, n);
    }
    s.push_str("],\"mappings\":");
    json_string(&mut s, &map.mappings);
    if let Some(c) = content {
        s.push_str(",\"sourcesContent\":[");
        json_string(&mut s, c);
        s.push(']');
    }
    s.push('}');
    s
}

/// `data:application/json;base64,...` for an inline map.
pub fn data_url(json: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = json.as_bytes();
    let mut s = String::with_capacity(b.len() * 4 / 3 + 40);
    s.push_str("data:application/json;base64,");
    for chunk in b.chunks(3) {
        let n = match chunk.len() {
            3 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            _ => (chunk[0] as u32) << 16,
        };
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(root: &str, map_root: Option<&str>, source_root: Option<&str>) -> MapRules {
        MapRules {
            root_dir: root.to_owned(),
            map_root: map_root.map(str::to_owned),
            source_root: source_root.map(str::to_owned),
            inline: false,
            inline_sources: false,
            case_sensitive: true,
        }
    }

    // Each case is a tsc 7.0.2 probe (p/src/sub/b.ts, see the crate's AGENTS.md).
    #[test]
    fn observed_paths() {
        let p = |r: &MapRules, out: &str| r.paths("/p/src/sub/b.ts", out);
        let m = p(&rules("/p", None, None), "/p/out/src/sub/b.js");
        assert_eq!(
            (m.source.as_str(), m.url.as_str()),
            ("../../../src/sub/b.ts", "b.js.map")
        );
        let m = p(&rules("/p", Some("mr"), Some("sr")), "/p/out/src/sub/b.js");
        assert_eq!(m.url, "../../../mr/src/sub/b.js.map");
        assert_eq!(
            (m.source_root.as_str(), m.source.as_str()),
            ("sr/", "src/sub/b.ts")
        );
        let m = p(
            &rules("/p", Some("/mr"), Some("/abs/sr/")),
            "/p/out/src/sub/b.js",
        );
        assert_eq!(m.url, "/mr/src/sub/b.js.map");
        assert_eq!(m.source_root, "/abs/sr/");
        let m = p(&rules("/p", Some("mr"), None), "/p/src/sub/b.js");
        assert_eq!(
            (m.source.as_str(), m.url.as_str()),
            ("../../../src/sub/b.ts", "../../mr/src/sub/b.js.map")
        );
        let m = p(
            &rules("/p", Some("https://x.com/m/"), None),
            "/p/src/sub/b.js",
        );
        assert_eq!(m.url, "https://x.com/m/src/sub/b.js.map");
        assert_eq!(m.source, "file:///p/src/sub/b.ts");
        let m = p(&rules("/p/src", Some("mr"), None), "/p/out/sub/b.js");
        assert_eq!(
            (m.source.as_str(), m.url.as_str()),
            ("../../sub/b.ts", "../../src/mr/sub/b.js.map")
        );
        let m = p(&rules("/p/src", Some("/mr"), None), "/p/out/sub/b.js");
        assert_eq!(
            (m.source.as_str(), m.url.as_str()),
            ("../../p/src/sub/b.ts", "/mr/sub/b.js.map")
        );
        let m = p(&rules("/p/src", None, Some("sr")), "/p/out/sub/b.js");
        assert_eq!(
            (m.source_root.as_str(), m.source.as_str(), m.url.as_str()),
            ("sr/", "sub/b.ts", "b.js.map")
        );
    }

    #[test]
    fn base64() {
        assert_eq!(data_url("{}"), "data:application/json;base64,e30=");
        assert_eq!(data_url("abc"), "data:application/json;base64,YWJj");
        assert_eq!(data_url("ab"), "data:application/json;base64,YWI=");
    }
}
