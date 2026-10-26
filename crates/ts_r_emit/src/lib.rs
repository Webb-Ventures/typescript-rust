//! Program emit for `ts-r-check` (design record 0012, phase E1): output paths, the writer,
//! source maps, `noEmit`/`emitDeclarationOnly` semantics, the checker-backed
//! [`EmitResolver`], and tsc's file listing.
//!
//! ```text
//! let result = ts_r_emit::emit(&program, &CheckerResolver::new(&checker, &options), &cfg);
//! // diagnostics first (noEmitOnError), then:
//! let written = ts_r_emit::write_outputs(&result, &mut FsWriter, &program);
//! ```
//!
//! Every output is produced in memory (in parallel, files handed out by an atomic counter,
//! results kept in program order), so the caller knows every diagnostic before anything is
//! written. Outputs are never wrong silently (record 0012, invariant 2): syntax whose lowering
//! for the target is not implemented (TSR2003, per file, JavaScript only), a declaration that
//! would print an unresolved type (TSR2001, [`dts`]), a JavaScript input's declarations
//! (TSR2006) and a file the resolver could not answer for (TSR2002) are skipped with an
//! error, and [`EmitResult::skipped`] makes the exit code 1.

pub mod dts;
pub mod maps;
pub mod paths;
pub mod writer;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ts_r_ast::NodeId;
use ts_r_checker::{Checker, Loc, ProgramFiles, SourceFile};
use ts_r_codegen::{
    CodegenOptions, Layout, PrintHints, Printer, SourceMap, SourceMapOptions, Syntax,
};
use ts_r_config::{CompilerOptions, ConfigDiagnostic, NewLine, ParsedConfig, path};
use ts_r_diagnostics::{Category, Diagnostic, FileId, SOUND_CODE_BASE};
use ts_r_span::Span;
use ts_r_transform::{Answer, ConstValue, EmitResolver, TransformOptions};

pub use maps::MapRules;
pub use paths::PathRules;
pub use writer::{FsWriter, MemoryWriter, Writer};

/// TSR2002: the resolver answered `Unknown`; the file's output is skipped.
pub const TSR_RESOLVER_UNKNOWN: u32 = SOUND_CODE_BASE + 2002;
/// TSR2003: lowering for this target is not implemented.
pub const TSR_LOWERING: u32 = SOUND_CODE_BASE + 2003;
/// TSR2004: an output-changing flag is not implemented.
pub const TSR_NOT_IMPLEMENTED: u32 = SOUND_CODE_BASE + 2004;

/// The printer layout of emitted JavaScript.
pub const LAYOUT: Layout = Layout::Tsc;

/// What to emit and how.
#[derive(Clone, Copy, Debug)]
pub struct EmitConfig<'a> {
    pub options: &'a CompilerOptions,
    /// The tsconfig the program comes from. Its directory is the default `rootDir`; without
    /// one (the emit oracle's tests) the common source directory is.
    pub config: Option<&'a ParsedConfig>,
    /// Worker threads (at most 4 under the machine rules).
    pub threads: usize,
    pub case_sensitive: bool,
    /// Package lookups for `import("pkg")` types in declarations: the program's resolver
    /// (`None`: relative and `node_modules` paths only).
    pub specifiers: Option<&'a dyn ts_r_checker::typenode::ModuleSpecifierHost>,
}

/// The kind of an output file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Js,
    JsMap,
    Json,
    /// `.d.ts` / `.d.mts` / `.d.cts`.
    Dts,
    /// `.d.ts.map` (`declarationMap`).
    DtsMap,
}

/// One output, ready to write.
#[derive(Clone, Debug)]
pub struct OutputFile {
    /// Absolute path.
    pub path: String,
    /// The bytes to write (a BOM included under `emitBOM`).
    pub text: String,
    pub kind: OutputKind,
    /// Index of the source in `ProgramFiles::files`.
    pub source: usize,
    /// The source map of a JS output (also when it is inline).
    pub map: Option<SourceMap>,
}

/// What [`emit`] produced.
#[derive(Debug, Default)]
pub struct EmitResult {
    /// In tsc's listing order: program order, and per file `.js.map` before `.js`.
    pub outputs: Vec<OutputFile>,
    /// TSR2002/TSR2003/TSR2004 and TS5055 (program files by index; `u32::MAX` = global).
    pub diagnostics: Vec<Diagnostic>,
    /// Some output tsc would write is not produced (exit code 1 with errors).
    pub skipped: bool,
}

fn global(code: u32, message: String) -> Diagnostic {
    Diagnostic {
        code,
        category: Category::Error,
        message,
        file: FileId(u32::MAX),
        span: Span::EMPTY,
        related: Vec::new(),
    }
}

/// Whether `f` gets outputs: a source of the program, not a lib, declaration or file found
/// through `node_modules`.
pub fn emittable(f: &SourceFile) -> bool {
    !f.is_lib && !f.is_declaration() && !f.is_external
}

