//! `unicorn/prefer-logical-operator-over-ternary`: ternaries that are really `||`, `&&`,
//! `??` or an optional chain.
//!
//! - One branch a boolean constant (a literal, through `!`/`satisfies`/`as const` and
//!   matching literal-type assertions, or a `const` alias of one in the same function),
//!   the other a known boolean: `a ? true : b` is `a || b`, `a ? b : false` is `a && b`,
//!   and the negated forms negate the test. A fix only outside TypeScript (the parser or
//!   the file), with the test coerced through `Boolean()` when it isn't known boolean.
//!   Two constant branches are left alone.
//! - A nullish test of `x` (`x == null`, `x === null || x === undefined`, ..) with `x` as
//!   the non-nullish branch suggests `x ?? other`; with `undefined` against `x.y` (plain
//!   member, not called, tagged or deleted) it suggests `x?.y`.
//! - `a ? a : b` and `!b ? a : b` suggest `??` and `||`.
//!
//! Suggestions are withheld (the report stays) when a comment would be dropped.

use super::prelude::*;
use super::unicorn_conditions::{
    logical_child_text, negated_expression_text, outermost_ts_wrapper,
};
use super::unicorn_text::{
    call_argument_text, is_parenthesized, needs_parens_at_expression_start, needs_semicolon,
    paren_span, would_remove_comments,
};
use super::unicorn_types::{binding_annotation, is_boolean};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::space_around_keyword;
use crate::semantic::{ScopeId, ScopeKind};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-logical-operator-over-ternary",
        "Prefer using a logical operator over a ternary.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferLogicalOperatorOverTernary>,
};

#[derive(Debug, Default)]
struct PreferLogicalOperatorOverTernary;

const MESSAGE: &str = "Prefer using a logical operator over a ternary.";
const ERROR: &str = "prefer-logical-operator-over-ternary/error";

fn bool_lit(ast: &Ast, id: NodeId) -> Option<bool> {
    match *ast.node(skip_parens(ast, id)) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn is_undefined(ast: &Ast, id: NodeId) -> bool {
    ident_name(ast, skip_parens(ast, id)) == Some("undefined")
}

fn is_null(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::NullLit {})
}

/// The function-level scope (`variableScope`) holding `scope`.
fn variable_scope(cx: &Ctx<'_, '_>, mut scope: ScopeId) -> ScopeId {
    let sem = cx.semantic();
    for _ in 0..100_000 {
        let s = sem.scope(scope);
        if matches!(
            s.kind,
            ScopeKind::Global
                | ScopeKind::Module
                | ScopeKind::Function
                | ScopeKind::ClassFieldInit
                | ScopeKind::StaticBlock
        ) {
            return scope;
        }
        match s.parent {
            Some(p) => scope = p,
            None => return scope,
        }
    }
    scope
}

/// The only declaration of `ident`: a plain `const` with an initializer, declared before it
/// in the same function. Returns the declarator.
fn constant_definition(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let sem = cx.semantic();
    let r = sem.reference_at(ident)?;
    let sym = sem.reference(r).symbol()?;
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls.as_slice() else {
        return None;
    };
    let Node::VarDeclarator {
        name,
        init: Some(_),
        ..
    } = *cx.node(decl.node)
    else {
        return None;
    };
    let is_const = matches!(
        cx.node(decl.parent),
        Node::VarDecl {
            kind: VarKind::Const,
            ..
        }
    );
    (is_const
        && name == decl.name
        && variable_scope(cx, sem.enclosing_scope(ident)) == variable_scope(cx, symbol.scope)
        && sem.scope(symbol.scope).kind != ScopeKind::Switch
        && cx.span(decl.node).end <= cx.span(ident).start)
        .then_some(decl.node)
}

fn declarator_parts(cx: &Ctx<'_, '_>, declarator: NodeId) -> Option<(Option<NodeId>, NodeId)> {
    match *cx.node(declarator) {
        Node::VarDeclarator {
            type_ann,
            init: Some(init),
            ..
        } => Some((type_ann, init)),
        _ => None,
    }
}

