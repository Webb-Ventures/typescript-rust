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
    /// An ES module compiled to CommonJS because it was asked for with `require()`
    /// ([`compile`]'s `as_cjs`); its own format is ESM.
    pub forced: bool,
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
    // `import()` stays native in CommonJS output, as under tsx: Node loads the target through
    // the ESM loader (so an unknown extension is `ERR_UNKNOWN_FILE_EXTENSION`), and the code
    // needs no helper, so a function's text still runs when it is `eval`ed elsewhere.
    o.node_module = true;
    o
}

/// Compiles `src`. Errors are syntax errors, rendered tsc-style.
///
/// `as_cjs`: the file is loaded with `require()`. Under tsx everything `require()` reaches is
/// compiled to CommonJS, so an ES module is too, unless it has top-level `await` (which
/// CommonJS can't hold: Node's `require(esm)` then throws, and the preload names the error as
/// tsx does). Such output is [`Compiled::forced`].
pub fn compile(
    path: &str,
    src: &str,
    base: &TransformOptions,
    pkg: PackageType,
    source_maps: bool,
    as_cjs: bool,
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
    let natural = format_of(path, pkg, &ast, src);
    let forced = as_cjs && natural == Format::Esm && !has_top_level_await(&ast);
    let format = if forced { Format::Cjs } else { natural };
    let emulate_meta =
        format == Format::Cjs && src.contains("import.meta") && calls_import_meta(&mut ast);
    if src.contains("import") {
        unwrap_dynamic_imports(&mut ast);
    }
    let opts = file_options(base, format);
    let mut result = ts_r_transform::transform(&mut ast, src, &opts);
    if format == Format::Cjs {
        // The statements the transform wrote for no source statement (its export lists).
        let generated: Vec<NodeId> = result
            .map_ranges
            .iter()
            .filter(|(_, r)| r.start == ts_r_codegen::NO_MAP && r.end == ts_r_codegen::NO_MAP)
            .map(|&(id, _)| id)
            .collect();
        sort_cjs_exports(&mut ast, &generated);
        default_first_in_import_star(&mut result.prelude);
    }
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
    if forced && src.contains("module.exports") {
        // `export { x as "module.exports" }` is what `require()` returns (Node's rule for
        // `require(esm)`, which tsx applies to the CommonJS it compiles).
        if !code.ends_with('\n') {
            code.push('\n');
        }
        code.push_str(MODULE_EXPORTS_EXPORT);
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
        forced,
        code,
        specs,
    })
}

/// Appended to an ES module compiled to CommonJS for `require()` that may export the name
/// `module.exports`.
const MODULE_EXPORTS_EXPORT: &str = "if (Object.prototype.hasOwnProperty.call(exports, \"module.exports\")) \
     module.exports = exports[\"module.exports\"];";

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

/// tsx's CommonJS output (esbuild's) defines every export of an ES module up front, sorted
/// by name, so `Object.keys(require("./m"))` and a namespace import of it list them in that
/// order; tsc's order is the declaration order with `default` where it is assigned. So the
/// `exports.b = exports.a = void 0;` list after the `__esModule` marker is rebuilt to hold every
/// name the module exports at the top level (`exports.x = ...`, `exports["x"] = ...` and
/// `Object.defineProperty(exports, "x", ...)`), sorted, `default` and hoisted functions
/// included. Re-exports through `__exportStar` come after, as under tsx. Files without the
/// marker (plain CommonJS, `export =`) are left alone.
///
/// Only `generated` statements (the transform's own, which stand for no source statement)
/// are export lists to replace: a user's `exports.x = void f();` there is code and stays.
fn sort_cjs_exports(ast: &mut Ast, generated: &[NodeId]) {
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return;
    };
    let items: Vec<NodeId> = ast.list(stmts).to_vec();
    let Some(marker) = items
        .iter()
        .position(|&s| exported_by_define(ast, s) == Some("__esModule"))
    else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    // The `void 0` lists written by the transform: right after the marker, among the hoisted
    // function exports.
    let mut lists = Vec::new();
    let mut prelude = true;
    for (i, &s) in items.iter().enumerate().skip(marker + 1) {
        if let Some(name) = exported_by_define(ast, s) {
            names.push(name.to_owned());
            prelude = false;
            continue;
        }
        let Node::ExprStmt { expr } = *ast.node(s) else {
            prelude = false;
            continue;
        };
        let mut e = expr;
        let mut chain = 0usize;
        while let Node::Assign {
            left,
            op: ts_r_ast::AssignOp::Assign,
            right,
        } = *ast.node(e)
        {
            match export_target(ast, left) {
                Some(name) => {
                    names.push(name.to_owned());
                    chain += 1;
                }
                None => break,
            }
            e = right;
        }
        let void0 = matches!(
            *ast.node(e),
            Node::Unary {
                op: UnaryOp::Void,
                ..
            }
        );
        let ours = generated.contains(&s);
        if prelude && ours && chain > 0 && void0 {
            lists.push(i);
        } else if !(prelude && ours && chain == 1 && matches!(*ast.node(e), Node::Ident { .. })) {
            prelude = false;
        }
    }
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return;
    }
    let span = ast.span(items[marker]);
    let exports = ast.intern("exports");
    let mut new_lists = Vec::new();
    // A chain assigns its last target first: the first name goes innermost. At most 50
    // names a statement, as tsc writes them (a long chain would nest deeply).
    for chunk in names.chunks(50) {
        let zero = ast.add(
            Node::NumberLit {
                value: ts_r_ast::F64Bits::new(0.0),
            },
            span,
        );
        let mut value = ast.add(
            Node::Unary {
                op: UnaryOp::Void,
                arg: zero,
            },
            span,
        );
        for name in chunk {
            let obj = ast.add(Node::Ident { name: exports }, span);
            let atom = ast.intern(name);
            let left = if is_identifier_name(name) {
                let prop = ast.add(Node::Ident { name: atom }, span);
                ast.add(
                    Node::Member {
                        obj,
                        optional: false,
                        prop,
                    },
                    span,
                )
            } else {
                let index = ast.add(Node::StringLit { value: atom }, span);
                ast.add(
                    Node::Index {
                        obj,
                        optional: false,
                        index,
                    },
                    span,
                )
            };
            value = ast.add(
                Node::Assign {
                    left,
                    op: ts_r_ast::AssignOp::Assign,
                    right: value,
                },
                span,
            );
        }
        new_lists.push(ast.add(Node::ExprStmt { expr: value }, span));
    }
    let mut out = Vec::with_capacity(items.len() + new_lists.len());
    for (i, &s) in items.iter().enumerate() {
        if lists.contains(&i) {
            continue;
        }
        out.push(s);
        if i == marker {
            out.extend_from_slice(&new_lists);
        }
    }
    let stmts = ast.add_list(&out);
    *ast.node_mut(ast.root) = Node::Program { stmts };
}