/// A JSON file only imported (not a root) is written only when its output lands elsewhere:
/// under `rootDir` with an `outDir`. Otherwise it is no source of the program's layout:
/// not in the common source directory, not TS6059, not TS6307 (observed:
/// `tsbuild/resolveJsonModule/include-only*`).
pub fn imported_json_is_emitted(
    root_dir: Option<&str>,
    out_dir: Option<&str>,
    file: &str,
    case_sensitive: bool,
) -> bool {
    match (root_dir, out_dir) {
        (Some(root), Some(out)) => {
            path::contains(root, file, case_sensitive)
                && !path::eq(
                    &path::combine(out, &path::relative(root, file, case_sensitive)),
                    file,
                    case_sensitive,
                )
        }
        _ => false,
    }
}

fn is_root(program: &ProgramFiles, file: &str, case_sensitive: bool) -> bool {
    program
        .roots
        .iter()
        .any(|r| path::eq(r, file, case_sensitive))
}

impl EmitConfig<'_> {
    fn emitted_paths<'p>(&self, program: &'p ProgramFiles) -> impl Iterator<Item = &'p str> {
        // The `rootDir` an imported JSON file is measured against: explicit or the config's
        // directory (without either it does not count).
        let root = self
            .options
            .root_dir()
            .map(str::to_owned)
            .or_else(|| self.config.map(|c| c.config_dir.clone()));
        let out = self.options.out_dir().map(str::to_owned);
        let cs = self.case_sensitive;
        program
            .files
            .iter()
            .filter(move |f| {
                emittable(f)
                    && (!f.is_json()
                        || is_root(program, &f.path, cs)
                        || imported_json_is_emitted(root.as_deref(), out.as_deref(), &f.path, cs))
            })
            .map(|f| f.path.as_str())
    }

    /// The effective `rootDir`: explicit, else the tsconfig's directory, else the common
    /// source directory.
    pub fn root_dir(&self, program: &ProgramFiles) -> String {
        if let Some(r) = self.options.root_dir() {
            return r.to_owned();
        }
        if let Some(c) = self.config {
            return c.config_dir.clone();
        }
        paths::common_source_dir(self.emitted_paths(program), self.case_sensitive)
            .unwrap_or_default()
    }

    pub fn path_rules(&self, program: &ProgramFiles) -> PathRules {
        PathRules {
            root_dir: self.root_dir(program),
            out_dir: self.options.out_dir().map(str::to_owned),
            declaration_dir: self.options.declaration_dir().map(str::to_owned),
            preserve_jsx: self.options.jsx() == Some(ts_r_config::JsxEmit::Preserve),
            case_sensitive: self.case_sensitive,
        }
    }

    fn map_rules(&self, root_dir: &str) -> MapRules {
        let o = self.options;
        MapRules {
            root_dir: root_dir.to_owned(),
            map_root: o.map_root().map(str::to_owned),
            source_root: o.source_root().map(str::to_owned),
            inline: o.inline_source_map() == Some(true),
            inline_sources: o.inline_sources() == Some(true),
            case_sensitive: self.case_sensitive,
        }
    }
}

