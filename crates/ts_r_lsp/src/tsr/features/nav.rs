//! Definition, type definition, references, document highlights and rename.

use super::keywords::{self, Doc, Special};
use super::locate::{
    Meaning, ident_at, key_text, literal_key, literal_key_span, literal_keys_in, meaning_at,
    name_text, names_in, node_at,
};
use super::rename_alias::Decor;
use super::{Ctx, LinesCache, LspLines, decl_span};
use crate::jsonrpc::codes;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{ObjectKind, SymId, TypeId, TypeKind};

/// Go to definition: the declarations of the symbol under the cursor, through imports. On a
/// module specifier, the resolved file.
pub fn definition(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    if let Some(n) = node_at(ast, offset)
        && let Node::StringLit { value } = *ast.node(n)
    {
        let spec = ast.atom(value);
        if let Some(target) = module_specifier_target(cx, n, spec) {
            let mut lc = LinesCache::default();
            // An import type names the module's symbol, shown as the whole file; other
            // specifiers name the file, shown at its start.
            let parents = c.file(cx.file).parents();
            let in_import_type = std::iter::successors(parents.parent(n), |&p| parents.parent(p))
                .take(2)
                .any(|p| matches!(ast.node(p), Node::ImportType { .. }));
            let end = if in_import_type {
                c.file(target).text.len() as u32
            } else {
                0
            };
            return json!([lc.location(c, target, 0, end)]);
        }
        // A specifier naming an ambient module (`declare module "x"`), or that name itself.
        let names_module = c
            .file(cx.file)
            .parents()
            .parent(n)
            .is_some_and(|p| matches!(*ast.node(p), Node::Module { name, .. } if name == n));
        if names_module || is_module_specifier(cx, n) {
            let decls = ambient_module_decls(c, spec);
            if !decls.is_empty() {
                return decl_locations(cx, decls);
            }
        }
        // A string name in an import or export specifier (`export { a as "b" }`): what the
        // specifier's alias resolves to.
        if let Some(p) = c.file(cx.file).parents().parent(n) {
            let names = match *ast.node(p) {
                Node::ImportSpecifier { local, .. } => vec![local, p],
                Node::ExportSpecifier {
                    local, exported, ..
                } => exported.into_iter().chain([p, local]).collect(),
                _ => Vec::new(),
            };
            for x in names {
                if let Some(s) = c.sym_of_decl(Loc::new(cx.file, x)) {
                    let t = c.resolve_alias(s).unwrap_or(s);
                    return decl_locations(cx, c.decls(t));
                }
            }
        }
    }
    // `break l` / `continue l` go to the label.
    let d = Doc::new(c, cx.file);
    match keywords::special_at(&d, offset) {
        Some(Special::Label(Some(l))) => {
            let mut lc = LinesCache::default();
            return match keywords::label_refs(&d, l).first() {
                // On the label itself there is nothing to go to.
                Some(&(_, s, e)) if (s..=e).contains(&offset) => Value::Null,
                Some(&(f, s, e)) => json!([lc.location(c, f, s, e)]),
                None => Value::Null,
            };
        }
        Some(Special::Label(None)) => return Value::Null,
        _ => {}
    }
    if let Some(t) = keywords::keyword_definition(&d, offset) {
        return keyword_target(cx, ck, &d, t);
    }
    let Some(n) = ident_at(ast, offset) else {
        // A name in a doc comment's `{@link}`, `@see` or `@throws` path.
        return match super::doc_links::at(ck, cx.file, offset) {
            Some((_, _, s)) => decl_locations(cx, c.decls(c.resolve_alias(s).unwrap_or(s))),
            None => Value::Null,
        };
    };
    if let Some(decls) = literal_member_definition(cx, ck, n) {
        return decl_locations(cx, decls);
    }
    let m = meaning_at(ck, Loc::new(cx.file, n));
    let mut decls = Vec::new();
    let mut own = Vec::new();
    for s in m.iter() {
        own.extend(c.decls(s));
        match c.resolve_alias(s) {
            Some(t) => decls.extend(c.decls(t)),
            // An import from a shorthand ambient module (`declare module "x";`) has nothing
            // to resolve to but the module.
            None => match shorthand_import_module(cx, s) {
                Some(m) => decls.extend(m),
                None => decls.extend(c.decls(s)),
            },
        }
    }
    if let Some(called) = called_overload(ck, Loc::new(cx.file, n))
        && is_navigable_signature(c, called)
    {
        decls = with_called_signature(c, &own, decls, called);
    }
    if decls.is_empty() {
        decls = index_signatures_at(ck, Loc::new(cx.file, n));
    }
    decl_locations(cx, decls)
}

/// For `o.name` that no property declares, the index signatures of `o`'s type that apply to
/// the key (every member of a union; template-pattern keys the name matches).
fn index_signatures_at(ck: &mut Ck<'_, '_>, name: Loc) -> Vec<Loc> {
    let c = ck.c;
    let Some(p) = c.parent(name) else {
        return Vec::new();
    };
    let Node::Member { obj, prop, .. } = *c.node(p) else {
        return Vec::new();
    };
    if prop != name.node {
        return Vec::new();
    }
    let Some(key) = name_text(c.ast(name.file), name.node).map(str::to_string) else {
        return Vec::new();
    };
    let t = ck.check_expr(Loc::new(name.file, obj));
    let numeric = key.parse::<f64>().is_ok_and(|n| n.is_finite());
    let lit = ck.store().string_literal(&key, false);
    let mut out = Vec::new();
    for u in ck.union_members(t) {
        let a = ck.apparent_type(u);
        let infos = ck.resolved_members(a).index.to_vec();
        for i in infos {
            let applies = if i.key == TypeId::STRING {
                true
            } else if i.key == TypeId::NUMBER {
                numeric
            } else if i.key == TypeId::SYMBOL {
                false
            } else {
                ck.is_assignable(lit, i.key)
            };
            if applies
                && let Some(d) = i.decl.and_then(Loc::unpack)
                && (d.file.0 as usize) < c.files.len()
                && !out.contains(&d)
            {
                out.push(d);
            }
        }
    }
    out
}

/// Go to definition on a name in an object literal or an object binding pattern (observed
/// tsc 7): a binding key goes to the property of the destructured type (each union member's
/// that has it; nothing for an untyped pattern); a shorthand `{ x }` to the variable and
/// the contextual type's property; another member name of a contextually typed literal to
/// the contextual type's property only. `None` elsewhere.
fn literal_member_definition(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, n: NodeId) -> Option<Vec<Loc>> {
    let c = cx.checker;
    let at = Loc::new(cx.file, n);
    let p = c.parent(at)?;
    let ast = c.ast(cx.file);
    match *c.node(p) {
        Node::BindingProperty { key, .. } if key == n => {
            let name = name_text(ast, n)?.to_string();
            let pattern = c.parent(p)?;
            if !matches!(c.node(pattern), Node::ObjectPattern { .. }) {
                return None;
            }
            let mut out = Vec::new();
            if let Some(t) = super::locate::pattern_source_type(ck, pattern) {
                for u in ck.union_members(t) {
                    if let Some(prop) = super::locate::property_of(ck, u, &name) {
                        out.extend(c.decls(prop));
                    }
                }
            } else if let Some(prop) = super::locate::binding_property(ck, p, &name) {
                out.extend(c.decls(prop));
            }
            Some(out)
        }
        Node::ShorthandProp { name, .. } if name == n => {
            let m = meaning_at(ck, at);
            let mut out = Vec::new();
            if let Some(v) = m.shorthand {
                out.extend(c.decls(c.resolve_alias(v).unwrap_or(v)));
            }
            if let Some(s) = m.contextual {
                let lit = c.parent(p)?;
                out.extend(contextual_member_decls(ck, lit, p, s));
            }
            Some(out)
        }
        Node::Property { key, .. } | Node::MethodDef { key, .. } if key == n => {
            let lit = c.parent(p)?;
            if !matches!(c.node(lit), Node::ObjectLit { .. }) {
                return None;
            }
            let s = meaning_at(ck, at).contextual?;
            let decls = contextual_member_decls(ck, lit, p, s);
            (!decls.is_empty()).then_some(decls)
        }
        _ => None,
    }
}

