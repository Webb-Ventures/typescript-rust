//! `unicorn/prefer-else-if`: two adjacent `if` chains (no trailing `else`) that compare the
//! same discriminant against disjoint values should be one chain.
//!
//! A test is either a boolean static reference (through `!` and `Boolean(..)`, known
//! boolean) or a `||` of `===` comparisons of one static reference (names, `this`,
//! `super`, static member paths; no optional chains) with literals, static strings or
//! `undefined`. Values are compared by type and value. Skipped when a consequent of the
//! first chain always exits or writes the discriminant (or a prefix of it). The first
//! chain ending in a dangling `if` gets no fix; otherwise `else ` is inserted, as a fix when
//! the discriminant is a plain name and nothing involved has side effects (getters and
//! conversions count), else as a suggestion.

use super::prelude::*;
use super::unicorn_conditions::{always_exits, unwrap_ts_and_parens};
use super::unicorn_no_redundant_comparison::contains_optional_chain;
use super::unicorn_no_top_level_side_effects::{assignment_targets, is_global_reference};
use super::unicorn_text::static_string_value;
use super::unicorn_types::is_boolean;
use super::unicorn_util::has_side_effect_with as has_side_effect;
use super::unicorn_util::{is_same_reference, logical_operands};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-else-if",
        "Prefer `else if` over adjacent `if` statements with related conditions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferElseIf>,
};

#[derive(Debug, Default)]
struct PreferElseIf;

impl FromOptions for PreferElseIf {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(PreferElseIf)
    }
}

const MAX_DEPTH: u32 = 500;

fn is_undefined(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some("undefined")
}

fn is_static_value(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
    ) || static_string_value(ast, id).is_some()
        || is_undefined(ast, id)
}

/// The value's type and value, as `typeof value + ":" + value`.
fn value_key(ast: &Ast, id: NodeId) -> String {
    if is_undefined(ast, id) {
        return "undefined:undefined".to_owned();
    }
    if let Some(s) = static_string_value(ast, id) {
        return format!("string:{s}");
    }
    match *ast.node(id) {
        Node::BigIntLit { value } => format!("bigint:{}", ast.atom(value)),
        Node::BoolLit { value } => format!("boolean:{value}"),
        Node::NullLit {} => "object:null".to_owned(),
        Node::NumberLit { value } => {
            let v = value.get();
            format!("number:{}", if v == 0.0 { 0.0 } else { v })
        }
        _ => String::new(),
    }
}

fn is_static_reference(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let id = unwrap_ts_and_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::This {} | Node::Super {} => true,
        Node::Member { obj, .. } => is_static_reference(ast, obj, depth + 1),
        Node::Index { obj, index, .. } => {
            let index = skip_parens(ast, index);
            (matches!(
                ast.node(index),
                Node::StringLit { .. }
                    | Node::NumberLit { .. }
                    | Node::BigIntLit { .. }
                    | Node::BoolLit { .. }
                    | Node::NullLit {}
            ) || static_string_value(ast, index).is_some())
                && is_static_reference(ast, obj, depth + 1)
        }
        _ => false,
    }
}

fn is_same(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    is_same_reference(cx, a, b)
}

struct Comparison {
    discriminant: NodeId,
    values: Vec<String>,
}

fn boolean_comparison(cx: &Ctx<'_, '_>, test: NodeId) -> Option<Comparison> {
    let ast = cx.ast();
    let mut node = unwrap_ts_and_parens(ast, test);
    let mut value = true;
    for _ in 0..100_000 {
        let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *ast.node(node)
        else {
            break;
        };
        value = !value;
        node = unwrap_ts_and_parens(ast, arg);
    }
    if let Node::Call { callee, args, .. } = *ast.node(node)
        && let [arg] = ast.list(args)
        && ident_name(ast, callee) == Some("Boolean")
        && is_global_reference(cx, callee)
    {
        node = unwrap_ts_and_parens(ast, *arg);
    }
    if contains_optional_chain(ast, node)
        || !is_static_reference(ast, node, 0)
        || !is_boolean(cx, node)
    {
        return None;
    }
    Some(Comparison {
        discriminant: node,
        values: vec![format!("boolean:{value}")],
    })
}

