//! `unicorn/iteration-fallback-style`: an optional loop source is either guarded
//! (`if (items) for (const x of items) ...`) or given an empty fallback
//! (`for (const x of items || [])`), as the option says (`"guard"`, the default, or
//! `"fallback"`).
//!
//! The loop source is the right side of `for ... of` / `for ... in`, or the only argument of
//! `Object.keys`/`values`/`entries` there (object iteration, like `for ... in`, falls back
//! to `{}`; the rest to `[]`). TS wrappers are looked through.
//!
//! - `guard`: a source `x ?? <empty>` / `x || <empty>` is reported at the empty literal.
//!   A synchronous `for ... of` is fixed to `if (x) { <loop> }` (`(x) != null` for `??`)
//!   when the fallback has no TS wrapper and no comment, `x` is a side-effect-free reference,
//!   the loop has no multi-line token, and its parent is neither a label nor an `if`.
//! - `fallback`: `if (x) <loop>` / `if (x != null) <loop>` (no `else`, the loop the only
//!   statement, `x` the same optional-chain-free reference as the source) is reported at the
//!   test. A synchronous `for ... of` whose source is `x` itself is fixed by moving the loop
//!   out with ` || <empty>` / ` ?? <empty>` added, when no comment outside the loop is lost,
//!   nothing spans lines, the `if` starts its line and isn't labelled.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_no_redundant_comparison::{contains_optional_chain, is_reference};
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{needs_semicolon, would_remove_comments};
use super::unicorn_util::{
    has_optional_chain, indent_before, indent_unit, is_same_reference, skip_wrappers,
};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/iteration-fallback-style",
        "Enforce a consistent style for optional loop sources.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<IterationFallbackStyle>,
};

#[derive(Debug, Default)]
struct IterationFallbackStyle {
    fallback: bool,
}

impl FromOptions for IterationFallbackStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first() {
            None => Ok(Self::default()),
            Some(Value::String(s)) if s == "guard" => Ok(Self { fallback: false }),
            Some(Value::String(s)) if s == "fallback" => Ok(Self { fallback: true }),
            Some(v) => Err(format!("invalid option {v}")),
        }
    }
}

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// The loop's `right` (parentheses skipped) and whether it is `for ... of`.
fn loop_parts(ast: &Ast, id: NodeId) -> Option<(NodeId, bool, bool)> {
    match *ast.node(id) {
        Node::ForOf {
            is_await, right, ..
        } => Some((skip_parens(ast, right), true, is_await)),
        Node::ForIn { right, .. } => Some((skip_parens(ast, right), false, false)),
        _ => None,
    }
}

/// `Object.keys/values/entries(arg)` (plain, one non-spread argument) as a `for ... of`
/// source: the argument (parentheses skipped).
fn object_method_arg(ast: &Ast, right: NodeId) -> Option<NodeId> {
    let call = skip_wrappers(ast, right);
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    if !matches!(ast.node(prop), Node::Ident { .. })
        || !matches!(ident_name(ast, prop), Some("keys" | "values" | "entries"))
        || !matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
        || ident_name(ast, skip_parens(ast, obj)) != Some("Object")
    {
        return None;
    }
    let &[arg] = ast.list(args) else {
        return None;
    };
    if matches!(ast.node(arg), Node::Spread { .. }) {
        return None;
    }
    Some(skip_parens(ast, arg))
}

/// `(source node, object iteration)` of a loop: the node as ESTree has it (no parentheses).
fn loop_source(ast: &Ast, lp: NodeId) -> Option<(NodeId, bool)> {
    let (right, of, _) = loop_parts(ast, lp)?;
    if !of {
        return Some((right, true));
    }
    match object_method_arg(ast, right) {
        Some(arg) => Some((arg, true)),
        None => Some((right, false)),
    }
}

/// A string, template piece or JSX text inside `root` that spans lines.
fn has_multiline_token(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(
            ast.node(n),
            Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText { .. }
        ) && ast
            .span(n)
            .text(src)
            .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
        {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// Moves every later line of `text` from `from` indentation to `to`; blank lines become
/// empty. Line breaks are kept.
pub(super) fn reindent(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut first = true;
    let mut rest = text;
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
            Some((lb, after)) => {
                out.push_str(lb);
                rest = after;
            }
            None => break,
        }
    }
    out
}

fn is_empty_literal(ast: &Ast, id: NodeId, object: bool) -> bool {
    match *ast.node(id) {
        Node::ArrayLit { elements } if !object => ast.list(elements).is_empty(),
        Node::ObjectLit { props } if object => ast.list(props).is_empty(),
        _ => false,
    }
}

impl IterationFallbackStyle {
    fn check_loop(&self, cx: &mut Ctx<'_, '_>, lp: NodeId) {
        let ast = cx.ast();
        let Some((source_node, object)) = loop_source(ast, lp) else {
            return;
        };
        let logical = skip_wrappers(ast, source_node);
        let Node::Binary {
            left,
            op: op @ (BinaryOp::Coalesce | BinaryOp::LogicalOr),
            right,
        } = *ast.node(logical)
        else {
            return;
        };
        let fallback = skip_wrappers(ast, right);
        if !is_empty_literal(ast, fallback, object) {
            return;
        }
        let ty = if object { "object" } else { "array" };
        let msg = format!("Prefer an `if` guard over the empty {ty} fallback.");
        let fix = self.guard_fix(cx, lp, logical, left, right, op);
        cx.report_node(fallback, msg).id("preferGuard").fix_opt(fix);
    }

