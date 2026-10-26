//! `unicorn/prefer-location-assign`: an assignment to `location.href` (also
//! `window.location` / `globalThis.location`, a `const` alias of those, and a computed
//! key with a static, side-effect-free value), with the globals unshadowed; reported at
//! `href`. A plain `=` whose value is unused, on the direct location object, with no
//! comment that would be lost, is fixed to `location.assign(value)`.

use super::prelude::*;
use super::unicorn_ast::is_value_not_usable;
use super::unicorn_static::static_value;
use super::unicorn_text::{has_possible_side_effect, paren_text, would_remove_comments};
use super::unicorn_util::{parenthesized_span, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-location-assign",
        "Prefer `location.assign()` over assigning to `location.href`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferLocationAssign>,
};

#[derive(Debug, Default)]
struct PreferLocationAssign;

fn is_direct_location(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Ident { .. } => {
            ident_name(ast, id) == Some("location") && cx.semantic().refers_to_global(id)
        }
        Node::Member { obj, prop, .. } => {
            matches!(ident_name(ast, obj), Some("window" | "globalThis"))
                && matches!(ast.node(obj), Node::Ident { .. })
                && ident_name(ast, prop) == Some("location")
                && cx.semantic().refers_to_global(obj)
        }
        _ => false,
    }
}

fn is_location(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if is_direct_location(cx, id) {
        return true;
    }
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(Resolution::Symbol(s)) = sem.reference_at(id).map(|r| sem.reference(r).resolution)
    else {
        return false;
    };
    let [decl] = sem.symbol(s).decls.as_slice() else {
        return false;
    };
    if decl.kind != DeclKind::Const {
        return false;
    }
    matches!(*ast.node(decl.node), Node::VarDeclarator { name, init: Some(init), .. }
        if name == decl.name && is_direct_location(cx, init))
}

impl Rule for PreferLocationAssign {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Assign { op, left, right } = *cx.node(node) else {
            return;
        };
        let (obj, key) = match *ast.node(skip_parens(ast, left)) {
            Node::Member { obj, prop, .. } => {
                if ident_name(ast, prop) != Some("href") {
                    return;
                }
                (obj, prop)
            }
            Node::Index { obj, index, .. } => {
                if has_possible_side_effect(cx, index)
                    || static_value(cx, index)
                        .and_then(|v| v.to_js_string())
                        .as_deref()
                        != Some("href")
                {
                    return;
                }
                (obj, index)
            }
            _ => return,
        };
        let obj = skip_parens(ast, obj);
        if !is_location(cx, obj) {
            return;
        }
        let span = cx.span(node);
        let right_span = parenthesized_span(cx, right);
        let fixable = op == AssignOp::Assign
            && is_value_not_usable(cx, node)
            && is_direct_location(cx, obj)
            && !would_remove_comments(cx, span, &[cx.span(obj), right_span]);
        let fix = fixable.then(|| {
            let text = format!("{}.assign({})", cx.text(obj), paren_text(cx, right));
            with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
        });
        cx.report(
            cx.span(key),
            "Prefer `Location#assign()` over assigning to `Location#href`.",
        )
        .id("prefer-location-assign")
        .fix_opt(fix);
    }
}
