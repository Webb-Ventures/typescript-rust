//! Building the set of source files a check runs over: lib files chosen by `target`/`lib`
//! (with their `/// <reference lib>` closure), root files, and everything reachable through
//! imports, `/// <reference>` directives and `types` entries. Files are parsed and bound in
//! parallel (bounded threads).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, CommentKind, ParentMap, SourceKind};
use ts_r_binder::{BindOptions, BoundFile, RequestKind};
use ts_r_config::{
    CompilerOptions, FileStamp, FileSystem, JsxEmit, ModuleDetection, ModuleKind, StrictFlag,
};
use ts_r_diagnostics::Diagnostic;
use ts_r_resolver::resolver::{Extension, ImportKind, ModuleFormat, ResolutionMode};
use ts_r_resolver::{Resolver, ResolverOptions};
use ts_r_span::LineIndex;

/// One parsed and bound file.
#[derive(Debug)]
pub struct SourceFile {
    /// Absolute, normalised path (lib files: `/lib/<name>` style virtual path).
    pub path: String,
    pub text: String,
    pub ast: Ast,
    pub bound: BoundFile,
    pub kind: SourceKind,
    /// A default library file (`lib.*.d.ts`).
    pub is_lib: bool,
    /// Static imports of this file resolve in ECMAScript-module mode
    /// ([`BindOptions::esm_format`]).
    pub esm_format: bool,
    /// The file's own format is ECMAScript module ([`BindOptions::package_esm`]).
    pub package_esm: bool,
    /// [`BindOptions::package_scope`].
    pub package_scope: Option<(u16, bool)>,
    /// Found through `node_modules`.
    pub is_external: bool,
    /// Syntax errors (with the parser's checker-grammar errors, as tsc reports them).
    pub syntax: Vec<Diagnostic>,
    /// The checker grammar errors in `syntax` ([`ts_r_parser::ParseResult::grammar`]).
    pub grammar: Vec<Diagnostic>,
    /// Syntax errors in JSDoc types (JavaScript files): reported for checked files only,
    /// and they do not stop semantic checking.
    pub jsdoc: Vec<Diagnostic>,
    /// `/// <reference ...>` directives: `(kind, value)` with kind `path`, `types`, `lib`
    /// (`types-import`/`types-require` with a `resolution-mode` attribute; plain `types`
    /// resolves in the file's mode).
    pub references: Vec<(&'static str, String)>,
    parents: OnceLock<ParentMap>,
    lines: OnceLock<LineIndex>,
    commonjs: OnceLock<bool>,
}

impl SourceFile {
    /// Parses and binds `text`.
    pub fn new(path: String, text: String, is_lib: bool, opts: &BindOptions) -> SourceFile {
        let kind = SourceKind::from_path(&path);
        // JavaScript files parse as tsc parses them: TypeScript syntax is accepted and gets
        // TS8xxx errors ("Type annotations can only be used in TypeScript files."), which
        // tsc 7 reports for every `.js` file of an `allowJs` program, `checkJs` or not.
        // JSON files (`resolveJsonModule`) hold one JSON value.
        let mut parsed = if is_json_path(&path) {
            ts_r_parser::parse_json(&text)
        } else {
            ts_r_parser::parse_with(
                &text,
                kind,
                ts_r_parser::ParseOptions {
                    ts_in_js: ts_r_parser::TsInJs::Report,
                },
            )
        };
        // JSDoc types become annotations (`ts_r_parser::attach_jsdoc`) before binding, so
        // `@template` parameters and `@typedef` aliases are scoped like TypeScript's.
        let jsdoc = if kind.is_typescript() || is_json_path(&path) {
            Vec::new()
        } else {
            let (jsdoc, syntax) = ts_r_parser::attach_jsdoc_with_syntax(&mut parsed.ast, &text);
            if !syntax.is_empty() {
                // Kept sorted by position (`ts_r_parser::without_grammar`).
                parsed.diagnostics.extend(syntax);
                parsed.diagnostics.sort_by_key(|d| d.span.start);
            }
            jsdoc
        };
        let mut o = *opts;
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".mts")
            || lower.ends_with(".cts")
            || lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
        {
            o.force_module = true;
        }
        let bound = ts_r_binder::bind(&parsed.ast, &text, &o);
        let references = triple_slash(&parsed.ast, &text);
        SourceFile {
            path,
            kind,
            is_lib,
            esm_format: o.esm_format,
            package_esm: o.package_esm,
            package_scope: o.package_scope,
            is_external: false,
            syntax: parsed.diagnostics,
            grammar: parsed.grammar,
            jsdoc,
            references,
            ast: parsed.ast,
            bound,
            text,
            parents: OnceLock::new(),
            lines: OnceLock::new(),
            commonjs: OnceLock::new(),
        }
    }

    pub fn parents(&self) -> &ParentMap {
        self.parents.get_or_init(|| ParentMap::build(&self.ast))
    }

    pub fn lines(&self) -> &LineIndex {
        self.lines.get_or_init(|| LineIndex::new(&self.text))
    }

    pub fn is_declaration(&self) -> bool {
        self.kind.is_declaration()
    }

    pub fn is_js(&self) -> bool {
        !self.kind.is_typescript()
    }

    /// A JavaScript file that is a CommonJS module: no ECMAScript module syntax (top-level
    /// `import`/`export`, `import.meta`) and a CommonJS indicator anywhere in it (observed
    /// in tsc 7): a `require(x)` call with one argument, an assignment to `module.exports`,
    /// `module.exports.x`, `exports.x` or `exports["x"]`, or
    /// `Object.defineProperty(exports, ...)`. In such a file `module` and `exports` are
    /// defined names.
    pub fn is_commonjs_module(&self) -> bool {
        *self
            .commonjs
            .get_or_init(|| self.is_js() && !self.is_json() && commonjs_module(&self.ast))
    }

    /// A `.json` file (included or imported under `resolveJsonModule`): syntax errors only,
    /// never type-checked (tsc).
    pub fn is_json(&self) -> bool {
        is_json_path(&self.path)
    }

