//! Refactors at a selection, as tsserver 6.0.3 offers them on the `refactor-*` fixtures
//! (names, titles and edits observed, not taken from its source):
//!
//! | Refactor / action | Offered at | Result |
//! |---|---|---|
//! | `Infer function return type` | a function's head (name, keyword, parameters) without a return type, not a constructor or setter | `: T` after the parameter list |
//! | `Convert import` / `Convert named imports to namespace import` | an `import { a, b }` | `import * as m`, uses become `m.a` |
//! | `Convert import` / `Convert namespace import to named imports` | an `import * as m` used only as `m.x` | `import { x }`, uses become `x` |
//! | `Extract Symbol` / `constant_scope_N` | a selected expression | `const newLocal = <expr>;` before the statement of scope N; in a class, `private readonly newProperty = <expr>;` before the member (`static` from a static member), used as `this.newProperty` / `C.newProperty` |
//! | `Extract Symbol` / `function_scope_N` | selected statements or an expression | module: `function newFunction(<locals>)` appended to the file; a function: an inner `function newFunction` after the last statement of its body (its own locals stay captured); a class: `private [static] newMethod` after the member, called as `this.`/`C.` |
//! | `Move to a new file` | selected top-level declarations | a file named after the first one, imports on both sides |
//!
//! Scopes are numbered from the innermost enclosing function (with a block body) or class
//! outwards, the module or global scope last, as tsserver numbers them: arrows with an
//! expression body and static blocks are not scopes. Scopes we can't extract to are skipped
//! (their numbers stay taken). Titles: `Extract to constant in enclosing scope` (scope 0),
//! `... in function 'f'` / `method 'm'` / `'get x'` / `constructor` / `arrow function` /
//! `anonymous function`, `Extract to readonly field in class 'C'`, `Extract to inner
//! function in ...`, `Extract to method in class 'C'` / `anonymous class expression`,
//! `... in module scope` (`global scope` in scripts).
//!
//! Extract to function: parameters are the locals out of reach of the new declaration, in
//! order of first use, typed as used; type parameters out of reach that they mention are
//! copied (`newMethod<T>(a: T)`, called as `this.newMethod<T>(a)`). An expression with a
//! contextual type it fits gives the function that return type (its own type when the
//! context is generic). Variables declared in the range and used after it are returned.
//! Not offered: `return`/`yield`/jumps leaving the range, `arguments`, `this` outside a
//! class scope, writes to passed locals, imports/exports, declarations used outside the
//! range. Where tsserver 6.0.3's result is wrong we differ (fixtures marked `@differs`):
//! an expression using an expression-bodied arrow's parameters gets the arrow a block body
//! (`(a) => { const newLocal = a * 2; return newLocal + 1; }`), static blocks extract to
//! static members, `this` is never moved into a function declaration.

use super::edit::Edit;
use super::fixes::node_covering;
use super::{Action, Ctx, Request, imports, style};
use std::collections::{BTreeMap, HashSet};
use ts_r_ast::{Ast, FnFlags, MethodKind, Modifiers, Node, NodeId, Span, VarKind};
use ts_r_binder::{BoundFile, SymbolId};
use ts_r_checker::{Ck, Loc};
use ts_r_types::TypeFlags;

const INFER: &str = "Infer function return type";
const CONVERT: &str = "Convert import";
const EXTRACT: &str = "Extract Symbol";
const MOVE: &str = "Move to a new file";
const MOVE_FILE: &str = "Move to file";

/// Refactors applicable at the request's selection, in tsserver's order.
pub fn refactors(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    let f = cx.checker.file(cx.file);
    if f.bound.is_declaration {
        return;
    }
    let first = out.len();
    if req.wants("refactor.rewrite.function.returnType")
        && let Some(a) = infer_return(cx, ck, req)
    {
        out.push(a);
    }
    if req.wants("refactor.rewrite.import") {
        convert_import(cx, req, out);
    }
    if req.start < req.end && req.wants("refactor.extract") {
        extract(cx, ck, req, out);
    }
    super::extract_type::extract_type(cx, req, out);
    super::exports::convert_export(cx, req, out);
    super::rewrite::rewrites(cx, ck, req, out);
    super::functions::refactors(cx, ck, req, out);
    // An interactive client asks the user for Move to file's target, then asks again.
    let ask_target =
        req.wants("refactor.move.file") && req.target_file.is_none() && req.interactive;
    if (req.wants("refactor.move.newFile") || ask_target)
        && let Some(a) = move_statements(cx, req, Dest::NewFile)
    {
        if ask_target {
            let mut m = Action::new(MOVE_FILE, "refactor.move.file");
            m.refactor = Some(format!("{MOVE_FILE}/{MOVE_FILE}"));
            m.interactive = true;
            out.push(m);
        }
        if req.wants("refactor.move.newFile") {
            out.push(a);
        }
    }
    if req.wants("refactor.move.file")
        && let Some(t) = &req.target_file
        && let Some(a) = move_statements(cx, req, Dest::File(t))
    {
        out.push(a);
    }
    out[first..].sort_by_key(|a| menu_rank(a.refactor.as_deref().unwrap_or("")));
}

/// tsserver's order of refactors in the menu (observed; refactors never offered together
/// sit anywhere between their neighbours).
fn menu_rank(refactor: &str) -> usize {
    const ORDER: [&str; 15] = [
        "Convert export",
        "Convert import",
        "Extract type",
        "Inline variable",
        "Move to file",
        "Move to a new file",
        "Add or remove braces in an arrow function",
        "Convert arrow function or function expression",
        "Convert parameters to destructured object",
        "Convert to template string",
        "Convert to optional chain expression",
        "Generate 'get' and 'set' accessors",
        "Convert overload list to single signature",
        "Extract Symbol",
        "Infer function return type",
    ];
    let name = refactor.split_once('/').map_or(refactor, |(n, _)| n);
    ORDER.iter().position(|o| *o == name).unwrap_or(ORDER.len())
}

fn refactor_action(
    cx: &Ctx<'_, '_>,
    title: impl Into<String>,
    kind: &str,
    name: &str,
    action: &str,
    edits: Vec<Edit>,
) -> Action {
    let mut a = Action::new(title, kind);
    a.refactor = Some(format!("{name}/{action}"));
    a.edit(&cx.checker.file(cx.file).path, edits);
    a
}

// ---- shared helpers ----

/// Visits `root` and its descendants in source order; `f` returns whether to descend.
pub(super) fn walk(ast: &Ast, root: NodeId, f: &mut impl FnMut(NodeId) -> bool) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if !f(n) {
            continue;
        }
        let at = stack.len();
        ast.for_each_child(n, |c| stack.push(c));
        stack[at..].reverse();
    }
}

fn is_function_like(ast: &Ast, n: NodeId) -> bool {
    matches!(
        ast.node(n),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

/// Names declared in any scope of the file, and the free names it uses: a new name must
/// avoid all of them.
pub(super) fn taken_names(ast: &Ast, b: &BoundFile) -> HashSet<String> {
    let mut names = HashSet::new();
    for sc in &b.scopes {
        for &(n, _) in b.table(sc.locals) {
            names.insert(b.name_text(ast, n).to_string());
        }
    }
    for r in &b.refs.free {
        names.insert(b.name_text(ast, r.name).to_string());
    }
    names
}

/// `base`, or `base_1`, `base_2`, ... when taken.
pub(super) fn unique(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (1..)
        .map(|i| format!("{base}_{i}"))
        .find(|n| !taken.contains(n))
        .unwrap_or_else(|| base.to_string())
}

/// Start of a statement with the comments directly above it.
pub(super) fn leading_start(ast: &Ast, text: &str, n: NodeId) -> u32 {
    let start = ast.span(n).start;
    let ls = style::line_start(text, start);
    if !style::blank(text, ls, start) {
        return start;
    }
    let mut cur = start;
    for c in ast.comments_in(Span::new(0, start)).iter().rev() {
        let gap = &text[c.span.end as usize..cur as usize];
        if !gap.trim().is_empty() || gap.matches('\n').count() > 1 {
            break;
        }
        let cl = style::line_start(text, c.span.start);
        if !style::blank(text, cl, c.span.start) {
            break;
        }
        cur = c.span.start;
    }
    cur
}

/// The selection without surrounding whitespace.
fn trimmed(text: &str, start: u32, end: u32) -> (u32, u32) {
    let s = &text[start as usize..end as usize];
    let lead = s.len() - s.trim_start().len();
    let trail = s.len() - s.trim_end().len();
    if lead == s.len() {
        return (start, start);
    }
    (start + lead as u32, end - trail as u32)
}

/// Re-indents the lines of `text[start..end]` after the first from `from` to `to`, except
/// lines that start inside a template literal (their indentation is part of the string).
fn reindent(text: &str, start: u32, end: u32, from: &str, to: &str, keep: &[Span]) -> String {
    let mut out = String::new();
    let mut at = start;
    for (i, line) in text[start as usize..end as usize].split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            let verbatim = keep.iter().any(|k| k.start < at && at < k.end);
            if verbatim {
                out.push_str(line);
            } else if !line.trim().is_empty() {
                out.push_str(to);
                out.push_str(line.strip_prefix(from).unwrap_or(line.trim_start()));
            }
        } else {
            out.push_str(line);
        }
        at += line.len() as u32 + 1;
    }
    out
}

/// Spans of the template literals under `roots`.
fn template_spans(ast: &Ast, roots: &[NodeId]) -> Vec<Span> {
    let mut out = Vec::new();
    for &r in roots {
        walk(ast, r, &mut |n| {
            if matches!(ast.node(n), Node::TemplateLit { .. }) {
                out.push(ast.span(n));
            }
            true
        });
    }
    out
}

// ---- Infer function return type ----

