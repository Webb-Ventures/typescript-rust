//! Loading and preparing one module: loader conversion, parse, TypeScript/JSX transform,
//! `define` substitution, constant folding and scope analysis.
//!
//! Preparation is deterministic: preparing the same source with the same settings yields
//! the same tree, node ids and symbol ids. The scanner and the printer both rely on it (the
//! printer may re-prepare a module whose tree was dropped and expects the ids the linker
//! saw).

use std::sync::{Arc, RwLock};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{
    Ast, BinaryOp, Modifiers, Node, NodeId, NodeKind, SourceKind, Span, UnaryOp, VarKind,
};
use ts_r_config::jsonc::{self, Value, ValueKind};
use ts_r_config::{JsxEmit, ScriptTarget};
use ts_r_minify::{ScopeId, Scopes, SymbolId, SymbolKind, build, names};
use ts_r_transform::{JsxMode, ModuleFormat, TransformOptions};

use crate::features::Features;
use crate::options::{BuildOptions, Format, Loader, Platform};
use crate::resolve::Resolver;
use crate::{Message, Severity};

/// A loaded file, converted to JavaScript where needed.
#[derive(Clone, Debug)]
pub struct Source {
    /// JavaScript source text (the file itself for JS/TS loaders).
    pub js: Arc<str>,
    /// CSS text for the `css` loader.
    pub css: Option<Arc<str>>,
    /// Bytes to emit as a separate file (the `file` loader).
    pub asset: Option<Asset>,
    /// Top-level JSON object keys before rewriting JavaScript-only computed properties.
    pub json_keys: Option<Arc<[String]>>,
    /// Hash of the raw file contents and loader, and of anything else `js` depends on (the
    /// `file` loader's URL). Part of the module's preparation key.
    pub hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct Asset {
    /// Output path relative to the output directory, with `[dir]` still in it: the build
    /// fills it in from the entry points' common directory (`bundle::asset_path`).
    pub rel_path: String,
    pub bytes: Arc<[u8]>,
    /// What the module's code holds in place of its URL: the build replaces it in each chunk
    /// with the URL as seen from that chunk (`./a.png`, `../a.png`, or under `public_path`).
    pub placeholder: String,
    /// The `?query` / `#hash` the import named the file with; the URL ends with it.
    pub query: String,
}

/// The placeholder string for the asset of the module at `path` with contents hash `hash`:
/// letters, digits and `_` only, so printing and minifying keep it as written.
pub fn asset_placeholder(path: &str, hash: &[u8; 32]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(path.as_bytes());
    h.update(&[0]);
    h.update(hash);
    let d = h.finalize();
    let hex: String = d.as_bytes()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("__TSR_ASSET_{hex}__")
}

/// Reads `path` with `loader`. Errors are user-facing messages.
pub fn load(
    fs: &dyn ts_r_config::FileSystem,
    path: &str,
    loader: Loader,
    opts: &BuildOptions,
) -> Result<Source, String> {
    // `key` keeps a `?query` suffix for the file loader's URL; the file is read without it.
    let key = path;
    let (path, query) = crate::graph::split_query(key);
    let data = || crate::data_url::decode(path).ok_or_else(|| format!("Invalid data URL: {path}"));
    let read_text = || {
        if path.starts_with(crate::data_url::PREFIX) {
            return data().map(|b| String::from_utf8_lossy(&b).into_owned());
        }
        fs.read_file(path)
            .ok_or_else(|| format!("Could not read from file: {path}"))
    };
    let read_bytes = || -> Result<Vec<u8>, String> {
        if path.starts_with(crate::data_url::PREFIX) {
            return data();
        }
        fs.read_bytes(path)
            .ok_or_else(|| format!("Could not read from file: {path}"))
    };
    let hash_of = |bytes: &[u8]| {
        let mut h = blake3::Hasher::new();
        h.update(loader.as_str().as_bytes());
        h.update(&[0]);
        h.update(bytes);
        *h.finalize().as_bytes()
    };
    let js_value = |expr: String, hash| Source {
        js: Arc::from(format!("module.exports = {expr};\n")),
        css: None,
        asset: None,
        json_keys: None,
        hash,
    };
    match loader {
        Loader::Js | Loader::Jsx | Loader::Ts | Loader::Tsx => {
            let text = read_text()?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
            let hash = hash_of(text.as_bytes());
            Ok(Source {
                js: Arc::from(text),
                css: None,
                asset: None,
                json_keys: None,
                hash,
            })
        }
        Loader::Json => {
            let text = read_text()?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let parsed = jsonc::parse(text, path);
            if let Some(d) = parsed.diagnostics.first() {
                return Err(format!("{path}: invalid JSON: {}", d.message));
            }
            if let Some(lit) = json_bigint(text) {
                return Err(format!("{path}: Unexpected {lit:?} in JSON"));
            }
            let hash = hash_of(text.as_bytes());
            let body = if text.trim().is_empty() {
                "{}".to_owned()
            } else {
                // JSON is a JavaScript expression; parenthesize so `{` isn't a block. Below
                // ES2015 there are no computed keys, and esbuild prints `"__proto__"` as is.
                let text = text.trim_end();
                if opts
                    .unsupported()
                    .has(crate::features::Feature::ObjectExtensions)
                {
                    format!("(\n{text}\n)")
                } else {
                    format!("(\n{}\n)", computed_proto_keys(text))
                }
            };
            let mut source = js_value(body, hash);
            source.json_keys = parsed
                .root
                .as_ref()
                .and_then(Value::as_object)
                .map(|members| members.iter().map(|m| m.key.clone()).collect());
            Ok(source)
        }
        Loader::Text => {
            let text = read_text()?;
            let hash = hash_of(text.as_bytes());
            Ok(js_value(js_string(&text), hash))
        }
        Loader::Base64 => {
            let bytes = read_bytes()?;
            let hash = hash_of(&bytes);
            let mut b = String::new();
            ts_r_codegen::sourcemap::base64_encode(&bytes, &mut b);
            Ok(js_value(js_string(&b), hash))
        }
        Loader::DataUrl => {
            let bytes = read_bytes()?;
            let hash = hash_of(&bytes);
            Ok(js_value(js_string(&data_url(path, &bytes)), hash))
        }
        Loader::Binary => {
            let bytes = read_bytes()?;
            let hash = hash_of(&bytes);
            let mut b = String::new();
            ts_r_codegen::sourcemap::base64_encode(&bytes, &mut b);
            Ok(js_value(
                format!(
                    "Uint8Array.from(atob({}), (c) => c.charCodeAt(0))",
                    js_string(&b)
                ),
                hash,
            ))
        }
        Loader::Bytes => {
            let bytes = read_bytes()?;
            let hash = hash_of(&bytes);
            let mut b = String::new();
            ts_r_codegen::sourcemap::base64_encode(&bytes, &mut b);
            Ok(js_value(
                format!("Uint8Array.fromBase64({})", js_string(&b)),
                hash,
            ))
        }
        Loader::File => {
            let bytes = read_bytes()?;
            let hash = hash_of(&bytes);
            let name = path.rsplit('/').next().unwrap_or(path);
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) => (s, format!(".{e}")),
                None => (name, String::new()),
            };
            let hash_text = crate::hash_text(&hash);
            let rel = opts
                .asset_names
                .replace("[name]", stem)
                .replace("[hash]", &hash_text)
                .replace("[ext]", ext.trim_start_matches('.'))
                + &ext;
            let placeholder = asset_placeholder(key, &hash);
            // The output path comes from `asset_names` and `public_path`, not only the bytes.
            let mut key = blake3::Hasher::new();
            key.update(&hash);
            key.update(rel.as_bytes());
            key.update(&[0]);
            key.update(opts.public_path.as_bytes());
            let mut s = js_value(js_string(&placeholder), *key.finalize().as_bytes());
            s.asset = Some(Asset {
                rel_path: rel,
                bytes: Arc::from(bytes),
                placeholder,
                query: query.to_owned(),
            });
            Ok(s)
        }
        Loader::Css => {
            let text = read_text()?;
            let hash = hash_of(text.as_bytes());
            Ok(Source {
                js: Arc::from(""),
                css: Some(Arc::from(text)),
                asset: None,
                json_keys: None,
                hash,
            })
        }
        Loader::Empty => Ok(Source {
            js: Arc::from(""),
            css: None,
            asset: None,
            json_keys: None,
            hash: hash_of(b""),
        }),
    }
}

fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    ts_r_codegen::literals::quote_string(s, '"', &mut out);
    out
}

/// A `data:` URL for a file, as esbuild's `dataurl` loader writes it (observed behaviour):
/// the MIME type comes from the extension, or is sniffed (text vs binary) for other
/// extensions; text types say `charset=utf-8`. Valid UTF-8 is written as text with only
/// tab, newline, carriage return, `#`, `%XX` lookalikes and trailing spaces percent-escaped,
/// unless base64 is no longer; anything else is base64.
pub fn data_url(path: &str, bytes: &[u8]) -> String {
    let mime = mime_type(path, bytes);
    let mut b64 = String::new();
    ts_r_codegen::sourcemap::base64_encode(bytes, &mut b64);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return format!("data:{mime};base64,{b64}");
    };
    let body = text.trim_end_matches(' ');
    let trailing = text.len() - body.len();
    let raw = body.as_bytes();
    let mut pct = String::with_capacity(text.len());
    for (i, c) in body.char_indices() {
        match c {
            '\t' => pct.push_str("%09"),
            '\n' => pct.push_str("%0A"),
            '\r' => pct.push_str("%0D"),
            '#' => pct.push_str("%23"),
            '%' if raw.len() > i + 2
                && raw[i + 1].is_ascii_hexdigit()
                && raw[i + 2].is_ascii_hexdigit() =>
            {
                pct.push_str("%25")
            }
            c => pct.push(c),
        }
    }
    for _ in 0..trailing {
        pct.push_str("%20");
    }
    if b64.len() + ";base64".len() <= pct.len() {
        format!("data:{mime};base64,{b64}")
    } else {
        format!("data:{mime},{pct}")
    }
}

fn mime_type(path: &str, bytes: &[u8]) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "json" => "application/json;charset=utf-8",
        "css" => "text/css;charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript;charset=utf-8",
        "html" | "htm" => "text/html;charset=utf-8",
        "xml" => "text/xml;charset=utf-8",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        // Sniffed like unlabeled content: a control byte other than whitespace and ESC
        // makes it binary.
        _ if bytes
            .iter()
            .any(|&b| matches!(b, 0..=8 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F)) =>
        {
            "application/octet-stream"
        }
        _ => "text/plain;charset=utf-8",
    }
}

// ---- define ----

/// A `define` replacement value.
#[derive(Clone, Debug, PartialEq)]
pub enum DefineValue {
    Json(Value),
    /// `globalThis`, `window.foo`.
    Path(Vec<String>),
}

