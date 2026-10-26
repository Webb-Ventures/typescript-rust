//! `no-useless-computed-key`: `{ ['a']: 1 }` -> `{ 'a': 1 }`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-computed-key",
        "Disallow unnecessary computed property keys in objects and classes",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoUselessComputedKey>,
};

#[derive(Debug)]
struct NoUselessComputedKey {
    enforce_for_class_members: bool,
}

impl FromOptions for NoUselessComputedKey {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(Self {
            enforce_for_class_members: bool_field(obj_opt(opts, 0), "enforceForClassMembers", true),
        })
    }
}

/// What kind of member owns the key, for the names it may not spell without brackets.
enum Owner {
    /// A property in an object literal (`__proto__` sets the prototype).
    ObjectLit,
    /// A property in a destructuring pattern.
    Pattern,
    Field {
        is_static: bool,
    },
    Method {
        is_static: bool,
    },
}

impl Rule for NoUselessComputedKey {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Property,
            NodeKind::BindingProperty,
            NodeKind::MethodDef,
            NodeKind::PropertyDef,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parents = cx.file.parents;
        let in_class = || {
            parents.parent(node).is_some_and(|p| {
                matches!(ast.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. })
            })
        };
        let (key, owner) = match *ast.node(node) {
            Node::Property { key, .. } => {
                let pattern = parents
                    .parent(node)
                    .is_some_and(|p| is_pattern_object(ast, parents, p));
                (
                    key,
                    if pattern {
                        Owner::Pattern
                    } else {
                        Owner::ObjectLit
                    },
                )
            }
            Node::BindingProperty { key, .. } => (key, Owner::Pattern),
            Node::MethodDef { key, modifiers, .. } if in_class() => {
                if modifiers.contains(Modifiers::ABSTRACT) || !self.enforce_for_class_members {
                    return;
                }
                let is_static = modifiers.contains(Modifiers::STATIC);
                (key, Owner::Method { is_static })
            }
            // Object-literal methods are ESTree `Property` nodes.
            Node::MethodDef { key, .. } => (key, Owner::ObjectLit),
            Node::PropertyDef { key, modifiers, .. } => {
                if modifiers.intersects(Modifiers::ABSTRACT | Modifiers::ACCESSOR)
                    || !self.enforce_for_class_members
                {
                    return;
                }
                let is_static = modifiers.contains(Modifiers::STATIC);
                (key, Owner::Field { is_static })
            }
            _ => return,
        };
        let Node::ComputedKey { expr } = *ast.node(key) else {
            return;
        };
        let lit = skip_parens(ast, expr);
        let value = match *ast.node(lit) {
            Node::StringLit { value } => Some(ast.atom(value)),
            Node::NumberLit { .. } => None,
            _ => return,
        };
        let allowed = match owner {
            Owner::ObjectLit => value != Some("__proto__"),
            Owner::Pattern => true,
            Owner::Field { is_static: true } => !matches!(value, Some("constructor" | "prototype")),
            Owner::Method { is_static: true } => value != Some("prototype"),
            Owner::Field { is_static: false } | Owner::Method { is_static: false } => {
                value != Some("constructor")
            }
        };
        if !allowed {
            return;
        }
        let src = cx.src();
        let lit_span = ast.span(lit);
        let raw = &src[lit_span.start as usize..lit_span.end as usize];
        let key_span = ast.span(key);
        let fix = ast
            .comments_in(key_span)
            .is_empty()
            .then(|| Fix::replace(key_span, space_before(src, key_span.start, raw.to_owned())));
        let span = cx.span(node);
        cx.report(
            span,
            format!("Unnecessarily computed property [{raw}] found."),
        )
        .id("unnecessarilyComputedProperty")
        .fix_opt(fix);
    }
}

/// An object literal that is an assignment target (`({ ['a']: x } = o)`), which ESTree
/// represents as an `ObjectPattern`.
fn is_pattern_object(ast: &ts_r_ast::Ast, parents: &ts_r_ast::ParentMap, obj: NodeId) -> bool {
    let mut id = obj;
    for _ in 0..10_000 {
        let Some(p) = parents.parent(id) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. } => {}
            Node::Assign { left, .. } => return left == id,
            Node::ForOf { left, .. } | Node::ForIn { left, .. } => return left == id,
            _ => return false,
        }
        id = p;
    }
    false
}
