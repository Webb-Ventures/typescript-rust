//! `unicorn/prefer-string-pad-start-end`: `c.repeat(n - s.length) + s` is `s.padStart(n, c)`
//! and `s + c.repeat(n - s.length)` is `s.padEnd(n, c)`, for a one-character static `c`.
//! `(c.repeat(n) + s).slice(-n)` and `(s + c.repeat(n)).slice(0, n)` (literal `n`) get the
//! same rewrite as a suggestion, since they also cut a longer `s`. Targets known not to be
//! strings are left alone. The fix needs a plain-name target, a literal length and a
//! literal padding; `" "` is the default and is dropped.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::{
    Static, has_side_effectful_const_initializer, mutable_member_access, static_value,
    static_value_control_flow, static_value_no_side_effects,
};
use super::unicorn_text::{
    has_comment_in, has_unparenthesized_optional_chain, needs_semicolon, paren_text,
};
use super::unicorn_types::{is_boolean, is_known_non_string, is_number};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::skip_wrappers;
use crate::semantic::DeclKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-pad-start-end",
        "Prefer `String#padStart()` and `String#padEnd()` over manual string padding.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferStringPadStartEnd>,
};

#[derive(Debug, Default)]
struct PreferStringPadStartEnd;

/// Expressions that are never strings.
fn is_clearly_non_string_kind(node: &Node) -> bool {
    matches!(
        node,
        Node::ArrayLit { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::ObjectLit { .. }
    )
}

/// The value a plain name stands for: its function or class declaration, or the
/// initializer of its `const` declaration, when it has one definition and is never
/// reassigned.
fn identifier_value_node(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(id)?;
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    let reassigned = sym.refs.iter().any(|&r| {
        let f = sem.reference(r).flags;
        f.contains(RefFlags::WRITE) && !f.contains(RefFlags::INIT)
    });
    if reassigned {
        return None;
    }
    match decl.kind {
        DeclKind::Function | DeclKind::Class => Some(decl.node),
        DeclKind::Const => match *ast.node(decl.node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if name == decl.name => Some(skip_parens(ast, init)),
            _ => None,
        },
        _ => None,
    }
}

/// `isBranchExpression`: a conditional or logical expression, through TS wrappers and
/// unary operators.
fn is_branch(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let mut node = skip_wrappers(ast, id);
    for _ in 0..1000 {
        match *ast.node(node) {
            Node::Unary { arg, .. } => node = skip_wrappers(ast, arg),
            Node::Conditional { .. }
            | Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                ..
            } => return true,
            _ => return false,
        }
    }
    false
}

fn static_value_result(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    let ast = cx.ast();
    let node = identifier_value_node(cx, id).unwrap_or(id);
    if has_side_effectful_const_initializer(cx, id) {
        return None;
    }
    let result = if is_branch(ast, node) {
        static_value_control_flow(cx, node)
    } else {
        static_value_no_side_effects(cx, node)
    };
    let Node::Seq { exprs } = *ast.node(node) else {
        return result;
    };
    if ast
        .list(exprs)
        .iter()
        .any(|&e| mutable_member_access(cx, e, &mut Vec::new()))
    {
        return None;
    }
    result.or_else(|| static_value(cx, node))
}

fn is_static_non_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let node = identifier_value_node(cx, id).unwrap_or(id);
    if let Node::Seq { exprs } = *ast.node(node)
        && mutable_member_access(cx, node, &mut Vec::new())
    {
        let last = ast.list(exprs).last().copied();
        let fin = last.and_then(|l| static_value_no_side_effects(cx, l));
        return !matches!(fin, Some(Static::Str(_)));
    }
    let result = static_value_result(cx, node);
    if is_branch(ast, node) {
        return !matches!(result, Some(Static::Str(_)));
    }
    result.is_some_and(|v| !matches!(v, Static::Str(_)))
}

fn is_clearly_non_string_target(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let value = identifier_value_node(cx, id).unwrap_or(id);
    is_clearly_non_string_kind(ast.node(id))
        || is_static_non_string(cx, id)
        || is_clearly_non_string_kind(ast.node(value))
        || is_number(cx, value)
        || is_boolean(cx, value)
        || is_known_non_string(cx, id)
}

fn is_numeric_literal(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::NumberLit { .. })
}

/// A string literal or a template without substitutions.
fn is_safe_padding(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { .. } => true,
        Node::TemplateLit { parts } => ast.list(parts).len() == 1,
        _ => false,
    }
}

fn is_simple_target(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::Ident { .. })
}

/// `c.repeat(width)` with a static one-character `c`: `(padding, is_space, width)`.
fn repeat_call(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, bool, NodeId)> {
    let ast = cx.ast();
    let m = method_call(ast, skip_parens(ast, id))?;
    let &[width] = m.args else {
        return None;
    };
    if m.name != "repeat" || !m.plain() || matches!(ast.node(width), Node::Spread { .. }) {
        return None;
    }
    let Some(Static::Str(s)) = static_value_result(cx, skip_parens(ast, m.obj)) else {
        return None;
    };
    (s.encode_utf16().count() == 1).then_some((m.obj, s == " ", width))
}