/// Parsed `define` table.
#[derive(Clone, Debug, Default)]
pub struct Defines {
    /// Key path (`["process", "env", "NODE_ENV"]`) and value.
    pub entries: Vec<(Vec<String>, DefineValue)>,
    /// Root identifiers of all keys (fast rejection).
    roots: FxHashSet<String>,
}

impl Defines {
    /// Builds the table from the options, adding esbuild's automatic
    /// `process.env.NODE_ENV` for browser builds.
    pub fn from_options(opts: &BuildOptions) -> Result<Defines, String> {
        let mut d = Defines::default();
        for (k, v) in &opts.define {
            let key: Vec<String> = k.split('.').map(str::to_owned).collect();
            if key.iter().any(|p| !names::is_identifier(p)) {
                return Err(format!("Invalid define key: {k:?}"));
            }
            d.push(key, parse_define_value(v)?);
        }
        if opts.platform == Platform::Browser
            && opts.bundle
            && !d.entries.iter().any(|(k, _)| {
                k.len() == 3 && k[0] == "process" && k[1] == "env" && k[2] == "NODE_ENV"
            })
        {
            let v = if opts.minify_any() {
                "production"
            } else {
                "development"
            };
            d.push(
                vec!["process".into(), "env".into(), "NODE_ENV".into()],
                parse_define_value(&format!("\"{v}\""))?,
            );
        }
        Ok(d)
    }

    fn push(&mut self, key: Vec<String>, v: DefineValue) {
        self.roots.insert(key[0].clone());
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push((key, v));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A stable string for cache keys.
    pub fn cache_key(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.entries {
            s.push_str(&k.join("."));
            s.push('=');
            match v {
                DefineValue::Json(j) => s.push_str(&j.to_json()),
                DefineValue::Path(p) => s.push_str(&p.join(".")),
            }
            s.push('\n');
        }
        s
    }

    fn lookup(&self, path: &[&str]) -> Option<&DefineValue> {
        self.entries
            .iter()
            .find(|(k, _)| k.len() == path.len() && k.iter().zip(path).all(|(a, b)| a == b))
            .map(|(_, v)| v)
    }
}

fn parse_define_value(v: &str) -> Result<DefineValue, String> {
    let t = v.trim();
    if !t.is_empty()
        && t.split('.').all(names::is_identifier)
        && !matches!(t, "true" | "false" | "null")
    {
        return Ok(DefineValue::Path(t.split('.').map(str::to_owned).collect()));
    }
    let parsed = jsonc::parse(t, "<define>");
    match parsed.root {
        Some(root) if parsed.diagnostics.is_empty() => Ok(DefineValue::Json(root)),
        _ => Err(format!(
            "Invalid define value: {v:?} (must be JSON or an identifier)"
        )),
    }
}

fn build_define_value(ast: &mut Ast, v: &DefineValue, at: u32) -> NodeId {
    match v {
        DefineValue::Path(p) => {
            if p.len() == 1 && p[0] == "undefined" {
                return build::undefined(ast, at);
            }
            let mut e = build::ident(ast, &p[0], at);
            for seg in &p[1..] {
                e = build::member(ast, e, seg, at);
            }
            e
        }
        DefineValue::Json(j) => build_json(ast, j, at, 0),
    }
}

fn build_json(ast: &mut Ast, v: &Value, at: u32, depth: u32) -> NodeId {
    if depth > 64 {
        return build::null(ast, at);
    }
    match &v.kind {
        ValueKind::Null | ValueKind::Invalid => build::null(ast, at),
        ValueKind::Bool(b) => build::boolean(ast, *b, at),
        ValueKind::Number(n) => {
            if *n < 0.0 {
                let pos = build::number(ast, -n, at);
                build::unary(ast, UnaryOp::Minus, pos, at)
            } else {
                build::number(ast, *n, at)
            }
        }
        ValueKind::String(s) => build::string(ast, s, at),
        ValueKind::Array(items) => {
            let ids: Vec<NodeId> = items
                .iter()
                .map(|i| build_json(ast, i, at, depth + 1))
                .collect();
            build::array(ast, &ids, at)
        }
        ValueKind::Object(members) => {
            let vals: Vec<(String, NodeId)> = members
                .iter()
                .map(|m| (m.key.clone(), build_json(ast, &m.value, at, depth + 1)))
                .collect();
            let refs: Vec<(&str, NodeId)> = vals.iter().map(|(k, v)| (k.as_str(), *v)).collect();
            build::object(ast, &refs, at)
        }
    }
}

/// Replaces `define` keys in read positions. Returns true when anything changed.
fn apply_defines(ast: &mut Ast, scopes: &Scopes, defines: &Defines) -> bool {
    // Unresolved identifier references.
    let global_refs: FxHashSet<u32> = scopes
        .refs
        .iter()
        .filter(|r| r.symbol.is_none())
        .map(|r| r.node.raw())
        .collect();
    if global_refs.is_empty() {
        return false;
    }
    let mut writes: FxHashSet<u32> = FxHashSet::default();
    for n in &ast.nodes {
        match *n {
            Node::Assign { left, .. } => {
                writes.insert(left.raw());
            }
            Node::Update { arg, .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                arg,
            } => {
                writes.insert(arg.raw());
            }
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => {
                writes.insert(left.raw());
            }
            _ => {}
        }
    }
    let mut changed = false;
    let count = ast.nodes.len() as u32;
    // Parents have larger ids than their children: visit them first so the longest key wins.
    for raw in (1..count).rev() {
        let Some(id) = NodeId::from_raw(raw) else {
            continue;
        };
        if writes.contains(&raw) {
            continue;
        }
        let mut path: Vec<&str> = Vec::new();
        let mut cur = id;
        let ok = loop {
            match *ast.node(cur) {
                Node::Member {
                    obj,
                    optional: false,
                    prop,
                } => match *ast.node(prop) {
                    Node::Ident { name } => {
                        path.push(ast.atom(name));
                        cur = obj;
                    }
                    _ => break false,
                },
                Node::Ident { name } => {
                    path.push(ast.atom(name));
                    break global_refs.contains(&cur.raw());
                }
                _ => break false,
            }
        };
        if !ok || !defines.roots.contains(*path.last().unwrap_or(&"")) {
            continue;
        }
        path.reverse();
        let Some(v) = defines.lookup(&path).cloned() else {
            continue;
        };
        let at = ast.span(id).start;
        let new = build_define_value(ast, &v, at);
        build::replace(ast, id, new);
        changed = true;
    }
    changed
}

// ---- transform options ----

/// Transform options and the cache key of everything that affects preparation with them.
type OptionsEntry = (Arc<TransformOptions>, Arc<str>);

/// Per-build preparation settings shared by all workers.
#[derive(Debug)]
pub struct PrepareContext {
    pub opts: Arc<BuildOptions>,
    pub defines: Defines,
    pub resolver: Arc<Resolver>,
    /// tsconfig path (or "") -> base transform options and their [`Self::cache_key`].
    ts: RwLock<FxHashMap<String, OptionsEntry>>,
}

impl PrepareContext {
    pub fn new(opts: Arc<BuildOptions>, defines: Defines, resolver: Arc<Resolver>) -> Self {
        PrepareContext {
            opts,
            defines,
            resolver,
            ts: RwLock::default(),
        }
    }