    fn guard_fix(
        &self,
        cx: &Ctx<'_, '_>,
        lp: NodeId,
        logical: NodeId,
        left: NodeId,
        right: NodeId,
        op: BinaryOp,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let (_, of, is_await) = loop_parts(ast, lp)?;
        if !of || is_await {
            return None;
        }
        let source = skip_parens(ast, left);
        let lspan = cx.span(logical);
        if !ast.comments_in(lspan).is_empty()
            || has_multiline_token(cx, lp)
            || is_ts_wrapper(ast, skip_parens(ast, right))
            || !is_reference(ast, source)
            // `a?.b` is a chain expression in ESTree, not a reference.
            || has_optional_chain(ast, source)
            || has_side_effect(ast, source)
            || cx
                .parent(lp)
                .is_some_and(|p| matches!(ast.node(p), Node::Labeled { .. } | Node::If { .. }))
        {
            return None;
        }
        let src = cx.src();
        let span = cx.span(lp);
        let source_text = cx.text(source);
        let loop_text = format!(
            "{}{}{}",
            &src[span.start as usize..lspan.start as usize],
            source_text,
            &src[lspan.end as usize..span.end as usize]
        );
        let indent = indent_before(cx, span.start);
        let body_indent = format!("{indent}{}", indent_unit(cx));
        let condition = if op == BinaryOp::Coalesce {
            format!("({source_text}) != null")
        } else {
            source_text.to_owned()
        };
        let lb = linebreak(src);
        Some(Fix::replace(
            span,
            format!(
                "if ({condition}) {{{lb}{body_indent}{}{lb}{indent}}}",
                reindent(&loop_text, indent, &body_indent)
            ),
        ))
    }

    fn check_if(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
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
                &[s] => s,
                _ => return,
            },
            _ => cons,
        };
        let Some((_, of, is_await)) = loop_parts(ast, lp) else {
            return;
        };
        let mut t = skip_wrappers(ast, test);
        let mut op = "||";
        if !is_reference(ast, t) {
            let Node::Binary {
                left,
                op: BinaryOp::NotEq,
                right,
            } = *ast.node(t)
            else {
                return;
            };
            let is_null = |n: NodeId| matches!(ast.node(skip_parens(ast, n)), Node::NullLit {});
            let (source, other) = if is_null(left) {
                (right, left)
            } else {
                (left, right)
            };
            if !is_null(other) || !is_reference(ast, source) {
                return;
            }
            t = skip_parens(ast, source);
            op = "??";
        }
        let Some((source_node, object)) = loop_source(ast, lp) else {
            return;
        };
        let source = skip_wrappers(ast, source_node);
        if contains_optional_chain(ast, t)
            || contains_optional_chain(ast, source)
            || !is_reference(ast, source)
            || !is_same_reference(cx, t, source)
        {
            return;
        }
        let ty = if object { "object" } else { "array" };
        let msg = format!("Prefer an empty {ty} fallback over the `if` guard.");
        let fix = (of && !is_await)
            .then(|| self.fallback_fix(cx, node, lp, source_node, source, op, object))
            .flatten();
        cx.report_node(skip_parens(ast, test), msg)
            .id("preferFallback")
            .fix_opt(fix);
    }

    #[allow(clippy::too_many_arguments)]
    fn fallback_fix(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        lp: NodeId,
        source_node: NodeId,
        source: NodeId,
        op: &str,
        object: bool,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let span = cx.span(node);
        let lspan = cx.span(lp);
        let lines = cx.file.lines;
        let line_start = lines.line_start(lines.line_of(span.start)).unwrap_or(0);
        if would_remove_comments(cx, span, &[lspan])
            || has_multiline_token(cx, node)
            || source != source_node
            || has_side_effect(ast, source)
            || cx
                .parent(node)
                .is_some_and(|p| matches!(ast.node(p), Node::Labeled { .. }))
            || indent_before(cx, span.start).len() != (span.start - line_start) as usize
        {
            return None;
        }
        let src = cx.src();
        let sspan = cx.span(source_node);
        let fallback = if object { "{}" } else { "[]" };
        let mut text = format!(
            "{}{} {op} {fallback}{}",
            &src[lspan.start as usize..sspan.start as usize],
            cx.text(source_node),
            &src[sspan.end as usize..lspan.end as usize]
        );
        if needs_semicolon(cx, lspan.end, "(") {
            text.push(';');
        }
        let indent = indent_before(cx, span.start);
        Some(Fix::replace(
            span,
            reindent(&text, indent_before(cx, lspan.start), indent),
        ))
    }
}

impl Rule for IterationFallbackStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForOf, NodeKind::ForIn, NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match (self.fallback, cx.node(node)) {
            (true, Node::If { .. }) => self.check_if(cx, node),
            (false, Node::ForOf { .. } | Node::ForIn { .. }) => self.check_loop(cx, node),
            _ => {}
        }
    }
}