fn unwrap_constant_aliases(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    for _ in 0..=cx.ast().nodes.len() {
        let inner = skip_parens(cx.ast(), id);
        if !matches!(cx.node(inner), Node::Ident { .. }) {
            return id;
        }
        match constant_definition(cx, inner).and_then(|d| declarator_parts(cx, d)) {
            Some((_, init)) => id = init,
            None => return id,
        }
    }
    id
}

/// A literal type `true`/`false` (or anything else: `None`).
fn literal_type_value(ast: &Ast, ty: NodeId) -> Option<bool> {
    match *ast.node(ty) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn is_const_assertion(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Const
        }
    )
}

/// `getBooleanConstantValue`.
fn boolean_constant(cx: &Ctx<'_, '_>, id: NodeId) -> Option<bool> {
    let ast = cx.ast();
    let mut annotations: Vec<NodeId> = Vec::new();
    let mut cur = id;
    for _ in 0..=ast.nodes.len() {
        cur = skip_parens(ast, cur);
        match *ast.node(cur) {
            Node::BoolLit { value } => {
                return annotations
                    .iter()
                    .all(|&t| literal_type_value(ast, t) == Some(value))
                    .then_some(value);
            }
            Node::NonNull { expr } | Node::Satisfies { expr, .. } => cur = expr,
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                if !is_const_assertion(ast, ty) {
                    annotations.push(ty);
                }
                cur = expr;
            }
            Node::Ident { .. } => {
                let (type_ann, init) = declarator_parts(cx, constant_definition(cx, cur)?)?;
                if let Some(t) = type_ann {
                    annotations.push(t);
                }
                cur = init;
            }
            _ => return None,
        }
    }
    None
}

/// The longest chain of `const a = b` aliases behind an identifier in `root` reaches
/// `limit`. `isBoolean` recurses through them, and gives up (false) on a chain that deep.
fn has_deep_alias(cx: &Ctx<'_, '_>, root: NodeId, limit: usize) -> bool {
    struct Find<'c, 'a, 'b> {
        cx: &'c Ctx<'a, 'b>,
        limit: usize,
        found: bool,
    }
    impl Visitor for Find<'_, '_, '_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(
                ast.node(id),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            ) {
                return Walk::SkipChildren;
            }
            if !matches!(ast.node(id), Node::Ident { .. }) {
                return Walk::Continue;
            }
            let sem = self.cx.semantic();
            let mut cur = id;
            for _ in 0..self.limit {
                let next = sem
                    .reference_at(cur)
                    .and_then(|r| sem.reference(r).symbol())
                    .and_then(|s| match sem.symbol(s).decls.as_slice() {
                        [d] => match *ast.node(d.node) {
                            Node::VarDeclarator { init: Some(i), .. } => Some(skip_parens(ast, i)),
                            _ => None,
                        },
                        _ => None,
                    })
                    .filter(|&i| matches!(ast.node(i), Node::Ident { .. }));
                match next {
                    Some(n) => cur = n,
                    None => return Walk::Continue,
                }
            }
            self.found = true;
            Walk::Stop
        }
    }
    let mut f = Find {
        cx,
        limit,
        found: false,
    };
    walk(cx.ast(), root, &mut f);
    f.found
}

/// The annotation on the only declaration of the binding `ident` refers to.
fn declared_annotation(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(ident)?).symbol()?;
    let [decl] = sem.symbol(sym).decls.as_slice() else {
        return None;
    };
    binding_annotation(cx, decl.name).0
}

