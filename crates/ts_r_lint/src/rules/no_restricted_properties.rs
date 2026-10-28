//! `no-restricted-properties`: report configured `object.property` accesses, including
//! destructuring.

use std::borrow::Cow;

use rustc_hash::FxHashMap;
use ts_r_ast::{Ast, ParentMap};

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-restricted-properties",
        "Disallow certain properties on certain objects",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoRestrictedProperties>,
};

#[derive(Debug, Default)]
struct Restriction {
    message: Option<String>,
    /// `allowObjects` for a property, `allowProperties` for an object.
    allow: Option<Vec<String>>,
}

#[derive(Debug, Default)]
struct NoRestrictedProperties {
    /// `{ object, property }`: object -> property -> restriction.
    pairs: FxHashMap<String, FxHashMap<String, Restriction>>,
    /// `{ object }` alone.
    objects: FxHashMap<String, Restriction>,
    /// `{ property }` alone.
    properties: FxHashMap<String, Restriction>,
}

impl FromOptions for NoRestrictedProperties {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut out = Self::default();
        for opt in opts {
            let Some(o) = opt.as_object() else {
                continue;
            };
            let list = |k: &str| o.get(k).and_then(Value::as_array).map(|_| str_list(o, k));
            let message = str_field(o, "message").map(str::to_owned);
            let object = str_field(o, "object");
            let property = str_field(o, "property");
            match (object, property) {
                (None, Some(p)) => {
                    let allow = list("allowObjects");
                    out.properties
                        .insert(p.to_owned(), Restriction { message, allow });
                }
                (Some(ob), None) => {
                    let allow = list("allowProperties");
                    out.objects
                        .insert(ob.to_owned(), Restriction { message, allow });
                }
                (Some(ob), Some(p)) => {
                    out.pairs.entry(ob.to_owned()).or_default().insert(
                        p.to_owned(),
                        Restriction {
                            message,
                            allow: None,
                        },
                    );
                }
                (None, None) => {}
            }
        }
        Ok(out)
    }
}

impl NoRestrictedProperties {
    fn check(&self, cx: &mut Ctx<'_, '_>, span: Span, object: Option<&str>, property: &str) {
        let by_object = object.and_then(|ob| match self.pairs.get(ob) {
            Some(props) => props.get(property),
            None => self.objects.get(ob),
        });
        let allowed = |r: &Restriction, name: &str| {
            r.allow
                .as_ref()
                .is_some_and(|a| a.iter().any(|x| x == name))
        };
        let suffix = |r: &Restriction| {
            r.message
                .as_ref()
                .map(|m| format!(" {m}"))
                .unwrap_or_default()
        };
        if let (Some(r), Some(ob)) = (by_object, object)
            && !allowed(r, property)
        {
            let allow = r
                .allow
                .as_ref()
                .map(|a| format!(" Only these properties are allowed: {}.", a.join(", ")))
                .unwrap_or_default();
            let msg = format!(
                "'{ob}.{property}' is restricted from being used.{allow}{}",
                suffix(r)
            );
            cx.report(span, msg).id("restrictedObjectProperty");
        } else if let Some(r) = self.properties.get(property)
            && !object.is_some_and(|ob| allowed(r, ob))
        {
            let allow = r
                .allow
                .as_ref()
                .map(|a| {
                    format!(
                        " Property '{property}' is only allowed on these objects: {}.",
                        a.join(", ")
                    )
                })
                .unwrap_or_default();
            let msg = format!(
                "'{property}' is restricted from being used.{allow}{}",
                suffix(r)
            );
            cx.report(span, msg).id("restrictedProperty");
        }
    }
}

impl Rule for NoRestrictedProperties {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Member,
            NodeKind::Index,
            NodeKind::ObjectPattern,
            NodeKind::ObjectLit,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let parents = cx.file.parents;
        let span = cx.span(node);
        match *ast.node(node) {
            Node::Member { obj, prop, .. } => {
                if matches!(ast.node(prop), Node::PrivateName { .. }) {
                    return;
                }
                let Some(name) = member_name(ast, src, node) else {
                    return;
                };
                let object = ident_name(ast, skip_parens(ast, obj));
                self.check(cx, span, object, &name);
            }
            Node::Index { obj, .. } => {
                let Some(name) = member_name(ast, src, node) else {
                    return;
                };
                let object = ident_name(ast, skip_parens(ast, obj));
                self.check(cx, span, object, &name);
            }
            Node::ObjectPattern { props } => {
                let source = match parents.parent(node).map(|p| ast.node(p)) {
                    Some(&Node::VarDeclarator { init, .. }) => init,
                    Some(&Node::Param { init, .. }) => init,
                    Some(&Node::BindingProperty { init, .. }) => init,
                    Some(&Node::AssignPattern { right, .. }) => Some(right),
                    _ => None,
                };
                let object = source.and_then(|s| ident_name(ast, s));
                for &p in ast.list(props) {
                    let name = match *ast.node(p) {
                        Node::BindingProperty { key, .. } => key_name(ast, src, key),
                        _ => None,
                    };
                    if let Some(name) = name {
                        self.check(cx, span, object, &name);
                    }
                }
            }
            Node::ObjectLit { props } => {
                if !is_assign_target(ast, parents, node) {
                    return;
                }
                let object = match parents.parent(node).map(|p| ast.node(p)) {
                    Some(&Node::Assign { right, .. }) => ident_name(ast, right),
                    _ => None,
                };
                for &p in ast.list(props) {
                    let name = match *ast.node(p) {
                        Node::Property { key, .. } => key_name(ast, src, key),
                        Node::ShorthandProp { name, .. } => {
                            ident_name(ast, name).map(Cow::Borrowed)
                        }
                        _ => None,
                    };
                    if let Some(name) = name {
                        self.check(cx, span, object, &name);
                    }
                }
            }
            _ => {}
        }
    }
}

/// An object or array literal in an assignment-target position (`({ a } = o)`,
/// `[{ a }] = o`, `for ({ a } of xs)`), which ESTree represents as a pattern.
fn is_assign_target(ast: &Ast, parents: &ParentMap, mut id: NodeId) -> bool {
    for _ in 0..1_000 {
        let Some(p) = parents.parent(id) else {
            return false;
        };
        match *ast.node(p) {
            Node::Assign { left, .. } if left == id => {
                // `{ a } = d` nested in a pattern is a default value.
                return true;
            }
            Node::ForOf { left, .. } | Node::ForIn { left, .. } => return left == id,
            Node::ArrayLit { .. } => id = p,
            Node::Spread { .. } => id = p,
            Node::Property { value, .. } if value == id => match parents.parent(p) {
                Some(obj) => id = obj,
                None => return false,
            },
            _ => return false,
        }
        if !matches!(
            ast.node(id),
            Node::ArrayLit { .. } | Node::ObjectLit { .. } | Node::Spread { .. }
        ) {
            return false;
        }
    }
    false
}