/// Program-level output-path diagnostics, reported like option errors (tsc then reports no
/// syntactic or semantic diagnostics): TS6059 for sources outside `rootDir` (an explicit
/// `rootDir`: every emitted file, located at the import that brought it in when it is not a
/// root, also for `composite` projects; the default `rootDir` with `outDir`/
/// `declarationDir`: the roots), and, when the
/// program emits, TS5011 when `outDir`/`declarationDir` is set without `rootDir` and the
/// common source directory is not the tsconfig's directory; TS5009 when `outDir` has no
/// common source directory to mirror (files on different drives, no tsconfig or `rootDir`).
/// Observed in tsc 7.0.2.
pub fn root_dir_diagnostics(
    program: &ProgramFiles,
    cfg: &EmitConfig,
) -> (Vec<Diagnostic>, Vec<ConfigDiagnostic>) {
    let o = cfg.options;
    let mut located = Vec::new();
    let mut config = Vec::new();
    // A composite project's `rootDir` is its config directory (E3, observed): no TS5011,
    // and files outside it are TS6059 as with an explicit `rootDir`.
    let explicit = o.root_dir().is_some() || (o.composite() == Some(true) && cfg.config.is_some());
    let dirs = o.out_dir().is_some() || o.declaration_dir().is_some();
    // TS5009: `outDir` with no common source directory (no `rootDir`, no config file, and
    // emitted files on different roots), when some file sits under a drive or URL root.
    if o.out_dir().is_some()
        && !explicit
        && cfg.config.is_none()
        && cfg.emitted_paths(program).next().is_some()
        && paths::common_source_dir(cfg.emitted_paths(program), cfg.case_sensitive).is_none()
        && program.files.iter().any(|f| path::root_length(&f.path) > 1)
    {
        let message = "Cannot find the common subdirectory path for the input files.".to_owned();
        config.push(match o.origin(ts_r_config::OptionId::OutDir) {
            Some(origin) => ConfigDiagnostic::at(5009, message, &origin.file, origin.key_span),
            None => ConfigDiagnostic::global(5009, message),
        });
    }
    if !(explicit || (dirs && cfg.config.is_some())) {
        return (located, config);
    }
    let rules = cfg.path_rules(program);
    let config_name = cfg
        .config
        .map_or("tsconfig.json", |c| path::file_name(&c.config_path));
    let mut importers = None;
    for f in &program.files {
        if !emittable(f) || rules.under_root(&f.path) {
            continue;
        }
        let root = is_root(program, &f.path, cfg.case_sensitive);
        // An imported JSON file outside `rootDir` is not written: no error (observed).
        if f.is_json() && !root {
            continue;
        }
        let message = format!(
            "File '{}' is not under 'rootDir' '{}'. 'rootDir' is expected to contain all source files.",
            f.path, rules.root_dir
        );
        let root_reason = if root {
            cfg.config
                .map(|c| inclusion_reason(c, &f.path, cfg.case_sensitive))
        } else {
            None
        };
        // An imported file is reported at its first import, also when `include` matches it
        // too; the chain lists every reason when there is more than one (tsc 7.0.2).
        let first = if explicit {
            importers
                .get_or_insert_with(|| first_importers(program))
                .get(f.path.as_str())
                .copied()
        } else {
            None
        };
        if let Some((from, node)) = first {
            let mut reasons = import_reasons(program, &f.path, cfg.case_sensitive);
            reasons.extend(root_reason);
            let mut message = message;
            if reasons.len() > 1 {
                message.push_str("\n  The file is in the program because:");
                for r in &reasons {
                    message.push_str("\n    ");
                    message.push_str(r);
                }
            }
            let span = program.files[from].ast.span(node);
            located.push(Diagnostic {
                code: 6059,
                category: Category::Error,
                message,
                file: FileId(from as u32),
                span: Span::new(span.start, span.end),
                related: Vec::new(),
            });
        } else if root {
            let mut d = ConfigDiagnostic::global(6059, message);
            if let Some(r) = root_reason {
                d.related = vec![
                    "The file is in the program because:".to_owned(),
                    format!("  {r}"),
                ];
            }
            config.push(d);
        }
    }
    if !explicit
        && !o.effective().no_emit()
        && let Some(c) = cfg.config
        && let Some(common) =
            paths::common_source_dir(cfg.emitted_paths(program), cfg.case_sensitive)
        && !path::eq(&common, &c.config_dir, cfg.case_sensitive)
    {
        let id = if o.out_dir().is_some() {
            ts_r_config::OptionId::OutDir
        } else {
            ts_r_config::OptionId::DeclarationDir
        };
        let shown = path::relative_dotted(&c.config_dir, &common, cfg.case_sensitive);
        let shown = shown.trim_end_matches('/');
        let message = format!(
            "The common source directory of '{config_name}' is '{shown}'. The 'rootDir' setting must be explicitly set to this or another path to adjust your output's file layout."
        );
        let mut d = match o.origin(id) {
            Some(origin) => ConfigDiagnostic::at(5011, message, &origin.file, origin.key_span),
            None => ConfigDiagnostic::global(5011, message),
        };
        d.related = vec!["Visit https://aka.ms/ts6 for migration information.".to_owned()];
        config.push(d);
    }
    (located, config)
}

/// tsc's output-path check, a program diagnostic (reported whether or not anything is
/// written; not under `noEmit`, nor under tsc's internal `suppressOutputPathCheck`, which
/// the caller handles): every output path of every
/// emitted source, in program order (`.js`, its map, `.d.ts`, its map), is TS5055 when it is
/// a program file and TS5056 when an earlier source already writes it. Returns
/// `(code, output path)` pairs; the caller words them (tsc adds the "Adding a tsconfig.json
/// file" hint to TS5055 when there is no config file). Rule from
/// `tsc/internal/compiler/program.go` (`verifyEmitFilePath`) and
/// `tsc/internal/outputpaths/outputpaths.go`.
pub fn output_path_conflicts(program: &ProgramFiles, cfg: &EmitConfig) -> Vec<(u32, String)> {
    let o = cfg.options;
    let mut out = Vec::new();
    if o.effective().no_emit() {
        return out;
    }
    let cs = cfg.case_sensitive;
    let rules = cfg.path_rules(program);
    let declaration_only = o.emit_declaration_only() == Some(true);
    let declarations = o.effective().declaration();
    let map = o.source_map() == Some(true) && o.inline_source_map() != Some(true);
    let declaration_map = declarations && o.declaration_map() == Some(true);
    let inputs: std::collections::HashSet<String> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| path::key(&f.path, cs))
        .collect();
    let emitted: std::collections::HashSet<&str> = cfg.emitted_paths(program).collect();
    let mut seen = std::collections::HashSet::new();
    let allow_js = o.effective().allow_js();
    for f in &program.files {
        // A file reached through `node_modules` (also by a relative import) is a library's,
        // not emitted; JavaScript is only emitted with `allowJs`.
        if !emitted.contains(f.path.as_str())
            || (f.path.contains("/node_modules/") && !is_root(program, &f.path, cs))
            || (!allow_js && !f.kind.is_typescript() && !f.is_json())
        {
            continue;
        }
        let mut outputs: Vec<String> = Vec::new();
        if f.is_json() {
            if !declaration_only && let Some(js) = rules.json_output(&f.path) {
                outputs.push(js);
            }
        } else {
            if !declaration_only && let Some(js) = rules.js_output(&f.path) {
                if map {
                    outputs.push(js.clone());
                    outputs.push(format!("{js}.map"));
                } else {
                    outputs.push(js);
                }
            }
            if declarations && let Some(dts) = rules.dts_output(&f.path) {
                if declaration_map {
                    outputs.push(dts.clone());
                    outputs.push(format!("{dts}.map"));
                } else {
                    outputs.push(dts);
                }
            }
        }
        for p in outputs {
            let key = path::key(&p, cs);
            if inputs.contains(&key) {
                out.push((5055, p.clone()));
            }
            if !seen.insert(key) {
                out.push((5056, p));
            }
        }
    }
    out
}