    /// Forgets the transform options read from tsconfig files (one changed).
    pub fn clear_options(&self) {
        self.ts.write().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Transform options for a file (from the nearest tsconfig outside `node_modules`).
    pub fn transform_options(&self, path: &str) -> Arc<TransformOptions> {
        self.options_entry(path).0
    }

    fn options_entry(&self, path: &str) -> OptionsEntry {
        let dir = ts_r_config::path::directory_of(path);
        let config = if path.contains("/node_modules/") && self.opts.tsconfig.is_none() {
            None
        } else {
            self.resolver.tsconfig_for(dir)
        };
        let key = config.clone().unwrap_or_default();
        if let Some(t) = self.ts.read().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return t.clone();
        }
        let t = Arc::new(self.compute_options(config.as_deref()));
        let cache_key: Arc<str> = Arc::from(format!(
            "{}|{}|{}|{:?}|{}",
            t.cache_key(),
            self.defines.cache_key(),
            self.opts.ignore_annotations,
            self.opts.effective_format(),
            self.opts.unsupported().key()
        ));
        let entry = (t, cache_key);
        self.ts
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, entry.clone());
        entry
    }

    fn compute_options(&self, config: Option<&str>) -> TransformOptions {
        let (mut t, tsconfig_jsx) = match config {
            Some(c) => {
                let parsed = ts_r_config::load_tsconfig(self.resolver.fs().as_ref(), c);
                // These options are shared across files: do not infer ESM from a package
                // format that has not been determined for any particular file.
                let mut t = TransformOptions::from_compiler_options(&parsed.options, "", true);
                t.always_strict = explicit_always_strict(&parsed.options);
                (t, parsed.options.effective().jsx())
            }
            None => (
                TransformOptions {
                    always_strict: false,
                    ..TransformOptions::default()
                },
                None,
            ),
        };
        t.module = ModuleFormat::Preserve;
        t.target = lowering_target(self.opts.unsupported());
        t.rewrite_relative_import_extensions = false;
        let jsx = &self.opts.jsx;
        t.jsx = match jsx.mode.as_deref() {
            Some("preserve") => JsxMode::Preserve,
            Some("automatic") => {
                if jsx.dev {
                    JsxMode::AutomaticDev
                } else {
                    JsxMode::Automatic
                }
            }
            Some(_) => JsxMode::Classic,
            None => match tsconfig_jsx {
                // `"jsx": "preserve"` (and `react-native`) in a tsconfig is ignored, as in
                // esbuild: the output is JavaScript, so JSX is compiled. `--jsx=preserve`
                // keeps it.
                Some(JsxEmit::React | JsxEmit::Preserve | JsxEmit::ReactNative) | None => {
                    JsxMode::Classic
                }
                // `--jsx-dev` picks the development runtime for `"jsx": "react-jsx"` too.
                Some(JsxEmit::ReactJsx) if jsx.dev => JsxMode::AutomaticDev,
                Some(JsxEmit::ReactJsx) => JsxMode::Automatic,
                Some(JsxEmit::ReactJsxDev) => JsxMode::AutomaticDev,
            },
        };
        if let Some(f) = &jsx.factory {
            t.jsx_factory = f.clone();
        } else if config.is_none() {
            t.jsx_factory = "React.createElement".into();
        }
        if let Some(f) = &jsx.fragment {
            t.jsx_fragment_factory = f.clone();
        } else if config.is_none() {
            t.jsx_fragment_factory = "React.Fragment".into();
        }
        if let Some(s) = &jsx.import_source {
            t.jsx_import_source = s.clone();
        }
        t
    }

    /// A stable string describing everything that affects preparation of `path`.
    pub fn cache_key(&self, path: &str) -> Arc<str> {
        self.options_entry(path).1
    }
}