fn infer_return(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>) -> Option<Action> {
    let f = cx.checker.file(cx.file);
    // JavaScript has no type annotations.
    if f.is_js() {
        return None;
    }
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    let n = super::super::locate::node_at(ast, req.start)?;
    let mut func = None;
    // A constructor or setter takes no return type (TS1093/TS1095).
    let no_return_type = |m: NodeId| {
        matches!(
            *ast.node(m),
            Node::MethodDef {
                kind: MethodKind::Constructor | MethodKind::Set,
                ..
            }
        )
    };
    for a in std::iter::once(n).chain(parents.ancestors(n)) {
        match *ast.node(a) {
            Node::MethodDef { .. } if no_return_type(a) => return None,
            Node::FunctionExpr { .. } if parents.parent(a).is_some_and(no_return_type) => {
                return None;
            }
            Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => {
                let body = body?;
                if req.start >= ast.span(body).start {
                    return None;
                }
                func = Some(a);
                break;
            }
            Node::Arrow { body, .. } => {
                if req.start >= ast.span(body).start {
                    return None;
                }
                func = Some(a);
                break;
            }
            Node::MethodDef { value, .. } => {
                func = Some(value);
                break;
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if ast.span(name).start <= req.start
                && req.start <= ast.span(name).end
                && matches!(
                    ast.node(init),
                    Node::Arrow { .. } | Node::FunctionExpr { .. }
                ) =>
            {
                func = Some(init);
                break;
            }
            Node::Block { .. }
            | Node::Program { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::ObjectLit { .. }
            | Node::ExprStmt { .. }
            | Node::Return { .. } => return None,
            _ => {}
        }
    }
    let func = func?;
    let (params, return_type, body, arrow) = match *ast.node(func) {
        Node::FunctionDecl {
            params,
            return_type,
            body: Some(body),
            ..
        }
        | Node::FunctionExpr {
            params,
            return_type,
            body: Some(body),
            ..
        } => (params, return_type, body, false),
        Node::Arrow {
            params,
            return_type,
            body,
            ..
        } => (params, return_type, body, true),
        _ => return None,
    };
    if return_type.is_some() {
        return None;
    }
    let sig = ck.signature_from_decl(Loc::new(cx.file, func));
    let t = ck.return_type_of(sig);
    let ty = ck.type_to_string(t);
    if !printable(&ty) {
        return None;
    }
    let fs = ast.span(func).start;
    let mut head = &text[fs as usize..ast.span(body).start as usize];
    if arrow {
        head = &head[..head.rfind("=>")?];
    }
    let edits = match head.rfind(')') {
        Some(i) => vec![Edit::insert(fs + i as u32 + 1, format!(": {ty}"))],
        None => {
            let &[p] = ast.list(params) else {
                return None;
            };
            let sp = ast.span(p);
            vec![
                Edit::insert(sp.start, "("),
                Edit::insert(sp.end, format!("): {ty}")),
            ]
        }
    };
    Some(refactor_action(
        cx,
        INFER,
        "refactor.rewrite.function.returnType",
        INFER,
        INFER,
        edits,
    ))
}

// ---- Convert import ----

fn convert_import(cx: &Ctx<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    // The token at the cursor, its leading whitespace and comments included.
    let q = super::exports::token_start(ast, text, req.start);
    let e = super::exports::prev_token_end(ast, text, req.end).max(q);
    let Some(stmt) = style::top_level(ast).into_iter().find(|&s| {
        let sp = ast.span(s);
        sp.start <= q
            && q < sp.end.max(sp.start + 1)
            && e <= sp.end
            && matches!(ast.node(s), Node::ImportDecl { .. })
    }) else {
        return;
    };
    // On a cursor move, only a selection of the whole statement.
    if !req.invoked {
        let sp = ast.span(stmt);
        if req.start == req.end
            || q > sp.start
            || super::exports::prev_token_end(ast, text, req.end) < sp.end
        {
            return;
        }
    }
    let Node::ImportDecl {
        has_clause: true,
        specifiers,
        source,
        ..
    } = *ast.node(stmt)
    else {
        return;
    };
    let specs = ast.list(specifiers);
    let ns = specs
        .iter()
        .copied()
        .find(|&s| matches!(ast.node(s), Node::ImportNamespaceSpecifier { .. }));
    let named: Vec<NodeId> = specs
        .iter()
        .copied()
        .filter(|&s| matches!(ast.node(s), Node::ImportSpecifier { .. }))
        .collect();
    if let Some(ns) = ns {
        if req.wants("refactor.rewrite.import.named")
            && let Some(edits) = namespace_to_named(cx, stmt, ns)
        {
            let t = "Convert namespace import to named imports";
            out.push(refactor_action(
                cx,
                t,
                "refactor.rewrite.import.named",
                CONVERT,
                t,
                edits,
            ));
        }
        return;
    }
    if named.is_empty() {
        return;
    }
    // A module with `export =` gets a default import (its namespace is not a module record).
    if export_equals(cx, source) {
        let has_default = specs
            .iter()
            .any(|&s| matches!(ast.node(s), Node::ImportDefaultSpecifier { .. }));
        if !has_default
            && req.wants("refactor.rewrite.import.default")
            && let Some(edits) = named_to_namespace(cx, stmt, source, &named, true)
        {
            let t = "Convert named imports to default import";
            out.push(refactor_action(
                cx,
                t,
                "refactor.rewrite.import.default",
                CONVERT,
                t,
                edits,
            ));
        }
    } else if req.wants("refactor.rewrite.import.namespace")
        && let Some(edits) = named_to_namespace(cx, stmt, source, &named, false)
    {
        let t = "Convert named imports to namespace import";
        out.push(refactor_action(
            cx,
            t,
            "refactor.rewrite.import.namespace",
            CONVERT,
            t,
            edits,
        ));
    }
}

/// Whether the module of import specifier `source` is written `export = x`.
fn export_equals(cx: &Ctx<'_, '_>, source: NodeId) -> bool {
    let b = &cx.checker.file(cx.file).bound;
    let Some(ri) = b.module_requests.iter().position(|r| r.node == source) else {
        return false;
    };
    let target = cx.checker.link.module_target(cx.file, ri as u32);
    target != ts_r_binder::program::ModuleTarget::Unresolved
        && cx.checker.link.resolve_export(target, "export=").is_some()
}

/// An identifier for module `spec`: its last segment, camel-cased (`lodash-es` → `lodashEs`).
fn module_ident(spec: &str) -> String {
    let last = spec
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(spec);
    let last = [
        ".d.ts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs",
    ]
    .iter()
    .find_map(|e| last.strip_suffix(e))
    .unwrap_or(last);
    let mut out = String::new();
    let mut upper = false;
    for ch in last.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            if upper && !out.is_empty() {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
            upper = false;
        } else {
            upper = true;
        }
    }
    if out.is_empty() {
        return "module".into();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) || is_reserved_word(&out) {
        out.insert(0, '_');
    }
    out
}

fn named_to_namespace(
    cx: &Ctx<'_, '_>,
    stmt: NodeId,
    source: NodeId,
    named: &[NodeId],
    default: bool,
) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    let parents = f.parents();
    let ssp = ast.span(stmt);
    let ns = unique(&module_ident(ast.name_of(source)?), &taken_names(ast, b));
    let mut edits = Vec::new();
    for &sp in named {
        let Node::ImportSpecifier {
            imported, local, ..
        } = *ast.node(sp)
        else {
            return None;
        };
        let prop = ast.name_of(imported.unwrap_or(local))?;
        if !super::super::complete::is_identifier_name(prop) {
            return None;
        }
        let local_name = ast.name_of(local)?;
        let sym = b.symbol_of_decl(sp).or_else(|| b.symbol_of_decl(local))?;
        for &(n, s) in &b.refs.resolved {
            let nsp = ast.span(n);
            if s != sym || (ssp.start <= nsp.start && nsp.end <= ssp.end) {
                continue;
            }
            match parents.parent(n).map(|p| ast.node(p)) {
                Some(Node::ShorthandProp {
                    name, init: None, ..
                }) if *name == n => {
                    edits.push(Edit::new(
                        nsp.start,
                        nsp.end,
                        format!("{local_name}: {ns}.{prop}"),
                    ));
                }
                Some(Node::ExportSpecifier { .. } | Node::ShorthandProp { .. }) => return None,
                Some(Node::JsxOpening { name, .. }) if *name == n => {
                    edits.push(Edit::new(nsp.start, nsp.end, format!("{ns}.{prop}")));
                    // The closing tag names it too.
                    let closing =
                        parents
                            .parent(n)
                            .and_then(|o| parents.parent(o))
                            .and_then(|el| match *ast.node(el) {
                                Node::JsxElement {
                                    closing: Some(c), ..
                                } => Some(c),
                                _ => None,
                            });
                    if let Some(c) = closing
                        && let Node::JsxClosing { name: cn } = *ast.node(c)
                        && matches!(ast.node(cn), Node::Ident { .. })
                    {
                        let csp = ast.span(cn);
                        edits.push(Edit::new(csp.start, csp.end, format!("{ns}.{prop}")));
                    }
                }
                _ => edits.push(Edit::new(nsp.start, nsp.end, format!("{ns}.{prop}"))),
            }
        }
    }
    let first = ast.span(named[0]).start;
    let last = ast.span(*named.last()?).end;
    let open = ssp.start + text[ssp.start as usize..first as usize].rfind('{')? as u32;
    let close = last + text[last as usize..ssp.end as usize].find('}')? as u32 + 1;
    let clause = if default { ns } else { format!("* as {ns}") };
    edits.push(Edit::new(open, close, clause));
    Some(edits)
}

fn namespace_to_named(cx: &Ctx<'_, '_>, stmt: NodeId, ns: NodeId) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    let parents = f.parents();
    let ssp = ast.span(stmt);
    let Node::ImportNamespaceSpecifier { local } = *ast.node(ns) else {
        return None;
    };
    let sym = b.symbol_of_decl(ns).or_else(|| b.symbol_of_decl(local))?;
    let mut uses: Vec<(u32, u32, &str)> = Vec::new();
    for &(n, s) in &b.refs.resolved {
        let nsp = ast.span(n);
        if s != sym || (ssp.start <= nsp.start && nsp.end <= ssp.end) {
            continue;
        }
        let p = parents.parent(n)?;
        let prop = match *ast.node(p) {
            Node::Member {
                obj,
                prop,
                optional: false,
            } if obj == n => prop,
            Node::QualifiedName { left, right } if left == n => right,
            _ => return None,
        };
        let psp = ast.span(p);
        uses.push((
            psp.start,
            psp.end,
            super::super::locate::name_text(ast, prop)?,
        ));
    }
    uses.sort_unstable();
    let mut taken = taken_names(ast, b);
    taken.remove(ast.name_of(local)?);
    let mut locals: BTreeMap<&str, String> = BTreeMap::new();
    let mut order: Vec<&str> = Vec::new();
    // `ns.default` can't become a local named `default`.
    if uses.iter().any(|&(_, _, name)| is_reserved_word(name)) {
        return None;
    }
    for &(_, _, name) in &uses {
        if !locals.contains_key(name) {
            let l = unique(name, &taken);
            taken.insert(l.clone());
            locals.insert(name, l);
            order.push(name);
        }
    }
    let mut edits: Vec<Edit> = uses
        .iter()
        .map(|&(s, e, name)| Edit::new(s, e, locals[name].clone()))
        .collect();
    let list: Vec<String> = order
        .iter()
        .map(|&n| {
            if locals[n] == n {
                n.to_string()
            } else {
                format!("{n} as {}", locals[n])
            }
        })
        .collect();
    let lsp = ast.span(local);
    let star = ssp.start + text[ssp.start as usize..lsp.start as usize].rfind('*')? as u32;
    let clause = if list.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {} }}", list.join(", "))
    };
    edits.push(Edit::new(star, lsp.end, clause));
    Some(edits)
}

/// A reserved word: not a binding name in a module (strict mode, `await` included).
pub(super) fn is_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
            | "await"
    )
}

// ---- Extract Symbol ----

/// A scope a selection can be extracted to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// A function with a block body (arrows with an expression body are not scopes).
    Function(NodeId),
    /// A class (from inside one of its members).
    Class(NodeId),
    Module,
}

/// A local of the enclosing region used in the selection but declared outside it.
#[derive(Clone, Copy)]
struct OuterRef {
    /// Its first use in the selection.
    id: NodeId,
    sym: SymbolId,
    /// A variable, parameter or function (not a type, class, enum or namespace).
    value: bool,
    type_param: bool,
    written: bool,
}

/// What a selection uses and does.
#[derive(Default)]
struct Usage {
    /// References to locals declared inside the anchor but outside the range (first use
    /// order).
    outer: Vec<OuterRef>,
    /// Some outer local is a type (type parameter, local type) or is written.
    blocked: bool,
    this_like: bool,
    /// `arguments` of the enclosing function.
    arguments: bool,
    /// `#private` names.
    private_names: bool,
    has_await: bool,
    has_yield: bool,
    has_return: bool,
    has_jump: bool,
}

