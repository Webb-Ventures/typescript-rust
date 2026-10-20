//! S-12: an overloaded function or method's implementation is checked once per overload
//! signature (open question 4, decided 2026-10-15: overload implementations are allowed).
//!
//! TypeScript checks the body only against the implementation signature, so an overload can
//! promise a return type the body never produces for its arguments. For each overload, the
//! body is walked statement by statement with every parameter at the overload's type
//! (narrowed by `if` tests and `?:` through the checker's own condition narrowing,
//! `Ck::narrow_symbol_by_condition`, as S-07 does). A `return e` that the overload's
//! parameter types make unreachable (a parameter narrowed to `never`) is skipped; any other
//! must produce the overload's return type (TSR1205), and so must a normal completion
//! (`undefined`).
//!
//! The type of `e` is the checker's type of `e` (computed with the implementation's types),
//! except a bare parameter, which takes its overload type. A mismatch is reported only when
//! that type is exact for the overload: every parameter `e` reads already has, at that
//! point, a type within the overload's, and `e` reads no local whose value comes from a
//! parameter (nor `arguments`). Other returns are left unreported rather than guessed, as
//! are returns inside `switch` and labelled statements. Constructors have no return to check.

use rustc_hash::FxHashSet;
use ts_r_ast::{FnFlags, MethodKind, Node, NodeId};
use ts_r_binder::meaning;
use ts_r_span::Span;
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::Ctx;

const MAX_DEPTH: u32 = 64;
const MAX_STEPS: u32 = 100_000;

/// An implementation parameter the walk narrows.
struct Param {
    sym: SymId,
    name: String,
    declared: TypeId,
    rest: bool,
    has_init: bool,
}

/// One overload's walk of the implementation body.
struct Walk<'p> {
    /// Implementation parameters, `None` for destructured or reassigned ones.
    params: &'p [Option<Param>],
    /// Names whose value may depend on a parameter in a way the walk does not model:
    /// untracked parameters, locals computed from parameters, `arguments`.
    opaque_names: &'p FxHashSet<String>,
    /// The overload's return type (awaited in `async` implementations).
    ret: TypeId,
    is_async: bool,
    /// Inside a construct the walk does not narrow through (returns there are skipped).
    opaque: u32,
    /// (where, the type returned).
    issues: Vec<(Span, TypeId)>,
}

/// Per-parameter type under the overload being walked (`None`: not tracked).
type Types = Vec<Option<TypeId>>;

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(12) {
        return;
    }
    let nodes = cx.nodes.clone();
    for n in nodes {
        let list = match cx.node(n) {
            Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => {
                cx.ast.list(stmts).to_vec()
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                cx.ast.list(members).to_vec()
            }
            _ => continue,
        };
        let mut sigs: Vec<NodeId> = Vec::new();
        let mut sig_name: Option<String> = None;
        for &s in &list {
            let Some((name, has_body, is_ctor)) = member_fn(cx, s) else {
                sigs.clear();
                sig_name = None;
                continue;
            };
            if sig_name.as_deref() != Some(name.as_str()) {
                sigs.clear();
            }
            if has_body {
                if !sigs.is_empty() && !is_ctor {
                    check_implementation(cx, &sigs, s);
                }
                sigs.clear();
                sig_name = None;
            } else {
                sigs.push(s);
                sig_name = Some(name);
            }
        }
    }
}

/// A function declaration or method member: its name, whether it has a body, and whether
/// it is a constructor.
fn member_fn(cx: &Ctx<'_, '_, '_>, s: NodeId) -> Option<(String, bool, bool)> {
    match cx.node(s) {
        Node::FunctionDecl {
            name: Some(name),
            body,
            ..
        } => Some((cx.src(name).to_owned(), body.is_some(), false)),
        Node::MethodDef {
            key, value, kind, ..
        } if matches!(kind, MethodKind::Method | MethodKind::Constructor) => {
            let has_body = matches!(cx.node(value), Node::FunctionExpr { body: Some(_), .. });
            let ctor = kind == MethodKind::Constructor;
            let name = if ctor {
                "constructor".to_owned()
            } else {
                cx.key_text(key).unwrap_or_else(|| cx.src(key).to_owned())
            };
            Some((name, has_body, ctor))
        }
        _ => None,
    }
}

/// The function node of a declaration or method member, and its name node.
fn fn_and_name(cx: &Ctx<'_, '_, '_>, s: NodeId) -> Option<(NodeId, NodeId)> {
    match cx.node(s) {
        Node::FunctionDecl { name: Some(n), .. } => Some((s, n)),
        Node::MethodDef { key, value, .. } => Some((value, key)),
        _ => None,
    }
}

