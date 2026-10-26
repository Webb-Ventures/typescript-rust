//! Build options (esbuild's option set, the common subset).

use rustc_hash::FxHashMap;

use crate::features::{Feature, Features};

/// Output module format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Format {
    #[default]
    Esm,
    Cjs,
    Iife,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s {
            "esm" => Format::Esm,
            "cjs" => Format::Cjs,
            "iife" => Format::Iife,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Esm => "esm",
            Format::Cjs => "cjs",
            Format::Iife => "iife",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Platform {
    #[default]
    Browser,
    Node,
    Neutral,
}

impl Platform {
    pub fn parse(s: &str) -> Option<Platform> {
        Some(match s {
            "browser" => Platform::Browser,
            "node" => Platform::Node,
            "neutral" => Platform::Neutral,
            _ => return None,
        })
    }
}

/// How a file is turned into a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loader {
    Js,
    Jsx,
    Ts,
    Tsx,
    Json,
    Text,
    /// Copied to the output directory; the module exports its URL.
    File,
    /// Copied to the output directory; imports of it stay imports of the copy (`copy`).
    Copy,
    /// A stylesheet, bundled into a `.css` output next to the JS (`crate::css`).
    Css,
    /// A CSS module: classes, IDs and keyframe names are local (`local-css`).
    LocalCss,
    /// A stylesheet whose names are global unless `:local` (`global-css`).
    GlobalCss,
    /// The module is empty.
    Empty,
    Base64,
    DataUrl,
    Binary,
    /// A `Uint8Array` of the file's bytes, through `Uint8Array.fromBase64` (what an import
    /// `with { type: 'bytes' }` loads; not selectable with `--loader`).
    Bytes,
}

impl Loader {
    pub fn parse(s: &str) -> Option<Loader> {
        Some(match s {
            "js" => Loader::Js,
            "jsx" => Loader::Jsx,
            "ts" => Loader::Ts,
            "tsx" => Loader::Tsx,
            "json" => Loader::Json,
            "text" => Loader::Text,
            "file" => Loader::File,
            "copy" => Loader::Copy,
            "css" => Loader::Css,
            "local-css" => Loader::LocalCss,
            "global-css" => Loader::GlobalCss,
            "empty" => Loader::Empty,
            "base64" => Loader::Base64,
            "dataurl" => Loader::DataUrl,
            "binary" => Loader::Binary,
            _ => return None,
        })
    }

    pub fn is_js(self) -> bool {
        matches!(self, Loader::Js | Loader::Jsx | Loader::Ts | Loader::Tsx)
    }

    pub fn is_css(self) -> bool {
        matches!(self, Loader::Css | Loader::LocalCss | Loader::GlobalCss)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Loader::Js => "js",
            Loader::Jsx => "jsx",
            Loader::Ts => "ts",
            Loader::Tsx => "tsx",
            Loader::Json => "json",
            Loader::Text => "text",
            Loader::File => "file",
            Loader::Copy => "copy",
            Loader::Css => "css",
            Loader::LocalCss => "local-css",
            Loader::GlobalCss => "global-css",
            Loader::Empty => "empty",
            Loader::Base64 => "base64",
            Loader::DataUrl => "dataurl",
            Loader::Binary => "binary",
            Loader::Bytes => "bytes",
        }
    }
}

/// `--sourcemap` modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SourceMapMode {
    #[default]
    None,
    /// `.map` file plus a `//# sourceMappingURL=` comment.
    Linked,
    /// Data URL in the output.
    Inline,
    /// `.map` file without the comment.
    External,
    /// Both a `.map` file and an inline map.
    Both,
}

/// An entry point.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntryPoint {
    /// Path as given (relative to the working directory or absolute).
    pub path: String,
    /// Output name override (`--entry-names` `[name]`), from `name=path` on the CLI.
    pub name: Option<String>,
}

/// JSX settings (from tsconfig, overridable).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct JsxOptions {
    /// `--jsx=automatic|transform|preserve`.
    pub mode: Option<String>,
    pub factory: Option<String>,
    pub fragment: Option<String>,
    pub import_source: Option<String>,
    pub dev: bool,
}