fn comparison(cx: &Ctx<'_, '_>, test: NodeId) -> Option<Comparison> {
    if let Some(c) = boolean_comparison(cx, test) {
        return Some(c);
    }
    let ast = cx.ast();
    let mut discriminant: Option<NodeId> = None;
    let mut values = Vec::new();
    for operand in logical_operands(ast, test, BinaryOp::LogicalOr) {
        let Node::Binary {
            left,
            op: BinaryOp::StrictEq,
            right,
        } = *ast.node(skip_parens(ast, operand))
        else {
            return None;
        };
        let (left, right) = (skip_parens(ast, left), skip_parens(ast, right));
        let left_static = is_static_value(ast, left);
        if left_static == is_static_value(ast, right) {
            return None;
        }
        let (candidate, value) = if left_static {
            (right, left)
        } else {
            (left, right)
        };
        if contains_optional_chain(ast, candidate) || !is_static_reference(ast, candidate, 0) {
            return None;
        }
        if discriminant.is_some_and(|d| !is_same(cx, d, candidate)) {
            return None;
        }
        discriminant.get_or_insert(candidate);
        values.push(value_key(ast, value));
    }
    Some(Comparison {
        discriminant: discriminant?,
        values,
    })
}

/// The `if`s of an `if`/`else if` chain; `None` when it ends with an `else`.
fn if_chain(ast: &Ast, first: NodeId) -> Option<Vec<NodeId>> {
    let mut chain = vec![first];
    let mut cur = first;
    for _ in 0..100_000 {
        let Node::If { alt, .. } = *ast.node(cur) else {
            return None;
        };
        match alt {
            None => return Some(chain),
            Some(a) if matches!(ast.node(a), Node::If { .. }) => {
                chain.push(a);
                cur = a;
            }
            Some(_) => return None,
        }
    }
    None
}

fn chain_comparison(cx: &Ctx<'_, '_>, chain: &[NodeId]) -> Option<Comparison> {
    let ast = cx.ast();
    let mut discriminant: Option<NodeId> = None;
    let mut values = Vec::new();
    for &stmt in chain {
        let Node::If { test, .. } = *ast.node(stmt) else {
            return None;
        };
        let c = comparison(cx, test)?;
        if discriminant.is_some_and(|d| !is_same(cx, d, c.discriminant)) {
            return None;
        }
        discriminant.get_or_insert(c.discriminant);
        values.extend(c.values);
    }
    Some(Comparison {
        discriminant: discriminant?,
        values,
    })
}

fn consequent(ast: &Ast, if_stmt: NodeId) -> Option<NodeId> {
    match *ast.node(if_stmt) {
        Node::If { cons, .. } => Some(cons),
        _ => None,
    }
}

/// The discriminant and the objects it is read through (`a.b.c`, `a.b`, `a`).
fn reference_prefixes(ast: &Ast, id: NodeId) -> Vec<NodeId> {
    let mut id = unwrap_ts_and_parens(ast, id);
    let mut out = vec![id];
    for _ in 0..100_000 {
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(id) else {
            break;
        };
        id = unwrap_ts_and_parens(ast, obj);
        out.push(id);
    }
    out
}

