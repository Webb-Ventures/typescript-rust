//! The plugin seam: Vite-style plugin hooks the dev server calls at fixed points.
//!
//! ts-r-dev implements [`PluginHost`] with a Node sidecar that loads `vite.config.*` and runs
//! the project's Vite plugins; the server never knows about Node. Where each hook runs:
//!
//! | Hook | Where |
//! |---|---|
//! | `resolveId` ([`Stage::Pre`]) | before the built-in resolver, batched per importing module |
//! | `resolveId` ([`Stage::Post`]) | for specifiers the built-in resolver couldn't resolve |
//! | `load` | plugin modules: ids a plugin resolved (virtual modules), ids with a query the server doesn't know (`icon.svg?react`) and imports of files that are neither scripts, CSS, JSON nor known assets (`.md`, `.yaml`) |
//! | `transform` ([`Stage::Pre`]) | on the source, before the built-in TS/JSX transform |
//! | `transform` ([`Stage::Post`]) | on the JS the built-in transform produced |
//! | `transformIndexHtml` | every served HTML page, before the client is injected |
//! | `handleHotUpdate` | every changed file, before the graph sees it |
//! | `configureServer` middlewares | requests no built-in route serves (before the SPA fallback), and every non-GET request |
//!
//! Plugin modules are graph modules whose "file" is the plugin id (`\0virtual:x`,
//! `/abs/icon.svg?react`) and whose URL is `/@id/<id>` ([`plugin_url`]).

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use crate::http::{Request, Response};

/// Which side of the built-in step a hook group runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Pre,
    Post,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Pre => "pre",
            Stage::Post => "post",
        }
    }
}

/// Which hooks the loaded plugins have; the server skips the calls for hooks nobody has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PluginHooks {
    pub resolve_pre: bool,
    pub resolve_post: bool,
    pub load: bool,
    pub transform_pre: bool,
    pub transform_post: bool,
    pub index_html: bool,
    pub hot_update: bool,
    pub middleware: bool,
    pub client_events: bool,
}

impl PluginHooks {
    /// Whether plugin modules exist at all (something can load or transform them).
    pub fn modules(&self) -> bool {
        self.load || self.transform_pre || self.transform_post
    }

    pub fn transforms(&self) -> bool {
        self.transform_pre || self.transform_post
    }
}

/// A `resolveId` answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginResolution {
    /// A module id: an absolute file path (optionally with a query) or a virtual id.
    Id(String),
    /// Left to the browser as is (`{ external: true }`).
    External(String),
    /// Resolve this specifier instead (`resolve.alias`).
    Rewrite(String),
}

/// What one stage of `transform` hooks made of a module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transformed {
    pub code: String,
    /// The source map (JSON) of each hook that changed the code, in the order they ran;
    /// `None` for a hook that returned no map (treated as not moving anything).
    pub maps: Vec<Option<String>>,
}

impl Transformed {
    /// Code without a map.
    pub fn code(code: impl Into<String>) -> Transformed {
        Transformed {
            code: code.into(),
            maps: Vec::new(),
        }
    }
}

/// `(specifier, importer id)` to `(id, external)`.
pub type ResolveFn = dyn Fn(&str, &str) -> Option<(String, bool)> + Send + Sync;

/// What the server lends a plugin host when it starts.
#[derive(Clone)]
pub struct ServerHandle {
    /// Sends one HMR payload (JSON text, Vite's protocol) to every connected page.
    pub broadcast: Arc<dyn Fn(&str) + Send + Sync>,
    /// The built-in resolver (`this.resolve` falls back to it): `(specifier, importer id)`
    /// to `(id, external)`.
    pub resolve: Arc<ResolveFn>,
}

impl fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerHandle").finish_non_exhaustive()
    }
}

/// Runs plugin hooks. Errors are messages shown in the error overlay (prefixed with the
/// plugin's name by the host).
pub trait PluginHost: Send + Sync + fmt::Debug {
    /// Identifies the plugins and their configuration in transform cache keys.
    fn cache_id(&self) -> &str;

    fn hooks(&self) -> PluginHooks;

    /// Called once when the server starts.
    fn attach(&self, server: ServerHandle) {
        let _ = server;
    }