/// The declarations of the contextual type's property `s` that member `member` of object
/// literal `lit` implements: of the union members the literal's discriminants select
/// (`kind: "a"` picks `A` of `A | B`; all of them when none matches), and when the
/// contextual type was inferred from this very literal (`fn<T extends C>(arg: T)`), of the
/// declared parameter type with type parameters replaced by their constraints.
fn contextual_member_decls(ck: &mut Ck<'_, '_>, lit: Loc, member: Loc, s: SymId) -> Vec<Loc> {
    let c = ck.c;
    let Some(name) = member_name(ck, member) else {
        return c.decls(s);
    };
    let decls = c.decls(s);
    let mut types: Vec<TypeId> = Vec::new();
    if decls.contains(&member) || decls.iter().any(|d| c.parent(*d) == Some(lit)) {
        if let Some(t) = uninferred_parameter_type(ck, lit) {
            for u in ck.union_members(t) {
                let u = if ck.store().type_param_data(u).is_some() {
                    match ck.type_param_constraint(u) {
                        Some(k) => k,
                        None => continue,
                    }
                } else {
                    u
                };
                types.extend(ck.union_members(u));
            }
        }
    } else if let Some(t) = ck.contextual_type(lit) {
        types = ck.union_members(t);
    }
    if types.is_empty() {
        return decls;
    }
    let types = discriminate(ck, lit, types);
    let mut out = Vec::new();
    for t in types {
        if let Some(p) = super::locate::property_of(ck, t, &name) {
            for d in c.decls(p) {
                if !out.contains(&d) {
                    out.push(d);
                }
            }
        }
    }
    if out.is_empty() { decls } else { out }
}

/// The key text of an object literal member.
fn member_name(ck: &Ck<'_, '_>, member: Loc) -> Option<String> {
    let c = ck.c;
    let key = match *c.node(member) {
        Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
        Node::ShorthandProp { name, .. } => name,
        _ => return None,
    };
    super::locate::key_text(ck, Loc::new(member.file, key))
}

/// The declared (uninstantiated) type of the parameter that object literal `lit` is passed
/// to, when it is an argument of a call to a generic signature.
fn uninferred_parameter_type(ck: &mut Ck<'_, '_>, lit: Loc) -> Option<TypeId> {
    let c = ck.c;
    let call = c.parent(lit)?;
    let args = match *c.node(call) {
        Node::Call { args, .. } | Node::New { args, .. } => args,
        _ => return None,
    };
    let i = c
        .ast(lit.file)
        .list(args)
        .iter()
        .position(|&a| a == lit.node)?;
    let sig = ck.resolved_signature(call)?;
    let store = ck.store();
    let (target, _) = store.sig(sig).target?;
    store.sig(target).params.get(i).map(|p| p.ty)
}

/// The members of a contextual union that the literal's discriminant properties (members
/// with a unit-typed value, `kind: "a"`) are assignable to; all of them when none is.
fn discriminate(ck: &mut Ck<'_, '_>, lit: Loc, types: Vec<TypeId>) -> Vec<TypeId> {
    if types.len() < 2 {
        return types;
    }
    let c = ck.c;
    let Node::ObjectLit { props } = *c.node(lit) else {
        return types;
    };
    let mut kept = types.clone();
    for &prop in c.ast(lit.file).list(props) {
        let Node::Property { key, value, .. } = *c.ast(lit.file).node(prop) else {
            continue;
        };
        let Some(name) = super::locate::key_text(ck, Loc::new(lit.file, key)) else {
            continue;
        };
        let v = ck.check_expr(Loc::new(lit.file, value));
        let v = ck.store().regular(v);
        if !ck.is_unit(v) {
            continue;
        }
        let mut discriminant = false;
        let mut matching = Vec::new();
        for &t in &kept {
            let Some(p) = super::locate::property_of(ck, t, &name) else {
                continue;
            };
            let pt = ck.type_of_symbol(p);
            if ck.union_members(pt).iter().any(|&m| ck.is_unit(m)) {
                discriminant = true;
            }
            if ck.is_assignable(v, pt) {
                matching.push(t);
            }
        }
        if discriminant && !matching.is_empty() {
            kept = matching;
        }
    }
    kept
}

/// The declarations go to definition shows for the callee of a call, `new` or tagged
/// template that resolves to the signature declared at `called` (observed tsc 7; the rule is
/// written up in AGENTS.md, "Keywords and navigation"). `own` are the declarations of the
/// symbol at the cursor before aliases are followed.
fn with_called_signature(c: &Checker<'_>, own: &[Loc], decls: Vec<Loc>, called: Loc) -> Vec<Loc> {
    let ctor_class = constructor_class(c, called);
    let parent = c.parent(called);
    let own_signature = own.contains(&called)
        || ctor_class.is_some_and(|k| own.contains(&k))
        || parent.is_some_and(|p| match *c.node(p) {
            Node::Assign { .. } => true,
            Node::Call { .. } | Node::New { .. } | Node::TaggedTemplate { .. } => false,
            _ => own.contains(&p),
        });
    let mut out: Vec<Loc> = if !own_signature {
        decls.into_iter().filter(|&d| d != called).collect()
    } else if ctor_class.is_some() {
        decls
            .into_iter()
            .filter(|&d| {
                d != called && matches!(c.node(d), Node::ClassDecl { .. } | Node::ClassExpr { .. })
            })
            .collect()
    } else {
        Vec::new()
    };
    out.push(called);
    out
}

