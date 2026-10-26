//! Enums and const enums.
//!
//! ```js
//! var E;
//! (function (E) {
//!     E[E["A"] = 0] = "A";          // numeric: reverse mapping
//!     E["S"] = "s";                 // string: none
//!     E[E["B"] = 20] = "B";         // constant-folded initializer
//!     E[E["G"] = foo(E.A)] = "G";   // computed: member references go through `E`
//! })(E || (E = {}));
//! ```
//! Const enums are erased (unless `preserveConstEnums`) and their member accesses in this
//! file are replaced by the value, followed by tsc's `/* E.A */` comment (the access as written),
//! kept in `Ast::synthetic_comments`.

use rustc_hash::FxHashMap;
use ts_r_ast::{Atom, BinaryOp, Modifiers, Node, NodeId, NodeKind, UnaryOp, VarKind};

use crate::builder::{Build, replace};
use crate::scope::BindingKind;
use crate::transform::Tx;

/// A constant enum member value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Num(f64),
    Str(String),
}

/// Evaluated members of one enum (all declarations merged).
#[derive(Clone, Debug, Default)]
pub(crate) struct EnumInfo {
    pub values: FxHashMap<String, Value>,
    /// Non-constant members whose initializer is syntactically a string.
    pub string_members: rustc_hash::FxHashSet<String>,
}

/// Evaluates every const enum up front: uses may precede the declaration.
pub(crate) fn prepass(tx: &mut Tx) {
    let ids: Vec<NodeId> = (1..tx.ast.nodes.len() as u32)
        .filter_map(NodeId::from_raw)
        .filter(|&i| matches!(tx.n(i), Node::Enum { modifiers, .. } if modifiers.contains(Modifiers::CONST)))
        .collect();
    for id in ids {
        let Node::Enum { name, members, .. } = tx.n(id) else {
            continue;
        };
        let Some(b) = tx.sc.decl_of.get(&name).copied() else {
            continue;
        };
        let b = tx.sc.enum_key(b);
        let enum_name = tx.name(name).to_owned();
        let mut info = tx.enums.get(&b).cloned().unwrap_or_default();
        let mut next: Option<f64> = Some(0.0);
        for m in tx.items(members) {
            let Node::EnumMember { name: mname, init } = tx.n(m) else {
                continue;
            };
            let member = member_name(tx, mname);
            let value = match init {
                Some(i) => eval(tx, i, &info, &enum_name),
                None => next.map(Value::Num),
            };
            next = match &value {
                Some(Value::Num(v)) => Some(v + 1.0),
                _ => None,
            };
            if let Some(v) = value {
                info.values.insert(member, v);
            }
        }
        tx.enums.insert(b, info);
    }
}

