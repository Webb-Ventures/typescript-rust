//! One file: TypeScript/TSX/JSX → JavaScript Node can run, plus the module specifiers the
//! output imports (so the graph walk and the resolve hook know what to resolve).

use std::io::IsTerminal as _;

use ts_r_ast::{
    Ast, BinaryOp, FnFlags, ImportPhase, MetaPropKind, Modifiers, Node, NodeId, NodeList,
    SourceKind, UnaryOp,
};
use ts_r_codegen::{CodegenOptions, Layout, PrintHints, SourceMapOptions};
use ts_r_diagnostics::{Renderer, format_pretty};
use ts_r_span::SourceText;
use ts_r_transform::{ModuleFormat, TransformOptions};

/// The module system a compiled file runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Esm,
    Cjs,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Esm => "esm",
            Format::Cjs => "cjs",
        }
    }
}

/// The `type` of the nearest `package.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackageType {
    Module,
    CommonJs,
    /// No `package.json`, or no `type` field: Node detects the format from the syntax.
    Unspecified,
}

/// A module specifier in compiled output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub text: String,
    /// Loaded with `require()` (CommonJS resolution and conditions) rather than `import`.
    pub require: bool,
    /// Only loaded when the program gets there: every use is an `import()` or a `require()`
    /// inside a function.
    pub lazy: bool,
}

/// A compiled file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compiled {
    pub format: Format,
    /// JavaScript, with an inline source map comment when maps are on.
    pub code: String,
    pub specs: Vec<Spec>,
}

/// Files ts-r-run compiles: TypeScript sources (not declarations) and JSX.
pub fn is_compiled_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(ext, "ts" | "tsx" | "mts" | "cts" | "jsx")
        && SourceKind::from_path(&lower) != SourceKind::Dts
}

/// The format of `path` given its package type: `.mts` → ESM, `.cts` → CommonJS, else the
/// `package.json` `type`, else Node's syntax detection (ES module syntax → ESM). Like tsx, a
/// file in a `"type": "module"` package that has no ES module syntax and assigns CommonJS
/// exports runs as CommonJS (`require` alone works in ES output, through `createRequire`).
fn format_of(path: &str, pkg: PackageType, ast: &Ast, src: &str) -> Format {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".mts") || lower.ends_with(".mjs") {
        return Format::Esm;
    }
    if lower.ends_with(".cts") || lower.ends_with(".cjs") {
        return Format::Cjs;
    }
    match pkg {
        PackageType::Module => {
            if assigns_cjs_exports(src) && !has_module_syntax(ast, src) {
                Format::Cjs
            } else {
                Format::Esm
            }
        }
        PackageType::CommonJs => Format::Cjs,
        PackageType::Unspecified => {
            if has_esm_syntax(ast, src) {
                Format::Esm
            } else {
                Format::Cjs
            }
        }
    }
}

/// Whether the file is an ES module by syntax once types are erased: a value
/// `import`/`export` (type-only declarations don't count), `import.meta` or top-level
/// `await`. Like tsx, a file that has ES module syntax but also uses CommonJS globals
/// (`require`, `module`, `exports`, `__dirname`, `__filename`) runs as CommonJS, so the two
/// can be mixed in packages without a `type`; only top-level `await` needs an ES module
/// (`import.meta` is emulated in CommonJS output, [`IMPORT_META`]).
pub fn has_esm_syntax(ast: &Ast, src: &str) -> bool {
    if has_top_level_await(ast) {
        return true;
    }
    (src.contains("import.meta") || has_value_module_decl(ast)) && !uses_cjs_globals(src)
}

/// Any ES module syntax: a value `import`/`export`, `import.meta` or top-level `await`.
fn has_module_syntax(ast: &Ast, src: &str) -> bool {
    src.contains("import.meta") || has_value_module_decl(ast) || has_top_level_await(ast)
}

