//! App CSS as the browser gets it: `@import`s inlined and file references rewritten to the
//! URLs the dev server serves them at, under the base. Used for stylesheets in the root
//! (linked, imported from JS) and for `<style>` blocks and `style` attributes of pages.
//!
//! The rules (Vite's dev CSS pipeline, `vite: packages/vite/src/node/plugins/css.ts`,
//! `urlResolver` and `packages/vite/src/node/plugins/asset.ts`, `fileToDevUrl`):
//!
//! - A reference is URL-decoded first. One naming a file in the public directory becomes
//!   `<base><path>` as written.
//! - Otherwise its path (without `?query` and `#fragment`) is resolved like an import that
//!   prefers relative files: `/x` from the root, `./x` and `x` from the stylesheet's directory,
//!   then aliases and packages through the resolver. A reference that resolves to nothing is
//!   kept as written.
//! - `?inline` makes a data URL; so does an SVG under the inline limit, unless the reference
//!   has a fragment or `?no-inline`. Everything else becomes `<base>/<path under the root>`
//!   (not percent-encoded; the CSS writer quotes it), with the query and fragment kept and
//!   `t=<last change>` added once the file has changed, so the browser fetches it again.
//! - Every file read or referenced is a dependency: a change to one updates the stylesheet.

use std::fs;
use std::path::{Path, PathBuf};

use crate::css::{self, CssHost};
use crate::http;
use crate::resolve::{Resolved, normalize};

use super::{Shared, ext_of, safe_relative, with_base};

/// The file `spec` (no query or fragment) names from `importer`, preferring relative files:
/// `/x` from the root, `./x` and `x` from the importer's directory, then aliases (plugins'
/// `resolveId`) and the resolver. Relative paths are tried as written unless `probe` (then
/// the resolver adds extensions, as for `new URL('./x', import.meta.url)`).
pub(super) fn resolve_ref(
    shared: &Shared,
    spec: &str,
    importer: &Path,
    probe: bool,
) -> Option<PathBuf> {
    if spec.is_empty() || spec.starts_with("//") || spec.contains("://") {
        return None;
    }
    let dir = importer.parent()?;
    let relative = spec.starts_with("./") || spec.starts_with("../");
    if let Some(rest) = spec.strip_prefix('/') {
        let file = shared.root.join(safe_relative(rest)?);
        if file.is_file() {
            return Some(file);
        }
    } else {
        let file = normalize(&dir.join(spec));
        if file.is_file() {
            return Some(file);
        }
        if relative && !probe {
            return None;
        }
    }
    let mut spec = spec.to_owned();
    if !relative && let Some(s) = super::alias_rewrite(shared, &spec, &crate::paths::id(importer)) {
        let p = Path::new(&s);
        if p.is_absolute() && p.is_file() {
            return Some(normalize(p));
        }
        spec = s;
    }
    match shared.resolver.resolve(&spec, importer).ok()? {
        Resolved::File(f) => Some(f),
        Resolved::Dep { file, .. } => Some(file),
        Resolved::External(_) => None,
    }
}

/// One stylesheet's host. `deps` collects the files it read or referenced, sorted.
pub(super) struct AppCss<'a> {
    shared: &'a Shared,
    deps: Vec<PathBuf>,
}

impl<'a> AppCss<'a> {
    pub(super) fn new(shared: &'a Shared) -> Self {
        AppCss {
            shared,
            deps: Vec::new(),
        }
    }

    /// `css` (the text of `file`, or of a page's `<style>` when `file` is the page) as the
    /// browser gets it.
    pub(super) fn transform(&mut self, file: &Path, css: &str) -> String {
        css::bundle_text(file, css, self)
    }

    /// The files read or referenced so far, sorted and without duplicates.
    pub(super) fn into_deps(mut self) -> Vec<PathBuf> {
        self.deps.sort();
        self.deps.dedup();
        self.deps
    }

    fn public_file(&self, path: &str) -> bool {
        let Some(public) = &self.shared.public_dir else {
            return false;
        };
        let Some(rest) = path.strip_prefix('/').filter(|r| !r.starts_with('/')) else {
            return false;
        };
        safe_relative(rest)
            .is_some_and(|rel| !rel.as_os_str().is_empty() && public.join(&rel).is_file())
    }

    /// The URL a resolved file is served at, before the base.
    fn served_path(&self, file: &Path) -> String {
        let root = &self.shared.root;
        if let Ok(rel) = file.strip_prefix(root) {
            let parts: Vec<_> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            return format!("/{}", parts.join("/"));
        }
        if file.components().any(|c| c.as_os_str() == "node_modules") {
            return css::asset_path(root, file);
        }
        super::watch_outside(self.shared, file);
        let abs = file.to_string_lossy().replace('\\', "/");
        format!("/@fs/{}", abs.trim_start_matches('/'))
    }
}

impl CssHost for AppCss<'_> {
    fn read(&mut self, file: &Path) -> Option<String> {
        let text = fs::read_to_string(file).ok();
        self.deps.push(file.to_path_buf());
        text
    }

    fn import(&mut self, target: &str, importer: &Path) -> Option<PathBuf> {
        let clean = target.split(['?', '#']).next().unwrap_or(target);
        if ext_of(Path::new(clean)) != "css" && !clean.ends_with('/') && clean.contains('.') {
            // `@import 'x.scss'` and friends need their preprocessor; keep them.
            return None;
        }
        if let Some(f) = resolve_ref(self.shared, clean, importer, false) {
            return Some(f);
        }
        // A public stylesheet is inlined from the public directory.
        if self.public_file(clean)
            && let Some(public) = &self.shared.public_dir
        {
            return Some(public.join(safe_relative(&clean[1..])?));
        }
        None
    }

    fn url(&mut self, raw: &str, importer: &Path) -> Option<String> {
        let decoded = http::percent_decode(raw);
        let (path, fragment) = match decoded.find('#') {
            Some(k) => (&decoded[..k], &decoded[k..]),
            None => (decoded.as_str(), ""),
        };
        let (clean, query) = match path.find('?') {
            Some(k) => (&path[..k], &path[k..]),
            None => (path, ""),
        };
        if self.public_file(clean) {
            return Some(with_base(&self.shared.base, &decoded));
        }
        let file = resolve_ref(self.shared, clean, importer, false)?;
        self.deps.push(file.clone());
        let ext = ext_of(&file);
        let has = |name: &str| {
            query
                .trim_start_matches('?')
                .split('&')
                .any(|p| p.split('=').next() == Some(name))
        };
        let inline = has("inline")
            || (fragment.is_empty()
                && !has("no-inline")
                && fs::metadata(&file).is_ok_and(|m| self.shared.inlines(&file, &ext, m.len())));
        if inline && let Ok(bytes) = fs::read(&file) {
            return Some(crate::assets::data_url(
                &ext,
                crate::render::content_type(&ext),
                &bytes,
            ));
        }
        let mut query = query.to_owned();
        if let Some(t) = self.shared.changed_at(&file) {
            query.push(if query.is_empty() { '?' } else { '&' });
            query.push_str(&format!("t={t}"));
        }
        let url = self.served_path(&file);
        Some(format!(
            "{}{query}{fragment}",
            with_base(&self.shared.base, &url)
        ))
    }
}