/// Something under `root` (outside nested functions) writes the discriminant or a prefix.
fn mutates(cx: &Ctx<'_, '_>, root: NodeId, discriminant: NodeId) -> bool {
    struct Find<'c, 'a, 'b> {
        cx: &'c Ctx<'a, 'b>,
        prefixes: Vec<NodeId>,
        found: bool,
    }
    impl Visitor for Find<'_, '_, '_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let node = ast.node(id);
            if is_function(node.kind()) {
                return Walk::SkipChildren;
            }
            let mut written = Vec::new();
            let mut target = |t: NodeId| assignment_targets(ast, t, &mut written, 0);
            match *node {
                Node::Assign { left, .. } => target(left),
                Node::Update { arg, .. }
                | Node::Unary {
                    op: UnaryOp::Delete,
                    arg,
                } => target(arg),
                Node::VarDecl {
                    kind: VarKind::Var,
                    decls,
                    ..
                } => {
                    for &d in ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                            target(name);
                        }
                    }
                }
                Node::ForIn { left, .. } | Node::ForOf { left, .. }
                    if !matches!(ast.node(left), Node::VarDecl { .. }) =>
                {
                    target(left);
                }
                _ => {}
            }
            if written
                .iter()
                .any(|&w| self.prefixes.iter().any(|&p| is_same(self.cx, w, p)))
            {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        cx,
        prefixes: reference_prefixes(cx.ast(), discriminant),
        found: false,
    };
    walk(cx.ast(), root, &mut f);
    f.found
}

/// An `if` without `else` at the end of `id`, which would take an `else` placed after it.
fn ends_with_dangling_if(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..100_000 {
        match *ast.node(id) {
            Node::If { alt: None, .. } => return true,
            Node::If { alt: Some(a), .. } => id = a,
            Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::While { body, .. }
            | Node::Labeled { body, .. }
            | Node::With { body, .. } => id = body,
            _ => return false,
        }
    }
    false
}

fn check_pair(cx: &mut Ctx<'_, '_>, previous: NodeId, current: NodeId) {
    let ast = cx.ast();
    let (Some(previous_chain), Some(chain)) = (if_chain(ast, previous), if_chain(ast, current))
    else {
        return;
    };
    if previous_chain
        .iter()
        .filter_map(|&s| consequent(ast, s))
        .any(|c| always_exits(cx, c))
    {
        return;
    }
    let (Some(before), Some(now)) = (
        chain_comparison(cx, &previous_chain),
        chain_comparison(cx, &chain),
    ) else {
        return;
    };
    if !is_same(cx, before.discriminant, now.discriminant)
        || before.values.iter().any(|v| now.values.contains(v))
        || previous_chain
            .iter()
            .filter_map(|&s| consequent(ast, s))
            .any(|c| mutates(cx, c, before.discriminant))
    {
        return;
    }
    let span = cx.span(current);
    const MESSAGE: &str = "Prefer `else if` over an adjacent `if` with a related condition.";
    let last = previous_chain
        .last()
        .and_then(|&s| consequent(ast, s))
        .unwrap_or(previous);
    if ends_with_dangling_if(ast, last) {
        cx.report(span, MESSAGE).id("prefer-else-if");
        return;
    }
    let fix = Fix::replace(Span::new(span.start, span.start), "else ");
    let autofix = matches!(
        ast.node(unwrap_ts_and_parens(ast, before.discriminant)),
        Node::Ident { .. }
    ) && previous_chain
        .iter()
        .filter_map(|&s| consequent(ast, s))
        .all(|c| !has_side_effect(ast, c, true, true))
        && chain.iter().all(|&s| match *ast.node(s) {
            Node::If { test, .. } => !has_side_effect(ast, test, true, true),
            _ => true,
        });
    let report = cx.report(span, MESSAGE).id("prefer-else-if");
    if autofix {
        report.fix(fix);
    } else {
        report.suggest("suggestion", "Insert `else` before this `if`.", fix);
    }
}

impl Rule for PreferElseIf {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Block,
            NodeKind::StaticBlock,
            NodeKind::SwitchCase,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let stmts = match *ast.node(node) {
            Node::Program { stmts } | Node::Block { stmts } => stmts,
            Node::StaticBlock { body } | Node::SwitchCase { body, .. } => body,
            _ => return,
        };
        for pair in ast.list(stmts).windows(2) {
            if matches!(ast.node(pair[0]), Node::If { .. })
                && matches!(ast.node(pair[1]), Node::If { .. })
            {
                check_pair(cx, pair[0], pair[1]);
            }
        }
    }
}