/// A signature declaration go to definition can show: function-like, not a function type
/// (`() => void` goes to the value that has it instead; `new () => T` is shown).
fn is_navigable_signature(c: &Checker<'_>, l: Loc) -> bool {
    matches!(
        c.node(l),
        Node::FunctionDecl { .. }
            | Node::ConstructorType { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
    )
}

/// The name an unnamed function, arrow or class expression gets from where it is assigned
/// (`const f = () => 0`, `{ m: function () {} }`, `x.y = class {}`), which go to definition
/// shows for it.
fn assigned_name(c: &Checker<'_>, d: Loc) -> Option<Loc> {
    let named = match *c.node(d) {
        Node::FunctionExpr { name, .. } | Node::ClassExpr { name, .. } => name.is_some(),
        Node::Arrow { .. } => false,
        _ => return None,
    };
    if named {
        return None;
    }
    let p = c.parent(d)?;
    let at = |n: NodeId| Some(Loc::new(p.file, n));
    match *c.node(p) {
        Node::VarDeclarator { name, init, .. } if init == Some(d.node) => {
            matches!(c.ast(p.file).node(name), Node::Ident { .. })
                .then_some(())
                .and_then(|_| at(name))
        }
        Node::Property { key, value, .. } if value == d.node => at(key),
        Node::PropertyDef { key, value, .. } if value == Some(d.node) => at(key),
        Node::Assign { left, right, .. } if right == d.node => match *c.ast(p.file).node(left) {
            Node::Ident { .. } => at(left),
            Node::Member { prop, .. } => at(prop),
            _ => None,
        },
        _ => None,
    }
}

/// The class a constructor declaration belongs to.
fn constructor_class(c: &Checker<'_>, l: Loc) -> Option<Loc> {
    let mut m = l;
    if matches!(c.node(m), Node::FunctionExpr { .. }) {
        m = c.parent(m)?;
    }
    if !matches!(
        c.node(m),
        Node::MethodDef {
            kind: MethodKind::Constructor,
            ..
        }
    ) {
        return None;
    }
    let k = c.parent(m)?;
    matches!(c.node(k), Node::ClassDecl { .. } | Node::ClassExpr { .. }).then_some(k)
}

/// Where go to definition on a keyword leads (see [`keywords::keyword_definition`]).
fn keyword_target(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    d: &Doc<'_>,
    t: keywords::KeywordTarget,
) -> Value {
    use keywords::KeywordTarget as T;
    let c = cx.checker;
    match t {
        T::Spans(v) if v.is_empty() => Value::Null,
        T::Spans(v) => locations(cx, v.into_iter().map(|(s, e)| (cx.file, s, e))),
        T::Name(n) => definition(cx, ck, c.ast(cx.file).span(n).start),
        T::Override { member, name } => {
            let own = || {
                let s = d.ast.span(name);
                locations(cx, std::iter::once((cx.file, s.start, s.end)))
            };
            let Some(key) = name_text(d.ast, name) else {
                return own();
            };
            let Some(class) =
                keywords::member_class(d, member).and_then(|k| c.sym_of_decl(Loc::new(cx.file, k)))
            else {
                return own();
            };
            let bases: Vec<TypeId> = if keywords::member_is_static(d, member) {
                ck.base_constructor_type(class).into_iter().collect()
            } else {
                ck.base_types(class).to_vec()
            };
            let mut decls = Vec::new();
            for b in bases {
                if let Some(p) = ck.property_of_type(b, key) {
                    decls.extend(c.decls(p));
                }
            }
            if decls.is_empty() {
                own()
            } else {
                decl_locations(cx, decls)
            }
        }
        T::Super(n) => {
            let Some((member, _)) = keywords::super_member(d, n) else {
                return Value::Null;
            };
            let Some(class) =
                keywords::member_class(d, member).and_then(|k| c.sym_of_decl(Loc::new(cx.file, k)))
            else {
                return Value::Null;
            };
            let mut decls = Vec::new();
            for b in ck.base_types(class).to_vec() {
                type_decls(ck, b, &mut decls, 0);
            }
            let mut out = Vec::new();
            for l in decls {
                let (s, e) = decl_span(c, l);
                out.push((l.file, s, e));
            }
            // `super(...)` also goes to the constructor it calls, shown whole.
            if let Some(ctor) = called_overload(ck, Loc::new(cx.file, n)) {
                let s = c.ast(ctor.file).span(ctor.node);
                out.push((ctor.file, s.start, s.end));
            }
            out.dedup();
            locations(cx, out.into_iter())
        }
    }
}

/// The declaration of the signature a call resolves to, when `name` is the callee (`f(1)`,
/// `o.m(1)`, `new C()`, ``f`x` ``).
fn called_overload(ck: &mut Ck<'_, '_>, name: Loc) -> Option<Loc> {
    let c = ck.c;
    let mut callee = name.node;
    let mut p = c.parent(name)?;
    if let Node::Member { prop, .. } = *c.node(p)
        && prop == name.node
    {
        callee = p.node;
        p = c.parent(p)?;
    }
    let is_callee = match *c.node(p) {
        Node::Call { callee: x, .. } | Node::New { callee: x, .. } => x == callee,
        Node::TaggedTemplate { tag, .. } => tag == callee,
        _ => false,
    };
    if !is_callee {
        return None;
    }
    let sig = ck.resolved_signature(p)?;
    let store = ck.store();
    let target = store.sig(sig).target.map(|t| t.0).unwrap_or(sig);
    let (f, n) = store.sig(target).decl?;
    if f as usize >= c.files.len() {
        return None;
    }
    Some(Loc::new(FileId(f), ck.node_id(n)?))
}

/// The declarations of the ambient module `declare module "<spec>"` (or the pattern that
/// matches it).
fn ambient_module_decls(c: &Checker<'_>, spec: &str) -> Vec<Loc> {
    let Some(i) = c.link.find_ambient_module(spec) else {
        return Vec::new();
    };
    c.link
        .module_symbols(ts_r_binder::program::ModuleTarget::Ambient(i))
        .into_iter()
        .flat_map(|r| c.decls(c.sym_ref(r)))
        .collect()
}

/// For an import (`import x from "m"`, `import { x } from "m"`) of a shorthand ambient
/// module, that module's declarations.
fn shorthand_import_module(cx: &Ctx<'_, '_>, s: SymId) -> Option<Vec<Loc>> {
    let c = cx.checker;
    let d = c.first_decl(s)?;
    let mut p = c.parent(d)?;
    for _ in 0..4 {
        if let Node::ImportDecl { source, .. } = *c.node(p) {
            let Node::StringLit { value } = *c.ast(p.file).node(source) else {
                return None;
            };
            let i = c.link.find_ambient_module(c.ast(p.file).atom(value))?;
            let target = ts_r_binder::program::ModuleTarget::Ambient(i);
            let &first = c.link.module_symbols(target).first()?;
            if !c.is_shorthand_ambient_module(c.sym_ref(first)) {
                return None;
            }
            return Some(ambient_module_decls(c, c.ast(p.file).atom(value)));
        }
        p = c.parent(p)?;
    }
    None
}

/// Whether string literal `lit` is a module specifier (imports, exports, `import x =
/// require(..)`, `import(..)`, import types).
fn is_module_specifier(cx: &Ctx<'_, '_>, lit: ts_r_ast::NodeId) -> bool {
    let c = cx.checker;
    let Some(parent) = c.file(cx.file).parents().parent(lit) else {
        return false;
    };
    match *c.ast(cx.file).node(parent) {
        Node::ImportDecl { source, .. } | Node::ExportAll { source, .. } => source == lit,
        Node::ExportNamed { source, .. } => source == Some(lit),
        Node::ExternalModuleRef { expr } => expr == lit,
        Node::ImportCall { arg, .. } => arg == lit,
        Node::ImportType { .. } => true,
        _ => false,
    }
}

/// The file a module specifier string resolves to.
fn module_specifier_target(cx: &Ctx<'_, '_>, lit: ts_r_ast::NodeId, spec: &str) -> Option<FileId> {
    let c = cx.checker;
    if !is_module_specifier(cx, lit) {
        return None;
    }
    let from = &c.file(cx.file).path;
    let r = cx
        .program
        .resolutions
        .get(&(from.clone(), spec.to_string()))?
        .as_ref()?;
    let target = r.resolved.as_str();
    c.files
        .iter()
        .position(|f| f.path == target)
        .map(|i| FileId(i as u32))
}

/// Go to type definition: the declarations of the type of the symbol under the cursor
/// (tsc: `tsc/internal/ls/definition.go`, ProvideTypeDefinition). A keyword before a
/// declaration's name stands for the name; a function's own type stands for its return
/// type; `Array<T>`, `Promise<T>` and the other well-known generics also lead to `T`; a
/// type-only symbol whose type has no declaration goes to itself.
pub fn type_definition(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let d = Doc::new(c, cx.file);
    let Some(n) = keywords::keyword_name(&d, offset)
        .or_else(|| ident_at(ast, offset))
        .or_else(|| node_at(ast, offset))
    else {
        return Value::Null;
    };
    let l = Loc::new(cx.file, n);
    let (t, sym) = match ast.node(n) {
        Node::This {} => (ck.check_expr(l), None),
        _ => {
            let Some(s) = meaning_at(ck, l).primary else {
                return Value::Null;
            };
            let s = c.resolve_alias(s).unwrap_or(s);
            let t = if super::locate::is_expression_name(ck, l) {
                ck.check_expr(l)
            } else if let Some(p) = c.parent(l)
                && let Node::Member { prop, .. } = *c.node(p)
                && prop == n
            {
                ck.check_expr(p)
            } else {
                ck.type_of_symbol(s)
            };
            (t, Some(s))
        }
    };
    let t = match sym {
        Some(s) if is_own_function_type(ck, s, t) => {
            let sigs = ck.signatures_of(t, ts_r_checker::members::SigKind::Call);
            match sigs[..] {
                [sig] => ck.return_type_of(sig),
                _ => t,
            }
        }
        _ => t,
    };
    let mut decls = Vec::new();
    if let Some(arg) = known_type_argument(ck, t) {
        type_decls(ck, arg, &mut decls, 0);
    }
    type_decls(ck, t, &mut decls, 0);
    if decls.is_empty()
        && let Some(s) = sym
    {
        let f = c.sym_flags(s);
        if !f.intersects(SymbolFlags::VALUE) && f.intersects(SymbolFlags::TYPE) {
            decls.extend(c.decls(s));
        }
    }
    let mut seen = HashSet::new();
    decls.retain(|l| seen.insert(*l));
    decl_locations(cx, decls)
}

/// The symbol a type comes from (tsc's `type.symbol`): the class or interface, the
/// function, class or namespace of a `typeof`, the type literal, the enum member.
pub(super) fn type_symbol(ck: &Ck<'_, '_>, t: TypeId) -> Option<SymId> {
    let store = ck.store();
    match store.kind(t) {
        TypeKind::Object(_) => match store.object_of(t)?.kind {
            ObjectKind::Declared { sym, .. } | ObjectKind::Literal { sym } => Some(sym),
            ObjectKind::Reference { target, .. } => match store.object_of(target)?.kind {
                ObjectKind::Declared { sym, .. } => Some(sym),
                _ => None,
            },
            ObjectKind::Anonymous {
                decl: (u32::MAX, s),
                ..
            } => Some(SymId(s)),
            ObjectKind::Anonymous { decl: (f, n), .. } => {
                let node = ck.node_id(n)?;
                ck.c.sym_of_decl(Loc::new(FileId(f), node))
            }
            _ => None,
        },
        TypeKind::Enum(s) => Some(s),
        TypeKind::EnumLiteral { member, .. } => Some(member),
        _ => None,
    }
}

/// Whether `t` is just the inferred type of the function `s` (or of the function that
/// initializes the variable `s`): go to type definition shows its return type instead.
fn is_own_function_type(ck: &Ck<'_, '_>, s: SymId, t: TypeId) -> bool {
    let c = ck.c;
    let store = ck.store();
    let fn_decl = match store.object_of(t).map(|o| o.kind) {
        Some(ObjectKind::Anonymous {
            decl: (u32::MAX, f),
            ..
        }) => c.first_decl(SymId(f)),
        Some(ObjectKind::Literal { sym }) => c.first_decl(sym),
        Some(ObjectKind::Anonymous { decl: (f, n), .. }) if (f as usize) < c.files.len() => {
            ck.node_id(n).map(|n| Loc::new(FileId(f), n))
        }
        _ => None,
    };
    if let Some(fd) = fn_decl {
        // `const f = () => {}`: the arrow is the variable's initializer.
        let is_init = |d: Loc| {
            d.file == fd.file
                && matches!(*c.node(d), Node::VarDeclarator { init: Some(i), .. } if i == fd.node)
        };
        if c.decls(s)
            .into_iter()
            .any(|d| is_init(d) || c.parent(d).is_some_and(is_init))
        {
            return true;
        }
    }
    type_symbol(ck, t) == Some(s)
}

/// The first type argument of `Array<T>`, `Promise<T>`, `Pick<T, K>` and the other
/// well-known global generics (tsc: `GetFirstTypeArgumentFromKnownType`).
fn known_type_argument(ck: &Ck<'_, '_>, t: TypeId) -> Option<TypeId> {
    const KNOWN: &[&str] = &[
        "Array",
        "ArrayLike",
        "ReadonlyArray",
        "Promise",
        "PromiseLike",
        "Iterable",
        "IterableIterator",
        "AsyncIterable",
        "Set",
        "WeakSet",
        "ReadonlySet",
        "Map",
        "WeakMap",
        "ReadonlyMap",
        "Partial",
        "Required",
        "Readonly",
        "Pick",
        "Omit",
        "NonNullable",
    ];
    let c = ck.c;
    let store = ck.store();
    let is_known_global = |s: SymId| {
        KNOWN.contains(&c.sym_name(s)) && c.decls(s).iter().all(|d| c.file(d.file).is_lib)
    };
    if let Some(ObjectKind::Reference { target, args }) = store.object_of(t).map(|o| o.kind)
        && let Some(ObjectKind::Declared { sym, .. }) = store.object_of(target).map(|o| o.kind)
        && is_known_global(sym)
    {
        return store.list_items(args).first().copied();
    }
    let a = store.alias_of(t)?;
    if is_known_global(a.sym) {
        return store.list_items(a.args).first().copied();
    }
    None
}

/// Declarations of the types in `t` (tsc's `getDeclarationsFromType`): the declarations of
/// the symbol of each union member.
fn type_decls(ck: &mut Ck<'_, '_>, t: TypeId, out: &mut Vec<Loc>, depth: u32) {
    if depth > 8 {
        return;
    }
    let c = ck.c;
    let store = ck.store();
    match store.kind(t) {
        TypeKind::Union { list, .. } => {
            let items = store.list_items(list).to_vec();
            for m in items {
                type_decls(ck, m, out, depth + 1);
            }
        }
        TypeKind::Object(_) => {
            let Some(o) = store.object_of(t) else { return };
            match o.kind {
                ObjectKind::Anonymous { decl: (f, n), .. }
                | ObjectKind::Mapped { decl: (f, n), .. }
                    if f != u32::MAX =>
                {
                    if let Some(node) = ck.node_id(n)
                        && (f as usize) < c.files.len()
                    {
                        out.push(Loc::new(FileId(f), node));
                    }
                }
                _ => {
                    if let Some(s) = type_symbol(ck, t) {
                        out.extend(c.decls(s));
                    }
                }
            }
        }
        TypeKind::Enum(_) | TypeKind::EnumLiteral { .. } => {
            if let Some(s) = type_symbol(ck, t) {
                out.extend(c.decls(s));
            }
        }
        TypeKind::TypeParameter(_) => {
            if let Some(d) = store.type_param_data(t) {
                out.extend(c.decls(d.sym));
            }
        }
        _ => {}
    }
}

fn decl_locations(cx: &Ctx<'_, '_>, decls: Vec<Loc>) -> Value {
    let c = cx.checker;
    let mut lc = LinesCache::default();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for d in decls {
        // A constructor has no name: the whole declaration is shown.
        let (s, e) = match constructor_class(c, d) {
            Some(_) => {
                let m = if matches!(c.node(d), Node::FunctionExpr { .. }) {
                    c.parent(d).unwrap_or(d)
                } else {
                    d
                };
                let sp = c.ast(m.file).span(m.node);
                (sp.start, sp.end)
            }
            None => match assigned_name(c, d) {
                Some(n) => {
                    let sp = c.ast(n.file).span(n.node);
                    (sp.start, sp.end)
                }
                None => decl_span(c, d),
            },
        };
        if seen.insert((d.file, s, e)) {
            out.push(lc.location(c, d.file, s, e));
        }
    }
    Value::Array(out)
}

/// Where to look for references.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    File,
    Project,
}

