//! Signature help: the signatures at a call, `new`, tagged template, `super(...)`, type
//! argument list or contextually typed parameter list (see `signature_site`), with the
//! active signature and parameter.
//!
//! Rules learned from tsc 7's answers on its fourslash tests and from
//! `tsc: tsc/internal/ls/signaturehelp.go` and the candidate handling of
//! `tsc: tsc/internal/checker/checker.go` (`resolveCall`, the overload failure fallback),
//! written here on our checker:
//!
//! - The candidates are the callee's signatures. The one the call resolves to replaces its
//!   declaration in the list (generic ones show instantiated) and is the active one.
//! - When the call does not resolve, the active candidate is the first with a rest
//!   parameter or enough parameters for the arguments, else the one with the most. A generic
//!   one shows instantiated: with the written type arguments (missing ones take their
//!   default, else their constraint, else `unknown`, `any` in JavaScript), or with what the
//!   written arguments infer.
//! - A rest parameter of a tuple type shows as the tuple's elements; of a union of tuples,
//!   as one signature per member. Unlabeled elements are named from the rest parameter's
//!   binding pattern, else `name_i` (`name_n` for a rest element of a pattern), duplicates
//!   get `_1`, `_2`. The active one among a candidate's expansions is the first that is
//!   variadic or has enough parameters.
//! - Labels print like quick info (`a1.connectModule`, `c?: () => string` without the
//!   `undefined` the `?` adds), with the callee's name in front (no `new`), and type
//!   predicates as written (`a is number`).
//! - In type arguments, the parameters are the type parameters (`f<T extends X, U>(a: T):
//!   R`). A possible type argument list that did not parse as a call lists the callee's
//!   generic signatures with enough type parameters, uninstantiated; with none, the type's
//!   own type parameters (`C<T>`).
//! - In a contextually typed parameter list, the last call signature of the (non-nullable)
//!   contextual type, named after the function type's declaration (`cb`, `Cb`).
//! - The active parameter is the argument index; past a trailing rest parameter, the rest
//!   parameter; with a rest parameter before others, the parameter count (none active).
//! - A `new` of a class whose constructor is private or protected, from outside the class,
//!   gets no help.

use super::Ctx;
use super::locate::name_text;
use super::signature_site::{SiteKind, sites_at};
use serde_json::{Value, json};
use ts_r_ast::{Ast, KeywordTypeKind, MethodKind, Modifiers, Node, NodeId};
use ts_r_binder::meaning;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc, PrintMode};
use ts_r_diagnostics::FileId;
use ts_r_types::{PredicateKind, SigId, SymId, TupleElement, TypeFlags, TypeId};

/// Signatures listed at most (overload sets in lib files can be long).
const MAX_SIGNATURES: usize = 20;
/// Expanded parameter lists of one signature at most (a rest union of tuples).
const MAX_EXPANSIONS: usize = 20;

/// One signature as shown.
struct Item {
    label: String,
    /// The parameters' label offsets (UTF-16).
    params: Vec<[u32; 2]>,
    variadic: bool,
    first_rest: Option<usize>,
    doc: Option<String>,
}

pub fn signature_help(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let file = cx.file;
    // The innermost site that has signatures.
    let mut found = None;
    for site in sites_at(ck, file, offset) {
        let help = match site.kind {
            SiteKind::Call {
                call,
                callee,
                kind,
                type_list,
            } => call_help(ck, file, call, callee, kind, type_list, site.count),
            SiteKind::TypeArgs { called } => type_args_help(ck, file, called, site.count),
            SiteKind::Contextual { node } => contextual_help(ck, file, node),
            SiteKind::Jsx { opening, name } => jsx_help(ck, file, opening, name),
        };
        if let Some(h) = help {
            found = Some((h, site.index));
            break;
        }
    }
    let Some(((items, active), index)) = found else {
        return Value::Null;
    };
    if items.is_empty() {
        return Value::Null;
    }
    let active = active.min(items.len() - 1);
    let mut out = Vec::with_capacity(items.len());
    for it in &items {
        let params: Vec<Value> = it.params.iter().map(|p| json!({"label": p})).collect();
        let mut s = json!({"label": it.label, "parameters": params});
        if let Some(doc) = &it.doc {
            s["documentation"] = json!({"kind": "markdown", "value": doc});
        }
        out.push(s);
    }
    let mut v = json!({"signatures": out, "activeSignature": active});
    if let Some(p) = active_parameter(&items[active], index as usize) {
        v["activeParameter"] = json!(p);
    }
    v
}

/// The active parameter of `item` at argument `index`.
fn active_parameter(item: &Item, index: usize) -> Option<usize> {
    let n = item.params.len();
    if n == 0 {
        return None;
    }
    if item.variadic {
        if let Some(r) = item.first_rest
            && r < n - 1
        {
            return Some(n);
        }
        return Some(index.min(n - 1));
    }
    Some(index)
}