    /// `resolveId` for each specifier imported by `importer` (a file path or plugin id).
    /// Returns one entry per specifier; `None` = no plugin claimed it.
    fn resolve_ids(
        &self,
        stage: Stage,
        importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String>;

    /// `load(id)`; `None` = no plugin loaded it.
    fn load(&self, id: &str) -> Result<Option<String>, String>;

    /// The `transform` hooks of one stage, chained; `None` = code unchanged.
    fn transform(&self, stage: Stage, id: &str, code: &str) -> Result<Option<Transformed>, String>;

    /// `transformIndexHtml` for the page at `url` (file `file`).
    fn transform_index_html(&self, url: &str, file: &Path, html: &str) -> Result<String, String> {
        let _ = (url, file);
        Ok(html.to_owned())
    }

    /// `handleHotUpdate` for a changed file whose modules have `module_urls`. `Some(list)`:
    /// only these modules are updated (empty = the plugin handled the change).
    fn handle_hot_update(
        &self,
        file: &Path,
        module_urls: &[String],
    ) -> Result<Option<Vec<String>>, String> {
        let _ = (file, module_urls);
        Ok(None)
    }

    /// `configureServer` middlewares; `None` = every middleware called `next()`.
    fn middleware(&self, req: &Request, body: &[u8]) -> Result<Option<Response>, String> {
        let _ = (req, body);
        Ok(None)
    }

    /// A custom event a page sent with `import.meta.hot.send` (`server.ws.on` listeners).
    fn client_event(&self, event: &str, data: &serde_json::Value) {
        let _ = (event, data);
    }
}

const NUL: &str = "__x00__";

/// Where plugin modules are served.
pub const URL_PREFIX: &str = "/@id/";

/// Whether a module URL is a plugin module's.
pub fn is_plugin_url(url: &str) -> bool {
    url.starts_with(URL_PREFIX)
}

/// The file behind a plugin id (`/abs/icon.svg?react` → `/abs/icon.svg`); the id itself for
/// virtual ids.
pub fn id_file(id: &str) -> &str {
    if id.starts_with('\0') {
        return id;
    }
    id.split(['?', '#']).next().unwrap_or(id)
}

/// `/@id/<id>`, with `\0` as `__x00__` and everything outside the URL path alphabet
/// (including `?`, `#` and `%`) percent-encoded.
pub fn plugin_url(id: &str) -> String {
    let id = id.replace('\0', NUL);
    let mut out = String::with_capacity(id.len() + 5);
    out.push_str("/@id/");
    for &c in id.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// The plugin id of a decoded request path below `/@id/`.
pub fn id_from_url_path(decoded_rest: &str) -> String {
    decoded_rest.replace(NUL, "\0")
}

/// A [`PluginHost`] that does nothing (tests, and projects without plugins).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPlugins;

impl PluginHost for NoPlugins {
    fn cache_id(&self) -> &str {
        "no-plugins"
    }

    fn hooks(&self) -> PluginHooks {
        PluginHooks::default()
    }

    fn resolve_ids(
        &self,
        _stage: Stage,
        _importer: &str,
        specifiers: &[String],
    ) -> Result<Vec<Option<PluginResolution>>, String> {
        Ok(vec![None; specifiers.len()])
    }

    fn load(&self, _id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn transform(
        &self,
        _stage: Stage,
        _id: &str,
        _code: &str,
    ) -> Result<Option<Transformed>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::percent_decode;

    #[test]
    fn plugin_urls_round_trip() {
        for id in [
            "\0virtual:greeting",
            "virtual:x",
            "/abs/dir with space/icon.svg?react",
            "/a/b.md?x=1#frag%",
        ] {
            let url = plugin_url(id);
            assert!(url.starts_with("/@id/"));
            assert!(!url[5..].contains(['?', '#', '\0', ' ']), "{url}");
            let back = id_from_url_path(&percent_decode(&url["/@id/".len()..]));
            assert_eq!(back, id);
        }
        assert_eq!(id_file("/a/icon.svg?react"), "/a/icon.svg");
        assert_eq!(id_file("\0virtual:x?y"), "\0virtual:x?y");
        assert!(is_plugin_url(&plugin_url("/a/notes.md")));
        assert!(!is_plugin_url("/src/a.ts"));
    }
}
