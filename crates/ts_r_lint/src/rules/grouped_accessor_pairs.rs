//! `grouped-accessor-pairs`: a getter and setter for the same key in an object literal or
//! class (static and instance members apart) sit next to each other, optionally in a fixed
//! order (`getBeforeSet`, `setBeforeGet`); with `enforceForTSTypes`, interface and type
//! literal accessor signatures too.
//!
//! Keys match by static name (identifiers, literals, constant computed keys), else by
//! their tokens (`[a]`, private names). A key with more than one getter or setter is not
//! checked. The report is on the latter accessor's head.

use std::borrow::Cow;

use super::prelude::*;
use ts_r_ast::{Ast, NodeList};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "grouped-accessor-pairs",
        "Require grouped accessor pairs in object literals and classes",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<GroupedAccessorPairs>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Order {
    #[default]
    Any,
    GetFirst,
    SetFirst,
}

#[derive(Debug, Default)]
struct GroupedAccessorPairs {
    order: Order,
    ts_types: bool,
}

impl FromOptions for GroupedAccessorPairs {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let order = match opts.first().and_then(Value::as_str) {
            None | Some("anyOrder") => Order::Any,
            Some("getBeforeSet") => Order::GetFirst,
            Some("setBeforeGet") => Order::SetFirst,
            Some(o) => return Err(format!("grouped-accessor-pairs: unknown order {o:?}")),
        };
        let ts_types = opts
            .get(1)
            .and_then(|o| o.get("enforceForTSTypes"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(GroupedAccessorPairs { order, ts_types })
    }
}

/// An accessor's key, as the rule compares them.
enum Key<'a> {
    Name(Cow<'a, str>),
    /// Compared by tokens: a computed key's expression, or a private name.
    Tokens(NodeId),
}

fn same_key(ast: &Ast, src: &str, a: &Key<'_>, b: &Key<'_>) -> bool {
    match (a, b) {
        (Key::Name(x), Key::Name(y)) => x == y,
        (Key::Tokens(x), Key::Tokens(y)) => match (ast.node(*x), ast.node(*y)) {
            (Node::PrivateName { name: n }, Node::PrivateName { name: m }) => n == m,
            (Node::PrivateName { .. }, _) | (_, Node::PrivateName { .. }) => false,
            _ => same_tokens(ast, src, *x, *y),
        },
        _ => false,
    }
}

fn key_of<'a>(ast: &'a Ast, src: &'a str, key: NodeId) -> Key<'a> {
    match *ast.node(key) {
        Node::PrivateName { .. } => Key::Tokens(key),
        Node::ComputedKey { expr } => match static_value_string(ast, src, expr) {
            Some(n) => Key::Name(n),
            None => Key::Tokens(expr),
        },
        _ => match key_name(ast, src, key) {
            Some(n) => Key::Name(n),
            None => Key::Tokens(key),
        },
    }
}

struct Accessor<'a> {
    key: Key<'a>,
    getters: Vec<usize>,
    setters: Vec<usize>,
}

impl GroupedAccessorPairs {
    /// `(index, kind, key)` of the accessors to check in `members`.
    fn check(
        &self,
        cx: &mut Ctx<'_, '_>,
        members: &[NodeId],
        accessor: impl Fn(NodeId) -> Option<(MethodKind, NodeId)>,
    ) {
        let ast = cx.ast();
        let src = cx.src();
        let mut groups: Vec<Accessor<'_>> = Vec::new();
        for (i, &m) in members.iter().enumerate() {
            let Some((kind, key)) = accessor(m) else {
                continue;
            };
            let key = key_of(ast, src, key);
            let g = match groups
                .iter_mut()
                .position(|g| same_key(ast, src, &g.key, &key))
            {
                Some(g) => &mut groups[g],
                None => {
                    groups.push(Accessor {
                        key,
                        getters: Vec::new(),
                        setters: Vec::new(),
                    });
                    groups.last_mut().expect("just pushed")
                }
            };
            if kind == MethodKind::Get {
                g.getters.push(i);
            } else {
                g.setters.push(i);
            }
        }
        for g in &groups {
            let (&[get], &[set]) = (g.getters.as_slice(), g.setters.as_slice()) else {
                continue;
            };
            let (former, latter) = if get < set { (get, set) } else { (set, get) };
            let (former, latter) = (members[former], members[latter]);
            let names = |cx: &Ctx<'_, '_>| (self.name(cx, former), self.name(cx, latter));
            if get.abs_diff(set) > 1 {
                let (f, l) = names(cx);
                let span = self.head(cx, latter);
                cx.report(
                    span,
                    format!("Accessor pair {f} and {l} should be grouped."),
                )
                .id("notGrouped");
            } else if (self.order == Order::GetFirst && get > set)
                || (self.order == Order::SetFirst && get < set)
            {
                let (f, l) = names(cx);
                let span = self.head(cx, latter);
                cx.report(span, format!("Expected {l} to be before {f}."))
                    .id("invalidOrder");
            }
        }
    }