fn usage(cx: &Ctx<'_, '_>, s: u32, e: u32, roots: &[NodeId], anchor: Span) -> Usage {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let b = &f.bound;
    let parents = f.parents();
    let mut u = Usage::default();
    for &root in roots {
        // Depth of loops/switches and of non-arrow functions inside the range.
        walk(ast, root, &mut |n| {
            let nested_fn = parents
                .ancestors(n)
                .take_while(|&a| a != root && ast.span(a).start >= s)
                .any(|a| is_function_like(ast, a) && !matches!(ast.node(a), Node::Arrow { .. }));
            let nested_any_fn = parents
                .ancestors(n)
                .take_while(|&a| a != root && ast.span(a).start >= s)
                .any(|a| is_function_like(ast, a));
            match *ast.node(n) {
                Node::This {} | Node::Super {} if !nested_fn => u.this_like = true,
                Node::Ident { name } if !nested_fn && ast.atom(name) == "arguments" => {
                    if b.resolved(n).is_none() {
                        u.this_like = true;
                        u.arguments = true;
                    }
                }
                Node::PrivateName { .. } => u.private_names = true,
                Node::Await { .. } if !nested_any_fn => u.has_await = true,
                Node::ForOf { is_await: true, .. } if !nested_any_fn => u.has_await = true,
                Node::Yield { .. } if !nested_any_fn => u.has_yield = true,
                Node::Return { .. } if !nested_any_fn => u.has_return = true,
                Node::Break { .. } | Node::Continue { .. } if !nested_any_fn => {
                    let inside_loop = parents
                        .ancestors(n)
                        .take_while(|&a| ast.span(a).start >= s && ast.span(a).end <= e)
                        .any(|a| {
                            matches!(
                                ast.node(a),
                                Node::For { .. }
                                    | Node::ForIn { .. }
                                    | Node::ForOf { .. }
                                    | Node::While { .. }
                                    | Node::DoWhile { .. }
                                    | Node::Switch { .. }
                            )
                        });
                    let labeled = matches!(
                        ast.node(n),
                        Node::Break { label: Some(_) } | Node::Continue { label: Some(_) }
                    );
                    if !inside_loop || labeled {
                        u.has_jump = true;
                    }
                }
                _ => {}
            }
            if let Some(sym) = b.resolved(n) {
                let decl_out = b.decls(sym).iter().any(|&d| {
                    let dsp = ast.span(d);
                    anchor.start <= dsp.start
                        && dsp.end <= anchor.end
                        && !(s <= dsp.start && dsp.end <= e)
                });
                if decl_out {
                    let flags = b.symbol(sym).flags;
                    use ts_r_binder::SymbolFlags as F;
                    let value_flags = F::FUNCTION_SCOPED_VARIABLE
                        | F::BLOCK_SCOPED_VARIABLE
                        | F::PARAMETER
                        | F::FUNCTION;
                    let value = flags.intersects(value_flags)
                        && !flags.intersects(
                            F::CLASS | F::INTERFACE | F::TYPE_ALIAS | F::TYPE_PARAMETER,
                        );
                    let written = is_write(ast, parents, n);
                    if !value || written {
                        u.blocked = true;
                    }
                    match u.outer.iter_mut().find(|o| o.sym == sym) {
                        Some(o) => o.written |= written,
                        None => u.outer.push(OuterRef {
                            id: n,
                            sym,
                            value,
                            type_param: flags.intersects(F::TYPE_PARAMETER),
                            written,
                        }),
                    }
                }
            }
            true
        });
    }
    u
}

/// Whether identifier `n` is assigned to.
pub(super) fn is_write(ast: &Ast, parents: &ts_r_ast::ParentMap, n: NodeId) -> bool {
    let mut cur = n;
    for p in parents.ancestors(n) {
        match *ast.node(p) {
            Node::Assign { left, .. } => return left == cur,
            Node::Update { .. } => return true,
            Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::ShorthandProp { .. }
            | Node::Property { .. }
            | Node::Spread { .. }
            | Node::Paren { .. } => cur = p,
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == cur,
            _ => return false,
        }
    }
    false
}

fn is_expression(n: &Node) -> bool {
    matches!(
        n,
        Node::Ident { .. }
            | Node::This { .. }
            | Node::NullLit { .. }
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
            | Node::TemplateLit { .. }
            | Node::TaggedTemplate { .. }
            | Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::Paren { .. }
            | Node::Unary { .. }
            | Node::Await { .. }
            | Node::Binary { .. }
            | Node::Conditional { .. }
            | Node::Call { .. }
            | Node::New { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
    )
}

/// The selection as an expression, or as a run of statements of one block.
enum Range {
    Expr(NodeId),
    Stmts(Vec<NodeId>),
}

fn selected_range(cx: &Ctx<'_, '_>, s: u32, e: u32) -> Option<Range> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    let n = node_covering(cx, s, e)?;
    let sp = ast.span(n);
    let p = parents.parent(n)?;
    if sp.start == s && sp.end == e && is_expression(ast.node(n)) {
        // Not a name being declared or a property name.
        if b_declares(cx, n) || !in_expression_context(ast, parents, n) {
            return None;
        }
        match *ast.node(p) {
            Node::Member { prop, .. } if prop == n => return None,
            Node::Property { key, .. } if key == n => return None,
            Node::ExprStmt { .. } if !matches!(ast.node(n), Node::Ident { .. }) => {}
            _ => {}
        }
        if is_write(ast, parents, n) {
            return None;
        }
        return Some(Range::Expr(n));
    }
    let list = match *ast.node(p) {
        Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => stmts,
        _ => return None,
    };
    let stmts: Vec<NodeId> = ast
        .list(list)
        .iter()
        .copied()
        .filter(|&x| {
            let xs = ast.span(x);
            xs.start >= s && xs.end <= e
        })
        .collect();
    let first = *stmts.first()?;
    let last = *stmts.last()?;
    if ast.span(first).start != s {
        return None;
    }
    let rest = &text[ast.span(last).end as usize..e as usize];
    if !rest.trim().trim_matches(';').is_empty() {
        return None;
    }
    Some(Range::Stmts(stmts))
}

/// Whether `n` sits where an expression is evaluated (not in a type, a pattern, a module
/// specifier, a property name, ...).
pub(super) fn in_expression_context(ast: &Ast, parents: &ts_r_ast::ParentMap, n: NodeId) -> bool {
    let mut child = n;
    for p in parents.ancestors(n) {
        let node = ast.node(p);
        let slot = |x: Option<NodeId>| x == Some(child);
        let no = match *node {
            Node::ImportDecl { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ImportEquals { .. }
            | Node::ExternalModuleRef { .. }
            | Node::Module { .. }
            | Node::ImportAttributes { .. }
            | Node::ImportAttribute { .. }
            | Node::Directive { .. }
            | Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::BindingProperty { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. }
            | Node::Labeled { .. }
            | Node::Break { .. }
            | Node::Continue { .. }
            | Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::QualifiedName { .. }
            | Node::ThisType { .. }
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::TypeLiteral { .. }
            | Node::PropertySignature { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::InferType { .. }
            | Node::MappedType { .. }
            | Node::ParenType { .. }
            | Node::TypePredicate { .. }
            | Node::ImportType { .. }
            | Node::TypeParams { .. }
            | Node::TypeParam { .. }
            | Node::TypeArgs { .. }
            | Node::IndexSignature { .. }
            | Node::HeritageClause { .. }
            | Node::Interface { .. }
            | Node::JsxOpening { .. }
            | Node::JsxClosing { .. }
            | Node::JsxNamespacedName { .. }
            | Node::JsxMemberExpr { .. } => true,
            Node::EnumMember { name, .. } => name == child,
            Node::Property { key, .. } | Node::MethodDef { key, .. } => key == child,
            Node::PropertyDef { key, type_ann, .. } => key == child || slot(type_ann),
            Node::ShorthandProp { name, .. } => name == child,
            Node::VarDeclarator { name, type_ann, .. } => name == child || slot(type_ann),
            Node::Param {
                pattern, type_ann, ..
            } => pattern == child || slot(type_ann),
            Node::FunctionDecl { return_type, .. }
            | Node::FunctionExpr { return_type, .. }
            | Node::Arrow { return_type, .. } => slot(return_type),
            Node::TypeAlias { .. } => true,
            Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } => {
                ty == child
            }
            Node::JsxAttr { name, .. } => name == child,
            Node::TaggedTemplate { quasi, .. } => quasi == child,
            Node::Member { prop, .. } => prop == child,
            Node::CatchClause { param, .. } => slot(param),
            _ => false,
        };
        if no {
            return false;
        }
        if node.is_statement() {
            return true;
        }
        child = p;
    }
    true
}

/// A declaration, or a name that isn't a reference (a declaration's name, a label).
fn b_declares(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let b = &cx.checker.file(cx.file).bound;
    if b.symbol_of_decl(n).is_some() {
        return true;
    }
    matches!(cx.checker.file(cx.file).ast.node(n), Node::Ident { .. })
        && b.resolved(n).is_none()
        && !b.refs.free.iter().any(|r| r.node == n)
}

/// Enclosing scopes of `n`, innermost first, the module or global scope last: functions with
/// a block body and classes (from a member); arrows with an expression body and static
/// blocks are not scopes, as tsserver numbers them.
fn scopes_of(cx: &Ctx<'_, '_>, n: NodeId) -> Vec<Scope> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let mut out = Vec::new();
    let at = ast.span(n).start;
    // Only bodies count: a parameter's default is not inside its function's scope here.
    let inside = |b: NodeId| ast.span(b).start <= at && at < ast.span(b).end;
    for a in f.parents().ancestors(n) {
        match *ast.node(a) {
            Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. }
                if inside(b) =>
            {
                out.push(Scope::Function(a));
            }
            Node::Arrow { body: b, flags, .. }
                if inside(b) && !flags.contains(FnFlags::EXPRESSION_BODY) =>
            {
                out.push(Scope::Function(a));
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }
                if ast.list(members).iter().any(|&m| inside(m)) =>
            {
                out.push(Scope::Class(a));
            }
            _ => {}
        }
    }
    out.push(Scope::Module);
    out
}

/// The statement of the innermost block (inside `body`) that contains `n`.
fn innermost_statement(cx: &Ctx<'_, '_>, body: NodeId, n: NodeId) -> Option<NodeId> {
    let f = cx.checker.file(cx.file);
    let parents = f.parents();
    let mut cur = n;
    for p in parents.ancestors(n) {
        if matches!(f.ast.node(p), Node::Block { .. }) {
            return Some(cur);
        }
        if p == body {
            return None;
        }
        cur = p;
    }
    None
}

/// The statement directly in `list_owner`'s statement list that contains `n`.
fn statement_in(cx: &Ctx<'_, '_>, owner: NodeId, n: NodeId) -> Option<NodeId> {
    let parents = cx.checker.file(cx.file).parents();
    let mut cur = n;
    for p in parents.ancestors(n) {
        if p == owner {
            return Some(cur);
        }
        cur = p;
    }
    None
}

/// The block body of a function scope.
fn scope_body(ast: &Ast, func: NodeId) -> Option<NodeId> {
    let body = match *ast.node(func) {
        Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => b,
        Node::Arrow { body, flags, .. } if !flags.contains(FnFlags::EXPRESSION_BODY) => body,
        _ => return None,
    };
    matches!(ast.node(body), Node::Block { .. }).then_some(body)
}

/// The class member holding `n`.
fn member_of(cx: &Ctx<'_, '_>, class: NodeId, n: NodeId) -> Option<NodeId> {
    statement_in(cx, class, n)
}

/// Whether a class member is static (static blocks are).
fn is_static_member(ast: &Ast, m: NodeId) -> bool {
    matches!(ast.node(m), Node::StaticBlock { .. })
        || ast.node(m).modifiers().contains(Modifiers::STATIC)
}

/// How tsserver names a scope in its titles: `function 'f'`, `method 'm'`, `'get x'`,
/// `constructor`, `arrow function`, `anonymous function`, `class 'C'`, `anonymous class
/// expression`.
fn scope_desc(cx: &Ctx<'_, '_>, scope: Scope) -> String {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    match scope {
        Scope::Module => String::new(),
        Scope::Class(c) => {
            let name = match *ast.node(c) {
                Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
                _ => None,
            };
            match name.and_then(|n| ast.name_of(n)) {
                Some(n) => format!("class '{n}'"),
                None if matches!(ast.node(c), Node::ClassExpr { .. }) => {
                    "anonymous class expression".into()
                }
                None => "anonymous class declaration".into(),
            }
        }
        Scope::Function(func) => {
            if let Some(p) = f.parents().parent(func)
                && let Node::MethodDef { kind, key, .. } = *ast.node(p)
            {
                let key = ast.span(key).text(text);
                return match kind {
                    ts_r_ast::MethodKind::Method => format!("method '{key}'"),
                    ts_r_ast::MethodKind::Get => format!("'get {key}'"),
                    ts_r_ast::MethodKind::Set => format!("'set {key}'"),
                    ts_r_ast::MethodKind::Constructor => "constructor".into(),
                };
            }
            match *ast.node(func) {
                Node::FunctionDecl { name: Some(n), .. }
                | Node::FunctionExpr { name: Some(n), .. } => {
                    format!("function '{}'", ast.name_of(n).unwrap_or(""))
                }
                Node::Arrow { .. } => "arrow function".into(),
                _ => "anonymous function".into(),
            }
        }
    }
}

