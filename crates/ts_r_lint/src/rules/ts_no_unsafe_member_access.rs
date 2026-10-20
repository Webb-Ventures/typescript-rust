//! `@typescript-eslint/no-unsafe-member-access`: disallow member access on `any` values.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-member-access",
        "Disallow member access on a value with type `any`",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<NoUnsafeMemberAccess>,
};

#[derive(Debug)]
struct NoUnsafeMemberAccess {
    allow_optional_chaining: bool,
}

impl FromOptions for NoUnsafeMemberAccess {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnsafeMemberAccess {
            allow_optional_chaining: bool_field(o, "allowOptionalChaining", false),
        })
    }
}

const THIS_HINT: &str = "\nYou can try to fix this by turning on the `noImplicitThis` compiler option, or adding a `this` parameter to the function.";

/// Whether a member access is on an `any` value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Safe,
    /// The object is `any`; reported here.
    Unsafe(AnyKind),
    /// Part of a chain whose first unsafe access was already reported.
    Chained,
}

impl Rule for NoUnsafeMemberAccess {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if in_heritage(cx, node) {
            return;
        }
        let ast = cx.ast();
        let (obj, text_node, prop_text) = match *cx.node(node) {
            Node::Member { obj, prop, .. } => {
                (obj, prop, format!(".{}", cx.span(prop).text(cx.src())))
            }
            Node::Index { obj, index, .. } => {
                let index = skip_parens(ast, index);
                (obj, index, format!("[{}]", cx.span(index).text(cx.src())))
            }
            _ => return,
        };
        // A computed key typed `any` is reported whatever the object is (observed: before
        // the object's own report at the same position).
        if let Node::Index {
            index, optional, ..
        } = *cx.node(node)
            && !(optional && self.allow_optional_chaining)
        {
            let index = skip_parens(ast, index);
            let ti = cx.types();
            if let Some(it) = ti.type_at(index) {
                let span = cx.span(index);
                let text = span.text(cx.src()).to_string();
                match any_kind(ti, it) {
                    Some(AnyKind::Any) => {
                        cx.report(
                            span,
                            format!("Computed name [{text}] resolves to an `any` value."),
                        )
                        .id("unsafeComputedMemberAccess");
                    }
                    Some(AnyKind::Error) => {
                        cx.report(
                            span,
                            format!("The type of computed name [{text}] cannot be resolved."),
                        )
                        .id("errorComputedMemberAccess");
                    }
                    None => {}
                }
            }
        }
        match self.state(cx, node, 0) {
            State::Unsafe(kind) => {
                let span = cx.span(text_node);
                let ti = cx.types();
                let this_any = this_root(ast, obj)
                    .filter(|&t| skip_parens(ast, obj) == t || is_member_chain_to(ast, obj, t))
                    .and_then(|t| ti.type_at(t))
                    .and_then(|t| any_kind(ti, t));
                match (kind, this_any) {
                    (AnyKind::Any, Some(AnyKind::Any)) => {
                        cx.report(
                            span,
                            format!(
                                "Unsafe member access {prop_text} on an `any` value. `this` is typed as `any`.{THIS_HINT}"
                            ),
                        )
                        .id("unsafeThisMemberExpression");
                    }
                    (AnyKind::Any, _) => {
                        cx.report(
                            span,
                            format!("Unsafe member access {prop_text} on an `any` value."),
                        )
                        .id("unsafeMemberExpression");
                    }
                    (AnyKind::Error, _) => {
                        cx.report(
                            span,
                            format!(
                                "Unsafe member access {prop_text} on a type that cannot be resolved."
                            ),
                        )
                        .id("errorMemberExpression");
                    }
                }
            }
            State::Chained | State::Safe => {}
        }
    }
}

impl NoUnsafeMemberAccess {
    fn state(&self, cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> State {
        let (obj, optional) = match *cx.node(node) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                (obj, optional)
            }
            _ => return State::Safe,
        };
        if self.allow_optional_chaining && optional {
            return State::Safe;
        }
        let ast = cx.ast();
        let o = skip_parens(ast, obj);
        if depth < 256
            && matches!(cx.node(o), Node::Member { .. } | Node::Index { .. })
            && self.state(cx, o, depth + 1) != State::Safe
        {
            return State::Chained;
        }
        let ti = cx.types();
        match constrained_type_at(ti, o).and_then(|t| any_kind(ti, t)) {
            Some(k) => State::Unsafe(k),
            None => State::Safe,
        }
    }
}

/// `obj` is a chain of member accesses rooted at `root`.
fn is_member_chain_to(ast: &ts_r_ast::Ast, obj: NodeId, root: NodeId) -> bool {
    let mut cur = skip_parens(ast, obj);
    for _ in 0..256 {
        if cur == root {
            return true;
        }
        match *ast.node(cur) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => cur = skip_parens(ast, obj),
            _ => return false,
        }
    }
    false
}

/// `class C implements A.B` / `interface I extends A.B`: names, not values.
fn in_heritage(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..256 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match cx.node(p) {
            Node::Member { .. } => cur = p,
            Node::ExprWithTypeArgs { .. } => {
                return cx.parent(p).is_some_and(|h| match cx.node(h) {
                    Node::HeritageClause { kind, .. } => {
                        *kind == ts_r_ast::HeritageKind::Implements
                    }
                    Node::Interface { .. } => true,
                    _ => false,
                });
            }
            _ => return false,
        }
    }
    false
}
