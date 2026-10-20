//! Values sound mode sees as `unknown`: `any` declared outside the sound set (S-01 TSR0102) and
//! data from external sources (S-14 TSR1401), plus the `@ts-r.dev/guard` call rules (TSR1402,
//! TSR1501 to TSR1503).
//!
//! The checker still types these values `any`; the design's tagged `unknown` intrinsics are not
//! in the checker yet (0004 change). This module finds the uses that would be errors on
//! `unknown` ("claims": member access, calls, typed targets, destructuring, spread, type
//! assertions) and traces the `any` back to its origin. A claim is reported only at the
//! expression where the `any` enters (not again on `a.b.c` when `a` is reported), and only when
//! the origin is outside the sound set: written `any` in a sound or strict file is TSR0101
//! already, and `loose` packages keep `any` (S-11). A typed target is also a claim on a foreign
//! `any` below the top of the value's type (`{ id: any }`, `any[]`), as the mapping is deep.

use rustc_hash::FxHashSet;
use ts_r_ast::{Node, NodeId};
use ts_r_binder::meaning;
use ts_r_checker::checker::Loc;
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::Ctx;

/// Where an `any` came from.
enum Origin {
    /// An S-14 external source (`JSON.parse`, `Response.json`, `MessageEvent.data`, ...).
    External(String),
    /// A declaration outside the sound set that says `any`.
    Foreign(String),
}

const MAX_TRACE: u32 = 16;
/// Type nesting walked for a foreign `any` below the top level, and properties per level.
const MAX_NESTED: u32 = 4;
const MAX_PROPS: usize = 256;

/// `@ts-r.dev/guard` functions that need a type argument.
const TYPED_HELPERS: &[&str] = &[
    "parse",
    "safeParse",
    "is",
    "assert",
    "parseJson",
    "fetchJson",
    "env",
    "guard",
    "body",
    "query",
    "stored",
    "readJson",
    "args",
];

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    let s01 = cx.has_rule(1);
    let s14 = cx.has_rule(14);
    if s01 || s14 {
        claims(cx);
    }
    if s14 {
        guard_calls(cx);
    }
}

fn claims(cx: &mut Ctx<'_, '_, '_>) {
    // (expression, report span node, target type when a site)
    let mut uses: Vec<(NodeId, NodeId)> = Vec::new();
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        match cx.node(n) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => uses.push((obj, obj)),
            Node::Call { callee, .. } | Node::New { callee, .. } => {
                uses.push((callee, callee));
            }
            Node::TaggedTemplate { tag, .. } => uses.push((tag, tag)),
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                if cx.src(ty) == "const" {
                    continue;
                }
                let t = cx.type_node(ty);
                if !cx.flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) && !t.is_any_like() {
                    uses.push((expr, n));
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(i),
                ..
            } if matches!(
                cx.node(name),
                Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
            ) =>
            {
                uses.push((i, i));
            }
            Node::Spread { arg } => uses.push((arg, arg)),
            _ => {}
        }
    }
    // Typed targets also get values whose `any` is below the top (`{ id: any }`).
    let mut nested: Vec<(NodeId, TypeId)> = Vec::new();
    for (e, site) in cx.sites() {
        if accepts_unknown(cx, site.target) {
            continue;
        }
        uses.push((e, e));
        nested.push((e, site.target));
    }
    uses.sort_unstable();
    uses.dedup();
    for (e, at) in uses {
        claim(cx, e, at);
    }
    if cx.has_rule(1) {
        for (e, target) in nested {
            nested_claim(cx, e, target);
        }
    }
}

fn accepts_unknown(cx: &mut Ctx<'_, '_, '_>, t: TypeId) -> bool {
    cx.flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN)
        || t.is_any_like()
        || cx.is_assignable(TypeId::UNKNOWN, t)
}

