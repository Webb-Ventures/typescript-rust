//! Quick fixes for diagnostics and their fix-all variants, as tsserver 6.0.3 offers them on
//! the `fix-*` fixtures (titles, fix ids and edits observed, not taken from its source):
//!
//! | Diagnostics | Fix | Fix all |
//! |---|---|---|
//! | 2304, 2552, 2503 | `Add import from "m"` / `Update import from "m"` | `Add all missing imports` |
//! | 2304 on a call's callee | `Add missing function declaration 'f'` | — |
//! | 2551, 2552, 2724, 2833 | `Change spelling to 'x'` | `Fix all detected spelling errors` |
//! | 2339, 2345, 2322, 2349, 2351, 2362, 2363, 2365, 2461, 2488, 2495, 2504 on a promise | `Add 'await'`, `Add 'await' to initializer for 'x'` | `Fix all expressions possibly missing 'await'` |
//! | 1308 | `Add async modifier to containing function` | `Add all missing 'async' modifiers` |
//! | 2420 | `Implement interface 'I'` | `Implement all unimplemented interfaces` |
//! | 2739, 2740, 2741 | `Add missing properties` | `Add all missing properties` |
//! | 6133, 6192, 6196, 6198, 6199, 6205, 6138 | `Remove unused declaration for: 'x'`, `Remove import from 'm'`, `Prefix 'x' with an underscore`, ... | `Delete all unused declarations` |
//!
//! A fix-all is offered when two or more diagnostics of the file have that fix.

use super::edit::Edit;
use super::{Action, Ctx, Request, imports, members, unused};
use ts_r_ast::{Ast, FnFlags, Node, NodeId};
use ts_r_binder::meaning;
use ts_r_checker::{Ck, Loc};

/// A diagnostic of the file as fixes see it.
#[derive(Clone, Copy, Debug)]
pub struct Diag<'a> {
    pub start: u32,
    pub end: u32,
    pub code: u32,
    pub message: &'a str,
}

/// One fix for one diagnostic.
#[derive(Clone, Debug)]
pub struct Fix {
    pub title: String,
    pub fix_id: &'static str,
    pub edits: Vec<Edit>,
}

impl Fix {
    pub fn new(title: impl Into<String>, fix_id: &'static str, edits: Vec<Edit>) -> Fix {
        Fix {
            title: title.into(),
            fix_id,
            edits,
        }
    }
}

const IMPORT_CODES: &[u32] = &[2304, 2552, 2503];
const SPELLING_CODES: &[u32] = &[2551, 2552, 2724, 2833];
const AWAIT_CODES: &[u32] = &[
    2339, 2345, 2322, 2349, 2351, 2362, 2363, 2365, 2461, 2488, 2495, 2504,
];
const MISSING_PROPS_CODES: &[u32] = &[2739, 2740, 2741];

/// Fix ids with their fix-all titles.
const FIX_ALL: &[(&str, &str)] = &[
    ("fixMissingImport", "Add all missing imports"),
    ("fixSpelling", "Fix all detected spelling errors"),
    (
        "addMissingAwait",
        "Fix all expressions possibly missing 'await'",
    ),
    (
        "fixAwaitInSyncFunction",
        "Add all missing 'async' modifiers",
    ),
    (
        "fixClassIncorrectlyImplementsInterface",
        "Implement all unimplemented interfaces",
    ),
    ("fixMissingProperties", "Add all missing properties"),
    ("unusedIdentifier_delete", "Delete all unused declarations"),
];

/// The file's diagnostics as [`Diag`]s.
fn file_diags<'a>(cx: &Ctx<'_, '_>, req: &'a Request<'_>) -> Vec<Diag<'a>> {
    req.file_diags
        .iter()
        .filter(|d| d.file == cx.file)
        .map(|d| Diag {
            start: d.span.start,
            end: d.span.end,
            code: d.code,
            message: &d.message,
        })
        .collect()
}

