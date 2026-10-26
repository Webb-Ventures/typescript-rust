//! Completions: members after `.`, names in scope (locals, globals, keywords) and
//! auto-imports from the project's own modules; `completionItem/resolve` adds the detail line
//! and documentation.

use super::hover::{describe, docs_of};
use super::locate::{is_expression_name, meaning_at, node_at};
use super::{Ctx, LspLines};
use serde_json::{Value, json};
use std::collections::HashSet;
use ts_r_ast::{Ast, Node, NodeId};
use ts_r_binder::{SymbolFacts, SymbolFlags, meaning};
use ts_r_checker::checker::GlobalType;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_config::{CompilerOptions, ModuleResolution, RealFs};
use ts_r_diagnostics::FileId;
use ts_r_resolver::{ModuleFormat, Resolver, ResolverOptions};
use ts_r_types::SymId;

/// Sort groups (tsserver's: locals before globals before auto-imports).
const SORT_LOCAL: &str = "11";
const SORT_MEMBER: &str = "11";
const SORT_GLOBAL: &str = "15";
const SORT_AUTO_IMPORT: &str = "16";
const SORT_KEYWORD: &str = "15";

/// Auto-import candidates offered at most (the client filters further as the user types).
const MAX_AUTO_IMPORTS: usize = 500;