/// The `ts_r_transform` target that lowers every unsupported feature lowering by year
/// handles (`features.rs`); ES2015 is the lowest tsc 7 emits.
pub fn lowering_target(unsupported: Features) -> ScriptTarget {
    match unsupported.lower_year() {
        None => ScriptTarget::ESNext,
        Some(..=2015) => ScriptTarget::ES2015,
        Some(2016) => ScriptTarget::ES2016,
        Some(2017) => ScriptTarget::ES2017,
        Some(2018) => ScriptTarget::ES2018,
        Some(2019) => ScriptTarget::ES2019,
        Some(2020) => ScriptTarget::ES2020,
        Some(2021) => ScriptTarget::ES2021,
        Some(2022) => ScriptTarget::ES2022,
        Some(2023) => ScriptTarget::ES2023,
        Some(2024) => ScriptTarget::ES2024,
        Some(2025) => ScriptTarget::ES2025,
        Some(_) => ScriptTarget::ESNext,
    }
}

// ---- prepare ----

/// A parsed, transformed and analyzed module.
#[derive(Debug)]
pub struct Prepared {
    pub ast: Ast,
    pub scopes: Scopes,
    pub src: Arc<str>,
    /// Erased source ranges from the transform (comments in them are dropped).
    pub erased: Vec<Span>,
    /// Runtime helpers the transform needs (`__decorate`, ...).
    pub prelude: String,
    /// The synthesized symbol for an anonymous `export default`.
    pub default_sym: Option<SymbolId>,
    pub messages: Vec<Message>,
    pub strict: bool,
}

fn source_kind(path: &str, loader: Loader) -> SourceKind {
    match loader {
        Loader::Ts => SourceKind::Ts,
        Loader::Tsx => SourceKind::Tsx,
        Loader::Jsx => SourceKind::Jsx,
        _ => {
            let lower = path.to_ascii_lowercase();
            if lower.ends_with(".jsx") {
                SourceKind::Jsx
            } else {
                SourceKind::Js
            }
        }
    }
}

/// Parse error codes that are not errors for bundling (top-level `return` in CommonJS).
fn ignored_parse_error(code: u32, kind: SourceKind) -> bool {
    !kind.is_typescript() && code == 1108
}

/// tsconfig's `alwaysStrict` as esbuild reads it: only when set, directly or through
/// `strict` (tsc 7 defaults both to true; esbuild does not).
fn explicit_always_strict(o: &ts_r_config::CompilerOptions) -> bool {
    use ts_r_config::{OptionId, OptionValue};
    let flag = |id| match o.get(id) {
        Some(OptionValue::Bool(b)) => Some(*b),
        _ => None,
    };
    flag(OptionId::AlwaysStrict)
        .or_else(|| flag(OptionId::Strict))
        .unwrap_or(false)
}

/// Prepares one module. `path` is the absolute path (for settings), `display` the name used
/// in messages and generated code. The tree may be deep; call on a thread with a large
/// stack.
pub fn prepare(
    ctx: &PrepareContext,
    path: &str,
    display: &str,
    src: Arc<str>,
    loader: Loader,
) -> Prepared {
    prepare_with(ctx, path, display, src, loader, None)
}

