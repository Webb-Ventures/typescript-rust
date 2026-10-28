//! `unicorn/prefer-combined-guards`: two guards in a row (`if` without `else` whose body ends
//! in the same `return`/`throw`/`break`/`continue`/`process.exit()`) become one joined
//! with `||`.
//!
//! The bodies must have the same length, the same text before the exit (no tagged template)
//! and the exit text must match (parentheses aside). Without `checkMultiStatementBodies` a
//! body is one statement; without `checkCompoundConditions` both tests are plain (no `&&`,
//! `??`, `?:`, assignment, sequence, arrow or `yield`; `||` of plain ones is fine). With the
//! TypeScript parser and no type information, narrowing may differ between the bodies, so
//! the exit must not mention a reference, `this`, `super` or a JSX name and the body is one
//! statement. The fix keeps the second body; it is withheld when a comment sits in the
//! range or right before the first guard.

use super::prelude::*;
use super::unicorn_conditions::{
    global_value_available, is_ts_wrapper, logical_child_text, unwrap_ts_and_parens,
};
use super::unicorn_text::is_parenthesized;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-combined-guards",
        "Prefer combining consecutive guard clauses with the same exit.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferCombinedGuards>,
};

#[derive(Debug)]
struct PreferCombinedGuards {
    compound: bool,
    multi_statement: bool,
}

impl FromOptions for PreferCombinedGuards {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let obj = obj_opt(opts, 0);
        Ok(PreferCombinedGuards {
            compound: bool_field(obj, "checkCompoundConditions", false),
            multi_statement: bool_field(obj, "checkMultiStatementBodies", false),
        })
    }
}

/// The statement before `node` in its statement list.
fn previous_sibling(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let list = match *ast.node(cx.parent(node)?) {
        Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => stmts,
        Node::SwitchCase { body, .. } | Node::StaticBlock { body } => body,
        _ => return None,
    };
    let list = ast.list(list);
    let i = list.iter().position(|&s| s == node)?;
    i.checked_sub(1).map(|j| list[j])
}

/// `process.exit(..)` on the global `process`, not optional or computed.
fn is_process_exit_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(unwrap_ts_and_parens(ast, callee))
    else {
        return false;
    };
    let obj = skip_parens(ast, obj);
    ident_name(ast, obj) == Some("process")
        && ident_name(ast, prop) == Some("exit")
        && global_value_available(cx, obj, "process")
}

/// The statements of a guard `if`, the last being its exit.
fn guard_statements(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    multi_statement: bool,
) -> Option<(NodeId, NodeId, Vec<NodeId>)> {
    let ast = cx.ast();
    let Node::If {
        test,
        cons,
        alt: None,
    } = *ast.node(node)
    else {
        return None;
    };
    let statements = match *ast.node(cons) {
        Node::Block { stmts } => ast.list(stmts).to_vec(),
        _ => vec![cons],
    };
    if statements.is_empty() || (statements.len() > 1 && !multi_statement) {
        return None;
    }
    let exit = *statements.last()?;
    let is_exit = match *ast.node(exit) {
        Node::Return { .. } | Node::Throw { .. } | Node::Break { .. } | Node::Continue { .. } => {
            true
        }
        Node::ExprStmt { expr } => is_process_exit_call(cx, skip_parens(ast, expr)),
        _ => false,
    };
    is_exit.then_some((test, cons, statements))
}

/// The exit's expression, argument or label text.
fn exit_text<'a>(cx: &Ctx<'a, '_>, exit: NodeId) -> &'a str {
    let ast = cx.ast();
    let value = match *ast.node(exit) {
        Node::ExprStmt { expr } => Some(expr),
        Node::Return { arg } => arg,
        Node::Throw { arg } => Some(arg),
        Node::Break { label } | Node::Continue { label } => label,
        _ => None,
    };
    value.map_or("", |v| cx.text(skip_parens(ast, v)))
}

/// The expressions an exit evaluates: the `process.exit` arguments or the argument.
fn exit_expressions(ast: &Ast, exit: NodeId) -> Vec<NodeId> {
    match *ast.node(exit) {
        Node::ExprStmt { expr } => match *ast.node(skip_parens(ast, expr)) {
            Node::Call { args, .. } => ast.list(args).to_vec(),
            _ => Vec::new(),
        },
        Node::Return { arg: Some(arg) } | Node::Throw { arg } => vec![arg],
        _ => Vec::new(),
    }
}

