//! Members from types, as tsserver 6.0.3 writes them on the `fix-implement-*` and
//! `fix-missing-prop*` fixtures:
//!
//! - "Implement interface 'I'" (TS2420 at the class name): the interface's members the class
//!   lacks go at the start of the class body, last member first. Properties print as
//!   `name: T;` (`readonly` kept, optional ones `name?: T | undefined;`), methods as
//!   `name(params): R {` + `throw new Error("Method not implemented.");` + `}`.
//! - "Add missing properties" (TS2739/TS2740/TS2741 at the variable or the literal): the
//!   object literal is written again on several lines, its properties first, then each
//!   required property the type lacks with a default value: `""`, `0`, `0n`, `false`, `[]`,
//!   the first literal of a literal union, a nested literal for object types, a function
//!   throwing `"Function not implemented."` for function types, else `undefined`.
//!
//! Indentation is the file's indentation unit ([`super::style::indent_unit`]).

use super::Ctx;
use super::edit::Edit;
use super::fixes::{Diag, Fix, node_covering};
use super::style;
use ts_r_ast::{Ast, HeritageKind, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_types::{SymId, TypeFlags, TypeId};

/// A property name as written in a declaration.
fn prop_name(name: &str) -> String {
    if super::super::complete::is_identifier_name(name) || name.parse::<f64>().is_ok() {
        name.to_string()
    } else {
        ts_r_checker::print::quote_string(name)
    }
}

/// `Implement interface 'I'`, one fix per implemented interface with missing members.
pub fn implement_interface(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>) -> Vec<Fix> {
    implement(cx, ck, d, false)
}

/// One fix adding the missing members of every interface the class at `d` implements, each
/// name once (the fix-all form).
pub fn implement_all_interfaces(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    d: &Diag<'_>,
) -> Option<Fix> {
    implement(cx, ck, d, true).pop()
}

fn implement(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>, merged: bool) -> Vec<Fix> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Some(id) = super::super::locate::node_at(ast, d.start) else {
        return Vec::new();
    };
    let Some(class) = std::iter::once(id)
        .chain(f.parents().ancestors(id))
        .find(|&n| matches!(ast.node(n), Node::ClassDecl { .. } | Node::ClassExpr { .. }))
    else {
        return Vec::new();
    };
    let (Node::ClassDecl {
        heritage, members, ..
    }
    | Node::ClassExpr {
        heritage, members, ..
    }) = *ast.node(class)
    else {
        return Vec::new();
    };
    let Some(sym) = ck.c.sym_of_decl(Loc::new(cx.file, class)) else {
        return Vec::new();
    };
    let instance = ck.declared_type_of_symbol(sym);
    let mut have: Vec<String> = ck
        .properties_of(instance)
        .into_iter()
        .map(|(n, _)| ck.store().str(n).to_string())
        .collect();
    let Some(open) = body_open(ast, text, class, heritage, members) else {
        return Vec::new();
    };
    let unit = style::indent_unit(text);
    let class_indent = style::line_indent(text, ast.span(class).start).to_string();
    let member_indent = format!("{class_indent}{unit}");
    let nl = style::newline(text);
    let empty =
        ast.list(members).is_empty() && style::blank(text, open + 1, ast.span(class).end - 1);
    let fix = |ins: String, title: String| {
        let mut ins = ins;
        if empty {
            ins.push_str(nl);
            ins.push_str(&class_indent);
        }
        let edits = if empty {
            vec![Edit::new(open + 1, ast.span(class).end - 1, ins)]
        } else {
            vec![Edit::insert(open + 1, ins)]
        };
        Fix::new(title, "fixClassIncorrectlyImplementsInterface", edits)
    };
    let mut types: Vec<NodeId> = Vec::new();
    for &h in ast.list(heritage) {
        if let Node::HeritageClause {
            kind: HeritageKind::Implements,
            types: ts,
        } = *ast.node(h)
        {
            types.extend(ast.list(ts));
        }
    }
    // Merged: the last interface's members come first, as tsserver's fix-all inserts them.
    if merged {
        types.reverse();
    }
    let mut out = Vec::new();
    let mut all = String::new();
    for t in types {
        let it = ck.type_from_node(Loc::new(cx.file, t));
        let props = ck.properties_of(it);
        let mut printed: Vec<String> = Vec::new();
        for (n, p) in props {
            let name = ck.store().str(n).to_string();
            if have.contains(&name) {
                continue;
            }
            printed.push(member_text(ck, p, &name, &member_indent, &unit, nl));
            if merged {
                have.push(name);
            }
        }
        if printed.is_empty() {
            continue;
        }
        printed.reverse();
        let mut ins = String::new();
        for m in &printed {
            ins.push_str(nl);
            ins.push_str(&member_indent);
            ins.push_str(m);
        }
        if merged {
            all.push_str(&ins);
            continue;
        }
        let iname = match *ast.node(t) {
            Node::ExprWithTypeArgs { expr, .. } => ast.span(expr).text(text).to_string(),
            _ => ast.span(t).text(text).to_string(),
        };
        out.push(fix(ins, format!("Implement interface '{iname}'")));
    }
    if merged && !all.is_empty() {
        out.push(fix(
            all,
            "Implement all unimplemented interfaces".to_string(),
        ));
    }
    out
}

