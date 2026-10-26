//! `unicorn/prefer-error-is-error`: `Error.isError(x)` over `x instanceof Error` and
//! `Object.prototype.toString.call(x) === '[object Error]'` (or `({}).toString.call(x)`, either
//! side, `==`/`!=`/`===`/`!==`; the negated forms become `!Error.isError(x)`). Not when a value
//! named `Error` (or, for the tag check, `Object`) is declared in reach: only type-only
//! declarations are ignored. No fix when a comment would go.

use super::prelude::*;
use super::unicorn_abort::is_type_only_decl;
use super::unicorn_static::static_string_literal;
use super::unicorn_text::{call_argument_text, would_remove_comments};
use super::unicorn_util::{space_around_keyword, with_edits};
use super::unicorn_util_h5::method_call_of;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-error-is-error",
        "Prefer `Error.isError()` when checking for errors.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferErrorIsError>,
};

#[derive(Debug, Default)]
struct PreferErrorIsError;

/// Some scope around `node` declares `name` as a value; type-only declarations in an inner
/// scope do not hide an outer value.
fn is_value_shadowed(cx: &Ctx<'_, '_>, name: &str, node: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut scope = Some(sem.enclosing_scope(node));
    while let Some(s) = scope {
        let Some(sym) = sem.lookup(s, name) else {
            return false;
        };
        let symbol = sem.symbol(sym);
        if symbol
            .decls
            .iter()
            .any(|d| !is_type_only_decl(ast, d.kind, d.node, d.parent))
        {
            return true;
        }
        scope = sem.scope(symbol.scope).parent;
    }
    false
}

/// `<obj>.<name>` without `?.` or computed access: the object.
fn member_named(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    match *ast.node(skip_parens(ast, id)) {
        Node::Member {
            obj,
            prop,
            optional: false,
        } if ident_name(ast, prop) == Some(name) => Some(skip_parens(ast, obj)),
        _ => None,
    }
}

/// `Object.prototype.toString.call(x)` (a global `Object`) or `({}).toString.call(x)`: `x`.
fn to_string_call_argument(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let (method, _, &[arg]) = method_call_of(ast, id, &["call"], 1, Some(1), Some(false))? else {
        return None;
    };
    let receiver = member_named(cx, method, "toString")?;
    let matches = match *ast.node(receiver) {
        Node::ObjectLit { props } => ast.list(props).is_empty(),
        _ => member_named(cx, receiver, "prototype").is_some_and(|object| {
            matches!(ast.node(object), Node::Ident { .. })
                && ident_name(ast, object) == Some("Object")
                && !is_value_shadowed(cx, "Object", object)
        }),
    };
    (matches && !matches!(ast.node(arg), Node::Spread { .. })).then_some(arg)
}

fn is_error_tag(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    static_string_literal(ast, skip_parens(ast, id)).as_deref() == Some("[object Error]")
}

impl Rule for PreferErrorIsError {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { op, left, right } = *ast.node(node) else {
            return;
        };
        let argument = match op {
            BinaryOp::InstanceOf => {
                let right = skip_parens(ast, right);
                (matches!(ast.node(right), Node::Ident { .. })
                    && ident_name(ast, right) == Some("Error"))
                .then_some(left)
            }
            BinaryOp::StrictEq | BinaryOp::StrictNotEq | BinaryOp::Eq | BinaryOp::NotEq => {
                if is_error_tag(cx, right)
                    && let Some(arg) = to_string_call_argument(cx, left)
                {
                    Some(arg)
                } else if is_error_tag(cx, left) {
                    to_string_call_argument(cx, right)
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(argument) = argument else {
            return;
        };
        if is_value_shadowed(cx, "Error", node) {
            return;
        }
        let span = cx.span(node);
        let fix = (!would_remove_comments(cx, span, &[])).then(|| {
            let negate = matches!(op, BinaryOp::StrictNotEq | BinaryOp::NotEq);
            let text = format!(
                "{}Error.isError({})",
                if negate { "!" } else { "" },
                call_argument_text(cx, argument)
            );
            with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
        });
        cx.report(span, "Prefer `Error.isError(…)`.")
            .id("prefer-error-is-error")
            .fix_opt(fix);
    }
}
