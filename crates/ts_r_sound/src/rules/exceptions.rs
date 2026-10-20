//! S-13: checked exceptions.
//!
//! A function's throws set is what escapes its body: `throw` statements, the `@throws {T}`
//! tags of called functions (any file or package), and the inferred sets of untagged
//! same-file callees, minus what a `try`/`catch` handles. Re-throwing the catch variable
//! passes on what escaped the `try` block, less the classes an `instanceof` test on the path
//! excluded. Nested functions are not entered (callbacks are unchecked).
//!
//! (a) TSR1301: a tagged function lets something outside its tags escape (at the `throw` or
//! the call). (b) TSR1302: an exported function or a public method of an exported class
//! throws without tags. (c) TSR1303: only `Error` values may be thrown or passed to
//! `Promise.reject`. (d) TSR1304: an override throws more than the member it overrides
//! declares. TSR1305: a tag names a type that does not resolve.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{
    Ast, BinaryOp, CommentKind, HeritageKind, MethodKind, Modifiers, Node, NodeId, UnaryOp,
};
use ts_r_binder::meaning;
use ts_r_checker::checker::{GlobalType, Loc};
use ts_r_span::Span;
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::Ctx;

#[derive(Clone)]
struct Thrown {
    ty: TypeId,
    /// The `throw` statement or the call it escapes from (in the function's own file).
    at: NodeId,
}

/// One `@throws {A | B}` tag type name with its span in the source.
struct TagName {
    name: String,
    span: Span,
}

#[derive(Default)]
struct State {
    /// Escaping sets of same-file functions, by body-owning node.
    memo: FxHashMap<NodeId, Vec<Thrown>>,
    in_progress: FxHashSet<NodeId>,
    /// Declared `@throws` types by declaration (`None`: no tag).
    declared: FxHashMap<Loc, Option<Vec<TypeId>>>,
    /// Nodes already reported for TSR1303.
    non_error_reported: FxHashSet<NodeId>,
}

/// An active `catch (e)`: the catch variable and what escaped the `try` block.
struct Catch {
    sym: SymId,
    escaped: Vec<Thrown>,
}

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(13) {
        return;
    }
    let mut st = State::default();
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        let Some((decl, func, name_node)) = function_of(cx, n) else {
            continue;
        };
        check_function(cx, &mut st, decl, func, name_node);
    }
}

/// For declarations of functions with bodies: (the node a JSDoc comment precedes, the
/// function-like node, its name).
fn function_of(cx: &Ctx<'_, '_, '_>, n: NodeId) -> Option<(NodeId, NodeId, NodeId)> {
    match cx.node(n) {
        Node::FunctionDecl {
            name: Some(name),
            body: Some(_),
            ..
        } => Some((n, n, name)),
        Node::MethodDef {
            key, value, kind, ..
        } if kind != MethodKind::Constructor => {
            matches!(cx.node(value), Node::FunctionExpr { body: Some(_), .. })
                .then_some((n, value, key))
        }
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if matches!(
            cx.node(cx.skip_parens(init)),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        ) =>
        {
            let stmt = cx.parent(n)?;
            Some((stmt, cx.skip_parens(init), name))
        }
        _ => None,
    }
}

fn type_names(cx: &Ctx<'_, '_, '_>, types: &[TypeId]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for &t in types {
        let s = cx.ck.quietly_ref_type_str(t);
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v
}

trait TypeStr {
    fn quietly_ref_type_str(&self, t: TypeId) -> String;
}

impl TypeStr for ts_r_checker::checker::Ck<'_, '_> {
    fn quietly_ref_type_str(&self, t: TypeId) -> String {
        // Class instance types print as their names; enough for messages.
        match self.store().object_of(t).map(|o| o.kind) {
            Some(ts_r_types::ObjectKind::Declared { sym, .. }) => self.c.sym_name(sym).to_owned(),
            Some(ts_r_types::ObjectKind::Reference { target, .. }) => {
                match self.store().object_of(target).map(|o| o.kind) {
                    Some(ts_r_types::ObjectKind::Declared { sym, .. }) => {
                        self.c.sym_name(sym).to_owned()
                    }
                    _ => "object".to_owned(),
                }
            }
            _ => "a value".to_owned(),
        }
    }
}