/// A value `import` or `export` declaration at the top level.
fn has_value_module_decl(ast: &Ast) -> bool {
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return false;
    };
    for &s in ast.list(stmts) {
        let n = *ast.node(s);
        let esm = match n {
            Node::ImportDecl { type_only, .. }
            | Node::ExportNamed { type_only, .. }
            | Node::ExportAll { type_only, .. } => !type_only,
            Node::ExportDefaultExpr { .. } => true,
            Node::Interface { .. } | Node::TypeAlias { .. } | Node::ImportEquals { .. } => false,
            _ => {
                let m = n.modifiers();
                m.contains(Modifiers::EXPORT) && !m.contains(Modifiers::DECLARE)
            }
        };
        if esm {
            return true;
        }
    }
    false
}

fn assigns_cjs_exports(src: &str) -> bool {
    src.contains("module.exports") || src.contains("exports.")
}

fn uses_cjs_globals(src: &str) -> bool {
    [
        "require(",
        "module.exports",
        "exports.",
        "__dirname",
        "__filename",
    ]
    .iter()
    .any(|p| src.contains(p))
}

/// `await` or `for await` outside any function (and so needing an ES module).
fn has_top_level_await(ast: &Ast) -> bool {
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        match *ast.node(id) {
            Node::Await { .. } | Node::ForOf { is_await: true, .. } => return true,
            Node::VarDecl {
                kind: ts_r_ast::VarKind::AwaitUsing,
                ..
            } => return true,
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodDef { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => continue,
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// Specifiers of `import`/`export ... from`, `import("literal")` and `require("literal")` in
/// the transformed tree.
fn collect_specs(ast: &Ast) -> Vec<Spec> {
    let mut out: Vec<Spec> = Vec::new();
    // (node, inside a function)
    let mut stack: Vec<(NodeId, bool)> = vec![(ast.root, false)];
    let lit = |id: NodeId| match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    };
    while let Some((id, in_fn)) = stack.pop() {
        let mut inner = in_fn;
        match *ast.node(id) {
            Node::ImportDecl { source, .. }
            | Node::ExportAll { source, .. }
            | Node::ExportNamed {
                source: Some(source),
                ..
            } => {
                if let Some(text) = lit(source) {
                    out.push(Spec {
                        text,
                        require: false,
                        lazy: false,
                    });
                }
                continue;
            }
            Node::ImportCall { arg, .. } => {
                if let Some(text) = lit(arg) {
                    out.push(Spec {
                        text,
                        require: false,
                        lazy: true,
                    });
                }
            }
            Node::Call { callee, args, .. } => {
                if let Node::Ident { name } = *ast.node(callee)
                    && matches!(ast.atom(name), "require" | "__require")
                    && let Some(&first) = ast.list(args).first()
                    && let Some(text) = lit(first)
                {
                    out.push(Spec {
                        text,
                        require: true,
                        lazy: in_fn,
                    });
                }
            }
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodDef { .. } => inner = true,
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push((c, inner)));
    }
    // Source order (the stack visits children last-first), without duplicates; a specifier
    // is lazy only when every use of it is.
    out.reverse();
    let mut at: rustc_hash::FxHashMap<(String, bool), usize> = rustc_hash::FxHashMap::default();
    let mut specs: Vec<Spec> = Vec::with_capacity(out.len());
    for s in out {
        match at.get(&(s.text.clone(), s.require)) {
            Some(&i) => specs[i].lazy &= s.lazy,
            None => {
                at.insert((s.text.clone(), s.require), specs.len());
                specs.push(s);
            }
        }
    }
    specs
}

/// The options one file is transformed with: the project's options with the file's module
/// format.
pub fn file_options(base: &TransformOptions, format: Format) -> TransformOptions {
    let mut o = base.clone();
    o.module = match format {
        Format::Esm => ModuleFormat::Esm { node_require: true },
        Format::Cjs => ModuleFormat::CommonJs,
    };
    o
}

/// Compiles `src`. Errors are syntax errors, rendered tsc-style.
pub fn compile(
    path: &str,
    src: &str,
    base: &TransformOptions,
    pkg: PackageType,
    source_maps: bool,
) -> Result<Compiled, String> {
    let kind = SourceKind::from_path(path);
    let parsed = ts_r_parser::parse(src, kind);
    // Only the errors a build fails on (as esbuild): tsc's checker grammar errors such as
    // `if (x) ;` or `declare function f() {}` are not syntax errors here.
    let errors = ts_r_transform::build_errors(&parsed);
    if !errors.is_empty() {
        return Err(render_errors(path, src, &errors));
    }
    let mut ast = parsed.ast;
    let format = format_of(path, pkg, &ast, src);
    let emulate_meta =
        format == Format::Cjs && src.contains("import.meta") && calls_import_meta(&mut ast);
    if src.contains("import") {
        unwrap_dynamic_imports(&mut ast);
    }
    let opts = file_options(base, format);
    let result = ts_r_transform::transform(&mut ast, src, &opts);
    let specs = collect_specs(&ast);
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
    let codegen = CodegenOptions {
        layout: Layout::Readable,
        comments: false,
        source_map: source_maps.then_some(SourceMapOptions {
            file: None,
            // Relative to the module's own URL, so cached output has no absolute paths.
            source: name,
            source_root: None,
            include_content: false,
        }),
        indent_width: 2,
        newline: "\n",
        collect_specifiers: false,
    };
    let hints = PrintHints {
        erased: &result.erased,
        prelude: &result.prelude,
        ..Default::default()
    };
    let printed = ts_r_codegen::print_with(&ast, src, &codegen, &hints);
    let mut code = printed.code;
    if format == Format::Cjs {
        hide_esmodule_marker(&mut code);
    }
    if emulate_meta {
        if !code.ends_with('\n') {
            code.push('\n');
        }
        code.push_str(IMPORT_META);
        code.push('\n');
    }
    if let Some(map) = printed.map {
        if !code.ends_with('\n') {
            code.push('\n');
        }
        code.push_str("//# sourceMappingURL=");
        code.push_str(&map.to_data_url());
        code.push('\n');
    }
    Ok(Compiled {
        format,
        code,
        specs,
    })
}

/// What `import.meta` becomes in CommonJS output (tsx runs such files too): a call of a
/// function appended to the file, hoisted, that makes the object once. `url` is the file's URL;
/// the preload swaps [`IMPORT_META_URL`] for the module's own URL when that has a query or a
/// hash (`./config.ts?x=1` is a module of its own, with that URL).
pub const IMPORT_META: &str = "function __ts_r_import_meta() { return __ts_r_import_meta.m ??= \
     { url: /*ts-r-run:url*/require(\"node:url\").pathToFileURL(__filename).href, \
     filename: __filename, dirname: __dirname }; }";

/// The `url` expression in [`IMPORT_META`] (`js/hooks.mjs` has the same text).
pub const IMPORT_META_URL: &str =
    "/*ts-r-run:url*/require(\"node:url\").pathToFileURL(__filename).href";

/// Every `import(x)` becomes `import(x).then((m) => m.default && typeof m.default == "object" &&
/// "__esModule" in m.default ? m.default : m)`, as under tsx: the namespace of a CommonJS module
/// whose `module.exports` is a compiled ES module (it has `__esModule`) is that object, so
/// `(await import('./cjs.js')).default` is the module's default export, not its exports. The
/// function is written inline (not a shared helper) because code can be `eval`ed from a
/// function's text. `import.defer()` and `import.source()` are left as they are.
fn unwrap_dynamic_imports(ast: &mut Ast) {
    let mut found = Vec::new();
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        if let Node::ImportCall {
            phase: ImportPhase::Evaluation,
            ..
        } = *ast.node(id)
        {
            found.push(id);
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    if found.is_empty() {
        return;
    }
    let names = Names {
        then: ast.intern("then"),
        m: ast.intern("m"),
        default: ast.intern("default"),
        object: ast.intern("object"),
        marker: ast.intern("__esModule"),
    };
    for id in found {
        let span = ast.span(id);
        let call = ast.add(*ast.node(id), span);
        let prop = ast.add(Node::Ident { name: names.then }, span);
        let callee = ast.add(
            Node::Member {
                obj: call,
                optional: false,
                prop,
            },
            span,
        );
        let f = interop_arrow(ast, &names, span);
        let args = ast.add_list(&[f]);
        *ast.node_mut(id) = Node::Call {
            callee,
            optional: false,
            type_args: None,
            args,
        };
    }
}

struct Names {
    then: ts_r_ast::Atom,
    m: ts_r_ast::Atom,
    default: ts_r_ast::Atom,
    object: ts_r_ast::Atom,
    marker: ts_r_ast::Atom,
}

fn interop_arrow(ast: &mut Ast, n: &Names, span: ts_r_span::Span) -> NodeId {
    let m = |ast: &mut Ast| ast.add(Node::Ident { name: n.m }, span);
    let m_default = |ast: &mut Ast| {
        let obj = m(ast);
        let prop = ast.add(Node::Ident { name: n.default }, span);
        ast.add(
            Node::Member {
                obj,
                optional: false,
                prop,
            },
            span,
        )
    };
    let binary = |ast: &mut Ast, left, op, right| ast.add(Node::Binary { left, op, right }, span);
    let has = m_default(ast);
    let arg = m_default(ast);
    let type_of = ast.add(
        Node::Unary {
            op: UnaryOp::TypeOf,
            arg,
        },
        span,
    );
    let object = ast.add(Node::StringLit { value: n.object }, span);
    let is_object = binary(ast, type_of, BinaryOp::Eq, object);
    let marker = ast.add(Node::StringLit { value: n.marker }, span);
    let target = m_default(ast);
    let marked = binary(ast, marker, BinaryOp::In, target);
    let both = binary(ast, has, BinaryOp::LogicalAnd, is_object);
    let test = binary(ast, both, BinaryOp::LogicalAnd, marked);
    let cons = m_default(ast);
    let alt = m(ast);
    let body = ast.add(Node::Conditional { test, cons, alt }, span);
    let pattern = m(ast);
    let none: NodeList = ast.add_list(&[]);
    let param = ast.add(
        Node::Param {
            decorators: none,
            modifiers: Modifiers::default(),
            rest: false,
            pattern,
            optional: false,
            type_ann: None,
            init: None,
        },
        span,
    );
    let params = ast.add_list(&[param]);
    ast.add(
        Node::Arrow {
            flags: FnFlags::EXPRESSION_BODY,
            type_params: None,
            params,
            return_type: None,
            body,
        },
        span,
    )
}

/// Turns every `import.meta` into `__ts_r_import_meta()`; false when there is none.
fn calls_import_meta(ast: &mut Ast) -> bool {
    let mut found = Vec::new();
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        if let Node::MetaProperty {
            kind: MetaPropKind::ImportMeta,
        } = *ast.node(id)
        {
            found.push(id);
            continue;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    if found.is_empty() {
        return false;
    }
    let name = ast.intern("__ts_r_import_meta");
    for id in found {
        let span = ast.span(id);
        let callee = ast.add(Node::Ident { name }, span);
        let args = ast.add_list(&[]);
        *ast.node_mut(id) = Node::Call {
            callee,
            optional: false,
            type_args: None,
            args,
        };
    }
    true
}

/// The CommonJS transform marks the exports with `__esModule` in the form Node's CommonJS
/// export detection reads, so an ES namespace import of the file would list `__esModule`
/// (tsx's output doesn't). The parenthesised name keeps the marker for `require` callers and
/// hides it from the detection.
fn hide_esmodule_marker(code: &mut String) {
    const MARKER: &str = "Object.defineProperty(exports, \"__esModule\", { value: true });";
    let head = code.len().min(1024);
    if let Some(at) = code[..head].find(MARKER) {
        code.replace_range(
            at..at + MARKER.len(),
            "Object.defineProperty(exports, (\"__esModule\"), { value: true });",
        );
    }
}

fn render_errors(path: &str, src: &str, diags: &[&ts_r_diagnostics::Diagnostic]) -> String {
    let file = SourceText::new(path, src);
    let r = Renderer {
        color: std::io::stderr().is_terminal(),
    };
    let mut out = String::new();
    for d in diags {
        out.push_str(&format_pretty(d, &file, r));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

// ---- cache payload: [format u8][spec count u32]{[flags u8][len u32][utf8]}[code] ----
// flags: 1 = require, 2 = lazy.

pub fn encode(c: &Compiled) -> Vec<u8> {
    let mut out = Vec::with_capacity(c.code.len() + 16 + c.specs.len() * 24);
    out.push(match c.format {
        Format::Esm => 0,
        Format::Cjs => 1,
    });
    out.extend_from_slice(&(c.specs.len() as u32).to_le_bytes());
    for s in &c.specs {
        out.push(u8::from(s.require) | (u8::from(s.lazy) << 1));
        out.extend_from_slice(&(s.text.len() as u32).to_le_bytes());
        out.extend_from_slice(s.text.as_bytes());
    }
    out.extend_from_slice(c.code.as_bytes());
    out
}

pub fn decode(data: &[u8]) -> Option<Compiled> {
    fn u32_at(d: &[u8], at: usize) -> Option<usize> {
        let b = d.get(at..at + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }
    let format = match *data.first()? {
        0 => Format::Esm,
        1 => Format::Cjs,
        _ => return None,
    };
    let n = u32_at(data, 1)?;
    let mut at = 5;
    let mut specs = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let flags = *data.get(at)?;
        let len = u32_at(data, at + 1)?;
        let text = std::str::from_utf8(data.get(at + 5..at + 5 + len)?).ok()?;
        specs.push(Spec {
            text: text.to_owned(),
            require: flags & 1 != 0,
            lazy: flags & 2 != 0,
        });
        at += 5 + len;
    }
    let code = std::str::from_utf8(data.get(at..)?).ok()?.to_owned();
    Some(Compiled {
        format,
        code,
        specs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, src: &str, pkg: PackageType) -> Compiled {
        compile(path, src, &TransformOptions::default(), pkg, false).expect("compiles")
    }

    #[test]
    fn detects_format_like_node() {
        let esm = "import { a } from './a';\nexport const x: number = a;\n";
        let cjs =
            "import type { T } from './t';\nexport interface I {}\nconst x: T = require('./a');\n";
        assert_eq!(
            run("a.ts", esm, PackageType::Unspecified).format,
            Format::Esm
        );
        assert_eq!(
            run("a.ts", cjs, PackageType::Unspecified).format,
            Format::Cjs
        );
        assert_eq!(run("a.ts", cjs, PackageType::Module).format, Format::Esm);
        let exports = "import path = require('node:path');\nmodule.exports = { b: path.sep };\n";
        assert_eq!(
            run("a.ts", exports, PackageType::Module).format,
            Format::Cjs
        );
        let both = "export const a = 1;\nmodule.exports.b = 2;\n";
        assert_eq!(run("a.ts", both, PackageType::Module).format, Format::Esm);
        assert_eq!(run("a.ts", esm, PackageType::CommonJs).format, Format::Cjs);
        assert_eq!(run("a.mts", cjs, PackageType::CommonJs).format, Format::Esm);
        assert_eq!(run("a.cts", esm, PackageType::Module).format, Format::Cjs);
        let meta = "const u: string = import.meta.url;\n";
        assert_eq!(
            run("a.ts", meta, PackageType::Unspecified).format,
            Format::Esm
        );
        let tla =
            "const r: number = await Promise.resolve(1);\nconst f = async () => { await 0; };\n";
        assert_eq!(
            run("a.ts", tla, PackageType::Unspecified).format,
            Format::Esm
        );
        let nested = "const f = async () => { await 0; };\n";
        assert_eq!(
            run("a.ts", nested, PackageType::Unspecified).format,
            Format::Cjs
        );
        let mixed =
            "import { a } from './a';\nconst b = require('./b');\nexport const c = a + b;\n";
        assert_eq!(
            run("a.ts", mixed, PackageType::Unspecified).format,
            Format::Cjs
        );
    }

    #[test]
    fn collects_runtime_specifiers_only() {
        let src = "import type { T } from './types';\nimport { a } from './a.js';\nimport { U } from './only-types';\nexport * from './b';\nconst c = await import('./c');\nlet u: U; let t: T;\nconsole.log(a, c);\n";
        let out = run("m.ts", src, PackageType::Module);
        let texts: Vec<&str> = out.specs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["./a.js", "./b", "./c"]);
        assert!(out.specs.iter().all(|s| !s.require));
        let cjs = run("m.ts", src, PackageType::CommonJs);
        let texts: Vec<&str> = cjs.specs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["./a.js", "./b", "./c"]);
        assert!(cjs.specs.iter().all(|s| s.require));
    }

    #[test]
    fn payload_round_trips() {
        let c = run(
            "m.ts",
            "import x from 'x';\nexport const y = x as number;\n",
            PackageType::Module,
        );
        assert_eq!(decode(&encode(&c)), Some(c));
        assert_eq!(decode(&[7]), None);
        assert_eq!(decode(&[0, 9, 0, 0, 0]), None);
    }

    #[test]
    fn syntax_errors_are_reported() {
        let e = compile(
            "bad.ts",
            "let x = ;\n",
            &TransformOptions::default(),
            PackageType::Module,
            false,
        )
        .expect_err("syntax error");
        assert!(e.contains("bad.ts"), "{e}");
    }

    #[test]
    fn grammar_errors_esbuild_accepts_do_not_stop_a_run() {
        let src = "declare function f() { return 1 }\nif (globalThis.x) ;\nconsole.log(1);\n";
        let c = run("a.ts", src, PackageType::Module);
        assert!(c.code.contains("console.log(1)"), "{}", c.code);
        let e = compile(
            "b.ts",
            "const x: number;\n",
            &TransformOptions::default(),
            PackageType::Module,
            false,
        )
        .expect_err("early error");
        assert!(e.contains("must be initialized"), "{e}");
    }

    #[test]
    fn inline_source_map_uses_the_file_name() {
        let c = compile(
            "/abs/dir/m.ts",
            "const a: number = 1;\nthrow new Error(String(a));\n",
            &TransformOptions::default(),
            PackageType::Module,
            true,
        )
        .expect("compiles");
        assert!(
            c.code
                .contains("//# sourceMappingURL=data:application/json;base64,")
        );
        assert!(!c.code.contains("/abs/dir"));
    }

    #[test]
    fn dynamic_import_unwraps_a_compiled_es_module_default() {
        let c = run(
            "a.ts",
            "export const f = () => import('./b');\n",
            PackageType::Module,
        );
        assert!(c.code.contains("\"__esModule\" in"), "{}", c.code);
        assert!(c.code.contains(".then("), "{}", c.code);
        // Source phase imports and the static graph are left alone.
        let c = run(
            "a.ts",
            "import { x } from './b';\nexport { x };\n",
            PackageType::Module,
        );
        assert!(!c.code.contains(".then("), "{}", c.code);
    }

    #[test]
    fn commonjs_output_hides_the_esmodule_marker_from_export_detection() {
        let c = run(
            "a.cts",
            "export const x: number = 1;\n",
            PackageType::Module,
        );
        assert_eq!(c.format, Format::Cjs);
        assert!(c.code.contains("(\"__esModule\")"), "{}", c.code);
        let mut code = String::from("let a;\n");
        hide_esmodule_marker(&mut code);
        assert_eq!(code, "let a;\n");
    }

    #[test]
    fn import_meta_in_commonjs_output_is_emulated() {
        let c = run(
            "a.cts",
            "export const u: string = import.meta.url;\n",
            PackageType::Module,
        );
        assert_eq!(c.format, Format::Cjs);
        assert!(!c.code.contains("import.meta"), "{}", c.code);
    }

    #[test]
    fn compiled_paths() {
        for p in ["a.ts", "b.TSX", "c.mts", "d.cts", "e.jsx"] {
            assert!(is_compiled_path(p), "{p}");
        }
        for p in ["a.d.ts", "b.d.mts", "c.js", "d.json", "e.d.css.ts"] {
            assert!(!is_compiled_path(p), "{p}");
        }
    }
}
