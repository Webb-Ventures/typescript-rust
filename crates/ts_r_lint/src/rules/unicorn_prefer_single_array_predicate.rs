//! `unicorn/prefer-single-array-predicate`: `a.some(x => p) || a.some(x => q)` (or `every`
//! with `&&`) as the whole chain of that operator, every operand a plain one-argument call
//! (no optional chaining, no type arguments) of the same receiver with a one-parameter,
//! expression-bodied, non-async arrow with no type parameters or return type and the same
//! parameter text, and the receiver not known to be something other than an indexed
//! collection. The chain is reported with a suggestion merging the bodies into one call
//! (bodies parenthesized where the operator would bind them differently), withheld when it
//! would drop a comment.

use super::prelude::*;
use super::unicorn_ast::{method_call, would_remove_comments};
use super::unicorn_types::is_known_non_indexed_collection;
use super::unicorn_util_h1::{is_outermost_logical, is_same_reference, logical_operands};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-single-array-predicate",
        "Prefer a single `Array#some()` or `Array#every()` with a combined predicate.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferSingleArrayPredicate>,
};

#[derive(Debug, Default)]
struct PreferSingleArrayPredicate;

struct Predicate {
    callee: NodeId,
    obj: NodeId,
    param_text: String,
    body: NodeId,
}

fn predicate(cx: &Ctx<'_, '_>, id: NodeId, method: &str) -> Option<Predicate> {
    let ast = cx.ast();
    let m = method_call(ast, id)?;
    let Node::Call {
        callee,
        type_args: None,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    if m.name != method || !m.plain() || m.args.len() != 1 {
        return None;
    }
    let Node::Arrow {
        flags,
        type_params: None,
        params,
        return_type: None,
        body,
    } = *ast.node(m.args[0])
    else {
        return None;
    };
    if flags.contains(FnFlags::ASYNC) || !flags.contains(FnFlags::EXPRESSION_BODY) {
        return None;
    }
    let &[param] = ast.list(params) else {
        return None;
    };
    let Node::Param {
        rest: false,
        pattern,
        optional,
        type_ann,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    if !matches!(ast.node(pattern), Node::Ident { .. }) {
        return None;
    }
    if is_known_non_indexed_collection(cx, m.obj) {
        return None;
    }
    let text = cx.text(param);
    let param_text = if type_ann.is_some() || optional {
        format!("({text})")
    } else {
        text.to_owned()
    };
    Some(Predicate {
        callee: skip_parens(ast, callee),
        obj: m.obj,
        param_text,
        body,
    })
}

fn body_text(cx: &Ctx<'_, '_>, body: NodeId, op: BinaryOp) -> String {
    let text = cx.text(body);
    let wrap = match *cx.node(body) {
        Node::Paren { .. } => false,
        Node::Binary { op: o, .. }
            if matches!(
                o,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
            ) =>
        {
            o != op && !(op == BinaryOp::LogicalOr && o == BinaryOp::LogicalAnd)
        }
        Node::Assign { .. }
        | Node::Arrow { .. }
        | Node::Conditional { .. }
        | Node::Seq { .. }
        | Node::As { .. }
        | Node::NonNull { .. }
        | Node::Satisfies { .. }
        | Node::TypeAssertion { .. }
        | Node::Yield { .. } => true,
        _ => false,
    };
    if wrap {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

impl Rule for PreferSingleArrayPredicate {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { op, .. } = *cx.node(node) else {
            return;
        };
        let method = match op {
            BinaryOp::LogicalOr => "some",
            BinaryOp::LogicalAnd => "every",
            _ => return,
        };
        if !is_outermost_logical(cx, node) {
            return;
        }
        let operands = logical_operands(ast, node, op);
        let mut calls = Vec::with_capacity(operands.len());
        for o in operands {
            let Some(p) = predicate(cx, o, method) else {
                return;
            };
            calls.push(p);
        }
        let [first, rest @ ..] = calls.as_slice() else {
            return;
        };
        if rest.is_empty()
            || !rest.iter().all(|c| {
                is_same_reference(cx, first.obj, c.obj) && c.param_text == first.param_text
            })
        {
            return;
        }
        let span = cx.span(node);
        let suggestion = (!would_remove_comments(ast, span, &[])).then(|| {
            let joiner = format!(" {} ", op.as_str());
            let preds: Vec<String> = calls.iter().map(|c| body_text(cx, c.body, op)).collect();
            let text = format!(
                "{}({} => {})",
                cx.text(first.callee),
                first.param_text,
                preds.join(&joiner)
            );
            Fix::replace(span, text)
        });
        let message = format!("Prefer a single `Array#{method}()` with a combined predicate.");
        let report = cx
            .report(span, message)
            .id("prefer-single-array-predicate/error");
        if let Some(fix) = suggestion {
            report.suggest(
                "prefer-single-array-predicate/suggestion",
                format!("Merge predicates into one `Array#{method}()` call."),
                fix,
            );
        }
    }
}
