//! Compares emitted files with TypeScript 7's emit baselines (design record 0012, E0).
//!
//! ```text
//! cargo xtask upstream sync --minimal   # once
//! cargo build --release -j 4 -p ts_r_check --example emit_oracle
//! RAYON_NUM_THREADS=4 scripts/limit.sh --mem-mb 4096 --time-s 1800 -- \
//!   target/release/examples/emit_oracle [--emitter check|transpile|program] [--layout readable|tsc]
//!   [--threads N] [--verbose] [--show] [--filter-file F] [--write-gaps] [filter...]
//! ```
//!
//! Every test under `tests/cases/{compiler,conformance}` (submodule baselines) and
//! `tsc/testdata/tests/cases/{compiler,conformance}` (tsc 7's own, area prefix `tsc7/`) with
//! a `.js` baseline, one run per baseline variant (`name(target=es2015).js`), is split into
//! its files under `/.src` in a [`MemoryFs`] with the test's options (a `tsconfig.json` unit
//! is extended, so its options and file list apply, with the directives over it). The
//! emitter under test writes its outputs, and each output the baseline lists is compared:
//!
//! - **JS** (`.js`, `.mjs`, `.cjs`, `.jsx`) and **d.ts** (`.d.ts`, `.d.mts`, `.d.cts`) from
//!   the `.js` baseline's output sections: `bytes` when byte-identical (the harness writes
//!   CRLF unless the test sets `newLine`, so emitters get its newline), `shape` when both
//!   parse to the same AST shape (`ts_r_codegen::shape`; comments and layout ignored).
//! - **map**: `.js.map` baselines (bytes of the JSON) and `.sourcemap.txt` baselines (every
//!   `Emitted(l, c) Source(l, c) + SourceIndex(i)` segment; `shape` when they all agree).
//! - **json**: `.json` copies (`resolveJsonModule` with `outDir`).
//!
//! Outputs the baseline names that the emitter did not write are `missing` with the
//! emitter's reason; outputs it wrote that the baseline does not name are `extra`. Not
//! byte-exact outputs fall into cause buckets: layout causes for shape-equal ones (line
//! endings, end of file, blank lines, indentation, spacing, comments), and for others the
//! construct on the first differing line (helpers, CommonJS, enum/namespace IIFE, JSX, ...).
//! Rows: ALL, ES2022+ (effective `target` ES2022 or later), each area. Mismatches go to
//! `target/emit-oracle.txt`.
//!
//! Emitters (`--emitter`): `check` (default) is what `ts-r-check` writes today: nothing.
//! `transpile` runs `ts_r_transform::transpile` on each root file, the per-file starting
//! point for E1's program emitter. `program` is E1's emitter: `ts_r_emit::emit` over the
//! loaded program with the checker's `EmitResolver`, as `ts-r-check` writes. For it the run
//! ends with record 0012's E1 gates over ES2022+ JavaScript: shape-equal outside
//! `examples/emit_known_gaps.txt` (`--write-gaps` rewrites it from a full run), the bytes
//! rate, and source-map segments on bytes-exact outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ts_r_ast::SourceKind;
use ts_r_codegen::sourcemap::{SourceMap, decode_mappings};
use ts_r_codegen::{CodegenOptions, SourceMapOptions};
use ts_r_config::options::{OptionType, find_option_ignore_case};
use ts_r_config::{
    FileSystem, JsxEmit, MemoryFs, NewLine, OptionId, OptionValue, ParsedConfig, ScriptTarget,
    load_tsconfig, path,
};
use ts_r_transform::TransformOptions;

ts_r_testing::cap_memory!();

type Pairs = Vec<(String, String)>;

/// A source-map segment as `.sourcemap.txt` prints it: generated line and column (1-based),
/// then source index (0-based), source line and column (1-based).
type Seg = (u32, u32, Option<(u32, u32, u32)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Js,
    Dts,
    Map,
    Json,
    Other,
}

impl Kind {
    const ALL: [Kind; 5] = [Kind::Js, Kind::Dts, Kind::Map, Kind::Json, Kind::Other];

    fn of(name: &str) -> Kind {
        let lower = name.to_ascii_lowercase();
        if SourceKind::from_path(&lower) == SourceKind::Dts {
            Kind::Dts
        } else if lower.ends_with(".map") {
            Kind::Map
        } else if [".js", ".mjs", ".cjs", ".jsx"]
            .iter()
            .any(|e| lower.ends_with(e))
        {
            Kind::Js
        } else if lower.ends_with(".json") {
            Kind::Json
        } else {
            Kind::Other
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Js => "js",
            Kind::Dts => "d.ts",
            Kind::Map => "map",
            Kind::Json => "json",
            Kind::Other => "other",
        }
    }
}

/// One output a baseline lists.
#[derive(Clone, Debug)]
struct Expected {
    /// As the baseline names it: the base name, or the full path with `@fullEmitPaths`.
    name: String,
    kind: Kind,
    /// The exact bytes (for maps: the `.js.map` JSON), when the baseline has them.
    text: Option<String>,
    /// Source-map segments (`.sourcemap.txt`, else decoded from the JSON).
    segs: Option<Vec<Seg>>,
}

struct Test {
    name: String,
    area: String,
    options: Pairs,
    files: Pairs,
    expected: Vec<Expected>,
}

/// What an emitter gets: the test's files under `/.src` and its loaded configuration.
struct EmitInput<'a> {
    fs: &'a MemoryFs,
    config: &'a ParsedConfig,
    /// Root files (absolute), in the test's order.
    roots: &'a [String],
    /// The newline the baseline was written with (`\r\n` unless the test sets `newLine`).
    newline: &'static str,
    /// The test has its own `tsconfig.json` (else the harness config only carries options).
    tsconfig: bool,
}

/// One file an emitter wrote.
struct Output {
    /// Absolute path.
    path: String,
    text: String,
    /// The source map of a JS or d.ts output (also when it is inline).
    map: Option<SourceMap>,
}

/// Why outputs are missing, when the emitter knows (overrides [`Emitter::missing_reason`]).
#[derive(Clone, Copy, Debug, Default)]
struct Reasons {
    js: Option<&'static str>,
    dts: Option<&'static str>,
}

trait Emitter: Sync {
    /// Why an expected output of `kind` that was not written is missing.
    fn missing_reason(&self, kind: Kind) -> &'static str;
    fn emit(&self, input: &EmitInput) -> Vec<Output>;
    /// Like [`Emitter::emit`], plus why JS outputs are missing for this test, when the
    /// emitter knows (overrides [`Emitter::missing_reason`] for JS).
    fn emit_explained(&self, input: &EmitInput) -> (Vec<Output>, Reasons) {
        (self.emit(input), Reasons::default())
    }
}

/// `ts-r-check` today: checks and writes nothing.
struct CheckEmitter;

impl Emitter for CheckEmitter {
    fn missing_reason(&self, _: Kind) -> &'static str {
        "ts-r-check writes no outputs (E1)"
    }

    fn emit(&self, _: &EmitInput) -> Vec<Output> {
        Vec::new()
    }
}

/// Per-file `ts_r_transform::transpile` next to each TypeScript root (no `outDir` mapping,
/// no declarations, no JSON copies, no JavaScript inputs). `layout` is `--layout`.
struct TranspileEmitter {
    layout: ts_r_codegen::Layout,
}

fn opt_bool(c: &ParsedConfig, id: OptionId) -> bool {
    matches!(c.options.get(id), Some(OptionValue::Bool(true)))
}

fn opt_str(c: &ParsedConfig, id: OptionId) -> Option<&str> {
    match c.options.get(id) {
        Some(OptionValue::String(s)) => Some(s),
        _ => None,
    }
}

/// The nearest `package.json` above `file` does not say `"type": "module"`.
fn commonjs_package(fs: &MemoryFs, file: &str) -> bool {
    let mut dir = path::directory_of(file).to_owned();
    loop {
        if let Some(text) = fs.read_file(&path::combine(&dir, "package.json")) {
            let Some(i) = text.find("\"type\"") else {
                return true;
            };
            let rest = text[i + 6..].trim_start();
            let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
            return !rest.starts_with("\"module\"");
        }
        let parent = path::directory_of(&dir).to_owned();
        if parent == dir || parent.is_empty() {
            return true;
        }
        dir = parent;
    }
}

