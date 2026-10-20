//! `@typescript-eslint/no-mixed-enums`: an enum's members are all numbers or all strings,
//! across every declaration it merges.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-mixed-enums",
        "Disallow enums from having both number and string members",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoMixedEnums>,
};

#[derive(Debug, Default)]
struct NoMixedEnums;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Number,
    String,
    Unknown,
}

const MESSAGE: &str = "Mixing number and string enums can be confusing.";

impl Rule for NoMixedEnums {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Enum]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Enum { name, members, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let list = ast.list(members).to_vec();
        if list.is_empty() {
            return;
        }
        let desired = match first_same_enum(cx, node, name) {
            Some(first) => member_kind(cx, first),
            None => merged_kind(cx, name).unwrap_or_else(|| member_kind(cx, list[0])),
        };
        if desired == Kind::Unknown {
            return;
        }
        for m in list {
            let k = member_kind(cx, m);
            if k != Kind::Unknown && k != desired {
                let Node::EnumMember { name, init } = *cx.node(m) else {
                    return;
                };
                cx.report_node(init.unwrap_or(name), MESSAGE).id("mixed");
                return;
            }
        }
    }
}

fn kind_of_type(ti: &dyn TypeInfo, t: Ty) -> Kind {
    if t == Ty::UNRESOLVED {
        return Kind::Unknown;
    }
    let f = ti.flags(t);
    // A computed member's type is the whole (non-literal) enum: its value kind is unknown.
    if f.intersects(TypeFlags::ANY)
        || (f.contains(TypeFlags::ENUM) && !f.intersects(TypeFlags::LITERAL))
    {
        Kind::Unknown
    } else if f.intersects(TypeFlags::STRING_LIKE) {
        Kind::String
    } else if f.intersects(TypeFlags::NUMBER_LIKE) {
        Kind::Number
    } else {
        Kind::Unknown
    }
}

fn member_kind(cx: &Ctx<'_, '_>, m: NodeId) -> Kind {
    let Node::EnumMember { init, .. } = *cx.node(m) else {
        return Kind::Unknown;
    };
    let Some(init) = init else {
        return Kind::Number;
    };
    let ti = cx.types();
    match ti.type_at(init) {
        Some(t) => kind_of_type(ti, t),
        None => Kind::Unknown,
    }
}

/// The namespace and module declarations around `node`, outermost first.
fn module_chain(cx: &Ctx<'_, '_>, node: NodeId) -> Vec<NodeId> {
    let mut chain = Vec::new();
    let mut cur = cx.parent(node);
    let mut depth = 0;
    while let Some(p) = cur {
        if matches!(cx.node(p), Node::Module { .. }) {
            chain.push(p);
        }
        depth += 1;
        if depth > 256 {
            break;
        }
        cur = cx.parent(p);
    }
    chain.reverse();
    chain
}

/// A declaration visible outside its namespace body: `export`, or the inner part of a
/// dotted name (`namespace A.B`).
fn exported(cx: &Ctx<'_, '_>, decl: NodeId) -> bool {
    match *cx.node(decl) {
        Node::Enum { modifiers, .. } | Node::Module { modifiers, .. }
            if modifiers.contains(Modifiers::EXPORT) =>
        {
            true
        }
        _ => cx
            .parent(decl)
            .is_some_and(|p| matches!(cx.node(p), Node::Module { .. })),
    }
}

/// `a` and `b` (enum declarations) merge: the same name in the same namespace path, where
/// everything below the first separate namespace body is exported.
fn merges(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let (ca, cb) = (module_chain(cx, a), module_chain(cx, b));
    if ca.len() != cb.len() {
        return false;
    }
    let src = cx.src();
    let mut split = false;
    for (&ma, &mb) in ca.iter().zip(&cb) {
        if split && !(exported(cx, ma) && exported(cx, mb)) {
            return false;
        }
        if ma != mb {
            let (Node::Module { name: na, .. }, Node::Module { name: nb, .. }) =
                (*cx.node(ma), *cx.node(mb))
            else {
                return false;
            };
            if cx.span(na).text(src) != cx.span(nb).text(src) {
                return false;
            }
            split = true;
        }
    }
    !split || (exported(cx, a) && exported(cx, b))
}

/// The first member of an earlier declaration of the same enum in this file, which merged
/// exports may not expose through the symbol.
fn first_same_enum(cx: &Ctx<'_, '_>, node: NodeId, name: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let src = cx.src();
    let name_text = cx.span(name).text(src);
    let start = cx.span(node).start;
    let mut best: Option<NodeId> = None;
    for raw in 1..ast.nodes.len() as u32 {
        let Some(id) = NodeId::from_raw(raw) else {
            continue;
        };
        let Node::Enum {
            name: n, members, ..
        } = *cx.node(id)
        else {
            continue;
        };
        if cx.span(id).start >= start
            || cx.span(n).text(src) != name_text
            || ast.list(members).is_empty()
            || best.is_some_and(|b| cx.span(b).start < cx.span(id).start)
        {
            continue;
        }
        if merges(cx, id, node) {
            best = Some(id);
        }
    }
    let Node::Enum { members, .. } = *cx.node(best?) else {
        return None;
    };
    ast.list(members).first().copied()
}

/// The kind of the first member of the merged enum (over all its declarations).
fn merged_kind(cx: &Ctx<'_, '_>, name: NodeId) -> Option<Kind> {
    let ti = cx.types();
    let sym = ti.symbol_at(name)?;
    let enum_obj = ti.type_of_symbol(sym);
    if enum_obj == Ty::UNRESOLVED || is_any(ti, enum_obj) {
        return None;
    }
    let first = *ti.properties_of(enum_obj).first()?;
    // A member of this file is judged by its initializer (a computed member's type is the
    // whole enum); others by their type.
    let k = match ti.symbol_decls(first).first() {
        Some(d) if d.in_this_file && matches!(cx.node(d.node), Node::EnumMember { .. }) => {
            member_kind(cx, d.node)
        }
        _ => kind_of_type(ti, ti.type_of_symbol(first)),
    };
    (k != Kind::Unknown).then_some(k)
}
