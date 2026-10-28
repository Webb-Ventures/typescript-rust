//! `unicorn/logical-assignment-operators`: ESLint's `logical-assignment-operators`, except
//! that with `enforceForIfStatements` a falsy `if` assignment (`if (!a) a = b`) is reported
//! with two suggestions, `||=` and `??=`, instead of a `||=` fix.
//!
//! `"always"` (the default) reports `a = a || b` (`assignment`), `a || (a = b)` (`logical`)
//! and, with `enforceForIfStatements`, `if (a) a = b` (`&&=`), `if (!a) a = b` (`||=`) and
//! `if (a == null) a = b` / `if (a === null || a === undefined) a = b` (`??=`) whose body is
//! that single assignment. A target that is a plain name (outside a sloppy `with`) is fixed;
//! a property access becomes a suggestion, except that an `if` whose test is not a logical
//! expression and whose target reads one property of a name, `this` or `super` is fixed.
//! `"never"` splits `a ||= b` into `a = a || b`. Nothing is fixed when a comment is inside
//! the node.

use super::prelude::*;
use super::unicorn_operator_assignment::eslint_same_reference;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/logical-assignment-operators",
        "Require or disallow logical assignment operator shorthand.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<LogicalAssignmentOperators>,
};

#[derive(Debug)]
pub(crate) struct LogicalAssignmentOperators {
    never: bool,
    check_if: bool,
    /// ESLint's core rule: a falsy `if` assignment gets the plain `||=` report.
    core: bool,
}

impl FromOptions for LogicalAssignmentOperators {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let never = opts.first().and_then(Value::as_str) == Some("never");
        Ok(LogicalAssignmentOperators {
            never,
            check_if: !never && bool_field(obj_opt(opts, 1), "enforceForIfStatements", false),
            core: false,
        })
    }
}

impl LogicalAssignmentOperators {
    pub(crate) fn core(opts: &[Value]) -> Result<Self, String> {
        Ok(LogicalAssignmentOperators {
            core: true,
            ..Self::from_options(opts)?
        })
    }
}

fn logical_op(op: BinaryOp) -> Option<&'static str> {
    match op {
        BinaryOp::LogicalOr => Some("||"),
        BinaryOp::LogicalAnd => Some("&&"),
        BinaryOp::Coalesce => Some("??"),
        _ => None,
    }
}

fn in_sloppy_with(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    ancestors_inclusive(cx.file.parents, id).any(|a| matches!(cx.node(a), Node::With { .. }))
        && !super::this_binding::is_strict(cx, id)
}

/// ESLint's `cannotBeGetter`: a plain name (not inside a sloppy-mode `with`).
fn cannot_be_getter(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = skip_parens(cx.ast(), id);
    matches!(cx.node(id), Node::Ident { .. }) && !in_sloppy_with(cx, id)
}

/// A name (other than `undefined`) or a property access.
fn is_reference(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { name } => ast.atom(name) != "undefined",
        Node::Member { .. } | Node::Index { .. } => true,
        _ => false,
    }
}

/// The core rule's `isReference`: an optional chain (`a?.b`) is not one.
fn is_reference_core(ast: &Ast, id: NodeId, core: bool) -> bool {
    is_reference(ast, id) && !(core && chain_has_optional(ast, skip_parens(ast, id)))
}

/// The core rule's `isInsideWithBlock`: `id` is in the body of a `with`.
fn in_with_body(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if let Node::With { body, .. } = *cx.node(p)
            && body == cur
        {
            return true;
        }
        cur = p;
    }
    false
}

/// The core rule's `isParenthesised`: a `(` before and a `)` after, which a call's own
/// parentheses around its only argument also are.
fn token_parenthesized(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let src = cx.src();
    let span = cx.span(id);
    let before = skip_trivia_backward(cx.ast(), src, span.start as usize);
    let after = skip_trivia_forward(src, span.end as usize);
    before > 0 && src.as_bytes()[before - 1] == b'(' && src.as_bytes().get(after) == Some(&b')')
}

fn is_undefined_ref(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = skip_parens(cx.ast(), id);
    is_undefined(cx.ast(), id) && !cx.is_shadowed(id, "undefined")
}

fn is_null(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::NullLit {})
}

/// `a === null` / `a === undefined` (either order): the reference and whether it is `null`.
fn strict_nullish(cx: &Ctx<'_, '_>, id: NodeId, core: bool) -> Option<(NodeId, bool)> {
    let ast = cx.ast();
    let void0 = |id: NodeId| {
        core && matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Void, arg }
            if matches!(ast.node(skip_parens(ast, arg)), Node::NumberLit { value } if value.get() == 0.0))
    };
    let Node::Binary {
        left,
        op: BinaryOp::StrictEq,
        right,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    for (r, n) in [(left, right), (right, left)] {
        if is_reference(ast, r) {
            if is_null(ast, n) {
                return Some((r, true));
            }
            if is_undefined_ref(cx, n) || void0(n) {
                return Some((r, false));
            }
        }
    }
    None
}

