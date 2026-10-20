//! Sound relations: narrowing type assertions (S-02), invariant mutable containers (S-05) and
//! contravariant methods (S-06).
//!
//! The checker's relation is TypeScript's; sound mode only adds the two stricter checks on top
//! of it. Where tsc already accepts `source` for `target`, [`violation`] walks both types and
//! finds the first place where the sound relation differs: a mutable container whose type
//! arguments are not identical, or a method declared in a sound file whose parameter is
//! narrower in the source. Sites are the positions tsc checks assignability at (see
//! [`Ctx::site_of`]), plus class members against their base class and interfaces.

use rustc_hash::FxHashSet;
use ts_r_ast::{BinaryOp, HeritageKind, MethodKind, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::checker::GlobalType;
use ts_r_checker::members::SigKind;
use ts_r_span::Span;
use ts_r_types::{ObjectKind, SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::{Ctx, Site};

/// Walk depth limit for nested types (deeper differences are not reported).
const MAX_DEPTH: u32 = 8;
/// Properties compared per object type pair.
const MAX_PROPS: usize = 256;

pub enum Violation {
    /// A mutable container (`target` side) whose type arguments differ.
    Container { target: TypeId },
    /// A method whose parameter `index` is narrower in the source.
    Method {
        name: String,
        param: String,
        source_param: TypeId,
        target_param: TypeId,
        source_prop: Option<SymId>,
    },
}

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    let s02 = cx.has_rule(2);
    let s05 = cx.has_rule(5);
    let s06 = cx.has_rule(6);
    if !(s02 || s05 || s06) {
        return;
    }
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        match cx.node(n) {
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } if s02 => {
                assertion(cx, n, expr, ty);
            }
            Node::ClassDecl {
                heritage, members, ..
            }
            | Node::ClassExpr {
                heritage, members, ..
            } if s06 => {
                let h = cx.ast.list(heritage).to_vec();
                let m = cx.ast.list(members).to_vec();
                heritage_methods(cx, n, &h, &m);
            }
            _ => {}
        }
    }
    if s05 || s06 {
        for (e, site) in cx.sites() {
            site_check(cx, e, site);
        }
    }
}

/// S-02: `e as T` / `<T>e` must be an upcast under sound relations (`any` counts as `unknown`).
fn assertion(cx: &mut Ctx<'_, '_, '_>, n: NodeId, expr: NodeId, ty: NodeId) {
    if cx.src(ty) == "const" {
        return;
    }
    let target = cx.type_node(ty);
    if cx.flags(target).intersects(TypeFlags::ANY_OR_UNKNOWN) || target.is_any_like() {
        return;
    }
    let source = cx.ty(expr);
    // `any` (and the checker's `any` for imports of shorthand ambient modules) is `unknown`.
    let is_any = source == TypeId::ANY || cx.is_error(source);
    let narrows = if is_any {
        true
    } else if source.is_any_like() {
        false
    } else {
        !cx.is_assignable(source, target)
            || violation(cx, source, target, 0, &mut FxHashSet::default()).is_some()
    };
    if narrows {
        let s = if is_any {
            "unknown".to_owned()
        } else {
            cx.type_str(source)
        };
        let t = cx.type_str(target);
        cx.report(codes::NARROWING_ASSERTION, cx.span(n), &[&s, &t]);
    }
}

fn site_check(cx: &mut Ctx<'_, '_, '_>, e: NodeId, site: Site) {
    let source = cx.ty(e);
    if source.is_any_like() || site.target.is_any_like() {
        return;
    }
    if !cx.is_assignable(source, site.target) {
        return; // tsc reports it already.
    }
    let Some(found) = expr_violation(cx, e, site.target, 0) else {
        return;
    };
    let at = found.span.unwrap_or(cx.span(site.report));
    report_violation(cx, found.v, found.source, found.target, found.expr, at);
}

struct Found {
    v: Violation,
    source: TypeId,
    target: TypeId,
    expr: NodeId,
    /// Overrides the site's report position (object literal members).
    span: Option<Span>,
}

