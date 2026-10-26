//! `unicorn/no-useless-boolean-cast`: `Boolean(x)` (the global, one non-spread argument)
//! returned by an `every`/`filter`/`find`/`findIndex`/`findLast`/`findLastIndex`/`some`
//! callback (a non-async, non-generator function or arrow without a return type, as its
//! concise body or its only `return`) adds nothing: the method already tests truthiness.
//! Not reported when `x` is the callback's first parameter (a type guard idiom) or may be an
//! optional chain result (`a?.b`, the last operand of `||`/`??`, either operand of `&&`, a
//! conditional branch, the last of a sequence). The fix is withheld when the call holds a
//! comment.

use super::prelude::*;
use super::unicorn_ast::{contains_node, method_call};
use super::unicorn_text::{expression_text, would_remove_comments};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-boolean-cast",
        "Disallow unnecessary `Boolean()` casts in array predicate callbacks.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessBooleanCast>,
};

#[derive(Debug, Default)]
struct NoUselessBooleanCast;

const METHODS: &[&str] = &[
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "some",
];

fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
    id
}

fn is_optional_chain_result(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    let id = unwrap(ast, id);
    match *ast.node(id) {
        Node::Member { .. } | Node::Index { .. } | Node::Call { .. } => in_optional_chain(ast, id),
        Node::Binary {
            op: BinaryOp::LogicalAnd,
            left,
            right,
        } => {
            is_optional_chain_result(ast, left, depth + 1)
                || is_optional_chain_result(ast, right, depth + 1)
        }
        Node::Binary {
            op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
            ..
        } => is_optional_chain_result(ast, right, depth + 1),
        Node::Conditional { cons, alt, .. } => {
            is_optional_chain_result(ast, cons, depth + 1)
                || is_optional_chain_result(ast, alt, depth + 1)
        }
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&e| is_optional_chain_result(ast, e, depth + 1)),
        _ => false,
    }
}

/// An optional link in the member/call chain `id` heads. Parentheses end the chain:
/// `(a?.b).c` reads `.c` from a plain value.
fn in_optional_chain(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..100_000 {
        match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            Node::NonNull { expr } => id = expr,
            _ => return false,
        }
    }
    false
}

/// `getConciseArrowBodyText`: `id`'s text as a concise arrow body, wrapped when it would
/// start with `{`, is a sequence or an assignment, or holds `in` inside a `for` initializer.
pub(crate) fn concise_arrow_body_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let needs = cx.text(id).starts_with('{')
        || matches!(ast.node(id), Node::Seq { .. } | Node::Assign { .. })
        || (in_for_init(cx, id)
            && contains_node(ast, id, |n| {
                matches!(
                    n,
                    Node::Binary {
                        op: BinaryOp::In,
                        ..
                    }
                )
            }));
    expression_text(cx, id, needs)
}

fn in_for_init(cx: &Ctx<'_, '_>, mut id: NodeId) -> bool {
    for _ in 0..100_000 {
        let Some(p) = cx.parent(id) else {
            return false;
        };
        if matches!(*cx.node(p), Node::For { init: Some(i), .. } if i == id) {
            return true;
        }
        id = p;
    }
    false
}

impl Rule for NoUselessBooleanCast {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !call.plain()
            || !METHODS.contains(&call.name)
            || call.args.is_empty()
            || call.args.len() > 2
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let method = call.name;
        let callback = skip_parens(ast, call.args[0]);
        let (flags, params, return_type, body) = match *ast.node(callback) {
            Node::Arrow {
                flags,
                params,
                return_type,
                body,
                ..
            } => (flags, params, return_type, Some(body)),
            Node::FunctionExpr {
                flags,
                params,
                return_type,
                body,
                ..
            } => (flags, params, return_type, body),
            _ => return,
        };
        if flags.contains(FnFlags::ASYNC)
            || flags.contains(FnFlags::GENERATOR)
            || return_type.is_some()
        {
            return;
        }
        let Some(body) = body else {
            return;
        };
        let concise = !matches!(ast.node(body), Node::Block { .. });
        let returned = match *ast.node(body) {
            Node::Block { stmts } => match *ast.list(stmts) {
                [s] => match *ast.node(s) {
                    Node::Return { arg: Some(a) } => a,
                    _ => return,
                },
                _ => return,
            },
            _ => body,
        };
        let boolean_call = skip_parens(ast, returned);
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(boolean_call)
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        if !matches!(ast.node(callee), Node::Ident { .. })
            || ident_name(ast, callee) != Some("Boolean")
            || !cx.semantic().refers_to_global(callee)
        {
            return;
        }
        let [arg] = *ast.list(args) else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let arg_inner = skip_parens(ast, arg);
        if let Some(&first) = ast.list(params).first()
            && let Node::Param {
                rest: false,
                pattern,
                init: None,
                ..
            } = *ast.node(first)
            && matches!(ast.node(pattern), Node::Ident { .. })
            && matches!(ast.node(arg_inner), Node::Ident { .. })
            && ident_name(ast, pattern) == ident_name(ast, arg_inner)
        {
            return;
        }
        if is_optional_chain_result(ast, arg, 0) {
            return;
        }
        let span = cx.span(boolean_call);
        let fix = (!would_remove_comments(cx, span, &[])).then(|| {
            let text = if concise {
                concise_arrow_body_text(cx, arg_inner)
            } else {
                cx.text(arg_inner).to_owned()
            };
            Fix::replace(span, text)
        });
        cx.report(
            span,
            format!("`Boolean()` is unnecessary in `Array#{method}()` callbacks."),
        )
        .fix_opt(fix);
    }
}