/// A value for typed target `target` whose type holds a foreign `any` below the top level
/// (`{ id: any }` or `any[]` from a `.d.ts`, for `{ id: number }` or `number[]`): S-01 sees
/// that `any` as `unknown` too (the mapping is deep), so the value does not fit the target.
fn nested_claim(cx: &mut Ctx<'_, '_, '_>, e: NodeId, target: TypeId) {
    let e = cx.skip_parens(e);
    let source = cx.ty(e);
    if source.is_any_like() || !cx.is_assignable(source, target) {
        return; // a bare `any` is `claim`'s; tsc reports the rest.
    }
    let owner = foreign_owner(cx, e, 0);
    let mut seen = FxHashSet::default();
    let Some((path, src)) = nested_any(cx, source, target, owner.as_deref(), 0, &mut seen) else {
        return;
    };
    let text = format!("{}{path}", cx.short(e));
    let span = cx.span(e);
    cx.report(codes::ANY_IS_UNKNOWN, span, &[&text, &src]);
}

/// The name of what declared the type of `e` outside the sound set: a call's function, a
/// property, or an imported value.
fn foreign_owner(cx: &mut Ctx<'_, '_, '_>, e: NodeId, depth: u32) -> Option<String> {
    let e = cx.skip_parens(e);
    match cx.node(e) {
        Node::Ident { .. } => {
            let d = value_decl(cx, e)?;
            (!cx.info.in_sound_set(d.file)).then(|| cx.short(e))
        }
        _ => match container_init_origin(cx, e, depth)? {
            Origin::External(s) | Origin::Foreign(s) => Some(s),
        },
    }
}

/// Where `source` (assignable to `target`) has an `any` declared outside the sound set at a
/// position where `target` needs more than `unknown`: the member path (`.id`, empty for a type
/// argument) and the declaring name. `owner` declared `source` outside the sound set.
fn nested_any(
    cx: &mut Ctx<'_, '_, '_>,
    source: TypeId,
    target: TypeId,
    owner: Option<&str>,
    depth: u32,
    seen: &mut FxHashSet<(TypeId, TypeId)>,
) -> Option<(String, String)> {
    if depth > MAX_NESTED || source == target || !seen.insert((source, target)) {
        return None;
    }
    if source.is_any_like() || accepts_unknown(cx, target) {
        return None;
    }
    let (sf, tf) = (cx.flags(source), cx.flags(target));
    if sf.contains(TypeFlags::UNION) {
        let members = cx.ck.store().members_of_union(source).to_vec();
        for m in members {
            if let Some(f) = nested_any(cx, m, target, owner, depth + 1, seen) {
                return Some(f);
            }
        }
        return None;
    }
    if tf.contains(TypeFlags::UNION) {
        // A finding only if every member the source fits gives one.
        let members = cx.ck.store().members_of_union(target).to_vec();
        let mut first = None;
        for m in members {
            if !cx.is_assignable(source, m) {
                continue;
            }
            match nested_any(cx, source, m, owner, depth + 1, seen) {
                None => return None,
                Some(f) => {
                    first.get_or_insert(f);
                }
            }
        }
        return first;
    }
    if !sf.contains(TypeFlags::OBJECT) || !tf.contains(TypeFlags::OBJECT) {
        return None;
    }
    // Type arguments of the same generic: `any[]` for `number[]`, `Promise<any>`.
    if let (Some((st, sargs)), Some((tt, targs))) = (
        super::relations::reference_parts(cx, source),
        super::relations::reference_parts(cx, target),
    ) && st == tt
    {
        for (&a, &b) in sargs.iter().zip(&targs) {
            if a == TypeId::ANY {
                if let Some(o) = owner
                    && !accepts_unknown(cx, b)
                {
                    return Some((String::new(), o.to_owned()));
                }
                continue;
            }
            if let Some(f) = nested_any(cx, a, b, owner, depth + 1, seen) {
                return Some(f);
            }
        }
        return None;
    }
    let props = cx.ck.quietly(|ck| ck.properties_of(target));
    for (name_id, tp) in props.into_iter().take(MAX_PROPS) {
        let name = cx.ck.store().str(name_id).to_owned();
        let Some(sp) = cx.ck.quietly(|ck| ck.property_of_type(source, &name)) else {
            continue;
        };
        if !all_decls_outside_sound_set(cx, sp) {
            continue; // written in the sound set: TSR0101 there
        }
        let st = cx.ck.quietly(|ck| ck.type_of_symbol(sp));
        let tt = cx.ck.quietly(|ck| ck.type_of_symbol(tp));
        let declared_by = owner.map_or_else(|| member_name(cx, sp), str::to_owned);
        if st == TypeId::ANY {
            if !accepts_unknown(cx, tt) {
                return Some((format!(".{name}"), declared_by));
            }
            continue;
        }
        if let Some((path, src)) = nested_any(cx, st, tt, Some(&declared_by), depth + 1, seen) {
            return Some((format!(".{name}{path}"), src));
        }
    }
    None
}