const KEYWORDS: &[&str] = &[
    "abstract",
    "any",
    "as",
    "async",
    "await",
    "bigint",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "module",
    "namespace",
    "never",
    "new",
    "null",
    "number",
    "object",
    "package",
    "readonly",
    "return",
    "satisfies",
    "string",
    "super",
    "switch",
    "symbol",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unique",
    "unknown",
    "using",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// What the cursor completes.
enum Context {
    /// After `obj.` (or `A.` in a type).
    Member { obj: NodeId, qualified: bool },
    /// A name in scope; `types_only` in a type reference, `values_only` in an expression.
    Scope {
        anchor: NodeId,
        types_only: bool,
        values_only: bool,
    },
    /// Inside a string literal: the literals, property names or module paths it may hold.
    String { lit: NodeId },
    /// Inside a comment: only `/// <reference path="|"` offers anything.
    Reference,
    /// Nothing.
    None,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The identifier prefix being typed: `(start, text)`.
fn word_before(text: &str, offset: u32) -> (u32, &str) {
    let b = text.as_bytes();
    let mut s = offset as usize;
    while s > 0 && is_ident_byte(b[s - 1]) {
        s -= 1;
    }
    (s as u32, &text[s..offset as usize])
}

fn context(ck: &Ck<'_, '_>, file: FileId, offset: u32) -> (Context, String) {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (ws, prefix) = word_before(text, offset);
    let prefix = prefix.to_string();
    if in_comment(ast, offset) {
        return (Context::Reference, prefix);
    }
    if let Some(lit) = super::strings::literal_at(ck, file, offset) {
        return (Context::String { lit }, prefix);
    }
    let node = node_at(ast, offset);
    if let Some(n) = node
        && matches!(
            ast.node(n),
            Node::StringLit { .. }
                | Node::TemplateLit { .. }
                | Node::RegexLit { .. }
                | Node::Directive { .. }
        )
        && ast.span(n).start < offset
        && offset < ast.span(n).end
    {
        return (Context::None, prefix);
    }
    // `obj.|` / `obj.pre|`
    let b = text.as_bytes();
    let mut d = ws as usize;
    while d > 0 && b[d - 1].is_ascii_whitespace() {
        d -= 1;
    }
    if d > 0 && b[d - 1] == b'.' && !(d > 1 && b[d - 2] == b'.') {
        let dot = (d - 1) as u32;
        let mut cur = node_at(ast, dot + 1);
        for _ in 0..4 {
            let Some(n) = cur else { break };
            match *ast.node(n) {
                Node::Member { obj, .. } if ast.span(obj).end <= dot => {
                    return (
                        Context::Member {
                            obj,
                            qualified: false,
                        },
                        prefix,
                    );
                }
                Node::QualifiedName { left, .. } if ast.span(left).end <= dot => {
                    return (
                        Context::Member {
                            obj: left,
                            qualified: true,
                        },
                        prefix,
                    );
                }
                _ => cur = c.file(file).parents().parent(n),
            }
        }
        // A number like `1.` or something unparsed: no member list.
        return (Context::None, prefix);
    }
    let anchor = node.unwrap_or(ast.root);
    let parent = c.file(file).parents().parent(anchor);
    // A name being declared (`var a|`, `function f|`, `catch (e|`) gets no list.
    if let Some(p) = parent
        && matches!(ast.node(anchor), Node::Ident { .. })
        && declares(ast.node(p), anchor)
    {
        return (Context::None, prefix);
    }
    let types_only = matches!(ast.node(anchor), Node::Ident { .. })
        && parent.is_some_and(|p| matches!(ast.node(p), Node::TypeRef { .. }));
    let values_only = matches!(ast.node(anchor), Node::Ident { .. })
        && is_expression_name(ck, Loc::new(file, anchor));
    (
        Context::Scope {
            anchor,
            types_only,
            values_only,
        },
        prefix,
    )
}

/// Whether `name` is the name a declaration `parent` introduces.
fn declares(parent: &Node, name: NodeId) -> bool {
    match *parent {
        Node::VarDeclarator { name: n, .. }
        | Node::Interface { name: n, .. }
        | Node::TypeAlias { name: n, .. }
        | Node::Enum { name: n, .. }
        | Node::Module { name: n, .. }
        | Node::TypeParam { name: n, .. }
        | Node::Param { pattern: n, .. } => n == name,
        Node::FunctionDecl { name: n, .. }
        | Node::ClassDecl { name: n, .. }
        | Node::ClassExpr { name: n, .. }
        | Node::FunctionExpr { name: n, .. }
        | Node::CatchClause { param: n, .. } => n == Some(name),
        _ => false,
    }
}

fn in_comment(ast: &Ast, offset: u32) -> bool {
    let i = ast.comments.partition_point(|c| c.span.end < offset);
    ast.comments.get(i).is_some_and(|c| {
        c.span.start < offset
            && (offset < c.span.end
                || (c.kind == ts_r_ast::CommentKind::Line && offset == c.span.end))
    })
}

/// LSP CompletionItemKind of a symbol.
fn item_kind(c: &Checker<'_>, s: SymId) -> u32 {
    let s = c.resolve_alias(s).unwrap_or(s);
    let f = c.sym_flags(s);
    kind_of_flags(f)
}

fn kind_of_flags(f: SymbolFlags) -> u32 {
    if f.intersects(SymbolFlags::METHOD) {
        2
    } else if f.intersects(SymbolFlags::FUNCTION) {
        3
    } else if f.intersects(SymbolFlags::CONSTRUCTOR) {
        4
    } else if f.intersects(SymbolFlags::CLASS) {
        7
    } else if f.intersects(SymbolFlags::INTERFACE | SymbolFlags::TYPE_ALIAS) {
        8
    } else if f.intersects(SymbolFlags::ENUM) {
        13
    } else if f.intersects(SymbolFlags::ENUM_MEMBER) {
        20
    } else if f.intersects(
        SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE | SymbolFlags::MODULE_FILE,
    ) {
        9
    } else if f.intersects(SymbolFlags::TYPE_PARAMETER) {
        25
    } else if f.intersects(SymbolFlags::PROPERTY | SymbolFlags::ACCESSOR) {
        10
    } else {
        6
    }
}

pub(crate) fn is_identifier_name(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && it.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// The completion list at `offset`.
pub fn completion(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let uri = crate::uri::path_to_uri(&c.file(cx.file).path);
    let (ctx, prefix) = context(ck, cx.file, offset);
    let data = |kind: &str, name: &str| json!({"uri": uri, "o": offset, "k": kind, "n": name});
    let mut items = Vec::new();
    let mut incomplete = false;
    match ctx {
        Context::None => {}
        Context::String { lit } => {
            let template = super::strings::is_template(c.ast(cx.file), lit);
            let found = super::strings::string_items(cx, ck, lit, offset).unwrap_or_default();
            if found.is_empty() {
                return Value::Null;
            }
            for (label, kind) in found {
                let label = super::strings::escape_label(&label, template);
                items.push(json!({"label": label, "kind": kind, "sortText": SORT_LOCAL}));
            }
        }
        Context::Reference => {
            let found = super::strings::reference_items(cx, ck, offset).unwrap_or_default();
            if found.is_empty() {
                return Value::Null;
            }
            for (label, kind) in found {
                items.push(json!({"label": label, "kind": kind, "sortText": SORT_LOCAL}));
            }
        }
        Context::Member { obj, qualified } => {
            for (name, s) in members(ck, Loc::new(cx.file, obj), qualified) {
                if !is_identifier_name(&name) {
                    continue;
                }
                items.push(json!({
                    "label": name,
                    "kind": item_kind(c, s),
                    "sortText": SORT_MEMBER,
                    "data": data("m", &name),
                }));
            }
        }
        Context::Scope {
            anchor,
            types_only,
            values_only,
        } => {
            let want = if types_only {
                meaning::TYPE | meaning::NAMESPACE
            } else if values_only {
                meaning::VALUE | meaning::NAMESPACE
            } else {
                meaning::ALL
            };
            let mut seen: HashSet<String> = HashSet::new();
            for (name, s) in scope_names(c, Loc::new(cx.file, anchor)) {
                if !c.has_meaning(c.resolve_alias(s).unwrap_or(s), want) && !c.has_meaning(s, want)
                {
                    continue;
                }
                if !seen.insert(name.clone()) {
                    continue;
                }
                items.push(json!({
                    "label": name,
                    "kind": item_kind(c, s),
                    "sortText": SORT_LOCAL,
                    "data": data("s", &name),
                }));
            }
            if !types_only
                && has_arguments(c, Loc::new(cx.file, anchor))
                && seen.insert("arguments".to_string())
            {
                items.push(json!({
                    "label": "arguments",
                    "kind": 6,
                    "sortText": SORT_LOCAL,
                    "data": data("s", "arguments"),
                }));
            }
            // Globals do not hide auto-imports (tsserver offers both); locals do.
            let locals = seen.clone();
            for (name, merged) in &c.link.globals {
                let name: &str = name;
                if !is_identifier_name(name) || seen.contains(name) {
                    continue;
                }
                let m = flags_meaning(merged.flags);
                if m & want == 0 {
                    continue;
                }
                seen.insert(name.to_string());
                items.push(json!({
                    "label": name,
                    "kind": kind_of_flags(merged.flags),
                    "sortText": SORT_GLOBAL,
                    "data": data("g", name),
                }));
            }
            if !types_only {
                for k in KEYWORDS {
                    if seen.insert((*k).to_string()) {
                        items.push(json!({"label": k, "kind": 14, "sortText": SORT_KEYWORD}));
                    }
                }
            }
            // Auto-imports need a typed prefix (as tsserver's list is filtered client-side,
            // the list is incomplete: typing more asks again).
            if !prefix.is_empty() {
                incomplete = true;
                items.extend(auto_imports(cx, ck, &prefix, &locals, want, offset, &uri));
            }
        }
    }
    json!({"isIncomplete": incomplete, "items": items})
}

fn flags_meaning(f: SymbolFlags) -> u8 {
    let mut m = 0;
    if f.intersects(SymbolFlags::VALUE) {
        m |= meaning::VALUE;
    }
    if f.intersects(SymbolFlags::TYPE) {
        m |= meaning::TYPE;
    }
    if f.intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE) {
        m |= meaning::NAMESPACE;
    }
    if f.intersects(SymbolFlags::ALIAS) {
        m |= meaning::ALL;
    }
    m
}

/// Members of the object (or namespace) before the dot.
fn members(ck: &mut Ck<'_, '_>, obj: Loc, qualified: bool) -> Vec<(String, SymId)> {
    let c = ck.c;
    // A namespace, enum or module (`import * as ns`) lists its exports.
    let named = match c.node(obj) {
        Node::Ident { .. } => {
            let s = if qualified {
                c.resolve_ident(obj, meaning::NAMESPACE)
                    .or_else(|| c.resolve_ident(obj, meaning::ALL))
            } else {
                meaning_at(ck, obj).primary
            };
            s.map(|s| c.resolve_alias(s).unwrap_or(s))
        }
        Node::QualifiedName { right, .. } => meaning_at(ck, Loc::new(obj.file, *right))
            .primary
            .map(|s| c.resolve_alias(s).unwrap_or(s)),
        _ => None,
    };
    if let Some(s) = named {
        let f = c.sym_flags(s);
        if qualified || f.intersects(SymbolFlags::MODULE_FILE) {
            return c.exports_of(s);
        }
        if f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
            && !f.intersects(
                SymbolFlags::CLASS
                    | SymbolFlags::FUNCTION
                    | SymbolFlags::VARIABLE
                    | SymbolFlags::ENUM,
            )
        {
            return c
                .exports_of(s)
                .into_iter()
                .filter(|(_, x)| {
                    c.has_meaning(
                        c.resolve_alias(*x).unwrap_or(*x),
                        meaning::VALUE | meaning::NAMESPACE,
                    )
                })
                .collect();
        }
    }
    if qualified {
        return Vec::new();
    }
    let t = ck.check_expr(obj);
    let a = ck.apparent_type(t);
    let mut props = ck.properties_of(a);
    // Callable and constructable values also have `Function`'s members (`apply`, `bind`,
    // `call`, ...), through `CallableFunction`/`NewableFunction` under `strictBindCallApply`.
    let call = !ck.signatures_of(a, SigKind::Call).is_empty();
    if call || !ck.signatures_of(a, SigKind::Construct).is_empty() {
        let strict = if !c.opts.strict_bind_call_apply {
            None
        } else if call {
            ck.global_type(GlobalType::CallableFunction)
        } else {
            ck.global_type(GlobalType::NewableFunction)
        };
        for ft in [strict, ck.global_type(GlobalType::Function)]
            .into_iter()
            .flatten()
        {
            if ft == a {
                continue;
            }
            for (n, s) in ck.properties_of(ft) {
                if !props.iter().any(|(m, _)| *m == n) {
                    props.push((n, s));
                }
            }
        }
    }
    let store = ck.store();
    let mut out: Vec<(String, SymId)> = props
        .into_iter()
        .map(|(n, s)| (store.str(n).to_string(), s))
        .filter(|(n, _)| !n.starts_with("__@") && !n.starts_with('#'))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Whether `arguments` is in scope at `l`: inside a function that is not an arrow function
/// (arrows see their parent's), and not reached through a class member initializer.
fn has_arguments(c: &Checker<'_>, l: Loc) -> bool {
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut cur = parents.parent(l.node);
    for _ in 0..100_000 {
        let Some(n) = cur else { return false };
        match ast.node(n) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => return true,
            Node::PropertyDef { .. }
            | Node::StaticBlock { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => {
                return false;
            }
            _ => {}
        }
        cur = parents.parent(n);
    }
    false
}

/// Names visible at `l`: every enclosing scope's locals, innermost first.
fn scope_names(c: &Checker<'_>, l: Loc) -> Vec<(String, SymId)> {
    let b = c.bound(l.file);
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut scope = None;
    let mut cur = Some(l.node);
    for _ in 0..100_000 {
        let Some(n) = cur else { break };
        if let Some(s) = b.scope_of(n) {
            scope = Some(s);
            break;
        }
        cur = parents.parent(n);
    }
    let mut scope = scope.or_else(|| (!b.scopes.is_empty()).then_some(ts_r_binder::ScopeId(0)));
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let Some(sid) = scope else { break };
        let Some(sc) = b.scopes.get(sid.0 as usize) else {
            break;
        };
        for &(name, sym) in b.table(sc.locals) {
            let text = b.name_text(ast, name);
            if !is_identifier_name(text) {
                continue;
            }
            let bs = b.symbol(sym);
            if bs
                .facts
                .intersects(SymbolFacts::EXPORT_SPECIFIER | SymbolFacts::DETACHED)
            {
                continue;
            }
            out.push((text.to_string(), c.sym(l.file, sym)));
        }
        scope = sc.parent;
    }
    out
}

/// Exports of the project's own modules whose names start with `prefix` (case-insensitive)
/// and are not visible here, with the edit that imports them.
fn auto_imports(
    cx: &Ctx<'_, '_>,
    ck: &Ck<'_, '_>,
    prefix: &str,
    visible: &HashSet<String>,
    want: u8,
    offset: u32,
    uri: &str,
) -> Vec<Value> {
    let c = ck.c;
    let here = c.file(cx.file);
    let lower = prefix.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut style = ImportStyle::of(c, cx.file);
    if needs_js_extension(&cx.program.options, &here.path) {
        style.js_ext = true;
    }
    for (i, f) in c.files.iter().enumerate() {
        let fid = FileId(i as u32);
        if fid == cx.file
            || f.is_lib
            || f.path.contains("/node_modules/")
            || !f.bound.is_module
            || f.is_declaration()
        {
            continue;
        }
        let module_sym = c.sym(fid, f.bound.file_symbol);
        for (name, s) in c.exports_of(module_sym) {
            if name == "default"
                || visible.contains(&name)
                || !name.to_ascii_lowercase().starts_with(&lower)
                || !is_identifier_name(&name)
            {
                continue;
            }
            let target = c.resolve_alias(s).unwrap_or(s);
            if !c.has_meaning(target, want) {
                continue;
            }
            let type_only = !c.has_meaning(target, meaning::VALUE)
                && cx.program.options.verbatim_module_syntax() == Some(true);
            let spec = style.specifier(&here.path, &f.path);
            let edit = style.import_edit(c, cx.file, &spec, &name, type_only);
            out.push(json!({
                "label": name,
                "kind": item_kind(c, s),
                "sortText": SORT_AUTO_IMPORT,
                "detail": format!("Auto import from '{spec}'"),
                "labelDetails": {"description": spec},
                "additionalTextEdits": [edit],
                "data": {"uri": uri, "o": offset, "k": "a", "n": name, "f": f.path},
            }));
            if out.len() >= MAX_AUTO_IMPORTS {
                return out;
            }
        }
    }
    out
}

/// Whether relative imports in `path` must name the output extension: an ES module under
/// `node16`/`nodenext` resolution (TS2835 otherwise).
pub(super) fn needs_js_extension(options: &CompilerOptions, path: &str) -> bool {
    if !matches!(
        options.effective().resolution_algorithm(),
        ModuleResolution::Node16 | ModuleResolution::NodeNext
    ) {
        return false;
    }
    let fs = RealFs::new();
    let r = Resolver::new(&fs, ResolverOptions::from_compiler_options(options));
    r.file_format(path).0 == ModuleFormat::Esm
}

/// How the current file writes imports (quotes, semicolons, extensions).
pub(super) struct ImportStyle {
    quote: char,
    semi: bool,
    pub(super) js_ext: bool,
}

impl ImportStyle {
    pub(super) fn of(c: &Checker<'_>, file: FileId) -> ImportStyle {
        let f = c.file(file);
        let ast = &f.ast;
        let mut st = ImportStyle {
            quote: '"',
            semi: true,
            js_ext: false,
        };
        let mut first = true;
        if let Node::Program { stmts } = *ast.node(ast.root) {
            for &s in ast.list(stmts) {
                let Node::ImportDecl { source, .. } = *ast.node(s) else {
                    continue;
                };
                let src = ast.span(source).text(&f.text);
                if first {
                    st.quote = if src.starts_with('\'') { '\'' } else { '"' };
                    st.semi = ast.span(s).text(&f.text).trim_end().ends_with(';');
                    first = false;
                }
                let inner = src.trim_matches(|ch| ch == '"' || ch == '\'');
                if inner.starts_with('.') && inner.ends_with(".js") {
                    st.js_ext = true;
                }
            }
        }
        st
    }

    /// Relative specifier from `from` (a file) to `to`.
    fn specifier(&self, from: &str, to: &str) -> String {
        let from_dir: Vec<&str> = from
            .rsplit_once('/')
            .map_or("", |x| x.0)
            .split('/')
            .collect();
        let to_parts: Vec<&str> = to.split('/').collect();
        let mut common = 0;
        while common < from_dir.len()
            && common + 1 < to_parts.len()
            && from_dir[common] == to_parts[common]
        {
            common += 1;
        }
        let mut s = String::new();
        if common == from_dir.len() {
            s.push_str("./");
        } else {
            for _ in common..from_dir.len() {
                s.push_str("../");
            }
        }
        s.push_str(&to_parts[common..].join("/"));
        let stem = ["d.ts", "tsx", "ts", "mts", "cts", "jsx", "js", "mjs", "cjs"]
            .iter()
            .find_map(|e| {
                s.strip_suffix(&format!(".{e}"))
                    .map(|x| (x.to_string(), *e))
            });
        match stem {
            Some((base, ext)) if self.js_ext => {
                let js = match ext {
                    "mts" => "mjs",
                    "cts" => "cjs",
                    _ => "js",
                };
                format!("{base}.{js}")
            }
            Some((base, _)) => base
                .strip_suffix("/index")
                .map(str::to_string)
                .unwrap_or(base),
            None => s,
        }
    }

    /// The edit adding `name` to an import of `spec` (an existing one, or a new line after
    /// the last import).
    fn import_edit(
        &self,
        c: &Checker<'_>,
        file: FileId,
        spec: &str,
        name: &str,
        type_only: bool,
    ) -> Value {
        let f = c.file(file);
        let ast = &f.ast;
        let lines = LspLines::new(&f.text);
        let mut last_import_end = None;
        let mut directives_end = None;
        if let Node::Program { stmts } = *ast.node(ast.root) {
            for &s in ast.list(stmts) {
                match *ast.node(s) {
                    Node::ImportDecl {
                        source,
                        specifiers,
                        type_only: decl_type_only,
                        ..
                    } => {
                        last_import_end = Some(ast.span(s).end);
                        let src = ast.span(source).text(&f.text);
                        let inner = src.trim_matches(|ch| ch == '"' || ch == '\'');
                        if inner != spec || decl_type_only && !type_only {
                            continue;
                        }
                        // Add to `{ ... }` when the import has named specifiers.
                        let named: Vec<NodeId> = ast
                            .list(specifiers)
                            .iter()
                            .copied()
                            .filter(|&x| matches!(ast.node(x), Node::ImportSpecifier { .. }))
                            .collect();
                        if let Some(&last) = named.last() {
                            let at = ast.span(last).end;
                            let t = if type_only && !decl_type_only {
                                format!(", type {name}")
                            } else {
                                format!(", {name}")
                            };
                            return json!({"range": lines.range(&f.text, at, at), "newText": t});
                        }
                    }
                    Node::Directive { .. } => directives_end = Some(ast.span(s).end),
                    _ => {}
                }
            }
        }
        let q = self.quote;
        let semi = if self.semi { ";" } else { "" };
        let kw = if type_only { "import type" } else { "import" };
        let line = format!("{kw} {{ {name} }} from {q}{spec}{q}{semi}");
        match last_import_end.or(directives_end) {
            Some(end) => {
                // After the statement's line.
                let eol = f.text[end as usize..]
                    .find('\n')
                    .map_or(f.text.len(), |i| end as usize + i + 1)
                    as u32;
                let text = if eol as usize == f.text.len() && !f.text.ends_with('\n') {
                    format!("\n{line}")
                } else {
                    format!("{line}\n")
                };
                json!({"range": lines.range(&f.text, eol, eol), "newText": text})
            }
            None => json!({"range": lines.range(&f.text, 0, 0), "newText": format!("{line}\n")}),
        }
    }
}

/// `completionItem/resolve`: the detail line (as hover shows it) and documentation.
pub fn resolve(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, item: &Value) -> Value {
    let c = cx.checker;
    let mut out = item.clone();
    let data = &item["data"];
    let (Some(kind), Some(name), Some(offset)) = (
        data["k"].as_str(),
        data["n"].as_str(),
        data["o"].as_u64().map(|o| o as u32),
    ) else {
        return out;
    };
    let text_len = c.file(cx.file).text.len() as u32;
    let offset = offset.min(text_len);
    let sym = match kind {
        "m" => match context(ck, cx.file, offset).0 {
            Context::Member { obj, qualified } => members(ck, Loc::new(cx.file, obj), qualified)
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|x| x.1),
            _ => None,
        },
        "s" => {
            let anchor = node_at(c.ast(cx.file), offset).unwrap_or(c.ast(cx.file).root);
            scope_names(c, Loc::new(cx.file, anchor))
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|x| x.1)
        }
        "g" => c.global_symbol(name, meaning::ALL),
        "a" => {
            let path = data["f"].as_str().unwrap_or("");
            c.files.iter().position(|f| f.path == path).and_then(|i| {
                let fid = FileId(i as u32);
                let m = c.sym(fid, c.file(fid).bound.file_symbol);
                c.export_of(m, name)
            })
        }
        _ => None,
    };
    let Some(s) = sym else {
        return out;
    };
    let detail = describe(ck, s, None);
    let target = c.resolve_alias(s).unwrap_or(s);
    let docs = docs_of(ck, target);
    let detail = match item["detail"].as_str() {
        Some(auto) if kind == "a" => format!("{auto}\n{detail}"),
        _ => detail,
    };
    out["detail"] = json!(detail);
    if !docs.is_empty() {
        out["documentation"] = json!({"kind": "markdown", "value": docs});
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_specifiers() {
        let st = ImportStyle {
            quote: '"',
            semi: true,
            js_ext: false,
        };
        assert_eq!(st.specifier("/p/src/a.ts", "/p/src/b.ts"), "./b");
        assert_eq!(st.specifier("/p/src/x/a.ts", "/p/src/b.tsx"), "../b");
        assert_eq!(st.specifier("/p/src/a.ts", "/p/src/lib/index.ts"), "./lib");
        let js = ImportStyle { js_ext: true, ..st };
        assert_eq!(js.specifier("/p/src/a.ts", "/p/src/lib/x.ts"), "./lib/x.js");
        assert_eq!(js.specifier("/p/a/b/c.ts", "/p/d/e.mts"), "../../d/e.mjs");
    }
}
