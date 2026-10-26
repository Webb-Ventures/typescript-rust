//! `unicorn/no-useless-length-check`: `a.length === 0 || a.every(...)` and
//! `a.length > 0 && a.some(...)` (`!== 0` too), in either order and anywhere in a chain of
//! the same operator (parentheses looked through), and `if (a.length > 0)` around nothing
//! but a `for (... of a)` loop.
//!
//! The check is removed with the operator next to it. That is a fix when it can't change
//! behaviour: a leading check whose call's arguments have no side effect, or a trailing
//! check after a call whose callback is an inline function without side effects (no calls,
//! assignments, tagged templates, spreads...); otherwise a suggestion. A removal that
//! leaves the statement starting with `(`/`[`/... after a statement without `;` gets one.
//! No removal when a comment sits in the removed range. Receivers known not to be indexed
//! collections are skipped, and so is a loop whose `of` expression reads its own
//! block-scoped bindings. The `if` fix puts the loop in its place, reindented, unless that
//! loses comments, a token of the loop spans lines, or the `if` or loop doesn't start its
//! line (single-line loops are fine).

use super::prelude::*;
use super::unicorn_static::has_side_effect;
use super::unicorn_types::is_known_non_indexed_collection;
use super::unicorn_util::indent_before;
use super::unicorn_util::{joins_previous_statement, parenthesized_span, skip_wrappers};
use std::borrow::Cow;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-length-check",
        "Disallow useless array length check.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoUselessLengthCheck>,
};

#[derive(Debug, Default)]
struct NoUselessLengthCheck;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Zero,
    NonZero,
}

/// `x.length <op> 0`: the check kind and the `length` member.
fn length_check(ast: &Ast, src: &str, id: NodeId) -> Option<(Check, NodeId)> {
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let right = skip_parens(ast, right);
    if !matches!(ast.node(right), Node::NumberLit { .. }) || ast.span(right).text(src) != "0" {
        return None;
    }
    let member = skip_parens(ast, left);
    let Node::Member {
        optional: false,
        prop,
        ..
    } = *ast.node(member)
    else {
        return None;
    };
    if ident_name(ast, prop) != Some("length") {
        return None;
    }
    let check = match op {
        BinaryOp::StrictEq => Check::Zero,
        BinaryOp::Gt | BinaryOp::StrictNotEq => Check::NonZero,
        _ => return None,
    };
    Some((check, member))
}

fn logical_op(ast: &Ast, id: NodeId) -> Option<BinaryOp> {
    match *ast.node(id) {
        Node::Binary {
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
            ..
        } => Some(op),
        _ => None,
    }
}

/// The operands of a chain of `op`, through parentheses, left to right.
fn flatten(ast: &Ast, id: NodeId, op: BinaryOp, out: &mut Vec<NodeId>, depth: u32) {
    let inner = skip_parens(ast, id);
    match *ast.node(inner) {
        Node::Binary { left, op: o, right } if o == op && depth < 512 => {
            flatten(ast, left, op, out, depth + 1);
            flatten(ast, right, op, out, depth + 1);
        }
        _ => out.push(inner),
    }
}

/// The static name of a member's property (`a.b`, `a["b"]`, `a[0]`).
fn static_property<'a>(ast: &'a Ast, id: NodeId) -> Option<Cow<'a, str>> {
    match *ast.node(id) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(Cow::Borrowed),
        Node::Index { index, .. } => {
            let index = skip_parens(ast, index);
            match *ast.node(index) {
                Node::StringLit { value } => Some(Cow::Borrowed(ast.atom(value))),
                Node::NumberLit { value } => Some(Cow::Owned(number_to_string(value.get()))),
                _ => static_string(ast, index).map(Cow::Borrowed),
            }
        }
        _ => None,
    }
}