/// `isBoolean`, false for a deep alias chain or a binding typed by a type parameter, true
/// for a call of a binding annotated with a function type returning `boolean`.
fn is_known_boolean(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if has_deep_alias(cx, id, 256) {
        return false;
    }
    match *ast.node(id) {
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let type_param = declared_annotation(cx, id).is_some_and(|t| {
                matches!(*ast.node(t), Node::TypeRef { name, type_args: None }
                    if sem.reference_at(name)
                        .and_then(|r| sem.reference(r).symbol())
                        .is_some_and(|s| sem.symbol(s).flags.contains(SymbolFlags::TYPE_PARAM)))
            });
            if type_param {
                return false;
            }
        }
        Node::Call { callee, .. } => {
            let callee = skip_parens(ast, callee);
            if matches!(ast.node(callee), Node::Ident { .. })
                && declared_annotation(cx, callee).is_some_and(|t| {
                    matches!(*ast.node(t), Node::FunctionType { return_type, .. }
                        if matches!(ast.node(return_type), Node::KeywordType { kind: KeywordTypeKind::Boolean }))
                })
            {
                return true;
            }
        }
        _ => {}
    }
    is_boolean(cx, id)
}

fn has_with_ancestor(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = cx.parent(id);
    while let Some(p) = cur {
        if matches!(cx.node(p), Node::With { .. }) {
            return true;
        }
        cur = cx.parent(p);
    }
    false
}

fn is_ts_file(cx: &Ctx<'_, '_>) -> bool {
    let path = cx.file.path;
    [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|e| path.ends_with(e))
}

/// `isSameNode`: the same reference, or the same text (never through `++`/`--`).
fn is_same_node(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > 100 {
        return false;
    }
    if is_same_reference(cx, a, b) {
        return true;
    }
    let ast = cx.ast();
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    if ast.node(a).kind() != ast.node(b).kind() {
        return false;
    }
    match (*ast.node(a), *ast.node(b)) {
        (Node::Await { arg: x }, Node::Await { arg: y }) => is_same_node(cx, x, y, depth + 1),
        (
            Node::Binary {
                left: l1,
                op: o1,
                right: r1,
            },
            Node::Binary {
                left: l2,
                op: o2,
                right: r2,
            },
        ) if matches!(
            o1,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
        ) =>
        {
            o1 == o2 && is_same_node(cx, l1, l2, depth + 1) && is_same_node(cx, r1, r2, depth + 1)
        }
        (Node::Unary { op: o1, arg: x }, Node::Unary { op: o2, arg: y }) => {
            o1 == o2 && is_same_node(cx, x, y, depth + 1)
        }
        (Node::Update { .. }, _) => false,
        _ => cx.text(a) == cx.text(b),
    }
}

struct NullishCheck {
    reference: NodeId,
    /// `None` for `==`/`!=`, else whether it compares with `null`.
    null: Option<bool>,
    true_when_nullish: bool,
}

fn nullish_binary_check(ast: &Ast, id: NodeId) -> Option<NullishCheck> {
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let true_when_nullish = match op {
        BinaryOp::Eq | BinaryOp::StrictEq => true,
        BinaryOp::NotEq | BinaryOp::StrictNotEq => false,
        _ => return None,
    };
    let kind = |n: NodeId| {
        if is_null(ast, n) {
            Some(true)
        } else if is_undefined(ast, n) {
            Some(false)
        } else {
            None
        }
    };
    let (reference, is_null_kind) = match (kind(left), kind(right)) {
        (Some(k), None) => (right, k),
        (None, Some(k)) => (left, k),
        _ => return None,
    };
    let loose = matches!(op, BinaryOp::Eq | BinaryOp::NotEq);
    Some(NullishCheck {
        reference,
        null: (!loose).then_some(is_null_kind),
        true_when_nullish,
    })
}

/// `getNullishTest`: the reference and whether the test holds for nullish values.
fn nullish_test(cx: &Ctx<'_, '_>, test: NodeId) -> Option<(NodeId, bool)> {
    let ast = cx.ast();
    let check = nullish_binary_check(ast, test);
    if let Some(c) = &check
        && c.null.is_none()
    {
        return Some((c.reference, c.true_when_nullish));
    }
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, test)) else {
        return None;
    };
    let (l, r) = (
        nullish_binary_check(ast, left)?,
        nullish_binary_check(ast, right)?,
    );
    if !is_same_node(cx, l.reference, r.reference, 0) {
        return None;
    }
    let both_kinds = matches!(
        (l.null, r.null),
        (Some(true), Some(false)) | (Some(false), Some(true))
    );
    match op {
        BinaryOp::LogicalOr if l.true_when_nullish && r.true_when_nullish && both_kinds => {
            Some((l.reference, true))
        }
        BinaryOp::LogicalAnd if !l.true_when_nullish && !r.true_when_nullish && both_kinds => {
            Some((l.reference, false))
        }
        _ => None,
    }
}