/// Help at a call, `new`, tagged template or `super(...)`.
fn call_help(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    call: NodeId,
    callee: NodeId,
    kind: SigKind,
    type_list: bool,
    count: u32,
) -> Option<(Vec<Item>, usize)> {
    let c = ck.c;
    let ast = c.ast(file);
    let at = |n: NodeId| Loc::new(file, n);
    let (sigs, name) = if matches!(ast.node(callee), Node::Super {}) {
        let base = super_base(ck, file, call)?;
        let t = ck.quietly(|ck| ck.check_expr(at(base)));
        let a = ck.apparent_type(t);
        (
            ck.signatures_of(a, SigKind::Construct),
            callee_name(ast, base),
        )
    } else {
        let t = ck.quietly(|ck| ck.check_expr(at(callee)));
        let a = ck.apparent_type(t);
        (ck.signatures_of(a, kind), callee_name(ast, callee))
    };
    let sigs: Vec<SigId> = sigs.into_iter().take(MAX_SIGNATURES).collect();
    if sigs.is_empty() {
        return None;
    }
    if kind == SigKind::Construct && !constructor_accessible(ck, file, call, sigs[0]) {
        return None;
    }
    let text = c.file(file).text.as_bytes();
    // The arguments with the parameter each one fills, and whether the list is unclosed
    // (then fewer arguments than required still resolve).
    let (type_args, args, incomplete) = match *ast.node(call) {
        Node::Call {
            type_args, args, ..
        }
        | Node::New {
            type_args, args, ..
        } => {
            let end = ast.span(call).end as usize;
            let args: Vec<(usize, NodeId)> = ast.list(args).iter().copied().enumerate().collect();
            (
                type_args,
                args,
                end == 0 || text.get(end - 1) != Some(&b')'),
            )
        }
        Node::TaggedTemplate { quasi, .. } => {
            let parts = match *ast.node(quasi) {
                Node::TemplateLit { parts } => ast.list(parts).to_vec(),
                _ => Vec::new(),
            };
            let args: Vec<(usize, NodeId)> = parts
                .iter()
                .skip(1)
                .step_by(2)
                .enumerate()
                .map(|(i, &e)| (i + 1, e))
                .collect();
            let end = ast.span(quasi).end as usize;
            let open = parts
                .last()
                .is_some_and(|&p| matches!(ast.node(p), Node::Missing {}))
                || end <= 1
                || text.get(end - 1) != Some(&b'`');
            (None, args, open)
        }
        _ => (None, Vec::new(), false),
    };
    let mut shown = sigs.clone();
    let mut active = None;
    if args
        .iter()
        .any(|&(_, a)| matches!(ast.node(a), Node::Missing {}))
    {
        // An argument not written yet fits any parameter: the first candidate whose arity
        // and written arguments fit is the one tsc resolves to.
        for (i, &s) in sigs.iter().enumerate() {
            let generic = !ck
                .store()
                .list_items(ck.store().sig(s).type_params)
                .is_empty();
            let fits = if generic {
                let shown_sig = instantiate_for_failure(ck, file, s, type_args, &args);
                resolved_applies(ck, file, shown_sig, None, count, incomplete, &[])
                    .then_some(shown_sig)
            } else {
                resolved_applies(ck, file, s, type_args, count, incomplete, &args).then_some(s)
            };
            if let Some(f) = fits {
                shown[i] = f;
                active = Some(i);
                break;
            }
        }
    } else if let Some(r) = ck.resolved_signature(at(call))
        && resolved_applies(ck, file, r, type_args, count, incomplete, &args)
        && let Some(i) = sigs.iter().position(|&s| derives_from(ck, r, s))
    {
        shown[i] = r;
        active = Some(i);
    }
    let active = match active {
        Some(i) => i,
        None => {
            let i = longest_candidate(ck, &sigs, count as usize);
            shown[i] = instantiate_for_failure(ck, file, sigs[i], type_args, &args);
            i
        }
    };
    let scope = at(call);
    let mut items = Vec::new();
    let mut active_item = 0;
    for (i, (&s, &orig)) in shown.iter().zip(&sigs).enumerate() {
        let doc = signature_docs(ck, orig);
        let mut its = signature_items(ck, s, &name, scope, type_list);
        if i == active {
            active_item = items.len();
            if its.len() > 1
                && let Some(k) = its
                    .iter()
                    .position(|it| it.variadic || it.params.len() >= count as usize)
            {
                active_item += k;
            }
        }
        for it in &mut its {
            it.doc.clone_from(&doc);
        }
        items.extend(its);
    }
    Some((items, active_item))
}