fn claim(cx: &mut Ctx<'_, '_, '_>, e: NodeId, at: NodeId) {
    let e = cx.skip_parens(e);
    if cx.ty(e) != TypeId::ANY {
        return;
    }
    // Report where the `any` enters, not on everything built from it.
    match cx.node(e) {
        Node::Member { obj, .. } | Node::Index { obj, .. } if cx.ty(obj) == TypeId::ANY => return,
        Node::Call { callee, .. } if cx.ty(callee) == TypeId::ANY => return,
        _ => {}
    }
    let Some(origin) = origin(cx, e, 0) else {
        return;
    };
    let text = cx.short(e);
    let span = cx.span(at);
    match origin {
        Origin::External(src) if cx.has_rule(14) => {
            cx.report(codes::EXTERNAL_VALUE, span, &[&src, &text]);
        }
        Origin::External(src) | Origin::Foreign(src) => {
            cx.report(codes::ANY_IS_UNKNOWN, span, &[&text, &src]);
        }
    }
}

/// `Parent.name` of a property symbol (`JSON.parse`, `Body.json`, `MessageEvent.data`).
fn qualified(cx: &Ctx<'_, '_, '_>, sym: SymId) -> String {
    let name = cx.ck.c.sym_name(sym).to_owned();
    match cx.ck.c.sym_parent(sym) {
        Some(p) => format!("{}.{name}", cx.ck.c.sym_name(p)),
        None => name,
    }
}

/// `Parent.name` of a property, or `name` when the parent is a type literal.
fn member_name(cx: &Ctx<'_, '_, '_>, sym: SymId) -> String {
    match cx.ck.c.sym_parent(sym) {
        Some(p) if !cx.ck.c.sym_name(p).starts_with("__") => qualified(cx, sym),
        _ => cx.ck.c.sym_name(sym).to_owned(),
    }
}

fn all_decls_outside_sound_set(cx: &Ctx<'_, '_, '_>, sym: SymId) -> bool {
    let decls = cx.ck.c.decls(sym);
    !decls.is_empty() && decls.iter().all(|d| !cx.info.in_sound_set(d.file))
}

/// Whether a lib symbol named `q` is an external source.
fn external_source(cx: &Ctx<'_, '_, '_>, sym: SymId, q: &str, call: bool) -> bool {
    let lib = cx.ck.c.decls(sym).iter().any(|d| cx.info.is_lib(d.file));
    if lib {
        let builtin = if call {
            matches!(
                q,
                "JSON.parse" | "Body.json" | "Response.json" | "Request.json"
            )
        } else {
            q == "MessageEvent.data"
        };
        if builtin {
            return true;
        }
    }
    // `[sound.sources]`: package name to `Parent.member` paths.
    cx.info.config.sources.iter().any(|(pkg, paths)| {
        paths.iter().any(|p| p == q)
            && cx.ck.c.decls(sym).iter().any(|d| {
                let path = &cx.ck.c.file(d.file).path;
                path.contains(&format!("/node_modules/{pkg}/"))
            })
    })
}