/// Quick fixes for the request's diagnostics, then their fix-all actions.
pub fn quick_fixes(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    req: &Request<'_>,
    out: &mut Vec<Action>,
) {
    let path = &cx.checker.file(cx.file).path;
    let all = file_diags(cx, req);
    let mut fix_ids: Vec<&'static str> = Vec::new();
    for &(start, end, code, lsp) in &req.diagnostics {
        // tsserver's `getCodeFixes` (the VS Code stand-in) names codes, not messages: take the
        // message of the file's diagnostic there.
        let message = match lsp["message"].as_str() {
            Some(m) if !m.is_empty() => m,
            _ => all
                .iter()
                .find(|d| d.code == code && d.start == start && d.end == end)
                .or_else(|| {
                    all.iter()
                        .find(|d| d.code == code && d.start <= start && start <= d.end)
                })
                .map_or("", |d| d.message),
        };
        let d = Diag {
            start,
            end,
            code,
            message,
        };
        for f in fixes_for(cx, ck, &d) {
            let mut a = Action::new(f.title, "quickfix");
            a.diagnostic = Some(lsp.clone());
            a.fix_id = Some(f.fix_id);
            a.edit(path, f.edits);
            if !fix_ids.contains(&f.fix_id) {
                fix_ids.push(f.fix_id);
            }
            out.push(a);
        }
    }
    for id in fix_ids {
        if let Some(mut a) = fix_all(cx, ck, id, &all) {
            a.diagnostic = req.diagnostics.first().map(|d| d.3.clone());
            out.push(a);
        }
    }
}

/// Every fix for one diagnostic, in tsserver's order.
pub fn fixes_for(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>) -> Vec<Fix> {
    let mut out = Vec::new();
    if IMPORT_CODES.contains(&d.code) {
        import_fixes(cx, ck, d, &mut out);
    }
    if d.code == 2304
        && let Some(f) = missing_function(cx, ck, d)
    {
        out.push(f);
    }
    if SPELLING_CODES.contains(&d.code)
        && let Some(f) = spelling(cx, d)
    {
        out.push(f);
    }
    if AWAIT_CODES.contains(&d.code) {
        await_fixes(cx, ck, d, &mut out);
    }
    if d.code == 1308
        && let Some(f) = add_async(cx, d)
    {
        out.push(f);
    }
    if d.code == 2420 {
        out.extend(members::implement_interface(cx, ck, d));
    }
    if MISSING_PROPS_CODES.contains(&d.code)
        && let Some(f) = members::missing_properties(cx, ck, d)
    {
        out.push(f);
    }
    if unused::CODES.contains(&d.code) {
        out.extend(unused::fixes(cx, ck, d));
    }
    out
}