/// Help in a JSX opening tag: a function component's signatures, the first active (tsc 7
/// does not resolve the element here).
fn jsx_help(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    opening: NodeId,
    name: NodeId,
) -> Option<(Vec<Item>, usize)> {
    let ast = ck.c.ast(file);
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return None;
    }
    let t = ck.quietly(|ck| ck.check_expr(Loc::new(file, name)));
    let a = ck.apparent_type(t);
    let sigs = ck.signatures_of(a, SigKind::Call);
    let label = callee_name(ast, name);
    let mut items = Vec::new();
    for s in sigs.into_iter().take(MAX_SIGNATURES) {
        let doc = signature_docs(ck, s);
        for mut it in signature_items(ck, s, &label, Loc::new(file, opening), false) {
            it.doc.clone_from(&doc);
            items.push(it);
        }
    }
    (!items.is_empty()).then_some((items, 0))
}

/// Whether the signature the checker resolved a call to is one tsc resolves it to: not left
/// generic, written type arguments in its range, and the arguments (a trailing comma counts
/// one more) within its arity.
#[allow(clippy::too_many_arguments)]
fn resolved_applies(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    r: SigId,
    type_args: Option<NodeId>,
    count: u32,
    incomplete: bool,
    args: &[(usize, NodeId)],
) -> bool {
    let ast = ck.c.ast(file);
    let d = ck.store().sig(r);
    if !ck.store().list_items(d.type_params).is_empty() {
        return false;
    }
    let (min, n, rest) = (d.min_args, d.params.len() as u32, d.has_rest());
    if (count < min && !incomplete) || (!rest && count > n) {
        return false;
    }
    // Each argument written fits its parameter (functions are left to the checker).
    for &(i, a) in args {
        if is_skipped_argument(ast, a) {
            continue;
        }
        let Some(pt) = parameter_type_at(ck, r, i) else {
            continue;
        };
        let at = ck.quietly(|ck| ck.check_expr(Loc::new(file, a)));
        if !ck.is_assignable(at, pt) {
            return false;
        }
    }
    if let Some(&Node::TypeArgs { args }) = type_args.map(|t| ast.node(t)) {
        let written = ast.list(args).len();
        let mut target = r;
        for _ in 0..8 {
            match ck.store().sig(target).target {
                Some((t, _)) => target = t,
                None => break,
            }
        }
        let tps: Vec<TypeId> = ck
            .store()
            .list_items(ck.store().sig(target).type_params)
            .to_vec();
        let required = tps
            .iter()
            .filter(|&&tp| ck.type_param_default(tp).is_none())
            .count();
        if written < required || written > tps.len() {
            return false;
        }
    }
    true
}

/// Arguments not used to pick or infer a candidate here: spreads, functions (typed by the
/// candidate) and missing ones.
fn is_skipped_argument(ast: &Ast, a: NodeId) -> bool {
    matches!(
        ast.node(a),
        Node::Spread { .. } | Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::Missing {}
    )
}

/// The type of the parameter argument `i` fills (a rest parameter's element type past it).
fn parameter_type_at(ck: &mut Ck<'_, '_>, sig: SigId, i: usize) -> Option<TypeId> {
    let d = ck.store().sig(sig);
    let rest = d.has_rest();
    let n = d.params.len();
    if i < n && !(rest && i + 1 >= n) {
        return Some(d.params[i].ty);
    }
    if rest && n > 0 {
        let last = d.params[n - 1].ty;
        return ck.element_type_of_array_like(last);
    }
    None
}

/// Whether `r` is `s` or an instantiation of it.
fn derives_from(ck: &Ck<'_, '_>, r: SigId, s: SigId) -> bool {
    let mut cur = r;
    for _ in 0..8 {
        if cur == s {
            return true;
        }
        match ck.store().sig(cur).target {
            Some((t, _)) => cur = t,
            None => return false,
        }
    }
    false
}

/// The candidate a call that does not resolve shows: the first with a rest parameter or at
/// least `count` parameters, else the one with the most.
fn longest_candidate(ck: &Ck<'_, '_>, sigs: &[SigId], count: usize) -> usize {
    let mut best = (0, -1i64);
    for (i, &s) in sigs.iter().enumerate() {
        let d = ck.store().sig(s);
        let n = d.params.len();
        if d.has_rest() || n >= count {
            return i;
        }
        if n as i64 > best.1 {
            best = (i, n as i64);
        }
    }
    best.0
}

