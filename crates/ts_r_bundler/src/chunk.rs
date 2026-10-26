//! Chunk assembly: wraps printed fragments with the chunk's imports, runtime helpers,
//! namespace objects, entry exports and format wrapper, and concatenates source maps.

use std::borrow::Cow;
use std::sync::Arc;

use crate::expr::Expr;
use crate::graph::{Graph, ModuleId};
use crate::options::{BuildOptions, Format};
use crate::plan::{Plan, chunk_import};
use crate::print::Fragment;
use crate::sourcemap::{ChunkMap, encode_fragment};

/// One assembled chunk.
#[derive(Debug, Default)]
pub struct ChunkOutput {
    pub code: String,
    /// Source map JSON (when enabled).
    pub map: Option<String>,
    /// Bytes each module contributed.
    pub module_bytes: Vec<(ModuleId, usize)>,
    pub css: Option<String>,
}

struct Out {
    code: String,
    line: u32,
    /// UTF-16 column where the next text starts (source maps count UTF-16 units).
    col: u32,
    /// `--minify-whitespace`: generated statements and fragments share one line.
    compact: bool,
}

/// How text moves the output position: its line breaks, and the UTF-16 length of what
/// follows the last one (or of all of it).
#[derive(Clone, Copy, Debug)]
struct Measure {
    newlines: u32,
    tail: u32,
}

impl Measure {
    fn of(s: &str) -> Measure {
        match s.rfind('\n') {
            Some(i) => Measure {
                newlines: s.bytes().filter(|&b| b == b'\n').count() as u32,
                tail: utf16_len(&s[i + 1..]),
            },
            None => Measure {
                newlines: 0,
                tail: utf16_len(s),
            },
        }
    }
}

impl Out {
    fn push(&mut self, s: &str) {
        self.push_measured(s, Measure::of(s));
    }
    fn push_measured(&mut self, s: &str, m: Measure) {
        if m.newlines > 0 {
            self.line += m.newlines;
            self.col = m.tail;
        } else {
            self.col += m.tail;
        }
        self.code.push_str(s);
    }
    /// A line of its own (hashbang, banner, footer, source-map comment).
    fn line(&mut self, s: &str) {
        if self.col != 0 {
            self.push("\n");
        }
        self.push(s);
        self.push("\n");
    }
    /// A generated statement: its own line, or appended to the current one when compact.
    fn stmt(&mut self, s: &str) {
        self.push(s);
        if !self.compact {
            self.push("\n");
        }
    }
}

fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        s.len() as u32
    } else {
        s.chars().map(|c| c.len_utf16() as u32).sum()
    }
}

/// True when JavaScript `code` (a banner) declares `require` at its top level.
fn declares_require(code: &str) -> bool {
    if !code.contains("require") {
        return false;
    }
    let ast = ts_r_parser::parse(code, ts_r_ast::SourceKind::Js).ast;
    let scopes = ts_r_minify::Scopes::analyze(&ast, true);
    scopes
        .module_symbols()
        .iter()
        .any(|&s| ast.atom(scopes.symbol(s).name) == "require")
}