/// Parameters of function `f` without a `this` parameter.
fn params_of(cx: &Ctx<'_, '_, '_>, f: NodeId) -> Vec<NodeId> {
    let Some(parts) = ts_r_checker::nodes::fn_parts(cx.ast, f) else {
        return Vec::new();
    };
    cx.ast
        .list(parts.params)
        .iter()
        .copied()
        .filter(|&p| !ts_r_checker::nodes::is_this_param(cx.ast, p))
        .collect()
}

/// How a message names an overload: `pad(value: number): string`.
fn signature_text(cx: &Ctx<'_, '_, '_>, sig: NodeId, name: NodeId) -> String {
    let sp = cx.span(sig);
    let start = cx.span(name).start;
    let text = cx.text.get(start as usize..sp.end as usize).unwrap_or("");
    let text = text.trim_end().trim_end_matches(';').trim_end();
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 100 {
        let mut t: String = one.chars().take(97).collect();
        t.push_str("...");
        t
    } else {
        one
    }
}

fn check_implementation(cx: &mut Ctx<'_, '_, '_>, sigs: &[NodeId], imp: NodeId) {
    let Some((f, name_node)) = fn_and_name(cx, imp) else {
        return;
    };
    let Some(parts) = ts_r_checker::nodes::fn_parts(cx.ast, f) else {
        return;
    };
    let Some(body) = parts.body else {
        return;
    };
    if parts.flags.contains(FnFlags::GENERATOR) {
        return;
    }
    let is_async = parts.flags.contains(FnFlags::ASYNC);
    let imp_params = params_of(cx, f);
    let mut params: Vec<Option<Param>> = Vec::new();
    let mut seeds: Vec<String> = vec!["arguments".to_owned()];
    for &p in &imp_params {
        let Node::Param {
            pattern,
            rest,
            init,
            ..
        } = cx.node(p)
        else {
            params.push(None);
            continue;
        };
        seeds.extend(binding_names(cx, pattern));
        if !matches!(cx.node(pattern), Node::Ident { .. }) {
            params.push(None);
            continue;
        }
        let name = cx.src(pattern).to_owned();
        let Some(sym) = cx
            .ck
            .c
            .sym_of_decl(cx.loc(p))
            .or_else(|| cx.ck.c.sym_of_decl(cx.loc(pattern)))
        else {
            params.push(None);
            continue;
        };
        if assigns_name(cx, body, &name) {
            params.push(None);
            continue;
        }
        let pl = cx.loc(p);
        let declared = cx.ck.quietly(|ck| ck.param_type(pl));
        if declared.is_any_like() || cx.is_error(declared) {
            params.push(None);
            continue;
        }
        params.push(Some(Param {
            sym,
            name,
            declared,
            rest,
            has_init: init.is_some(),
        }));
    }
    let mut opaque_names = derived_names(cx, body, &seeds);
    for (p, &node) in params.iter().zip(&imp_params) {
        if p.is_none()
            && let Node::Param { pattern, .. } = cx.node(node)
        {
            opaque_names.extend(binding_names(cx, pattern));
        }
    }
    opaque_names.insert("arguments".to_owned());
    let end_reachable = match cx.ck.c.bound(cx.file).flow.function_flow(f) {
        Some(ff) => {
            let end = ff.end;
            !end.is_unreachable() && cx.ck.is_flow_reachable(cx.file, end)
        }
        None => false,
    };

    for &sig in sigs {
        let Some((sf, sname)) = fn_and_name(cx, sig) else {
            continue;
        };
        let Some(sparts) = ts_r_checker::nodes::fn_parts(cx.ast, sf) else {
            continue;
        };
        let Some(rt) = sparts.return_type else {
            continue;
        };
        if matches!(cx.node(rt), Node::TypePredicate { .. }) {
            continue;
        }
        let declared_ret = cx.type_node(rt);
        let ret = if is_async {
            cx.ck.quietly(|ck| ck.awaited_type(declared_ret))
        } else {
            declared_ret
        };
        if cx.flags(ret).intersects(TypeFlags::ANY_OR_UNKNOWN) || cx.is_error(ret) {
            continue;
        }
        let sig_params = params_of(cx, sf);
        let mut types: Types = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let Some(p) = p else {
                types.push(None);
                continue;
            };
            let t = match sig_params.get(i) {
                Some(&sp) => {
                    let s_rest = matches!(cx.node(sp), Node::Param { rest: true, .. });
                    if s_rest != p.rest {
                        None
                    } else {
                        let l = cx.loc(sp);
                        Some(cx.ck.quietly(|ck| ck.param_type(l)))
                    }
                }
                // Not passed through this overload: `undefined`, or the default.
                None if p.has_init || p.rest => None,
                None => Some(TypeId::UNDEFINED),
            };
            types.push(t.filter(|&t| !t.is_any_like() && !cx.is_error(t)));
        }
        let mut w = Walk {
            params: &params,
            opaque_names: &opaque_names,
            ret,
            is_async,
            opaque: 0,
            issues: Vec::new(),
        };
        let end = stmt(cx, &mut w, body, types, 0);
        if let Some(end) = end
            && end_reachable
            && w.opaque == 0
            && !unreachable(cx, &end)
            && !cx.is_assignable(TypeId::UNDEFINED, ret)
        {
            w.issues.push((cx.span(name_node), TypeId::UNDEFINED));
        }
        if w.issues.is_empty() {
            continue;
        }
        let text = signature_text(cx, sig, sname);
        let rs = cx.type_str(ret);
        let mut seen = FxHashSet::default();
        for (span, t) in std::mem::take(&mut w.issues) {
            if !seen.insert((span.start, span.end)) {
                continue;
            }
            let ts = cx.type_str(t);
            cx.report(codes::OVERLOAD_RETURN, span, &[&text, &ts, &rs]);
        }
    }
}