fn is_unchecked(cx: &Ctx<'_, '_, '_>, t: TypeId) -> bool {
    let name = cx.ck.quietly_ref_type_str(t);
    cx.info.config.unchecked_errors.contains(&name)
}

fn check_function(
    cx: &mut Ctx<'_, '_, '_>,
    st: &mut State,
    decl: NodeId,
    func: NodeId,
    name_node: NodeId,
) {
    let fname = cx
        .key_text(name_node)
        .unwrap_or_else(|| cx.src(name_node).to_owned());
    let tags = declared_here(cx, st, decl);
    let escaping: Vec<Thrown> = fn_throws(cx, st, func)
        .into_iter()
        .filter(|t| !is_unchecked(cx, t.ty))
        .collect();
    match &tags {
        Some(declared) => {
            // (a)
            let mut seen = FxHashSet::default();
            for t in &escaping {
                if !declared.iter().any(|&d| cx.is_assignable(t.ty, d)) && seen.insert(t.at) {
                    let ts = cx.ck.quietly_ref_type_str(t.ty);
                    cx.report(codes::UNDECLARED_THROW, cx.span(t.at), &[&fname, &ts]);
                }
            }
        }
        None => {
            // (b)
            if cx.info.config.policy.exports_declare_throws
                && !escaping.is_empty()
                && is_exported(cx, decl)
            {
                let tys: Vec<TypeId> = escaping.iter().map(|t| t.ty).collect();
                let names = type_names(cx, &tys).join(" | ");
                cx.report(
                    codes::EXPORT_WITHOUT_THROWS,
                    cx.span(name_node),
                    &[&fname, &names],
                );
            }
        }
    }
    // (d)
    if let Node::MethodDef { .. } = cx.node(decl) {
        let own: Vec<TypeId> = match &tags {
            Some(d) => d.clone(),
            None => escaping.iter().map(|t| t.ty).collect(),
        };
        if !own.is_empty() {
            overrides(cx, st, decl, &fname, name_node, &own);
        }
    }
}

/// Whether a declaration is exported: an exported function or variable statement, or a
/// public method of an exported class.
fn is_exported(cx: &Ctx<'_, '_, '_>, decl: NodeId) -> bool {
    let exported = |m: Modifiers| m.contains(Modifiers::EXPORT);
    match cx.node(decl) {
        Node::FunctionDecl { modifiers, .. } | Node::VarDecl { modifiers, .. } => {
            exported(modifiers)
        }
        Node::MethodDef { modifiers, key, .. } => {
            if modifiers.intersects(Modifiers::PRIVATE | Modifiers::PROTECTED)
                || matches!(cx.node(key), Node::PrivateName { .. })
            {
                return false;
            }
            let Some(class) = cx.parent(decl) else {
                return false;
            };
            matches!(cx.node(class), Node::ClassDecl { modifiers, .. } if exported(modifiers))
        }
        _ => false,
    }
}