/// Assembles chunk `ci`. `fragments` is indexed by module id.
pub fn assemble(
    ci: usize,
    plan: &Plan,
    graph: &Graph,
    fragments: &[Option<Arc<Fragment>>],
    opts: &BuildOptions,
    threads: usize,
    outbase: &str,
) -> ChunkOutput {
    let c = &plan.chunks[ci];
    let compact = opts.minify_whitespace;
    let sp = if compact { "" } else { " " };
    let mut out = Out {
        code: String::new(),
        line: 0,
        col: 0,
        compact,
    };
    let mut result = ChunkOutput::default();
    // Hashbang of the entry.
    if let Some(e) = c.entry
        && c.user_entry.is_some()
    {
        let src = &graph.modules[e as usize].source.js;
        if src.starts_with("#!") {
            let first = src.lines().next().unwrap_or("");
            out.line(first.trim_end());
        }
    }
    if c.use_strict {
        out.stmt("\"use strict\";");
    }
    if let Some(b) = &opts.banner {
        out.line(b);
    }
    let iife = plan.format == Format::Iife;
    if iife {
        match (&opts.global_name, &c.iife_return) {
            (Some(g), _) if c.user_entry.is_some() => {
                let open = if compact { "(()=>{" } else { "(() => {" };
                out.stmt(&format!("{}{sp}={sp}{open}", global_assign_target(g)));
            }
            _ => out.stmt(if compact { "(()=>{" } else { "(() => {" }),
        }
    }
    let q = |s: &str| Expr::Str(s.to_owned()).to_text(true);
    if plan.format == Format::Esm {
        // The common `const require = createRequire(import.meta.url)` banner provides
        // `require` itself; a second declaration would be a SyntaxError.
        if c.require_shim && !opts.banner.as_deref().is_some_and(declares_require) {
            out.stmt(&format!(
                "import{sp}{{{sp}createRequire as __tsrCreateRequire{sp}}}{sp}from{sp}\"module\";"
            ));
            out.stmt(&format!(
                "var require{sp}={sp}__tsrCreateRequire(import.meta.url);"
            ));
        }
        for x in &c.ext_imports {
            // A phase (`import defer * as ns`) or attributes (`with { type: "json" }`) are
            // kept as written: such imports have their own entry (`graph::external_key`).
            let (path, phase, attrs) = crate::graph::split_external(&x.path);
            let from = if attrs.is_empty() {
                q(path)
            } else {
                format!("{}{sp}{attrs}", q(path))
            };
            let kw = match phase {
                crate::meta::PHASE_DEFER => "import defer",
                crate::meta::PHASE_SOURCE => "import source",
                _ => "import",
            };
            let mut clause: Vec<String> = Vec::new();
            if let Some(d) = &x.default {
                clause.push(d.clone());
            }
            if !x.named.is_empty() {
                let specs: Vec<String> = x
                    .named
                    .iter()
                    .map(|(imp, local)| {
                        if imp == local {
                            local.clone()
                        } else if ts_r_codegen::literals::is_identifier_name(imp) {
                            format!("{imp} as {local}")
                        } else {
                            format!("{} as {local}", q(imp))
                        }
                    })
                    .collect();
                clause.push(format!(
                    "{{{sp}{}{sp}}}",
                    specs.join(if compact { "," } else { ", " })
                ));
            }
            if let Some(ns) = &x.namespace {
                if clause.is_empty() {
                    out.stmt(&format!("{kw} * as {ns} from{sp}{from};"));
                    continue;
                }
                out.stmt(&format!("{kw} * as {ns} from{sp}{from};"));
            }
            if clause.is_empty() {
                if x.namespace.is_none() {
                    out.stmt(&format!("import{sp}{from};"));
                }
            } else {
                let clause = clause.join(if compact { "," } else { ", " });
                // A default-import binding is an identifier: `from` needs a space after it
                // even when minified (`import o from"node:os"`).
                let gap = if clause.ends_with('}') { sp } else { " " };
                out.stmt(&format!("{kw} {clause}{gap}from{sp}{from};"));
            }
        }
        for (oc, names) in &c.imports {
            let path = chunk_import(&c.file, &plan.chunks[*oc].file, &opts.public_path);
            if names.is_empty() {
                out.stmt(&format!("import{sp}{};", q(&path)));
            } else {
                out.stmt(&format!(
                    "import{sp}{{{sp}{}{sp}}}{sp}from{sp}{};",
                    names.join(if compact { "," } else { ", " }),
                    q(&path)
                ));
            }
        }
    }
    if c.helpers.any() {
        let text = c
            .helpers
            .render(opts.minify_whitespace && opts.minify_identifiers);
        if compact {
            out.stmt(text.trim_end_matches('\n'));
        } else {
            out.push(&text);
        }
    }
    if plan.format != Format::Esm {
        for (path, var) in &c.ext_requires {
            let path = crate::graph::split_external(path).0;
            if var.is_empty() {
                out.stmt(&format!("require({});", q(path)));
            } else {
                out.stmt(&format!(
                    "var {var}{sp}={sp}{}(require({}));",
                    plan.helper_names[crate::runtime::Helper::ToEsm as usize],
                    q(path)
                ));
            }
        }
    }
    for p in &c.preludes {
        out.push(p);
        if !p.ends_with('\n') {
            out.push("\n");
        }
    }
    for (name, getters) in &c.ns_objects {
        out.stmt(&format!("var {name}{sp}={sp}{{}};"));
        let obj = Expr::Object(getters.clone());
        let call = Expr::call(
            &plan.helper_names[crate::runtime::Helper::Export as usize],
            vec![Expr::ident(name), obj],
        );
        out.stmt(&format!("{};", call.to_text(compact)));
    }
    for h in &c.head {
        out.stmt(h);
    }
    let mut map = (opts.sourcemap != crate::options::SourceMapMode::None).then(ChunkMap::new);
    let file = format!("{}/{}", plan.outdir, c.file);
    let map_dir = ts_r_config::path::directory_of(&file).to_owned();
    // Per fragment, in parallel: where its code ends (lines and last-line columns), and with
    // a map its re-encoded mappings, source path and escaped `sourcesContent` (the costly
    // part of a big chunk's map). They are joined in order below.
    let frags: Vec<(ModuleId, &Fragment)> = c
        .modules
        .iter()
        .filter_map(|&m| {
            let f = fragments[m as usize].as_deref()?;
            (!f.code.is_empty()).then_some((m, f))
        })
        .collect();
    let ids: Vec<Option<Vec<u32>>> = frags
        .iter()
        .map(|(_, f)| Some(map.as_mut()?.intern_names(&f.map.as_ref()?.names)))
        .collect();
    let work: Vec<usize> = (0..frags.len()).collect();
    let measured = crate::pool::map(threads, work, |i| {
        let (m, f) = frags[i];
        let module = &graph.modules[m as usize];
        // Resolve a file loader's URL in its small fragment, before copying it into the chunk.
        let code = if let Some(sheet) = module.source.css.as_deref()
            && !sheet.locals.is_empty()
        {
            let mut code = f.code.clone();
            for (k, v) in plan.css.values[m as usize].iter().enumerate() {
                let ph = format!(
                    "\"{}\"",
                    crate::prepare::css_placeholder(&sheet.id, k as u32)
                );
                let mut quoted = String::new();
                ts_r_codegen::literals::quote_string(v, '"', &mut quoted);
                code = code.replace(&ph, &quoted);
            }
            Cow::Owned(code)
        } else if let Some(a) = &module.source.asset {
            let rel = crate::bundle::asset_path(&a.rel_path, &module.path, outbase);
            let url =
                crate::bundle::asset_url(&plan.outdir, &file, &rel, &opts.public_path) + &a.query;
            let mut quoted = String::new();
            ts_r_codegen::literals::quote_string(&url, '"', &mut quoted);
            Cow::Owned(f.code.replace(&format!("\"{}\"", a.placeholder), &quoted))
        } else {
            Cow::Borrowed(f.code.as_str())
        };
        let size = Measure::of(&code);
        let map = match (&f.map, &ids[i]) {
            (Some(fm), Some(ids)) => {
                let file = crate::graph::split_query(&module.path).0;
                let rel = if ts_r_config::path::is_rooted(file) {
                    ts_r_config::path::relative(&map_dir, file, true)
                } else {
                    module.pretty.clone()
                };
                let content = opts.sources_content.then(|| {
                    let mut j = String::new();
                    ts_r_codegen::sourcemap::json_string(&mut j, &module.source.js);
                    j
                });
                Some((encode_fragment(fm, ids), rel, content))
            }
            _ => None,
        };
        (code, size, map)
    });
    out.code
        .reserve(frags.iter().map(|(_, f)| f.code.len()).sum::<usize>() + 64);
    let mut printed = frags.iter().zip(measured).peekable();
    for &cm in &c.modules {
        if let Some(stmts) = c.before.get(&cm) {
            let mut last = None;
            for (importer, s) in stmts {
                if !compact && last != Some(*importer) {
                    out.line(&format!("// {}", graph.modules[*importer as usize].pretty));
                }
                last = Some(*importer);
                out.stmt(s);
            }
        }
        if printed.peek().is_none_or(|((m, _), _)| *m != cm) {
            continue;
        }
        let Some(((m, _), (code, size, fmap))) = printed.next() else {
            continue;
        };
        let module = &graph.modules[*m as usize];
        if !compact {
            out.line(&format!("// {}", module.pretty));
        }
        if let (Some(cm), Some((enc, rel, content))) = (map.as_mut(), fmap) {
            let si = cm.add_source_json(rel, content);
            cm.add_encoded(out.line, out.col, si, &enc);
        }
        let start = out.code.len();
        out.push_measured(&code, size);
        result.module_bytes.push((*m, out.code.len() - start));
    }
    for t in &c.tail {
        out.stmt(t);
    }
    for s in &c.star_reexports {
        out.stmt(s);
    }
    if plan.format == Format::Esm && !c.exports.is_empty() {
        out.stmt(&format!(
            "export{sp}{{{sp}{}{sp}}};",
            c.exports.join(if compact { "," } else { ", " })
        ));
    }
    if iife {
        if let Some(r) = &c.iife_return
            && opts.global_name.is_some()
            && c.user_entry.is_some()
        {
            out.stmt(&format!("return {};", r.to_text(compact)));
        }
        out.stmt("})();");
    }
    if let Some(f) = &opts.footer {
        out.line(f);
    }
    // CSS.
    if !c.css.is_empty() {
        let host = CssHost {
            graph,
            plan,
            file: &file,
            opts,
            outbase,
        };
        result.css = crate::css::order::bundle(&host, &c.css, compact, opts.minify_syntax);
    }
    if out.col != 0 {
        out.push("\n");
    }
    let file_name = c.file.rsplit('/').next().unwrap_or(&c.file).to_owned();
    if let Some(cm) = map {
        let json = cm.to_json(&file_name, opts.sources_content);
        use crate::options::SourceMapMode::*;
        match opts.sourcemap {
            Linked => out.line(&format!("//# sourceMappingURL={file_name}.map")),
            Inline | Both => {
                let mut b = String::new();
                ts_r_codegen::sourcemap::base64_encode(json.as_bytes(), &mut b);
                out.line(&format!(
                    "//# sourceMappingURL=data:application/json;base64,{b}"
                ));
            }
            _ => {}
        }
        if matches!(opts.sourcemap, Linked | External | Both) {
            result.map = Some(json);
        }
    }
    result.code = out.code;
    result
}