impl Emitter for TranspileEmitter {
    fn missing_reason(&self, kind: Kind) -> &'static str {
        match kind {
            Kind::Js => "not a TypeScript root next to its output (outDir, JS input, ...)",
            Kind::Dts => "no declaration emitter (E2)",
            Kind::Map => "no source map for this output",
            Kind::Json => "JSON copies not implemented (E1)",
            Kind::Other => "not emitted",
        }
    }

    fn emit(&self, input: &EmitInput) -> Vec<Output> {
        let c = input.config;
        if opt_bool(c, OptionId::NoEmit) || opt_bool(c, OptionId::EmitDeclarationOnly) {
            return Vec::new();
        }
        let source_map = opt_bool(c, OptionId::SourceMap);
        let inline = opt_bool(c, OptionId::InlineSourceMap);
        let preserve_jsx = c.options.effective().jsx() == Some(JsxEmit::Preserve);
        let mut out = Vec::new();
        for root in input.roots {
            let lower = root.to_ascii_lowercase();
            if SourceKind::from_path(root) == SourceKind::Dts {
                continue;
            }
            let ext = [".tsx", ".mts", ".cts", ".ts"]
                .into_iter()
                .find(|e| lower.ends_with(e));
            let Some(ext) = ext else {
                continue;
            };
            let Some(src) = input.fs.read_file(root) else {
                continue;
            };
            let js_ext = match ext {
                ".tsx" if preserve_jsx => ".jsx",
                ".mts" => ".mjs",
                ".cts" => ".cjs",
                _ => ".js",
            };
            let js = format!("{}{js_ext}", &root[..root.len() - ext.len()]);
            let js_name = path::file_name(&js).to_owned();
            let opts = TransformOptions::from_compiler_options(
                &c.options,
                root,
                commonjs_package(input.fs, root),
            );
            let codegen = CodegenOptions {
                layout: self.layout,
                newline: input.newline,
                source_map: (source_map || inline).then(|| SourceMapOptions {
                    file: Some(js_name.clone()),
                    source: path::file_name(root).to_owned(),
                    source_root: opt_str(c, OptionId::SourceRoot).map(str::to_owned),
                    include_content: opt_bool(c, OptionId::InlineSources),
                }),
                ..CodegenOptions::default()
            };
            let t = ts_r_transform::transpile(root, &src, &opts, &codegen);
            let mut text = t.code;
            if let Some(map) = &t.map {
                if inline {
                    text.push_str(&format!("//# sourceMappingURL={}", map.to_data_url()));
                } else {
                    let root_url = opt_str(c, OptionId::MapRoot)
                        .map(|r| format!("{}/", r.trim_end_matches('/')))
                        .unwrap_or_default();
                    text.push_str(&format!("//# sourceMappingURL={root_url}{js_name}.map"));
                    out.push(Output {
                        path: format!("{js}.map"),
                        text: map.to_json(),
                        map: None,
                    });
                }
            }
            out.push(Output {
                path: js,
                text,
                map: t.map,
            });
        }
        out
    }
}

/// E1's program emitter: `ts_r_emit::emit` over the loaded program with the checker as
/// its `EmitResolver`, as `ts-r-check` runs it.
struct ProgramEmitter;

fn libs() -> Option<&'static ts_r_checker::LibStore> {
    static LIBS: std::sync::OnceLock<Option<ts_r_checker::LibStore>> = std::sync::OnceLock::new();
    LIBS.get_or_init(ts_r_checker::LibStore::locate).as_ref()
}

/// `TSR2003: <construct>` from a "Lowering a <construct> for target ..." message (E5 cause
/// buckets), interned so outcomes can keep `&'static str` reasons.
fn lowering_reason(message: &str) -> &'static str {
    static SEEN: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let what = message
        .strip_prefix("Lowering a ")
        .and_then(|m| m.split(" for target").next())
        .unwrap_or("syntax");
    let reason = format!("TSR2003: {what} needs lowering (E5)");
    let Ok(mut seen) = SEEN.lock() else {
        return "TSR2003: syntax needs lowering (E5)";
    };
    if let Some(r) = seen.get(reason.as_str()) {
        return r;
    }
    let leaked: &'static str = Box::leak(reason.into_boxed_str());
    seen.insert(leaked);
    leaked
}

impl Emitter for ProgramEmitter {
    fn missing_reason(&self, kind: Kind) -> &'static str {
        match kind {
            Kind::Js => "not emitted",
            Kind::Dts => "not emitted",
            Kind::Map => "no source map for this output",
            Kind::Json => "JSON copy not emitted",
            Kind::Other => "not emitted",
        }
    }

    fn emit(&self, input: &EmitInput) -> Vec<Output> {
        self.emit_explained(input).0
    }

    fn emit_explained(&self, input: &EmitInput) -> (Vec<Output>, Reasons) {
        let mut options = input.config.options.clone();
        if input.newline == "\r\n" {
            options.set(
                OptionId::NewLine,
                Some(OptionValue::NewLine(NewLine::Crlf)),
                None,
            );
        }
        if !input.tsconfig {
            // The directives are options, not a config file (tsc's harness): no tsconfig
            // directory stands in for `rootDir` when resolving (TS2209/TS2210).
            options.config_file_path = None;
        }
        let program = ts_r_checker::load_program(input.fs, input.roots, &options, libs(), 1);
        let checker = ts_r_checker::Checker::new(&program);
        let resolver = ts_r_emit::CheckerResolver::new(&checker, &options);
        let host = ts_r_checker::typenode::specifier_host(input.fs, &options);
        let cfg = ts_r_emit::EmitConfig {
            options: &options,
            config: input.tsconfig.then_some(input.config),
            threads: 1,
            case_sensitive: input.fs.case_sensitive(),
            specifiers: Some(&host),
        };
        let result = ts_r_emit::emit(&program, &resolver, &cfg);
        // `EMIT_ORACLE_TSR=1` prints where declaration emit refused a type.
        if std::env::var_os("EMIT_ORACLE_TSR").is_some() {
            for d in &result.diagnostics {
                if d.code != ts_r_emit::dts::TSR_UNRESOLVED_TYPE {
                    continue;
                }
                let Some(f) = program.files.get(d.file.0 as usize) else {
                    continue;
                };
                let at = (d.span.start as usize).min(f.text.len());
                let line = f.text[..at].matches('\n').count() + 1;
                let snippet: String = f.text[at..].chars().take(60).collect();
                eprintln!("TSR2001 {}:{line}: {} | {snippet:?}", f.path, d.message);
            }
        }
        let dts_reason = result.diagnostics.iter().find_map(|d| match d.code {
            ts_r_emit::dts::TSR_UNRESOLVED_TYPE => Some("TSR2001: type not written"),
            ts_r_emit::dts::TSR_JS_DECLARATIONS => Some("TSR2006: JavaScript input"),
            _ => None,
        });
        let reason = result.diagnostics.iter().find_map(|d| match d.code {
            ts_r_emit::TSR_LOWERING if d.message.starts_with("Lowering") => {
                Some(lowering_reason(&d.message))
            }
            ts_r_emit::TSR_LOWERING => Some("TSR2003: target not supported"),
            ts_r_emit::TSR_RESOLVER_UNKNOWN => Some("TSR2002: resolver answered Unknown"),
            5055 => Some("TS5055: would overwrite an input"),
            _ => None,
        });
        let outputs = result
            .outputs
            .into_iter()
            .map(|o| Output {
                path: o.path,
                text: o.text,
                map: o.map,
            })
            .collect();
        (
            outputs,
            Reasons {
                js: reason,
                dts: dts_reason,
            },
        )
    }
}

fn upstream() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/upstream/typescript")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str())
            && matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs"
            )
        {
            out.push(p);
        }
    }
}

fn parse_directive(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("//")?.trim_start().strip_prefix('@')?;
    let (k, v) = rest.split_once(':')?;
    let k = k.trim_end();
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
}