/// Fresh literals (array and object literals, through `?:` and `||`) are walked by
/// expression: their own elements are contextually typed and never aliased, so only the
/// values they hold are compared. Anything else is compared by type.
fn expr_violation(
    cx: &mut Ctx<'_, '_, '_>,
    e: NodeId,
    target: TypeId,
    depth: u32,
) -> Option<Found> {
    if depth > MAX_DEPTH || target.is_any_like() {
        return None;
    }
    let e = cx.skip_parens(e);
    let literal = matches!(cx.node(e), Node::ArrayLit { .. } | Node::ObjectLit { .. });
    if literal && cx.flags(target).contains(TypeFlags::UNION) {
        let st = cx.ty(e);
        let members = cx.ck.store().members_of_union(target).to_vec();
        let mut first = None;
        for m in members {
            if !cx.is_assignable(st, m) {
                continue;
            }
            match expr_violation(cx, e, m, depth + 1) {
                None => return None,
                Some(f) => {
                    if first.is_none() {
                        first = Some(f);
                    }
                }
            }
        }
        return first;
    }
    match cx.node(e) {
        Node::As { .. }
        | Node::TypeAssertion { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. } => None,
        Node::ArrayLit { elements } => {
            let elem = cx.ck.quietly(|ck| ck.element_type_of_array_like(target))?;
            for &el in cx.ast.list(elements).to_vec().iter() {
                if matches!(cx.node(el), Node::Spread { .. }) {
                    continue; // a copy
                }
                if let Some(f) = expr_violation(cx, el, elem, depth + 1) {
                    return Some(f);
                }
            }
            None
        }
        Node::ObjectLit { props } => {
            for &p in cx.ast.list(props).to_vec().iter() {
                if let Some(f) = member_violation(cx, p, target, depth) {
                    return Some(f);
                }
            }
            None
        }
        Node::Conditional { cons, alt, .. } => expr_violation(cx, cons, target, depth + 1)
            .or_else(|| expr_violation(cx, alt, target, depth + 1)),
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } => expr_violation(cx, left, target, depth + 1)
            .or_else(|| expr_violation(cx, right, target, depth + 1)),
        _ => {
            let source = cx.ty(e);
            if source.is_any_like() {
                return None;
            }
            let v = violation(cx, source, target, depth, &mut FxHashSet::default())?;
            Some(Found {
                v,
                source,
                target,
                expr: e,
                span: None,
            })
        }
    }
}

/// One member of an object literal against the target's property of the same name.
fn member_violation(
    cx: &mut Ctx<'_, '_, '_>,
    p: NodeId,
    target: TypeId,
    depth: u32,
) -> Option<Found> {
    let (key, value, is_method) = match cx.node(p) {
        Node::Property { key, value, .. } => (key, value, false),
        Node::ShorthandProp { name, .. } => (name, name, false),
        Node::MethodDef {
            key,
            value,
            kind: MethodKind::Method,
            ..
        } => (key, value, true),
        Node::Spread { arg } => {
            let source = cx.ty(arg);
            if source.is_any_like() {
                return None;
            }
            let v = violation(cx, source, target, depth + 1, &mut FxHashSet::default())?;
            return Some(Found {
                v,
                source,
                target,
                expr: arg,
                span: None,
            });
        }
        _ => return None,
    };
    let name = cx.key_text(key)?;
    let tp = cx.ck.quietly(|ck| ck.property_of_type(target, &name))?;
    let value_inner = cx.skip_parens(value);
    let fn_value = is_method
        || matches!(
            cx.node(value_inner),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        );
    if fn_value {
        if !sound_method(cx, tp) {
            return None;
        }
        let st = cx.ty(value);
        let v = method_violation(cx, &name, st, tp, None)?;
        return Some(Found {
            v,
            source: st,
            target,
            expr: value,
            span: Some(cx.span(key)),
        });
    }
    let tt = cx.ck.quietly(|ck| ck.type_of_symbol(tp));
    expr_violation(cx, value, tt, depth + 1)
}

