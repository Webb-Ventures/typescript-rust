//! Chunk assembly: wraps printed fragments with the chunk's imports, runtime helpers,
//! namespace objects, entry exports and format wrapper, and concatenates source maps.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::expr::Expr;
use crate::graph::{Graph, ModuleId};
use crate::options::{BuildOptions, Format};
use crate::plan::{Plan, relative_import};
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
                    out.stmt(&format!("import * as {ns} from{sp}{};", q(&x.path)));
                    continue;
                }
                out.stmt(&format!("import * as {ns} from{sp}{};", q(&x.path)));
            }
            if clause.is_empty() {
                if x.namespace.is_none() {
                    out.stmt(&format!("import{sp}{};", q(&x.path)));
                }
            } else {
                let clause = clause.join(if compact { "," } else { ", " });
                // A default-import binding is an identifier: `from` needs a space after it
                // even when minified (`import o from"node:os"`).
                let gap = if clause.ends_with('}') { sp } else { " " };
                out.stmt(&format!("import {clause}{gap}from{sp}{};", q(&x.path)));
            }
        }
        for (oc, names) in &c.imports {
            let path = relative_import(&c.file, &plan.chunks[*oc].file);
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
    let map_dir =
        ts_r_config::path::directory_of(&format!("{}/{}", plan.outdir, c.file)).to_owned();
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
        let size = Measure::of(&f.code);
        let map = match (&f.map, &ids[i]) {
            (Some(fm), Some(ids)) => {
                let rel = if ts_r_config::path::is_rooted(&module.path) {
                    ts_r_config::path::relative(&map_dir, &module.path, true)
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
        (size, map)
    });
    out.code
        .reserve(frags.iter().map(|(_, f)| f.code.len()).sum::<usize>() + 64);
    for ((m, f), (size, fmap)) in frags.iter().zip(measured) {
        let module = &graph.modules[*m as usize];
        if !compact {
            out.line(&format!("// {}", module.pretty));
        }
        if let (Some(cm), Some((enc, rel, content))) = (map.as_mut(), fmap) {
            let si = cm.add_source_json(rel, content);
            cm.add_encoded(out.line, out.col, si, &enc);
        }
        let start = out.code.len();
        out.push_measured(&f.code, size);
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
        let mut css = String::new();
        for &m in &c.css {
            let module = &graph.modules[m as usize];
            if let Some(text) = &module.source.css {
                if !compact {
                    let _ = writeln!(css, "/* {} */", module.pretty);
                }
                css.push_str(text);
                if !text.ends_with('\n') {
                    css.push('\n');
                }
            }
        }
        result.css = Some(css);
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