/// [`prepare`] with an output module format for the transform (non-bundle builds convert
/// ES modules to CommonJS here; bundles always preserve module syntax).
pub fn prepare_with(
    ctx: &PrepareContext,
    path: &str,
    display: &str,
    src: Arc<str>,
    loader: Loader,
    module: Option<ModuleFormat>,
) -> Prepared {
    let path = crate::graph::split_query(path).0;
    let kind = source_kind(path, loader);
    let parsed = ts_r_parser::parse(&src, kind);
    let mut messages = Vec::new();
    let mut lines = None;
    // As esbuild: tsc's checker grammar errors fail the build only when they are
    // ECMAScript early errors (`ts_r_transform::build_errors`).
    for d in ts_r_transform::build_errors(&parsed) {
        if ignored_parse_error(d.code, kind) {
            continue;
        }
        let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
        messages.push(lines.message(Severity::Error, display, d.span.start, d.message.clone()));
    }
    let mut ast = parsed.ast;
    let mut erased = Vec::new();
    let mut prelude = String::new();
    let first_jsx = ast
        .nodes
        .iter()
        .position(|n| matches!(n.kind(), NodeKind::JsxElement | NodeKind::JsxFragment));
    if let Some(i) = first_jsx
        && loader == Loader::Js
        && messages.is_empty()
    {
        let span = ast.spans.get(i).map_or(0, |s| s.start);
        let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
        messages.push(lines.message(
            Severity::Error,
            display,
            span,
            "The JSX syntax extension is not currently enabled".into(),
        ));
    }
    // Syntax the target forbids that lowering does not handle (`lowering.rs`).
    let unsupported = ctx.opts.unsupported();
    if messages.is_empty() {
        for (at, text) in crate::lowering::errors(&ast, unsupported) {
            let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
            messages.push(lines.message(Severity::Error, display, at, text));
        }
    }
    // Lowering a top-level `using` turns the module's constants into variables, so a write
    // to one would no longer throw: esbuild makes it an error.
    if messages.is_empty()
        && unsupported.has(crate::features::Feature::Using)
        && build::program_stmts(&ast).iter().any(|&s| {
            matches!(
                ast.node(s),
                Node::VarDecl {
                    kind: VarKind::Using | VarKind::AwaitUsing,
                    ..
                }
            )
        })
    {
        let scopes = Scopes::analyze(&ast, true);
        for r in scopes.refs.iter().filter(|r| r.write) {
            let Some(sym) = r.symbol else { continue };
            let sym = scopes.symbol(sym);
            if sym.scope == ScopeId::MODULE
                && matches!(sym.kind, SymbolKind::Const | SymbolKind::Using)
            {
                let name = ast.atom(sym.name);
                let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
                messages.push(lines.message(
                    Severity::Error,
                    display,
                    ast.span(r.node).start,
                    format!("Cannot assign to {name:?} because it is a constant"),
                ));
            }
        }
    }
    // `import.meta` below its target becomes the module's empty object (`plan.rs`); esbuild
    // warns at each use in the source.
    let mut late = Vec::new();
    if unsupported.has(crate::features::Feature::ImportMeta) {
        for (i, n) in ast.nodes.iter().enumerate() {
            if let Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::ImportMeta,
            } = n
            {
                let at = ast.spans.get(i).map_or(0, |s| s.start);
                let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
                late.push(lines.message(
                    Severity::Warning,
                    display,
                    at,
                    "\"import.meta\" is not available in the configured target environment and will be empty".into(),
                ));
            }
        }
    }
    // JavaScript files go through the transform when the target needs lowering.
    let lower_js = !kind.is_typescript()
        && crate::lowering::js_needs_lowering(&ast, lowering_target(unsupported));
    let needs_transform =
        kind.is_typescript() || first_jsx.is_some() || module.is_some() || lower_js;
    if needs_transform && messages.is_empty() {
        let base = ctx.transform_options(path);
        let mut topts = (*base).clone();
        topts.jsx_file_name = display.to_owned();
        if !kind.is_typescript() {
            // JavaScript class fields always have define semantics.
            topts.use_define_for_class_fields = true;
        }
        if let Some(m) = module {
            topts.module = m;
        }
        if anonymous_default(&ast).is_some() {
            // Lowering a class names it as the bundler names the default export.
            topts.default_class_name = Some(format!("{}_default", file_stem_name(path)));
        }
        if kind.is_typescript() && exports_const_enum(&ast) {
            // Files are transformed one at a time, so an exported const enum can't be inlined
            // into its importers: keep its object, as `isolatedModules` does.
            topts.preserve_const_enums = true;
        }
        let r = ts_r_transform::transform(&mut ast, &src, &topts);
        if unsupported.has(crate::features::Feature::ConstAndLet) {
            // The source has none (`lowering::errors`), so these are the transform's own
            // (`import x = require()` is `const x`): esbuild declares them with `var`.
            for n in &mut ast.nodes {
                if let Node::VarDecl { kind, .. } = n
                    && matches!(kind, VarKind::Const | VarKind::Let)
                {
                    *kind = VarKind::Var;
                }
            }
        }
        erased = r.erased;
        prelude = r.prelude;
        for (span, what) in &r.unlowered {
            let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
            messages.push(lines.message(
                Severity::Error,
                display,
                span.start,
                format!(
                    "Transforming {what} to the configured target environment is not supported yet"
                ),
            ));
        }
        for d in r.diagnostics {
            if d.is_error() {
                let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
                messages.push(lines.message(Severity::Warning, display, d.span.start, d.message));
            }
        }
    }
    messages.append(&mut late);
    let strict = build::program_stmts(&ast).iter().any(|&s| {
        matches!(
            ast.kind(s),
            NodeKind::ImportDecl
                | NodeKind::ExportNamed
                | NodeKind::ExportAll
                | NodeKind::ExportDefaultExpr
        ) || ast.node(s).modifiers().contains(Modifiers::EXPORT)
    });
    // `with` and `delete name` in strict code, as esbuild reports them.
    for (at, what, local) in ts_r_transform::sloppy_only_syntax(&ast) {
        let why = if local || (kind.is_typescript() && ctx.transform_options(path).always_strict) {
            "in strict mode"
        } else if strict {
            "in an ECMAScript module"
        } else if ctx.opts.format == Some(Format::Esm)
            || (ctx.opts.bundle && ctx.opts.effective_format() == Format::Esm)
        {
            "with the \"esm\" output format due to strict mode"
        } else {
            continue;
        };
        let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
        messages.push(lines.message(
            Severity::Error,
            display,
            at,
            format!("{} cannot be used {why}", what.what()),
        ));
    }
    // Top-level `this` is `undefined` in an ES module, and `return` is an error there (a
    // top-level `await` also makes the file a module for this, as in esbuild).
    let has_return = || ast.nodes.iter().any(|n| matches!(n, Node::Return { .. }));
    if (strict
        && ast
            .nodes
            .iter()
            .any(|n| matches!(n, Node::This {} | Node::Return { .. })))
        || (!strict
            && has_return()
            && ast
                .nodes
                .iter()
                .any(|n| matches!(n, Node::Await { .. } | Node::ForOf { is_await: true, .. })))
    {
        let (this, returns, tla) = top_level_this_and_return(&ast);
        if strict {
            for id in this {
                let at = ast.span(id).start;
                let undefined = build::undefined(&mut ast, at);
                build::replace(&mut ast, id, undefined);
            }
        }
        if let Some(&r) = returns.first().filter(|_| strict || tla) {
            let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
            messages.push(lines.message(
                Severity::Error,
                display,
                ast.span(r).start,
                "Top-level return cannot be used inside an ECMAScript module".into(),
            ));
        }
    }
    // Top-level `this` of a CommonJS file is its `exports` object: when bundling it becomes
    // `exports`, so the file is wrapped as CommonJS (as esbuild does). Not for `.mjs`/`.mts`,
    // files with a top-level `await` (modules), or a file that declares its own `exports`.
    if !strict
        && ctx.opts.bundle
        && !path.ends_with(".mjs")
        && !path.ends_with(".mts")
        && ast.nodes.iter().any(|n| matches!(n, Node::This {}))
    {
        let (this, _, tla) = top_level_this_and_return(&ast);
        let own_exports = || {
            let scopes = Scopes::analyze(&ast, strict);
            scopes
                .module_symbols()
                .iter()
                .any(|&s| ast.atom(scopes.symbol(s).name) == "exports")
        };
        if !this.is_empty() && !tla && (!src.contains("exports") || !own_exports()) {
            for id in this {
                let at = ast.span(id).start;
                let exports = build::ident(&mut ast, "exports", at);
                build::replace(&mut ast, id, exports);
            }
        }
    }
    let mut scopes = Scopes::analyze(&ast, strict);
    let mut dirty = false;
    if !ctx.defines.is_empty()
        && ctx.defines.roots.iter().any(|r| src.contains(r.as_str()))
        && apply_defines(&mut ast, &scopes, &ctx.defines)
    {
        scopes = Scopes::analyze(&ast, strict);
        dirty = true;
    }
    // Folding removes branches that defines made constant (or that are written constant,
    // like `if (false)`); skip the pass when there is nothing for it to do.
    if (dirty || ctx.opts.minify_syntax || has_constant_test(&ast))
        && ts_r_minify::fold::fold(&mut ast, &src, &scopes)
    {
        dirty = true;
    }
    if dirty {
        scopes = Scopes::analyze(&ast, strict);
    }
    if ctx.opts.bundle || ctx.opts.minify_syntax {
        constant_assignments(&ast, &scopes, ctx.opts.bundle, |at, text| {
            let lines = lines.get_or_insert_with(|| crate::LineIndex::new(&src));
            let m = lines.message(Severity::Error, display, at, text);
            if !messages.contains(&m) {
                messages.push(m);
            }
        });
    }
    let default_sym = anonymous_default(&ast).map(|_| {
        let name = format!("{}_default", file_stem_name(path));
        let atom = ast.intern(&name);
        scopes.add_synthetic(ScopeId::MODULE, atom)
    });
    Prepared {
        ast,
        scopes,
        src,
        erased,
        prelude,
        default_sym,
        messages,
        strict,
    }
}