fn origin(cx: &mut Ctx<'_, '_, '_>, e: NodeId, depth: u32) -> Option<Origin> {
    if depth > MAX_TRACE {
        return None;
    }
    let e = cx.skip_parens(e);
    match cx.node(e) {
        Node::Await { arg } => origin(cx, arg, depth + 1),
        Node::Call { callee, .. } => {
            let callee_inner = cx.skip_parens(callee);
            if matches!(cx.node(callee_inner), Node::Member { .. }) {
                let l = cx.loc(callee_inner);
                if let Some(sym) = cx.ck.quietly(|ck| ck.property_symbol_of_access(l)) {
                    let q = qualified(cx, sym);
                    if external_source(cx, sym, &q, true) {
                        return Some(Origin::External(q));
                    }
                }
            }
            if cx.ty(callee).is_any_like() {
                return None;
            }
            let sig = cx.ck.resolved_signature(cx.loc(e))?;
            let d = cx.sig_decl(sig)?;
            if cx.info.in_sound_set(d.file) {
                return None;
            }
            Some(Origin::Foreign(cx.short(callee)))
        }
        Node::Member { obj, .. } => {
            if cx.ty(obj) == TypeId::ANY {
                return origin(cx, obj, depth + 1);
            }
            let l = cx.loc(e);
            let sym = cx.ck.quietly(|ck| ck.property_symbol_of_access(l))?;
            let q = qualified(cx, sym);
            if external_source(cx, sym, &q, false) {
                return Some(Origin::External(q));
            }
            all_decls_outside_sound_set(cx, sym).then_some(Origin::Foreign(q))
        }
        Node::Index { obj, index, .. } => {
            if cx.ty(obj) == TypeId::ANY {
                return origin(cx, obj, depth + 1);
            }
            if cx.ty(index).is_any_like() {
                // `any` from an `any` key (an implicit `any` the checker already reports),
                // not from the container.
                return None;
            }
            container_origin(cx, obj, depth + 1)
        }
        Node::Ident { .. } => ident_origin(cx, e, depth),
        _ => None,
    }
}

/// The origin of the `any` elements of a container expression (`xs` in `xs[0]`, `for of xs`).
fn container_origin(cx: &mut Ctx<'_, '_, '_>, obj: NodeId, depth: u32) -> Option<Origin> {
    let obj = cx.skip_parens(obj);
    match cx.node(obj) {
        Node::Ident { .. } => {
            let decl = value_decl(cx, obj)?;
            if decl.file == cx.file && has_written_any(cx, decl.node) {
                return None;
            }
            if decl.file != cx.file && cx.info.in_sound_set(decl.file) {
                return None;
            }
            // A sound-set variable whose type has `any` elements it did not write: a lib
            // narrowing (`Array.isArray`) or an `any[]` from a foreign call.
            match cx.node(decl.node) {
                Node::VarDeclarator {
                    type_ann: None,
                    init: Some(i),
                    ..
                } if decl.file == cx.file => container_init_origin(cx, i, depth),
                _ => Some(Origin::Foreign(narrowing_name(cx, obj))),
            }
        }
        Node::Call { .. } | Node::Member { .. } | Node::Await { .. } => {
            container_init_origin(cx, obj, depth)
        }
        _ => None,
    }
}

fn container_init_origin(cx: &mut Ctx<'_, '_, '_>, init: NodeId, depth: u32) -> Option<Origin> {
    let init = cx.skip_parens(init);
    match cx.node(init) {
        Node::Call { callee, .. } => {
            let sig = cx.ck.resolved_signature(cx.loc(init))?;
            let d = cx.sig_decl(sig)?;
            (!cx.info.in_sound_set(d.file)).then(|| Origin::Foreign(cx.short(callee)))
        }
        Node::Await { arg } if depth < MAX_TRACE => container_init_origin(cx, arg, depth + 1),
        Node::Member { .. } => {
            let l = cx.loc(init);
            let sym = cx.ck.quietly(|ck| ck.property_symbol_of_access(l))?;
            all_decls_outside_sound_set(cx, sym).then(|| Origin::Foreign(qualified(cx, sym)))
        }
        _ => None,
    }
}

/// Names the lib narrowing that produced an `any` (`Array.isArray`), else the expression.
fn narrowing_name(cx: &Ctx<'_, '_, '_>, n: NodeId) -> String {
    // The only lib narrowing that produces `any` elements today is `Array.isArray`.
    let _ = (cx, n);
    "Array.isArray".to_owned()
}

/// The value declaration an identifier resolves to (through imports).
fn value_decl(cx: &Ctx<'_, '_, '_>, ident: NodeId) -> Option<Loc> {
    let sym = cx.ck.c.resolve_ident(cx.loc(ident), meaning::VALUE)?;
    let sym = cx.ck.c.resolve_alias(sym)?;
    cx.ck.c.value_decl(sym)
}