/// A generic candidate of a call that does not resolve, instantiated as tsc shows it (see
/// the module docs).
pub(super) fn instantiate_for_failure(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    sig: SigId,
    type_args: Option<NodeId>,
    args: &[(usize, NodeId)],
) -> SigId {
    let tps: Vec<TypeId> = ck
        .store()
        .list_items(ck.store().sig(sig).type_params)
        .to_vec();
    if tps.is_empty() {
        return sig;
    }
    let c = ck.c;
    let ast = c.ast(file);
    let js = c.file(file).is_js();
    let written: Vec<NodeId> = match type_args.map(|n| ast.node(n)) {
        Some(&Node::TypeArgs { args }) => ast.list(args).to_vec(),
        _ => Vec::new(),
    };
    let types: Vec<TypeId> = if !written.is_empty() {
        let mut out = Vec::with_capacity(tps.len());
        for (i, &tp) in tps.iter().enumerate() {
            let t = match written.get(i) {
                Some(&w) => ck.type_from_node(Loc::new(file, w)),
                None => match ck.type_param_default(tp) {
                    Some(d) => d,
                    None => ck.type_param_constraint(tp).unwrap_or(if js {
                        TypeId::ANY
                    } else {
                        TypeId::UNKNOWN
                    }),
                },
            };
            out.push(t);
        }
        out
    } else if js {
        // Observed: tsc 7 shows a JavaScript call that does not resolve with `any` for each
        // type parameter.
        vec![TypeId::ANY; tps.len()]
    } else {
        let mut ctx = ck.inference_context_for(sig);
        for &(i, a) in args {
            if is_skipped_argument(ast, a) {
                continue;
            }
            let Some(pt) = parameter_type_at(ck, sig, i) else {
                continue;
            };
            let at = ck.quietly(|ck| ck.check_expr(Loc::new(file, a)));
            ck.infer_types(&mut ctx, at, pt, 0);
        }
        (0..tps.len())
            .map(|i| ck.inferred_type_at(&mut ctx, i))
            .collect()
    };
    let m = ck.store().mapper(&tps, &types);
    ck.instantiate_signature_full(sig, m)
}

/// The class a `super(...)` call constructs: the `extends` expression of the class around it.
fn super_base(ck: &Ck<'_, '_>, file: FileId, call: NodeId) -> Option<NodeId> {
    let c = ck.c;
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    let mut cur = parents.parent(call);
    for _ in 0..10_000 {
        let n = cur?;
        if let Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } = *ast.node(n) {
            for &h in ast.list(heritage) {
                if let Node::HeritageClause {
                    kind: ts_r_ast::HeritageKind::Extends,
                    types,
                } = *ast.node(h)
                    && let Some(&e) = ast.list(types).first()
                {
                    return match *ast.node(e) {
                        Node::ExprWithTypeArgs { expr, .. } => Some(expr),
                        _ => Some(e),
                    };
                }
            }
            return None;
        }
        cur = parents.parent(n);
    }
    None
}

/// Whether the constructor of `sig` may be called at `call`: a private or protected one
/// only from inside its class.
fn constructor_accessible(ck: &Ck<'_, '_>, file: FileId, call: NodeId, sig: SigId) -> bool {
    let c = ck.c;
    let Some((f, n)) = ck.store().sig(sig).decl else {
        return true;
    };
    if f as usize >= c.files.len() {
        return true;
    }
    let Some(node) = ck.node_id(n) else {
        return true;
    };
    let df = FileId(f);
    let dast = c.ast(df);
    let Node::MethodDef {
        kind: MethodKind::Constructor,
        modifiers,
        ..
    } = *dast.node(node)
    else {
        return true;
    };
    if !modifiers.intersects(Modifiers::PRIVATE | Modifiers::PROTECTED) {
        return true;
    }
    let Some(class) = c.file(df).parents().parent(node) else {
        return true;
    };
    if df != file {
        return false;
    }
    let cs = dast.span(class);
    let at = c.ast(file).span(call);
    cs.start <= at.start && at.end <= cs.end
}

/// Help in a possible type argument list that did not parse as a call's.
fn type_args_help(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    called: NodeId,
    count: u32,
) -> Option<(Vec<Item>, usize)> {
    let c = ck.c;
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    let at = Loc::new(file, called);
    let parent = parents.parent(called);
    let in_type = parent.is_some_and(|p| {
        matches!(
            ast.node(p),
            Node::TypeRef { .. } | Node::QualifiedName { .. } | Node::ExprWithTypeArgs { .. }
        )
    });
    let name = name_text(ast, called).unwrap_or("").to_string();
    if !in_type {
        let is_new = parent
            .is_some_and(|p| matches!(*ast.node(p), Node::New { callee, .. } if callee == called));
        let t = ck.quietly(|ck| ck.check_expr(at));
        let a = ck.apparent_type(t);
        let kind = if is_new {
            SigKind::Construct
        } else {
            SigKind::Call
        };
        let sigs: Vec<SigId> = ck
            .signatures_of(a, kind)
            .into_iter()
            .filter(|&s| {
                let n = ck.store().list_items(ck.store().sig(s).type_params).len();
                n > 0 && n >= count as usize
            })
            .take(MAX_SIGNATURES)
            .collect();
        if !sigs.is_empty() {
            let mut items = Vec::new();
            for s in sigs {
                let doc = signature_docs(ck, s);
                let mut its = signature_items(ck, s, &name, at, true);
                for it in &mut its {
                    it.doc.clone_from(&doc);
                }
                items.extend(its);
            }
            return Some((items, 0));
        }
    }
    // The type's own type parameters: `C<T>`.
    let s = c
        .resolve_ident(at, meaning::TYPE)
        .or_else(|| c.resolve_ident(at, meaning::VALUE))?;
    let s = c.resolve_alias(s).unwrap_or(s);
    let tps = ck.local_type_params(s);
    if tps.is_empty() {
        return None;
    }
    let mode = PrintMode::quick_info(at);
    let mut label = name;
    label.push('<');
    let mut params = Vec::new();
    for (i, &tp) in tps.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let start = utf16_len(&label);
        label.push_str(&type_param_text(ck, tp, mode));
        params.push([start, utf16_len(&label)]);
    }
    label.push('>');
    let item = Item {
        label,
        params,
        variadic: false,
        first_rest: None,
        doc: None,
    };
    Some((vec![item], 0))
}