/// The test's units and directives (like the checker conformance harness). As tsc's harness
/// does, a unit is its lines joined with `\n` (a `\r\n` file reaches the compiler with `\n`
/// line breaks, which show in text it copies: templates, JSX, line continuations), and its
/// leading empty lines are dropped.
fn split_test(text: &str, default_name: &str) -> (Pairs, Pairs) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut files: Pairs = Vec::new();
    let mut options = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur = String::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((k, v)) = parse_directive(line) {
            if k == "filename" {
                if let Some(n) = cur_name.take() {
                    files.push((n, std::mem::take(&mut cur)));
                } else {
                    cur.clear();
                }
                cur_name = Some(v);
            } else {
                options.push((k, v));
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    match cur_name {
        Some(n) => files.push((n, cur)),
        None => files.push((default_name.to_owned(), cur)),
    }
    (files, options)
}

/// The tsconfig `compilerOptions` members for the test's directives (last one wins).
fn options_json(opts: &[(String, String)]) -> String {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (k, v) in opts.iter().rev() {
        let Some(decl) = find_option_ignore_case(k) else {
            continue;
        };
        if !seen.insert(decl.name) {
            continue;
        }
        let first = v.split(',').next().unwrap_or("").trim();
        let val = match decl.ty {
            OptionType::Bool => match first.to_ascii_lowercase().as_str() {
                "true" => "true".to_owned(),
                "false" => "false".to_owned(),
                _ => continue,
            },
            OptionType::Number => match first.parse::<f64>() {
                Ok(_) => first.to_owned(),
                Err(_) => continue,
            },
            OptionType::StringList | OptionType::PathList | OptionType::Lib => {
                let items: Vec<String> = v
                    .split(',')
                    .map(|s| format!("{:?}", s.trim()))
                    .filter(|s| s != "\"\"")
                    .collect();
                format!("[{}]", items.join(","))
            }
            OptionType::PathMap | OptionType::Plugins => v.clone(),
            _ => format!("{first:?}"),
        };
        out.push(format!("{:?}: {val}", decl.name));
    }
    out.join(", ")
}

fn is_tsconfig(name: &str) -> bool {
    path::file_name(name).eq_ignore_ascii_case("tsconfig.json")
}

/// A `//// [name]` header line (not the `//// [test] ////` title, not a visualisation URL).
fn header_name(line: &str) -> Option<&str> {
    let t = line.trim_end_matches(['\r', '\n']);
    let inner = t.strip_prefix("//// [")?.strip_suffix(']')?;
    (!inner.ends_with("] ////")).then_some(inner)
}

/// The output sections of a `.js` baseline: the sections after the `inputs` input files, up
/// to `DtsFileErrors`. Sections of one group (JS, then declarations) are concatenated
/// directly; the harness puts an extra `\r\n\r\n` between the groups.
fn output_sections(text: &str, inputs: usize) -> Vec<(String, String)> {
    let mut heads: Vec<(usize, usize, &str)> = Vec::new();
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        // A declaration file ends at its `//# sourceMappingURL=` comment and the next
        // header follows on the same line (`...d.ts.map//// [b.d.ts]`).
        let start = match line.find("//// [") {
            Some(i) if i > 0 && line.starts_with("//# sourceMappingURL=") => i,
            _ => 0,
        };
        if let Some(n) = header_name(&line[start..]) {
            heads.push((pos + start, pos + line.len(), n));
        }
        pos += line.len();
    }
    let outs = heads.get(inputs..).unwrap_or(&[]);
    let mut out = Vec::new();
    for (i, &(_, body, name)) in outs.iter().enumerate() {
        if name == "DtsFileErrors" {
            break;
        }
        let next = outs.get(i + 1);
        let end = next.map_or(text.len(), |n| n.0);
        let mut content = &text[body..end];
        if let Some(&(_, _, next_name)) = next {
            let group_change = next_name == "DtsFileErrors"
                || (Kind::of(next_name) == Kind::Dts && Kind::of(name) != Kind::Dts);
            if group_change {
                content = content.strip_suffix("\r\n\r\n").unwrap_or(content);
            }
        }
        // tsc ends a file at its `//# sourceMappingURL=` comment, with no line break (observed
        // with tsc 7.0.2); the harness adds one so the next header starts a line.
        let body = content.trim_end_matches(['\r', '\n']);
        let last_line = body.rsplit('\n').next().unwrap_or(body);
        if last_line.starts_with("//# sourceMappingURL=") && body.len() < content.len() {
            let nl = &content[body.len()..];
            if nl == "\n" || nl == "\r\n" {
                content = body;
            }
        }
        out.push((name.to_owned(), content.to_owned()));
    }
    out
}

/// `//// [a.js.map]` sections of a `.js.map` baseline: name and JSON line.
fn map_sections(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.split_inclusive('\n');
    while let Some(line) = lines.next() {
        if let Some(n) = header_name(line) {
            let json = lines.next().unwrap_or("").trim_end_matches(['\r', '\n']);
            out.push((n.to_owned(), json.to_owned()));
        }
    }
    out
}

/// `N >Emitted(l, c) Source(l, c) + SourceIndex(i)` → segment.
fn parse_seg(line: &str) -> Option<Seg> {
    let rest = &line[line.find("Emitted(")? + "Emitted(".len()..];
    let (gen_part, rest) = rest.split_once(')')?;
    let (gl, gc) = gen_part.split_once(',')?;
    let (gl, gc) = (gl.trim().parse().ok()?, gc.trim().parse().ok()?);
    let source = rest.find("Source(").and_then(|i| {
        let r = &rest[i + "Source(".len()..];
        let (sp, r) = r.split_once(')')?;
        let (sl, sc) = sp.split_once(',')?;
        let r = &r[r.find("SourceIndex(")? + "SourceIndex(".len()..];
        let (si, _) = r.split_once(')')?;
        Some((
            si.trim().parse().ok()?,
            sl.trim().parse().ok()?,
            sc.trim().parse().ok()?,
        ))
    });
    Some((gl, gc, source))
}

/// The `emittedFile:` blocks of a `.sourcemap.txt` baseline with their segments.
fn sourcemap_txt(text: &str) -> Vec<(String, Vec<Seg>)> {
    let mut out: Vec<(String, Vec<Seg>)> = Vec::new();
    for line in text.lines() {
        if let Some(f) = line.strip_prefix("emittedFile:") {
            out.push((f.trim().to_owned(), Vec::new()));
        } else if line.contains(">Emitted(")
            && let Some(seg) = parse_seg(line)
            && let Some(last) = out.last_mut()
        {
            last.1.push(seg);
        }
    }
    for (_, segs) in &mut out {
        segs.sort();
        segs.dedup();
    }
    out
}