/// Whether the type annotation of declaration `d` (in this file) writes `any`.
fn has_written_any(cx: &Ctx<'_, '_, '_>, d: NodeId) -> bool {
    let ann = match cx.node(d) {
        Node::VarDeclarator { type_ann, .. } | Node::Param { type_ann, .. } => type_ann,
        Node::PropertyDef { type_ann, .. } => type_ann,
        _ => None,
    };
    let Some(ann) = ann else {
        return false;
    };
    let mut found = false;
    let mut stack = vec![ann];
    while let Some(n) = stack.pop() {
        if matches!(
            cx.node(n),
            Node::KeywordType {
                kind: ts_r_ast::KeywordTypeKind::Any
            }
        ) {
            found = true;
            break;
        }
        cx.ast.for_each_child(n, |c| stack.push(c));
    }
    found
}

/// Whether identifier `e` is imported from a `loose` module (whose values stay `any`).
fn from_loose_module(cx: &Ctx<'_, '_, '_>, e: NodeId) -> bool {
    let Some(sym) = cx.ck.c.resolve_ident(cx.loc(e), meaning::VALUE) else {
        return false;
    };
    for d in cx.ck.c.decls(sym) {
        if d.file != cx.file {
            continue;
        }
        let mut cur = Some(d.node);
        for _ in 0..4 {
            let Some(n) = cur else { break };
            if let Node::ImportDecl { source, .. } = cx.node(n)
                && let Node::StringLit { value } = cx.node(source)
            {
                return cx.info.loose_modules.contains(cx.ast.atom(value));
            }
            cur = cx.parent(n);
        }
    }
    false
}

fn ident_origin(cx: &mut Ctx<'_, '_, '_>, e: NodeId, depth: u32) -> Option<Origin> {
    if from_loose_module(cx, e) {
        return None;
    }
    let decl = value_decl(cx, e)?;
    if decl.file != cx.file {
        // Imported: `any` declared in a file outside the sound set (`.d.ts`, trusted
        // package, non-sound local file).
        if cx.info.in_sound_set(decl.file) {
            return None;
        }
        let name = cx.src(e).to_owned();
        return Some(Origin::Foreign(name));
    }
    match cx.node(decl.node) {
        Node::VarDeclarator {
            type_ann: Some(_), ..
        } => None,
        Node::VarDeclarator {
            type_ann: None,
            init: Some(i),
            ..
        } => origin(cx, i, depth + 1),
        Node::VarDeclarator {
            type_ann: None,
            init: None,
            ..
        } => {
            // `for (const x of xs)`: the element of `xs`.
            let mut cur = cx.parent(decl.node);
            for _ in 0..3 {
                let p = cur?;
                if let Node::ForOf { right, .. } = cx.node(p) {
                    return container_origin(cx, right, depth + 1);
                }
                cur = cx.parent(p);
            }
            None
        }
        Node::Ident { .. } => {
            // A binding name: its parameter (or declarator) is the parent.
            let p = cx.parent(decl.node)?;
            match cx.node(p) {
                Node::Param { type_ann: None, .. } => param_origin(cx, p),
                Node::VarDeclarator { .. } if p != decl.node => match cx.node(p) {
                    Node::VarDeclarator {
                        type_ann: None,
                        init: Some(i),
                        name,
                        ..
                    } if name == decl.node => origin(cx, i, depth + 1),
                    _ => None,
                },
                _ => None,
            }
        }
        Node::Param { type_ann: None, .. } => param_origin(cx, decl.node),
        _ => None,
    }
}

/// An unannotated parameter: `any` from the contextual signature, if that signature is
/// declared outside the sound set (`p.catch((e) => ...)`).
fn param_origin(cx: &mut Ctx<'_, '_, '_>, param: NodeId) -> Option<Origin> {
    let f = cx.parent(param)?;
    let fl = cx.loc(f);
    let sig = cx.ck.quietly(|ck| ck.contextual_signature(fl))?;
    let d = cx.sig_decl(sig)?;
    if cx.info.in_sound_set(d.file) {
        return None;
    }
    // Name the callee the function is passed to, when there is one.
    let mut name = String::from("its contextual signature");
    if let Some((p, _)) = cx.parent_skip_parens(f)
        && let Node::Call { callee, .. } = cx.node(p)
    {
        name = cx.short(callee);
    }
    Some(Origin::Foreign(name))
}