/// Why a root file is in the program (tsc's `--explainFiles` wording).
fn inclusion_reason(c: &ParsedConfig, file: &str, cs: bool) -> String {
    let name = path::file_name(&c.config_path);
    if let Some(files) = &c.files
        && files
            .specs
            .iter()
            .any(|s| path::eq(&path::resolve(&files.base_dir, s), file, cs))
    {
        return format!("Part of 'files' list in {name}");
    }
    match &c.include {
        None => "Matched by default include pattern '**/*'".to_owned(),
        Some(inc) => {
            let spec = inc.specs.iter().find(|s| {
                let abs = path::resolve(&inc.base_dir, s);
                ts_r_config::glob::Pattern::new(&abs, true).matches(file, cs)
            });
            match spec {
                // The config's absolute path here, its file name for `files` (tsc 7.0.2).
                Some(s) => format!("Matched by include pattern '{s}' in '{}'", c.config_path),
                None => "Matched by default include pattern '**/*'".to_owned(),
            }
        }
    }
}

/// The "Imported via" reasons for `target`, in the order tsc finds them: depth first from
/// each root in root order, each file's imports in source order (absolute paths).
fn import_reasons(program: &ProgramFiles, target: &str, cs: bool) -> Vec<String> {
    let index: std::collections::HashMap<String, usize> = program
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (path::key(&f.path, cs), i))
        .collect();
    let find = |p: &str| index.get(&path::key(p, cs)).copied();
    let target = path::key(target, cs);
    let mut out = Vec::new();
    let mut visited = vec![false; program.files.len()];
    // (file, the next module request to follow): an explicit stack, no recursion.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for root in &program.roots {
        let Some(start) = find(root) else { continue };
        if visited[start] {
            continue;
        }
        visited[start] = true;
        stack.push((start, 0));
        while let Some((i, next)) = stack.last_mut() {
            let f = &program.files[*i];
            let Some(req) = f.bound.module_requests.get(*next) else {
                stack.pop();
                continue;
            };
            *next += 1;
            let spec = f.ast.atom(req.specifier);
            let Some(Some(r)) = program.resolutions.get(&(f.path.clone(), spec.to_owned())) else {
                continue;
            };
            let Some(t) = find(&r.resolved).filter(|_| r.included) else {
                continue;
            };
            if path::key(&r.resolved, cs) == target {
                let start = f.ast.span(req.node).start as usize;
                let quote = match f.text.as_bytes().get(start) {
                    Some(b'\'') => '\'',
                    _ => '"',
                };
                out.push(format!(
                    "Imported via {quote}{spec}{quote} from file '{}'",
                    f.path
                ));
            }
            if !visited[t] && !program.files[t].is_lib {
                visited[t] = true;
                stack.push((t, 0));
            }
        }
    }
    out
}

/// Each target's first importer in program order, and the specifier node.
fn first_importers(program: &ProgramFiles) -> std::collections::HashMap<&str, (usize, NodeId)> {
    let mut importers = std::collections::HashMap::new();
    for (i, f) in program.files.iter().enumerate() {
        for req in &f.bound.module_requests {
            let spec = f.ast.atom(req.specifier);
            if let Some(Some(r)) = program.resolutions.get(&(f.path.clone(), spec.to_owned())) {
                importers
                    .entry(r.resolved.as_str())
                    .or_insert((i, req.node));
            }
        }
    }
    importers
}

/// The checker's answers as an [`EmitResolver`].
pub struct CheckerResolver<'c, 'a> {
    checker: &'c Checker<'a>,
    preserve_const_enums: bool,
    /// `isolatedModules`: const enums of other files are not inlined (observed).
    isolated_modules: bool,
    /// Per file: its implicit automatic JSX runtime import resolves to a module.
    jsx_runtime_found: Vec<bool>,
}