    /// A `// @ts-nocheck` pragma among the comments before the first token: the file gets
    /// no semantic diagnostics (tsc, TypeScript and JavaScript files alike).
    pub fn has_ts_nocheck(&self) -> bool {
        has_ts_nocheck(&self.text)
    }

    /// A `// @ts-check` pragma among the comments before the first token (and no
    /// `@ts-nocheck`): a JavaScript file is checked without `checkJs`.
    pub fn has_ts_check(&self) -> bool {
        ts_check_pragma(&self.text) == Some(true)
    }
}

fn commonjs_module(ast: &Ast) -> bool {
    use ts_r_ast::{AssignOp, MetaPropKind, Modifiers, Node, NodeId};
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return false;
    };
    let esm = ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::ImportDecl { .. }
        | Node::ExportNamed { .. }
        | Node::ExportAll { .. }
        | Node::ExportDefaultExpr { .. }
        | Node::ExportAssignment { .. } => true,
        ref n => n.modifiers().contains(Modifiers::EXPORT),
    });
    if esm {
        return false;
    }
    let ident =
        |n: NodeId, t: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == t);
    let literal_key = |n: NodeId| {
        matches!(
            *ast.node(n),
            Node::StringLit { .. } | Node::NumberLit { .. }
        )
    };
    // `module.exports` or `module["exports"]`
    let module_exports = |n: NodeId| match *ast.node(n) {
        Node::Member { obj, prop, .. } => ident(obj, "module") && ident(prop, "exports"),
        Node::Index { obj, index, .. } => {
            ident(obj, "module")
                && matches!(*ast.node(index), Node::StringLit { value } if ast.atom(value) == "exports")
        }
        _ => false,
    };
    // `exports` or `module.exports`
    let exports_object = |n: NodeId| ident(n, "exports") || module_exports(n);
    let mut indicator = false;
    for n in (1..=ast.node_count() as u32).filter_map(NodeId::from_raw) {
        match *ast.node(n) {
            Node::MetaProperty {
                kind: MetaPropKind::ImportMeta,
            } => return false,
            _ if indicator => {}
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } => {
                // `exports.x`, `exports["x"]` (not `exports.#x` or `exports[k]`).
                indicator = module_exports(left)
                    || match *ast.node(left) {
                        Node::Member { obj, prop, .. } => {
                            matches!(*ast.node(prop), Node::Ident { .. }) && exports_object(obj)
                        }
                        Node::Index { obj, index, .. } => literal_key(index) && exports_object(obj),
                        _ => false,
                    };
            }
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                indicator = (ident(callee, "require") && args.len() == 1)
                    || (matches!(*ast.node(callee), Node::Member { obj, prop, .. }
                            if ident(obj, "Object") && ident(prop, "defineProperty"))
                        && args.first().is_some_and(|&a| exports_object(a)));
            }
            _ => {}
        }
    }
    indicator
}

/// Whether a `//` comment before the first token is `// @ts-nocheck` (`///` too, the name
/// case-insensitive and ending at whitespace).
pub fn has_ts_nocheck(text: &str) -> bool {
    ts_check_pragma(text) == Some(false)
}

/// `// @ts-nocheck` (`false`, it wins) or `// @ts-check` (`true`) among the `//`
/// comments before the first token, if any.
pub fn ts_check_pragma(text: &str) -> Option<bool> {
    let mut found = None;
    let mut s = text.strip_prefix('\u{feff}').unwrap_or(text);
    if s.starts_with("#!") {
        s = s.find('\n').map_or("", |i| &s[i..]);
    }
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("//") {
            let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
            let line = &rest[..end];
            let body = line.strip_prefix('/').unwrap_or(line).trim_start();
            if let Some(p) = body.strip_prefix('@') {
                let name = p.split(char::is_whitespace).next().unwrap_or("");
                if name.eq_ignore_ascii_case("ts-nocheck") {
                    return Some(false);
                } else if name.eq_ignore_ascii_case("ts-check") {
                    found = Some(true);
                }
            }
            s = &rest[end..];
        } else if let Some(rest) = s.strip_prefix("/*") {
            match rest.find("*/") {
                Some(i) => s = &rest[i + 2..],
                None => return found,
            }
        } else {
            return found;
        }
    }
}

fn is_json_path(path: &str) -> bool {
    path.len() >= 5 && path.as_bytes()[path.len() - 5..].eq_ignore_ascii_case(b".json")
}

/// `/// <reference path|types|lib="..." />` directives in the leading comments.
fn triple_slash(ast: &Ast, text: &str) -> Vec<(&'static str, String)> {
    let first_stmt = match ast.node(ast.root) {
        ts_r_ast::Node::Program { stmts } => ast.list(*stmts).first().map(|&s| ast.span(s).start),
        _ => None,
    }
    .unwrap_or(u32::MAX);
    let mut out = Vec::new();
    for c in &ast.comments {
        if c.span.start >= first_stmt {
            break;
        }
        if c.kind != CommentKind::Line {
            continue;
        }
        let t = c.span.text(text);
        for key in ["path", "types", "lib"] {
            if let Some(v) = reference_attribute(t, key) {
                // `resolution-mode` overrides the file's mode for a `types` reference.
                let kind = match (key, reference_attribute(t, "resolution-mode").as_deref()) {
                    ("types", Some("import")) => "types-import",
                    ("types", Some("require")) => "types-require",
                    _ => key,
                };
                out.push((kind, v));
                break;
            }
        }
    }
    out
}

pub(crate) fn reference_attribute(text: &str, key: &str) -> Option<String> {
    let s = text
        .strip_prefix("///")?
        .trim_start()
        .strip_prefix("<reference")?;
    let mut i = 0;
    while let Some(p) = s[i..].find(key) {
        let at = i + p;
        let before_ok = at == 0 || s.as_bytes()[at - 1].is_ascii_whitespace();
        let after = s[at + key.len()..].trim_start();
        if before_ok && let Some(after) = after.strip_prefix('=') {
            let after = after.trim_start();
            let q = after.chars().next()?;
            if q == '"' || q == '\'' {
                let body = &after[1..];
                let end = body.find(q)?;
                return Some(body[..end].to_owned());
            }
        }
        i = at + key.len();
    }
    None
}