/// `x` of `exports.x` or `exports["x"]`.
fn export_target(ast: &Ast, id: NodeId) -> Option<&str> {
    let (obj, key) = match *ast.node(id) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } => match *ast.node(prop) {
            Node::Ident { name } => (obj, name),
            _ => return None,
        },
        Node::Index {
            obj,
            optional: false,
            index,
        } => match *ast.node(index) {
            Node::StringLit { value } => (obj, value),
            _ => return None,
        },
        _ => return None,
    };
    match *ast.node(obj) {
        Node::Ident { name } if ast.atom(name) == "exports" => Some(ast.atom(key)),
        _ => None,
    }
}

/// `x` of the statement `Object.defineProperty(exports, "x", ...);`.
fn exported_by_define(ast: &Ast, stmt: NodeId) -> Option<&str> {
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let Node::Call { callee, args, .. } = *ast.node(expr) else {
        return None;
    };
    let Node::Member { obj, prop, .. } = *ast.node(callee) else {
        return None;
    };
    let (Node::Ident { name: o }, Node::Ident { name: p }) = (*ast.node(obj), *ast.node(prop))
    else {
        return None;
    };
    if ast.atom(o) != "Object" || ast.atom(p) != "defineProperty" {
        return None;
    }
    let args = ast.list(args);
    let (Some(&target), Some(&key)) = (args.first(), args.get(1)) else {
        return None;
    };
    match (*ast.node(target), *ast.node(key)) {
        (Node::Ident { name }, Node::StringLit { value }) if ast.atom(name) == "exports" => {
            Some(ast.atom(value))
        }
        _ => None,
    }
}

/// An ASCII identifier name (reserved words included: `exports.default` is fine).
fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// tsx's namespace of a CommonJS module (esbuild's `__toESM`) has `default` first, then the
/// module's own keys; tsc's `__importStar` adds `default` last. The two lines of the helper are
/// swapped (on their own lines, so nothing below moves).
fn default_first_in_import_star(prelude: &mut String) {
    const COPY: &str = "if (mod != null) for (var k = ownKeys(mod)";
    const DEFAULT: &str = "__setModuleDefault(result, mod);";
    let Some(copy) = prelude.find(COPY) else {
        return;
    };
    let line_start = prelude[..copy].rfind('\n').map_or(0, |i| i + 1);
    let Some(copy_end) = prelude[copy..].find('\n').map(|i| copy + i + 1) else {
        return;
    };
    let rest = &prelude[copy_end..];
    let Some(end) = rest.find('\n') else {
        return;
    };
    if rest[..end].trim() != DEFAULT {
        return;
    }
    let copy_line = prelude[line_start..copy_end].to_owned();
    let default_line = prelude[copy_end..copy_end + end + 1].to_owned();
    prelude.replace_range(line_start..copy_end + end + 1, &(default_line + &copy_line));
}