impl<'c, 'a> CheckerResolver<'c, 'a> {
    pub fn new(checker: &'c Checker<'a>, options: &CompilerOptions) -> Self {
        CheckerResolver {
            checker,
            preserve_const_enums: options.effective().preserve_const_enums(),
            isolated_modules: options.effective().isolated_modules(),
            jsx_runtime_found: jsx_runtimes_found(checker, options),
        }
    }

    /// The checker behind the answers (declaration emit asks it for types).
    pub fn checker(&self) -> &'c Checker<'a> {
        self.checker
    }
}

impl std::fmt::Debug for CheckerResolver<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckerResolver")
            .field("preserve_const_enums", &self.preserve_const_enums)
            .finish_non_exhaustive()
    }
}

/// Per file of the checker: whether the automatic JSX runtime it imports implicitly
/// (`react/jsx-runtime`) resolves to a module, ambient or a program file, as the checker
/// resolves it to type the file's JSX.
fn jsx_runtimes_found(checker: &Checker<'_>, options: &CompilerOptions) -> Vec<bool> {
    if options.jsx().is_none() {
        return Vec::new();
    }
    let paths: rustc_hash::FxHashSet<&str> =
        checker.files.iter().map(|f| f.path.as_str()).collect();
    checker
        .files
        .iter()
        .map(|f| {
            let Some(spec) = ts_r_checker::jsx_runtime::runtime_specifier(f, options) else {
                return false;
            };
            if checker.link.find_ambient_module(&spec).is_some() {
                return true;
            }
            matches!(
                checker.resolutions.get(&(f.path.clone(), spec)),
                Some(Some(r)) if paths.contains(r.resolved.as_str())
            )
        })
        .collect()
}