/// Help in a parameter list typed by its context.
fn contextual_help(ck: &mut Ck<'_, '_>, file: FileId, node: NodeId) -> Option<(Vec<Item>, usize)> {
    let c = ck.c;
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    let at = |n: NodeId| Loc::new(file, n);
    // A method in an object literal: its property's contextual type.
    let method = parents
        .parent(node)
        .filter(|&p| matches!(ast.node(p), Node::MethodDef { .. }));
    let ct = match method.map(|m| (m, ast.node(m))) {
        Some((m, &Node::MethodDef { key, .. })) => {
            let obj = parents.parent(m)?;
            if !matches!(ast.node(obj), Node::ObjectLit { .. }) {
                return None;
            }
            let ot = ck.contextual_type(at(obj))?;
            let name = ts_r_checker::nodes::key_text(ast, key)?;
            ck.contextual_property_type(ot, &name)?
        }
        _ => ck.contextual_type(at(node))?,
    };
    let t = ck.non_nullable(ct);
    if ck
        .store()
        .flags(t)
        .intersects(TypeFlags::UNION | TypeFlags::ANY_OR_UNKNOWN)
    {
        return None;
    }
    let a = ck.apparent_type(t);
    let sigs = ck.signatures_of(a, SigKind::Call);
    let &sig = sigs.last()?;
    let name = contextual_name(ck, t, sig);
    let mut items = signature_items(ck, sig, &name, at(node), false);
    let doc = signature_docs(ck, sig);
    for it in &mut items {
        it.doc.clone_from(&doc);
    }
    Some((items, 0))
}

/// The name a contextual signature shows with: its type alias, or the declaration its
/// function type annotates (a parameter, variable or property), or its member's name.
fn contextual_name(ck: &Ck<'_, '_>, t: TypeId, sig: SigId) -> String {
    let c = ck.c;
    if let Some(info) = ck.store().alias_of(t) {
        return c.sym_name(info.sym).to_owned();
    }
    let target = ck.store().sig(sig).target.map_or(sig, |t| t.0);
    let Some((f, n)) = ck.store().sig(target).decl else {
        return String::new();
    };
    if f as usize >= c.files.len() {
        return String::new();
    }
    let Some(node) = ck.node_id(n) else {
        return String::new();
    };
    let df = FileId(f);
    let ast = c.ast(df);
    let parents = c.file(df).parents();
    let named = |d: NodeId| -> String {
        if let Node::VarDeclarator { name, .. } = *ast.node(d) {
            return name_text(ast, name).unwrap_or("").to_owned();
        }
        ts_r_binder::decl_name_node(ast, d)
            .and_then(|k| ts_r_checker::nodes::key_text(ast, k))
            .unwrap_or_default()
    };
    match ast.node(node) {
        Node::FunctionType { .. } => {
            let mut p = parents.parent(node);
            for _ in 0..8 {
                let Some(x) = p else { break };
                match ast.node(x) {
                    Node::ParenType { .. } => p = parents.parent(x),
                    _ => return named(x),
                }
            }
            String::new()
        }
        Node::CallSignature { .. } => parents.parent(node).map(named).unwrap_or_default(),
        _ => named(node),
    }
}