/// Offset of the class body's `{`.
fn body_open(
    ast: &Ast,
    text: &str,
    class: NodeId,
    heritage: ts_r_ast::NodeList,
    members: ts_r_ast::NodeList,
) -> Option<u32> {
    let sp = ast.span(class);
    let from = ast
        .list(heritage)
        .last()
        .map_or(sp.start, |&h| ast.span(h).end);
    let to = ast
        .list(members)
        .first()
        .map_or(sp.end, |&m| ast.span(m).start);
    let i = text.get(from as usize..to as usize)?.find('{')?;
    Some(from + i as u32)
}

/// One class member implementing property `p`.
fn member_text(
    ck: &mut Ck<'_, '_>,
    p: SymId,
    name: &str,
    indent: &str,
    unit: &str,
    nl: &str,
) -> String {
    let flags = ck.c.sym_flags(p);
    let optional = flags.contains(SymbolFlags::OPTIONAL);
    let q = if optional { "?" } else { "" };
    let name = prop_name(name);
    let t = ck.type_of_symbol(p);
    if flags.contains(SymbolFlags::METHOD) {
        let sigs = ck.signatures_of(t, SigKind::Call);
        let mut stack = Vec::new();
        let body = format!(
            " {{{nl}{indent}{unit}throw new Error(\"Method not implemented.\");{nl}{indent}}}"
        );
        if sigs.len() == 1 {
            let s = ck.print_signature(sigs[0], SigKind::Call, false, 0, &mut stack);
            return format!("{name}{q}{s}{body}");
        }
        let mut out = String::new();
        for &s in &sigs {
            let s = ck.print_signature(s, SigKind::Call, false, 0, &mut stack);
            out.push_str(&format!("{name}{q}{s};{nl}{indent}"));
        }
        out.push_str(&format!("{name}{q}(...args: any[]): any{body}"));
        return out;
    }
    let readonly = if ck.is_readonly_symbol(p) {
        "readonly "
    } else {
        ""
    };
    let ts = ck.type_to_string(t);
    format!("{readonly}{name}{q}: {ts};")
}

pub fn missing_properties(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, d: &Diag<'_>) -> Option<Fix> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let n = node_covering(cx, d.start, d.end)?;
    let (obj, annotation) = match *ast.node(n) {
        Node::ObjectLit { .. } => (n, None),
        Node::Ident { .. } => {
            let p = f.parents().parent(n)?;
            match *ast.node(p) {
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    type_ann,
                    ..
                } if name == n => (init, type_ann),
                _ => return None,
            }
        }
        _ => return None,
    };
    let Node::ObjectLit { props } = *ast.node(obj) else {
        return None;
    };
    let target = match annotation {
        Some(ty) => ck.type_from_node(Loc::new(cx.file, ty)),
        None => ck.contextual_type(Loc::new(cx.file, obj))?,
    };
    let mut have: Vec<String> = Vec::new();
    let mut existing: Vec<String> = Vec::new();
    for &p in ast.list(props) {
        existing.push(ast.span(p).text(text).trim().to_string());
        let key = match *ast.node(p) {
            Node::Property { key, .. } | Node::MethodDef { key, .. } => {
                ts_r_checker::nodes::key_text(ast, key)
            }
            Node::ShorthandProp { name, .. } => {
                super::super::locate::name_text(ast, name).map(str::to_string)
            }
            Node::Spread { arg } => {
                // Its properties are there (a new one after it would overwrite them); no fix
                // when they are unknown.
                have.extend(spread_names(ck, Loc::new(cx.file, arg))?);
                None
            }
            _ => None,
        };
        have.extend(key);
    }
    let unit = style::indent_unit(text);
    let outer = style::line_indent(text, ast.span(obj).start).to_string();
    let inner = format!("{outer}{unit}");
    let nl = style::newline(text);
    let mut lines: Vec<String> = existing;
    let mut added = 0;
    for (name, p) in ck.properties_of(target) {
        let name = ck.store().str(name).to_string();
        if have.contains(&name) || ck.c.sym_flags(p).contains(SymbolFlags::OPTIONAL) {
            continue;
        }
        let t = ck.type_of_symbol(p);
        let v = default_value(ck, t, &inner, &unit, nl, 0);
        lines.push(format!("{}: {v}", prop_name(&name)));
        added += 1;
    }
    if added == 0 {
        return None;
    }
    let body: Vec<String> = lines.iter().map(|l| format!("{inner}{l}")).collect();
    let printed = format!("{{{nl}{}{nl}{outer}}}", body.join(&format!(",{nl}")));
    let sp = ast.span(obj);
    Some(Fix::new(
        "Add missing properties",
        "fixMissingProperties",
        vec![Edit::new(sp.start, sp.end, printed)],
    ))
}