/// Everything a build needs. Defaults follow esbuild's.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub entry_points: Vec<EntryPoint>,
    /// Absolute working directory (paths in messages and the metafile are relative to it).
    pub working_dir: String,
    pub bundle: bool,
    pub outfile: Option<String>,
    pub outdir: Option<String>,
    /// `None` = esbuild's default for the platform.
    pub format: Option<Format>,
    pub platform: Platform,
    /// ECMAScript year of the target (`es5` is 2014); `None` = esnext.
    pub target: Option<u32>,
    /// `--supported:<feature>=<bool>` overrides of what the target supports, in order.
    pub supported: Vec<(Feature, bool)>,
    pub minify_whitespace: bool,
    pub minify_identifiers: bool,
    pub minify_syntax: bool,
    pub sourcemap: SourceMapMode,
    pub sources_content: bool,
    pub splitting: bool,
    /// Import paths to leave alone (`*` wildcard allowed).
    pub external: Vec<String>,
    /// `--packages=external`: every bare import is external.
    pub packages_external: bool,
    /// `(key, value)`: `process.env.NODE_ENV` -> `"production"` (value is JS/JSON text).
    pub define: Vec<(String, String)>,
    /// `--drop-labels=A,B`: statements labeled `A:` or `B:` are removed before scanning.
    pub drop_labels: Vec<String>,
    /// `--alias:pkg=replacement`: an import path that is `pkg` or starts with `pkg/` is
    /// rewritten (the longest key wins) and resolved from the working directory.
    pub alias: Vec<(String, String)>,
    pub metafile: bool,
    /// Extension (with dot) -> loader overrides.
    pub loaders: FxHashMap<String, Loader>,
    pub global_name: Option<String>,
    pub main_fields: Option<Vec<String>>,
    pub conditions: Option<Vec<String>>,
    pub resolve_extensions: Vec<String>,
    /// Explicit tsconfig path; otherwise the nearest one per file.
    pub tsconfig: Option<String>,
    pub jsx: JsxOptions,
    /// `None` = on when bundling.
    pub tree_shaking: Option<bool>,
    pub ignore_annotations: bool,
    pub entry_names: String,
    pub chunk_names: String,
    pub asset_names: String,
    pub public_path: String,
    pub banner: Option<String>,
    pub footer: Option<String>,
    /// Output extension for JS (`.js` default).
    pub out_extension_js: String,
    /// Write outputs to disk (the CLI) or only return them (tests, the dev server).
    pub write: bool,
    /// Worker threads for parsing and printing (0 = all cores).
    pub threads: usize,
    /// Persistent cache across runs (`ts_r_cache` namespace `build`).
    pub persistent_cache: bool,
    pub legal_comments_inline: bool,
    /// ESM output only (not an esbuild option): `require()` of an external becomes a hoisted
    /// `import * as` of it, unwrapped by the `__tsrToCjs` helper (a `"module.exports"`
    /// export, as Node's `require(esm)` honours, or the namespace viewed as CommonJS). The
    /// dev server's dependency adapter uses this so CommonJS packages can require their
    /// peers in the browser.
    pub require_to_import: bool,
    /// Not an esbuild option: a dynamic `import()` whose resolved file this maps to `Some(url)`
    /// stays external, importing `url`. The dev server's dependency adapter keeps
    /// `import("./x.css")` in a package as a request for the original file this way.
    pub dynamic_import_url: Option<DynamicImportUrl>,
}

/// See [`BuildOptions::dynamic_import_url`]: resolved file path to URL, or `None` to bundle.
#[derive(Clone)]
pub struct DynamicImportUrl(pub std::sync::Arc<UrlFn>);

/// The function behind [`DynamicImportUrl`].
pub type UrlFn = dyn Fn(&str) -> Option<String> + Send + Sync;