/// The signature `sig` as shown, one item per expansion of its rest parameter.
fn signature_items(
    ck: &mut Ck<'_, '_>,
    sig: SigId,
    prefix: &str,
    scope: Loc,
    type_list: bool,
) -> Vec<Item> {
    let mode = PrintMode::quick_info(scope);
    let d = ck.store().sig(sig);
    let own_tps: Vec<TypeId> = ck.store().list_items(d.type_params).to_vec();
    let has_rest = d.has_rest();
    let list_tps: Vec<TypeId> = if type_list {
        let target = d.target.map_or(sig, |t| t.0);
        ck.store()
            .list_items(ck.store().sig(target).type_params)
            .to_vec()
    } else {
        own_tps
    };
    let tp_texts: Vec<String> = list_tps
        .iter()
        .map(|&tp| type_param_text(ck, tp, mode))
        .collect();
    let lists = expanded_params(ck, sig);
    let ret = return_text(ck, sig, mode);
    let n_lists = lists.len();
    let mut out = Vec::with_capacity(n_lists);
    for list in lists {
        let mut label = prefix.to_string();
        let mut params = Vec::new();
        if !tp_texts.is_empty() || type_list {
            label.push('<');
            for (i, t) in tp_texts.iter().enumerate() {
                if i > 0 {
                    label.push_str(", ");
                }
                let start = utf16_len(&label);
                label.push_str(t);
                if type_list {
                    params.push([start, utf16_len(&label)]);
                }
            }
            label.push('>');
        }
        label.push('(');
        for (i, p) in list.iter().enumerate() {
            if i > 0 {
                label.push_str(", ");
            }
            let ty = match &p.written {
                Some(w) => w.clone(),
                None => ck.type_to_string_with(p.ty, mode),
            };
            let text = if p.rest {
                format!("...{}: {ty}", p.name)
            } else if p.optional {
                format!("{}?: {ty}", p.name)
            } else {
                format!("{}: {ty}", p.name)
            };
            let start = utf16_len(&label);
            label.push_str(&text);
            if !type_list {
                params.push([start, utf16_len(&label)]);
            }
        }
        label.push_str("): ");
        label.push_str(&ret);
        let variadic =
            !type_list && has_rest && (n_lists == 1 || list.last().is_some_and(|p| p.rest));
        out.push(Item {
            label,
            params,
            variadic,
            first_rest: list.iter().position(|p| p.rest),
            doc: None,
        });
    }
    out
}

/// One parameter as shown.
#[derive(Clone)]
struct Shown {
    name: String,
    ty: TypeId,
    /// The annotation as written, when tsc reuses it (a string literal type keeps its
    /// quotes: `x: 'hi'`).
    written: Option<String>,
    optional: bool,
    rest: bool,
}

/// The parameter lists `sig` shows (see the module docs).
fn expanded_params(ck: &mut Ck<'_, '_>, sig: SigId) -> Vec<Vec<Shown>> {
    let d = ck.store().sig(sig);
    let params = d.params.to_vec();
    let min = d.min_args as usize;
    let rest = d.has_rest() && !params.is_empty();
    let fixed = params.len() - usize::from(rest);
    let d_target = d.target;
    let mut base = Vec::with_capacity(params.len());
    for (i, p) in params.iter().take(fixed).enumerate() {
        let optional = i >= min && p.optional;
        let ty = if optional {
            without_optional_undefined(ck, p.ty, p.sym)
        } else {
            p.ty
        };
        let written = if d_target.is_none() {
            written_literal(ck, sig, p.sym, i)
        } else {
            None
        };
        base.push(Shown {
            name: ck.store().str(p.name).to_owned(),
            ty,
            written,
            optional,
            rest: false,
        });
    }
    if !rest {
        return vec![base];
    }
    let rp = params[fixed];
    let rname = ck.store().str(rp.name).to_owned();
    let tuples: Vec<TypeId> = if ck.c.is_tuple_type(rp.ty) {
        vec![rp.ty]
    } else if ck.store().flags(rp.ty).contains(TypeFlags::UNION) {
        let ms = ck.union_members(rp.ty);
        if ms.iter().all(|&m| ck.c.is_tuple_type(m)) {
            ms
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    if tuples.is_empty() {
        base.push(Shown {
            name: rname,
            ty: rp.ty,
            written: None,
            optional: false,
            rest: true,
        });
        return vec![base];
    }
    let rest_decl = param_decl(ck, sig, rp.sym, fixed);
    let mut out = Vec::new();
    for t in tuples.into_iter().take(MAX_EXPANSIONS) {
        let mut list = base.clone();
        list.extend(tuple_params(ck, t, &rname, rest_decl));
        out.push(list);
    }
    out
}

/// The elements of tuple `t` as the parameters a rest parameter of that type stands for.
fn tuple_params(ck: &mut Ck<'_, '_>, t: TypeId, rest_name: &str, decl: Option<Loc>) -> Vec<Shown> {
    let Some(shape) = ck.store().tuple_shape_of(t).cloned() else {
        return Vec::new();
    };
    let args = ck.c.type_args_of_tuple(t).to_vec();
    let mut names: Vec<String> = shape
        .elements
        .iter()
        .enumerate()
        .map(|(i, el)| {
            if let Some(l) = el.label {
                return ck.store().str(l).to_owned();
            }
            match decl {
                Some(d) => binding_label(ck.c.ast(d.file), d.node, i, el.kind, 0),
                None => format!("{rest_name}_{i}"),
            }
        })
        .collect();
    dedupe(&mut names);
    let mut out = Vec::with_capacity(names.len());
    for (i, (el, name)) in shape.elements.iter().zip(names).enumerate() {
        let a = args.get(i).copied().unwrap_or(TypeId::ANY);
        let (ty, optional, rest) = match el.kind {
            TupleElement::OPTIONAL => (a, true, false),
            TupleElement::REST => (ck.create_array(a, false), false, true),
            TupleElement::VARIADIC => (a, false, true),
            _ => (a, false, false),
        };
        out.push(Shown {
            name,
            ty,
            written: None,
            optional,
            rest,
        });
    }
    out
}

/// A parameter annotated with a string literal type, as written.
fn written_literal(ck: &Ck<'_, '_>, sig: SigId, sym: SymId, index: usize) -> Option<String> {
    let l = param_decl(ck, sig, sym, index)?;
    let ast = ck.c.ast(l.file);
    let Node::Param {
        type_ann: Some(t), ..
    } = *ast.node(l.node)
    else {
        return None;
    };
    if !matches!(ast.node(t), Node::StringLit { .. }) {
        return None;
    }
    let text = ck.c.file(l.file).text.as_str();
    Some(ast.span(t).text(text).to_owned())
}

/// The declaration of parameter `index` of `sig` (its symbol's, or the one at that position
/// in the signature's declaration).
fn param_decl(ck: &Ck<'_, '_>, sig: SigId, sym: SymId, index: usize) -> Option<Loc> {
    let c = ck.c;
    if sym != SymId::NONE
        && let Some(mut l) = c.first_decl(sym)
    {
        for _ in 0..4 {
            if matches!(c.ast(l.file).node(l.node), Node::Param { .. }) {
                return Some(l);
            }
            l = c.parent(l)?;
        }
    }
    let mut target = sig;
    for _ in 0..8 {
        match ck.store().sig(target).target {
            Some((t, _)) => target = t,
            None => break,
        }
    }
    let (f, n) = ck.store().sig(target).decl?;
    if f as usize >= c.files.len() {
        return None;
    }
    let file = FileId(f);
    let ast = c.ast(file);
    let mut node = ck.node_id(n)?;
    if let Node::MethodDef { value, .. } = *ast.node(node) {
        node = value;
    }
    let params = match *ast.node(node) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. }
        | Node::FunctionType { params, .. }
        | Node::ConstructorType { params, .. }
        | Node::MethodSignature { params, .. }
        | Node::CallSignature { params, .. }
        | Node::ConstructSignature { params, .. } => params,
        _ => return None,
    };
    let p = *ast.list(params).get(index)?;
    matches!(ast.node(p), Node::Param { .. }).then(|| Loc::new(file, p))
}