/// The tested reference and the operator the `if` amounts to.
fn existence(cx: &Ctx<'_, '_>, test: NodeId, core: bool) -> Option<(NodeId, &'static str)> {
    let ast = cx.ast();
    let is_reference = |ast: &Ast, id: NodeId| is_reference_core(ast, id, core);
    let test = skip_parens(ast, test);
    let (negated, base) = match *ast.node(test) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => (true, skip_parens(ast, arg)),
        _ => (false, test),
    };
    if is_reference(ast, base) {
        return Some((base, if negated { "||" } else { "&&" }));
    }
    if negated
        && let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *ast.node(base)
        && is_reference(ast, arg)
    {
        return Some((skip_parens(ast, arg), "&&"));
    }
    // `Boolean(a)`.
    if core
        && let Node::Call { callee, args, .. } = *ast.node(base)
        && let [arg] = ast.list(args)
        && cx.is_global_ref(callee, "Boolean")
        && is_reference(ast, *arg)
    {
        return Some((skip_parens(ast, *arg), if negated { "||" } else { "&&" }));
    }
    let is_undefined_ref = |cx: &Ctx<'_, '_>, id: NodeId| {
        is_undefined_ref(cx, id)
            || (core
                && matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Void, arg }
                    if matches!(ast.node(skip_parens(ast, arg)), Node::NumberLit { value } if value.get() == 0.0)))
    };
    let strict_nullish = |cx: &Ctx<'_, '_>, id: NodeId| strict_nullish(cx, id, core);
    match *ast.node(test) {
        Node::Binary {
            left,
            op: BinaryOp::Eq,
            right,
        } => {
            let (r, n) = if is_reference(ast, left) {
                (left, right)
            } else {
                (right, left)
            };
            (is_reference(ast, r) && (is_null(ast, n) || is_undefined_ref(cx, n)))
                .then(|| (skip_parens(ast, r), "??"))
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr,
            right,
        } => {
            let (l, l_null) = strict_nullish(cx, left)?;
            let (r, r_null) = strict_nullish(cx, right)?;
            (l_null != r_null && eslint_same_reference(ast, l, r, true))
                .then(|| (skip_parens(ast, l), "??"))
        }
        _ => None,
    }
}

/// ESLint's `accessesSingleProperty`: `a.b`, `this.b`, `super.b`, or `a[k]` whose key is
/// not itself a property access.
fn accesses_single_property(cx: &Ctx<'_, '_>, id: NodeId, core: bool) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if core && in_with_body(cx, id) && !super::this_binding::is_strict(cx, id) {
        return matches!(cx.node(id), Node::Ident { .. });
    }
    if !core && in_sloppy_with(cx, id) {
        return false;
    }
    let base = |obj: NodeId| {
        matches!(
            ast.node(obj),
            Node::Ident { .. } | Node::This {} | Node::Super {}
        )
    };
    match *ast.node(id) {
        Node::Member { obj, .. } => base(obj),
        Node::Index { obj, index, .. } => {
            base(obj)
                && !matches!(
                    ast.node(skip_parens(ast, index)),
                    Node::Member { .. } | Node::Index { .. }
                )
                && !chain_has_optional(ast, index)
        }
        _ => false,
    }
}

fn has_comments(cx: &Ctx<'_, '_>, span: Span) -> bool {
    !cx.ast().comments_in(span).is_empty()
}