impl EmitResolver for CheckerResolver<'_, '_> {
    fn jsx_runtime_found(&self, file: FileId) -> bool {
        self.jsx_runtime_found
            .get(file.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    fn is_value_alias(&self, file: FileId, alias: NodeId) -> Answer {
        match self
            .checker
            .alias_is_value(Loc::new(file, alias), self.preserve_const_enums)
        {
            Some(true) => Answer::Yes,
            Some(false) => Answer::No,
            None => Answer::Unknown,
        }
    }

    fn const_enum_value(&self, file: FileId, access: NodeId) -> Option<ConstValue> {
        if self.isolated_modules {
            return None;
        }
        match self
            .checker
            .const_enum_access_value(Loc::new(file, access))?
        {
            ts_r_checker::emit::EnumConst::Number(n) => Some(ConstValue::Number(n)),
            ts_r_checker::emit::EnumConst::String(s) => Some(ConstValue::String(s)),
        }
    }
}

struct Ctx<'a, 'c> {
    program: &'a ProgramFiles,
    resolver: &'a CheckerResolver<'c, 'a>,
    cfg: &'a EmitConfig<'a>,
    rules: PathRules,
    maps: MapRules,
    newline: &'static str,
    bom: bool,
    comments: bool,
    source_map: bool,
    js: bool,
    /// Declaration output options (`declaration` / `emitDeclarationOnly`).
    dts: Option<dts::DtsOptions>,
}

#[derive(Default)]
struct FileOut {
    outputs: Vec<OutputFile>,
    diagnostics: Vec<Diagnostic>,
    skipped: bool,
}

/// Emits every emittable file of `program` into memory.
pub fn emit<'a, 'c>(
    program: &'a ProgramFiles,
    resolver: &'a CheckerResolver<'c, 'a>,
    cfg: &EmitConfig,
) -> EmitResult {
    let o = cfg.options;
    let e = o.effective();
    let mut result = EmitResult::default();
    if e.no_emit() {
        result.skipped = true;
        return result;
    }
    let declaration_only = o.emit_declaration_only() == Some(true);
    let declarations = e.declaration() || declaration_only;
    let mut js = !declaration_only;
    if js && !ts_r_transform::downlevel::is_supported(e.target()) {
        result.diagnostics.push(global(
            TSR_LOWERING,
            format!(
                "Emitting for target '{}' is not implemented: ts-r writes JavaScript for ES5 (lowered like ES2015) and later only. Nothing was written.",
                target_name(e.target())
            ),
        ));
        result.skipped = true;
        js = false;
    }
    if !js && !declarations {
        return result;
    }
    let newline = match e.new_line() {
        NewLine::Crlf => "\r\n",
        NewLine::Lf => "\n",
    };
    let dts = declarations.then_some(dts::DtsOptions {
        newline,
        remove_comments: o.remove_comments() == Some(true),
        strip_internal: o.strip_internal() == Some(true),
        declaration_map: o.declaration_map() == Some(true),
        isolated_declarations: o.isolated_declarations() == Some(true),
    });
    let rules = cfg.path_rules(program);
    let ctx = Ctx {
        program,
        resolver,
        cfg,
        maps: cfg.map_rules(&rules.root_dir),
        rules,
        newline: match e.new_line() {
            NewLine::Crlf => "\r\n",
            NewLine::Lf => "\n",
        },
        bom: o.emit_bom() == Some(true),
        comments: o.remove_comments() != Some(true),
        source_map: o.source_map() == Some(true) || o.inline_source_map() == Some(true),
        js,
        dts,
    };
    let todo: Vec<usize> = (0..program.files.len())
        .filter(|&i| emittable(&program.files[i]))
        .collect();
    let slots: Vec<Mutex<Option<FileOut>>> = todo.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            let k = next.fetch_add(1, Ordering::Relaxed);
            let Some(&i) = todo.get(k) else { break };
            let out = emit_file(&ctx, i);
            if let Ok(mut s) = slots[k].lock() {
                *s = Some(out);
            }
        }
    };
    let threads = cfg.threads.clamp(1, 4).min(todo.len().max(1));
    if threads <= 1 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..threads {
                let spawned = std::thread::Builder::new()
                    .stack_size(64 << 20)
                    .spawn_scoped(s, work);
                if spawned.is_err() {
                    work();
                }
            }
        });
    }
    // The input set, for overwrite protection (TS5055).
    let inputs: std::collections::HashSet<String> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| path::key(&f.path, cfg.case_sensitive))
        .collect();
    let files: Vec<FileOut> = slots
        .into_iter()
        .filter_map(|slot| slot.into_inner().ok().flatten())
        .collect();
    let mut output_counts = std::collections::HashMap::new();
    for out in &files {
        for f in &out.outputs {
            *output_counts
                .entry(path::key(&f.path, cfg.case_sensitive))
                .or_insert(0usize) += 1;
        }
    }
    let mut reported = std::collections::HashSet::new();
    for mut out in files {
        result
            .diagnostics
            .extend(std::mem::take(&mut out.diagnostics));
        result.skipped |= out.skipped;
        // Outputs that overwrite inputs (TS5055) or each other (TS5056) are not written,
        // and neither are the other outputs of their group (JavaScript with its map,
        // declarations with theirs): tsc 7.0.2 still writes `main.cjs` when `main.d.cts`
        // would overwrite an input.
        for group in [false, true] {
            let in_group = |f: &OutputFile| {
                (f.kind == OutputKind::Dts || f.kind == OutputKind::DtsMap) == group
            };
            let clobbers = out
                .outputs
                .iter()
                .filter(|f| in_group(f))
                .any(|f| inputs.contains(&path::key(&f.path, cfg.case_sensitive)));
            let conflicts = out
                .outputs
                .iter()
                .filter(|f| in_group(f))
                .any(|f| output_counts[&path::key(&f.path, cfg.case_sensitive)] > 1);
            if !(clobbers || conflicts) {
                continue;
            }
            for f in out.outputs.iter().filter(|f| in_group(f)) {
                if inputs.contains(&path::key(&f.path, cfg.case_sensitive)) {
                    result.diagnostics.push(global(
                        5055,
                        format!(
                            "Cannot write file '{}' because it would overwrite input file.",
                            f.path
                        ),
                    ));
                }
                let key = path::key(&f.path, cfg.case_sensitive);
                if output_counts[&key] > 1 && reported.insert(key) {
                    result.diagnostics.push(global(
                        5056,
                        format!(
                            "Cannot write file '{}' because it would be overwritten by multiple input files.",
                            f.path
                        ),
                    ));
                }
            }
            result.skipped = true;
            out.outputs.retain(|f| !in_group(f));
        }
        result.outputs.extend(out.outputs);
    }
    result
}

fn target_name(t: ts_r_config::ScriptTarget) -> &'static str {
    use ts_r_config::ScriptTarget as T;
    match t {
        T::ES5 => "ES5",
        T::ES2015 => "ES2015",
        T::ES2016 => "ES2016",
        T::ES2017 => "ES2017",
        T::ES2018 => "ES2018",
        T::ES2019 => "ES2019",
        T::ES2020 => "ES2020",
        T::ES2021 => "ES2021",
        T::ES2022 => "ES2022",
        T::ES2023 => "ES2023",
        T::ES2024 => "ES2024",
        T::ES2025 => "ES2025",
        T::ESNext => "ESNext",
    }
}

fn emit_file(ctx: &Ctx, i: usize) -> FileOut {
    let mut out = emit_js(ctx, i);
    if let Some(opts) = &ctx.dts {
        emit_declarations(ctx, opts, i, &mut out);
    }
    out
}