/// Where lib files come from. Parsed lib files are cached and shared between programs
/// (they are immutable once bound).
#[derive(Debug)]
pub struct LibStore {
    dir: PathBuf,
    cache: Mutex<FxHashMap<(String, BindOptions), Arc<SourceFile>>>,
}

impl LibStore {
    pub fn new(dir: impl Into<PathBuf>) -> LibStore {
        LibStore {
            dir: dir.into(),
            cache: Mutex::new(FxHashMap::default()),
        }
    }

    /// Finds the lib directory: `$TS_R_LIB_DIR`, `<exe dir>/lib`, `<exe dir>/../lib`, then the
    /// synced upstream copy in this repository's `target/upstream`.
    pub fn locate() -> Option<LibStore> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(d) = std::env::var_os("TS_R_LIB_DIR") {
            candidates.push(PathBuf::from(d));
        }
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            candidates.push(dir.join("lib"));
            candidates.push(dir.join("../lib"));
        }
        candidates.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/upstream/typescript/tsc/internal/bundled/libs"),
        );
        candidates
            .into_iter()
            .find(|d| d.join("lib.es5.d.ts").is_file())
            .map(LibStore::new)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Parses (once) and returns lib file `name` (`lib.es2020.d.ts`).
    pub fn get(&self, name: &str, opts: &BindOptions) -> Option<Arc<SourceFile>> {
        let key = (name.to_owned(), *opts);
        if let Some(f) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Some(f.clone());
        }
        let text = std::fs::read_to_string(self.dir.join(name)).ok()?;
        let path = format!("{}/{}", self.dir.to_string_lossy().replace('\\', "/"), name);
        let f = Arc::new(SourceFile::new(path, text, true, opts));
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        Some(c.entry(key).or_insert(f).clone())
    }
}

/// Parsed and bound source files shared between program loads, for a tool that loads many
/// overlapping programs in one process (`parse<T>` expansion loads each expanded file's
/// import closure). A file is parsed at most once per contents and binding: an entry is
/// reused when the bind options and `is_external` match and either
///
/// - the file system's [`FileStamp`] (length, modification time) equals the one recorded
///   when the entry's text was last read, and that modification time is more than
///   [`SourceCache::RACY_NS`] older than the read (no read at all), or
/// - the text read now is byte-equal to the cached text.
///
/// Reading a changed text evicts every entry of that path, so the cache holds at most one
/// contents version per path; past [`SourceCache::MAX_PATHS`] paths it starts over (bounded
/// in long watch sessions).
#[derive(Debug, Default)]
pub struct SourceCache {
    files: Mutex<FxHashMap<String, Vec<CachedSource>>>,
    parses: AtomicUsize,
}

#[derive(Debug)]
struct CachedSource {
    opts: BindOptions,
    file: Arc<SourceFile>,
    /// The stamp seen when `file.text` was last read (and found current), and when.
    stamp: Option<FileStamp>,
    read_at_ns: u128,
}

impl CachedSource {
    fn trusted(&self, stamp: FileStamp) -> bool {
        self.stamp == Some(stamp)
            && stamp.modified_ns.saturating_add(SourceCache::RACY_NS) < self.read_at_ns
    }
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

impl SourceCache {
    /// Paths kept before the cache is cleared.
    pub const MAX_PATHS: usize = 65_536;
    /// A stamp is only trusted for a file modified at least this long before it was read
    /// (2 s: file systems with coarse modification times, as git's "racy" entries).
    pub const RACY_NS: u128 = 2_000_000_000;

    pub fn new() -> SourceCache {
        SourceCache::default()
    }

    /// Files parsed through this cache so far (misses).
    pub fn parses(&self) -> usize {
        self.parses.load(Ordering::Relaxed)
    }