/// (d): `own` against the tags of the member this method overrides or implements.
fn overrides(
    cx: &mut Ctx<'_, '_, '_>,
    st: &mut State,
    method: NodeId,
    fname: &str,
    name_node: NodeId,
    own: &[TypeId],
) {
    let Some(class) = cx.parent(method) else {
        return;
    };
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = cx.node(class)
    else {
        return;
    };
    let mut bases: Vec<TypeId> = Vec::new();
    if let Some(sym) = cx.ck.c.sym_of_decl(cx.loc(class)) {
        bases.extend(cx.ck.quietly(|ck| ck.base_types(sym)).iter().copied());
    }
    for &h in cx.ast.list(heritage).to_vec().iter() {
        if let Node::HeritageClause {
            kind: HeritageKind::Implements,
            types,
        } = cx.node(h)
        {
            for &t in cx.ast.list(types).to_vec().iter() {
                bases.push(cx.type_node(t));
            }
        }
    }
    for b in bases {
        let Some(p) = cx.ck.quietly(|ck| ck.property_of_type(b, fname)) else {
            continue;
        };
        let decls = cx.ck.c.decls(p);
        let Some(&d) = decls.first() else {
            continue;
        };
        let declared = declared_at(cx, st, d);
        let base_declared = match declared {
            Some(t) => t,
            None if cx.info.is_sound_file(d.file) => Vec::new(),
            None => continue,
        };
        for &t in own {
            if is_unchecked(cx, t) {
                continue;
            }
            if !base_declared.iter().any(|&bd| cx.is_assignable(t, bd)) {
                let ts = cx.ck.quietly_ref_type_str(t);
                let parent = cx
                    .ck
                    .c
                    .sym_parent(p)
                    .map(|s| cx.ck.c.sym_name(s).to_owned())
                    .unwrap_or_default();
                let base_name = if parent.is_empty() {
                    fname.to_owned()
                } else {
                    format!("{parent}.{fname}")
                };
                cx.report(
                    codes::OVERRIDE_THROWS_MORE,
                    cx.span(name_node),
                    &[fname, &ts, &base_name],
                );
                return;
            }
        }
    }
}

/// The escaping set of a function-like node of this file (memoised; recursion yields empty).
fn fn_throws(cx: &mut Ctx<'_, '_, '_>, st: &mut State, func: NodeId) -> Vec<Thrown> {
    if let Some(v) = st.memo.get(&func) {
        return v.clone();
    }
    if !st.in_progress.insert(func) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(parts) = ts_r_checker::nodes::fn_parts(cx.ast, func)
        && let Some(body) = parts.body
    {
        let mut catches = Vec::new();
        walk(cx, st, body, &mut out, &mut catches, 0);
    }
    st.in_progress.remove(&func);
    let mut seen = FxHashSet::default();
    out.retain(|t| seen.insert((t.ty, t.at)));
    st.memo.insert(func, out.clone());
    out
}

fn walk(
    cx: &mut Ctx<'_, '_, '_>,
    st: &mut State,
    n: NodeId,
    out: &mut Vec<Thrown>,
    catches: &mut Vec<Catch>,
    depth: u32,
) {
    if depth > 400 {
        return;
    }
    match cx.node(n) {
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. } => {}
        Node::Throw { arg } => {
            walk(cx, st, arg, out, catches, depth + 1);
            throw(cx, st, n, arg, out, catches);
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            let mut inner = Vec::new();
            walk(cx, st, block, &mut inner, catches, depth + 1);
            match handler {
                Some(h) => {
                    if let Node::CatchClause { param, body, .. } = cx.node(h) {
                        let sym = param.and_then(|p| catch_sym(cx, p)).unwrap_or(SymId::NONE);
                        catches.push(Catch {
                            sym,
                            escaped: inner,
                        });
                        walk(cx, st, body, out, catches, depth + 1);
                        catches.pop();
                    }
                }
                None => out.extend(inner),
            }
            if let Some(f) = finalizer {
                walk(cx, st, f, out, catches, depth + 1);
            }
        }
        Node::Call { callee, args, .. } => {
            let mut kids = Vec::new();
            cx.ast.for_each_child(n, |k| kids.push(k));
            for k in kids {
                walk(cx, st, k, out, catches, depth + 1);
            }
            if cx.src(cx.skip_parens(callee)) == "Promise.reject"
                && let Some(&a) = cx.ast.list(args).first()
            {
                non_error(cx, st, a);
            }
            call(cx, st, n, out);
        }
        Node::New { .. } | Node::TaggedTemplate { .. } => {
            let mut kids = Vec::new();
            cx.ast.for_each_child(n, |k| kids.push(k));
            for k in kids {
                walk(cx, st, k, out, catches, depth + 1);
            }
            call(cx, st, n, out);
        }
        _ => {
            let mut kids = Vec::new();
            cx.ast.for_each_child(n, |k| kids.push(k));
            for k in kids {
                walk(cx, st, k, out, catches, depth + 1);
            }
        }
    }
}