fn static_string(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The plugin's `isSameReference`.
fn same_reference(ast: &Ast, src: &str, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > 128 {
        return false;
    }
    let a = skip_wrappers(ast, a);
    let b = skip_wrappers(ast, b);
    if let Some(s) = static_string(ast, a) {
        return static_string(ast, b) == Some(s);
    }
    match (ast.node(a), ast.node(b)) {
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (Node::Ident { .. }, Node::Ident { .. }) => ident_name(ast, a) == ident_name(ast, b),
        (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => {
            ast.atom(*x) == ast.atom(*y)
        }
        (Node::NumberLit { value: x }, Node::NumberLit { value: y }) => x.get() == y.get(),
        (Node::BoolLit { value: x }, Node::BoolLit { value: y }) => x == y,
        (Node::NullLit {}, Node::NullLit {}) => true,
        (Node::RegexLit { .. }, Node::RegexLit { .. })
        | (Node::BigIntLit { .. }, Node::BigIntLit { .. }) => {
            ast.span(a).text(src) == ast.span(b).text(src)
        }
        (
            Node::Member { obj: oa, .. } | Node::Index { obj: oa, .. },
            Node::Member { obj: ob, .. } | Node::Index { obj: ob, .. },
        ) => {
            let (oa, ob) = (*oa, *ob);
            if let Some(name) = static_property(ast, a) {
                return same_reference(ast, src, oa, ob, depth + 1)
                    && static_property(ast, b).is_some_and(|n| n == name);
            }
            match (ast.node(a), ast.node(b)) {
                (Node::Index { index: ia, .. }, Node::Index { index: ib, .. }) => {
                    same_reference(ast, src, oa, ob, depth + 1)
                        && same_reference(ast, src, *ia, *ib, depth + 1)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// `x.method(...)` (plain member and call) with `x` the length check's object.
fn matching_call(cx: &Ctx<'_, '_>, cond: NodeId, method: &str, length: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(cond)
    else {
        return false;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return false;
    };
    let Node::Member {
        obj: length_obj, ..
    } = *ast.node(length)
    else {
        return false;
    };
    ident_name(ast, prop) == Some(method)
        && same_reference(ast, cx.src(), length_obj, obj, 0)
        && !is_known_non_indexed_collection(cx, obj)
}

/// A tagged template or spread anywhere inside `span`.
fn has_tag_or_spread(ast: &Ast, span: Span) -> bool {
    (1..ast.node_count() as u32)
        .filter_map(NodeId::from_raw)
        .any(|id| {
            let s = ast.span(id);
            s.start >= span.start
                && s.end <= span.end
                && matches!(
                    ast.node(id),
                    Node::TaggedTemplate { .. } | Node::Spread { .. }
                )
        })
}

/// An inline function whose parameters and body have no side effects.
fn side_effect_free_function(cx: &Ctx<'_, '_>, id: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let Some(id) = id.map(|i| skip_parens(ast, i)) else {
        return false;
    };
    let (params, body) = match *ast.node(id) {
        Node::Arrow { params, body, .. } => (params, body),
        Node::FunctionExpr {
            params,
            body: Some(body),
            ..
        } => (params, body),
        _ => return false,
    };
    ast.list(params)
        .iter()
        .chain(std::iter::once(&body))
        .all(|&c| !has_side_effect(ast, c) && !has_tag_or_spread(ast, cx.span(c)))
}

/// A token of `span` that spans lines: a string or template piece with a line break.
fn has_multiline_token(cx: &Ctx<'_, '_>, span: Span) -> bool {
    let ast = cx.ast();
    (1..ast.node_count() as u32)
        .filter_map(NodeId::from_raw)
        .any(|id| {
            let s = ast.span(id);
            s.start >= span.start
                && s.end <= span.end
                && matches!(
                    ast.node(id),
                    Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText { .. }
                )
                && s.text(cx.src())
                    .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
        })
}

/// Moves every later line of `text` from `from` indentation to `to`; blank lines become
/// empty.
fn reindent(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut first = true;
    loop {
        let brk = rest
            .char_indices()
            .find(|&(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'));
        let (line, next) = match brk {
            Some((i, c)) => {
                let len = if rest[i..].starts_with("\r\n") {
                    2
                } else {
                    c.len_utf8()
                };
                (&rest[..i], Some((&rest[i..i + len], &rest[i + len..])))
            }
            None => (rest, None),
        };
        if first {
            out.push_str(line);
            first = false;
        } else if line.trim().is_empty() {
        } else if let Some(stripped) = line.strip_prefix(from) {
            out.push_str(to);
            out.push_str(stripped);
        } else {
            out.push_str(line);
        }
        match next {
            Some((brk, after)) => {
                out.push_str(brk);
                rest = after;
            }
            None => break,
        }
    }
    out
}

impl NoUselessLengthCheck {
    fn check_chain(cx: &mut Ctx<'_, '_>, root: NodeId, op: BinaryOp) {
        let ast = cx.ast();
        let src = cx.src();
        let mut operands = Vec::new();
        flatten(ast, root, op, &mut operands, 0);
        for (i, &node) in operands.iter().enumerate() {
            let Some((check, member)) = length_check(ast, src, node) else {
                continue;
            };
            if parent_skip_parens(ast, cx.file.parents, node)
                .is_none_or(|p| logical_op(ast, p).is_none())
            {
                continue;
            }
            let method = match (op, check) {
                (BinaryOp::LogicalOr, Check::Zero) => "every",
                (BinaryOp::LogicalAnd, Check::NonZero) => "some",
                _ => continue,
            };
            let previous = i.checked_sub(1).map(|j| operands[j]);
            let next = operands.get(i + 1).copied();
            let leading = next.is_some_and(|n| matching_call(cx, n, method, member));
            if !leading && !previous.is_some_and(|p| matching_call(cx, p, method, member)) {
                continue;
            }
            let call_args = |c: NodeId| match *ast.node(c) {
                Node::Call { args, .. } => ast.list(args),
                _ => &[],
            };
            let safe = if leading {
                call_args(next.unwrap_or(node))
                    .iter()
                    .all(|&a| !has_side_effect(ast, a))
            } else {
                side_effect_free_function(cx, call_args(previous.unwrap_or(node)).first().copied())
            };
            let fix = Self::removal(cx, node);
            let Node::Member { prop, .. } = *ast.node(member) else {
                continue;
            };
            let span = Span::new(cx.span(prop).start, cx.span(node).end);
            let (message, id) = match check {
                Check::Zero => (
                    "The empty check is useless as `Array#every()` returns `true` for an empty array.",
                    "zero",
                ),
                Check::NonZero => (
                    "The non-empty check is useless as `Array#some()` returns `false` for an empty array.",
                    "non-zero",
                ),
            };
            let report = cx.report(span, message).id(id);
            match fix {
                Some(f) if safe => {
                    report.fix(f);
                }
                Some(f) => {
                    report.suggest("remove-length-check", "Remove the length check.", f);
                }
                None => {}
            }
        }
    }

    /// Deletes the check `node` and the operator joining it to its sibling.
    fn removal(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Fix> {
        let ast = cx.ast();
        let parent = parent_skip_parens(ast, cx.file.parents, node)?;
        let Node::Binary { left, right, .. } = *ast.node(parent) else {
            return None;
        };
        let (l, r) = (cx.span(left), cx.span(right));
        let is_left = skip_parens(ast, left) == node;
        let range = if is_left {
            Span::new(l.start, r.start)
        } else {
            Span::new(l.end, r.end)
        };
        if !ast.comments_in(range).is_empty() {
            return None;
        }
        let src = cx.src();
        let semicolon = is_left
            && src[range.end as usize..]
                .starts_with(['[', '(', '/', '`', '+', '-', '*', ',', '.', '<'])
            && joins_previous_statement(cx, left);
        Some(Fix::replace(range, if semicolon { ";" } else { "" }))
    }

    fn check_if(cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::If {
            test,
            cons,
            alt: None,
        } = *ast.node(node)
        else {
            return;
        };
        let lp = match *ast.node(cons) {
            Node::Block { stmts } => match ast.list(stmts) {
                [only] => *only,
                _ => return,
            },
            _ => cons,
        };
        let Node::ForOf {
            is_await: false,
            left,
            right,
            ..
        } = *ast.node(lp)
        else {
            return;
        };
        let Some((Check::NonZero, member)) = length_check(ast, src, test) else {
            return;
        };
        if matches!(
            *ast.node(skip_parens(ast, test)),
            Node::Binary {
                op: BinaryOp::StrictEq,
                ..
            }
        ) {
            return;
        }
        let Node::Member { obj, prop, .. } = *ast.node(member) else {
            return;
        };
        if !same_reference(ast, src, obj, right, 0)
            || Self::binding_read_in_right(cx, left, right)
            || is_known_non_indexed_collection(cx, right)
        {
            return;
        }
        let span = Span::new(cx.span(prop).start, cx.span(skip_parens(ast, test)).end);
        let lines = cx.file.lines;
        let (if_span, loop_span) = (cx.span(node), cx.span(lp));
        let at_indent = |pos: u32| {
            let line_start = lines.line_start(lines.line_of(pos)).unwrap_or(0);
            indent_before(cx, pos).len() == (pos - line_start) as usize
        };
        let one_line = lines.line_of(loop_span.start) == lines.line_of(loop_span.end);
        let can_unindent = one_line || (at_indent(if_span.start) && at_indent(loop_span.start));
        let loses_comments = ast
            .comments_in(if_span)
            .iter()
            .any(|c| c.span.start < loop_span.start || c.span.end > loop_span.end);
        let fix =
            (can_unindent && !has_multiline_token(cx, loop_span) && !loses_comments).then(|| {
                let text = reindent(
                    loop_span.text(src),
                    indent_before(cx, loop_span.start),
                    indent_before(cx, if_span.start),
                );
                Fix::replace(if_span, text)
            });
        cx.report(
            span,
            "The non-empty check is useless as `for…of` does not iterate over an empty array.",
        )
        .id("for-of")
        .fix_opt(fix);
    }

    /// The loop's block-scoped bindings are read in its `of` expression.
    fn binding_read_in_right(cx: &Ctx<'_, '_>, left: NodeId, right: NodeId) -> bool {
        let ast = cx.ast();
        let Node::VarDecl { kind, decls, .. } = *ast.node(left) else {
            return false;
        };
        if kind == VarKind::Var {
            return false;
        }
        let range = parenthesized_span(cx, right);
        let mut names = Vec::new();
        for &d in ast.list(decls) {
            if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                binding_idents(ast, name, &mut names);
            }
        }
        let sem = cx.semantic();
        names.iter().any(|&n| {
            sem.symbol_of_decl(n).is_some_and(|s| {
                sem.symbol(s).refs.iter().any(|&r| {
                    let p = cx.span(sem.reference(r).node);
                    p.start >= range.start && p.end <= range.end
                })
            })
        })
    }
}

impl Rule for NoUselessLengthCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if matches!(ast.node(node), Node::If { .. }) {
            Self::check_if(cx, node);
            return;
        }
        let Some(op) = logical_op(ast, node) else {
            return;
        };
        let outermost = parent_skip_parens(ast, cx.file.parents, node)
            .is_none_or(|p| logical_op(ast, p) != Some(op));
        if outermost {
            Self::check_chain(cx, node, op);
        }
    }
}