/// The `.d.ts` output of file `i` (after its JavaScript outputs, tsc's listing order).
fn emit_declarations(ctx: &Ctx, opts: &dts::DtsOptions, i: usize, out: &mut FileOut) {
    let f = &ctx.program.files[i];
    if f.is_json() {
        return;
    }
    let Some(dts_path) = ctx.rules.dts_output(&f.path) else {
        return;
    };
    if !f.kind.is_typescript() {
        out.diagnostics.push(Diagnostic {
            code: dts::TSR_JS_DECLARATIONS,
            category: Category::Error,
            message: format!(
                "Declaration emit for JavaScript files is not implemented yet (design record 0012); '{dts_path}' was not written."
            ),
            file: FileId(i as u32),
            span: Span::EMPTY,
            related: Vec::new(),
        });
        out.skipped = true;
        return;
    }
    let file = dts::declaration_file(
        ctx.resolver.checker(),
        FileId(i as u32),
        opts,
        ctx.cfg.specifiers,
    );
    out.diagnostics.extend(file.diagnostics);
    let Some(text) = file.text else {
        out.skipped = true;
        return;
    };
    let mut text = rewrite_reference_paths(ctx, &f.path, &dts_path, text);
    let mut map_output = None;
    if let Some(mut map) = file.map {
        // Declaration maps are files next to the declaration, never inline.
        let rules = MapRules {
            inline: false,
            ..ctx.maps.clone()
        };
        let mp = rules.paths(&f.path, &dts_path);
        map.file = Some(mp.file.clone());
        map.sources = vec![mp.source.clone()];
        let json = maps::map_json(&map, &mp, None);
        text.push_str("//# sourceMappingURL=");
        text.push_str(&mp.url);
        map_output = Some(OutputFile {
            path: format!("{dts_path}.map"),
            text: json,
            kind: OutputKind::DtsMap,
            source: i,
            map: None,
        });
    }
    // The map is written first, as for JavaScript (observed: `--listEmittedFiles`).
    out.outputs.extend(map_output);
    out.outputs.push(OutputFile {
        path: dts_path,
        text: with_bom(ctx, text),
        kind: OutputKind::Dts,
        source: i,
        map: None,
    });
}

/// A kept `/// <reference path="..." />` names the referenced file's declaration (its
/// output for a source of the program, else the file) relative to the declaration file,
/// as tsc rewrites it (observed): `../../types/bar.d.ts` from `bin/`, `internal.d.ts`.
fn rewrite_reference_paths(ctx: &Ctx, source: &str, dts_path: &str, text: String) -> String {
    const PREFIX: &str = "/// <reference path=\"";
    if !text.contains(PREFIX) {
        return text;
    }
    let cs = ctx.cfg.case_sensitive;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix(PREFIX)
            && let Some(end) = rest.find('"')
        {
            let written = &rest[..end];
            let abs = path::normalize(&path::combine(path::directory_of(source), written));
            let target = reference_target(ctx, &abs).unwrap_or(abs);
            out.push_str(PREFIX);
            out.push_str(&path::relative(path::directory_of(dts_path), &target, cs));
            out.push_str(&rest[end..]);
            continue;
        }
        out.push_str(line);
    }
    out
}

/// The declaration a reference to `abs` (a `.d.ts` path, as the declaration emitter
/// writes it) names: a declaration file of the program as it is, or the declaration output
/// of the source it stands for.
fn reference_target(ctx: &Ctx, abs: &str) -> Option<String> {
    let cs = ctx.cfg.case_sensitive;
    let files = &ctx.program.files;
    if files.iter().any(|f| path::eq(&f.path, abs, cs)) {
        return Some(abs.to_owned());
    }
    let (base, exts): (&str, &[&str]) = if let Some(b) = abs.strip_suffix(".d.mts") {
        (b, &[".mts"])
    } else if let Some(b) = abs.strip_suffix(".d.cts") {
        (b, &[".cts"])
    } else {
        (abs.strip_suffix(".d.ts")?, &[".ts", ".tsx"])
    };
    exts.iter().find_map(|ext| {
        let candidate = format!("{base}{ext}");
        files
            .iter()
            .find(|f| path::eq(&f.path, &candidate, cs))
            .and_then(|f| ctx.rules.dts_output(&f.path))
    })
}