/// The fix-all action of `id` over the file's diagnostics (two or more must have the fix).
fn fix_all(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, id: &str, all: &[Diag<'_>]) -> Option<Action> {
    let (fix_id, title) = *FIX_ALL.iter().find(|x| x.0 == id)?;
    let edits = match fix_id {
        "fixMissingImport" => missing_imports_all(cx, ck, all)?,
        "unusedIdentifier_delete" => unused::delete_all(cx, ck, all)?,
        _ => {
            let mut edits: Vec<Edit> = Vec::new();
            let mut count = 0;
            for d in all {
                let fixes = if fix_id == "fixClassIncorrectlyImplementsInterface" && d.code == 2420
                {
                    // Every interface of the class in one edit (a diagnostic per interface).
                    members::implement_all_interfaces(cx, ck, d)
                        .into_iter()
                        .collect()
                } else {
                    fixes_for(cx, ck, d)
                };
                // The fix-all form: the initializer variant of `await` when there is one.
                let Some(f) = fixes.iter().find(|f| f.fix_id == fix_id) else {
                    continue;
                };
                count += 1;
                for e in &f.edits {
                    if !edits.iter().any(|x| overlaps(x, e)) {
                        edits.push(e.clone());
                    }
                }
            }
            if count < 2 {
                return None;
            }
            edits
        }
    };
    let mut a = Action::new(title, "quickfix");
    a.fix_id = Some(fix_id);
    a.fix_all = true;
    a.edit(&cx.checker.file(cx.file).path, edits);
    Some(a)
}

/// Whether two edits touch the same text (two inserts at one point count, so a fix-all does
/// not insert the same text twice).
fn overlaps(a: &Edit, b: &Edit) -> bool {
    if a == b {
        return true;
    }
    if a.start == a.end && b.start == b.end {
        return a.start == b.start;
    }
    a.start < b.end && b.start < a.end
}

// ---- Imports ----

fn import_fixes(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>, out: &mut Vec<Fix>) {
    let Some((_, name, m)) = imports::missing_name(cx, d.start) else {
        return;
    };
    for src in imports::sources(cx, ck, &name, m) {
        let want = want_of(&src, &name);
        let (edits, updated) = imports::plan(cx, std::slice::from_ref(&want));
        let verb = if updated[0] { "Update" } else { "Add" };
        out.push(Fix::new(
            format!("{verb} import from \"{}\"", src.spec),
            "fixMissingImport",
            edits,
        ));
    }
}

fn want_of(src: &imports::Source, name: &str) -> imports::Want {
    imports::Want {
        spec: src.spec.clone(),
        default: src.default.then(|| name.to_string()),
        named: if src.default {
            Vec::new()
        } else {
            vec![(name.to_string(), src.type_only)]
        },
    }
}

/// Imports for every missing name of the file (the best source of each), grouped by module.
fn missing_imports_all(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    all: &[Diag<'_>],
) -> Option<Vec<Edit>> {
    let mut wants: Vec<imports::Want> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut count = 0;
    for d in all.iter().filter(|d| IMPORT_CODES.contains(&d.code)) {
        let Some((_, name, m)) = imports::missing_name(cx, d.start) else {
            continue;
        };
        let Some(src) = imports::sources(cx, ck, &name, m).into_iter().next() else {
            continue;
        };
        count += 1;
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let w = want_of(&src, &name);
        match wants.iter_mut().find(|x| x.spec == w.spec) {
            Some(x) => {
                if x.default.is_none() {
                    x.default = w.default;
                }
                x.named.extend(w.named);
            }
            None => wants.push(w),
        }
    }
    if count < 2 {
        return None;
    }
    Some(imports::plan(cx, &wants).0)
}

/// Every missing import of the file at once (`source.addMissingImports.ts`).
pub fn add_all_missing_imports(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    req: &Request<'_>,
) -> Option<Action> {
    let all = file_diags(cx, req);
    let mut wants: Vec<imports::Want> = Vec::new();
    for d in all.iter().filter(|d| IMPORT_CODES.contains(&d.code)) {
        let Some((_, name, m)) = imports::missing_name(cx, d.start) else {
            continue;
        };
        let Some(src) = imports::sources(cx, ck, &name, m).into_iter().next() else {
            continue;
        };
        let w = want_of(&src, &name);
        match wants.iter_mut().find(|x| x.spec == w.spec) {
            Some(x) => {
                if x.default.is_none() {
                    x.default = w.default;
                }
                for n in w.named {
                    if !x.named.contains(&n) {
                        x.named.push(n);
                    }
                }
            }
            None => wants.push(w),
        }
    }
    if wants.is_empty() {
        return None;
    }
    let mut a = Action::new("Add all missing imports", "source.addMissingImports.ts");
    a.edit(&cx.checker.file(cx.file).path, imports::plan(cx, &wants).0);
    Some(a)
}

/// Every unused declaration of the file removed (`source.removeUnused.ts`).
pub fn remove_all_unused(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    req: &Request<'_>,
) -> Option<Action> {
    let all = file_diags(cx, req);
    let edits = unused::delete_all(cx, ck, &all)?;
    let mut a = Action::new("Remove all unused code", "source.removeUnused.ts");
    a.edit(&cx.checker.file(cx.file).path, edits);
    Some(a)
}

// ---- Spelling ----

/// The suggestion of a "Did you mean 'x'?" message.
fn suggestion(message: &str) -> Option<&str> {
    let i = message.rfind("Did you mean '")?;
    let rest = &message[i + "Did you mean '".len()..];
    let end = rest.find("'?")?;
    Some(&rest[..end])
}

fn spelling(cx: &Ctx<'_, '_>, d: &Diag<'_>) -> Option<Fix> {
    let s = suggestion(d.message)?;
    let ast = cx.checker.ast(cx.file);
    let id = super::super::locate::ident_at(ast, d.start)?;
    if !super::super::complete::is_identifier_name(s) {
        return None;
    }
    let sp = ast.span(id);
    Some(Fix::new(
        format!("Change spelling to '{s}'"),
        "fixSpelling",
        vec![Edit::new(sp.start, sp.end, s)],
    ))
}

// ---- missing function ----

/// `helper(a, b)` where `helper` is not declared: a function at the end of the file with a
/// parameter per argument (named after an identifier or property argument, else `argN`;
/// typed as the argument, widened) and the return type the call's context wants.
fn missing_function(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>) -> Option<Fix> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    let id = super::super::locate::ident_at(ast, d.start)?;
    let name = super::super::locate::name_text(ast, id)?;
    let call = parents.parent(id)?;
    let Node::Call { callee, args, .. } = *ast.node(call) else {
        return None;
    };
    if callee != id
        || parents
            .parent(call)
            .is_some_and(|p| matches!(ast.node(p), Node::Await { .. }))
    {
        return None;
    }
    let mut params: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (i, &arg) in ast.list(args).iter().enumerate() {
        let base = match *ast.node(arg) {
            Node::Ident { .. } => super::super::locate::name_text(ast, arg)?.to_string(),
            Node::Member { prop, .. } => super::super::locate::name_text(ast, prop)?.to_string(),
            Node::Spread { .. } => return None,
            _ => format!("arg{i}"),
        };
        let mut n = base.clone();
        let mut k = 1;
        while names.contains(&n) {
            n = format!("{base}_{k}");
            k += 1;
        }
        let t = ck.check_expr(Loc::new(cx.file, arg));
        let t = ck.widen_literal(t);
        let t = ck.widened_type(t);
        let ty = ck.type_to_string(t);
        params.push(format!("{n}: {ty}"));
        names.push(n);
    }
    let ret = match ck.contextual_type(Loc::new(cx.file, call)) {
        Some(t)
            if !ck
                .store()
                .flags(t)
                .intersects(ts_r_types::TypeFlags::ANY | ts_r_types::TypeFlags::UNKNOWN) =>
        {
            format!(": {}", ck.type_to_string(t))
        }
        _ => String::new(),
    };
    let nl = super::style::newline(text);
    let unit = super::style::indent_unit(text);
    let q = super::style::quote(ast, text);
    let lead = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        nl
    };
    let body = format!(
        "{lead}{nl}function {name}({}){ret} {{{nl}{unit}throw new Error({q}Function not implemented.{q});{nl}}}{nl}",
        params.join(", ")
    );
    Some(Fix::new(
        format!("Add missing function declaration '{name}'"),
        "fixMissingFunctionDeclaration",
        vec![Edit::insert(text.len() as u32, body)],
    ))
}