/// One reference to the symbol.
#[derive(Clone, Copy, Debug)]
pub struct Ref {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
    pub is_decl: bool,
    pub is_write: bool,
    /// Which meaning of a shorthand matched (`None` when not a shorthand).
    pub shorthand: Option<ShorthandSide>,
}

/// Which side of a shorthand (`{ x }`) a reference is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShorthandSide {
    /// The property (`x:` of `{ x: x }`).
    Property,
    /// The variable (`: x` of `{ x: x }`).
    Variable,
}

impl Ref {
    pub fn loc_span(&self) -> (FileId, u32, u32) {
        (self.file, self.start, self.end)
    }
}

/// The symbol under the cursor, resolved through aliases, and the declarations that
/// identify it.
struct Target {
    name: String,
    syms: Vec<SymId>,
    decls: HashSet<Loc>,
    /// Declarations of the members the target inherits or overrides
    /// ([`super::inherit::ancestor_decls`]); empty when it is no class or interface member.
    ancestors: HashSet<Loc>,
    /// Candidates already compared by ancestry.
    related: std::collections::HashMap<SymId, bool>,
    /// The meanings searched ([`super::ref_meaning::search_meaning`]).
    meaning: u8,
    /// A search from an alias that renames (`import { a as b }`): names match when this
    /// alias is on their alias chain ([`super::aliases`]).
    alias: Option<SymId>,
    /// Only aliases of the target match (a search from the `default` keyword of an
    /// exported declaration finds its imports, not its local uses).
    imports_only: bool,
    /// Other names aliases give the target (`b` of `import { a as b }`).
    alias_names: Vec<String>,
}