fn scope_title(cx: &Ctx<'_, '_>, scope: Scope, innermost: bool, constant: bool) -> String {
    let module = cx.checker.file(cx.file).bound.is_module;
    match scope {
        Scope::Class(_) if constant => {
            format!("Extract to readonly field in {}", scope_desc(cx, scope))
        }
        Scope::Class(_) => format!("Extract to method in {}", scope_desc(cx, scope)),
        _ if innermost && constant => "Extract to constant in enclosing scope".into(),
        Scope::Module => format!(
            "Extract to {} in {} scope",
            if constant { "constant" } else { "function" },
            if module { "module" } else { "global" }
        ),
        Scope::Function(_) if constant => {
            format!("Extract to constant in {}", scope_desc(cx, scope))
        }
        Scope::Function(_) => format!("Extract to inner function in {}", scope_desc(cx, scope)),
    }
}

fn extract(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    let f = cx.checker.file(cx.file);
    let text = f.text.as_str();
    let (s, e) = trimmed(text, req.start, req.end);
    if s >= e {
        return;
    }
    let Some(range) = selected_range(cx, s, e) else {
        return;
    };
    let first = match &range {
        Range::Expr(n) => *n,
        Range::Stmts(v) => v[0],
    };
    let scopes = scopes_of(cx, first);
    if req.wants("refactor.extract.function") {
        let roots = match &range {
            Range::Expr(n) => vec![*n],
            Range::Stmts(v) => v.clone(),
        };
        for (i, &scope) in scopes.iter().enumerate() {
            if let Some(edits) = extract_function(cx, ck, req, &range, &roots, s, e, &scopes, i) {
                out.push(refactor_action(
                    cx,
                    scope_title(cx, scope, i == 0, false),
                    "refactor.extract.function",
                    EXTRACT,
                    &format!("function_scope_{i}"),
                    edits,
                ));
            }
        }
    }
    if let Range::Expr(expr) = range
        && req.wants("refactor.extract.constant")
    {
        for (i, &scope) in scopes.iter().enumerate() {
            if let Some(edits) = extract_constant(cx, req, expr, &scopes, i) {
                out.push(refactor_action(
                    cx,
                    scope_title(cx, scope, i == 0, true),
                    "refactor.extract.constant",
                    EXTRACT,
                    &format!("constant_scope_{i}"),
                    edits,
                ));
            }
        }
    }
}

fn extract_constant(
    cx: &Ctx<'_, '_>,
    req: &Request<'_>,
    expr: NodeId,
    scopes: &[Scope],
    i: usize,
) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let esp = ast.span(expr);
    // The statement the constant goes before.
    let anchor = match scopes[i] {
        // A bare `this` gains nothing as a field (and its `this` type trips our checker).
        Scope::Class(_) if matches!(ast.node(expr), Node::This {}) => return None,
        Scope::Class(class) => return extract_constant_to_class(cx, expr, class),
        Scope::Module => statement_in(cx, ast.root, expr)?,
        Scope::Function(func) => {
            let body = scope_body(ast, func)?;
            if i == 0 {
                // The enclosing scope: before the statement of the innermost block.
                innermost_statement(cx, body, expr)?
            } else {
                statement_in(cx, body, expr)?
            }
        }
    };
    // Not between an overloaded function's signatures and its implementation.
    if matches!(ast.node(anchor), Node::FunctionDecl { .. })
        && f.bound
            .symbol_of_decl(anchor)
            .is_some_and(|s| f.bound.decls(s).len() > 1)
    {
        return None;
    }
    let asp = ast.span(anchor);
    let u = usage(cx, esp.start, esp.end, &[expr], asp);
    if super::rewrite::extract_hazard(cx, &[expr], anchor, true) {
        return None;
    }
    // Locals declared between the anchor and the expression are out of reach, unless they
    // are parameters of an arrow with an expression body around it, which then gets a block.
    if !u.outer.is_empty() && i == 0 {
        return extract_constant_in_arrow(cx, req, expr, asp);
    }
    if !u.outer.is_empty() || u.blocked || u.has_yield {
        return None;
    }
    if i > 0 && (u.this_like || u.has_await) {
        return None;
    }
    if matches!(scopes[i], Scope::Module) && u.has_await {
        return None;
    }
    let taken = taken_names(ast, &f.bound);
    let name = unique("newLocal", &taken);
    let lead = leading_start(ast, text, anchor);
    let ls = style::line_start(text, lead);
    let indent = style::line_indent(text, asp.start);
    let nl = style::newline(text);
    let semi = style::semi(ast, text);
    let src = esp.text(text);
    // A line may start with an ASI guard (`;(x as T).f()`): that still counts as its own line.
    let own_line = text[ls as usize..lead as usize]
        .chars()
        .all(|c| c.is_whitespace() || c == ';');
    let insert = if own_line {
        Edit::insert(ls, format!("{indent}const {name} = {src}{semi}{nl}"))
    } else {
        // The statement shares its line (`{ return x; }`): the constant goes right before it.
        Edit::insert(lead, format!("const {name} = {src}; "))
    };
    Some(vec![insert, Edit::new(esp.start, esp.end, name)])
}

/// The constant in the innermost arrow with an expression body around `expr`, whose body
/// becomes a block: `(a) => { const newLocal = a * 2; return newLocal + 1; }`. (tsserver
/// 6.0.3 puts the constant before the statement, out of reach of the arrow's parameters.)
fn extract_constant_in_arrow(
    cx: &Ctx<'_, '_>,
    req: &Request<'_>,
    expr: NodeId,
    anchor: Span,
) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let esp = ast.span(expr);
    let arrow = f.parents().ancestors(expr).find(|&a| {
        matches!(ast.node(a), Node::Arrow { flags, .. } if flags.contains(FnFlags::EXPRESSION_BODY))
    })?;
    let asp_arrow = ast.span(arrow);
    if asp_arrow.start < anchor.start || asp_arrow.end > anchor.end {
        return None;
    }
    let Node::Arrow { body, flags, .. } = *ast.node(arrow) else {
        return None;
    };
    // Everything the expression uses must be in reach at the start of the arrow's body.
    let bsp = ast.span(body);
    let inner = usage(cx, esp.start, esp.end, &[expr], bsp);
    if !inner.outer.is_empty() || inner.has_yield {
        return None;
    }
    if inner.has_await && !flags.contains(FnFlags::ASYNC) {
        return None;
    }
    let taken = taken_names(ast, &f.bound);
    let name = unique("newLocal", &taken);
    let nl = style::newline(text);
    let semi = style::semi(ast, text);
    let unit = req.indent_unit(text);
    let ind = style::line_indent(text, asp_arrow.start);
    // `({ ... })` returns the object literal without its parentheses.
    let (rs, re) = match *ast.node(body) {
        Node::Paren { expr: o } if matches!(ast.node(o), Node::ObjectLit { .. }) => {
            let o = ast.span(o);
            (o.start, o.end)
        }
        _ => (bsp.start, bsp.end),
    };
    if esp.start < rs || esp.end > re {
        return None;
    }
    let ret = format!(
        "{}{name}{}",
        &text[rs as usize..esp.start as usize],
        &text[esp.end as usize..re as usize]
    );
    let block = format!(
        "{{{nl}{ind}{unit}const {name} = {}{semi}{nl}{ind}{unit}return {ret}{semi}{nl}{ind}}}",
        esp.text(text)
    );
    Some(vec![Edit::new(bsp.start, bsp.end, block)])
}

/// Names of a class's members.
fn member_names<'a>(ast: &'a Ast, text: &'a str, class: NodeId) -> HashSet<String> {
    let members = match *ast.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
        _ => return HashSet::new(),
    };
    ast.list(members)
        .iter()
        .filter_map(|&m| match *ast.node(m) {
            Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => {
                Some(ast.span(key).text(text).to_string())
            }
            _ => None,
        })
        .collect()
}

/// `this.` or `C.` for a member of `class` used from `member`.
/// `None` when `this` at `from` is not the instance (a `function` or another class between
/// `from` and `member`).
fn member_receiver(
    cx: &Ctx<'_, '_>,
    class: NodeId,
    member: NodeId,
    from: NodeId,
) -> Option<String> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    if !is_static_member(ast, member) {
        let parents = f.parents();
        for a in std::iter::once(from).chain(parents.ancestors(from)) {
            if a == member {
                return Some("this".into());
            }
            let own_function = parents.parent(a) == Some(member)
                && matches!(ast.node(member), Node::MethodDef { .. });
            if !own_function
                && matches!(
                    ast.node(a),
                    Node::FunctionDecl { .. }
                        | Node::FunctionExpr { .. }
                        | Node::MethodDef { .. }
                        | Node::ClassDecl { .. }
                        | Node::ClassExpr { .. }
                )
            {
                return None;
            }
        }
        return None;
    }
    let name = match *ast.node(class) {
        Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
        _ => None,
    };
    ast.name_of(name?).map(str::to_string)
}

/// `private readonly newProperty = <expr>;` before the member holding `expr`.
fn extract_constant_to_class(cx: &Ctx<'_, '_>, expr: NodeId, class: NodeId) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    // Observed: not offered in JavaScript (no `private readonly`).
    if f.is_js() {
        return None;
    }
    let ast = &f.ast;
    let text = f.text.as_str();
    let esp = ast.span(expr);
    let member = member_of(cx, class, expr)?;
    let msp = ast.span(member);
    let u = usage(cx, esp.start, esp.end, &[expr], msp);
    if !u.outer.is_empty() || u.blocked || u.has_yield || u.has_await || u.arguments {
        return None;
    }
    let recv = member_receiver(cx, class, member, expr)?;
    let static_kw = if is_static_member(ast, member) {
        "static "
    } else {
        ""
    };
    let mut taken = taken_names(ast, &f.bound);
    taken.extend(member_names(ast, text, class));
    let name = unique("newProperty", &taken);
    let lead = leading_start(ast, text, member);
    let ls = style::line_start(text, lead);
    if !style::blank(text, ls, lead) {
        return None;
    }
    let indent = style::line_indent(text, msp.start);
    let nl = style::newline(text);
    let semi = style::semi(ast, text);
    Some(vec![
        Edit::insert(
            ls,
            format!(
                "{indent}private {static_kw}readonly {name} = {}{semi}{nl}{nl}",
                esp.text(text)
            ),
        ),
        Edit::new(esp.start, esp.end, format!("{recv}.{name}")),
    ])
}

/// Whether local `sym` (declared inside function `func`) is in scope at the end of `func`'s
/// body: a parameter or a top-level declaration of the body, or a `var` of the function.
fn visible_in_body(cx: &Ctx<'_, '_>, func: NodeId, sym: SymbolId) -> bool {
    let f = cx.checker.file(cx.file);
    let b = &f.bound;
    let Some(sc) = b.scope_of(func) else {
        return false;
    };
    let locals = b.scopes[sc.0 as usize].locals;
    b.table(locals).iter().any(|&(_, s)| s == sym)
}