enum Left {
    Plain,
    Negated,
    Coerced,
}

fn logical_text(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    left: NodeId,
    right: NodeId,
    op: BinaryOp,
    how: Left,
) -> Fix {
    let ast = cx.ast();
    let left_text = match how {
        Left::Negated => negated_expression_text(cx, left),
        Left::Coerced => format!("Boolean({})", call_argument_text(cx, left)),
        Left::Plain => logical_child_text(cx, skip_parens(ast, left), op),
    };
    let mut text = format!(
        "{left_text} {} {}",
        op.as_str(),
        logical_child_text(cx, skip_parens(ast, right), op)
    );
    let inner = skip_parens(ast, left);
    let child_parens = match *cx.node(inner) {
        Node::Binary { op: o, .. } => o != op,
        Node::Await { .. }
        | Node::Conditional { .. }
        | Node::Assign { .. }
        | Node::Arrow { .. }
        | Node::Yield { .. }
        | Node::Seq { .. } => true,
        _ => false,
    };
    if matches!(how, Left::Plain)
        && !is_parenthesized(cx, inner)
        && !child_parens
        && needs_parens_at_expression_start(cx, inner, node)
    {
        text = format!("({text})");
    }
    let span = cx.span(node);
    if needs_semicolon(cx, span.start, &text) {
        text = format!(";{text}");
    }
    let mut fix = Fix::replace(span, text);
    for (s, t) in space_around_keyword(cx, node) {
        fix = fix.and(s, t);
    }
    fix
}

fn report_operators(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    left: NodeId,
    right: NodeId,
    ops: &[BinaryOp],
) {
    let span = cx.span(node);
    if would_remove_comments(cx, span, &[paren_span(cx, left), paren_span(cx, right)]) {
        cx.report(span, MESSAGE).id(ERROR);
        return;
    }
    let fixes: Vec<(BinaryOp, Fix)> = ops
        .iter()
        .map(|&op| (op, logical_text(cx, node, left, right, op, Left::Plain)))
        .collect();
    let mut report = cx.report(span, MESSAGE).id(ERROR);
    for (op, fix) in fixes {
        report = report.suggest(
            "prefer-logical-operator-over-ternary/suggestion",
            format!("Switch to `{}` operator.", op.as_str()),
            fix,
        );
    }
}

/// The text of `member` with `?.` in place of its `.` or `[`.
fn optional_chain_text(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let src = cx.src();
    let span = cx.span(member);
    let (obj, computed) = match *cx.node(member) {
        Node::Member { obj, .. } => (obj, false),
        Node::Index { obj, .. } => (obj, true),
        _ => return None,
    };
    let op_start = skip_trivia_forward(src, paren_span(cx, obj).end as usize);
    let op_end = op_start + 1;
    Some(format!(
        "{}{}{}",
        &src[span.start as usize..op_start],
        if computed { "?.[" } else { "?." },
        &src[op_end..span.end as usize]
    ))
}

fn is_unsafe_optional_chain_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let outer = outermost_ts_wrapper(cx, node);
    let Some(parent) = parent_skip_parens(ast, cx.file.parents, outer) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Unary {
            op: UnaryOp::Delete,
            ..
        } => true,
        Node::Call { callee, .. } => skip_parens(ast, callee) == outer,
        Node::TaggedTemplate { tag, .. } => skip_parens(ast, tag) == outer,
        _ => false,
    }
}