fn report_violation(
    cx: &mut Ctx<'_, '_, '_>,
    v: Violation,
    source: TypeId,
    target: TypeId,
    source_expr: NodeId,
    at: Span,
) {
    match v {
        Violation::Container { target: c } => {
            let s = cx.type_str(source);
            let t = cx.type_str(target);
            let m = cx.type_str(c);
            let ro = readonly_form(&m);
            cx.report(codes::INVARIANT_CONTAINER, at, &[&s, &t, &m, &ro]);
        }
        Violation::Method {
            name,
            param,
            source_param,
            target_param,
            source_prop,
        } => {
            // An object literal member is reported at its name.
            let mut span = at;
            if let Some(p) = source_prop {
                let expr_span = cx.span(source_expr);
                for d in cx.ck.c.decls(p) {
                    if d.file == cx.file {
                        let ds = cx.span(d.node);
                        if ds.start >= expr_span.start && ds.end <= expr_span.end {
                            span = member_name_span(cx, d.node).unwrap_or(ds);
                        }
                    }
                }
            }
            let sp = cx.type_str(source_param);
            let tp = cx.type_str(target_param);
            cx.report(codes::METHOD_PARAMETER, span, &[&name, &param, &sp, &tp]);
        }
    }
}

fn member_name_span(cx: &Ctx<'_, '_, '_>, d: NodeId) -> Option<Span> {
    match cx.node(d) {
        Node::MethodDef { key, .. } | Node::Property { key, .. } => Some(cx.span(key)),
        _ => None,
    }
}

/// `readonly Animal[]`, `ReadonlyMap<K, V>`, ... for a mutable container's printed type.
fn readonly_form(m: &str) -> String {
    for (from, to) in [
        ("Map<", "ReadonlyMap<"),
        ("Set<", "ReadonlySet<"),
        ("Array<", "ReadonlyArray<"),
    ] {
        if let Some(rest) = m.strip_prefix(from) {
            return format!("{to}{rest}");
        }
    }
    if m.ends_with("[]") || m.starts_with('[') {
        return format!("readonly {m}");
    }
    m.to_owned()
}