/// Reports writes to `const` and `using` bindings and (when `imports`) to imports, as
/// esbuild does when bundling or minifying: those become `var`s or plain references in the
/// output, where the write would no longer throw.
fn constant_assignments(
    ast: &Ast,
    scopes: &Scopes,
    imports: bool,
    mut report: impl FnMut(u32, String),
) {
    let constant = |kind| match kind {
        SymbolKind::Const | SymbolKind::Using => true,
        SymbolKind::Import => imports,
        _ => false,
    };
    if !scopes
        .symbols
        .iter()
        .any(|s| s.writes > 0 && constant(s.kind))
    {
        return;
    }
    for r in scopes.refs.iter().filter(|r| r.write) {
        let Some(sym) = r.symbol else { continue };
        let sym = scopes.symbol(sym);
        if !constant(sym.kind) {
            continue;
        }
        let name = ast.atom(sym.name);
        let text = if sym.kind == SymbolKind::Import {
            format!("Cannot assign to import {name:?}")
        } else {
            format!("Cannot assign to {name:?} because it is a constant")
        };
        report(ast.span(r.node).start, text);
    }
}

/// `this` expressions and `return` statements outside functions (and, for `this`, classes).
/// Also says whether the file has a top-level `await` (or `for await`).
fn top_level_this_and_return(ast: &Ast) -> (Vec<NodeId>, Vec<NodeId>, bool) {
    let (mut this, mut returns, mut tla) = (Vec::new(), Vec::new(), false);
    // (node, inside a class, inside an arrow function)
    let mut stack = vec![(ast.root, false, false)];
    while let Some((id, in_class, in_arrow)) = stack.pop() {
        match ast.node(id) {
            Node::This {} if !in_class => this.push(id),
            Node::Return { .. } if !in_arrow => returns.push(id),
            Node::Await { .. } | Node::ForOf { is_await: true, .. } if !in_arrow => tla = true,
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => continue,
            _ => {}
        }
        let in_class = in_class
            || matches!(
                ast.node(id),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            );
        let in_arrow = in_arrow || matches!(ast.node(id), Node::Arrow { .. });
        ast.for_each_child(id, |c| stack.push((c, in_class, in_arrow)));
    }
    returns.sort_by_key(|&r| ast.span(r).start);
    (this, returns, tla)
}

/// Whether any `if`, `?:` or logical operator tests a literal (`if (false)`, `!0 && x`).
fn has_constant_test(ast: &Ast) -> bool {
    let literal = |id: NodeId| {
        let mut id = id;
        loop {
            match ast.node(id) {
                Node::BoolLit { .. } | Node::NumberLit { .. } | Node::NullLit {} => return true,
                Node::Unary { arg, .. } | Node::Paren { expr: arg } => id = *arg,
                _ => return false,
            }
        }
    };
    ast.nodes.iter().any(|n| match *n {
        Node::If { test, .. } | Node::Conditional { test, .. } => literal(test),
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            ..
        } => literal(left),
        // `try {} catch { ... }`: the handler is dead code.
        Node::Try {
            block,
            handler: Some(_),
            ..
        } => matches!(*ast.node(block), Node::Block { stmts } if stmts.is_empty()),
        _ => false,
    })
}

/// The top-level `export default` statement that needs a synthesized name.
pub fn anonymous_default(ast: &Ast) -> Option<NodeId> {
    build::program_stmts(ast)
        .into_iter()
        .find(|&s| match *ast.node(s) {
            Node::ExportDefaultExpr { .. } => true,
            Node::FunctionDecl {
                modifiers, name, ..
            }
            | Node::ClassDecl {
                modifiers, name, ..
            } => modifiers.contains(Modifiers::DEFAULT) && name.is_none(),
            _ => false,
        })
}

