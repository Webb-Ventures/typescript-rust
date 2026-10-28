//! `accessor-pairs`: a setter without a getter (and, optionally, a getter without a setter)
//! in object literals, property descriptors, classes and (optionally) TS types.

use super::grouped_accessor_pairs::{GroupedAccessorPairs, Key, key_of, method_accessor, same_key};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "accessor-pairs",
        "Enforce getter and setter pairs in objects and classes",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<AccessorPairs>,
};

#[derive(Debug)]
struct AccessorPairs {
    get_without_set: bool,
    set_without_get: bool,
    class_members: bool,
    ts_types: bool,
}

impl FromOptions for AccessorPairs {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(AccessorPairs {
            get_without_set: bool_field(o, "getWithoutSet", false),
            set_without_get: bool_field(o, "setWithoutGet", true),
            class_members: bool_field(o, "enforceForClassMembers", true),
            ts_types: bool_field(o, "enforceForTSTypes", false),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    Object,
    Class,
    Type,
}

impl AccessorPairs {
    fn check(
        &self,
        cx: &mut Ctx<'_, '_>,
        members: &[NodeId],
        at: Where,
        accessor: impl Fn(NodeId) -> Option<(MethodKind, NodeId)>,
    ) {
        let ast = cx.ast();
        let src = cx.src();
        let mut groups: Vec<(Key<'_>, Vec<NodeId>, Vec<NodeId>)> = Vec::new();
        for &m in members {
            let Some((kind, key)) = accessor(m) else {
                continue;
            };
            let key = key_of(ast, src, key);
            let i = match groups.iter().position(|g| same_key(ast, src, &g.0, &key)) {
                Some(i) => i,
                None => {
                    groups.push((key, Vec::new(), Vec::new()));
                    groups.len() - 1
                }
            };
            if kind == MethodKind::Get {
                groups[i].1.push(m);
            } else {
                groups[i].2.push(m);
            }
        }
        let place = match at {
            Where::Object => "",
            Where::Class => "class ",
            Where::Type => "type ",
        };
        for (_, getters, setters) in &groups {
            let mut out = Vec::new();
            if self.set_without_get && getters.is_empty() {
                out.extend(
                    setters
                        .iter()
                        .map(|&m| (m, "Getter", "missingGetterInObjectLiteral")),
                );
            }
            if self.get_without_set && setters.is_empty() {
                out.extend(
                    getters
                        .iter()
                        .map(|&m| (m, "Setter", "missingSetterInObjectLiteral")),
                );
            }
            for (m, what, id) in out {
                let id = match (at, id) {
                    (Where::Object, id) => id,
                    (Where::Class, "missingGetterInObjectLiteral") => "missingGetterInClass",
                    (Where::Class, _) => "missingSetterInClass",
                    (_, "missingGetterInObjectLiteral") => "missingGetterInType",
                    _ => "missingSetterInType",
                };
                let name = GroupedAccessorPairs::name(cx, m);
                let span = GroupedAccessorPairs::head(cx, m);
                cx.report(span, format!("{what} is not present for {place}{name}."))
                    .id(id);
            }
        }
    }
}

impl Rule for AccessorPairs {
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
        if !self.get_without_set && !self.set_without_get {
            return;
        }
        let ast = cx.ast();
        let list = |l| ast.list(l).to_vec();
        match *ast.node(node) {
            Node::ObjectLit { props } => {
                let props = list(props);
                self.check(cx, &props, Where::Object, |m| method_accessor(ast, m, None));
                if !super::no_setter_return::is_descriptor(cx, node) {
                    return;
                }
                // Non-computed `get` / `set` keys of the descriptor.
                let has = |want: &str| {
                    props.iter().any(|&p| {
                        let key = match *ast.node(p) {
                            Node::Property { key, .. } => key,
                            Node::ShorthandProp { name, .. } => name,
                            Node::MethodDef {
                                key,
                                kind: MethodKind::Method,
                                ..
                            } => key,
                            _ => return false,
                        };
                        matches!(ast.node(key), Node::Ident { .. })
                            && ident_name(ast, key) == Some(want)
                    })
                };
                let (get, set) = (has("get"), has("set"));
                let span = ast.span(node);
                if self.set_without_get && set && !get {
                    cx.report(span, "Getter is not present in property descriptor.")
                        .id("missingGetterInPropertyDescriptor");
                }
                if self.get_without_set && get && !set {
                    cx.report(span, "Setter is not present in property descriptor.")
                        .id("missingSetterInPropertyDescriptor");
                }
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }
                if self.class_members =>
            {
                let members = list(members);
                self.check(cx, &members, Where::Class, |m| {
                    method_accessor(ast, m, Some(true))
                });
                self.check(cx, &members, Where::Class, |m| {
                    method_accessor(ast, m, Some(false))
                });
            }
            Node::Interface { body, .. } | Node::TypeLiteral { members: body } if self.ts_types => {
                self.check(cx, &list(body), Where::Type, |m| match *ast.node(m) {
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