impl Target {
    fn at(ck: &mut Ck<'_, '_>, l: Loc, rename: bool) -> Option<Target> {
        let c = ck.c;
        let name = key_text(ck, l)?;
        let m = meaning_at(ck, l);
        // On a shorthand, a rename is the variable's (what tsserver does); references
        // are those of both the variable and the property.
        let (s, also) = match (m.primary, m.shorthand) {
            (Some(p), Some(v)) if matches!(c.node(c.parent(l)?), Node::ShorthandProp { .. }) => {
                (v, (!rename).then_some(p))
            }
            (Some(p), v) => (p, v.filter(|_| !rename)),
            (None, Some(v)) => (v, None),
            (None, None) => return None,
        };
        let mut s = s;
        if let Some(t) = super::aliases::imported_side(c, l) {
            s = t;
        }
        let start = super::aliases::start(c, s, rename);
        if super::aliases::is_alias(c, start) && also.is_none() && m.contextual.is_none() {
            return Some(Target::of_alias(ck, l, name, start, rename));
        }
        let s = c.resolve_alias(start).unwrap_or(start);
        let own = c.decls(s);
        let meaning = if rename {
            ts_r_binder::meaning::ALL
        } else {
            super::ref_meaning::search_meaning(ck, l, &own)
        };
        let mut decls: HashSet<Loc> = own.into_iter().collect();
        let mut syms = vec![s];
        if let Some(a) = also {
            let a = c.resolve_alias(a).unwrap_or(a);
            decls.extend(c.decls(a));
            syms.push(a);
        }
        // A contextually typed literal's property is one with the property it implements.
        if let Some(p) = m.contextual
            && p != s
        {
            decls.extend(c.decls(p));
            syms.push(p);
        }
        let mut ancestors = HashSet::new();
        for &t in &syms {
            ancestors.extend(super::inherit::ancestor_decls(ck, t));
        }
        Some(Target {
            name,
            syms,
            decls,
            ancestors,
            related: Default::default(),
            meaning,
            alias: None,
            imports_only: false,
            alias_names: Vec::new(),
        })
    }

    /// A search for the alias `a` itself (see [`Target::alias`]).
    fn of_alias(ck: &mut Ck<'_, '_>, l: Loc, name: String, a: SymId, rename: bool) -> Target {
        let c = ck.c;
        let own = c.decls(a);
        let meaning = if rename {
            ts_r_binder::meaning::ALL
        } else {
            super::ref_meaning::search_meaning(ck, l, &own)
        };
        Target {
            name,
            syms: vec![a],
            decls: own.into_iter().collect(),
            ancestors: HashSet::new(),
            related: Default::default(),
            meaning,
            alias: Some(a),
            imports_only: false,
            alias_names: Vec::new(),
        }
    }

    /// The symbol searched for: the alias in an alias search, else the declared symbol.
    fn symbol(&self) -> SymId {
        self.alias.unwrap_or(self.syms[0])
    }

    fn has_name(&self, n: &str) -> bool {
        self.name == n || self.alias_names.iter().any(|a| a == n)
    }

    fn matches(&mut self, ck: &mut Ck<'_, '_>, s: SymId) -> bool {
        let c = ck.c;
        if let Some(a) = self.alias {
            return super::aliases::chain(c, s).contains(&a);
        }
        if self.imports_only {
            return super::aliases::is_alias(c, s)
                && super::aliases::chain(c, s).contains(&self.syms[0]);
        }
        let s = c.resolve_alias(s).unwrap_or(s);
        if self.syms.contains(&s) {
            return true;
        }
        if !self.decls.is_empty() && c.decls(s).iter().any(|d| self.decls.contains(d)) {
            return true;
        }
        if self.ancestors.is_empty() {
            return false;
        }
        if let Some(&r) = self.related.get(&s) {
            return r;
        }
        let up = super::inherit::ancestor_decls(ck, s);
        let r = up.iter().any(|d| self.ancestors.contains(d));
        self.related.insert(s, r);
        r
    }
}

/// Whether a numeric literal indexing an element access (`o[0]`) starts at `offset`.
fn is_numeric_index(c: &ts_r_checker::Checker, f: FileId, offset: u32) -> bool {
    let ast = c.ast(f);
    node_at(ast, offset).is_some_and(|n| {
        matches!(ast.node(n), Node::NumberLit { .. })
            && c.file(f)
                .parents()
                .parent(n)
                .is_some_and(|p| matches!(ast.node(p), Node::Index { index, .. } if *index == n))
    })
}

/// The string or numeric literal key at `offset` (`"a": 1`, `o["a"]`, `o[0]`).
fn literal_key_at(cx: &Ctx<'_, '_>, offset: u32) -> Option<NodeId> {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let parents = c.file(cx.file).parents();
    [Some(offset), offset.checked_sub(1)]
        .into_iter()
        .flatten()
        .filter_map(|o| node_at(ast, o))
        .find(|&n| literal_key(ast, parents, n).is_some())
}

/// References to the symbol at `offset`, in `scope`, in file order then source order.
pub fn references(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32, scope: Scope) -> Vec<Ref> {
    references_with(cx, ck, offset, scope, false)
}