/// `c.repeat(n - s.length)`: `(padding, is_space, n, s)`.
fn repeat_length_call(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, bool, NodeId, NodeId)> {
    let ast = cx.ast();
    let (padding, space, width) = repeat_call(cx, id)?;
    let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *ast.node(skip_parens(ast, width))
    else {
        return None;
    };
    let right = skip_parens(ast, right);
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(right)
    else {
        return None;
    };
    if ident_name(ast, prop) != Some("length") || has_unparenthesized_optional_chain(cx, right) {
        return None;
    }
    Some((padding, space, left, obj))
}

fn replacement(
    cx: &Ctx<'_, '_>,
    method: &str,
    target: NodeId,
    length: NodeId,
    padding: NodeId,
    space: bool,
) -> String {
    let padding = if space {
        String::new()
    } else {
        format!(", {}", paren_text(cx, padding))
    };
    format!(
        "{}.{method}({}{padding})",
        paren_text(cx, target),
        paren_text(cx, length)
    )
}

fn message(method: &str) -> String {
    format!("Prefer `String#{method}()` over manual string padding.")
}

impl PreferStringPadStartEnd {
    fn binary(&self, node: NodeId, left: NodeId, right: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let start = repeat_length_call(cx, left).and_then(|(padding, space, length, target)| {
            (is_same_reference(cx, right, target) && !is_clearly_non_string_target(cx, right))
                .then_some(("padStart", right, padding, space, length))
        });
        let found = start.or_else(|| {
            repeat_length_call(cx, right).and_then(|(padding, space, length, target)| {
                (is_same_reference(cx, left, target) && !is_clearly_non_string_target(cx, left))
                    .then_some(("padEnd", left, padding, space, length))
            })
        });
        let Some((method, target, padding, space, length)) = found else {
            return;
        };
        let span = cx.span(node);
        let can_fix = is_simple_target(ast, target)
            && is_numeric_literal(ast, length)
            && is_safe_padding(ast, padding)
            && !has_comment_in(cx, span);
        let fix = can_fix.then(|| {
            let text = replacement(cx, method, target, length, padding, space);
            let semi = if needs_semicolon(cx, span.start, &text) {
                ";"
            } else {
                ""
            };
            Fix::replace(span, format!("{semi}{text}"))
        });
        cx.report_node(node, message(method))
            .id("prefer-string-pad-start-end")
            .fix_opt(fix);
    }

    fn slice(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(m) = method_call(ast, node) else {
            return;
        };
        if m.name != "slice"
            || !m.plain()
            || m.args.is_empty()
            || m.args.len() > 2
            || m.args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } = *ast.node(skip_parens(ast, m.obj))
        else {
            return;
        };
        let start = match m.args {
            &[first] => repeat_call(cx, left).and_then(|(padding, space, width)| {
                let Node::Unary {
                    op: UnaryOp::Minus,
                    arg,
                } = *ast.node(skip_parens(ast, first))
                else {
                    return None;
                };
                (is_numeric_literal(ast, width)
                    && is_same_reference(cx, width, arg)
                    && is_simple_target(ast, right)
                    && !is_clearly_non_string_target(cx, skip_parens(ast, right)))
                .then_some(("padStart", right, padding, space, width))
            }),
            _ => None,
        };
        let found = start.or_else(|| match m.args {
            &[first, second] => repeat_call(cx, right).and_then(|(padding, space, width)| {
                (matches!(*ast.node(skip_parens(ast, first)), Node::NumberLit { value } if value.get() == 0.0)
                    && is_numeric_literal(ast, width)
                    && is_same_reference(cx, width, second)
                    && is_simple_target(ast, left)
                    && !is_clearly_non_string_target(cx, skip_parens(ast, left)))
                .then_some(("padEnd", left, padding, space, width))
            }),
            _ => None,
        });
        let Some((method, target, padding, space, width)) = found else {
            return;
        };
        let span = cx.span(node);
        let suggestion = (is_safe_padding(ast, padding) && !has_comment_in(cx, span))
            .then(|| replacement(cx, method, target, width, padding, space));
        let report = cx
            .report_node(node, message(method))
            .id("prefer-string-pad-start-end");
        if let Some(text) = suggestion {
            report.suggest(
                "suggest-padding-method",
                format!("Replace with `String#{method}()`."),
                Fix::replace(span, text),
            );
        }
    }
}

impl Rule for PreferStringPadStartEnd {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.ast().node(node) {
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            } => {
                let ast = cx.ast();
                self.binary(node, skip_parens(ast, left), skip_parens(ast, right), cx);
            }
            Node::Call { .. } => self.slice(node, cx),
            _ => {}
        }
    }
}