/// The `@ts-r.dev/guard` export a reference resolves to.
fn guard_export(cx: &mut Ctx<'_, '_, '_>, r: NodeId) -> Option<String> {
    let sym = match cx.node(r) {
        Node::Ident { .. } => {
            let s = cx.ck.c.resolve_ident(cx.loc(r), meaning::VALUE)?;
            cx.ck.c.resolve_alias(s)?
        }
        Node::Member { .. } => {
            let l = cx.loc(r);
            cx.ck.quietly(|ck| ck.property_symbol_of_access(l))?
        }
        _ => return None,
    };
    let decls = cx.ck.c.decls(sym);
    let d = decls.first()?;
    // Module-level functions only (not `Guard<T>.parse`).
    if !matches!(cx.ck.c.node(*d), Node::FunctionDecl { .. }) {
        return None;
    }
    let file = cx.ck.c.file(d.file);
    let in_pkg = file.path.contains("/node_modules/@ts-r.dev/guard/") || {
        let ast = cx.ck.c.ast(d.file);
        let parents = file.parents();
        let mut cur = parents.parent(d.node);
        let mut found = false;
        for _ in 0..8 {
            let Some(p) = cur else { break };
            if let Node::Module { name, .. } = *ast.node(p)
                && let Node::StringLit { value } = *ast.node(name)
                && ast.atom(value) == "@ts-r.dev/guard"
            {
                found = true;
                break;
            }
            cur = parents.parent(p);
        }
        found
    };
    in_pkg.then(|| cx.ck.c.sym_name(sym).to_owned())
}

fn guard_calls(cx: &mut Ctx<'_, '_, '_>) {
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        if !matches!(cx.node(n), Node::Ident { .. } | Node::Member { .. }) {
            continue;
        }
        let Some((p, child)) = cx.parent_skip_parens(n) else {
            continue;
        };
        // Skip names inside import/export declarations and the property name of a member.
        match cx.node(p) {
            Node::Member { prop, .. } if prop == child => continue,
            Node::ImportSpecifier { .. }
            | Node::ImportDecl { .. }
            | Node::ExportSpecifier { .. }
            | Node::ImportNamespaceSpecifier { .. }
            | Node::ImportDefaultSpecifier { .. }
            | Node::ExportNamed { .. } => continue,
            _ => {}
        }
        if let Node::Ident { .. } = cx.node(n)
            && matches!(cx.node(p), Node::Member { obj, .. } if obj == child)
            && guard_export(cx, n).is_none()
        {
            continue;
        }
        let Some(name) = guard_export(cx, n) else {
            continue;
        };
        if !TYPED_HELPERS.contains(&name.as_str()) {
            continue;
        }
        let text = cx.src(n).to_owned();
        match cx.node(p) {
            Node::Call {
                callee, type_args, ..
            } if callee == child => match type_args {
                None => cx.report(codes::GUARD_NEEDS_TYPE_ARGUMENT, cx.span(p), &[&text]),
                Some(ta) => type_argument(cx, &text, ta),
            },
            Node::Instantiation { expr, type_args } if expr == child => {
                type_argument(cx, &text, type_args)
            }
            _ => cx.report(codes::GUARD_AS_VALUE, cx.span(n), &[&text]),
        }
    }
}

/// TSR1502 / TSR1501 for the type argument of a guard call.
fn type_argument(cx: &mut Ctx<'_, '_, '_>, callee: &str, type_args: NodeId) {
    let first = match cx.node(type_args) {
        Node::TypeArgs { args } => cx.ast.list(args).first().copied(),
        _ => None,
    };
    let Some(arg) = first else {
        return;
    };
    let t = cx.type_node(arg);
    if cx.flags(t).contains(TypeFlags::TYPE_PARAMETER) {
        let name = cx.src(arg).to_owned();
        cx.report(codes::GUARD_TYPE_PARAMETER, cx.span(arg), &[callee, &name]);
        return;
    }
    // The same evaluator `parse<T>` code generation uses (declarations only, record P4),
    // with the configured refinements, so check and build agree on what is supported.
    let mut g = ts_r_guard::Generator::new(cx.info.program, cx.info.config.guard.options());
    if let Err(u) = g.add(cx.file.0 as usize, arg) {
        let s = cx.src(arg).to_owned();
        cx.report(codes::NOT_VALIDATABLE, cx.span(arg), &[&s, &u.reason]);
    }
}