/// [`references`], for a rename when `rename` is set (a shorthand renames only its variable).
fn references_with(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    offset: u32,
    scope: Scope,
    rename: bool,
) -> Vec<Ref> {
    let c = cx.checker;
    if let Some(refs) = special_references(cx, offset, scope) {
        return refs;
    }
    // For find-all-references, a modifier or declaration keyword stands for the declared
    // name; document highlights on such a keyword are the keyword's own (or none).
    let keyword_name = (scope == Scope::Project)
        .then(|| {
            let d = Doc::new(c, cx.file);
            if rename {
                keywords::keyword_name(&d, offset)
            } else {
                keywords::reference_keyword(&d, offset)
            }
        })
        .flatten();
    // A module specifier (also as a keyword's operand: `from`, `require`), an ambient
    // module's name or a `/// <reference path>`: the references of the module.
    if !rename {
        let module = match keyword_name {
            Some(n) => super::module_refs::literal_module(c, cx.file, n),
            None => super::module_refs::module_at(c, cx.file, offset),
        };
        if let Some(m) = module {
            let files: Vec<FileId> = match scope {
                Scope::File => vec![cx.file],
                Scope::Project => project_files(cx).collect(),
            };
            return super::module_refs::module_references(c, &files, m)
                .into_iter()
                .map(|(file, start, end)| Ref {
                    file,
                    start,
                    end,
                    is_decl: false,
                    is_write: false,
                    shorthand: None,
                })
                .collect();
        }
    }
    if keyword_name.is_none()
        && let Some((decl, ks, ke)) = super::aliases::default_keyword_at(c, cx.file, offset)
    {
        return default_keyword_references(cx, ck, decl, (ks, ke), scope, rename);
    }
    let Some(n) = keyword_name
        .or_else(|| ident_at(c.ast(cx.file), offset))
        .or_else(|| literal_key_at(cx, offset))
    else {
        // A name in a doc comment's `{@link}`, `@see` or `@throws` path stands for its
        // declaration.
        if let Some(refs) = link_references(cx, ck, offset, scope, rename) {
            return refs;
        }
        return string_literal_references(cx, ck, offset, scope).unwrap_or_default();
    };
    if scope == Scope::Project {
        let files: Vec<FileId> = project_files(cx).collect();
        if let Some(refs) = super::constructor_refs::constructor_references(cx, ck, &files, n) {
            return refs;
        }
    }
    let Some(mut target) = Target::at(ck, Loc::new(cx.file, n), rename) else {
        // A JSX attribute that sets no prop is a reference to itself alone.
        if is_jsx_attr_name(c, Loc::new(cx.file, n)) {
            let span = c.ast(cx.file).span(n);
            return vec![Ref {
                file: cx.file,
                start: span.start,
                end: span.end,
                is_decl: false,
                is_write: false,
                shorthand: None,
            }];
        }
        // A string literal key that names no symbol: the literals of its type.
        return string_literal_references(cx, ck, offset, scope).unwrap_or_default();
    };
    collect_alias_names(cx, &mut target, scope, rename);
    let mut out = scan_target(cx, ck, &mut target, scope);
    // On a class's name, `this` in its static methods and accessors is the class too.
    let on_class_name = c.parent(Loc::new(cx.file, n)).is_some_and(|p| {
        matches!(c.node(p), Node::ClassDecl { name: Some(x), .. } | Node::ClassExpr { name: Some(x), .. } if *x == n)
    });
    if on_class_name {
        let mut classes: Vec<Loc> = target
            .decls
            .iter()
            .copied()
            .filter(|d| matches!(c.node(*d), Node::ClassDecl { .. } | Node::ClassExpr { .. }))
            .filter(|d| scope == Scope::Project || d.file == cx.file)
            .collect();
        classes.sort_by_key(|d| (d.file.0, d.node.index()));
        for d in classes {
            let doc = Doc::new(c, d.file);
            out.extend(keywords::static_this_refs(&doc, d.node).into_iter().map(
                |(file, start, end)| Ref {
                    file,
                    start,
                    end,
                    is_decl: false,
                    is_write: false,
                    shorthand: None,
                },
            ));
        }
        out.sort_by_key(|r| (r.file.0, r.start));
        out.dedup_by_key(|r| (r.file.0, r.start));
    }
    out
}

/// Every name in `scope` that refers to `target`, in file order then source order, plus
/// the `default` keyword of an anonymous default export among its declarations.
fn scan_target(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    target: &mut Target,
    scope: Scope,
) -> Vec<Ref> {
    let c = cx.checker;
    let files: Vec<FileId> = match scope {
        Scope::File => vec![cx.file],
        Scope::Project => (0..c.files.len())
            .map(|i| FileId(i as u32))
            .filter(|&f| {
                let sf = c.file(f);
                !sf.is_lib
                    && !sf.path.contains("/node_modules/")
                    && (sf.text.contains(&target.name)
                        || target
                            .alias_names
                            .iter()
                            .any(|a| sf.text.contains(a.as_str())))
            })
            .collect(),
    };
    let mut out = Vec::new();
    for f in files {
        let ast = c.ast(f);
        let first = out.len();
        for id in names_in(ast) {
            if !name_text(ast, id).is_some_and(|t| target.has_name(t)) {
                continue;
            }
            let l = Loc::new(f, id);
            if target.meaning != ts_r_binder::meaning::ALL
                && super::ref_meaning::location_meaning(ck, l) & target.meaning == 0
            {
                continue;
            }
            let m = meaning_at(ck, l);
            let Meaning {
                mut primary,
                shorthand,
                is_decl,
                contextual,
            } = m;
            if target.alias.is_some()
                && let Some(t) = super::aliases::imported_side(c, l)
            {
                primary = Some(t);
            }
            let p = primary.is_some_and(|s| target.matches(ck, s))
                || contextual.is_some_and(|s| target.matches(ck, s));
            let v = shorthand.is_some_and(|s| target.matches(ck, s));
            if !p && !v {
                continue;
            }
            let side = match (shorthand.is_some(), p, v) {
                (false, ..) => None,
                (true, true, true) => None,
                (true, true, false) => Some(primary_side(ck, l)),
                (true, false, _) => Some(other(primary_side(ck, l))),
            };
            let span = ast.span(id);
            out.push(Ref {
                file: f,
                start: span.start,
                end: span.end,
                is_decl: is_decl && p,
                is_write: if is_decl && p {
                    decl_is_write(ck, l)
                } else {
                    is_write(ck, l)
                },
                shorthand: side,
            });
        }
        // Literal keys: `"name": 1`, `get 42()`, `o["name"]`, `o[0]`.
        let text = c.file(f).text.as_str();
        for id in literal_keys_in(ast, c.file(f).parents()) {
            let l = Loc::new(f, id);
            if key_text(ck, l).as_deref() != Some(target.name.as_str()) {
                continue;
            }
            let Some((start, end)) = literal_key_span(ast, text, id, &target.name) else {
                continue;
            };
            let m = meaning_at(ck, l);
            let p = m.primary.is_some_and(|s| target.matches(ck, s))
                || m.contextual.is_some_and(|s| target.matches(ck, s));
            if !p {
                continue;
            }
            let access = super::locate::literal_key_owner(ck, l).unwrap_or(l);
            out.push(Ref {
                file: f,
                start,
                end,
                is_decl: m.is_decl,
                is_write: if m.is_decl {
                    decl_is_write(ck, access)
                } else {
                    is_write(ck, access)
                },
                shorthand: None,
            });
        }
        if text.contains('@') {
            link_refs(ck, f, target, &mut out);
        }
        out[first..].sort_by_key(|r| r.start);
    }
    for d in target.decls.iter().copied().collect::<Vec<_>>() {
        if (scope == Scope::Project || d.file == cx.file)
            && let Some((start, end)) = super::aliases::default_keyword_of(c, d)
        {
            out.push(Ref {
                file: d.file,
                start,
                end,
                is_decl: true,
                is_write: false,
                shorthand: None,
            });
        }
    }
    out.sort_by_key(|r| (r.file.0, r.start));
    out.dedup_by_key(|r| (r.file.0, r.start));
    out
}

/// The names in file `f`'s doc comment paths ([`super::doc_links`]) that refer to `target`.
fn link_refs(ck: &mut Ck<'_, '_>, f: FileId, target: &mut Target, out: &mut Vec<Ref>) {
    let c = ck.c;
    let text = c.file(f).text.as_str();
    for link in super::doc_links::links_in(c, f) {
        let named: Vec<bool> = link
            .segs
            .iter()
            .map(|&(s, e)| target.has_name(&text[s as usize..e as usize]))
            .collect();
        if !named.contains(&true) {
            continue;
        }
        let syms = super::doc_links::resolve(ck, f, &link);
        for ((&(start, end), sym), named) in link.segs.iter().zip(syms).zip(named) {
            if named && sym.is_some_and(|s| target.matches(ck, s)) {
                out.push(Ref {
                    file: f,
                    start,
                    end,
                    is_decl: false,
                    is_write: false,
                    shorthand: None,
                });
            }
        }
    }
}