pub(crate) fn enum_decl(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::Enum {
        modifiers,
        name,
        members,
    } = tx.n(id)
    else {
        return;
    };
    let binding = tx.sc.decl_of.get(&name).copied();
    let is_const = modifiers.contains(Modifiers::CONST);
    let name_atom = tx.atom_of(name).unwrap_or(Atom::EMPTY);
    let enum_name = tx.ast.atom(name_atom).to_owned();
    let at = tx.at(id);
    let span = tx.span(id);
    let name_span = tx.span(name);
    let member_scope = tx.sc.enum_scope_of.get(&id).copied();
    if let Some(s) = member_scope {
        tx.enum_params.insert(s, name_atom);
    }

    // Evaluate members (needed for const enum inlining even when the enum is erased).
    let key = binding.map(|b| tx.sc.enum_key(b));
    let mut info = key
        .filter(|_| !is_const)
        .and_then(|b| tx.enums.get(&b).cloned())
        .unwrap_or_default();
    let mut next: Option<f64> = Some(0.0);
    let mut body = Vec::new();
    let erase = is_const && !tx.opts.preserve_const_enums;
    for m in tx.items(members) {
        let Node::EnumMember { name: mname, init } = tx.n(m) else {
            continue;
        };
        let member = member_name(tx, mname);
        let mat = tx.at(m);
        let value = match init {
            Some(i) => eval(tx, i, &info, &enum_name).or_else(|| resolver_member_value(tx, m)),
            None => next.map(Value::Num),
        };
        next = match &value {
            Some(Value::Num(v)) => Some(v + 1.0),
            _ => None,
        };
        if let Some(v) = &value {
            info.values.insert(member.clone(), v.clone());
        }
        if value.is_none() && init.is_some_and(|i| is_string_expr(tx, i, &info, &enum_name)) {
            info.string_members.insert(member.clone());
        }
        if erase {
            continue;
        }
        // `E[E["A"] = 0] = "A";` / `E["S"] = "s";`
        let value_expr = match &value {
            Some(Value::Num(v)) => tx.number(*v, mat),
            Some(Value::Str(s)) => tx.string(s, mat),
            None => match init {
                Some(i) => {
                    tx.expr(i);
                    i
                }
                // `enum E { A = "x", B }`: TS1061, and tsc emits `void 0`.
                None => tx.void0(mat),
            },
        };
        let is_string =
            matches!(value, Some(Value::Str(_))) || info.string_members.contains(&member);
        // A computed name other than a string or number (an error) is used as is: `E[E[e] =
        // 1] = e`
        // (observed).
        let computed = match tx.n(mname) {
            Node::ComputedKey { expr }
                if !matches!(tx.n(expr), Node::StringLit { .. } | Node::NumberLit { .. }) =>
            {
                Some(expr)
            }
            _ => None,
        };
        let e1 = tx.ident_atom(name_atom, mat);
        let key = match computed {
            Some(e) => e,
            None => tx.string(&member, mat),
        };
        let inner_target = tx.index(e1, key, mat);
        let inner = tx.assign(inner_target, value_expr, mat);
        let stmt_expr = if is_string {
            inner
        } else {
            let e2 = tx.ident_atom(name_atom, mat);
            let outer_target = tx.index(e2, inner, mat);
            let rev = match computed {
                Some(e) => crate::builder::clone_node(tx.ast, e),
                None => tx.string(&member, mat),
            };
            tx.assign(outer_target, rev, mat)
        };
        let s = tx.expr_stmt(stmt_expr, mat);
        tx.ast.set_span(s, ts_r_span::Span::new(mat, mat));
        // Source maps (tsc's): a member's statement and its outer assignment map to it.
        let mspan = tx.span(m);
        tx.map_range(s, mspan);
        tx.map_range(stmt_expr, mspan);
        body.push(s);
    }
    if let Some(k) = key {
        tx.enums.insert(k, info);
    }
    if erase {
        tx.erase(id);
        return;
    }

    let parent = tx.ns_stack.last().copied();
    let exported = modifiers.contains(Modifiers::EXPORT);
    let top_level = parent.is_none() && tx.list_depth == 0;
    let first_decl = binding.is_none_or(|b| crate::namespace::first_declaration(tx, b));
    if first_decl {
        let kind = if top_level {
            VarKind::Var
        } else {
            VarKind::Let
        };
        let mut mods = if exported && top_level && !tx.opts.is_cjs() {
            Modifiers::EXPORT
        } else {
            Modifiers::NONE
        };
        // The `var` takes the enum's JavaScript modifiers: a misplaced `accessor enum E {}`
        // is `accessor var E;` (autoAccessorDisallowedModifiers, tsc 7.0.2).
        if modifiers.contains(Modifiers::ACCESSOR) {
            mods.insert(Modifiers::ACCESSOR);
        }
        let n = tx.ident_atom(name_atom, at);
        let v = tx.var(kind, mods, n, None, at);
        tx.ast.set_span(v, ts_r_span::Span::new(at, at));
        // Source maps (tsc's): the declaration list maps to the enum (through its declarator,
        // see `PrintHints::map_ranges`), the name to the enum's name.
        if let Node::VarDecl { decls, .. } = tx.n(v)
            && let Some(&d) = tx.ast.list(decls).first()
        {
            tx.map_range(d, span);
        }
        tx.map_range(n, name_span);
        out.push(v);
    }
    let arg = match parent {
        Some(p) if exported => {
            // `E = N.E || (N.E = {})`
            let left = tx.ident_atom(name_atom, at);
            let pobj = tx.ident_atom(p.param, at);
            let pm = tx.member(pobj, &enum_name, at);
            let pobj2 = tx.ident_atom(p.param, at);
            let pm2 = tx.member(pobj2, &enum_name, at);
            let empty = tx.object(&[], at);
            let a = tx.assign(pm2, empty, at);
            let pa = tx.paren(a, at);
            let or = tx.binary(pm, BinaryOp::LogicalOr, pa, at);
            crate::namespace::map_names(tx, &[left, pm, pm2], name_span);
            tx.assign(left, or, at)
        }
        _ => {
            let l = tx.ident_atom(name_atom, at);
            let n2 = tx.ident_atom(name_atom, at);
            let empty = tx.object(&[], at);
            let mut rhs = tx.assign(n2, empty, at);
            if exported && top_level && tx.opts.is_cjs() {
                let e = tx.ident("exports", at);
                let em = tx.member(e, &enum_name, at);
                if let Node::Member { prop, .. } = tx.n(em) {
                    tx.map_range(prop, name_span);
                }
                rhs = tx.assign(em, rhs, at);
            } else if top_level && tx.opts.is_cjs() && !is_const {
                for alias in crate::cjs::list_aliases(tx, name_atom) {
                    let e = tx.ident("exports", at);
                    let em = tx.member_or_index(e, &alias, at);
                    rhs = tx.assign(em, rhs, at);
                }
            }
            let pa = tx.paren(rhs, at);
            crate::namespace::map_names(tx, &[l, n2], name_span);
            tx.binary(l, BinaryOp::LogicalOr, pa, at)
        }
    };
    let s = crate::namespace::iife(tx, name_atom, &body, arg, at);
    crate::namespace::map_iife(tx, s, span, name_span, span);
    out.push(s);
}