/// The property names an object spread of `arg` supplies (of any union member), or `None`
/// when its type does not tell (`any`, `unknown`, a type parameter).
fn spread_names(ck: &mut Ck<'_, '_>, arg: Loc) -> Option<Vec<String>> {
    let t = ck.check_expr(arg);
    let members = if ck.store().flags(t).contains(TypeFlags::UNION) {
        ck.union_members(t)
    } else {
        vec![t]
    };
    let mut names = Vec::new();
    for m in members {
        if ck
            .store()
            .flags(m)
            .intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::INSTANTIABLE)
        {
            return None;
        }
        let a = ck.apparent_type(m);
        for (name, _) in ck.properties_of(a) {
            names.push(ck.store().str(name).to_string());
        }
    }
    Some(names)
}

/// A value of type `t` for a new property (`indent`: the property's indentation).
fn default_value(
    ck: &mut Ck<'_, '_>,
    t: TypeId,
    indent: &str,
    unit: &str,
    nl: &str,
    depth: u32,
) -> String {
    let flags = ck.store().flags(t);
    if flags.intersects(TypeFlags::STRING | TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
    {
        return "\"\"".into();
    }
    if flags.contains(TypeFlags::NUMBER) {
        return "0".into();
    }
    if flags.contains(TypeFlags::BIGINT) {
        return "0n".into();
    }
    if flags.contains(TypeFlags::BOOLEAN) {
        return "false".into();
    }
    if flags.intersects(
        TypeFlags::STRING_LITERAL
            | TypeFlags::NUMBER_LITERAL
            | TypeFlags::BIGINT_LITERAL
            | TypeFlags::BOOLEAN_LITERAL
            | TypeFlags::ENUM_LITERAL,
    ) {
        return ck.type_to_string(t);
    }
    if flags.contains(TypeFlags::NULL) {
        return "null".into();
    }
    if flags.contains(TypeFlags::UNION) {
        let members = ck.union_members(t);
        // `boolean` inside a union is `true | false`.
        if members
            .iter()
            .any(|&m| ck.store().flags(m).contains(TypeFlags::BOOLEAN_LITERAL))
            && members.iter().all(|&m| {
                ck.store()
                    .flags(m)
                    .intersects(TypeFlags::BOOLEAN_LITERAL | TypeFlags::UNDEFINED | TypeFlags::NULL)
            })
        {
            return "false".into();
        }
        let first = members.iter().copied().find(|&m| {
            !ck.store()
                .flags(m)
                .intersects(TypeFlags::UNDEFINED | TypeFlags::NULL)
        });
        return match first {
            Some(m) => default_value(ck, m, indent, unit, nl, depth + 1),
            None => "undefined".into(),
        };
    }
    if flags.contains(TypeFlags::ES_SYMBOL) {
        return "Symbol()".into();
    }
    if flags.contains(TypeFlags::OBJECT) {
        if ck.c.is_array_type(t) || ck.c.is_tuple_type(t) || ck.is_readonly_array_like(t) {
            return "[]".into();
        }
        let sigs = ck.signatures_of(t, SigKind::Call);
        if let Some(&s) = sigs.first() {
            let mut stack = Vec::new();
            let sig = ck.print_signature(s, SigKind::Call, false, 0, &mut stack);
            return format!(
                "function{sig} {{{nl}{indent}{unit}throw new Error(\"Function not implemented.\");{nl}{indent}}}"
            );
        }
        if depth >= 3 || ck.is_class_instance_type(t) {
            return "undefined".into();
        }
        let inner = format!("{indent}{unit}");
        let mut parts = Vec::new();
        for (name, p) in ck.properties_of(t) {
            if ck.c.sym_flags(p).contains(SymbolFlags::OPTIONAL) {
                continue;
            }
            let name = ck.store().str(name).to_string();
            let pt = ck.type_of_symbol(p);
            let v = default_value(ck, pt, &inner, unit, nl, depth + 1);
            parts.push(format!("{inner}{}: {v}", prop_name(&name)));
        }
        if parts.is_empty() {
            return "{}".into();
        }
        return format!("{{{nl}{}{nl}{indent}}}", parts.join(&format!(",{nl}")));
    }
    "undefined".into()
}