/// Whether `l` is the name of a JSX attribute.
fn is_jsx_attr_name(c: &Checker<'_>, l: Loc) -> bool {
    c.parent(l)
        .is_some_and(|p| matches!(*c.node(p), Node::JsxAttr { name, .. } if name == l.node))
}

/// The string literal (or substitution-free template) at `offset`.
fn string_literal_at(cx: &Ctx<'_, '_>, offset: u32) -> Option<NodeId> {
    let ast = cx.checker.ast(cx.file);
    let n = node_at(ast, offset)?;
    let n = match ast.node(n) {
        Node::TemplateElement { .. } => cx.checker.file(cx.file).parents().parent(n)?,
        _ => n,
    };
    super::string_refs::literal_text(ast, n).map(|_| n)
}

/// References from a string literal that names no symbol ([`super::string_refs`]).
fn string_literal_references(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    offset: u32,
    scope: Scope,
) -> Option<Vec<Ref>> {
    let n = string_literal_at(cx, offset)?;
    let files: Vec<FileId> = match scope {
        Scope::File => vec![cx.file],
        Scope::Project => (0..cx.checker.files.len())
            .map(|i| FileId(i as u32))
            .collect(),
    };
    let spans = super::string_refs::references(ck, Loc::new(cx.file, n), &files);
    Some(
        spans
            .into_iter()
            .map(|(file, start, end)| Ref {
                file,
                start,
                end,
                is_decl: false,
                is_write: false,
                shorthand: None,
            })
            .collect(),
    )
}

/// References from a name in a doc comment path at `offset`: those of the declaration it
/// refers to (`None` when the offset is on no such name).
fn link_references(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    offset: u32,
    scope: Scope,
    rename: bool,
) -> Option<Vec<Ref>> {
    let (_, _, s) = super::doc_links::at(ck, cx.file, offset)?;
    let c = ck.c;
    let s = c.resolve_alias(s).unwrap_or(s);
    let d = c.first_decl(s)?;
    let name = ts_r_binder::decl_name_node(c.ast(d.file), d.node)?;
    let mut target = Target::at(ck, Loc::new(d.file, name), rename)?;
    collect_alias_names(cx, &mut target, scope, rename);
    Some(scan_target(cx, ck, &mut target, scope))
}

/// Fills [`Target::alias_names`] (not for a rename: renaming `a` keeps `{ a as b }`'s `b`).
fn collect_alias_names(cx: &Ctx<'_, '_>, target: &mut Target, scope: Scope, rename: bool) {
    if rename {
        return;
    }
    let files: Vec<FileId> = match scope {
        Scope::File => vec![cx.file],
        Scope::Project => project_files(cx).collect(),
    };
    target.alias_names =
        super::aliases::alias_names(cx.checker, &files, target.symbol(), &target.name);
}

/// References from the `default` keyword of `export default ...` (`decl`): the keyword,
/// and the imports of that default export with their uses (for an exported declaration,
/// not its local uses).
fn default_keyword_references(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    decl: Loc,
    keyword: (u32, u32),
    scope: Scope,
    rename: bool,
) -> Vec<Ref> {
    let c = cx.checker;
    let Some(s) = c.sym_of_decl(decl) else {
        return Vec::new();
    };
    let is_alias = super::aliases::is_alias(c, s);
    let mut target = Target {
        name: c.sym_name(s).to_string(),
        syms: vec![s],
        decls: HashSet::new(),
        ancestors: HashSet::new(),
        related: Default::default(),
        meaning: ts_r_binder::meaning::ALL,
        alias: is_alias.then_some(s),
        imports_only: !is_alias,
        alias_names: Vec::new(),
    };
    collect_alias_names(cx, &mut target, scope, rename);
    let mut out = scan_target(cx, ck, &mut target, scope);
    out.push(Ref {
        file: cx.file,
        start: keyword.0,
        end: keyword.1,
        is_decl: true,
        is_write: false,
        shorthand: None,
    });
    out.sort_by_key(|r| (r.file.0, r.start));
    out.dedup_by_key(|r| (r.file.0, r.start));
    out
}

/// The files a project-wide search looks at.
fn project_files(cx: &Ctx<'_, '_>) -> impl Iterator<Item = FileId> {
    let c = cx.checker;
    (0..c.files.len())
        .map(|i| FileId(i as u32))
        .filter(move |&f| {
            let sf = c.file(f);
            !sf.is_lib && !sf.path.contains("/node_modules/")
        })
}

/// References that do not come from a symbol: `this`, `super`, labels and primitive type
/// keywords (`None` when the cursor is on none of them).
fn special_references(cx: &Ctx<'_, '_>, offset: u32, scope: Scope) -> Option<Vec<Ref>> {
    let c = cx.checker;
    let d = Doc::new(c, cx.file);
    let mut decl = None;
    let occ = match keywords::special_at(&d, offset)? {
        Special::This(n) => {
            let others: Vec<Doc<'_>> = match scope {
                Scope::File => Vec::new(),
                Scope::Project => project_files(cx)
                    .filter(|&f| f != cx.file)
                    .map(|f| Doc::new(c, f))
                    .collect(),
            };
            keywords::this_refs(&d, n, &others)
        }
        Special::Super(n) => keywords::super_refs(&d, n),
        Special::Label(Some(l)) => {
            let v = keywords::label_refs(&d, l);
            decl = v.first().copied();
            v
        }
        Special::Label(None) => Vec::new(),
        Special::Primitive(kind) => match scope {
            Scope::File => keywords::primitive_refs(&d, kind),
            Scope::Project => project_files(cx)
                .flat_map(|f| keywords::primitive_refs(&Doc::new(c, f), kind))
                .collect(),
        },
    };
    let mut out: Vec<Ref> = occ
        .into_iter()
        .map(|o| Ref {
            file: o.0,
            start: o.1,
            end: o.2,
            is_decl: Some(o) == decl,
            is_write: false,
            shorthand: None,
        })
        .collect();
    out.sort_by_key(|r| (r.file.0, r.start));
    out.dedup_by_key(|r| (r.file.0, r.start));
    Some(out)
}

/// What `primary` is for a shorthand name: the property for `{ x }`, the variable for
/// `const { x } = o`.
fn primary_side(ck: &Ck<'_, '_>, l: Loc) -> ShorthandSide {
    let c = ck.c;
    match c.parent(l).map(|p| c.node(p)) {
        Some(Node::ShorthandProp { .. }) => ShorthandSide::Property,
        _ => ShorthandSide::Variable,
    }
}

fn other(s: ShorthandSide) -> ShorthandSide {
    match s {
        ShorthandSide::Property => ShorthandSide::Variable,
        ShorthandSide::Variable => ShorthandSide::Property,
    }
}

/// Whether the name is assigned (`x = 1`, `x++`, `this.x += 1`).
fn is_write(ck: &Ck<'_, '_>, l: Loc) -> bool {
    let c = ck.c;
    let Some(mut p) = c.parent(l) else {
        return false;
    };
    let mut at = l.node;
    if let Node::Member { prop, .. } = *c.node(p)
        && prop == l.node
    {
        at = p.node;
        let Some(pp) = c.parent(p) else { return false };
        p = pp;
    }
    match *c.node(p) {
        Node::Assign { left, .. } => left == at,
        Node::Update { .. } => true,
        _ => false,
    }
}

/// Whether a declaration counts as a write (tsc 7's highlight kind): one that gives a value.
/// Signatures, constructors and fields or variables without an initializer are reads.
fn decl_is_write(ck: &Ck<'_, '_>, l: Loc) -> bool {
    let c = ck.c;
    let Some(p) = c.parent(l) else { return true };
    match *c.node(p) {
        Node::PropertySignature { .. } | Node::MethodSignature { .. } => false,
        Node::PropertyDef { value, .. } => value.is_some(),
        Node::MethodDef { kind, .. } => kind != MethodKind::Constructor,
        // `for (const x of xs)` assigns each element.
        Node::VarDeclarator { init: None, .. } => c
            .parent(p)
            .and_then(|d| c.parent(d))
            .is_some_and(|n| matches!(c.node(n), Node::ForOf { .. } | Node::ForIn { .. })),
        _ => true,
    }
}