/// Every identifier a binding pattern declares.
fn binding_names(cx: &Ctx<'_, '_, '_>, pattern: NodeId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![pattern];
    let mut steps = 0;
    while let Some(n) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            break;
        }
        if let Node::Ident { .. } = cx.node(n) {
            out.push(cx.src(n).to_owned());
        }
        cx.ast.for_each_child(n, |c| stack.push(c));
    }
    out
}

/// Whether `name` is assigned (`=`, compound, `++`/`--`) anywhere in `n`.
fn assigns_name(cx: &Ctx<'_, '_, '_>, n: NodeId, name: &str) -> bool {
    let mut stack = vec![n];
    let mut steps = 0;
    while let Some(x) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return true;
        }
        match cx.node(x) {
            Node::Assign { left, .. } | Node::Update { arg: left, .. }
                if cx.src(cx.skip_parens(left)) == name =>
            {
                return true;
            }
            _ => {}
        }
        cx.ast.for_each_child(x, |c| stack.push(c));
    }
    false
}

/// Whether an identifier in `n` is named one of `names`.
fn mentions(cx: &Ctx<'_, '_, '_>, n: NodeId, names: &FxHashSet<String>) -> bool {
    let mut stack = vec![n];
    let mut steps = 0;
    while let Some(x) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return true;
        }
        if let Node::Ident { .. } = cx.node(x)
            && names.contains(cx.src(x))
        {
            return true;
        }
        cx.ast.for_each_child(x, |c| stack.push(c));
    }
    false
}

/// The names declared or assigned in `body` whose value may come from one of `seeds`
/// (variables initialised or assigned from them, functions and classes that mention them),
/// closed under that relation. By name, so a shadowing name only makes the walk more
/// careful.
fn derived_names(cx: &Ctx<'_, '_, '_>, body: NodeId, seeds: &[String]) -> FxHashSet<String> {
    let mut names: FxHashSet<String> = seeds.iter().cloned().collect();
    // (names it binds, the node its value comes from)
    let mut sources: Vec<(Vec<String>, NodeId)> = Vec::new();
    let mut stack = vec![body];
    let mut steps = 0;
    while let Some(x) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            break;
        }
        match cx.node(x) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => sources.push((binding_names(cx, name), init)),
            Node::Assign { left, right, .. } => {
                sources.push((binding_names(cx, cx.skip_parens(left)), right));
            }
            Node::FunctionDecl { name: Some(n), .. } | Node::ClassDecl { name: Some(n), .. } => {
                sources.push((vec![cx.src(n).to_owned()], x));
            }
            Node::ForOf { left, right, .. } | Node::ForIn { left, right, .. } => {
                sources.push((binding_names(cx, left), right));
            }
            _ => {}
        }
        cx.ast.for_each_child(x, |c| stack.push(c));
    }
    for _ in 0..16 {
        let before = names.len();
        for (bound, from) in &sources {
            if bound.iter().all(|b| names.contains(b)) {
                continue;
            }
            if mentions(cx, *from, &names) {
                names.extend(bound.iter().cloned());
            }
        }
        if names.len() == before {
            break;
        }
    }
    names
}