fn member_name(tx: &Tx, name: NodeId) -> String {
    match tx.n(name) {
        Node::Ident { name } | Node::StringLit { value: name } => tx.ast.atom(name).to_owned(),
        Node::ComputedKey { expr } => match tx.n(expr) {
            Node::StringLit { value } => tx.ast.atom(value).to_owned(),
            Node::TemplateLit { parts } => tx
                .ast
                .list(parts)
                .first()
                .and_then(|&p| match tx.n(p) {
                    Node::TemplateElement { cooked, .. } => Some(tx.ast.atom(cooked).to_owned()),
                    _ => None,
                })
                .unwrap_or_default(),
            _ => String::new(),
        },
        Node::NumberLit { value } => ts_r_codegen::literals::js_number(value.get()),
        _ => String::new(),
    }
}

/// tsc's "syntactically string" test for non-constant initializers: string and template
/// literals, string members of this enum, and `+` with a string operand.
fn is_string_expr(tx: &Tx, e: NodeId, own: &EnumInfo, own_name: &str) -> bool {
    match tx.n(e) {
        Node::StringLit { .. } | Node::TemplateLit { .. } => true,
        Node::Paren { expr } => is_string_expr(tx, expr, own, own_name),
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => is_string_expr(tx, left, own, own_name) || is_string_expr(tx, right, own, own_name),
        Node::Member { .. } | Node::Index { .. } | Node::Ident { .. } => {
            if matches!(eval(tx, e, own, own_name), Some(Value::Str(_))) {
                return true;
            }
            let name = match tx.n(e) {
                Node::Ident { name } => Some(tx.ast.atom(name).to_owned()),
                Node::Member { obj, prop, .. } if tx.name(obj) == own_name => {
                    Some(tx.name(prop).to_owned())
                }
                Node::Index { obj, index, .. } if tx.name(obj) == own_name => {
                    Some(tx.name(index).to_owned())
                }
                _ => None,
            };
            name.is_some_and(|n| own.string_members.contains(&n))
        }
        _ => false,
    }
}