/// Type parameters declared by the functions and classes around `n` inside `region` (not
/// in reach of a declaration placed outside them): (name, declaration text).
fn inner_type_params<'a>(cx: &'a Ctx<'_, '_>, n: NodeId, region: Span) -> Vec<(&'a str, &'a str)> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let mut out = Vec::new();
    for a in f.parents().ancestors(n) {
        let sp = ast.span(a);
        if sp.start < region.start || sp.end > region.end {
            break;
        }
        let tp = match *ast.node(a) {
            Node::FunctionDecl { type_params, .. }
            | Node::FunctionExpr { type_params, .. }
            | Node::Arrow { type_params, .. }
            | Node::ClassDecl { type_params, .. }
            | Node::ClassExpr { type_params, .. } => type_params,
            _ => None,
        };
        if let Some(tp) = tp
            && let Node::TypeParams { params } = *ast.node(tp)
        {
            let mut these: Vec<(&str, &str)> = ast
                .list(params)
                .iter()
                .filter_map(|&p| match *ast.node(p) {
                    Node::TypeParam { name, .. } => {
                        Some((ast.name_of(name)?, ast.span(p).text(text)))
                    }
                    _ => None,
                })
                .collect();
            // Outer ones first.
            these.append(&mut out);
            out = these;
        }
    }
    out
}