fn unreachable(cx: &Ctx<'_, '_, '_>, types: &Types) -> bool {
    types
        .iter()
        .flatten()
        .any(|&t| cx.flags(t).contains(TypeFlags::NEVER))
}

/// `types` with each tracked parameter narrowed by `cond` assumed true or false.
fn narrow(
    cx: &mut Ctx<'_, '_, '_>,
    w: &Walk<'_>,
    types: &Types,
    cond: NodeId,
    assume: bool,
) -> Types {
    let l = cx.loc(cond);
    let mut out = types.clone();
    for (i, slot) in out.iter_mut().enumerate() {
        let (Some(t), Some(p)) = (*slot, w.params.get(i).and_then(Option::as_ref)) else {
            continue;
        };
        let names: FxHashSet<String> = std::iter::once(p.name.clone()).collect();
        if !mentions(cx, cond, &names) {
            continue;
        }
        let (sym, declared, name) = (p.sym, p.declared, p.name.clone());
        *slot = Some(
            cx.ck
                .quietly(|ck| ck.narrow_symbol_by_condition(sym, &name, declared, t, l, assume)),
        );
    }
    out
}

fn join(cx: &mut Ctx<'_, '_, '_>, a: Option<Types>, b: Option<Types>) -> Option<Types> {
    match (a, b) {
        (Some(a), Some(b)) => Some(
            a.iter()
                .zip(&b)
                .map(|(x, y)| match (x, y) {
                    (Some(x), Some(y)) if x == y => Some(*x),
                    (Some(x), Some(y)) => {
                        let (x, y) = (*x, *y);
                        Some(cx.ck.quietly(|ck| ck.union(&[x, y])))
                    }
                    _ => None,
                })
                .collect(),
        ),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Walks statement `s` with the parameters at `types`; the types after it, `None` when it
/// never completes normally.
fn stmt(
    cx: &mut Ctx<'_, '_, '_>,
    w: &mut Walk<'_>,
    s: NodeId,
    types: Types,
    depth: u32,
) -> Option<Types> {
    if depth > MAX_DEPTH {
        w.opaque += 1;
        return Some(types);
    }
    if unreachable(cx, &types) {
        return None;
    }
    match cx.node(s) {
        Node::Block { stmts } => {
            let mut cur = types;
            for &x in cx.ast.list(stmts).to_vec().iter() {
                cur = stmt(cx, w, x, cur, depth + 1)?;
            }
            Some(cur)
        }
        Node::If { test, cons, alt } => {
            let tt = narrow(cx, w, &types, test, true);
            let ff = narrow(cx, w, &types, test, false);
            let constant = constant_test(cx, test);
            let a = match constant {
                Some(false) => None,
                _ => stmt(cx, w, cons, tt, depth + 1),
            };
            let b = match (constant, alt) {
                (Some(true), _) => None,
                (_, Some(alt)) => stmt(cx, w, alt, ff, depth + 1),
                (_, None) => Some(ff),
            };
            join(cx, a, b)
        }
        Node::Return { arg } => {
            if w.opaque == 0 {
                match arg {
                    Some(e) => ret(cx, w, e, &types),
                    None if !cx.is_assignable(TypeId::UNDEFINED, w.ret) => {
                        w.issues.push((cx.span(s), TypeId::UNDEFINED));
                    }
                    None => {}
                }
            }
            None
        }
        Node::Throw { .. } => None,
        Node::ExprStmt { .. } | Node::VarDecl { .. } => Some(types),
        Node::FunctionDecl { .. } | Node::ClassDecl { .. } => Some(types),
        Node::While { test, body }
        | Node::For {
            test: Some(test),
            body,
            ..
        } => {
            let tt = narrow(cx, w, &types, test, true);
            if constant_test(cx, test) != Some(false) {
                let _ = stmt(cx, w, body, tt, depth + 1);
            }
            let ff = narrow(cx, w, &types, test, false);
            join(cx, Some(ff), Some(types))
        }
        Node::For { body, .. }
        | Node::ForOf { body, .. }
        | Node::ForIn { body, .. }
        | Node::DoWhile { body, .. } => {
            let _ = stmt(cx, w, body, types.clone(), depth + 1);
            Some(types)
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            let mark = w.issues.len();
            let a = stmt(cx, w, block, types.clone(), depth + 1);
            let b = match handler {
                Some(h) => match cx.node(h) {
                    Node::CatchClause { body, .. } => stmt(cx, w, body, types.clone(), depth + 1),
                    _ => Some(types.clone()),
                },
                None => None,
            };
            let out = join(cx, a, b);
            if let Some(f) = finalizer {
                let end = w.issues.len();
                if stmt(cx, w, f, types, depth + 1).is_none() {
                    // A `finally` that returns or throws overrides every return before it.
                    w.issues.drain(mark..end);
                    return None;
                }
            }
            out
        }
        _ => {
            // `switch`, labels, `with`: returns inside are not narrowed by the walk.
            w.opaque += 1;
            let mut kids = Vec::new();
            cx.ast.for_each_child(s, |c| kids.push(c));
            for k in kids {
                if is_statement(cx, k) {
                    let _ = stmt(cx, w, k, types.clone(), depth + 1);
                }
            }
            w.opaque -= 1;
            Some(types)
        }
    }
}

/// The value of a `true` or `false` literal test: tsc's flow graph never enters the branch
/// it rules out.
fn constant_test(cx: &Ctx<'_, '_, '_>, test: NodeId) -> Option<bool> {
    match cx.node(cx.skip_parens(test)) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn is_statement(cx: &Ctx<'_, '_, '_>, n: NodeId) -> bool {
    matches!(
        cx.node(n),
        Node::Block { .. }
            | Node::If { .. }
            | Node::Return { .. }
            | Node::Throw { .. }
            | Node::ExprStmt { .. }
            | Node::While { .. }
            | Node::For { .. }
            | Node::ForOf { .. }
            | Node::ForIn { .. }
            | Node::DoWhile { .. }
            | Node::Try { .. }
            | Node::Switch { .. }
            | Node::SwitchCase { .. }
            | Node::Labeled { .. }
            | Node::VarDecl { .. }
    )
}

/// The index of the tracked parameter `e` is a plain reference to.
fn param_ref(cx: &Ctx<'_, '_, '_>, w: &Walk<'_>, e: NodeId) -> Option<usize> {
    if !matches!(cx.node(e), Node::Ident { .. }) {
        return None;
    }
    let sym = cx.ck.c.resolve_ident(cx.loc(e), meaning::VALUE)?;
    w.params
        .iter()
        .position(|p| p.as_ref().is_some_and(|p| p.sym == sym))
}

/// Checks `return e` (or a branch of a conditional it returns) under the overload.
fn ret(cx: &mut Ctx<'_, '_, '_>, w: &mut Walk<'_>, e: NodeId, types: &Types) {
    let e = cx.skip_parens(e);
    if let Node::Conditional { test, cons, alt } = cx.node(e) {
        for (branch, assume) in [(cons, true), (alt, false)] {
            let t = narrow(cx, w, types, test, assume);
            if !unreachable(cx, &t) {
                ret(cx, w, branch, &t);
            }
        }
        return;
    }
    let mut r = cx.ty(e);
    if w.is_async {
        r = cx.ck.quietly(|ck| ck.awaited_type(r));
    }
    if r.is_any_like() || cx.is_error(r) || cx.is_assignable(r, w.ret) {
        return;
    }
    if let Some(i) = param_ref(cx, w, e) {
        let Some(mut t) = types.get(i).copied().flatten() else {
            return;
        };
        if w.is_async {
            t = cx.ck.quietly(|ck| ck.awaited_type(t));
        }
        if !cx.is_assignable(t, w.ret) {
            w.issues.push((cx.span(e), t));
        }
        return;
    }
    if exact(cx, w, e, types) {
        w.issues.push((cx.span(e), r));
    }
}

/// Whether the checker's type of `e` is its type under the overload: every parameter it
/// reads has, there, a type within the overload's, and it reads nothing derived from a
/// parameter.
fn exact(cx: &mut Ctx<'_, '_, '_>, w: &Walk<'_>, e: NodeId, types: &Types) -> bool {
    let mut stack = vec![e];
    let mut steps = 0;
    while let Some(x) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return false;
        }
        cx.ast.for_each_child(x, |c| stack.push(c));
        if !matches!(cx.node(x), Node::Ident { .. }) {
            continue;
        }
        // A property name is not a read of a variable.
        if let Some(p) = cx.parent(x)
            && let Node::Member { prop, .. } = cx.node(p)
            && prop == x
        {
            continue;
        }
        if let Some(i) = param_ref(cx, w, x) {
            let Some(t) = types.get(i).copied().flatten() else {
                return false;
            };
            let real = cx.ty(x);
            if !cx.is_assignable(real, t) {
                return false;
            }
            continue;
        }
        if w.opaque_names.contains(cx.src(x)) {
            return false;
        }
    }
    true
}