/// The name element `index` of a rest parameter's tuple takes from the binding at `node` (the
/// parameter, or an element of its array pattern).
fn binding_label(ast: &Ast, node: NodeId, index: usize, kind: u8, depth: u32) -> String {
    let fallback = || format!("arg_{index}");
    if depth > 32 {
        return fallback();
    }
    let (name, dots) = match *ast.node(node) {
        Node::Param { pattern, rest, .. } => (pattern, rest),
        Node::Rest { arg } => (arg, true),
        Node::AssignPattern { left, .. } => (left, false),
        _ => (node, false),
    };
    let variable = kind >= TupleElement::REST;
    match *ast.node(name) {
        Node::Ident { .. } => {
            let n = name_text(ast, name).unwrap_or("arg");
            if dots {
                if variable {
                    n.to_owned()
                } else {
                    format!("{n}_{index}")
                }
            } else if !variable {
                n.to_owned()
            } else {
                format!("{n}_n")
            }
        }
        Node::ArrayPattern { elements } if dots => {
            let els = ast.list(elements);
            let last_rest = els
                .last()
                .is_some_and(|&e| matches!(ast.node(e), Node::Rest { .. }));
            let count = els.len() - usize::from(last_rest);
            if index < count {
                let e = els[index];
                if matches!(ast.node(e), Node::Hole {}) {
                    return fallback();
                }
                return binding_label(ast, e, index, kind, depth + 1);
            }
            if last_rest && let Some(&e) = els.last() {
                return binding_label(ast, e, index - count, kind, depth + 1);
            }
            fallback()
        }
        _ => fallback(),
    }
}

/// Repeated names get `_1`, `_2`, ... (skipping names already taken).
fn dedupe(names: &mut [String]) {
    let mut seen = std::collections::HashSet::new();
    let mut dups = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if !seen.insert(n.clone()) {
            dups.push(i);
        }
    }
    for i in dups {
        for k in 1..=names.len() + 1 {
            let cand = format!("{}_{k}", names[i]);
            if seen.insert(cand.clone()) {
                names[i] = cand;
                break;
            }
        }
    }
}