fn emit_js(ctx: &Ctx, i: usize) -> FileOut {
    let f = &ctx.program.files[i];
    let mut out = FileOut::default();
    if f.is_json() {
        if let Some(p) = ctx.rules.json_output(&f.path) {
            let text = print_json(ctx, f);
            out.outputs.push(OutputFile {
                path: p,
                text: with_bom(ctx, text),
                kind: OutputKind::Json,
                source: i,
                map: None,
            });
        }
        return out;
    }
    if !ctx.js {
        return out;
    }
    let Some(js_path) = ctx.rules.js_output(&f.path) else {
        return out;
    };
    let mut opts =
        TransformOptions::from_compiler_options(ctx.cfg.options, &f.path, !f.package_esm);
    // tsc 7 adds no `export {};` marker under `module: preserve` (observed).
    opts.preserve_module_marker = false;
    let mut ast = f.ast.clone();
    // JavaScript is printed as written, without the types JSDoc desugaring added.
    ast.strip_jsdoc();
    let r =
        ts_r_transform::transform_with(&mut ast, &f.text, &opts, ctx.resolver, FileId(i as u32));
    // Syntax the target lacks that ts-r cannot lower yet (record 0012, E5): never written.
    if let Some(&(span, what)) = r.unlowered.first() {
        out.diagnostics.push(Diagnostic {
            code: TSR_LOWERING,
            category: Category::Error,
            message: format!(
                "Lowering a {what} for target '{}' is not implemented yet (design record 0012, E5); '{}' was not written.",
                target_name(opts.target),
                js_path
            ),
            file: FileId(i as u32),
            span,
            related: Vec::new(),
        });
        out.skipped = true;
        return out;
    }
    if !r.unknown.is_empty() {
        for (span, what) in &r.unknown {
            out.diagnostics.push(Diagnostic {
                code: TSR_RESOLVER_UNKNOWN,
                category: Category::Error,
                message: format!(
                    "The checker cannot tell whether this {what} must stay in the JavaScript output; '{}' was not written.",
                    js_path
                ),
                file: FileId(i as u32),
                span: Span::new(span.start, span.end),
                related: Vec::new(),
            });
        }
        out.skipped = true;
        return out;
    }
    let codegen = CodegenOptions {
        layout: LAYOUT,
        comments: ctx.comments,
        newline: ctx.newline,
        source_map: ctx.source_map.then(|| SourceMapOptions {
            file: Some(path::file_name(&js_path).to_owned()),
            source: path::file_name(&f.path).to_owned(),
            source_root: None,
            include_content: false,
        }),
        ..CodegenOptions::default()
    };
    let hints = PrintHints {
        erased: &r.erased,
        prelude: &r.prelude,
        single_line: &r.single_line,
        multi_line: &r.multi_line,
        single_quoted: &r.single_quoted,
        map_ranges: &r.map_ranges,
        moved_comments: &r.moved_comments,
        no_detached_comments: r.no_detached_comments,
        strip_numeric_separators: opts.strip_numeric_separators,
    };
    let printed = ts_r_codegen::print_with(&ast, &f.text, &codegen, &hints);
    let mut text = printed.code;
    if let Some(map) = &printed.map {
        let mp = ctx.maps.paths(&f.path, &js_path);
        let content = ctx.maps.inline_sources.then_some(f.text.as_str());
        let json = maps::map_json(map, &mp, content);
        text.push_str("//# sourceMappingURL=");
        if ctx.maps.inline {
            text.push_str(&maps::data_url(&json));
        } else {
            text.push_str(&mp.url);
            out.outputs.push(OutputFile {
                path: format!("{js_path}.map"),
                text: json,
                kind: OutputKind::JsMap,
                source: i,
                map: None,
            });
        }
    }
    out.outputs.push(OutputFile {
        path: js_path,
        text: with_bom(ctx, text),
        kind: OutputKind::Js,
        source: i,
        map: printed.map,
    });
    out
}

fn with_bom(ctx: &Ctx, text: String) -> String {
    if ctx.bom {
        format!("\u{feff}{text}")
    } else {
        text
    }
}

/// A JSON source as tsc copies it: the value printed like a JavaScript expression.
fn print_json(ctx: &Ctx, f: &SourceFile) -> String {
    let ast = &f.ast;
    let stmts = match ast.node(ast.root) {
        ts_r_ast::Node::Program { stmts } => ast.list(*stmts).to_vec(),
        _ => Vec::new(),
    };
    let Some(&first) = stmts.first() else {
        return String::new();
    };
    let expr = match ast.node(first) {
        ts_r_ast::Node::ExprStmt { expr } => *expr,
        _ => first,
    };
    let opts = CodegenOptions {
        layout: LAYOUT,
        comments: false,
        newline: ctx.newline,
        ..CodegenOptions::default()
    };
    let hints = PrintHints::default();
    let mut p = Printer::new(ast, &f.text, &opts, &hints).with_syntax(Syntax::JavaScript);
    p.any(expr);
    p.finish().code
}

/// Writes `result`'s outputs in order. Returns the paths written (tsc's
/// `--listEmittedFiles` order) and TS5033 for each failed write.
pub fn write_outputs(
    result: &EmitResult,
    writer: &mut dyn Writer,
) -> (Vec<String>, Vec<Diagnostic>) {
    let mut written = Vec::with_capacity(result.outputs.len());
    let mut failed = Vec::new();
    let files: Vec<(&str, &[u8])> = result
        .outputs
        .iter()
        .map(|f| (f.path.as_str(), f.text.as_bytes()))
        .collect();
    for (f, r) in result.outputs.iter().zip(writer.write_all(&files)) {
        match r {
            Ok(()) => written.push(f.path.clone()),
            Err(e) => failed.push(global(
                5033,
                format!("Could not write file '{}': {e}.", f.path),
            )),
        }
    }
    (written, failed)
}

/// `--listEmittedFiles`: `TSFILE: <path>` per written file.
pub fn listing(written: &[String]) -> String {
    let mut s = String::new();
    for p in written {
        s.push_str("TSFILE: ");
        s.push_str(p);
        s.push('\n');
    }
    s
}