/// A identifier-safe name from a file path (`src/my-file.ts` -> `my_file`; `index` files use
/// the directory name).
pub fn file_stem_name(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or(path);
    let mut stem = file.split('.').next().unwrap_or(file);
    if stem == "index"
        && let Some(dir) = parts.next()
    {
        stem = dir;
    }
    names::sanitize(stem)
}

/// The first BigInt literal (`123n`) outside strings in `text`. The JSONC parser reads it as
/// a number, but JSON has no BigInt and esbuild rejects it.
/// JSON text used as a JavaScript expression, with each `"__proto__"` key made computed
/// (`["__proto__"]`): in an object literal the plain key would set the prototype instead
/// of creating a property, which is what `JSON.parse` does.
fn computed_proto_keys(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("__proto__") && !text.contains("\\u") {
        return std::borrow::Cow::Borrowed(text);
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 4);
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        let end = (i + 1).min(b.len());
        i = end;
        let mut j = end;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let raw = text.get(start + 1..end.saturating_sub(1)).unwrap_or("");
        if j < b.len() && b[j] == b':' && json_string_value(raw) == "__proto__" {
            out.push_str(&text[last..start]);
            out.push('[');
            out.push_str(&text[start..end]);
            out.push(']');
            last = end;
        }
    }
    if last == 0 {
        return std::borrow::Cow::Borrowed(text);
    }
    out.push_str(&text[last..]);
    std::borrow::Cow::Owned(out)
}

/// The value of a JSON string's raw contents (only `\uXXXX` and one-character escapes).
fn json_string_value(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(e) => out.push(e),
            None => {}
        }
    }
    out
}

fn json_bigint(text: &str) -> Option<&str> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < b.len() {
        let c = b[i];
        if in_string {
            match c {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else if c == b'"' {
            in_string = true;
        } else if c.is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            if b[i - 1] == b'n' {
                return text.get(start..i);
            }
            continue;
        }
        i += 1;
    }
    None
}

/// Whether a TypeScript file exports a const enum (`export const enum`, or a const enum named
/// by a top-level `export { .. }`).
fn exports_const_enum(ast: &Ast) -> bool {
    let stmts = build::program_stmts(ast);
    let mut consts: Vec<&str> = Vec::new();
    for &s in stmts.iter() {
        if let Node::Enum {
            modifiers, name, ..
        } = *ast.node(s)
            && modifiers.contains(Modifiers::CONST)
            && !modifiers.contains(Modifiers::DECLARE)
        {
            if modifiers.contains(Modifiers::EXPORT) {
                return true;
            }
            if let Node::Ident { name } = *ast.node(name) {
                consts.push(ast.atom(name));
            }
        }
    }
    !consts.is_empty()
        && stmts.iter().any(|&s| {
            let Node::ExportNamed {
                type_only: false,
                specifiers,
                source: None,
                ..
            } = *ast.node(s)
            else {
                return false;
            };
            ast.list(specifiers).iter().any(|&sp| {
                matches!(*ast.node(sp), Node::ExportSpecifier { type_only: false, local, .. }
                    if matches!(*ast.node(local), Node::Ident { name } if consts.contains(&ast.atom(name))))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_proto_keys_become_computed() {
        let t = r#"{"a": "__proto__", "__proto__": {"x": 1}, "\u005f_proto__" : 2, "b\"": 3}"#;
        assert_eq!(
            computed_proto_keys(t),
            r#"{"a": "__proto__", ["__proto__"]: {"x": 1}, ["\u005f_proto__"] : 2, "b\"": 3}"#
        );
        assert_eq!(computed_proto_keys(r#"{"a": 1}"#), r#"{"a": 1}"#);
        // Unterminated input does not panic.
        let _ = computed_proto_keys("\"__proto__\\");
    }

    #[test]
    fn file_loader_hash_covers_the_generated_url() {
        let mut fs = ts_r_config::MemoryFs::new();
        fs.add_file("/p/a.png", "png");
        let load_with = |asset_names: &str, public_path: &str| {
            let opts = BuildOptions {
                asset_names: asset_names.into(),
                public_path: public_path.into(),
                ..BuildOptions::default()
            };
            let s = load(&fs, "/p/a.png", Loader::File, &opts).unwrap();
            (s.hash, s.js.to_string())
        };
        let old = load_with("old/[name]", "");
        assert!(old.1.contains("__TSR_ASSET_"), "{}", old.1);
        assert_eq!(load_with("old/[name]", ""), old);
        assert_ne!(load_with("new/[name]", "").0, old.0);
        assert_ne!(load_with("old/[name]", "https://cdn").0, old.0);
    }

    #[test]
    fn data_urls() {
        assert_eq!(data_url("a.txt", b"x"), "data:text/plain;charset=utf-8,x");
        assert_eq!(
            data_url("a.txt", b"a #%41 \n"),
            "data:text/plain;charset=utf-8,a %23%2541 %0A"
        );
        assert_eq!(
            data_url("a.bin", b"\x00b"),
            "data:application/octet-stream,\x00b"
        );
        assert_eq!(
            data_url("a.png", &[0xff, 0x00]),
            "data:image/png;base64,/wA="
        );
        assert_eq!(
            data_url("a.txt", b"\n\n\n\n\n"),
            "data:text/plain;charset=utf-8;base64,CgoKCgo="
        );
    }
}