/// Whether `roots` read a variable the new function would capture (not one passed as a
/// parameter) that is narrowed where they are: the function sees its declared type.
fn narrowed_capture(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    roots: &[NodeId],
    passed: &[OuterRef],
) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    let mut stack: Vec<NodeId> = roots.to_vec();
    while let Some(n) = stack.pop() {
        if is_function_like(ast, n) {
            continue;
        }
        if matches!(ast.node(n), Node::Ident { .. })
            && let Some(local) = f.bound.resolved(n)
            && !passed.iter().any(|o| o.sym == local)
            && !is_write(ast, parents, n)
        {
            let loc = Loc::new(cx.file, n);
            if let Some(sym) = cx.checker.resolve_ident(loc, ts_r_binder::meaning::VALUE)
                && ck.type_of_symbol(sym) != ck.check_expr(loc)
            {
                return true;
            }
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// Whether `word` appears in `s` as a whole identifier.
fn mentions(s: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    s.match_indices(word).any(|(i, _)| {
        !s[..i].chars().next_back().is_some_and(ident)
            && !s[i + word.len()..].chars().next().is_some_and(ident)
    })
}

#[allow(clippy::too_many_arguments)]
fn extract_function(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    req: &Request<'_>,
    range: &Range,
    roots: &[NodeId],
    s: u32,
    e: u32,
    scopes: &[Scope],
    i: usize,
) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    let scope = scopes[i];
    // Imports, exports and exported declarations stay where they are, and so do
    // declarations (other than variables, which are returned) used outside the range.
    if let Range::Stmts(stmts) = range {
        for &st in stmts {
            let n = ast.node(st);
            if n.modifiers().contains(Modifiers::EXPORT) {
                return None;
            }
            match *n {
                Node::ImportDecl { .. }
                | Node::ImportEquals { .. }
                | Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. }
                | Node::NamespaceExport { .. }
                | Node::Directive { .. } => return None,
                Node::FunctionDecl { .. }
                | Node::ClassDecl { .. }
                | Node::Interface { .. }
                | Node::TypeAlias { .. }
                | Node::Enum { .. }
                | Node::Module { .. } => {
                    let sym = b.symbol_of_decl(st)?;
                    // Overloads and merged declarations move together or not at all.
                    if b.decls(sym).iter().any(|&d| {
                        let p = ast.span(d).start;
                        p < s || p >= e
                    }) {
                        return None;
                    }
                    let used_outside = b.refs.resolved.iter().any(|&(r, x)| {
                        x == sym && {
                            let p = ast.span(r).start;
                            p < s || p >= e
                        }
                    });
                    if used_outside {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
    let nl = style::newline(text);
    let semi = style::semi(ast, text);
    let unit = req.indent_unit(text);
    // Where the new declaration goes, the region whose locals become parameters, the
    // declaration's indentation and the receiver of a method.
    let (region, insert_at, ind, recv, class) = match scope {
        Scope::Module => {
            // Appended to the file, after its last statement.
            let first = ast.span(statement_in(cx, ast.root, roots[0])?);
            let last = ast.span(statement_in(cx, ast.root, *roots.last()?)?);
            let end = ast.span(*style::top_level(ast).last()?).end;
            (
                Span::new(first.start, last.end),
                end,
                String::new(),
                None,
                None,
            )
        }
        Scope::Function(func) => {
            let body = scope_body(ast, func)?;
            let Node::Block { stmts } = *ast.node(body) else {
                return None;
            };
            let last = *ast.list(stmts).last()?;
            let ind = style::line_indent(text, ast.span(last).start).to_string();
            (ast.span(body), ast.span(last).end, ind, None, None)
        }
        Scope::Class(class) => {
            // A bare `this` gains nothing as a member (and its `this` type trips our checker).
            if matches!(range, Range::Expr(n) if matches!(ast.node(*n), Node::This {})) {
                return None;
            }
            let member = member_of(cx, class, roots[0])?;
            let msp = ast.span(member);
            let ind = style::line_indent(text, msp.start).to_string();
            let recv = member_receiver(cx, class, member, roots[0])?;
            (msp, msp.end, ind, Some((recv, member)), Some(class))
        }
    };
    // The selection may end with the body's last statement.
    let insert_at = if s < insert_at && insert_at < e {
        e
    } else {
        insert_at
    };
    let u = usage(cx, s, e, roots, region);
    if super::rewrite::extract_hazard(cx, roots, statement_in(cx, ast.root, roots[0])?, false)
        || super::rewrite::narrowed_access(cx, ck, roots)
    {
        return None;
    }
    if u.has_yield || u.has_return || u.has_jump || u.arguments {
        return None;
    }
    // `this` keeps its meaning only in a method.
    if u.this_like && class.is_none() {
        return None;
    }
    if u.private_names && class.is_none() {
        return None;
    }
    // Locals in reach of the new declaration stay captured; the others become parameters.
    let mut passed: Vec<OuterRef> = Vec::new();
    for o in &u.outer {
        let captured = match scope {
            Scope::Function(func) => visible_in_body(cx, func, o.sym),
            _ => false,
        };
        if captured || o.type_param {
            continue;
        }
        if !o.value || o.written {
            return None;
        }
        passed.push(*o);
    }
    if narrowed_capture(cx, ck, roots, &passed) {
        return None;
    }
    // Parameters: in order of first use, typed as used (JavaScript: untyped, and no return
    // type, type parameters or `private`).
    let js = cx.checker.file(cx.file).is_js();
    let mut params = Vec::new();
    let mut args = Vec::new();
    for o in &passed {
        let name = super::super::locate::name_text(ast, o.id)?;
        if js {
            params.push(name.to_string());
            args.push(name.to_string());
            continue;
        }
        let t = ck.check_expr(Loc::new(cx.file, o.id));
        let t = if ck.store().flags(t).intersects(TypeFlags::LITERAL) {
            ck.base_of_literal(t)
        } else {
            t
        };
        let ty = ck.type_to_string(t);
        if !printable(&ty) {
            return None;
        }
        params.push(format!("{name}: {ty}"));
        args.push(name.to_string());
    }
    // An expression with a contextual type: the function returns that type (the
    // expression's own type where the context is generic).
    let mut ret_type = String::new();
    if let Range::Expr(n) = range
        && !js
        && let Some(ct) = ck.contextual_type(Loc::new(cx.file, *n))
    {
        let own = ck.check_expr(Loc::new(cx.file, *n));
        let own = if ck.store().flags(own).intersects(TypeFlags::LITERAL) {
            ck.base_of_literal(own)
        } else {
            own
        };
        // A context the expression doesn't fit (the file has an error there) would move
        // the error into the new function.
        let t = if ck
            .store()
            .traits(ct)
            .intersects(ts_r_types::TypeTraits::CONTAINS_TYPE_PARAMETER)
            || !ck.is_assignable(own, ct)
        {
            own
        } else {
            ct
        };
        let ty = ck.type_to_string(t);
        if printable(&ty) {
            ret_type = format!(": {ty}");
        }
    }
    // Type parameters out of reach that the parameters or the range mention.
    let range_text = &text[s as usize..e as usize];
    let inner = inner_type_params(cx, roots[0], region);
    let mut wanted: Vec<bool> = inner
        .iter()
        .map(|(name, _)| {
            params.iter().any(|p| mentions(p, name))
                || mentions(range_text, name)
                || mentions(&ret_type, name)
        })
        .collect();
    // And those their constraints or defaults mention (`U extends T` needs `T`).
    loop {
        let more: Vec<usize> = (0..inner.len())
            .filter(|&k| {
                !wanted[k]
                    && inner
                        .iter()
                        .zip(&wanted)
                        .any(|((_, d), &w)| w && mentions(d, inner[k].0))
            })
            .collect();
        if more.is_empty() {
            break;
        }
        for k in more {
            wanted[k] = true;
        }
    }
    let tparams: Vec<(&str, &str)> = inner
        .iter()
        .zip(&wanted)
        .filter(|(_, w)| **w)
        .map(|(t, _)| *t)
        .collect();
    // Other local types the parameters mention are out of reach.
    for sc in b.scopes.iter().filter(|_| !js) {
        let nsp = ast.span(sc.node);
        if nsp.start < region.start || nsp.end > region.end {
            continue;
        }
        for &(n, sym) in b.table(sc.locals) {
            use ts_r_binder::SymbolFlags as F;
            if !b
                .symbol(sym)
                .flags
                .intersects(F::CLASS | F::INTERFACE | F::TYPE_ALIAS | F::ENUM)
            {
                continue;
            }
            if let Scope::Function(func) = scope
                && visible_in_body(cx, func, sym)
            {
                continue;
            }
            let name = b.name_text(ast, n);
            if params.iter().any(|p| mentions(p, name)) || mentions(&ret_type, name) {
                return None;
            }
        }
    }
    let targs = if tparams.is_empty() {
        String::new()
    } else {
        let v: Vec<&str> = tparams.iter().map(|x| x.0).collect();
        format!("<{}>", v.join(", "))
    };
    let tdecl = if tparams.is_empty() {
        String::new()
    } else {
        let v: Vec<&str> = tparams.iter().map(|x| x.1).collect();
        format!("<{}>", v.join(", "))
    };
    // Variables declared in the range and used after it.
    let mut returned: Vec<(u32, &str)> = Vec::new();
    let mut all_const = true;
    if let Range::Stmts(stmts) = range {
        for &st in stmts {
            let Node::VarDecl { kind, decls, .. } = *ast.node(st) else {
                continue;
            };
            for &d in ast.list(decls) {
                let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                    continue;
                };
                let mut idents = Vec::new();
                walk(ast, name, &mut |n| {
                    if matches!(ast.node(n), Node::Ident { .. }) {
                        idents.push(n);
                    }
                    true
                });
                for id in idents {
                    let Some(sym) = b.symbol_of_decl(id).or_else(|| b.symbol_of_decl(d)) else {
                        continue;
                    };
                    let after = b
                        .refs
                        .resolved
                        .iter()
                        .filter(|&&(n, x)| x == sym && ast.span(n).start >= e)
                        .map(|&(n, _)| ast.span(n).start)
                        .min();
                    if let Some(pos) = after {
                        returned.push((pos, super::super::locate::name_text(ast, id)?));
                        if kind != VarKind::Const {
                            all_const = false;
                        }
                    }
                }
            }
        }
    }
    returned.sort_unstable();
    let returned: Vec<&str> = returned.into_iter().map(|(_, n)| n).collect();
    let mut taken = taken_names(ast, b);
    if let Some(c) = class {
        taken.extend(member_names(ast, text, c));
    }
    let fname = unique(
        if class.is_some() {
            "newMethod"
        } else {
            "newFunction"
        },
        &taken,
    );
    let base = style::line_indent(text, s).to_string();
    let body_ind = format!("{ind}{unit}");
    let callee = match &recv {
        Some((r, _)) => format!("{r}.{fname}"),
        None => fname.clone(),
    };
    let call = format!(
        "{}{callee}{targs}({})",
        if u.has_await { "await " } else { "" },
        args.join(", ")
    );
    // `const x = <expr>;` alone, `x` used after: the function returns the expression.
    let single_decl = match range {
        Range::Stmts(stmts) if returned.len() == 1 => match stmts.as_slice() {
            [st] => match *ast.node(*st) {
                Node::VarDecl { kind, decls, .. } => match ast.list(decls) {
                    [d] => match *ast.node(*d) {
                        Node::VarDeclarator {
                            name,
                            init: Some(init),
                            type_ann: None,
                            ..
                        } if matches!(ast.node(name), Node::Ident { .. }) => Some((init, kind)),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        _ => None,
    };
    let keep = template_spans(ast, roots);
    let mut body = String::new();
    let replacement = match range {
        Range::Expr(n) => {
            let sp = ast.span(*n);
            body.push_str(&format!(
                "{body_ind}return {}{semi}",
                reindent(text, sp.start, sp.end, &base, &body_ind, &keep)
            ));
            // `new newFunction()()` would construct `newFunction`.
            let callee_of_new =
                cx.checker.file(cx.file).parents().parent(*n).is_some_and(
                    |p| matches!(*ast.node(p), Node::New { callee, .. } if callee == *n),
                );
            if callee_of_new {
                format!("({call})")
            } else {
                call
            }
        }
        Range::Stmts(_) if single_decl.is_some() => {
            let (init, kind) = single_decl?;
            let sp = ast.span(init);
            body.push_str(&format!(
                "{body_ind}return {}{semi}",
                reindent(text, sp.start, sp.end, &base, &body_ind, &keep)
            ));
            let kw = if kind == VarKind::Const {
                "const"
            } else {
                "let"
            };
            format!("{kw} {} = {call}{semi}", returned[0])
        }
        Range::Stmts(_) => {
            body.push_str(&body_ind);
            body.push_str(&reindent(text, s, e, &base, &body_ind, &keep));
            let ret = match returned.as_slice() {
                [] => None,
                [one] => Some(one.to_string()),
                many => Some(format!("{{ {} }}", many.join(", "))),
            };
            match ret {
                None => format!("{call}{semi}"),
                Some(r) => {
                    body.push_str(&format!("{nl}{body_ind}return {r}{semi}"));
                    let kw = if all_const { "const" } else { "let" };
                    format!("{kw} {r} = {call}{semi}")
                }
            }
        }
    };
    let asyn = if u.has_await { "async " } else { "" };
    let head = match &recv {
        Some((_, member)) => format!(
            "{}{}{asyn}{fname}",
            if js { "" } else { "private " },
            if is_static_member(ast, *member) {
                "static "
            } else {
                ""
            }
        ),
        None => format!("{asyn}function {fname}"),
    };
    let decl = format!(
        "{nl}{nl}{ind}{head}{tdecl}({}){ret_type} {{{nl}{body}{nl}{ind}}}",
        params.join(", ")
    );
    Some(vec![
        Edit::new(s, e, replacement),
        Edit::insert(insert_at, decl),
    ])
}

// ---- Move to a new file ----

/// Names a top-level declaration declares (`None`: not movable).
fn declared_names(ast: &Ast, st: NodeId) -> Option<Vec<NodeId>> {
    let n = ast.node(st);
    if n.modifiers().contains(Modifiers::DEFAULT) {
        return None;
    }
    match *n {
        Node::FunctionDecl { name: Some(id), .. }
        | Node::ClassDecl { name: Some(id), .. }
        | Node::Interface { name: id, .. }
        | Node::TypeAlias { name: id, .. }
        | Node::Enum { name: id, .. } => Some(vec![id]),
        Node::VarDecl { decls, .. } => {
            let mut v = Vec::new();
            for &d in ast.list(decls) {
                let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                    return None;
                };
                if !matches!(ast.node(name), Node::Ident { .. }) {
                    return None;
                }
                v.push(name);
            }
            Some(v)
        }
        _ => None,
    }
}

/// Where moved statements go.
#[derive(Clone, Copy)]
enum Dest<'a> {
    /// A new file next to this one, named after the first moved declaration.
    NewFile,
    /// A file the user chose (`Move to file`): one of the program, or a new path.
    File(&'a str),
}

/// The specifier `spec` (written in `from_path`) as written in a file at `to_path`.
fn respecify(cx: &Ctx<'_, '_>, from_path: &str, to_path: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') {
        return Some(spec.to_string());
    }
    let same_dir = from_path.rsplit_once('/').map(|x| x.0) == to_path.rsplit_once('/').map(|x| x.0);
    if same_dir {
        return Some(spec.to_string());
    }
    let resolved = cx
        .program
        .resolutions
        .get(&(from_path.to_string(), spec.to_string()))
        .and_then(|r| r.as_ref())?
        .resolved
        .clone();
    let js_ext = spec.ends_with(".js") || spec.ends_with(".jsx");
    Some(imports::relative(to_path, &resolved, js_ext))
}

/// The file a relative or bare specifier of `file` resolves to.
fn resolved_path<'p>(cx: &'p Ctx<'_, '_>, file: &str, spec: &str) -> Option<&'p str> {
    cx.program
        .resolutions
        .get(&(file.to_string(), spec.to_string()))
        .and_then(|r| r.as_ref())
        .map(|r| r.resolved.as_str())
}

/// `Move to a new file` and `Move to file`: the selected top-level declarations go to
/// `dest`, with the imports they need; this file imports what it still uses of them, and
/// other files importing them from here import them from `dest`.
fn move_statements(cx: &Ctx<'_, '_>, req: &Request<'_>, dest: Dest<'_>) -> Option<Action> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    if !b.is_module {
        return None;
    }
    let top = style::top_level(ast);
    let (s, e) = trimmed(text, req.start, req.end.max(req.start));
    let moved: Vec<NodeId> = if s < e {
        top.iter()
            .copied()
            .filter(|&st| {
                let sp = ast.span(st);
                sp.start < e && s < sp.end
            })
            .collect()
    } else {
        // A cursor on the first line of a declaration.
        top.iter()
            .copied()
            .filter(|&st| {
                let sp = ast.span(st);
                sp.start <= req.start
                    && req.start <= sp.end
                    && !text[sp.start as usize..req.start as usize].contains('\n')
            })
            .collect()
    };
    if moved.is_empty() {
        return None;
    }
    let mut moved_syms: Vec<(SymbolId, NodeId)> = Vec::new();
    for &st in &moved {
        for id in declared_names(ast, st)? {
            let d = match *ast.node(st) {
                Node::VarDecl { .. } => f.parents().parent(id)?,
                _ => st,
            };
            let sym = b.symbol_of_decl(d).or_else(|| b.symbol_of_decl(id))?;
            moved_syms.push((sym, id));
        }
    }
    let name_of = |id: NodeId| super::super::locate::name_text(ast, id).unwrap_or("");
    // Every declaration of a moved name moves (overloads, merged declarations).
    for &(sym, _) in &moved_syms {
        if b.decls(sym)
            .iter()
            .any(|&d| !inside_moved_span(ast, &moved, d))
        {
            return None;
        }
    }
    let first_name = name_of(moved_syms.first()?.1);
    let inside_moved = |pos: u32| {
        moved
            .iter()
            .any(|&st| ast.span(st).start <= pos && pos < ast.span(st).end)
    };
    // The destination.
    let (dir, file_name) = f.path.rsplit_once('/')?;
    let ext = [".tsx", ".mts", ".cts", ".ts", ".jsx", ".mjs", ".cjs", ".js"]
        .iter()
        .find(|x| file_name.ends_with(*x))?;
    // Taken: a program file with the same name and any extension (`util.d.ts` would stop
    // `./util` naming it), compared case-insensitively (on macOS and Windows `View.tsx`
    // would overwrite `view.tsx`).
    let stem_of = |p: &str| -> String {
        let n = p.rsplit('/').next().unwrap_or(p);
        let cut = [
            ".d.ts", ".d.mts", ".d.cts", ".tsx", ".mts", ".cts", ".ts", ".jsx", ".mjs", ".cjs",
            ".js",
        ]
        .iter()
        .find(|x| n.len() > x.len() && n.ends_with(*x))
        .map_or(0, |x| x.len());
        p[..p.len() - cut].to_ascii_lowercase()
    };
    let exists = |p: &str| {
        let stem = stem_of(p);
        cx.program
            .files
            .iter()
            .any(|o| !o.is_lib && stem_of(&o.path) == stem)
    };
    let new_path = match dest {
        Dest::NewFile => {
            let mut p = format!("{dir}/{first_name}{ext}");
            let mut k = 1;
            while exists(&p) {
                p = format!("{dir}/{first_name}.{k}{ext}");
                k += 1;
            }
            p
        }
        Dest::File(p) => p.to_string(),
    };
    if new_path == f.path {
        return None;
    }
    // An existing destination: one of the program's own source files.
    let dest_file = match dest {
        Dest::File(p) => cx.program.file_index(p).map(|i| {
            let id = ts_r_diagnostics::FileId(i as u32);
            (id, cx.checker.file(id))
        }),
        Dest::NewFile => None,
    };
    if let Some((_, df)) = dest_file
        && (df.is_lib || df.is_external || df.bound.is_declaration || !df.bound.is_module)
    {
        return None;
    }
    let js_ext = text.contains(".js\"") || text.contains(".js'");
    let new_spec = imports::relative(&f.path, &new_path, js_ext);
    let old_spec = imports::relative(&new_path, &f.path, js_ext);
    let q = style::quote(ast, text);
    let semi = style::semi(ast, text);
    let nl = style::newline(text);

    // What the moved code uses: imports of this file, and declarations staying here.
    let import_stmts: Vec<NodeId> = top
        .iter()
        .copied()
        .filter(|&st| matches!(ast.node(st), Node::ImportDecl { .. }))
        .collect();
    // Import binding symbol → (import statement, specifier node).
    let mut bindings: Vec<(SymbolId, NodeId, NodeId)> = Vec::new();
    for &st in &import_stmts {
        let Node::ImportDecl { specifiers, .. } = *ast.node(st) else {
            continue;
        };
        for &sp in ast.list(specifiers) {
            let local = match *ast.node(sp) {
                Node::ImportDefaultSpecifier { local }
                | Node::ImportNamespaceSpecifier { local }
                | Node::ImportSpecifier { local, .. } => local,
                _ => continue,
            };
            if let Some(sym) = b.symbol_of_decl(sp).or_else(|| b.symbol_of_decl(local)) {
                bindings.push((sym, st, sp));
            }
        }
    }
    let mut used_bindings: Vec<NodeId> = Vec::new();
    let mut staying_used: Vec<SymbolId> = Vec::new();
    let mut kept_refs: HashSet<SymbolId> = HashSet::new();
    let mut moved_used_here: Vec<SymbolId> = Vec::new();
    let mut refs: Vec<(NodeId, SymbolId)> = b.refs.resolved.clone();
    refs.sort_by_key(|&(n, _)| ast.span(n).start);
    let import_span = |pos: u32| {
        import_stmts
            .iter()
            .any(|&st| ast.span(st).start <= pos && pos < ast.span(st).end)
    };
    for &(n, sym) in &refs {
        let pos = ast.span(n).start;
        if inside_moved(pos) {
            if let Some(&(_, _, sp)) = bindings.iter().find(|x| x.0 == sym) {
                if !used_bindings.contains(&sp) {
                    used_bindings.push(sp);
                }
            } else if !moved_syms.iter().any(|x| x.0 == sym)
                && !staying_used.contains(&sym)
                && b.decls(sym).iter().any(|&d| {
                    top.iter().any(|&st| {
                        st == d
                            || ast.span(st).start <= ast.span(d).start
                                && ast.span(d).end <= ast.span(st).end
                    }) && !inside_moved(ast.span(d).start)
                })
                && b.symbol(sym).parent.is_none_or(|p| p == b.file_symbol)
            {
                staying_used.push(sym);
            }
        } else if !import_span(pos) {
            kept_refs.insert(sym);
            if moved_syms.iter().any(|x| x.0 == sym) && !moved_used_here.contains(&sym) {
                moved_used_here.push(sym);
            }
        }
    }
    // Imports the moved code needs, in the order of this file's imports: as statements (a
    // new file) or as wants of the destination (an existing one).
    let mut new_imports: Vec<String> = Vec::new();
    let mut wants: Vec<imports::Want> = Vec::new();
    let mut raw_dest_imports: Vec<String> = Vec::new();
    let mut deleted_imports: Vec<NodeId> = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();
    for &st in &import_stmts {
        let Node::ImportDecl {
            specifiers,
            source,
            type_only,
            ..
        } = *ast.node(st)
        else {
            continue;
        };
        let specs = ast.list(specifiers);
        let used: Vec<NodeId> = specs
            .iter()
            .copied()
            .filter(|sp| used_bindings.contains(sp))
            .collect();
        if used.is_empty() {
            continue;
        }
        let spec_value = ast.name_of(source)?;
        let to_dest = resolved_path(cx, &f.path, spec_value) == Some(new_path.as_str());
        let mut def = None;
        let mut ns = None;
        let mut named = Vec::new();
        for &sp in &used {
            let t = ast.span(sp).text(text);
            match *ast.node(sp) {
                Node::ImportDefaultSpecifier { .. } => def = Some(t),
                Node::ImportNamespaceSpecifier { .. } => ns = Some(t),
                Node::ImportSpecifier {
                    imported, local, ..
                } => {
                    // Names the destination declares itself need no import there, unless
                    // they are renamed here.
                    if to_dest && imported.is_some_and(|i| ast.name_of(i) != ast.name_of(local)) {
                        return None;
                    }
                    named.push(t);
                }
                _ => {}
            }
        }
        if to_dest {
            if def.is_some() || ns.is_some() {
                return None;
            }
        } else {
            let spec = respecify(cx, &f.path, &new_path, spec_value)?;
            let squote = &ast.span(source).text(text)[..1];
            match dest_file {
                None => {
                    let mut clause = Vec::new();
                    if let Some(d) = def {
                        clause.push(d.to_string());
                    }
                    if let Some(n) = ns {
                        clause.push(n.to_string());
                    }
                    if !named.is_empty() {
                        clause.push(format!("{{ {} }}", named.join(", ")));
                    }
                    new_imports.push(format!(
                        "import {}{} from {squote}{spec}{squote}{semi}",
                        if type_only { "type " } else { "" },
                        clause.join(", ")
                    ));
                }
                Some(_) => {
                    if let Some(n) = ns {
                        raw_dest_imports
                            .push(format!("import {n} from {squote}{spec}{squote}{semi}"));
                    }
                    if def.is_some() || !named.is_empty() {
                        wants.push(imports::Want {
                            spec,
                            default: def.map(str::to_string),
                            named: named
                                .iter()
                                .map(|n| {
                                    let (t, n) = match n.strip_prefix("type ") {
                                        Some(rest) => (true, rest),
                                        None => (type_only, *n),
                                    };
                                    (n.to_string(), t)
                                })
                                .collect(),
                        });
                    }
                }
            }
        }
        // Bindings only the moved code used leave this file.
        let gone: Vec<NodeId> = used
            .iter()
            .copied()
            .filter(|&sp| {
                bindings
                    .iter()
                    .find(|x| x.2 == sp)
                    .is_some_and(|x| !kept_refs.contains(&x.0))
            })
            .collect();
        if gone.len() == specs.len() {
            deleted_imports.push(st);
        } else {
            let named_specs: Vec<NodeId> = specs
                .iter()
                .copied()
                .filter(|&x| matches!(ast.node(x), Node::ImportSpecifier { .. }))
                .collect();
            let gone_named: Vec<usize> = named_specs
                .iter()
                .enumerate()
                .filter(|(_, x)| gone.contains(x))
                .map(|(i, _)| i)
                .collect();
            if gone_named.len() == named_specs.len() && !named_specs.is_empty() {
                // `import d, { a } from`: drop the braces with the comma before them.
                let first = ast.span(named_specs[0]).start;
                let last = ast.span(*named_specs.last()?).end;
                let ssp = ast.span(st);
                let open = ssp.start + text[ssp.start as usize..first as usize].rfind('{')? as u32;
                let close = last + text[last as usize..ssp.end as usize].find('}')? as u32 + 1;
                let comma = ssp.start + text[ssp.start as usize..open as usize].rfind(',')? as u32;
                edits.push(Edit::delete(comma, close));
            } else {
                for &i in gone_named.iter().rev() {
                    edits.extend(super::unused::delete_element(ast, &named_specs, i));
                }
            }
        }
    }
    let copied_imports = new_imports.len();
    if !staying_used.is_empty() {
        let names: Vec<&str> = staying_used
            .iter()
            .map(|&s| b.name_text(ast, b.symbol(s).name))
            .collect();
        match dest_file {
            None => new_imports.push(format!(
                "import {{ {} }} from {q}{old_spec}{q}{semi}",
                names.join(", ")
            )),
            Some(_) => wants.push(imports::Want {
                spec: old_spec.clone(),
                default: None,
                named: names.iter().map(|n| (n.to_string(), false)).collect(),
            }),
        }
        // Declarations used from the destination must be exported.
        for &sym in &staying_used {
            let Some(&d) = b.decls(sym).first() else {
                continue;
            };
            let Some(st) = top.iter().copied().find(|&st| {
                let sp = ast.span(st);
                sp.start <= ast.span(d).start && ast.span(d).end <= sp.end
            }) else {
                continue;
            };
            if !ast.node(st).modifiers().contains(Modifiers::EXPORT) {
                let at = ast.span(st).start;
                if !edits.iter().any(|x| x.start == at && x.end == at) {
                    edits.push(Edit::insert(at, "export "));
                }
            }
        }
    }
    // The moved text, exporting what this file still uses.
    let ms = leading_start(ast, text, moved[0]);
    let me = ast.span(*moved.last()?).end;
    let mut moved_text = String::new();
    let mut cur = ms;
    for &st in &moved {
        let sp = ast.span(st);
        let needs_export = !ast.node(st).modifiers().contains(Modifiers::EXPORT)
            && declared_names(ast, st)?.iter().any(|&id| {
                moved_syms
                    .iter()
                    .any(|&(sym, x)| x == id && moved_used_here.contains(&sym))
            });
        if needs_export {
            moved_text.push_str(&text[cur as usize..sp.start as usize]);
            moved_text.push_str("export ");
            cur = sp.start;
        }
    }
    moved_text.push_str(&text[cur as usize..me as usize]);

    // This file: drop the moved statements (and blank lines after them).
    let del_start = style::line_start(text, ms);
    let mut del_end = style::next_line(text, me);
    while (del_end as usize) < text.len() {
        let nx = style::next_line(text, del_end);
        if !text[del_end as usize..nx as usize].trim().is_empty() {
            break;
        }
        del_end = nx;
    }
    if style::blank(text, del_start, ms)
        && text[me as usize..style::next_line(text, me) as usize]
            .trim()
            .is_empty()
    {
        edits.push(Edit::delete(del_start, del_end));
    } else {
        edits.push(Edit::delete(ms, me));
    }
    // ... and import what it still uses from the destination.
    let moved_names_here: Vec<&str> = moved_used_here
        .iter()
        .map(|&s| b.name_text(ast, b.symbol(s).name))
        .collect();
    let existing_import_of_dest = dest_file.is_some()
        && import_stmts.iter().any(|&st| {
            !deleted_imports.contains(&st)
                && matches!(*ast.node(st), Node::ImportDecl { source, has_clause: true, .. }
                    if ast.name_of(source).and_then(|sp| resolved_path(cx, &f.path, sp)) == Some(new_path.as_str()))
        });
    if !moved_names_here.is_empty() && existing_import_of_dest {
        // Joins this file's import of the destination.
        let spec = import_stmts
            .iter()
            .find_map(|&st| match *ast.node(st) {
                Node::ImportDecl { source, .. }
                    if ast
                        .name_of(source)
                        .and_then(|sp| resolved_path(cx, &f.path, sp))
                        == Some(new_path.as_str()) =>
                {
                    ast.name_of(source)
                }
                _ => None,
            })?
            .to_string();
        let (ie, _) = imports::plan(
            cx,
            &[imports::Want {
                spec,
                default: None,
                named: moved_names_here
                    .iter()
                    .map(|n| (n.to_string(), false))
                    .collect(),
            }],
        );
        edits.extend(ie);
    } else if !moved_names_here.is_empty() {
        let import = format!(
            "import {{ {} }} from {q}{new_spec}{q}{semi}",
            moved_names_here.join(", ")
        );
        let remaining: Vec<NodeId> = import_stmts
            .iter()
            .copied()
            .filter(|st| !deleted_imports.contains(st))
            .collect();
        if let Some(&last) = remaining.last() {
            edits.push(Edit::insert(ast.span(last).end, format!("{nl}{import}")));
        } else if let Some(&first) = deleted_imports.first() {
            let sp = ast.span(first);
            edits.push(Edit::new(sp.start, sp.end, import));
            deleted_imports.remove(0);
        } else {
            // First: an `export` added to the first statement goes after it.
            edits.insert(0, Edit::insert(0, format!("{import}{nl}{nl}")));
        }
    }
    for &st in &deleted_imports {
        edits.extend(super::unused::delete_lines(ast, text, st));
    }
    if overlapping(&edits) {
        return None;
    }
    let mut a = match dest {
        Dest::NewFile => Action::new(MOVE, "refactor.move.newFile"),
        Dest::File(_) => Action::new(MOVE_FILE, "refactor.move.file"),
    };
    a.refactor = Some(match dest {
        Dest::NewFile => format!("{MOVE}/{MOVE}"),
        Dest::File(_) => format!("{MOVE_FILE}/{MOVE_FILE}"),
    });
    let moved_names: Vec<&str> = moved
        .iter()
        .filter(|&&st| ast.node(st).modifiers().contains(Modifiers::EXPORT))
        .filter_map(|&st| declared_names(ast, st))
        .flatten()
        .map(name_of)
        .collect();
    match dest_file {
        None => {
            let mut new_text = String::new();
            if !new_imports.is_empty() {
                for i in &new_imports {
                    new_text.push_str(i);
                    new_text.push_str(nl);
                }
                new_text.push_str(nl);
                // tsserver leaves a second blank line after imports copied from this file.
                if copied_imports > 0 {
                    new_text.push_str(nl);
                }
            }
            new_text.push_str(&moved_text);
            new_text.push_str(nl);
            a.creates.insert(new_path.clone(), new_text);
        }
        Some((did, df)) => {
            let dcx = Ctx {
                program: cx.program,
                checker: cx.checker,
                file: did,
            };
            let de = dest_edits(
                &dcx,
                &f.path,
                &moved_text,
                &moved_syms
                    .iter()
                    .map(|&(_, id)| name_of(id))
                    .collect::<Vec<_>>(),
                &wants,
                &raw_dest_imports,
            )?;
            a.edit(&df.path, de);
        }
    }
    a.edit(&f.path, edits);
    // Other files importing the moved declarations from this file.
    if moved_names.is_empty() {
        return Some(a);
    }
    for (i, other) in cx.checker.files.iter().enumerate() {
        if i == cx.file.0 as usize
            || other.is_lib
            || other.is_external
            || other.bound.is_declaration
            || dest_file.is_some_and(|(d, _)| d.0 as usize == i)
        {
            continue;
        }
        let oe = retarget_importers(cx, other, &f.path, &new_path, &moved_names)?;
        a.edit(&other.path, oe);
    }
    Some(a)
}

/// The path Move to a new file would create for the request's selection.
pub fn new_file_target(cx: &Ctx<'_, '_>, req: &Request<'_>) -> Option<String> {
    let a = move_statements(cx, req, Dest::NewFile)?;
    a.creates.keys().next().cloned()
}

/// Whether two edits overlap (insertions at one offset don't).
fn overlapping(edits: &[Edit]) -> bool {
    let mut v: Vec<(u32, u32)> = edits.iter().map(|e| (e.start, e.end)).collect();
    v.sort_unstable();
    v.windows(2)
        .any(|w| w[0].1 > w[1].0 || (w[0].0 < w[0].1 && w[0] == w[1]))
}

/// Edits of the destination of `Move to file`: drop its imports of the moved names from
/// the source, add the imports the moved code needs (joining its own), and append the
/// moved text after its last statement.
fn dest_edits(
    dcx: &Ctx<'_, '_>,
    source: &str,
    moved_text: &str,
    moved_names: &[&str],
    wants: &[imports::Want],
    raw_imports: &[String],
) -> Option<Vec<Edit>> {
    let df = dcx.checker.file(dcx.file);
    let ast = &df.ast;
    let text = df.text.as_str();
    let b = &df.bound;
    let nl = style::newline(text);
    let top = style::top_level(ast);
    let mut edits = Vec::new();
    // Its imports of the moved names from the source go.
    let mut removed_locals: Vec<&str> = Vec::new();
    for &st in &top {
        let Node::ImportDecl {
            specifiers,
            source: src,
            ..
        } = *ast.node(st)
        else {
            continue;
        };
        let Some(spec) = ast.name_of(src) else {
            continue;
        };
        if resolved_path(dcx, &df.path, spec) != Some(source) {
            continue;
        }
        let specs = ast.list(specifiers);
        let hit: Vec<usize> = specs
            .iter()
            .enumerate()
            .filter(|(_, sp)| match *ast.node(**sp) {
                Node::ImportSpecifier {
                    imported, local, ..
                } => ast
                    .name_of(imported.unwrap_or(local))
                    .is_some_and(|n| moved_names.contains(&n)),
                _ => false,
            })
            .map(|(i, _)| i)
            .collect();
        if hit.is_empty() {
            continue;
        }
        for &i in &hit {
            let Node::ImportSpecifier {
                imported, local, ..
            } = *ast.node(specs[i])
            else {
                continue;
            };
            // A renamed import would leave uses of the local name behind.
            if imported.is_some() && ast.name_of(imported?) != ast.name_of(local) {
                return None;
            }
            removed_locals.push(ast.name_of(local)?);
        }
        if hit.len() == specs.len() {
            edits.extend(super::unused::delete_lines(ast, text, st));
        } else {
            let named: Vec<NodeId> = specs
                .iter()
                .copied()
                .filter(|&x| matches!(ast.node(x), Node::ImportSpecifier { .. }))
                .collect();
            if named.len() != specs.len() {
                return None;
            }
            for &i in hit.iter().rev() {
                edits.extend(super::unused::delete_element(ast, &named, i));
            }
        }
    }
    // Names the moved code declares must be free here (or come from the imports dropped).
    let file_scope = b.scopes.first()?.locals;
    for &n in moved_names {
        if removed_locals.contains(&n) {
            continue;
        }
        if b.lookup_str(ast, file_scope, n).is_some() {
            return None;
        }
    }
    // Imports the moved code needs, unless this file has the same binding already.
    let mut needed: Vec<imports::Want> = Vec::new();
    for w in wants {
        let mut w2 = imports::Want {
            spec: w.spec.clone(),
            default: None,
            named: Vec::new(),
        };
        let bound = |name: &str| b.lookup_str(ast, file_scope, name).is_some();
        if let Some(d) = &w.default
            && !bound(d)
        {
            w2.default = Some(d.clone());
        }
        for (n, t) in &w.named {
            let local = n.rsplit(" as ").next().unwrap_or(n);
            if bound(local) {
                // Bound by an import of the same module: nothing to do; else a clash.
                if !imported_from(dcx, local, &w.spec) {
                    return None;
                }
                continue;
            }
            w2.named.push((n.clone(), *t));
        }
        if w2.default.is_some() || !w2.named.is_empty() {
            needed.push(w2);
        }
    }
    let (ie, _) = imports::plan(dcx, &needed);
    edits.extend(ie);
    if !raw_imports.is_empty() {
        let at = top
            .iter()
            .rev()
            .find(|&&st| matches!(ast.node(st), Node::ImportDecl { .. }))
            .map(|&st| style::next_line(text, ast.span(st).end));
        let mut t = String::new();
        for r in raw_imports {
            t.push_str(r);
            t.push_str(nl);
        }
        match at {
            Some(at) => edits.push(Edit::insert(at, t)),
            None => edits.push(Edit::insert(0, format!("{t}{nl}"))),
        }
    }
    // The moved text after the last statement.
    match top.last() {
        Some(&last) => edits.push(Edit::insert(
            ast.span(last).end,
            format!("{nl}{moved_text}"),
        )),
        None => {
            let sep = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                nl
            };
            edits.push(Edit::insert(
                text.len() as u32,
                format!("{sep}{moved_text}{nl}"),
            ));
        }
    }
    if overlapping(&edits) {
        return None;
    }
    Some(edits)
}

/// Whether `local` is bound in the file of `cx` by an import of module `spec`.
fn imported_from(cx: &Ctx<'_, '_>, local: &str, spec: &str) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    style::top_level(ast).into_iter().any(|st| {
        let Node::ImportDecl {
            specifiers, source, ..
        } = *ast.node(st)
        else {
            return false;
        };
        ast.name_of(source) == Some(spec)
            && ast.list(specifiers).iter().any(|&sp| match *ast.node(sp) {
                Node::ImportSpecifier { local: l, .. }
                | Node::ImportDefaultSpecifier { local: l }
                | Node::ImportNamespaceSpecifier { local: l } => ast.name_of(l) == Some(local),
                _ => false,
            })
    })
}

/// Whether declaration `d` lies in one of the `moved` statements.
fn inside_moved_span(ast: &Ast, moved: &[NodeId], d: NodeId) -> bool {
    let dsp = ast.span(d);
    moved.iter().any(|&st| {
        let sp = ast.span(st);
        sp.start <= dsp.start && dsp.end <= sp.end
    })
}

/// Edits of `file` moving its imports of `names` from module `from` to module `to`; `None`
/// when it uses `from` in a way these edits can't follow (namespace import, `export *`,
/// `require`, dynamic import).
fn retarget_importers(
    cx: &Ctx<'_, '_>,
    file: &ts_r_checker::SourceFile,
    from: &str,
    to: &str,
    names: &[&str],
) -> Option<Vec<Edit>> {
    let ast = &file.ast;
    let text = file.text.as_str();
    let parents = file.parents();
    let mut edits = Vec::new();
    // By resolution, not spelling: `paths` aliases and package names reach `from` too.
    for r in &file.bound.module_requests {
        let Some(spec) = ast.name_of(r.node) else {
            continue;
        };
        let relative = spec.starts_with('.') || spec.starts_with('/');
        let resolved = cx
            .program
            .resolutions
            .get(&(file.path.clone(), spec.to_string()))
            .and_then(|r| r.as_ref())
            .map(|r| r.resolved.as_str());
        if resolved != Some(from) {
            continue;
        }
        let st = parents.parent(r.node)?;
        let (specifiers, source, is_export, type_only) = match *ast.node(st) {
            Node::ImportDecl {
                has_clause: false, ..
            } => continue,
            Node::ImportDecl {
                specifiers,
                source,
                type_only,
                ..
            } => (specifiers, source, false, type_only),
            Node::ExportNamed {
                specifiers,
                source: Some(source),
                type_only,
                ..
            } => (specifiers, source, true, type_only),
            _ => return None,
        };
        if ast
            .list(specifiers)
            .iter()
            .any(|&x| matches!(ast.node(x), Node::ImportNamespaceSpecifier { .. }))
        {
            return None;
        }
        let specs = ast.list(specifiers);
        let imported_name = |sp: NodeId| -> Option<&str> {
            match *ast.node(sp) {
                Node::ImportSpecifier {
                    imported, local, ..
                } => ast.name_of(imported.unwrap_or(local)),
                Node::ExportSpecifier { local, .. } => ast.name_of(local),
                _ => None,
            }
        };
        let hits: Vec<usize> = specs
            .iter()
            .enumerate()
            .filter(|(_, sp)| imported_name(**sp).is_some_and(|n| names.contains(&n)))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            continue;
        }
        let js_ext = spec.ends_with(".js") || spec.ends_with(".jsx");
        let kw = match (is_export, type_only) {
            (true, true) => "export type",
            (true, false) => "export",
            (false, true) => "import type",
            (false, false) => "import",
        };
        // The shorter of the relative path and a `paths` alias (fewer segments; a tie goes
        // to the relative one); an alias that covers only the old file gives way to the
        // relative path (as tsserver). A package specifier would need the package's manifest
        // (its subpaths and `exports`), which actions don't read: decline.
        let rel = imports::relative(&file.path, to, js_ext);
        let new_spec = if relative {
            rel
        } else if let Some(alias) = super::rename::alias_specifier(cx.program, spec, to) {
            let segs = |s: &str| s.trim_start_matches("./").split('/').count();
            if segs(&alias) < segs(&rel) {
                alias
            } else {
                rel
            }
        } else if super::rename::is_alias(cx.program, spec) {
            rel
        } else {
            return None;
        };
        let ssp = ast.span(source);
        let q = &text[ssp.start as usize..ssp.start as usize + 1];
        if hits.len() == specs.len() {
            edits.push(Edit::new(ssp.start, ssp.end, format!("{q}{new_spec}{q}")));
            continue;
        }
        let moved: Vec<&str> = hits
            .iter()
            .map(|&i| ast.span(specs[i]).text(text))
            .collect();
        let named: Vec<NodeId> = specs
            .iter()
            .copied()
            .filter(|&x| {
                matches!(
                    ast.node(x),
                    Node::ImportSpecifier { .. } | Node::ExportSpecifier { .. }
                )
            })
            .collect();
        for &i in hits.iter().rev() {
            let Some(j) = named.iter().position(|&x| x == specs[i]) else {
                continue;
            };
            edits.extend(super::unused::delete_element(ast, &named, j));
        }
        let semi = style::semi(ast, text);
        let nl = style::newline(text);
        edits.push(Edit::insert(
            ast.span(st).end,
            format!(
                "{nl}{kw} {{ {} }} from {q}{new_spec}{q}{semi}",
                moved.join(", ")
            ),
        ));
    }
    Some(edits)
}