fn catch_sym(cx: &Ctx<'_, '_, '_>, param: NodeId) -> Option<SymId> {
    cx.ck
        .c
        .sym_of_decl(cx.loc(param))
        .or_else(|| match cx.node(param) {
            Node::Param { pattern, .. } => cx.ck.c.sym_of_decl(cx.loc(pattern)),
            _ => None,
        })
}

/// TSR1303 when `e` is not an `Error`.
fn non_error(cx: &mut Ctx<'_, '_, '_>, st: &mut State, e: NodeId) {
    if !st.non_error_reported.insert(e) {
        return;
    }
    let t = cx.ty(e);
    if t.is_any_like() && t != TypeId::ANY {
        return;
    }
    let Some(err) = cx.ck.global_type(GlobalType::Error) else {
        return;
    };
    if t == TypeId::ANY || !cx.is_assignable(t, err) {
        let ts = if t == TypeId::ANY {
            "unknown".to_owned()
        } else {
            cx.type_str(t)
        };
        cx.report(codes::THROW_NON_ERROR, cx.span(e), &[&ts]);
    }
}

fn throw(
    cx: &mut Ctx<'_, '_, '_>,
    st: &mut State,
    stmt: NodeId,
    arg: NodeId,
    out: &mut Vec<Thrown>,
    catches: &[Catch],
) {
    let a = cx.skip_parens(arg);
    if matches!(cx.node(a), Node::Ident { .. })
        && let Some(sym) = cx.ck.c.resolve_ident(cx.loc(a), meaning::VALUE)
        && let Some(c) = catches
            .iter()
            .rev()
            .find(|c| c.sym == sym && !sym.is_none())
    {
        // Re-throw of the catch variable: what escaped the try, less `instanceof` exclusions.
        let (only, excluded) = instanceof_path(cx, stmt, sym);
        for t in &c.escaped {
            if excluded.iter().any(|&x| cx.is_assignable(t.ty, x)) {
                continue;
            }
            if !only.is_empty() && !only.iter().any(|&x| cx.is_assignable(t.ty, x)) {
                continue;
            }
            out.push(Thrown { ty: t.ty, at: stmt });
        }
        return;
    }
    non_error(cx, st, arg);
    let t = cx.ty(arg);
    if t.is_any_like()
        || cx
            .flags(t)
            .intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::NEVER)
    {
        return;
    }
    out.push(Thrown { ty: t, at: stmt });
}

/// Classes the path to `stmt` has tested the catch variable against: (`instanceof` that must
/// hold, `instanceof` that must not).
fn instanceof_path(
    cx: &mut Ctx<'_, '_, '_>,
    stmt: NodeId,
    sym: SymId,
) -> (Vec<TypeId>, Vec<TypeId>) {
    let mut only = Vec::new();
    let mut excluded = Vec::new();
    let mut child = stmt;
    let mut cur = cx.parent(stmt);
    for _ in 0..256 {
        let Some(p) = cur else { break };
        match cx.node(p) {
            Node::CatchClause { .. } => break,
            Node::If { test, cons, alt } => {
                if let Some(class) = instanceof_class(cx, test, sym) {
                    if cons == child {
                        only.push(class);
                    } else if alt == Some(child) {
                        excluded.push(class);
                    }
                }
            }
            Node::Block { stmts } => {
                let list = cx.ast.list(stmts).to_vec();
                if let Some(i) = list.iter().position(|&s| s == child) {
                    for &s in &list[..i] {
                        if let Node::If {
                            test,
                            cons,
                            alt: None,
                        } = cx.node(s)
                            && exits(cx, cons)
                            && let Some(class) = instanceof_class(cx, test, sym)
                        {
                            excluded.push(class);
                        }
                    }
                }
            }
            _ => {}
        }
        child = p;
        cur = cx.parent(p);
    }
    (only, excluded)
}

fn exits(cx: &Ctx<'_, '_, '_>, s: NodeId) -> bool {
    match cx.node(s) {
        Node::Return { .. } | Node::Throw { .. } | Node::Break { .. } | Node::Continue { .. } => {
            true
        }
        Node::Block { stmts } => cx.ast.list(stmts).last().is_some_and(|&l| exits(cx, l)),
        _ => false,
    }
}