impl std::fmt::Debug for DynamicImportUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DynamicImportUrl(..)")
    }
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            entry_points: Vec::new(),
            working_dir: std::env::current_dir()
                .map(|p| ts_r_config::path::normalize_slashes(&p.to_string_lossy()))
                .unwrap_or_else(|_| "/".into()),
            bundle: false,
            outfile: None,
            outdir: None,
            format: None,
            platform: Platform::Browser,
            target: None,
            supported: Vec::new(),
            minify_whitespace: false,
            minify_identifiers: false,
            minify_syntax: false,
            sourcemap: SourceMapMode::None,
            sources_content: true,
            splitting: false,
            external: Vec::new(),
            packages_external: false,
            define: Vec::new(),
            drop_labels: Vec::new(),
            alias: Vec::new(),
            metafile: false,
            loaders: FxHashMap::default(),
            global_name: None,
            main_fields: None,
            conditions: None,
            resolve_extensions: [".tsx", ".ts", ".jsx", ".js", ".css", ".json"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            tsconfig: None,
            jsx: JsxOptions::default(),
            tree_shaking: None,
            ignore_annotations: false,
            entry_names: "[dir]/[name]".into(),
            chunk_names: "[name]-[hash]".into(),
            asset_names: "[name]-[hash]".into(),
            public_path: String::new(),
            banner: None,
            footer: None,
            out_extension_js: ".js".into(),
            write: true,
            threads: 0,
            persistent_cache: false,
            legal_comments_inline: true,
            require_to_import: false,
            dynamic_import_url: None,
        }
    }
}

impl BuildOptions {
    /// The syntax features the output may not use: those newer than `--target`, with the
    /// `--supported:` overrides applied (`features.rs`).
    pub fn unsupported(&self) -> Features {
        let mut s = match self.target {
            Some(y) => Features::newer_than(y),
            None => Features::NONE,
        };
        for &(f, ok) in &self.supported {
            if ok {
                s.remove(f);
            } else {
                s.insert(f);
            }
        }
        s
    }

    /// The output format after defaults (esbuild: iife for browser, cjs for node, esm for
    /// neutral when bundling; esm when splitting).
    pub fn effective_format(&self) -> Format {
        if let Some(f) = self.format {
            return f;
        }
        if !self.bundle {
            return Format::Esm;
        }
        match self.platform {
            Platform::Browser => Format::Iife,
            Platform::Node => Format::Cjs,
            Platform::Neutral => Format::Esm,
        }
    }

    pub fn tree_shaking(&self) -> bool {
        self.tree_shaking
            .unwrap_or(self.bundle || self.format == Some(Format::Iife))
    }

    pub fn minify_any(&self) -> bool {
        self.minify_whitespace || self.minify_identifiers || self.minify_syntax
    }

    /// Conditions for `exports`/`imports` (without `import`/`require`, which depend on the
    /// import kind).
    pub fn effective_conditions(&self) -> Vec<String> {
        if let Some(c) = &self.conditions {
            let mut v = c.clone();
            match self.platform {
                Platform::Browser => v.push("browser".into()),
                Platform::Node => v.push("node".into()),
                Platform::Neutral => {}
            }
            return v;
        }
        match self.platform {
            Platform::Browser => vec!["browser".into(), "module".into()],
            Platform::Node => vec!["node".into(), "module".into()],
            Platform::Neutral => vec!["module".into()],
        }
    }