/// A node in `root` (itself included) matches `pred`.
fn contains(cx: &Ctx<'_, '_>, root: NodeId, pred: &dyn Fn(NodeId) -> bool) -> bool {
    struct Find<'p> {
        pred: &'p dyn Fn(NodeId) -> bool,
        found: bool,
    }
    impl Visitor for Find<'_> {
        fn enter(&mut self, _ast: &Ast, id: NodeId) -> Walk {
            if (self.pred)(id) {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find { pred, found: false };
    walk(cx.ast(), root, &mut f);
    f.found
}

fn has_tagged_template(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    contains(cx, root, &|id| {
        matches!(cx.node(id), Node::TaggedTemplate { .. })
    })
}

/// Something whose type a guard may narrow: a reference (not a type name), `this`,
/// `super` or a JSX name.
fn has_narrowable(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let sem = cx.semantic();
    contains(cx, root, &|id| match cx.node(id) {
        Node::Ident { .. } => {
            sem.reference_at(id).is_some()
                && !cx
                    .parent(id)
                    .is_some_and(|p| matches!(cx.node(p), Node::TypeRef { .. }))
        }
        Node::This {}
        | Node::Super {}
        | Node::JsxOpening { .. }
        | Node::JsxClosing { .. }
        | Node::JsxAttr { .. }
        | Node::JsxMemberExpr { .. }
        | Node::JsxNamespacedName { .. } => true,
        _ => false,
    })
}

/// `isSimpleCondition`: no `&&`/`??`, `?:`, assignment, sequence, arrow or `yield`; `||`
/// only of simple conditions and not under `!`/`await`.
fn is_simple_condition(ast: &Ast, id: NodeId, allow_or: bool, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    match *ast.node(unwrap_ts_and_parens(ast, id)) {
        Node::Binary {
            left,
            op: op @ (BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce),
            right,
        } => {
            allow_or
                && op == BinaryOp::LogicalOr
                && is_simple_condition(ast, left, true, depth + 1)
                && is_simple_condition(ast, right, true, depth + 1)
        }
        Node::Unary { arg, .. } | Node::Await { arg } => {
            is_simple_condition(ast, arg, false, depth + 1)
        }
        Node::Conditional { .. }
        | Node::Assign { .. }
        | Node::Seq { .. }
        | Node::Arrow { .. }
        | Node::Yield { .. } => false,
        _ => true,
    }
}

/// A test as an operand of `||`: a bare TS wrapper gets parentheses.
fn condition_text(cx: &Ctx<'_, '_>, test: NodeId) -> String {
    let inner = skip_parens(cx.ast(), test);
    if is_ts_wrapper(cx.node(inner)) && !is_parenthesized(cx, inner) {
        return format!("({})", cx.text(inner));
    }
    logical_child_text(cx, inner, BinaryOp::LogicalOr)
}

impl Rule for PreferCombinedGuards {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((test, cons, statements)) = guard_statements(cx, node, self.multi_statement)
        else {
            return;
        };
        let Some(previous) = previous_sibling(cx, node) else {
            return;
        };
        let Some((previous_test, _, previous_statements)) =
            guard_statements(cx, previous, self.multi_statement)
        else {
            return;
        };
        let (Some(&exit), Some(&previous_exit)) = (statements.last(), previous_statements.last())
        else {
            return;
        };
        let untyped_ts = cx.env().typescript;
        if ast.node(exit).kind() != ast.node(previous_exit).kind()
            || exit_text(cx, exit) != exit_text(cx, previous_exit)
            || exit_expressions(ast, exit)
                .iter()
                .any(|&e| has_tagged_template(cx, e) || (untyped_ts && has_narrowable(cx, e)))
            || previous_statements.len() != statements.len()
            || statements[..statements.len() - 1]
                .iter()
                .zip(&previous_statements)
                .any(|(&s, &p)| cx.text(s) != cx.text(p) || has_tagged_template(cx, s))
            || (!self.compound
                && (!is_simple_condition(ast, previous_test, true, 0)
                    || !is_simple_condition(ast, test, true, 0)))
            || (untyped_ts && statements.len() != 1)
        {
            return;
        }
        let span = cx.span(node);
        let start = cx.span(previous).start;
        let src = cx.src();
        let comment_start = skip_trivia_backward(ast, src, start as usize) as u32;
        let has_comment = !ast
            .comments_in(Span::new(comment_start, span.end))
            .is_empty();
        let fix = (!has_comment).then(|| {
            let close = skip_trivia_backward(ast, src, cx.span(cons).start as usize) as u32;
            let suffix = &src[close as usize..span.end as usize];
            Fix::replace(
                Span::new(start, span.end),
                format!(
                    "if ({} || {}){suffix}",
                    condition_text(cx, previous_test),
                    condition_text(cx, test)
                ),
            )
        });
        cx.report(span, "Combine this guard with the previous one using `||`.")
            .id("prefer-combined-guards")
            .fix_opt(fix);
    }
}