/// The CommonJS transform marks the exports with `__esModule` in the form Node's CommonJS
/// export detection reads, so an ES namespace import of the file would list `__esModule`
/// (tsx's output doesn't). The parenthesised name keeps the marker for `require` callers and
/// hides it from the detection.
fn hide_esmodule_marker(code: &mut String) {
    const MARKER: &str = "Object.defineProperty(exports, \"__esModule\", { value: true });";
    let head = code.floor_char_boundary(1024);
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

// ---- cache payload: [format u8: 0 ESM, 1 CJS, 2 CJS forced][spec count u32]{[flags u8][len u32][utf8]}[code] ----
// flags: 1 = require, 2 = lazy.

pub fn encode(c: &Compiled) -> Vec<u8> {
    let mut out = Vec::with_capacity(c.code.len() + 16 + c.specs.len() * 24);
    out.push(match (c.format, c.forced) {
        (Format::Esm, _) => 0,
        (Format::Cjs, false) => 1,
        (Format::Cjs, true) => 2,
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
    let (format, forced) = match *data.first()? {
        0 => (Format::Esm, false),
        1 => (Format::Cjs, false),
        2 => (Format::Cjs, true),
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
        forced,
        code,
        specs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    ts_r_testing::cap_memory!();

    fn run(path: &str, src: &str, pkg: PackageType) -> Compiled {
        compile(path, src, &TransformOptions::default(), pkg, false, false).expect("compiles")
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
        // `import()` stays native in CommonJS output (an ES module with top-level `await`
        // loads through it); static imports become `require`.
        let required: Vec<bool> = cjs.specs.iter().map(|s| s.require).collect();
        assert_eq!(required, [true, true, false]);
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
            false,
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
    fn commonjs_output_preserves_multibyte_strings_across_the_prefix_limit() {
        let value = "é".repeat(600);
        let src = format!("const s = \"{value}\";\n");
        let opts = TransformOptions {
            target: ts_r_config::ScriptTarget::ES2022,
            ..Default::default()
        };
        let c = compile("a.cts", &src, &opts, PackageType::Unspecified, false, false)
            .expect("compiles");
        assert_eq!(c.format, Format::Cjs);
        assert!(c.code.contains(&value), "{}", c.code);
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

    #[test]
    fn commonjs_exports_are_declared_in_name_order() {
        let src =
            "export const z = 1;\nexport function b() {}\nexport default 3;\nexport { z as a };\n";
        let c = run("a.cts", src, PackageType::Unspecified);
        let line = c
            .code
            .lines()
            .find(|l| l.contains("= void 0"))
            .expect("a void 0 chain");
        assert_eq!(
            line.trim(),
            "exports.z = exports.default = exports.b = exports.a = void 0;",
            "{}",
            c.code
        );
    }

    /// A user's `exports.x = void f()` right after the transform's export list is code, not
    /// another list: it is kept, and `extra` is still declared up front.
    #[test]
    fn user_void_assignments_after_the_export_list_stay() {
        let src = "exports.extra = void console.log('side');\nexport const value = 1;\n";
        let c = run("a.cts", src, PackageType::Unspecified);
        assert!(
            c.code.contains("exports.extra = void console.log('side');")
                || c.code
                    .contains("exports.extra = void console.log(\"side\");"),
            "{}",
            c.code
        );
        assert!(
            c.code.contains("exports.value = exports.extra = void 0;"),
            "{}",
            c.code
        );
    }

    #[test]
    fn import_star_sets_the_default_before_copying_keys() {
        let c = run(
            "a.cts",
            "import * as ns from './b';\nconsole.log(ns);\n",
            PackageType::Unspecified,
        );
        let default = c
            .code
            .find("__setModuleDefault(result, mod);")
            .expect("default");
        let keys = c.code.find("for (var k = ownKeys(mod)").expect("keys");
        assert!(default < keys, "{}", c.code);
    }

    #[test]
    fn dynamic_import_stays_native_in_commonjs_output() {
        let c = run(
            "a.cts",
            "export const p = import('./b.mjs');\n",
            PackageType::Unspecified,
        );
        assert_eq!(c.format, Format::Cjs);
        assert!(
            c.code.contains("import('./b.mjs')") || c.code.contains("import(\"./b.mjs\")"),
            "{}",
            c.code
        );
        assert!(!c.code.contains("require(\"./b.mjs\")"), "{}", c.code);
    }

    #[test]
    fn a_required_es_module_compiles_to_commonjs_unless_it_awaits() {
        let opts = TransformOptions::default();
        let esm = "export const a = 1;\n";
        let c = compile("a.ts", esm, &opts, PackageType::Module, false, true).expect("compiles");
        assert_eq!(c.format, Format::Cjs);
        assert!(c.forced);
        let natural =
            compile("a.ts", esm, &opts, PackageType::Module, false, false).expect("compiles");
        assert_eq!(natural.format, Format::Esm);
        let tla = "export const a = await 1;\n";
        let c = compile("a.ts", tla, &opts, PackageType::Module, false, true).expect("compiles");
        assert_eq!(c.format, Format::Esm);
        assert!(!c.forced);
    }
}