/// Constant-folds an enum initializer. Identifiers refer to earlier members of this enum;
/// `E.A`/`E["A"]` may refer to any evaluated enum of the file.
fn eval(tx: &Tx, e: NodeId, own: &EnumInfo, own_name: &str) -> Option<Value> {
    match tx.n(e) {
        Node::NumberLit { value } => Some(Value::Num(value.get())),
        Node::StringLit { value } => Some(Value::Str(tx.ast.atom(value).to_owned())),
        Node::TemplateLit { parts } => {
            let mut s = String::new();
            for (i, &p) in tx.ast.list(parts).iter().enumerate() {
                if i % 2 == 0 {
                    let Node::TemplateElement { cooked, .. } = tx.n(p) else {
                        return None;
                    };
                    s.push_str(tx.ast.atom(cooked));
                } else {
                    match eval(tx, p, own, own_name)? {
                        Value::Str(v) => s.push_str(&v),
                        Value::Num(v) => s.push_str(&ts_r_codegen::literals::js_number(v)),
                    }
                }
            }
            Some(Value::Str(s))
        }
        Node::Paren { expr } => eval(tx, expr, own, own_name),
        Node::Ident { name } => {
            let n = tx.ast.atom(name);
            match n {
                "Infinity" if tx.sc.binding_of_ref(e).is_none() => Some(Value::Num(f64::INFINITY)),
                "NaN" if tx.sc.binding_of_ref(e).is_none() => Some(Value::Num(f64::NAN)),
                // Members of this enum, including those of earlier declarations of the same
                // (merged) enum, come before outer names (observed: `declare let y` loses to
                // member `y`).
                _ => own
                    .values
                    .get(n)
                    .cloned()
                    .or_else(|| const_value(tx, e, own, own_name)),
            }
        }
        Node::Member { obj, prop, .. } => {
            let info = enum_of(tx, obj, own, own_name)?;
            let key = tx.name(prop);
            info.values.get(key).cloned()
        }
        Node::Index { obj, index, .. } => {
            let info = enum_of(tx, obj, own, own_name)?;
            let Node::StringLit { value } = tx.n(index) else {
                return None;
            };
            info.values.get(tx.ast.atom(value)).cloned()
        }
        Node::Unary { op, arg } => {
            let Value::Num(v) = eval(tx, arg, own, own_name)? else {
                return None;
            };
            Some(Value::Num(match op {
                UnaryOp::Minus => -v,
                UnaryOp::Plus => v,
                UnaryOp::BitNot => !to_i32(v) as f64,
                _ => return None,
            }))
        }
        Node::Binary { left, op, right } => {
            let l = eval(tx, left, own, own_name)?;
            let r = eval(tx, right, own, own_name)?;
            match (l, r) {
                (Value::Num(a), Value::Num(b)) => Some(Value::Num(match op {
                    BinaryOp::Add => a + b,
                    BinaryOp::Sub => a - b,
                    BinaryOp::Mul => a * b,
                    BinaryOp::Div => a / b,
                    BinaryOp::Mod => a % b,
                    BinaryOp::Exp => a.powf(b),
                    BinaryOp::BitOr => (to_i32(a) | to_i32(b)) as f64,
                    BinaryOp::BitAnd => (to_i32(a) & to_i32(b)) as f64,
                    BinaryOp::BitXor => (to_i32(a) ^ to_i32(b)) as f64,
                    BinaryOp::Shl => to_i32(a).wrapping_shl(to_u32(b) & 31) as f64,
                    BinaryOp::Shr => to_i32(a).wrapping_shr(to_u32(b) & 31) as f64,
                    BinaryOp::UShr => to_u32(a).wrapping_shr(to_u32(b) & 31) as f64,
                    _ => return None,
                })),
                (Value::Str(a), Value::Str(b)) if op == BinaryOp::Add => Some(Value::Str(a + &b)),
                (Value::Str(a), Value::Num(b)) if op == BinaryOp::Add => {
                    Some(Value::Str(a + &ts_r_codegen::literals::js_number(b)))
                }
                (Value::Num(a), Value::Str(b)) if op == BinaryOp::Add => {
                    Some(Value::Str(ts_r_codegen::literals::js_number(a) + &b))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

thread_local! {
    /// Nesting of [`const_value`] (a chain of constants is bounded).
    static CONST_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The value of a reference to a `const` declared before it without a type annotation,
/// from its initializer (tsc: tsc/internal/checker/checker.go, `evaluateEntity`).
fn const_value(tx: &Tx, e: NodeId, own: &EnumInfo, own_name: &str) -> Option<Value> {
    let b = tx.sc.binding_of_ref(e)?;
    let decl = tx.sc.binding(b).decl;
    if tx.sc.binding(b).kind != BindingKind::Const || tx.span(decl).end > tx.span(e).start {
        return None;
    }
    let init = *tx.sc.const_inits.get(&decl)?;
    let depth = CONST_DEPTH.with(|d| d.get());
    if depth >= 64 {
        return None;
    }
    CONST_DEPTH.with(|d| d.set(depth + 1));
    // The initializer is outside the enum: its names are not members.
    let _ = (own, own_name);
    let v = eval(tx, init, &EnumInfo::default(), "");
    CONST_DEPTH.with(|d| d.set(depth));
    v
}

/// The evaluated enum an expression names (`E` or the enum being declared).
fn enum_of<'t>(tx: &'t Tx, obj: NodeId, own: &'t EnumInfo, own_name: &str) -> Option<&'t EnumInfo> {
    // `N.E.A`: an enum exported from a namespace.
    if let Node::Member { obj: ns, prop, .. } = tx.n(obj) {
        let ns = namespace_of(tx, ns)?;
        let Node::Ident { name } = tx.n(prop) else {
            return None;
        };
        let b = tx.sc.ns_enum(ns, name)?;
        return tx.enums.get(&tx.sc.enum_key(b));
    }
    let Node::Ident { name } = tx.n(obj) else {
        return None;
    };
    match tx.sc.binding_of_ref(obj) {
        Some(b)
            if matches!(
                tx.sc.binding(b).kind,
                BindingKind::Enum | BindingKind::ConstEnum
            ) =>
        {
            let b = tx.sc.enum_key(b);
            if tx.ast.atom(name) == own_name && !tx.enums.contains_key(&b) {
                Some(own)
            } else {
                tx.enums.get(&b).or(if tx.ast.atom(name) == own_name {
                    Some(own)
                } else {
                    None
                })
            }
        }
        _ => None,
    }
}

/// The namespace binding `N` or `N.M` names.
fn namespace_of(tx: &Tx, e: NodeId) -> Option<crate::scope::BindingId> {
    match tx.n(e) {
        Node::Ident { .. } => {
            let b = tx.sc.binding_of_ref(e)?;
            (tx.sc.binding(b).kind == BindingKind::Namespace).then_some(b)
        }
        Node::Member { obj, prop, .. } => {
            let ns = namespace_of(tx, obj)?;
            let Node::Ident { name } = tx.n(prop) else {
                return None;
            };
            tx.sc.ns_namespace(ns, name)
        }
        _ => None,
    }
}

fn to_i32(v: f64) -> i32 {
    if !v.is_finite() {
        return 0;
    }
    let m = v.trunc().rem_euclid(4294967296.0);
    m as u32 as i32
}

fn to_u32(v: f64) -> u32 {
    to_i32(v) as u32
}

/// `access` (on `obj`) through an import binding, answered by the [`crate::EmitResolver`].
fn imported_const_enum_value(tx: &mut Tx, access: NodeId, obj: NodeId) -> Option<Value> {
    let mut root = obj;
    let imported = loop {
        if let Some(&imported) = tx.enum_import_roots.get(&root) {
            break imported;
        }
        match tx.n(root) {
            Node::Member { obj, .. } => root = obj,
            _ => {
                // An import, or an `import x = N.M` alias (of a namespace another file
                // may declare: `mappa.E.A`, constEnumOnlyModuleImportAliasEmit).
                break tx.sc.binding_of_ref(root).is_some_and(|b| {
                    matches!(
                        tx.sc.binding(b).kind,
                        BindingKind::Import | BindingKind::ImportEquals
                    )
                });
            }
        }
    };
    // The visitor checks the outer access first. Cache every receiver so visiting its
    // children does not walk the same chain again.
    let mut root = obj;
    while let Node::Member { obj, .. } = tx.n(root) {
        if tx.enum_import_roots.insert(root, imported).is_some() {
            break;
        }
        root = obj;
    }
    if !imported {
        return None;
    }
    resolver_const_enum_value(tx, access)
}

fn resolver_const_enum_value(tx: &Tx, access: NodeId) -> Option<Value> {
    // Synthesized nodes are not the checker's.
    let span = tx.ast.span(access);
    if span.start >= span.end {
        return None;
    }
    match tx.resolver.const_enum_value(tx.file, access)? {
        crate::ConstValue::Number(n) => Some(Value::Num(n)),
        crate::ConstValue::String(s) => Some(Value::Str(s)),
    }
}

/// The program resolver's value for an enum member the file-local evaluator could not
/// fold (an initializer reading another file's enum). Only original members count.
fn resolver_member_value(tx: &Tx, member: NodeId) -> Option<Value> {
    let span = tx.ast.span(member);
    if span.start >= span.end {
        return None;
    }
    match tx.resolver.enum_member_value(tx.file, member)? {
        crate::ConstValue::Number(n) => Some(Value::Num(n)),
        crate::ConstValue::String(s) => Some(Value::Str(s)),
    }
}

/// Replaces `CE.A` / `CE["A"]` for a const enum of this file, or of another file through an
/// import, by its value. Returns true if it did.
pub(crate) fn inline_const_enum_access(tx: &mut Tx, id: NodeId) -> bool {
    let (obj, key) = match tx.n(id) {
        Node::Member { obj, prop, .. } => (obj, tx.name(prop).to_owned()),
        Node::Index { obj, index, .. } => match tx.n(index) {
            Node::StringLit { value } => (obj, tx.ast.atom(value).to_owned()),
            // ``E[`A`]`` (no substitutions) is inlined too, as in tsc.
            Node::TemplateLit { parts } => match *tx.ast.list(parts) {
                [only] => match tx.n(only) {
                    Node::TemplateElement { cooked, .. } => (obj, tx.ast.atom(cooked).to_owned()),
                    _ => return false,
                },
                _ => return false,
            },
            _ => return false,
        },
        _ => return false,
    };
    let v = if tx.kind(obj) == NodeKind::Ident
        && let Some(b) = tx.sc.binding_of_ref(obj)
        && tx.sc.binding(b).kind == BindingKind::ConstEnum
    {
        match tx
            .enums
            .get(&tx.sc.enum_key(b))
            .and_then(|i| i.values.get(&key))
            .cloned()
            .or_else(|| resolver_const_enum_value(tx, id))
        {
            Some(v) => v,
            None => return false,
        }
    } else {
        // A const enum of another file, reached through an import (`E.A`, `ns.E.A`): the
        // checker knows its value.
        match imported_const_enum_value(tx, id, obj) {
            Some(v) => v,
            None => return false,
        }
    };
    let at = tx.at(id);
    let lit = match v {
        Value::Num(n) => tx.number(n, at),
        Value::Str(s) => tx.string(&s, at),
    };
    // tsc follows the value with the access as written: `0 /* E.A */`, `5 /* E["B"] */`,
    // with a `*/` inside escaped as `*_/`.
    let span = tx.ast.span(id);
    let written = tx
        .src
        .get(span.start as usize..span.end as usize)
        .unwrap_or("");
    let comment = (!written.is_empty()).then(|| format!("/* {} */", written.replace("*/", "*_/")));
    tx.erase(id);
    replace(tx.ast, id, lit);
    if let Some(comment) = comment {
        tx.ast.add_synthetic_comment(id, &comment, true);
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::{TransformOptions, transpile};
    use ts_r_codegen::CodegenOptions;

    /// Inlined const enum values carry tsc's `/* E.A */` comment (R6 part 2). The expected
    /// text is tsc 7.0.2's output for this input (`--target esnext --module esnext`).
    #[test]
    fn const_enum_comments_match_tsc() {
        let src = r#"const enum E { A, B = 5, S = "s", N = -1, F = 1.5, Big = 1e21 }
declare function use(...a: any[]): void;
use(E.A, E["B"], E.S, E.N, E.F, E.Big);
use(E . A, E /* c */ .B);
const x = E.A + 1;
const y = E.N.toString();
const z = E.A.toString();
const w = { k: E.S };
use(E[`A`]);
"#;
        let tsc = r#""use strict";
use(0 /* E.A */, 5 /* E["B"] */, "s" /* E.S */, -1 /* E.N */, 1.5 /* E.F */, 1e+21 /* E.Big */);
use(0 /* E . A */, 5 /* E /* c *_/ .B */);
const x = 0 /* E.A */ + 1;
const y = (-1 /* E.N */).toString();
const z = 0 /* E.A */.toString();
const w = { k: "s" /* E.S */ };
use(0 /* E[`A`] */);
"#;
        let opts = TransformOptions::from_json("{}", "ce.ts");
        let out = transpile("ce.ts", src, &opts, &CodegenOptions::default());
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(out.code, tsc);
    }
}