    /// Paths with a cached file.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops every entry.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Drops the entries of `path` (changed or deleted).
    pub fn forget(&self, path: &str) {
        self.lock().remove(path);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<String, Vec<CachedSource>>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cached file for `path` when `stamp` says it is unchanged (no read needed).
    fn by_stamp(
        &self,
        path: &str,
        stamp: FileStamp,
        opts: &BindOptions,
        external: bool,
    ) -> Option<Arc<SourceFile>> {
        let files = self.lock();
        let e = files
            .get(path)?
            .iter()
            .find(|e| e.opts == *opts && e.file.is_external == external)?;
        e.trusted(stamp).then(|| e.file.clone())
    }

    /// The parsed file for `path` with contents `text`, read at `read_at_ns` when the file
    /// had `stamp` (parsed now unless cached).
    fn get_or_parse(
        &self,
        path: &str,
        text: String,
        stamp: Option<FileStamp>,
        read_at_ns: u128,
        opts: &BindOptions,
        external: bool,
    ) -> Arc<SourceFile> {
        if let Some(entries) = self.lock().get_mut(path)
            && let Some(e) = entries
                .iter_mut()
                .find(|e| e.opts == *opts && e.file.is_external == external && e.file.text == text)
        {
            e.stamp = stamp;
            e.read_at_ns = read_at_ns;
            return e.file.clone();
        }
        // Parsed outside the lock: a wave parses in parallel.
        let mut f = SourceFile::new(path.to_owned(), text, false, opts);
        f.is_external = external;
        let f = Arc::new(f);
        self.parses.fetch_add(1, Ordering::Relaxed);
        let mut files = self.lock();
        if files.len() >= Self::MAX_PATHS && !files.contains_key(path) {
            files.clear();
        }
        let entries = files.entry(path.to_owned()).or_default();
        // Another contents version of this path is stale now.
        entries.retain(|e| e.file.text == f.text);
        if let Some(e) = entries
            .iter()
            .find(|e| e.opts == *opts && e.file.is_external == external)
        {
            // Parsed concurrently by another load: keep the first.
            return e.file.clone();
        }
        entries.push(CachedSource {
            opts: *opts,
            file: f.clone(),
            stamp,
            read_at_ns,
        });
        f
    }
}

/// A module specifier resolution recorded while loading.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub resolved: String,
    pub extension: Extension,
    pub external: bool,
    /// The resolved file is part of the program.
    pub included: bool,
    /// The resolved file is inside a package with a `name` and `version` (tsc's package
    /// id): an untyped import of it gets the `@types` hint.
    pub package: bool,
    /// `resolved` is the declaration output of this source of a referenced project
    /// (`references`; it may not exist yet: TS6305).
    pub reference_source: Option<String>,
}

/// The loaded program: files in check order (lib files first) and how every module
/// specifier resolved.
/// Resolutions of requests in another mode than the first request of their specifier in
/// their file ([`ProgramFiles::mode_resolutions`]).
pub type ModeResolutions = FxHashMap<(String, String, ResolutionMode), Option<Resolution>>;

#[derive(Debug)]
pub struct ProgramFiles {
    pub files: Vec<Arc<SourceFile>>,
    /// `(containing file path, specifier)` → resolution (`None`: not found).
    pub resolutions: FxHashMap<(String, String), Option<Resolution>>,
    /// Requests resolved in another mode than the first request of their specifier in their
    /// file (`import x = require("./a")` after `import "./a"` in an ES module, or a
    /// `resolution-mode` attribute): `(containing file path, specifier, mode)` → resolution.
    /// Look up with [`request_mode`] before falling back to `resolutions`.
    pub mode_resolutions: ModeResolutions,
    pub options: CompilerOptions,
    /// Root file paths as given.
    pub roots: Vec<String>,
    /// Lib names that could not be found.
    pub missing_libs: Vec<String>,
    /// `types` entries that could not be resolved.
    pub missing_types: Vec<String>,
    /// `exports`/`imports` entries of the program's own package left unresolved for want
    /// of a project root (TS2209/TS2210, global diagnostics).
    pub ambiguous_roots: Vec<ts_r_resolver::AmbiguousRoot>,
    /// Every loaded file's dependencies: resolved imports that are part of the program and
    /// `/// <reference path|types>` targets (paths, in source order).
    pub deps: FxHashMap<String, Vec<String>>,
}

impl ProgramFiles {
    pub fn file_index(&self, path: &str) -> Option<usize> {
        self.files.iter().position(|f| f.path == path)
    }
}

/// Binding options derived from compiler options for one file.
pub fn bind_options_for<F: FileSystem>(
    path: &str,
    options: &CompilerOptions,
    resolver: Option<&Resolver<F>>,
) -> BindOptions {
    let e = options.effective();
    let lower = path.to_ascii_lowercase();
    // `moduleDetection: force` applies to implementation files only: a global `.d.ts`
    // (`declare module "node:fs"` in @types/node) stays a script (observed in tsc 7).
    let is_declaration = is_declaration_file_name(&lower);
    // A JSON file is always a module (its value).
    let mut force = (matches!(e.module_detection(), ModuleDetection::Force) && !is_declaration)
        || is_json_path(&lower);
    let node = matches!(
        e.module(),
        ModuleKind::Node16 | ModuleKind::Node18 | ModuleKind::Node20 | ModuleKind::NodeNext
    );
    let package_esm = match resolver {
        // A referenced project's file has that project's format (E3).
        Some(r) => r.file_format(path).0 == ModuleFormat::Esm || r.is_reference_esm(path),
        None => lower.ends_with(".mts") || lower.ends_with(".mjs"),
    };
    let esm_format = if node {
        package_esm
    } else {
        !matches!(
            e.module(),
            ModuleKind::CommonJS | ModuleKind::AMD | ModuleKind::UMD | ModuleKind::System
        )
    };
    if !force && matches!(e.module_detection(), ModuleDetection::Auto) {
        if node && esm_format && resolver.is_some() {
            force = true;
        }
        if (lower.ends_with(".tsx") || lower.ends_with(".jsx"))
            && matches!(e.jsx(), Some(JsxEmit::ReactJsx | JsxEmit::ReactJsxDev))
        {
            force = true;
        }
    }
    BindOptions {
        force_module: force,
        always_strict: Some(e.strict_flag(StrictFlag::AlwaysStrict)),
        allow_unreachable_code: options.allow_unreachable_code(),
        allow_unused_labels: options.allow_unused_labels(),
        preserve_const_enums: e.preserve_const_enums() || e.isolated_modules(),
        esm_format,
        package_esm,
        package_scope: match resolver {
            Some(r) if matches!(e.module(), ModuleKind::Node16 | ModuleKind::Node18) => {
                package_scope_of(r, path)
            }
            _ => None,
        },
    }
}

/// [`BindOptions::package_scope`]: the `package.json` deciding `path`'s format, as directory
/// levels above the file's directory and whether it has a `type` field.
fn package_scope_of<F: FileSystem>(r: &Resolver<F>, path: &str) -> Option<(u16, bool)> {
    let (_, dir) = r.file_format(path);
    let dir = dir?;
    let mut d = ts_r_config::path::directory_of(path);
    let mut up = 0u16;
    while d != dir {
        let parent = ts_r_config::path::directory_of(d);
        if parent == d || parent.is_empty() || up == u16::MAX {
            return None;
        }
        d = parent;
        up += 1;
    }
    let has_type = r
        .package_json(&dir)
        .is_some_and(|pj| pj.type_field.is_some());
    Some((up, has_type))
}

/// `x.d.ts`, `x.d.mts`, `x.d.cts` and arbitrary-extension declarations (`x.d.css.ts`).
fn is_declaration_file_name(lower: &str) -> bool {
    lower.ends_with(".d.ts")
        || lower.ends_with(".d.mts")
        || lower.ends_with(".d.cts")
        || (lower.ends_with(".ts") && lower.rsplit('/').next().is_some_and(|n| n.contains(".d.")))
}

/// The mode request `req` of a file at `path` resolves in (as `Resolver::mode_for`, from the
/// file's format `package_esm` under `node16`..`nodenext`), or its `resolution-mode`.
pub fn request_mode(
    module: ModuleKind,
    path: &str,
    package_esm: bool,
    req: &ts_r_binder::ModuleRequest,
) -> ResolutionMode {
    if let Some(m) = req.resolution_mode {
        return m;
    }
    match req.kind {
        RequestKind::ImportEquals => return ResolutionMode::Require,
        RequestKind::DynamicImport if module != ModuleKind::CommonJS => {
            return ResolutionMode::Import;
        }
        _ => {}
    }
    if module.is_node() {
        return if package_esm {
            ResolutionMode::Import
        } else {
            ResolutionMode::Require
        };
    }
    let lower = path.to_ascii_lowercase();
    if module == ModuleKind::CommonJS && !lower.ends_with(".mts") && !lower.ends_with(".mjs") {
        return ResolutionMode::Require;
    }
    if lower.ends_with(".cts") || lower.ends_with(".cjs") {
        ResolutionMode::Require
    } else {
        ResolutionMode::Import
    }
}

fn import_kind(k: RequestKind) -> ImportKind {
    match k {
        RequestKind::DynamicImport => ImportKind::Dynamic,
        RequestKind::ImportEquals => ImportKind::ImportEquals,
        _ => ImportKind::Static,
    }
}

fn includable(ext: Extension, allow_js: bool) -> bool {
    match ext {
        Extension::Ts
        | Extension::Tsx
        | Extension::Dts
        | Extension::Mts
        | Extension::Cts
        | Extension::Dmts
        | Extension::Dcts => true,
        Extension::Js | Extension::Jsx | Extension::Mjs | Extension::Cjs => allow_js,
        // Only resolved under `resolveJsonModule`.
        Extension::Json => true,
    }
}

/// Lib file names for the options, with their `/// <reference lib>` closure, in dependency
/// order (referenced libs first).
fn lib_closure(
    libs: &LibStore,
    options: &CompilerOptions,
    opts: &BindOptions,
    referenced: &[String],
) -> (Vec<Arc<SourceFile>>, Vec<String>) {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    let mut missing = Vec::new();
    fn visit(
        name: &str,
        libs: &LibStore,
        opts: &BindOptions,
        seen: &mut FxHashSet<String>,
        out: &mut Vec<Arc<SourceFile>>,
        missing: &mut Vec<String>,
        depth: u32,
    ) {
        if depth > 64 || !seen.insert(name.to_owned()) {
            return;
        }
        let Some(f) = libs.get(name, opts) else {
            missing.push(name.to_owned());
            return;
        };
        for (kind, v) in &f.references {
            if *kind == "lib"
                && let Some(file) = ts_r_config::lib_file_name(v)
            {
                visit(file, libs, opts, seen, out, missing, depth + 1);
            }
        }
        out.push(f);
    }
    for name in options.effective().lib_files() {
        visit(name, libs, opts, &mut seen, &mut out, &mut missing, 0);
    }
    // `/// <reference lib="..." />` in program files (for example `esnext.disposable` in
    // @types/node) adds that lib too; an unknown name adds nothing. `noLib` disables it.
    let mut ignored = Vec::new();
    let referenced = if options.no_lib().unwrap_or(false) {
        &[][..]
    } else {
        referenced
    };
    for v in referenced {
        if let Some(file) = ts_r_config::lib_file_name(v) {
            visit(file, libs, opts, &mut seen, &mut out, &mut ignored, 0);
        }
    }
    (out, missing)
}

struct Pending {
    path: String,
    external: bool,
    /// How many `node_modules` JavaScript imports led here (0 for other files).
    js_depth: u32,
}

impl Pending {
    fn enqueue(self, queue: &mut Vec<Self>, depths: &mut FxHashMap<String, u32>) {
        if depths.get(&self.path).is_none_or(|&d| self.js_depth < d) {
            depths.insert(self.path.clone(), self.js_depth);
            queue.push(self);
        }
    }
}

/// JavaScript under `node_modules` that `allowJs` loads only within `maxNodeModuleJsDepth`
/// ([`resolve_request_in`] leaves it out of the program).
fn is_node_modules_js(r: &Resolution, allow_js: bool) -> bool {
    allow_js
        && r.resolved.contains("/node_modules/")
        && includable(r.extension, true)
        && !includable(r.extension, false)
}

/// Admits a `node_modules` JavaScript resolution `res` requested from a file at `js_depth`
/// when within `max` (`maxNodeModuleJsDepth`); returns the depth of the resolved file.
fn admit_js(res: &mut Option<Resolution>, allow_js: bool, js_depth: u32, max: u32) -> u32 {
    match res {
        Some(r) if is_node_modules_js(r, allow_js) => {
            if js_depth < max {
                r.included = true;
            }
            js_depth + 1
        }
        _ => 0,
    }
}

/// Loads a program: libs, roots and everything they reach. `threads` bounds parallel
/// parsing.
pub fn load_program<F: FileSystem>(
    fs: &F,
    roots: &[String],
    options: &CompilerOptions,
    libs: Option<&LibStore>,
    threads: usize,
) -> ProgramFiles {
    load(fs, roots, options, libs, threads, None, None, None)
}

/// [`load_program`] for a project with `references`: modules (and root files) that are
/// sources of referenced projects resolve to those projects' declaration outputs
/// (`ts_r_resolver::references`).
pub fn load_program_with_references<F: FileSystem>(
    fs: &F,
    roots: &[String],
    options: &CompilerOptions,
    libs: Option<&LibStore>,
    threads: usize,
    references: Option<Arc<ts_r_resolver::ProjectReferences>>,
) -> ProgramFiles {
    load(fs, roots, options, libs, threads, None, references, None)
}

/// [`load_program_with_references`] that also returns every `package.json` path module
/// resolution looked for, found or not (sorted): the build inputs besides the program's
/// files that `ts_r_projects` records in the buildinfo (design record 0012, E3).
pub fn load_program_tracking_package_jsons<F: FileSystem>(
    fs: &F,
    roots: &[String],
    options: &CompilerOptions,
    libs: Option<&LibStore>,
    threads: usize,
    references: Option<Arc<ts_r_resolver::ProjectReferences>>,
) -> (ProgramFiles, Vec<String>) {
    let mut lookups = Vec::new();
    let program = load(
        fs,
        roots,
        options,
        libs,
        threads,
        None,
        references,
        Some(&mut lookups),
    );
    (program, lookups)
}

/// [`load_program_tracking_package_jsons`] taking parsed files from `cache` when given (and
/// adding the ones it parses): `ts-r-check --watch` keeps unchanged files across rebuilds
/// (design record 0012, E4).
pub fn load_program_cached_tracking<F: FileSystem>(
    fs: &F,
    roots: &[String],
    options: &CompilerOptions,
    libs: Option<&LibStore>,
    threads: usize,
    cache: Option<&SourceCache>,
    references: Option<Arc<ts_r_resolver::ProjectReferences>>,
) -> (ProgramFiles, Vec<String>) {
    let mut lookups = Vec::new();
    let program = load(
        fs,
        roots,
        options,
        libs,
        threads,
        cache,
        references,
        Some(&mut lookups),
    );
    (program, lookups)
}

/// [`load_program`] taking parsed files from `cache` (and adding the ones it parses).
pub fn load_program_cached<F: FileSystem>(
    fs: &F,
    roots: &[String],
    options: &CompilerOptions,
    libs: Option<&LibStore>,
    threads: usize,
    cache: &SourceCache,
) -> ProgramFiles {
    load(fs, roots, options, libs, threads, Some(cache), None, None)
}

#[allow(clippy::too_many_arguments)]
fn load<F: FileSystem>(
    fs: &F,
    roots: &[String],
    options: &CompilerOptions,
    libs: Option<&LibStore>,
    threads: usize,
    cache: Option<&SourceCache>,
    references: Option<Arc<ts_r_resolver::ProjectReferences>>,
    package_jsons: Option<&mut Vec<String>>,
) -> ProgramFiles {
    let mut resolver_options = ResolverOptions::from_compiler_options(options);
    // Root files that are sources of a referenced project are replaced by its outputs.
    let redirected_roots: Vec<String>;
    let roots = match references.as_deref() {
        Some(refs) if !refs.is_empty() => {
            redirected_roots = roots
                .iter()
                .map(|r| {
                    refs.redirect(r)
                        .map_or_else(|| r.clone(), |(d, _)| d.to_owned())
                })
                .collect();
            &redirected_roots[..]
        }
        _ => roots,
    };
    resolver_options.project_references = references;
    let resolver = Resolver::new(fs, resolver_options);
    let lib_bind = BindOptions {
        always_strict: Some(options.effective().strict_flag(StrictFlag::AlwaysStrict)),
        ..BindOptions::default()
    };
    let allow_js = options.effective().allow_js();
    let max_js_depth = options.effective().max_node_module_js_depth();
    let mut loaded: FxHashMap<String, Arc<SourceFile>> = FxHashMap::default();
    let mut order_edges: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut resolutions: FxHashMap<(String, String), Option<Resolution>> = FxHashMap::default();
    let mut mode_resolutions: ModeResolutions = FxHashMap::default();
    let mut queue: Vec<Pending> = Vec::new();
    let mut queued: FxHashMap<String, u32> = FxHashMap::default();
    let mut missing_types = Vec::new();
    for r in roots {
        Pending {
            path: r.clone(),
            external: false,
            js_depth: 0,
        }
        .enqueue(&mut queue, &mut queued);
    }
    // `types` entries.
    let mut type_roots: Vec<String> = Vec::new();
    let types = expand_wildcard_types(fs, &resolver, options.effective().types());
    for t in &types {
        match resolver.resolve_type_reference(t, None, ResolutionMode::Import) {
            Some(r) => {
                Pending {
                    path: r.resolved_file_name.clone(),
                    external: r.is_external_library_import,
                    js_depth: 0,
                }
                .enqueue(&mut queue, &mut queued);
                type_roots.push(r.resolved_file_name.clone());
            }
            // A directory found through `*` that holds no declarations adds nothing.
            None if !options.effective().types().contains(t) => {}
            None => missing_types.push(t.clone()),
        }
    }
    let threads = threads.clamp(1, 16);
    let mut guard = 0;
    while !queue.is_empty() && guard < 100_000 {
        guard += 1;
        let wave = std::mem::take(&mut queue);
        // The first wave also parses the default libs, next to the roots (the libs that
        // program files reference are added after the last wave).
        let warm_libs = || {
            if let Some(l) = libs {
                let _ = lib_closure(l, options, &lib_bind, &[]);
            }
        };
        let side: Option<&(dyn Fn() + Sync)> = if guard == 1 { Some(&warm_libs) } else { None };
        let parsed = parse_wave(fs, &wave, options, &resolver, threads, cache, side, &loaded);
        for (p, f) in wave.into_iter().zip(parsed) {
            if queued.get(&p.path).is_some_and(|&d| p.js_depth > d) {
                continue;
            }
            let Some(f) = f else { continue };
            let mut deps = Vec::new();
            // The mode in which each specifier's entry in `resolutions` was resolved (its
            // first request's, ignoring `resolution-mode`).
            let mut first_modes: FxHashMap<&str, ResolutionMode> = FxHashMap::default();
            // The implicit import of the emit helpers (`tslib` under `importHelpers`), first
            // as in tsc.
            if let Some(spec) = crate::emit_helpers::helpers_specifier(&f, options) {
                let key = (f.path.clone(), spec.to_owned());
                let res = resolutions.entry(key).or_insert_with(|| {
                    resolve_request(&resolver, allow_js, &f.path, spec, RequestKind::Import)
                });
                let js_depth = admit_js(res, allow_js, p.js_depth, max_js_depth);
                if let Some(r) = res
                    && r.included
                {
                    deps.push(r.resolved.clone());
                    Pending {
                        path: r.resolved.clone(),
                        external: r.external || p.external,
                        js_depth,
                    }
                    .enqueue(&mut queue, &mut queued);
                }
            }
            for req in &f.bound.module_requests {
                let spec = f.ast.atom(req.specifier).to_owned();
                let base = resolver.mode_for(&f.path, import_kind(req.kind));
                let first = *first_modes.entry(f.ast.atom(req.specifier)).or_insert(base);
                // A request in another mode than its specifier's first resolves apart.
                let mode = req.resolution_mode.unwrap_or(base);
                if mode != first {
                    let key = (f.path.clone(), spec.clone(), mode);
                    let res = mode_resolutions.entry(key).or_insert_with(|| {
                        resolve_request_in(&resolver, allow_js, &f.path, &spec, mode)
                    });
                    let js_depth = admit_js(res, allow_js, p.js_depth, max_js_depth);
                    if let Some(r) = res
                        && r.included
                    {
                        deps.push(r.resolved.clone());
                        Pending {
                            path: r.resolved.clone(),
                            external: r.external || p.external,
                            js_depth,
                        }
                        .enqueue(&mut queue, &mut queued);
                    }
                    continue;
                }
                let key = (f.path.clone(), spec.clone());
                let res = resolutions.entry(key).or_insert_with(|| {
                    resolve_request(&resolver, allow_js, &f.path, &spec, req.kind)
                });
                let js_depth = admit_js(res, allow_js, p.js_depth, max_js_depth);
                if let Some(r) = res
                    && r.included
                {
                    deps.push(r.resolved.clone());
                    Pending {
                        path: r.resolved.clone(),
                        external: r.external || p.external,
                        js_depth,
                    }
                    .enqueue(&mut queue, &mut queued);
                }
            }
            // The implicit import of the JSX runtime (`react-jsx`, `react-jsxdev`).
            if let Some(spec) = crate::jsx_runtime::runtime_specifier(&f, options) {
                let key = (f.path.clone(), spec.clone());
                let res = resolutions.entry(key).or_insert_with(|| {
                    resolve_request(&resolver, allow_js, &f.path, &spec, RequestKind::Import)
                });
                let js_depth = admit_js(res, allow_js, p.js_depth, max_js_depth);
                if let Some(r) = res
                    && r.included
                {
                    deps.push(r.resolved.clone());
                    Pending {
                        path: r.resolved.clone(),
                        external: r.external || p.external,
                        js_depth,
                    }
                    .enqueue(&mut queue, &mut queued);
                }
            }
            for (kind, v) in &f.references {
                if let Some(t) = reference_target(fs, &resolver, &f.path, kind, v) {
                    deps.push(t.clone());
                    Pending {
                        path: t,
                        external: p.external,
                        js_depth: p.js_depth,
                    }
                    .enqueue(&mut queue, &mut queued);
                }
            }
            order_edges.insert(f.path.clone(), deps);
            loaded.insert(f.path.clone(), f);
        }
    }
    // Lib references of program files, in a deterministic order.
    let mut referenced: Vec<String> = Vec::new();
    let mut paths: Vec<&String> = loaded.keys().collect();
    paths.sort();
    for path in paths {
        for (kind, v) in &loaded[path].references {
            if *kind == "lib" && !referenced.iter().any(|r| r.eq_ignore_ascii_case(v)) {
                referenced.push(v.to_ascii_lowercase());
            }
        }
    }
    let (lib_files, missing_libs) = match libs {
        Some(l) => lib_closure(l, options, &lib_bind, &referenced),
        None => (Vec::new(), Vec::new()),
    };
    // Order: lib files, then a depth-first post-order from the `types` entries and roots
    // (dependencies before dependents, as tsc lists them).
    let mut files: Vec<Arc<SourceFile>> = lib_files;
    let mut done: FxHashSet<String> = FxHashSet::default();
    let mut starts: Vec<String> = type_roots;
    starts.extend(roots.iter().cloned());
    for s in starts {
        let mut stack: Vec<(String, usize)> = vec![(s, 0)];
        while let Some((cur, i)) = stack.pop() {
            if i == 0 && done.contains(&cur) {
                continue;
            }
            if i == 0 {
                done.insert(cur.clone());
            }
            let deps = order_edges.get(&cur).map(Vec::as_slice).unwrap_or(&[]);
            if i < deps.len() {
                stack.push((cur, i + 1));
                let d = deps[i].clone();
                if !done.contains(&d) {
                    stack.push((d, 0));
                }
            } else if let Some(f) = loaded.get(&cur) {
                files.push(f.clone());
            }
        }
    }
    if let Some(out) = package_jsons {
        *out = resolver.package_json_lookups();
    }
    ProgramFiles {
        files,
        resolutions,
        mode_resolutions,
        options: options.clone(),
        roots: roots.to_vec(),
        missing_libs,
        missing_types,
        ambiguous_roots: resolver.ambiguous_roots(),
        deps: order_edges,
    }
}

/// How [`load_program`] resolves module request `spec` (of kind `kind`) in file `from`
/// (also used to verify a saved history without loading the program).
pub fn resolve_request<R: FileSystem>(
    resolver: &Resolver<R>,
    allow_js: bool,
    from: &str,
    spec: &str,
    kind: RequestKind,
) -> Option<Resolution> {
    resolve_request_in(
        resolver,
        allow_js,
        from,
        spec,
        resolver.mode_for(from, import_kind(kind)),
    )
}

/// [`resolve_request`] in resolution mode `mode`.
pub fn resolve_request_in<R: FileSystem>(
    resolver: &Resolver<R>,
    allow_js: bool,
    from: &str,
    spec: &str,
    mode: ResolutionMode,
) -> Option<Resolution> {
    resolver.resolve_module(spec, from, mode).map(|r| {
        // JavaScript of a linked workspace package (real path outside `node_modules`) is
        // loaded with allowJs, though the package counts as a library (tsc 7, observed).
        // JavaScript under `node_modules` is not, even through a relative path
        // (`../../node_modules/eslint/lib/linter`), unless within `maxNodeModuleJsDepth`
        // (the program loader admits it).
        let js_library = r.resolved_file_name.contains("/node_modules/");
        let included = includable(r.extension, allow_js && !js_library);
        Resolution {
            resolved: r.resolved_file_name.clone(),
            extension: r.extension,
            external: r.is_external_library_import,
            included,
            package: r.package_id.is_some(),
            reference_source: r.project_reference_source.clone(),
        }
    })
}

/// `types: ["*"]` stands for every package directory in the type roots (`typeRoots`, else
/// each ancestor `node_modules/@types` of the config directory), by name, skipping names
/// that start with `.`; the other entries are kept. Without `*` the list is unchanged.
pub fn expand_wildcard_types<F: FileSystem>(
    fs: &F,
    resolver: &Resolver<&F>,
    types: &[String],
) -> Vec<String> {
    if !types.iter().any(|t| t == "*") {
        return types.to_vec();
    }
    let mut out: Vec<String> = types.iter().filter(|t| *t != "*").cloned().collect();
    let o = resolver.options();
    let roots = match &o.type_roots {
        Some(r) => r.clone(),
        None => {
            let mut roots = Vec::new();
            let mut d = o.config_dir.clone().unwrap_or_else(|| fs.cwd());
            for _ in 0..256 {
                roots.push(ts_r_config::path::combine(&d, "node_modules/@types"));
                let parent = ts_r_config::path::directory_of(&d).to_owned();
                if parent == d || parent.is_empty() {
                    break;
                }
                d = parent;
            }
            roots
        }
    };
    for root in roots {
        let mut names: Vec<String> = fs
            .read_dir(&root)
            .into_iter()
            .filter(|e| e.kind == ts_r_config::EntryKind::Directory && !e.name.starts_with('.'))
            .map(|e| e.name)
            .collect();
        names.sort();
        for n in names {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// The file a `/// <reference path|types="value" />` directive of `from` names, as
/// [`load_program`] finds it.
pub fn reference_target<F: FileSystem, R: FileSystem>(
    fs: &F,
    resolver: &Resolver<R>,
    from: &str,
    kind: &str,
    value: &str,
) -> Option<String> {
    match kind {
        "path" => {
            let dir = ts_r_config::path::directory_of(from);
            let joined = ts_r_config::path::normalize(&ts_r_config::path::combine(dir, value));
            let mut cands = vec![joined.clone()];
            if !crate::references::TS_EXTENSIONS
                .iter()
                .any(|e| joined.ends_with(e))
            {
                for e in crate::references::TS_EXTENSIONS {
                    cands.push(format!("{joined}{e}"));
                }
            }
            cands.into_iter().find(|c| fs.file_exists(c))
        }
        "types" | "types-import" | "types-require" => {
            let mode = match kind {
                "types-import" => ResolutionMode::Import,
                "types-require" => ResolutionMode::Require,
                // Without `resolution-mode`, the containing file's own mode under the Node
                // module kinds (a CommonJS file takes a package's `require` condition).
                _ if resolver.options().module.is_node() => {
                    resolver.mode_for(from, ImportKind::Static)
                }
                _ => ResolutionMode::Import,
            };
            resolver
                .resolve_type_reference(value, Some(from), mode)
                .map(|r| r.resolved_file_name.clone())
        }
        _ => None,
    }
}

/// Resolves the module requests of `f` (with the implicit emit helper and JSX runtime
/// imports) into `resolver`'s caches, so that [`load`], which walks the files one after
/// the other, finds the answers there. Called by the parsing threads: resolution is most
/// of the work of loading a program. The answers do not depend on the order they are asked in.
fn warm_resolutions<F: FileSystem>(
    resolver: &Resolver<&F>,
    f: &SourceFile,
    options: &CompilerOptions,
) {
    let implicit = crate::emit_helpers::helpers_specifier(f, options)
        .map(str::to_owned)
        .into_iter()
        .chain(crate::jsx_runtime::runtime_specifier(f, options));
    for spec in implicit {
        let mode = resolver.mode_for(&f.path, import_kind(RequestKind::Import));
        let _ = resolver.resolve_module(&spec, &f.path, mode);
    }
    for req in &f.bound.module_requests {
        let spec = f.ast.atom(req.specifier);
        let mode = req
            .resolution_mode
            .unwrap_or_else(|| resolver.mode_for(&f.path, import_kind(req.kind)));
        let _ = resolver.resolve_module(spec, &f.path, mode);
    }
}

/// Parses (and binds) the files of `wave` on up to `threads` threads; `side` runs on a
/// thread of its own meanwhile.
#[allow(clippy::too_many_arguments)]
fn parse_wave<F: FileSystem>(
    fs: &F,
    wave: &[Pending],
    options: &CompilerOptions,
    resolver: &Resolver<&F>,
    threads: usize,
    cache: Option<&SourceCache>,
    side: Option<&(dyn Fn() + Sync)>,
    loaded: &FxHashMap<String, Arc<SourceFile>>,
) -> Vec<Option<Arc<SourceFile>>> {
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<Arc<SourceFile>>>> = wave.iter().map(|_| Mutex::new(None)).collect();
    let work = |_: usize| loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        let Some(p) = wave.get(i) else { break };
        if let Some(f) = loaded.get(&p.path) {
            *out[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(f.clone());
            continue;
        }
        let opts = bind_options_for(&p.path, options, Some(resolver));
        // Taken before the stamp and the read: a write after this instant is never trusted.
        let read_at = if cache.is_some() { now_ns() } else { 0 };
        let stamp = cache.and_then(|_| fs.stamp(&p.path));
        if let (Some(c), Some(st)) = (cache, stamp)
            && let Some(f) = c.by_stamp(&p.path, st, &opts, p.external)
        {
            *out[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
            continue;
        }
        let Some(text) = fs.read_file(&p.path) else {
            continue;
        };
        let f = match cache {
            Some(c) => c.get_or_parse(&p.path, text, stamp, read_at, &opts, p.external),
            None => {
                let mut f = SourceFile::new(p.path.clone(), text, false, &opts);
                f.is_external = p.external;
                Arc::new(f)
            }
        };
        warm_resolutions(resolver, &f, options);
        *out[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    };
    let n = threads.min(wave.len()).max(1);
    if threads <= 1 || (n == 1 && side.is_none()) {
        if let Some(side) = side {
            side();
        }
        work(0);
    } else {
        std::thread::scope(|s| {
            if let Some(side) = side {
                s.spawn(side);
            }
            for t in 0..n {
                let work = &work;
                s.spawn(move || work(t));
            }
        });
    }
    out.into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect()
}