/// Whether a printed type can be written into code: not empty, not truncated (`...`), and
/// without `infer` (the type printer writes `infer U[]` where `(infer U)[]` is needed;
/// observed in private-app `infra-project`, infra-project's `Unwrap<T>`).
fn printable(ty: &str) -> bool {
    !ty.is_empty() && !ty.contains("...") && !ty.contains("infer ")
}

#[cfg(test)]
mod tests {
    use super::{module_ident, reindent, unique};
    use std::collections::HashSet;
    use ts_r_ast::Span;

    #[test]
    fn module_identifiers() {
        assert_eq!(module_ident("./util"), "util");
        assert_eq!(module_ident("lodash-es"), "lodashEs");
        assert_eq!(module_ident("@scope/my-pkg"), "myPkg");
        assert_eq!(module_ident("./lib/index.js"), "index");
        assert_eq!(module_ident("./3d"), "_3d");
        assert_eq!(module_ident("./class"), "_class");
        let taken: HashSet<String> = ["util".to_string(), "util_1".to_string()].into();
        assert_eq!(unique("util", &taken), "util_2");
    }

    #[test]
    fn reindent_keeps_template_lines() {
        let text = "    a();\n    const s = `\n      x\n    `;\n    b();";
        let tpl = text.find('`').unwrap() as u32;
        let tpl_end = text.rfind('`').unwrap() as u32 + 1;
        let out = reindent(
            text,
            4,
            text.len() as u32,
            "    ",
            "  ",
            &[Span::new(tpl, tpl_end)],
        );
        assert_eq!(out, "a();\n  const s = `\n      x\n    `;\n  b();");
    }
}