    pub fn effective_main_fields(&self) -> Vec<String> {
        if let Some(m) = &self.main_fields {
            return m.clone();
        }
        let v: &[&str] = match self.platform {
            Platform::Browser => &["browser", "module", "main"],
            Platform::Node => &["main", "module"],
            Platform::Neutral => &[],
        };
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The loader for a path, from overrides and then the default extension table. Paths
    /// with no configured loader get [`Loader::File`]; check [`BuildOptions::has_loader`].
    pub fn loader_for(&self, path: &str) -> Loader {
        self.configured_loader(path).unwrap_or(Loader::File)
    }

    /// Whether a loader is configured for the path (esbuild errors otherwise).
    pub fn has_loader(&self, path: &str) -> bool {
        self.configured_loader(path).is_some()
    }

    fn configured_loader(&self, path: &str) -> Option<Loader> {
        let name = path.rsplit('/').next().unwrap_or(path);
        // Longest matching extension first (`.d.ts` before `.ts`).
        let mut ext = name;
        while let Some(i) = ext.find('.') {
            ext = &ext[i..];
            if let Some(&l) = self.loaders.get(ext) {
                return Some(l);
            }
            ext = &ext[1..];
        }
        let lower = name.to_ascii_lowercase();
        let Some((_, e)) = lower.rsplit_once('.') else {
            // Extensionless files are JavaScript, unless `--loader:=x` says otherwise.
            return Some(self.loaders.get("").copied().unwrap_or(Loader::Js));
        };
        if lower.ends_with(".module.css") {
            return Some(Loader::LocalCss);
        }
        Some(match e {
            "js" | "mjs" | "cjs" => Loader::Js,
            "jsx" => Loader::Jsx,
            "ts" | "mts" | "cts" => Loader::Ts,
            "tsx" => Loader::Tsx,
            "json" => Loader::Json,
            "css" => Loader::Css,
            "txt" => Loader::Text,
            _ => return None,
        })
    }

    /// Validates the options; returns an error message for invalid combinations.
    pub fn validate(&self) -> Result<(), String> {
        if self.entry_points.is_empty() {
            return Err("no entry points".into());
        }
        if self.outfile.is_some() && self.outdir.is_some() {
            return Err("cannot use both \"outfile\" and \"outdir\"".into());
        }
        if self.splitting && self.effective_format() != Format::Esm {
            return Err("Splitting currently only works with the \"esm\" format".into());
        }
        if !self.bundle && (!self.external.is_empty() || self.packages_external) {
            return Err("Cannot use \"external\" without \"bundle\"".into());
        }
        if self.splitting && self.outdir.is_none() && self.write {
            return Err("splitting requires \"outdir\"".into());
        }
        if self.entry_points.len() > 1 && self.outfile.is_some() {
            return Err("must use \"outdir\" when there are multiple input files".into());
        }
        Ok(())
    }
}

/// Parses a `--target` list (`es2022`, `esnext`, `node20`, `chrome110,firefox115`) into the
/// ECMAScript year it implies (`None` = esnext).
pub fn parse_target(s: &str) -> Result<Option<u32>, String> {
    let mut year: Option<u32> = None;
    for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let lower = t.to_ascii_lowercase();
        let y = if lower == "esnext" {
            None
        } else if let Some(n) = lower.strip_prefix("es") {
            let n: u32 = n.parse().map_err(|_| format!("invalid target {t:?}"))?;
            Some(if n < 2015 { 2009 + n } else { n })
        } else {
            let split = lower
                .find(|c: char| c.is_ascii_digit())
                .ok_or_else(|| format!("invalid target {t:?}"))?;
            let (engine, ver) = lower.split_at(split);
            let major: u32 = ver
                .split('.')
                .next()
                .and_then(|m| m.parse().ok())
                .ok_or_else(|| format!("invalid target {t:?}"))?;
            // The first version of each engine with full ES2022 support.
            let es2022 = match engine {
                "node" => 16,
                "chrome" | "edge" => 94,
                "firefox" => 93,
                "safari" | "ios" => 16,
                "opera" => 80,
                "deno" => 1,
                _ => return Err(format!("unknown target engine {engine:?}")),
            };
            Some(if major >= es2022 { 2022 } else { 2021 })
        };
        year = match (year, y) {
            (None, y) => y,
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
        };
    }
    Ok(year)
}

/// Matches `pattern` (one optional `*` wildcard) against `path`.
pub fn wildcard_match(pattern: &str, path: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == path,
        Some((pre, post)) => {
            path.len() >= pre.len() + post.len() && path.starts_with(pre) && path.ends_with(post)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("esnext"), Ok(None));
        assert_eq!(parse_target("es2022"), Ok(Some(2022)));
        assert_eq!(parse_target("es2024,node20"), Ok(Some(2022)));
        assert_eq!(parse_target("chrome80"), Ok(Some(2021)));
        assert!(parse_target("bogus").is_err());
    }

    #[test]
    fn loaders_and_formats() {
        let o = BuildOptions::default();
        assert_eq!(o.loader_for("/a/b.tsx"), Loader::Tsx);
        assert_eq!(o.loader_for("/a/b.d.ts"), Loader::Ts);
        assert!(!o.has_loader("/a/b.png") && o.has_loader("/a/LICENSE"));
        let b = BuildOptions {
            bundle: true,
            platform: Platform::Node,
            ..BuildOptions::default()
        };
        assert_eq!(b.effective_format(), Format::Cjs);
        assert!(wildcard_match("@scope/*", "@scope/x"));
        assert!(!wildcard_match("@scope/*", "@other/x"));
    }
}