/// `e instanceof C` (or `!(e instanceof C)` is not recognised): the instance type of `C`.
fn instanceof_class(cx: &mut Ctx<'_, '_, '_>, test: NodeId, sym: SymId) -> Option<TypeId> {
    let t = cx.skip_parens(test);
    match cx.node(t) {
        Node::Binary {
            left,
            op: BinaryOp::InstanceOf,
            right,
        } => {
            let l = cx.skip_parens(left);
            if cx.ck.c.resolve_ident(cx.loc(l), meaning::VALUE) != Some(sym) {
                return None;
            }
            let ct = cx.ty(right);
            let sigs = cx
                .ck
                .quietly(|ck| ck.signatures_of(ct, ts_r_checker::members::SigKind::Construct));
            let s = *sigs.first()?;
            Some(cx.ck.quietly(|ck| ck.return_type_of(s)))
        }
        Node::Unary {
            op: UnaryOp::Not, ..
        } => None,
        _ => None,
    }
}

/// A call's contribution: the callee's `@throws` tags, or a same-file untagged callee's
/// inferred set.
fn call(cx: &mut Ctx<'_, '_, '_>, st: &mut State, call: NodeId, out: &mut Vec<Thrown>) {
    let Some(sig) = cx.ck.resolved_signature(cx.loc(call)) else {
        return;
    };
    let Some(decl) = cx.sig_decl(sig) else {
        return;
    };
    // Lib declarations are unchecked: their few `@throws` tags (`JSON.parse`'s
    // `SyntaxError`) describe runtime errors, which the record leaves unchecked.
    if cx.info.is_lib(decl.file) {
        return;
    }
    // Tags sit on the declaration statement or class member.
    let doc_node = doc_owner(cx, decl);
    if let Some(types) = declared_at(cx, st, doc_node) {
        for t in types {
            out.push(Thrown { ty: t, at: call });
        }
        return;
    }
    if decl.file != cx.file {
        return;
    }
    let func = match cx.node(decl.node) {
        Node::MethodDef { value, .. } => value,
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => decl.node,
        _ => return,
    };
    for t in fn_throws(cx, st, func) {
        out.push(Thrown { ty: t.ty, at: call });
    }
}

/// The node a declaration's JSDoc precedes (the `MethodDef` of a method's function, the
/// variable statement of a function-valued variable).
fn doc_owner(cx: &Ctx<'_, '_, '_>, decl: Loc) -> Loc {
    let parents = cx.ck.c.file(decl.file).parents();
    let ast = cx.ck.c.ast(decl.file);
    let mut n = decl.node;
    if matches!(ast.node(n), Node::FunctionExpr { .. } | Node::Arrow { .. })
        && let Some(p) = parents.parent(n)
    {
        match ast.node(p) {
            Node::MethodDef { .. } => n = p,
            Node::VarDeclarator { .. } => {
                if let Some(s) = parents.parent(p) {
                    n = s;
                }
            }
            _ => {}
        }
    }
    if let Node::VarDeclarator { .. } = ast.node(n)
        && let Some(s) = parents.parent(n)
    {
        n = s;
    }
    Loc::new(decl.file, n)
}

/// Declared types of a declaration of this file (and TSR1305 for unresolved names).
fn declared_here(cx: &mut Ctx<'_, '_, '_>, st: &mut State, decl: NodeId) -> Option<Vec<TypeId>> {
    let l = cx.loc(decl);
    if let Some(names) = tag_names(cx.ast, cx.text, decl) {
        for tn in &names {
            if resolve_tag(cx, l, &tn.name).is_none() && !tn.name.contains('.') {
                cx.report(codes::UNKNOWN_THROWS_TYPE, tn.span, &[&tn.name]);
            }
        }
    }
    declared_at(cx, st, l)
}