/// An optional parameter's type without the `undefined` its `?` adds, unless its annotation
/// spells `undefined` out.
fn without_optional_undefined(ck: &mut Ck<'_, '_>, ty: TypeId, sym: SymId) -> TypeId {
    if !ck.c.opts.strict_null_checks || annotation_spells_undefined(ck, sym) {
        return ty;
    }
    let t = ck.filter_type(ty, |ck, x| {
        !ck.store().flags(x).contains(TypeFlags::UNDEFINED)
    });
    if ck.store().flags(t).contains(TypeFlags::NEVER) {
        ty
    } else {
        t
    }
}

fn annotation_spells_undefined(ck: &Ck<'_, '_>, sym: SymId) -> bool {
    if sym == SymId::NONE {
        return false;
    }
    let Some(d) = ck.c.first_decl(sym) else {
        return false;
    };
    let ast = ck.c.ast(d.file);
    let Node::Param {
        type_ann: Some(mut n),
        ..
    } = *ast.node(d.node)
    else {
        return false;
    };
    for _ in 0..64 {
        match *ast.node(n) {
            Node::ParenType { ty } => n = ty,
            _ => break,
        }
    }
    let is_undefined = |n: NodeId| {
        matches!(
            ast.node(n),
            Node::KeywordType {
                kind: KeywordTypeKind::Undefined
            }
        )
    };
    match *ast.node(n) {
        Node::UnionType { types } => ast.list(types).iter().any(|&m| is_undefined(m)),
        _ => is_undefined(n),
    }
}

/// `T`, `T extends C` or `T extends C = D`.
fn type_param_text(ck: &mut Ck<'_, '_>, tp: TypeId, mode: PrintMode) -> String {
    let mut s = ck.type_to_string_with(tp, mode);
    if let Some(c) = ck.type_param_constraint(tp) {
        s.push_str(" extends ");
        s.push_str(&ck.type_to_string_with(c, mode));
    }
    if let Some(d) = ck.type_param_default(tp) {
        s.push_str(" = ");
        s.push_str(&ck.type_to_string_with(d, mode));
    }
    s
}

/// The return type, or the type predicate, as shown after `): `.
fn return_text(ck: &mut Ck<'_, '_>, sig: SigId, mode: PrintMode) -> String {
    if let Some(p) = ck.predicate_of(sig) {
        let name = match p.kind {
            PredicateKind::This | PredicateKind::AssertsThis => "this".to_owned(),
            _ => ck.store().str(p.param_name).to_owned(),
        };
        let asserts = matches!(
            p.kind,
            PredicateKind::AssertsIdentifier | PredicateKind::AssertsThis
        );
        return match (asserts, p.ty) {
            (true, None) => format!("asserts {name}"),
            (true, Some(t)) => format!("asserts {name} is {}", ck.type_to_string_with(t, mode)),
            (false, Some(t)) => format!("{name} is {}", ck.type_to_string_with(t, mode)),
            (false, None) => {
                let r = ck.return_type_of(sig);
                ck.type_to_string_with(r, mode)
            }
        };
    }
    let r = ck.return_type_of(sig);
    ck.type_to_string_with(r, mode)
}

fn callee_name(ast: &Ast, callee: NodeId) -> String {
    match *ast.node(callee) {
        Node::Ident { .. } => name_text(ast, callee).unwrap_or("").to_string(),
        Node::Member { prop, .. } => name_text(ast, prop).unwrap_or("").to_string(),
        Node::Paren { expr } => callee_name(ast, expr),
        _ => String::new(),
    }
}

fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// JSDoc of the signature's declaration.
fn signature_docs(ck: &Ck<'_, '_>, sig: SigId) -> Option<String> {
    let c = ck.c;
    let target = ck.store().sig(sig).target.map(|t| t.0).unwrap_or(sig);
    let (f, n) = ck.store().sig(target).decl?;
    if f as usize >= c.files.len() {
        return None;
    }
    let node = ck.node_id(n)?;
    let d = Loc::new(FileId(f), node);
    let ast = c.ast(d.file);
    let text = c.file(d.file).text.as_str();
    // The comment before the declaration (or its variable statement for `const f = () =>`).
    let mut at = d;
    for _ in 0..4 {
        match c.parent(at).map(|p| (p, c.node(p))) {
            Some((
                p,
                Node::VarDeclarator { .. } | Node::VarDecl { .. } | Node::ExportDefaultExpr { .. },
            )) => at = p,
            _ => break,
        }
    }
    let start = ast.span(at.node).start;
    let comment = ast.comments_between(0, start).last()?;
    let between = text.get(comment.span.end as usize..start as usize)?;
    if !between.trim().is_empty() {
        return None;
    }
    let raw = comment.span.text(text);
    raw.starts_with("/**")
        .then(|| super::jsdoc::render(&super::jsdoc::parse(raw), &mut |_| None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_are_numbered() {
        let mut v: Vec<String> = ["a", "a", "a_1", "b", "a"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        dedupe(&mut v);
        assert_eq!(v, ["a", "a_2", "a_1", "b", "a_3"]);
    }
}