/// Returns true when it reported.
fn nullish_problem(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    test: NodeId,
    cons: NodeId,
    alt: NodeId,
) -> bool {
    let ast = cx.ast();
    let Some((reference, true_when_nullish)) = nullish_test(cx, test) else {
        return false;
    };
    if true_when_nullish && is_same_node(cx, reference, alt, 0) {
        report_operators(cx, node, reference, cons, &[BinaryOp::Coalesce]);
        return true;
    }
    if !true_when_nullish && is_same_node(cx, reference, cons, 0) {
        report_operators(cx, node, reference, alt, &[BinaryOp::Coalesce]);
        return true;
    }
    let (nullish, other) = if true_when_nullish {
        (cons, alt)
    } else {
        (alt, cons)
    };
    let other = skip_parens(ast, other);
    let (obj, optional) = match *ast.node(other) {
        Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => (obj, optional),
        _ => return false,
    };
    if !is_undefined(ast, nullish)
        || optional
        || !is_same_reference(cx, reference, obj)
        || is_unsafe_optional_chain_context(cx, node)
    {
        return false;
    }
    let Some(text) = optional_chain_text(cx, other) else {
        return false;
    };
    let span = cx.span(node);
    let fix = (!would_remove_comments(cx, span, &[])).then(|| {
        let text = if needs_semicolon(cx, span.start, &text) {
            format!(";{text}")
        } else {
            text
        };
        let mut fix = Fix::replace(span, text);
        for (s, t) in space_around_keyword(cx, node) {
            fix = fix.and(s, t);
        }
        fix
    });
    let report = cx
        .report(span, "Prefer using optional chaining over a ternary.")
        .id("prefer-logical-operator-over-ternary/optional-chain-error");
    if let Some(fix) = fix {
        report.suggest(
            "prefer-logical-operator-over-ternary/optional-chain-suggestion",
            "Switch to optional chaining.",
            fix,
        );
    }
    true
}

/// Returns true when it reported.
fn boolean_problem(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    test: NodeId,
    cons: NodeId,
    alt: NodeId,
) -> bool {
    if has_with_ancestor(cx, node) {
        return false;
    }
    let (cons_value, alt_value) = (boolean_constant(cx, cons), boolean_constant(cx, alt));
    let (value, right) = match (cons_value, alt_value) {
        (Some(v), None) => (v, alt),
        (None, Some(v)) => (v, cons),
        _ => return false,
    };
    if !is_known_boolean(cx, right) {
        return false;
    }
    let negate = cons_value == Some(false) || alt_value == Some(true);
    let span = cx.span(node);
    let can_fix = !cx.env().typescript
        && !is_ts_file(cx)
        && !would_remove_comments(cx, span, &[paren_span(cx, test), paren_span(cx, right)]);
    let fix = can_fix.then(|| {
        let how = if negate {
            Left::Negated
        } else if !is_known_boolean(cx, unwrap_constant_aliases(cx, test)) {
            Left::Coerced
        } else {
            Left::Plain
        };
        let op = if value {
            BinaryOp::LogicalOr
        } else {
            BinaryOp::LogicalAnd
        };
        logical_text(cx, node, test, right, op, how)
    });
    cx.report(span, MESSAGE).id(ERROR).fix_opt(fix);
    true
}

impl Rule for PreferLogicalOperatorOverTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Conditional { test, cons, alt } = *cx.node(node) else {
            return;
        };
        if (bool_lit(ast, cons).is_some() && bool_lit(ast, alt).is_some())
            || (!has_with_ancestor(cx, node)
                && boolean_constant(cx, cons).is_some()
                && boolean_constant(cx, alt).is_some())
        {
            return;
        }
        if boolean_problem(cx, node, test, cons, alt) || nullish_problem(cx, node, test, cons, alt)
        {
            return;
        }
        let both = [BinaryOp::Coalesce, BinaryOp::LogicalOr];
        if is_same_node(cx, test, cons, 0) {
            report_operators(cx, node, test, alt, &both);
            return;
        }
        if let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *ast.node(skip_parens(ast, test))
            && is_same_node(cx, arg, alt, 0)
        {
            report_operators(cx, node, arg, cons, &both);
        }
    }
}