/// S-06 for class members: each own method against the same-named method of the base class
/// and of every implemented interface.
fn heritage_methods(
    cx: &mut Ctx<'_, '_, '_>,
    class: NodeId,
    heritage: &[NodeId],
    members: &[NodeId],
) {
    if heritage.is_empty() {
        return;
    }
    let mut bases: Vec<TypeId> = Vec::new();
    if let Some(sym) = cx.ck.c.sym_of_decl(cx.loc(class)) {
        bases.extend(cx.ck.quietly(|ck| ck.base_types(sym)).iter().copied());
    }
    for &h in heritage {
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
    if bases.is_empty() {
        return;
    }
    for &m in members {
        let Node::MethodDef {
            kind: MethodKind::Method,
            key,
            value,
            ..
        } = cx.node(m)
        else {
            continue;
        };
        let Some(name) = cx.key_text(key) else {
            continue;
        };
        let own = cx.ty(value);
        for &b in &bases {
            let Some(bp) = cx.ck.quietly(|ck| ck.property_of_type(b, &name)) else {
                continue;
            };
            if let Some(v) = method_violation(cx, &name, own, bp, None) {
                report_violation(cx, v, own, b, value, cx.span(key));
                break;
            }
        }
    }
}

fn is_lib_sym(cx: &Ctx<'_, '_, '_>, sym: SymId) -> bool {
    cx.ck
        .c
        .decls(sym)
        .iter()
        .any(|d| cx.info.is_lib(d.file) || !cx.info.is_sound_file(d.file))
}

/// Whether property `p` is a method declared only in sound files (S-06 applies).
fn sound_method(cx: &Ctx<'_, '_, '_>, p: SymId) -> bool {
    if !cx.ck.c.sym_flags(p).contains(SymbolFlags::METHOD) {
        return false;
    }
    let decls = cx.ck.c.decls(p);
    !decls.is_empty() && decls.iter().all(|d| cx.info.is_sound_file(d.file))
}

/// Compares `source` (the type of the source member) with target method `tp`.
fn method_violation(
    cx: &mut Ctx<'_, '_, '_>,
    name: &str,
    source: TypeId,
    tp: SymId,
    source_prop: Option<SymId>,
) -> Option<Violation> {
    if !sound_method(cx, tp) {
        return None;
    }
    let tt = cx.ck.quietly(|ck| ck.type_of_symbol(tp));
    let ssigs = cx.ck.quietly(|ck| ck.signatures_of(source, SigKind::Call));
    let tsigs = cx.ck.quietly(|ck| ck.signatures_of(tt, SigKind::Call));
    let ([ss], [ts]) = (ssigs.as_slice(), tsigs.as_slice()) else {
        return None;
    };
    let store = cx.ck.store();
    if !store.list_items(store.sig(*ss).type_params).is_empty()
        || !store.list_items(store.sig(*ts).type_params).is_empty()
    {
        return None;
    }
    let (sn, _) = cx.ck.param_count(*ss);
    let (tn, _) = cx.ck.param_count(*ts);
    for i in 0..sn.min(tn) {
        let sp = cx.ck.quietly(|ck| ck.param_type_at(*ss, i))?;
        let tpt = cx.ck.quietly(|ck| ck.param_type_at(*ts, i))?;
        if sp.is_any_like() || tpt.is_any_like() {
            continue;
        }
        if !cx.is_assignable(tpt, sp) {
            let store = cx.ck.store();
            let param = store
                .sig(*ss)
                .params
                .get(i)
                .map(|p| store.str(p.name).to_owned())
                .unwrap_or_default();
            return Some(Violation::Method {
                name: name.to_owned(),
                param,
                source_param: sp,
                target_param: tpt,
                source_prop,
            });
        }
    }
    None
}

/// The generic declaration and type arguments of a reference type.
pub(crate) fn reference_parts(cx: &Ctx<'_, '_, '_>, t: TypeId) -> Option<(TypeId, Vec<TypeId>)> {
    let store = cx.ck.store();
    match store.object_of(t)?.kind {
        ObjectKind::Reference { target, args } => Some((target, store.list_items(args).to_vec())),
        _ => None,
    }
}

/// Number of real type arguments of a generic target (without the trailing `this`).
fn arity(cx: &Ctx<'_, '_, '_>, target: TypeId, args: usize) -> usize {
    let store = cx.ck.store();
    match store.object_of(target).map(|o| o.kind) {
        Some(ObjectKind::Declared { type_params, .. }) => {
            store.list_items(type_params).len().min(args)
        }
        _ => args,
    }
}

/// The declaring symbol of a declared generic target (`Array`, `Map`, a user interface).
fn declared_sym(cx: &Ctx<'_, '_, '_>, target: TypeId) -> Option<SymId> {
    match cx.ck.store().object_of(target)?.kind {
        ObjectKind::Declared { sym, .. } => Some(sym),
        _ => None,
    }
}

/// Mutable built-in containers: `Array`, mutable tuples, `Map`, `Set`, `WeakMap`, `WeakSet`.
fn is_mutable_container(cx: &mut Ctx<'_, '_, '_>, target: TypeId) -> bool {
    if cx.ck.global_type(GlobalType::Array) == Some(target) {
        return true;
    }
    let store = cx.ck.store();
    if let Some(ObjectKind::TupleTarget { shape }) = store.object_of(target).map(|o| o.kind) {
        return !store.tuple_shape(shape).readonly;
    }
    let Some(sym) = declared_sym(cx, target) else {
        return false;
    };
    matches!(cx.ck.c.sym_name(sym), "Map" | "Set" | "WeakMap" | "WeakSet") && is_lib_sym(cx, sym)
}

/// Whether target type `t` is declared in a lib, `.d.ts` or non-sound file (its members are
/// not walked; containers aside).
fn is_foreign(cx: &Ctx<'_, '_, '_>, t: TypeId) -> bool {
    let store = cx.ck.store();
    let target = match store.object_of(t).map(|o| o.kind) {
        Some(ObjectKind::Reference { target, .. }) => target,
        Some(_) => t,
        None => return false,
    };
    declared_sym(cx, target).is_some_and(|s| is_lib_sym(cx, s))
}

/// The first place where `source` (already assignable to `target` under tsc's rules) is not
/// assignable under sound relations.
pub fn violation(
    cx: &mut Ctx<'_, '_, '_>,
    source: TypeId,
    target: TypeId,
    depth: u32,
    seen: &mut FxHashSet<(TypeId, TypeId)>,
) -> Option<Violation> {
    if depth > MAX_DEPTH || source == target || !seen.insert((source, target)) {
        return None;
    }
    if source.is_any_like() || target.is_any_like() {
        return None;
    }
    let sf = cx.flags(source);
    let tf = cx.flags(target);
    if sf.intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::NEVER)
        || tf.intersects(TypeFlags::ANY_OR_UNKNOWN)
    {
        return None;
    }
    if sf.contains(TypeFlags::UNION) {
        let members = cx.ck.store().members_of_union(source).to_vec();
        for m in members {
            if let Some(v) = violation(cx, m, target, depth + 1, seen) {
                return Some(v);
            }
        }
        return None;
    }
    if tf.contains(TypeFlags::UNION) {
        let members = cx.ck.store().members_of_union(target).to_vec();
        let mut first = None;
        for m in members {
            if !cx.is_assignable(source, m) {
                continue;
            }
            match violation(cx, source, m, depth + 1, seen) {
                None => return None,
                Some(v) => {
                    if first.is_none() {
                        first = Some(v);
                    }
                }
            }
        }
        return first;
    }
    if tf.contains(TypeFlags::INTERSECTION) {
        let members = cx.ck.store().members_of_union(target).to_vec();
        for m in members {
            if let Some(v) = violation(cx, source, m, depth + 1, seen) {
                return Some(v);
            }
        }
        return None;
    }
    if !sf.contains(TypeFlags::OBJECT) || !tf.contains(TypeFlags::OBJECT) {
        return None;
    }
    let sr = reference_parts(cx, source);
    let tr = reference_parts(cx, target);
    if let Some((tt, targs)) = &tr {
        let mutable = is_mutable_container(cx, *tt);
        match &sr {
            Some((st, sargs)) if st == tt => {
                let n = arity(cx, *tt, sargs.len().min(targs.len()));
                for i in 0..n {
                    let (a, b) = (sargs[i], targs[i]);
                    // `never[]` (an empty array literal) fits any element type.
                    let empty = cx.flags(a).contains(TypeFlags::NEVER);
                    if mutable && a != b && !empty && !(a.is_any_like() || b.is_any_like()) {
                        let same = cx.is_assignable(a, b) && cx.is_assignable(b, a);
                        if !same {
                            return Some(Violation::Container { target });
                        }
                    }
                    if let Some(v) = violation(cx, a, b, depth + 1, seen) {
                        return Some(v);
                    }
                }
                if is_foreign(cx, target) || mutable {
                    return None;
                }
            }
            Some((st, sargs)) if mutable => {
                // A tuple (or another container) to a mutable array: every element must be
                // identical to the array's element type.
                let tuple_like = matches!(
                    cx.ck.store().object_of(*st).map(|o| o.kind),
                    Some(ObjectKind::TupleTarget { .. })
                );
                if tuple_like && cx.ck.global_type(GlobalType::Array) == Some(*tt) {
                    let elem = targs.first().copied().unwrap_or(TypeId::UNKNOWN);
                    for &a in sargs.iter() {
                        if a != elem && !(cx.is_assignable(a, elem) && cx.is_assignable(elem, a)) {
                            return Some(Violation::Container { target });
                        }
                    }
                }
                return None;
            }
            _ => {
                if mutable || is_foreign(cx, target) {
                    return None;
                }
            }
        }
    } else if is_foreign(cx, target) {
        return None;
    }
    // Structural: same-named properties.
    let props = cx.ck.quietly(|ck| ck.properties_of(target));
    for (name_id, tp) in props.into_iter().take(MAX_PROPS) {
        let name = cx.ck.store().str(name_id).to_owned();
        let Some(sp) = cx.ck.quietly(|ck| ck.property_of_type(source, &name)) else {
            continue;
        };
        let st = cx.ck.quietly(|ck| ck.type_of_symbol(sp));
        if sound_method(cx, tp) {
            if let Some(v) = method_violation(cx, &name, st, tp, Some(sp)) {
                return Some(v);
            }
            continue;
        }
        let tt = cx.ck.quietly(|ck| ck.type_of_symbol(tp));
        if let Some(v) = violation(cx, st, tt, depth + 1, seen) {
            return Some(v);
        }
    }
    None
}