fn report_with(
    cx: &mut Ctx<'_, '_>,
    span: Span,
    msg: String,
    id: &'static str,
    fix: Option<Fix>,
    as_fix: bool,
    sugg: (&'static str, String),
) {
    let r = cx.report(span, msg).id(id);
    match fix {
        Some(f) if as_fix => {
            r.fix(f);
        }
        Some(f) => {
            r.suggest(sugg.0, sugg.1, f);
        }
        None => {}
    }
}

impl LogicalAssignmentOperators {
    fn cannot_be_getter(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        if !self.core {
            return cannot_be_getter(cx, id);
        }
        let id = skip_parens(cx.ast(), id);
        matches!(cx.node(id), Node::Ident { .. })
            && (!in_with_body(cx, id) || super::this_binding::is_strict(cx, id))
    }

    fn check_assign(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Assign { left, op, right } = *cx.node(node) else {
            return;
        };
        let span = cx.span(node);
        let eq_at =
            skip_trivia_forward(src, super::unicorn_text::paren_span(cx, left).end as usize) as u32;
        if self.never {
            let lop = match op {
                AssignOp::OrAssign => "||",
                AssignOp::AndAssign => "&&",
                AssignOp::CoalesceAssign => "??",
                _ => return,
            };
            let mut fix = None;
            if !has_comments(cx, span) {
                let op_end = eq_at + 3;
                let r = skip_parens(ast, right);
                let looser = precedence(ast, r) <= if lop == "&&" { 5 } else { 4 };
                let mixed = lop == "??"
                    && matches!(
                        ast.node(r),
                        Node::Binary {
                            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                            ..
                        }
                    );
                let mut edits = vec![(
                    Span::new(eq_at, op_end),
                    format!("= {} {lop}", cx.text(skip_parens(ast, left))),
                )];
                if r == right && (looser || mixed) {
                    edits.push((Span::at(cx.span(r).start), "(".to_owned()));
                    edits.push((Span::at(cx.span(r).end), ")".to_owned()));
                }
                fix = Some(super::unicorn_util::with_edits(
                    Fix::replace(edits[0].0, edits[0].1.clone()),
                    edits[1..].to_vec(),
                ));
            }
            let as_fix = self.cannot_be_getter(cx, left);
            report_with(
                cx,
                span,
                format!(
                    "Unexpected logical operator assignment ({}) shorthand.",
                    op.as_str()
                ),
                "unexpected",
                fix,
                as_fix,
                (
                    "separate",
                    "Separate the logical assignment into an assignment with a logical operator."
                        .to_owned(),
                ),
            );
            return;
        }
        if op != AssignOp::Assign {
            return;
        }
        let r = skip_parens(ast, right);
        let Node::Binary {
            left: ll,
            op: lop,
            right: lr,
        } = *ast.node(r)
        else {
            return;
        };
        let Some(lop_text) = logical_op(lop) else {
            return;
        };
        // The core rule looks through `a || b || c` to its leftmost operand.
        let (mut inner, mut ll, mut lr) = (r, ll, lr);
        while self.core
            && let Node::Binary {
                left: l2,
                op: op2,
                right: r2,
            } = *ast.node(ll)
            && op2 == lop
        {
            (inner, ll, lr) = (ll, l2, r2);
        }
        let lop = lop_text;
        if !eslint_same_reference(ast, left, ll, true) {
            return;
        }
        let operator = format!("{lop}=");
        let mut fix = None;
        if !has_comments(cx, span) {
            let first = super::unicorn_text::paren_span(cx, lr).start;
            fix = Some(super::unicorn_util::with_edits(
                Fix::replace(Span::at(eq_at), lop.to_owned()),
                vec![(Span::new(cx.span(inner).start, first), String::new())],
            ));
        }
        let as_fix = self.cannot_be_getter(cx, left);
        report_with(
            cx,
            span,
            format!("Assignment (=) can be replaced with operator assignment ({operator})."),
            "assignment",
            fix,
            as_fix,
            (
                "useLogicalOperator",
                format!("Convert this assignment to use the operator {operator}."),
            ),
        );
    }

    fn check_logical(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let Some(lop) = logical_op(op) else {
            return;
        };
        let assign = skip_parens(ast, right);
        let Node::Assign {
            left: target,
            op: AssignOp::Assign,
            ..
        } = *ast.node(assign)
        else {
            return;
        };
        if !eslint_same_reference(ast, left, target, true)
            || (self.core && !is_reference_core(ast, left, true))
        {
            return;
        }
        let span = cx.span(node);
        let operator = format!("{lop}=");
        let mut fix = None;
        if !has_comments(cx, span) {
            let parent = parent_skip_parens(ast, cx.file.parents, node);
            let parenthesized = if self.core {
                token_parenthesized(cx, node)
            } else {
                super::unicorn_text::is_parenthesized(cx, node)
            };
            // Any parent but a statement, sequence, assignment, arrow or `yield` binds
            // tighter than the new assignment.
            let needs_outer = parent.is_some_and(|p| {
                !matches!(cx.node(p), Node::ExprStmt { .. }) && precedence(ast, p) > 1
            });
            let a = cx.span(assign);
            let target_end = super::unicorn_text::paren_span(cx, target).end;
            let eq_at = skip_trivia_forward(src, target_end as usize) as u32;
            let mut edits = [
                (Span::new(span.start, a.start), String::new()),
                (Span::at(eq_at), lop.to_owned()),
                (Span::new(a.end, span.end), String::new()),
            ];
            if !parenthesized && needs_outer {
                edits[0].1 = "(".to_owned();
                edits[2].1 = ")".to_owned();
            }
            fix = Some(super::unicorn_util::with_edits(
                Fix::replace(edits[0].0, edits[0].1.clone()),
                edits[1..].to_vec(),
            ));
        }
        let as_fix = self.cannot_be_getter(cx, left)
            || (self.core && accesses_single_property(cx, left, true));
        report_with(
            cx,
            span,
            format!("Logical expression can be replaced with an assignment ({operator})."),
            "logical",
            fix,
            as_fix,
            (
                "convertLogical",
                format!(
                    "Replace this logical expression with an assignment with the operator {operator}."
                ),
            ),
        );
    }

    fn check_if(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::If {
            test,
            cons,
            alt: None,
        } = *cx.node(node)
        else {
            return;
        };
        let has_body = matches!(cx.node(cons), Node::Block { .. });
        let body = match *cx.node(cons) {
            Node::Block { stmts } => match ast.list(stmts) {
                [only] => *only,
                _ => return,
            },
            _ => cons,
        };
        let Node::ExprStmt { expr } = *cx.node(body) else {
            return;
        };
        let assign = if self.core {
            skip_parens(ast, expr)
        } else {
            expr
        };
        let Node::Assign {
            left: target,
            op: AssignOp::Assign,
            ..
        } = *cx.node(assign)
        else {
            return;
        };
        let Some((reference, operator)) = existence(cx, test, self.core) else {
            return;
        };
        if !eslint_same_reference(ast, reference, target, true) {
            return;
        }
        let span = cx.span(node);
        // The fix, when it can be made.
        let mut fix = None;
        if !has_comments(cx, span) {
            let first_body = cx.span(body).start;
            let first_char = src[first_body as usize..].chars().next().unwrap_or(' ');
            let prev = skip_trivia_backward(ast, src, span.start as usize);
            let prev_char = src[..prev].chars().next_back();
            let body_starts_word =
                first_char.is_alphabetic() || first_char == '_' || first_char == '$';
            let asi_risk = prev_char.is_some_and(|c| c != ';' && c != '{') && !body_starts_word;
            if !asi_risk {
                let target_end = super::unicorn_text::paren_span(cx, target).end;
                let eq_at = skip_trivia_forward(src, target_end as usize) as u32;
                let body_span = cx.span(body);
                let mut edits = vec![
                    (Span::at(eq_at), operator.to_owned()),
                    (Span::new(body_span.end, span.end), String::new()),
                ];
                let after_expr = skip_trivia_forward(src, cx.span(expr).end as usize);
                if has_body && src.as_bytes().get(after_expr) != Some(&b';') {
                    edits.push((Span::at(span.end), ";".to_owned()));
                }
                fix = Some(super::unicorn_util::with_edits(
                    Fix::delete(Span::new(span.start, body_span.start)),
                    edits,
                ));
            }
        }
        let test_is_logical = matches!(
            ast.node(skip_parens(ast, test)),
            Node::Binary {
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                ..
            }
        );
        let as_fix = self.cannot_be_getter(cx, reference)
            || (!test_is_logical && accesses_single_property(cx, reference, self.core));
        let msg_for = |op: &str| {
            format!(
                "'if' statement can be replaced with a logical operator assignment with operator {op}."
            )
        };
        if operator == "||" && !self.core {
            // Offered as both `||=` and `??=`.
            let r = cx
                .report(
                    span,
                    "'if' statement can be replaced with a logical operator assignment with operator ||= or ??=.",
                )
                .id("ifNullish");
            if let Some(f) = fix {
                let mut nullish = f.clone();
                for e in &mut nullish.edits {
                    if e.text == "||" {
                        e.text = "??".to_owned();
                    }
                }
                r.suggest(
                    "convertIf",
                    "Replace this 'if' statement with a logical assignment with operator ||=.",
                    f,
                )
                .suggest(
                    "convertIf",
                    "Replace this 'if' statement with a logical assignment with operator ??=.",
                    nullish,
                );
            }
            return;
        }
        let op = format!("{operator}=");
        report_with(
            cx,
            span,
            msg_for(&op),
            "if",
            fix,
            as_fix,
            (
                "convertIf",
                format!(
                    "Replace this 'if' statement with a logical assignment with operator {op}."
                ),
            ),
        );
    }
}

impl Rule for LogicalAssignmentOperators {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign, NodeKind::Binary, NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::Assign { .. } => self.check_assign(node, cx),
            Node::Binary { .. } if !self.never => self.check_logical(node, cx),
            Node::If { .. } if self.check_if => self.check_if(node, cx),
            _ => {}
        }
    }
}