/// `var X` or `(globalThis.a ??= {}).b` for a dotted `--global-name`.
fn global_assign_target(g: &str) -> String {
    let parts: Vec<&str> = g.split('.').collect();
    if parts.len() == 1 {
        return format!("var {g}");
    }
    let mut e = format!("globalThis.{}", parts[0]);
    for p in &parts[1..parts.len() - 1] {
        e = format!("({e} ??= {{}}).{p}");
    }
    format!("({e} ??= {{}}).{}", parts[parts.len() - 1])
}

/// The bundle as `css::order` sees it for one chunk: `url()`s relative to the chunk's
/// stylesheet (next to its JavaScript file), or inlined for the data loaders.
struct CssHost<'a> {
    graph: &'a Graph,
    plan: &'a Plan,
    /// The chunk's output file (absolute).
    file: &'a str,
    opts: &'a BuildOptions,
    outbase: &'a str,
}

impl crate::css::order::Host for CssHost<'_> {
    fn sheet(&self, m: ModuleId) -> Option<&crate::css::Sheet> {
        self.graph.modules[m as usize].source.css.as_deref()
    }

    fn target(&self, m: ModuleId, r: u32) -> crate::graph::Target {
        self.graph.modules[m as usize]
            .targets
            .get(r as usize)
            .cloned()
            .unwrap_or(crate::graph::Target::Missing)
    }

    fn url(&self, m: ModuleId, r: u32) -> String {
        let module = &self.graph.modules[m as usize];
        let spec = || {
            module
                .meta
                .records
                .get(r as usize)
                .map(|x| x.spec.clone())
                .unwrap_or_default()
        };
        match module.targets.get(r as usize) {
            Some(crate::graph::Target::Module(t)) => {
                let tm = &self.graph.modules[*t as usize];
                if let Some(u) = &tm.source.inline_url {
                    return u.to_string();
                }
                if let Some(a) = &tm.source.asset {
                    let rel = crate::bundle::asset_path(&a.rel_path, &tm.path, self.outbase);
                    return crate::bundle::asset_url(
                        &self.plan.outdir,
                        self.file,
                        &rel,
                        &self.opts.public_path,
                    ) + &a.query;
                }
                spec()
            }
            Some(crate::graph::Target::External(e)) => crate::graph::split_external(e).0.to_owned(),
            _ => spec(),
        }
    }

    fn local(&self, m: ModuleId, n: u32) -> String {
        self.plan
            .css
            .names
            .get(m as usize)
            .and_then(|v| v.get(n as usize))
            .cloned()
            .or_else(|| {
                self.sheet(m)
                    .and_then(|s| s.locals.get(n as usize).cloned())
            })
            .unwrap_or_default()
    }

    fn pretty(&self, m: ModuleId) -> String {
        self.graph.modules[m as usize].pretty.clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn banner_declaring_require() {
        assert!(super::declares_require(
            "import { createRequire } from 'module'; const require = createRequire(import.meta.url);"
        ));
        assert!(super::declares_require("var require = 1"));
        assert!(!super::declares_require("require('x'); // require"));
    }
}