fn segs_of_mappings(mappings: &str) -> Vec<Seg> {
    let mut v: Vec<Seg> = decode_mappings(mappings)
        .into_iter()
        .map(|m| {
            (
                m.gen_line + 1,
                m.gen_col + 1,
                m.source.map(|(s, l, c, _)| (s, l + 1, c + 1)),
            )
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The `mappings` string of a source-map JSON text.
fn json_mappings(json: &str) -> Option<&str> {
    let i = json.find("\"mappings\":\"")? + "\"mappings\":\"".len();
    let end = json[i..].find('"')?;
    Some(&json[i..i + end])
}

/// `--show`: print expected and emitted text of outputs that are not byte-exact.
static SHOW: AtomicBool = AtomicBool::new(false);
/// `--show-maps`: print the segments of maps that differ (`-` tsc only, `+` ours only), with
/// the generated text up to the next segment and the source text at the segment.
static SHOW_MAPS: AtomicBool = AtomicBool::new(false);

fn explain_map(want: &[Seg], ours: &[Seg], js: &str, src: &str) {
    let gen_lines: Vec<&str> = js.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let srcl: Vec<&str> = src.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    // UTF-16 column to a byte offset in `line` (approximate for non-BMP text).
    let at = |line: &str, col: u32| -> usize {
        let mut n = 0u32;
        for (i, c) in line.char_indices() {
            if n >= col {
                return i;
            }
            n += c.len_utf16() as u32;
        }
        line.len()
    };
    let mut all: Vec<(Seg, char)> = Vec::new();
    for s in want {
        all.push((*s, if ours.contains(s) { ' ' } else { '-' }));
    }
    for s in ours {
        if !want.contains(s) {
            all.push((*s, '+'));
        }
    }
    all.sort();
    for (i, ((gl, gc, so), mark)) in all.iter().enumerate() {
        let line = gen_lines.get(*gl as usize - 1).copied().unwrap_or("");
        let next = all[i + 1..]
            .iter()
            .find(|((l, c, _), _)| *l == *gl && *c > *gc)
            .map(|((_, c, _), _)| *c);
        let from = at(line, gc - 1);
        let to = next.map_or(line.len(), |c| at(line, c - 1)).max(from);
        let src_at = so.map_or(String::new(), |(_, sl, sc)| {
            let l = srcl.get(sl as usize - 1).copied().unwrap_or("");
            let f = at(l, sc - 1);
            l[f..].chars().take(16).collect()
        });
        let so = so.map_or("-".to_owned(), |(_, l, c)| format!("{l}:{c}"));
        eprintln!(
            "{mark} {gl}:{gc} -> {so}  gen={:?}  src@={:?}",
            &line[from..to],
            src_at
        );
    }
}

static SKIPPED_NO_JS: AtomicUsize = AtomicUsize::new(0);
static SKIPPED_UNREADABLE: AtomicUsize = AtomicUsize::new(0);

fn read_text(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    match String::from_utf8(bytes) {
        Ok(s) => Some(s),
        Err(_) => {
            SKIPPED_UNREADABLE.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

struct Suite {
    tests: PathBuf,
    baselines: PathBuf,
    /// Area / name prefix (`tsc7/` for tsc 7's own tests).
    prefix: &'static str,
    names: BTreeSet<String>,
}

/// One test per `.js` baseline variant.
fn load(p: &Path, suite: &Suite) -> Vec<Test> {
    let (Some(stem), Some(file_name)) = (
        p.file_stem().and_then(|s| s.to_str()),
        p.file_name().and_then(|s| s.to_str()),
    ) else {
        return Vec::new();
    };
    let mut variants: BTreeSet<Option<String>> = BTreeSet::new();
    for n in suite.names.range(stem.to_owned()..) {
        if !n.starts_with(stem) {
            break;
        }
        let rest = &n[stem.len()..];
        if rest == ".js" {
            variants.insert(None);
        } else if let Some(v) = rest.strip_prefix('(').and_then(|v| v.strip_suffix(").js")) {
            variants.insert(Some(v.to_owned()));
        }
    }
    if variants.is_empty() {
        SKIPPED_NO_JS.fetch_add(1, Ordering::Relaxed);
        return Vec::new();
    }
    let Some(text) = read_text(p) else {
        return Vec::new();
    };
    let rel = p
        .strip_prefix(suite.tests.parent().unwrap_or(&suite.tests))
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let comps: Vec<String> = rel
        .iter()
        .filter_map(|c| c.to_str().map(str::to_owned))
        .collect();
    let area = match comps.first().map(String::as_str) {
        Some("conformance") if comps.len() > 3 => format!("{}/{}", comps[1], comps[2]),
        Some("conformance") if comps.len() > 2 => comps[1].clone(),
        Some(s) => s.to_owned(),
        None => String::new(),
    };
    let area = format!("{}{area}", suite.prefix);
    let (files, options) = split_test(&text, file_name);
    let inputs = files.iter().filter(|(n, _)| !is_tsconfig(n)).count();
    let mut out = Vec::new();
    for v in variants {
        let mut options = options.clone();
        let base = match &v {
            Some(v) => {
                for kv in v.split(',') {
                    if let Some((k, val)) = kv.split_once('=') {
                        options.push((k.trim().to_ascii_lowercase(), val.trim().to_owned()));
                    }
                }
                format!("{stem}({v})")
            }
            None => stem.to_owned(),
        };
        let Some(js) = read_text(&suite.baselines.join(format!("{base}.js"))) else {
            continue;
        };
        let mut expected: Vec<Expected> = output_sections(&js, inputs)
            .into_iter()
            .map(|(name, text)| Expected {
                kind: Kind::of(&name),
                name,
                text: Some(text),
                segs: None,
            })
            .collect();
        let mut maps: BTreeMap<String, Expected> = BTreeMap::new();
        if suite.names.contains(&format!("{base}.js.map"))
            && let Some(t) = read_text(&suite.baselines.join(format!("{base}.js.map")))
        {
            for (name, json) in map_sections(&t) {
                let segs = json_mappings(&json).map(segs_of_mappings);
                maps.insert(
                    name.clone(),
                    Expected {
                        name,
                        kind: Kind::Map,
                        text: Some(json),
                        segs,
                    },
                );
            }
        }
        if suite.names.contains(&format!("{base}.sourcemap.txt"))
            && let Some(t) = read_text(&suite.baselines.join(format!("{base}.sourcemap.txt")))
        {
            for (file, segs) in sourcemap_txt(&t) {
                let name = format!("{file}.map");
                maps.entry(name.clone())
                    .or_insert(Expected {
                        name,
                        kind: Kind::Map,
                        text: None,
                        segs: None,
                    })
                    .segs = Some(segs);
            }
        }
        expected.extend(maps.into_values());
        let name = match &v {
            Some(v) => format!("{}{}({v})", suite.prefix, rel.display()),
            None => format!("{}{}", suite.prefix, rel.display()),
        };
        out.push(Test {
            name,
            area: area.clone(),
            options,
            files: files.clone(),
            expected,
        });
    }
    out
}

/// The harness's shared test libraries (`tests/lib`), visible to tests as `/.lib/...`.
fn test_libs() -> &'static [(String, String)] {
    static LIBS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    LIBS.get_or_init(|| {
        let dir = upstream().join("tests/lib");
        let mut paths = Vec::new();
        collect(&dir, &mut paths);
        paths
            .iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(&dir).ok()?.to_str()?.replace('\\', "/");
                Some((format!("/.lib/{rel}"), std::fs::read_to_string(p).ok()?))
            })
            .collect()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Bytes,
    /// Shape-equal, with the layout cause.
    Shape(&'static str),
    /// Different, with the cause and the first differing lines (expected, ours).
    Differs(String, String, String),
    Missing(&'static str),
}

struct Outcome {
    name: String,
    area: String,
    es2022: bool,
    /// The effective target, for the E5 (below ES2022) rows.
    target: ScriptTarget,
    files: Vec<(String, Kind, Status)>,
    extra: Vec<(String, Kind)>,
    /// Map probe (program emitter, target below ES2022, a map baseline): the outputs of a
    /// second run at ES2022 with the original target's class-field semantics. Where tsc's
    /// output needed no lowering the JS is byte-exact and its map comparable; only the maps
    /// gate's all-targets line uses these.
    probe: Vec<(String, Kind, Status)>,
}

fn abs_name(n: &str) -> String {
    let n = n.trim_start_matches("./");
    if n.starts_with('/') {
        n.to_owned()
    } else {
        format!("/.src/{n}")
    }
}

/// Runs one test; for the program emitter, a test below ES2022 with a map baseline also gets
/// the map probe run ([`Outcome::probe`]).
fn run(test: &Test, emitter: &dyn Emitter, probe_maps: bool) -> Outcome {
    let mut outcome = run_with(test, emitter, false);
    if probe_maps && !outcome.es2022 && test.expected.iter().any(|e| e.kind == Kind::Map) {
        outcome.probe = run_with(test, emitter, true).files;
    }
    outcome
}

/// `at_es2022`: emit at ES2022, keeping the effective `module` and `useDefineForClassFields`
/// of the test's own target.
fn run_with(test: &Test, emitter: &dyn Emitter, at_es2022: bool) -> Outcome {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/.src");
    let mut roots = Vec::new();
    let mut tsconfig = None;
    for (name, src) in &test.files {
        let a = abs_name(name);
        fs.add_file(&a, src);
        if is_tsconfig(name) {
            tsconfig = Some(a);
            continue;
        }
        if a.to_ascii_lowercase().ends_with(".json") {
            continue;
        }
        roots.push(a);
    }
    if test.files.iter().any(|(_, src)| src.contains("/.lib/")) {
        for (p, src) in test_libs() {
            fs.add_file(p, src);
        }
    }
    for (k, v) in &test.options {
        if k == "link"
            && let Some((target, link)) = v.split_once("->")
        {
            fs.add_symlink(&abs_name(link.trim()), &abs_name(target.trim()));
        }
    }
    let co = options_json(&test.options);
    let mut config = match &tsconfig {
        // The test's tsconfig decides the files; the directives override its options.
        Some(t) => {
            let at = path::combine(path::directory_of(t), "__harness.tsconfig.json");
            fs.add_file(
                &at,
                &format!("{{\"extends\": {t:?}, \"compilerOptions\": {{{co}}}}}"),
            );
            let cfg = load_tsconfig(&fs, &at);
            roots = cfg.file_names.clone();
            cfg
        }
        None => {
            fs.add_file(
                "/.src/__tsconfig.json",
                &format!("{{\"compilerOptions\": {{{co}}}, \"files\": []}}"),
            );
            load_tsconfig(&fs, "/.src/__tsconfig.json")
        }
    };
    let newline = match config.options.get(OptionId::NewLine) {
        Some(OptionValue::NewLine(NewLine::Lf)) => "\n",
        _ => "\r\n",
    };
    let target = config.options.effective().target();
    let es2022 = target >= ScriptTarget::ES2022;
    if at_es2022 && !es2022 {
        let eff = config.options.effective();
        let (module, define) = (eff.module(), eff.use_define_for_class_fields());
        let o = &mut config.options;
        o.set(OptionId::Module, Some(OptionValue::Module(module)), None);
        o.set(
            OptionId::UseDefineForClassFields,
            Some(OptionValue::Bool(define)),
            None,
        );
        o.set(
            OptionId::Target,
            Some(OptionValue::Target(ScriptTarget::ES2022)),
            None,
        );
    }
    let (outputs, reasons) = emitter.emit_explained(&EmitInput {
        fs: &fs,
        config: &config,
        roots: &roots,
        newline,
        tsconfig: tsconfig.is_some(),
    });
    let full = test
        .options
        .iter()
        .any(|(k, v)| k == "fullemitpaths" && v.eq_ignore_ascii_case("true"));
    let shown = |p: &str| -> String {
        if full {
            p.strip_prefix("/.src/").unwrap_or(p).to_owned()
        } else {
            path::file_name(p).to_owned()
        }
    };
    // Match by shown name, positionally among equal names.
    let mut used = vec![false; outputs.len()];
    let mut files = Vec::new();
    for e in &test.expected {
        let found = outputs
            .iter()
            .enumerate()
            .position(|(i, o)| !used[i] && shown(&o.path) == e.name);
        let status = match e.kind {
            Kind::Map => {
                // A map is the `.map` file and the map of the output it belongs to.
                let file = found.map(|i| &outputs[i]);
                let owner = e.name.strip_suffix(".map").unwrap_or(&e.name);
                let owner_map = outputs
                    .iter()
                    .find(|o| shown(&o.path) == owner)
                    .and_then(|o| o.map.as_ref());
                if let Some(i) = found {
                    used[i] = true;
                }
                let status = compare_map(e, file.map(|o| o.text.as_str()), owner_map)
                    .unwrap_or(Status::Missing(emitter.missing_reason(Kind::Map)));
                if SHOW_MAPS.load(Ordering::Relaxed)
                    && matches!(status, Status::Differs(..))
                    && let (Some(want), Some(map), Some(js)) = (
                        e.segs.as_ref(),
                        owner_map,
                        outputs.iter().find(|o| shown(&o.path) == owner),
                    )
                {
                    let stem = path::file_name(owner)
                        .rsplit_once('.')
                        .map_or(owner, |(s, _)| s);
                    let src = test
                        .files
                        .iter()
                        .find(|(n, _)| {
                            let f = path::file_name(n);
                            f.rsplit_once('.').is_some_and(|(s, _)| s == stem)
                        })
                        .map_or("", |(_, t)| t.as_str());
                    eprintln!(
                        "==== map {} {}{}",
                        test.name,
                        e.name,
                        if at_es2022 { " (probe)" } else { "" }
                    );
                    explain_map(want, &segs_of_mappings(&map.mappings), &js.text, src);
                }
                status
            }
            _ => match found {
                Some(i) => {
                    used[i] = true;
                    let status = compare_text(e, &outputs[i].text);
                    if status != Status::Bytes && SHOW.load(Ordering::Relaxed) {
                        eprintln!(
                            "==== {} {}\n--- tsc\n{:?}\n--- ours\n{:?}",
                            test.name,
                            e.name,
                            e.text.as_deref().unwrap_or(""),
                            outputs[i].text
                        );
                    }
                    status
                }
                None => Status::Missing(match (e.kind, reasons.js, reasons.dts) {
                    (Kind::Js | Kind::Json, Some(r), _) => r,
                    (Kind::Dts, _, Some(r)) => r,
                    _ => emitter.missing_reason(e.kind),
                }),
            },
        };
        files.push((e.name.clone(), e.kind, status));
    }
    let extra = outputs
        .iter()
        .enumerate()
        .filter(|(i, _)| !used[*i])
        .map(|(_, o)| (shown(&o.path), Kind::of(&o.path)))
        .filter(|(n, k)| {
            // A map counted through its owner is not extra.
            !(*k == Kind::Map && test.expected.iter().any(|e| e.name == *n))
        })
        .collect();
    Outcome {
        name: test.name.clone(),
        area: test.area.clone(),
        es2022,
        target,
        files,
        extra,
        probe: Vec::new(),
    }
}

fn compare_map(e: &Expected, file: Option<&str>, owner: Option<&SourceMap>) -> Option<Status> {
    if let (Some(want), Some(got)) = (&e.text, file)
        && want == got
    {
        return Some(Status::Bytes);
    }
    let ours = owner
        .map(|m| segs_of_mappings(&m.mappings))
        .or_else(|| file.and_then(json_mappings).map(segs_of_mappings))?;
    let want = e.segs.as_ref()?;
    if *want == ours {
        // Inline maps have no JSON baseline: equal segments are all there is to compare.
        Some(if e.text.is_none() {
            Status::Bytes
        } else {
            Status::Shape("map fields")
        })
    } else {
        let first = want
            .iter()
            .zip(&ours)
            .find(|(a, b)| a != b)
            .map(|(a, b)| (format!("{a:?}"), format!("{b:?}")))
            .unwrap_or_else(|| {
                (
                    format!("{} segments", want.len()),
                    format!("{} segments", ours.len()),
                )
            });
        Some(Status::Differs("mappings".to_owned(), first.0, first.1))
    }
}

fn shape_of(name: &str, text: &str) -> String {
    let kind = match Kind::of(name) {
        Kind::Dts => SourceKind::Dts,
        _ => SourceKind::Js,
    };
    let parsed = ts_r_parser::parse(text, kind);
    ts_r_codegen::shape::shape(&parsed.ast)
}

fn compare_text(e: &Expected, got: &str) -> Status {
    let Some(want) = &e.text else {
        return Status::Missing("no expected text");
    };
    if want == got {
        return Status::Bytes;
    }
    if matches!(e.kind, Kind::Js | Kind::Dts) && shape_of(&e.name, want) == shape_of(&e.name, got) {
        return Status::Shape(layout_cause(want, got));
    }
    let (cause, a, b) = diff_cause(want, got, e.kind);
    Status::Differs(cause, a, b)
}

/// Why two shape-equal texts differ.
fn layout_cause(want: &str, got: &str) -> &'static str {
    let lf = |s: &str| s.replace("\r\n", "\n");
    let (w, g) = (lf(want), lf(got));
    if w == g {
        return "line endings";
    }
    if w.trim_end() == g.trim_end() {
        return "end of file";
    }
    let non_blank = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect()
    };
    if non_blank(&w) == non_blank(&g) {
        return "blank lines";
    }
    let trimmed = |s: &str| -> Vec<String> {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    };
    if trimmed(&w) == trimmed(&g) {
        return "indentation";
    }
    let no_ws = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    if no_ws(&w) == no_ws(&g) {
        return "line breaks and spacing";
    }
    "comments or tokens"
}

/// The characters of `s` that matter for a difference (no whitespace or comments outside
/// strings), each with the index of the line it is on.
fn significant(s: &str) -> Vec<(char, usize)> {
    let mut out = Vec::with_capacity(s.len());
    let mut line = 0usize;
    let mut chars = s.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
        }
        if let Some(q) = quote {
            out.push((c, line));
            if c == '\\' {
                if let Some(n) = chars.next() {
                    if n == '\n' {
                        line += 1;
                    }
                    out.push((n, line));
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => {
                quote = Some(c);
                out.push((c, line));
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if n == '\n' {
                        line += 1;
                    }
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            c if c.is_whitespace() => {}
            c => out.push((c, line)),
        }
    }
    out
}

/// The cause of a difference: the construct where the code (whitespace and comments
/// aside) first differs, plus the expected and emitted lines there.
fn diff_cause(want: &str, got: &str, kind: Kind) -> (String, String, String) {
    let (w, g) = (significant(want), significant(got));
    let i = w.iter().zip(&g).take_while(|(a, b)| a.0 == b.0).count();
    let line_at = |text: &str, sig: &[(char, usize)]| -> Option<String> {
        let (_, l) = sig.get(i).or_else(|| sig.last())?;
        text.lines().nth(*l).map(|s| s.trim().to_owned())
    };
    let (a, b) = (
        (i < w.len()).then(|| line_at(want, &w)).flatten(),
        (i < g.len()).then(|| line_at(got, &g)).flatten(),
    );
    let construct = if kind == Kind::Dts {
        dts_construct
    } else {
        construct
    };
    let cause = match (&a, &b) {
        (None, Some(_)) => "extra code at the end".to_owned(),
        (Some(l), None) => format!("missing code: {}", construct(l, "")),
        (Some(l), Some(o)) => construct(l, o),
        (None, None) => "comments only".to_owned(),
    };
    let cut = |s: Option<String>| {
        s.unwrap_or_else(|| "<end>".to_owned())
            .chars()
            .take(160)
            .collect::<String>()
    };
    (cause, cut(a), cut(b))
}

/// What differs in a declaration file, from tsc's line `l` and ours `o`.
fn dts_construct(l: &str, o: &str) -> String {
    let either = |p: &dyn Fn(&str) -> bool| p(l) || p(o);
    if either(&|s: &str| s.starts_with("/*") || s.starts_with('*') || s.starts_with("//")) {
        return "comments".to_owned();
    }
    if either(&|s: &str| s.starts_with("import ")) {
        return "imports".to_owned();
    }
    if either(&|s: &str| s == "export {};") {
        return "export {}".to_owned();
    }
    if either(&|s: &str| {
        s.starts_with("export {")
            || s.starts_with("export =")
            || s.starts_with("export default ")
            || s.starts_with("export *")
            || s.starts_with("export as ")
    }) {
        return "export statements".to_owned();
    }
    if either(&|s: &str| s.contains("_default") || s.contains("_base")) {
        return "synthesized declarations".to_owned();
    }
    if either(&|s: &str| s.contains("#private")) {
        return "#private".to_owned();
    }
    // Same declaration head, different type: an inferred or copied type.
    let head = |s: &str| -> String {
        let cut = s.find([':', '=', '(', '<']).unwrap_or(s.len());
        s[..cut].trim().to_owned()
    };
    if !l.is_empty() && head(l) == head(o) && !head(l).is_empty() {
        if either(&|s: &str| s.contains("import(")) {
            return "types: import types".to_owned();
        }
        if either(&|s: &str| s.contains("typeof ")) {
            return "types: typeof".to_owned();
        }
        if either(&|s: &str| s.contains("=>")) {
            return "types: functions".to_owned();
        }
        if either(&|s: &str| s.ends_with('{')) {
            return "types: object literals".to_owned();
        }
        if either(&|s: &str| s.contains(" | ")) {
            return "types: unions".to_owned();
        }
        if either(&|s: &str| s.contains(" = ")) {
            return "types: literal initializers".to_owned();
        }
        if either(&|s: &str| s.contains('?')) {
            return "types: optionality".to_owned();
        }
        return "types: other".to_owned();
    }
    let word = |s: &str| -> String {
        s.split_whitespace()
            .find(|w| {
                !matches!(
                    *w,
                    "export"
                        | "declare"
                        | "default"
                        | "abstract"
                        | "public"
                        | "private"
                        | "protected"
                        | "static"
                        | "readonly"
                        | "const"
                        | "async"
                )
            })
            .unwrap_or("")
            .to_owned()
    };
    if word(l) == word(o) {
        return "modifiers".to_owned();
    }
    let kind = |s: &str| -> &'static str {
        let w = word(s);
        match w.as_str() {
            "class" => "classes",
            "interface" => "interfaces",
            "namespace" | "module" | "global" => "namespaces",
            "enum" => "enums",
            "function" => "functions",
            "type" => "type aliases",
            "var" | "let" => "variables",
            "}" => "end of block",
            _ => "members",
        }
    };
    format!("declarations: {} / ours {}", kind(l), kind(o))
}

/// What differs, from tsc's line `l` and ours `o` at the first difference.
fn construct(l: &str, o: &str) -> String {
    let has = |s: &str| l.contains(s);
    let ours = |s: &str| o.contains(s);
    if has("__decorate")
        || has("__metadata")
        || has("__param(")
        || has("__esDecorate")
        || has("__runInitializers")
    {
        return "decorators".to_owned();
    }
    if let Some(i) = l.find("__") {
        let name: String = l[i..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.len() > 3
            && (has("var ") || has("this && this.") || has("(0, ") || has(&format!("{name}(")))
        {
            return format!("helper {name}");
        }
    }
    if ours("static {") && !has("static {") {
        return "lowering: class static blocks".to_owned();
    }
    if (has("=== null ||") || has("void 0 ?") || has("!== null &&")) && !ours("=== null") {
        return "lowering: optional chaining / nullish".to_owned();
    }
    if has("Math.pow(") {
        return "lowering: exponentiation".to_owned();
    }
    if (ours("async ") && !has("async ")) || (ours("await") && has("yield")) {
        return "lowering: async functions".to_owned();
    }
    if has("var _") || has("let _") || (has("_a") && !ours("_a")) {
        return "lowering: temporaries".to_owned();
    }
    if (has("/* ") && has(" */") && !ours("/* ")) || (ours("/* ") && ours(" */") && !has("/* ")) {
        return "const enum inlining".to_owned();
    }
    if has("\"use strict\"") || ours("\"use strict\"") {
        return "\"use strict\"".to_owned();
    }
    if has("sourceMappingURL") || ours("sourceMappingURL") {
        return "sourceMappingURL".to_owned();
    }
    if has("require(")
        || has("exports.")
        || has("Object.defineProperty(exports")
        || has("module.exports")
        || ours("require(")
        || ours("exports.")
    {
        return "CommonJS".to_owned();
    }
    if (has(" || (") && has(" = {}))")) || has("(function (") {
        return "enum/namespace IIFE".to_owned();
    }
    if has("createElement")
        || has("_jsx")
        || has("jsx(")
        || l.starts_with('<')
        || o.starts_with('<')
    {
        return "JSX".to_owned();
    }
    if l.eq_ignore_ascii_case(o) {
        return "literal spelling".to_owned();
    }
    if l.starts_with("import ")
        || l.starts_with("export ")
        || o.starts_with("import ")
        || o.starts_with("export ")
    {
        return "import/export".to_owned();
    }
    if l.starts_with("declare ") || has(" declare ") {
        return "declarations".to_owned();
    }
    if has("class ") || ours("class ") || has("constructor(") || ours("constructor(") {
        return "classes".to_owned();
    }
    if has("=>") || ours("=>") {
        return "arrow functions".to_owned();
    }
    "other".to_owned()
}

/// files, bytes, shape (bytes included), differs, missing, extra
type Counts = [u32; 6];

fn add(c: &mut Counts, s: &Status) {
    c[0] += 1;
    match s {
        Status::Bytes => {
            c[1] += 1;
            c[2] += 1;
        }
        Status::Shape(_) => c[2] += 1,
        Status::Differs(..) => c[3] += 1,
        Status::Missing(_) => c[4] += 1,
    }
}

fn row(label: &str, c: &Counts) -> String {
    let pct = |n: u32| {
        if c[0] == 0 {
            0.0
        } else {
            100.0 * n as f64 / c[0] as f64
        }
    };
    format!(
        "  {label}: {} / {} ({:.1}%) / {} ({:.1}%) / {} / {} / {}",
        c[0],
        c[1],
        pct(c[1]),
        c[2],
        pct(c[2]),
        c[3],
        c[4],
        c[5]
    )
}

fn main() {
    let mut verbose = false;
    let mut filters: Vec<String> = Vec::new();
    let mut threads = 4usize;
    let mut emitter_name = "check".to_owned();
    let mut write_gaps = false;
    let mut layout = ts_r_codegen::Layout::Readable;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--verbose" => verbose = true,
            "--show" => SHOW.store(true, Ordering::Relaxed),
            "--show-maps" => SHOW_MAPS.store(true, Ordering::Relaxed),
            "--threads" => {
                threads = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(4)
                    .clamp(1, 4)
            }
            "--emitter" => emitter_name = args.next().unwrap_or_default(),
            "--write-gaps" => write_gaps = true,
            "--layout" => {
                layout = match args.next().as_deref() {
                    Some("tsc") => ts_r_codegen::Layout::Tsc,
                    _ => ts_r_codegen::Layout::Readable,
                }
            }
            "--filter-file" => {
                if let Some(text) = args.next().and_then(|p| std::fs::read_to_string(p).ok()) {
                    filters.extend(text.split_whitespace().map(str::to_owned));
                }
            }
            _ => filters.push(a),
        }
    }
    let transpile = TranspileEmitter { layout };
    let emitter: &dyn Emitter = match emitter_name.as_str() {
        "check" => &CheckEmitter,
        "transpile" => &transpile,
        "program" => &ProgramEmitter,
        other => {
            eprintln!("unknown emitter '{other}': expected check, transpile or program");
            std::process::exit(2);
        }
    };
    let root = upstream();
    let ref_dir = root.join("tsc/testdata/baselines/reference");
    let specs: [(PathBuf, PathBuf, &'static str); 4] = [
        (
            root.join("tests/cases/compiler"),
            ref_dir.join("submodule/compiler"),
            "",
        ),
        (
            root.join("tests/cases/conformance"),
            ref_dir.join("submodule/conformance"),
            "",
        ),
        (
            root.join("tsc/testdata/tests/cases/compiler"),
            ref_dir.join("compiler"),
            "tsc7/",
        ),
        (
            root.join("tsc/testdata/tests/cases/conformance"),
            ref_dir.join("conformance"),
            "tsc7/",
        ),
    ];
    let mut suites = Vec::new();
    let mut paths: Vec<(usize, PathBuf)> = Vec::new();
    for (tests, baselines, prefix) in specs {
        let names: BTreeSet<String> = std::fs::read_dir(&baselines)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut v = Vec::new();
        collect(&tests, &mut v);
        for p in v {
            let s = format!("{prefix}{}", p.display());
            if filters.is_empty() || filters.iter().any(|f| s.contains(f.as_str())) {
                paths.push((suites.len(), p));
            }
        }
        suites.push(Suite {
            tests,
            baselines,
            prefix,
            names,
        });
    }
    if paths.is_empty() {
        eprintln!("no tests found; run `cargo xtask upstream sync --minimal`");
        std::process::exit(2);
    }
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let start = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            let b = std::thread::Builder::new().stack_size(256 << 20);
            let _ = b.spawn_scoped(s, || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((suite, p)) = paths.get(i) else {
                        break;
                    };
                    for test in load(p, &suites[*suite]) {
                        let outcome = run(&test, emitter, emitter_name == "program");
                        if let Ok(mut r) = results.lock() {
                            r.push(outcome);
                        }
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_default();
    results.sort_by(|a, b| a.name.cmp(&b.name));

    // (row, kind) → counts; kind None = every kind.
    let mut rows: BTreeMap<(String, Option<Kind>), Counts> = BTreeMap::new();
    let mut causes: BTreeMap<(Kind, String), u32> = BTreeMap::new();
    let mut exact_tests = 0u32;
    let mut with_outputs = 0u32;
    let mut report = String::new();
    for o in &results {
        let top = o.area.split('/').next().unwrap_or("").to_owned();
        let mut groups = vec!["ALL".to_owned(), o.area.clone(), format!("{top}/*")];
        if o.es2022 {
            groups.push("ES2022+".to_owned());
        }
        for (_, kind, status) in &o.files {
            for g in &groups {
                add(rows.entry((g.clone(), Some(*kind))).or_default(), status);
                add(rows.entry((g.clone(), None)).or_default(), status);
            }
            let cause = match status {
                Status::Bytes => None,
                Status::Shape(c) => Some(format!("shape-equal: {c}")),
                Status::Differs(c, _, _) => Some(format!("differs: {c}")),
                Status::Missing(r) => Some(format!("missing: {r}")),
            };
            if let Some(c) = cause {
                *causes.entry((*kind, c)).or_default() += 1;
            }
        }
        for (_, kind) in &o.extra {
            for g in &groups {
                rows.entry((g.clone(), Some(*kind))).or_default()[5] += 1;
                rows.entry((g.clone(), None)).or_default()[5] += 1;
            }
            *causes
                .entry((*kind, "extra output".to_owned()))
                .or_default() += 1;
        }
        if !o.files.is_empty() {
            with_outputs += 1;
        }
        let exact = o.extra.is_empty() && o.files.iter().all(|(_, _, s)| *s == Status::Bytes);
        if exact && !o.files.is_empty() {
            exact_tests += 1;
        }
        if !exact {
            let tag = if o.es2022 { " [ES2022+]" } else { "" };
            report.push_str(&format!("== {}{tag}\n", o.name));
            for (name, kind, status) in &o.files {
                let k = kind.label();
                match status {
                    Status::Bytes => {}
                    Status::Shape(c) => report.push_str(&format!("  shape {k} {name} ({c})\n")),
                    Status::Differs(c, a, b) => report.push_str(&format!(
                        "  differs {k} {name} ({c})\n    - {a}\n    + {b}\n"
                    )),
                    Status::Missing(r) => report.push_str(&format!("  missing {k} {name} ({r})\n")),
                }
            }
            for (name, kind) in &o.extra {
                report.push_str(&format!("  extra {} {name}\n", kind.label()));
            }
            for (name, kind, status) in &o.probe {
                let k = kind.label();
                match status {
                    Status::Bytes => report.push_str(&format!("  probe bytes {k} {name}\n")),
                    Status::Shape(c) => {
                        report.push_str(&format!("  probe shape {k} {name} ({c})\n"))
                    }
                    Status::Differs(c, a, b) => report.push_str(&format!(
                        "  probe differs {k} {name} ({c})\n    - {a}\n    + {b}\n"
                    )),
                    Status::Missing(r) => {
                        report.push_str(&format!("  probe missing {k} {name} ({r})\n"))
                    }
                }
            }
        }
    }
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/emit-oracle.txt");
    let _ = std::fs::write(&out, &report);
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(
        stdout,
        "emitter {emitter_name}: {} tests ({with_outputs} with outputs, {exact_tests} all bytes-exact) in {:.1}s (skipped: {} without a .js baseline, {} unreadable baselines)",
        results.len(),
        start.elapsed().as_secs_f64(),
        SKIPPED_NO_JS.load(Ordering::Relaxed),
        SKIPPED_UNREADABLE.load(Ordering::Relaxed),
    );
    let _ = writeln!(
        stdout,
        "row: outputs / bytes-exact / shape-equal / differ / missing / extra"
    );
    for group in ["ALL", "ES2022+"] {
        if let Some(c) = rows.get(&(group.to_owned(), None)) {
            let _ = writeln!(stdout, "{}", row(group, c));
        }
        for k in Kind::ALL {
            if let Some(c) = rows.get(&(group.to_owned(), Some(k))) {
                let _ = writeln!(stdout, "{}", row(&format!("{group} {}", k.label()), c));
            }
        }
    }
    let _ = writeln!(stdout, "areas (all kinds):");
    for ((g, k), c) in &rows {
        if k.is_some() || g == "ALL" || g == "ES2022+" {
            continue;
        }
        // Top-level rows always; smaller areas with --verbose or a filter.
        if !g.ends_with("/*") && c[0] < 50 && filters.is_empty() && !verbose {
            continue;
        }
        let _ = writeln!(stdout, "{}", row(g, c));
    }
    let _ = writeln!(stdout, "causes (outputs not bytes-exact):");
    for k in Kind::ALL {
        let mut v: Vec<(&String, &u32)> = causes
            .iter()
            .filter(|((kind, _), _)| *kind == k)
            .map(|((_, c), n)| (c, n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let limit = if verbose { usize::MAX } else { 12 };
        for (c, n) in v.into_iter().take(limit) {
            let _ = writeln!(stdout, "  {} {c}: {n}", k.label());
        }
    }
    if emitter_name == "program" {
        gates(&mut stdout, &results, filters.is_empty(), write_gaps);
        downlevel_gates(&mut stdout, &results);
        dts_gates(&mut stdout, &results);
    }
    let _ = writeln!(stdout, "mismatches: {}", out.display());
}

/// Record 0012's E5 gate: shape-equal JavaScript on the `(target=es20xx)` variants below
/// ES2022 (gate 95%), plus every output below ES2022 and each target on its own.
fn downlevel_gates(out: &mut impl std::io::Write, results: &[Outcome]) {
    let mut rows: BTreeMap<String, [u32; 3]> = BTreeMap::new();
    for o in results.iter().filter(|o| !o.es2022) {
        let variant = o.name.contains("target=es20");
        let year = format!("  {:?}", o.target).to_ascii_lowercase();
        for (_, kind, status) in &o.files {
            if *kind != Kind::Js {
                continue;
            }
            let mut groups = vec!["below ES2022 (all)".to_owned(), year.clone()];
            if variant {
                groups.push("(target=es20xx) variants".to_owned());
            }
            for g in groups {
                let c = rows.entry(g).or_default();
                c[0] += 1;
                match status {
                    Status::Bytes => {
                        c[1] += 1;
                        c[2] += 1;
                    }
                    Status::Shape(_) => c[2] += 1,
                    _ => {}
                }
            }
        }
    }
    let pct = |a: u32, b: u32| 100.0 * f64::from(a) / f64::from(b.max(1));
    let _ = writeln!(
        out,
        "E5 gates (JavaScript below ES2022: outputs / bytes / shape):"
    );
    let mut keys: Vec<&String> = rows.keys().collect();
    keys.sort_by_key(|k| (k.starts_with("  "), k.as_str()));
    for k in keys {
        let c = rows[k];
        let gate = if k.starts_with('(') {
            "; gate shape 95%"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {k}: {} / {} ({:.1}%) / {} ({:.1}%{gate})",
            c[0],
            c[1],
            pct(c[1], c[0]),
            c[2],
            pct(c[2], c[0])
        );
    }
}

/// Record 0012's E2 gate: declaration outputs bytes-exact, per output and per baseline
/// (a baseline counts when every one of its declaration outputs is bytes-exact and no
/// declaration file was written that tsc refused, for example after TS2883).
fn dts_gates(out: &mut impl std::io::Write, results: &[Outcome]) {
    let (mut outputs, mut exact, mut shape, mut extra) = (0u32, 0u32, 0u32, 0u32);
    let (mut baselines, mut baselines_exact) = (0u32, 0u32);
    // Baselines with a JavaScript input's declarations (JSDoc declaration emit, outside
    // record 0012: TSR2006).
    let mut js_baselines = 0u32;
    let mut missing: BTreeMap<&str, u32> = BTreeMap::new();
    for o in results {
        let dts: Vec<&Status> = o
            .files
            .iter()
            .filter(|(_, k, _)| *k == Kind::Dts)
            .map(|(_, _, s)| s)
            .collect();
        let extra_dts = o.extra.iter().filter(|(_, k)| *k == Kind::Dts).count() as u32;
        if dts.is_empty() && extra_dts == 0 {
            continue;
        }
        extra += extra_dts;
        baselines += 1;
        if dts
            .iter()
            .any(|s| matches!(s, Status::Missing(r) if r.starts_with("TSR2006")))
        {
            js_baselines += 1;
        }
        if extra_dts == 0 && dts.iter().all(|s| **s == Status::Bytes) {
            baselines_exact += 1;
        }
        for s in dts {
            outputs += 1;
            match s {
                Status::Bytes => {
                    exact += 1;
                    shape += 1;
                }
                Status::Shape(_) => shape += 1,
                Status::Missing(r) => *missing.entry(r).or_default() += 1,
                Status::Differs(..) => {}
            }
        }
    }
    let pct = |n: u32, d: u32| {
        if d == 0 {
            0.0
        } else {
            100.0 * n as f64 / d as f64
        }
    };
    let _ = writeln!(out, "E2 gates (declaration outputs):");
    let _ = writeln!(
        out,
        "  baselines: {baselines_exact} / {baselines} with every .d.ts bytes-exact ({:.1}%; gate 85%)",
        pct(baselines_exact, baselines)
    );
    let ts_baselines = baselines - js_baselines;
    let _ = writeln!(
        out,
        "  baselines without JavaScript inputs: {baselines_exact} / {ts_baselines} ({:.1}%)",
        pct(baselines_exact, ts_baselines)
    );
    let _ = writeln!(
        out,
        "  outputs: {exact} / {outputs} bytes-exact ({:.1}%), {shape} shape-equal ({:.1}%)",
        pct(exact, outputs),
        pct(shape, outputs)
    );
    for (r, n) in missing {
        let _ = writeln!(out, "  missing: {r}: {n}");
    }
    let _ = writeln!(out, "  extra (tsc wrote none): {extra}");
    // Declaration maps (`.d.ts.map`): on every `.d.ts` output with a map baseline, and on
    // the bytes-exact ones (where segments can be compared position for position).
    // Declarations do not depend on the target, so every test's own run counts.
    let (mut maps, mut maps_equal, mut maps_exact_owner, mut maps_exact_equal) =
        (0u32, 0u32, 0u32, 0u32);
    let mut misses: Vec<(&str, &str)> = Vec::new();
    for o in results {
        for (name, kind, status) in &o.files {
            let Some(owner) = name.strip_suffix(".map") else {
                continue;
            };
            if *kind != Kind::Map || Kind::of(owner) != Kind::Dts {
                continue;
            }
            let equal = matches!(status, Status::Bytes | Status::Shape(_));
            maps += 1;
            maps_equal += u32::from(equal);
            if o.files
                .iter()
                .any(|(n, k, s)| n == owner && *k == Kind::Dts && *s == Status::Bytes)
            {
                maps_exact_owner += 1;
                maps_exact_equal += u32::from(equal);
                if !equal {
                    misses.push((&o.name, name));
                }
            }
        }
    }
    let _ = writeln!(
        out,
        "  declaration maps: segments equal on {maps_exact_equal} of {maps_exact_owner} bytes-exact .d.ts outputs with a map baseline ({:.1}%); {maps_equal} / {maps} over all",
        pct(maps_exact_equal, maps_exact_owner)
    );
    for (t, f) in misses.iter().take(10) {
        let _ = writeln!(out, "    differs: {t} {f}");
    }
}

/// The file listing the ES2022+ JavaScript outputs the program emitter does not get
/// shape-equal, one `test<TAB>output<TAB>cause` per line. It may only shrink.
fn known_gaps_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/emit_known_gaps.txt")
}

fn status_cause(s: &Status) -> String {
    match s {
        Status::Bytes => "bytes".to_owned(),
        Status::Shape(c) => format!("shape-equal: {c}"),
        Status::Differs(c, _, _) => format!("differs: {c}"),
        Status::Missing(r) => format!("missing: {r}"),
    }
}

/// Record 0012's E1 gates over the supported set (ES2022+ JavaScript outputs): shape-equal
/// on every output not listed in `KNOWN_GAPS`, the bytes-exact rate, and source-map segments
/// equal on the bytes-exact outputs that have a map baseline. With `--write-gaps` (a full
/// run) the gaps file is rewritten from this run.
fn gates(out: &mut impl std::io::Write, results: &[Outcome], full: bool, write: bool) {
    let listed: BTreeSet<(String, String)> = std::fs::read_to_string(known_gaps_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some((it.next()?.to_owned(), it.next()?.to_owned()))
        })
        .collect();
    let (mut total, mut bytes, mut shape) = (0u32, 0u32, 0u32);
    let mut gaps: Vec<(String, String, String)> = Vec::new();
    let (mut mapped, mut segs_equal) = (0u32, 0u32);
    for o in results.iter().filter(|o| o.es2022) {
        for (name, kind, status) in &o.files {
            if *kind == Kind::Js {
                total += 1;
                match status {
                    Status::Bytes => {
                        bytes += 1;
                        shape += 1;
                    }
                    Status::Shape(_) => shape += 1,
                    _ => gaps.push((o.name.clone(), name.clone(), status_cause(status))),
                }
            }
            if *kind == Kind::Map
                && let Some(owner) = name.strip_suffix(".map")
                && o.files
                    .iter()
                    .any(|(n, k, s)| n == owner && *k == Kind::Js && *s == Status::Bytes)
            {
                mapped += 1;
                if matches!(status, Status::Bytes | Status::Shape(_)) {
                    segs_equal += 1;
                }
            }
        }
    }
    // Maps on every bytes-exact output with a map baseline: ES2022+ tests as emitted, earlier
    // targets through the map probe (emitted at ES2022; byte-exact where tsc lowered nothing).
    let (mut mapped_all, mut segs_all) = (0u32, 0u32);
    let mut map_misses: Vec<(String, String)> = Vec::new();
    for o in results {
        let files = if o.es2022 { &o.files } else { &o.probe };
        for (name, kind, status) in files {
            if *kind == Kind::Map
                && let Some(owner) = name.strip_suffix(".map")
                && files
                    .iter()
                    .any(|(n, k, s)| n == owner && *k == Kind::Js && *s == Status::Bytes)
            {
                mapped_all += 1;
                if matches!(status, Status::Bytes | Status::Shape(_)) {
                    segs_all += 1;
                } else {
                    map_misses.push((o.name.clone(), name.clone()));
                }
            }
        }
    }
    let pct = |a: u32, b: u32| 100.0 * f64::from(a) / f64::from(b.max(1));
    let _ = writeln!(out, "E1 gates (ES2022+ JavaScript, {total} outputs):");
    let unlisted: Vec<&(String, String, String)> = gaps
        .iter()
        .filter(|(t, f, _)| !listed.contains(&(t.clone(), f.clone())))
        .collect();
    let stale = listed
        .iter()
        .filter(|(t, f)| !gaps.iter().any(|(gt, gf, _)| gt == t && gf == f))
        .count();
    let _ = writeln!(
        out,
        "  shape: {shape} ({:.1}%); {} not shape-equal, {} in KNOWN_GAPS, {} unlisted{}",
        pct(shape, total),
        gaps.len(),
        gaps.len() - unlisted.len(),
        unlisted.len(),
        if full && stale > 0 {
            format!(", {stale} listed gaps now pass (shrink the list)")
        } else {
            String::new()
        }
    );
    for (t, f, c) in unlisted.iter().take(20) {
        let _ = writeln!(out, "    unlisted: {t} {f} ({c})");
    }
    let _ = writeln!(
        out,
        "  bytes: {bytes} ({:.1}%; gate 90%)",
        pct(bytes, total)
    );
    let _ = writeln!(
        out,
        "  maps: segments equal on {segs_equal} of {mapped} bytes-exact outputs with a map baseline ({:.1}%; gate 90%)",
        pct(segs_equal, mapped)
    );
    let _ = writeln!(
        out,
        "  maps, all targets: segments equal on {segs_all} of {mapped_all} bytes-exact outputs with a map baseline ({:.1}%; earlier targets emitted at ES2022)",
        pct(segs_all, mapped_all)
    );
    for (t, f) in map_misses.iter().take(if full { 0 } else { 20 }) {
        let _ = writeln!(out, "    map differs: {t} {f}");
    }
    if write && full {
        let mut text = String::from(
            "# ES2022+ JavaScript outputs the program emitter does not get shape-equal\n\
             # (record 0012, E1 gate). test<TAB>output<TAB>cause. Regenerate with a full\n\
             # `emit_oracle --emitter program --write-gaps` run; the list may only shrink.\n",
        );
        for (t, f, c) in &gaps {
            text.push_str(&format!("{t}\t{f}\t{c}\n"));
        }
        let _ = std::fs::write(known_gaps_path(), text);
    }
}

#[cfg(test)]
mod tests {
    use super::split_test;

    /// Units reach the compiler as tsc's harness gives them: `\n` line breaks, leading empty
    /// lines dropped, a whitespace-only first line kept, no line break before the next unit.
    #[test]
    fn units_are_joined_with_lf() {
        let (files, options) = split_test(
            "// @target: es2015\r\n\r\n`a\r\nb`;\r\n// @filename: b.ts\r\n  \r\nx;\r\n",
            "a.ts",
        );
        assert_eq!(options, vec![("target".to_owned(), "es2015".to_owned())]);
        assert_eq!(files.len(), 1, "{files:?}");
        let (files, _) = split_test("\r\n\r\n`a\r\nb`;\r\n", "a.ts");
        assert_eq!(files, vec![("a.ts".to_owned(), "`a\nb`;\n".to_owned())]);
        let (files, _) = split_test(
            "// @filename: a.ts\r\n\r\nx;\r\n// @filename: b.ts\r\n  \r\ny;",
            "t.ts",
        );
        assert_eq!(
            files,
            vec![
                ("a.ts".to_owned(), "x;".to_owned()),
                ("b.ts".to_owned(), "  \ny;".to_owned()),
            ]
        );
    }
}