/// LSP locations of byte ranges.
pub fn locations(cx: &Ctx<'_, '_>, spans: impl Iterator<Item = (FileId, u32, u32)>) -> Value {
    let mut lc = LinesCache::default();
    Value::Array(
        spans
            .map(|(f, s, e)| lc.location(cx.checker, f, s, e))
            .collect(),
    )
}

/// Why a symbol cannot be renamed (tsserver's messages).
fn rename_blocker(ck: &Ck<'_, '_>, s: SymId) -> Option<&'static str> {
    let c = ck.c;
    let s = c.resolve_alias(s).unwrap_or(s);
    for d in c.decls(s) {
        let f = c.file(d.file);
        if f.is_lib {
            return Some(
                "You cannot rename elements that are defined in the standard TypeScript library.",
            );
        }
        if f.path.contains("/node_modules/") {
            return Some("You cannot rename elements that are defined in a 'node_modules' folder.");
        }
    }
    None
}

/// Whether the cursor is on a label (of a labelled statement, `break` or `continue`).
fn is_label(cx: &Ctx<'_, '_>, offset: u32) -> bool {
    let d = Doc::new(cx.checker, cx.file);
    matches!(
        keywords::special_at(&d, offset),
        Some(Special::Label(Some(_)))
    )
}

/// The name a rename at `offset` starts from: the name or literal key there, else the
/// declared name a declaration keyword or modifier stands for (`class C`, `get x`).
fn rename_name_at(cx: &Ctx<'_, '_>, offset: u32) -> Option<NodeId> {
    let ast = cx.checker.ast(cx.file);
    ident_at(ast, offset)
        .or_else(|| literal_key_at(cx, offset))
        .or_else(|| keywords::keyword_name(&Doc::new(cx.checker, cx.file), offset))
}

/// Whether `n` is `default` in an import or export specifier (`export { a as default }`,
/// `import { default as b }`), which cannot be renamed.
fn is_default_specifier_name(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    name_text(ast, n) == Some("default")
        && c.file(cx.file).parents().parent(n).is_some_and(|p| {
            matches!(
                ast.node(p),
                Node::ImportSpecifier { .. } | Node::ExportSpecifier { .. }
            )
        })
}

pub fn prepare_rename(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    lines: &LspLines,
    offset: u32,
) -> Result<Value, (i64, String)> {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let text = c.file(cx.file).text.as_str();
    let n = rename_name_at(cx, offset);
    if n.is_some_and(|n| is_default_specifier_name(cx, n)) {
        return Ok(Value::Null);
    }
    let Some(n) = n else {
        // A string literal that names no symbol renames the literals of its type.
        if let Some(lit) = string_literal_at(cx, offset) {
            if !super::string_refs::renamable(ck, Loc::new(cx.file, lit)) {
                return Ok(Value::Null);
            }
            let (start, end) = super::string_refs::inner_span(ast, lit);
            return Ok(json!({
                "range": lines.range(text, start, end),
                "placeholder": text.get(start as usize..end as usize).unwrap_or(""),
            }));
        }
        // A name in a doc comment path renames its declaration.
        let Some((start, end, s)) = super::doc_links::at(ck, cx.file, offset) else {
            return Ok(Value::Null);
        };
        if let Some(why) = rename_blocker(ck, s) {
            return Err((codes::REQUEST_FAILED, why.to_string()));
        }
        return Ok(json!({
            "range": lines.range(text, start, end),
            "placeholder": &text[start as usize..end as usize],
        }));
    };
    if !is_label(cx, offset) {
        let m = meaning_at(ck, Loc::new(cx.file, n));
        let Some(s) = m.primary.or(m.shorthand) else {
            return Ok(Value::Null);
        };
        if let Some(why) = rename_blocker(ck, s) {
            return Err((codes::REQUEST_FAILED, why.to_string()));
        }
    }
    let span = ast.span(n);
    Ok(json!({
        "range": lines.range(text, span.start, span.end),
        "placeholder": span.text(text),
    }))
}

pub fn rename(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    offset: u32,
    new_name: &str,
) -> Result<Value, (i64, String)> {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let on_this = match rename_name_at(cx, offset) {
        Some(n) if is_default_specifier_name(cx, n) => return Ok(Value::Null),
        Some(n) => {
            if !is_label(cx, offset) && !is_jsx_attr_name(c, Loc::new(cx.file, n)) {
                let m = meaning_at(ck, Loc::new(cx.file, n));
                let Some(s) = m.primary.or(m.shorthand) else {
                    return Ok(Value::Null);
                };
                if let Some(why) = rename_blocker(ck, s) {
                    return Err((codes::REQUEST_FAILED, why.to_string()));
                }
            }
            ast.span(n).text(c.file(cx.file).text.as_str()) == "this"
        }
        // A string literal that names no symbol.
        None => {
            if let Some(lit) = string_literal_at(cx, offset) {
                if !super::string_refs::renamable(ck, Loc::new(cx.file, lit)) {
                    return Ok(Value::Null);
                }
                // Every reference is a literal's text (`"this"` included), never the
                // `this` keyword a class rename leaves alone.
                true
            } else {
                // A name in a doc comment path renames its declaration.
                let Some((_, _, s)) = super::doc_links::at(ck, cx.file, offset) else {
                    return Ok(Value::Null);
                };
                if let Some(why) = rename_blocker(ck, s) {
                    return Err((codes::REQUEST_FAILED, why.to_string()));
                }
                false
            }
        }
    };
    let refs = references_with(cx, ck, offset, Scope::Project, true);
    // The symbol renamed before aliases are followed: name-keeping specifiers stop the
    // rename there ([`super::rename_alias`]).
    let renamed = rename_name_at(cx, offset)
        .and_then(|n| super::rename_alias::rename_symbol(ck, Loc::new(cx.file, n), false));
    let mut lc = LinesCache::default();
    let mut changes: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for r in refs {
        let f = c.file(r.file);
        let old = &f.text[r.start as usize..r.end as usize];
        // `this` in a static method refers to the class but is not renamed with it.
        if old == "this" && !on_this {
            continue;
        }
        let decor = match renamed {
            Some(s) => {
                let variable = r.shorthand == Some(ShorthandSide::Variable);
                match super::rename_alias::decor(ck, s, r.file, (r.start, r.end), variable) {
                    Some(d) => d,
                    None => continue,
                }
            }
            None => Decor::Plain,
        };
        let new_text = match r.shorthand {
            None if decor == Decor::Prefix => format!("{old} as {new_name}"),
            None if decor == Decor::Suffix => format!("{new_name} as {old}"),
            // `{ x }` -> `{ x: y }` when the variable is renamed, `{ y: x }` for the property.
            Some(ShorthandSide::Variable) => format!("{old}: {new_name}"),
            Some(ShorthandSide::Property) => format!("{new_name}: {old}"),
            // A numeric index (`o[0]`) becomes a string one; a numeric key stays bare.
            None if old.starts_with(|ch: char| ch.is_ascii_digit())
                && !new_name.starts_with(|ch: char| ch.is_ascii_digit())
                && is_numeric_index(c, r.file, r.start) =>
            {
                format!("\"{new_name}\"")
            }
            None => new_name.to_string(),
        };
        changes
            .entry(crate::uri::path_to_uri(&f.path))
            .or_default()
            .push(json!({"range": lc.range(c, r.file, r.start, r.end), "newText": new_text}));
    }
    Ok(json!({"changes": changes}))
}