// ---- await / async ----

/// The outermost node inside `[start, end)` at `start`.
pub fn node_covering(cx: &Ctx<'_, '_>, start: u32, end: u32) -> Option<NodeId> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    let mut n = super::super::locate::node_at(ast, start)?;
    while let Some(p) = parents.parent(n) {
        let sp = ast.span(p);
        if sp.start < start || sp.end > end || matches!(ast.node(p), Node::Program { .. }) {
            break;
        }
        n = p;
    }
    Some(n)
}

/// The innermost enclosing function of `n` (function, function expression or arrow).
pub fn enclosing_function(cx: &Ctx<'_, '_>, n: NodeId) -> Option<NodeId> {
    let f = cx.checker.file(cx.file);
    f.parents().ancestors(n).find(|&p| {
        matches!(
            f.ast.node(p),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
        )
    })
}

fn is_async(ast: &Ast, f: NodeId) -> bool {
    match *ast.node(f) {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => flags.contains(FnFlags::ASYNC),
        _ => false,
    }
}

/// The expression a diagnostic about a promise used as its value points at.
fn awaitable_expr(cx: &Ctx<'_, '_>, d: &Diag<'_>) -> Option<NodeId> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    if d.code == 2339 {
        let id = super::super::locate::ident_at(ast, d.start)?;
        let p = f.parents().parent(id)?;
        return match *ast.node(p) {
            Node::Member { obj, prop, .. } if prop == id => Some(obj),
            _ => None,
        };
    }
    let n = node_covering(cx, d.start, d.end)?;
    if ast.span(n).start != d.start || ast.span(n).end != d.end {
        return None;
    }
    Some(n)
}