fn declared_at(cx: &mut Ctx<'_, '_, '_>, st: &mut State, decl: Loc) -> Option<Vec<TypeId>> {
    if let Some(v) = st.declared.get(&decl) {
        return v.clone();
    }
    let sf = cx.ck.c.file(decl.file);
    let names = tag_names(&sf.ast, &sf.text, decl.node);
    let v = names.map(|names| {
        let mut out = Vec::new();
        for tn in names {
            if let Some(t) = resolve_tag(cx, decl, &tn.name)
                && !out.contains(&t)
            {
                out.push(t);
            }
        }
        out
    });
    st.declared.insert(decl, v.clone());
    v
}

/// Resolves a `@throws` type name in the scope of `at` (locals up the scope chain, then the
/// file, then globals), through imports.
fn resolve_tag(cx: &mut Ctx<'_, '_, '_>, at: Loc, name: &str) -> Option<TypeId> {
    let name = name.split('<').next().unwrap_or(name).trim();
    if name.is_empty() || name.contains('.') {
        return None;
    }
    let c = cx.ck.c;
    let b = c.bound(at.file);
    let ast = c.ast(at.file);
    let parents = c.file(at.file).parents();
    let mut found = None;
    let mut cur = Some(at.node);
    let mut steps = 0;
    while let Some(node) = cur {
        steps += 1;
        if steps > 10_000 {
            break;
        }
        if let Some(scope) = b.scope_of(node) {
            let sc = b.scopes[scope.0 as usize];
            if let Some(s) = b.lookup_str(ast, sc.locals, name) {
                found = Some(c.sym(at.file, s));
                break;
            }
        }
        cur = parents.parent(node);
    }
    if found.is_none()
        && let Some(fs) = b.scopes.first()
        && let Some(s) = b.lookup_str(ast, fs.locals, name)
    {
        found = Some(c.sym(at.file, s));
    }
    let sym = match found {
        Some(s) => s,
        None => c.global_symbol(name, meaning::TYPE)?,
    };
    let sym = c.resolve_alias(sym)?;
    if !c.has_meaning(sym, meaning::TYPE) {
        return None;
    }
    let t = cx.ck.quietly(|ck| ck.declared_type_of_symbol(sym));
    (!t.is_any_like()).then_some(t)
}

/// The `@throws {...}` type names of the JSDoc comment right before `decl` (`None`: no tag).
fn tag_names(ast: &Ast, text: &str, decl: NodeId) -> Option<Vec<TagName>> {
    let start = ast.span(decl).start as usize;
    let idx = ast
        .comments
        .partition_point(|c| (c.span.end as usize) <= start);
    let c = ast.comments.get(idx.checked_sub(1)?)?;
    if c.kind != CommentKind::Block {
        return None;
    }
    let ctext = c.span.text(text);
    if !ctext.starts_with("/**") {
        return None;
    }
    let between = text.get(c.span.end as usize..start)?;
    let ok = between.split_whitespace().all(|w| {
        matches!(
            w,
            "export"
                | "default"
                | "declare"
                | "async"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "override"
                | "readonly"
                | "abstract"
                | "const"
                | "let"
                | "var"
        )
    });
    if !ok {
        return None;
    }
    let mut out = Vec::new();
    let mut any = false;
    let mut from = 0;
    while let Some(i) = ctext[from..].find("@throws") {
        any = true;
        let after = from + i + "@throws".len();
        from = after;
        let rest = &ctext[after..];
        let trimmed = rest.trim_start();
        if !trimmed.starts_with('{') {
            continue;
        }
        let open = after + (rest.len() - trimmed.len());
        let mut level = 0usize;
        let mut close = None;
        for (j, ch) in ctext[open..].char_indices() {
            match ch {
                '{' => level += 1,
                '}' => {
                    level -= 1;
                    if level == 0 {
                        close = Some(open + j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        let inner_start = open + 1;
        let mut pos = inner_start;
        for part in ctext[inner_start..close].split('|') {
            let lead = part.len() - part.trim_start().len();
            let name = part.trim();
            if !name.is_empty() {
                let s = c.span.start + (pos + lead) as u32;
                out.push(TagName {
                    name: name.to_owned(),
                    span: Span::new(s, s + name.len() as u32),
                });
            }
            pos += part.len() + 1;
        }
        from = close;
    }
    any.then_some(out)
}