    /// `getFunctionNameWithKind` of an accessor member.
    fn name(&self, cx: &Ctx<'_, '_>, member: NodeId) -> String {
        let ast = cx.ast();
        match *ast.node(member) {
            Node::MethodDef { value, .. } => {
                function_name_with_kind(ast, cx.src(), cx.file.parents, value)
            }
            Node::MethodSignature { kind, key, .. } => {
                let word = if kind == MethodKind::Get {
                    "getter"
                } else {
                    "setter"
                };
                let name = match *ast.node(key) {
                    Node::ComputedKey { expr } => static_value_string(ast, cx.src(), expr),
                    _ => key_name(ast, cx.src(), key),
                };
                format!("{word} '{}'", name.as_deref().unwrap_or("null"))
            }
            _ => String::new(),
        }
    }

    /// `getFunctionHeadLoc`: from the member's start to its parameters' `(`.
    fn head(&self, cx: &Ctx<'_, '_>, member: NodeId) -> Span {
        let ast = cx.ast();
        match *ast.node(member) {
            Node::MethodDef { value, .. } => {
                function_head_span(ast, cx.src(), cx.file.parents, value)
            }
            _ => {
                let span = ast.span(member);
                let key_end = match *ast.node(member) {
                    Node::MethodSignature { key, .. } => ast.span(key).end,
                    _ => span.start,
                };
                let paren = find_char(cx.src(), key_end, b'(').unwrap_or(span.end);
                Span::new(span.start, paren.min(span.end))
            }
        }
    }
}

fn method_accessor(
    ast: &Ast,
    m: NodeId,
    want_static: Option<bool>,
) -> Option<(MethodKind, NodeId)> {
    let Node::MethodDef {
        kind: kind @ (MethodKind::Get | MethodKind::Set),
        key,
        modifiers,
        ..
    } = *ast.node(m)
    else {
        return None;
    };
    if modifiers.contains(Modifiers::ABSTRACT) {
        return None;
    }
    if let Some(s) = want_static
        && modifiers.contains(Modifiers::STATIC) != s
    {
        return None;
    }
    Some((kind, key))
}

impl Rule for GroupedAccessorPairs {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ObjectLit,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::Interface,
            NodeKind::TypeLiteral,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let list = |l: NodeList| ast.list(l).to_vec();
        match *ast.node(node) {
            Node::ObjectLit { props } => {
                self.check(cx, &list(props), |m| method_accessor(ast, m, None));
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                let members = list(members);
                self.check(cx, &members, |m| method_accessor(ast, m, Some(false)));
                self.check(cx, &members, |m| method_accessor(ast, m, Some(true)));
            }
            Node::Interface { body, .. } | Node::TypeLiteral { members: body } if self.ts_types => {
                self.check(cx, &list(body), |m| match *ast.node(m) {
                    Node::MethodSignature {
                        kind: kind @ (MethodKind::Get | MethodKind::Set),
                        key,
                        ..
                    } => Some((kind, key)),
                    _ => None,
                });
            }
            _ => {}
        }
    }
}