fn await_fixes(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>, out: &mut Vec<Fix>) {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Some(e) = awaitable_expr(cx, d) else {
        return;
    };
    if matches!(ast.node(e), Node::Await { .. }) {
        return;
    }
    // `await` is valid in an async function, or at the top level of a module.
    let await_ok = |n: NodeId| match enclosing_function(cx, n) {
        Some(func) => is_async(ast, func),
        None => f.bound.is_module,
    };
    if !await_ok(e) {
        return;
    }
    let t = ck.check_expr(Loc::new(cx.file, e));
    let awaited = ck.awaited_type(t);
    if awaited == t {
        return;
    }
    // `const r = load()`: await the initializer.
    if let Node::Ident { .. } = ast.node(e)
        && let Some(init) = const_initializer(cx, ck, e)
        && !matches!(ast.node(init), Node::Await { .. })
        && await_ok(init)
    {
        let name = ast.span(e).text(text);
        out.push(Fix::new(
            format!("Add 'await' to initializer for '{name}'"),
            "addMissingAwait",
            await_edits(ast, init, false),
        ));
    }
    let parent = f.parents().parent(e);
    let needs_parens = parent.is_some_and(|p| match *ast.node(p) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == e,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == e,
        Node::TaggedTemplate { tag, .. } => tag == e,
        _ => false,
    });
    out.push(Fix::new(
        "Add 'await'",
        "addMissingAwait",
        await_edits(ast, e, needs_parens),
    ));
}

fn await_edits(ast: &Ast, e: NodeId, wrap: bool) -> Vec<Edit> {
    let sp = ast.span(e);
    let low = matches!(
        ast.node(e),
        Node::Conditional { .. }
            | Node::Binary { .. }
            | Node::Assign { .. }
            | Node::Arrow { .. }
            | Node::Seq { .. }
            | Node::Yield { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
    );
    if wrap {
        vec![Edit::insert(sp.start, "(await "), Edit::insert(sp.end, ")")]
    } else if low {
        vec![Edit::insert(sp.start, "await ("), Edit::insert(sp.end, ")")]
    } else {
        vec![Edit::insert(sp.start, "await ")]
    }
}

/// The initializer of the `const` an identifier refers to, when declared in this file.
fn const_initializer(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, id: NodeId) -> Option<NodeId> {
    let c = ck.c;
    let s = c.resolve_ident(Loc::new(cx.file, id), meaning::VALUE)?;
    let decl = c.value_decl(s)?;
    if decl.file != cx.file {
        return None;
    }
    let f = c.file(cx.file);
    let ast = &f.ast;
    let mut n = decl.node;
    if !matches!(ast.node(n), Node::VarDeclarator { .. }) {
        n = f.parents().parent(n)?;
    }
    let Node::VarDeclarator { name, init, .. } = *ast.node(n) else {
        return None;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return None;
    }
    let list = f.parents().parent(n)?;
    match *ast.node(list) {
        Node::VarDecl {
            kind: ts_r_ast::VarKind::Const,
            ..
        } => init,
        _ => None,
    }
}

fn add_async(cx: &Ctx<'_, '_>, d: &Diag<'_>) -> Option<Fix> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let n = super::super::locate::node_at(ast, d.start)?;
    let func = enclosing_function(cx, n)?;
    if is_async(ast, func) {
        return None;
    }
    let mut edits = Vec::new();
    let (at, return_type) = match *ast.node(func) {
        Node::FunctionDecl { return_type, .. } | Node::FunctionExpr { return_type, .. } => {
            let parent = f.parents().parent(func);
            let at = match parent.map(|p| ast.node(p)) {
                Some(Node::MethodDef { key, .. }) => ast.span(*key).start,
                Some(Node::Property { key, value, .. })
                    if *value == func && is_method_shorthand(ast, text, *key, func) =>
                {
                    ast.span(*key).start
                }
                _ => {
                    let sp = ast.span(func);
                    let src = sp.text(text);
                    sp.start + src.find("function").unwrap_or(0) as u32
                }
            };
            (at, return_type)
        }
        Node::Arrow { return_type, .. } => (ast.span(func).start, return_type),
        _ => return None,
    };
    edits.push(Edit::insert(at, "async "));
    if let Some(rt) = return_type {
        let sp = ast.span(rt);
        let src = sp.text(text);
        if !src.starts_with("Promise<") {
            edits.push(Edit::new(sp.start, sp.end, format!("Promise<{src}>")));
        }
    }
    Some(Fix::new(
        "Add async modifier to containing function",
        "fixAwaitInSyncFunction",
        edits,
    ))
}

fn is_method_shorthand(ast: &Ast, text: &str, key: NodeId, func: NodeId) -> bool {
    let between = &text[ast.span(key).end as usize..ast.span(func).start as usize];
    !between.contains(':')
}
