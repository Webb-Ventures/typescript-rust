//! `unicorn/no-mismatched-map-key`: a `map.has(k)` guard (or `!map.has(k)`; one non-spread
//! argument, a plain non-computed call, receiver not known to be a non-`Map`) followed by a
//! `map.get(j)` / `map.set(j, ..)` on the same receiver with a key that is clearly
//! different. The guard is the test of a conditional, `if` or `while` (the last operand of
//! an `&&` chain), or the left of a logical expression. Guarded are: both conditional
//! branches; the right of the logical; the single statement of a `while` body; the single
//! statement of each `if` branch (not an `else if`).
//!
//! Inside a guarded part the walk skips functions and classes, nested `if`/conditionals
//! whose test checks the same receiver again, and any subtree that rewrites the receiver
//! (assignment, update, `for (x of ..)` head, or a declaration with an initializer binding
//! the receiver's root variable). Keys differ when neither contains a call and either both
//! are comparable static values that aren't SameValueZero, or both are references
//! (identifier, member, `this`, `super`) that don't name the same variable path. Receivers
//! and references are the same when their text paths match and their root identifiers
//! resolve to the same binding. Each key is reported once, at the key.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_types::is_known_non_map;
use super::unicorn_util::is_same_reference;
use crate::semantic::{Resolution, SymbolId};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-mismatched-map-key",
        "Disallow checking a Map key before accessing a different key.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoMismatchedMapKey>,
};

#[derive(Debug, Default)]
struct NoMismatchedMapKey;

const MESSAGE: &str = "Do not check a Map key before accessing a different key.";

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

fn is_skipped(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Arrow { .. }
            | Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::MethodDef { .. }
    )
}

fn no_spread(ast: &Ast, args: &[NodeId]) -> bool {
    args.iter()
        .all(|&a| !matches!(ast.node(a), Node::Spread { .. }))
}

/// `map.has(k)` / `!map.has(k)`: the `has` call.
fn map_has_call(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let id = unwrap_ts(ast, id);
    let is_has = |c: NodeId| {
        method_call(ast, c).is_some_and(|m| {
            m.name == "has" && m.args.len() == 1 && m.plain() && no_spread(ast, m.args)
        })
    };
    if is_has(id) {
        return Some(id);
    }
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => {
            let arg = unwrap_ts(ast, arg);
            is_has(arg).then_some(arg)
        }
        _ => None,
    }
}

/// The variable `ident` names: its reference, its declaration, or a lookup by name.
pub(crate) fn binding(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    if let Some(r) = sem.reference_at(ident) {
        return match sem.reference(r).resolution {
            Resolution::Symbol(s) => Some(s),
            _ => None,
        };
    }
    if let Some(s) = sem.symbol_of_decl(ident) {
        return Some(s);
    }
    sem.lookup(sem.enclosing_scope(ident), ident_name(cx.ast(), ident)?)
}

pub(crate) fn same_binding(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    match (binding(cx, a), binding(cx, b)) {
        (None, None) => ident_name(cx.ast(), a) == ident_name(cx.ast(), b),
        (x, y) => x == y,
    }
}

fn root_identifier(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let mut id = unwrap_ts(ast, id);
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Ident { .. } => return Some(id),
            Node::Member { obj, .. } | Node::Index { obj, .. } => id = unwrap_ts(ast, obj),
            _ => return None,
        }
    }
    None
}

fn same_reference_binding(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    if !is_same_reference(cx, a, b) {
        return false;
    }
    let ast = cx.ast();
    match (root_identifier(ast, a), root_identifier(ast, b)) {
        (Some(x), Some(y)) => same_binding(cx, x, y),
        _ => true,
    }
}

fn is_reference(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_ts(ast, id)),
        Node::Ident { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::This {}
            | Node::Super {}
    )
}

fn has_call(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if is_skipped(ast, n) {
            continue;
        }
        if matches!(ast.node(n), Node::Call { .. }) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn comparable_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    let v = static_value_no_side_effects(cx, unwrap_ts(cx.ast(), id))?;
    matches!(
        v,
        Static::Undefined
            | Static::Null
            | Static::Bool(_)
            | Static::Num(_)
            | Static::Str(_)
            | Static::BigInt(_)
    )
    .then_some(v)
}

fn same_value_zero(a: &Static, b: &Static) -> bool {
    match (a, b) {
        (Static::Num(x), Static::Num(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

fn different_keys(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    if has_call(ast, a) || has_call(ast, b) {
        return false;
    }
    if let (Some(x), Some(y)) = (comparable_value(cx, a), comparable_value(cx, b)) {
        return !same_value_zero(&x, &y);
    }
    is_reference(ast, a) && is_reference(ast, b) && !same_reference_binding(cx, a, b)
}

/// The `if`/`while` guard: the last operand of an `&&` chain.
fn guard_test(ast: &Ast, mut test: NodeId) -> NodeId {
    for _ in 0..100_000 {
        match *ast.node(test) {
            Node::Binary {
                op: BinaryOp::LogicalAnd,
                right,
                ..
            } => test = right,
            _ => break,
        }
    }
    test
}

fn single_statement(ast: &Ast, stmt: NodeId) -> Option<NodeId> {
    match *ast.node(stmt) {
        Node::Block { stmts } => match *ast.list(stmts) {
            [s] => Some(s),
            _ => None,
        },
        _ => Some(stmt),
    }
}

struct Guard<'a, 'b, 'c> {
    cx: &'a Ctx<'b, 'c>,
    receiver: NodeId,
    key: NodeId,
}

impl Guard<'_, '_, '_> {
    fn same_receiver(&self, other: NodeId) -> bool {
        same_reference_binding(self.cx, self.receiver, other)
    }

    /// The pattern (or destructuring target) binds the receiver's root variable.
    fn pattern_binds(&self, root: NodeId, id: NodeId, depth: u32) -> bool {
        if depth > 500 {
            return false;
        }
        let ast = self.cx.ast();
        let recur = |n: NodeId| self.pattern_binds(root, n, depth + 1);
        match *ast.node(id) {
            Node::Ident { .. } => same_binding(self.cx, id, root),
            Node::Paren { expr } => recur(expr),
            Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
                ast.list(elements).iter().any(|&e| recur(e))
            }
            Node::ObjectPattern { props } | Node::ObjectLit { props } => {
                ast.list(props).iter().any(|&p| match *ast.node(p) {
                    Node::Property { value, .. } => recur(value),
                    Node::ShorthandProp { name, .. } => recur(name),
                    _ => recur(p),
                })
            }
            Node::AssignPattern { left, .. } => recur(left),
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } => recur(left),
            Node::Spread { arg } => recur(arg),
            Node::Rest { arg } => recur(arg),
            Node::Param { pattern, .. } => recur(pattern),
            _ => false,
        }
    }

    fn root_rewrite(&self, target: NodeId) -> bool {
        root_identifier(self.cx.ast(), self.receiver)
            .is_some_and(|root| self.pattern_binds(root, target, 0))
    }

    fn is_write(&self, id: NodeId) -> bool {
        let ast = self.cx.ast();
        match *ast.node(id) {
            Node::VarDecl { decls, .. } => ast.list(decls).iter().any(|&d| {
                matches!(*ast.node(d), Node::VarDeclarator { name, init: Some(_), .. } if self.root_rewrite(name))
            }),
            Node::Assign { left, .. } => {
                let left = unwrap_ts(ast, left);
                self.same_receiver(left) || self.root_rewrite(left)
            }
            Node::Update { arg, .. } => {
                let arg = unwrap_ts(ast, arg);
                self.same_receiver(arg) || self.root_rewrite(arg)
            }
            Node::ForIn { left, .. } | Node::ForOf { left, .. }
                if !matches!(ast.node(left), Node::VarDecl { .. }) =>
            {
                let left = unwrap_ts(ast, left);
                self.same_receiver(left) || self.root_rewrite(left)
            }
            _ => false,
        }
    }

    fn checks_same_receiver(&self, root: NodeId) -> bool {
        let ast = self.cx.ast();
        let mut stack = vec![root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 1_000_000 || is_skipped(ast, n) {
                continue;
            }
            if let Some(has) = map_has_call(ast, n)
                && method_call(ast, has).is_some_and(|m| self.same_receiver(m.obj))
            {
                return true;
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    }

    fn is_nested_guard(&self, id: NodeId) -> bool {
        match *self.cx.ast().node(id) {
            Node::If { test, .. } | Node::Conditional { test, .. } => {
                self.checks_same_receiver(test)
            }
            _ => false,
        }
    }

    fn has_write(&self, root: NodeId) -> bool {
        let ast = self.cx.ast();
        let mut stack = vec![root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 1_000_000 {
                return true;
            }
            if is_skipped(ast, n) || self.is_nested_guard(n) {
                continue;
            }
            if self.is_write(n) {
                return true;
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    }

    fn access_key(&self, id: NodeId) -> Option<NodeId> {
        let ast = self.cx.ast();
        let m = method_call(ast, id)?;
        if !matches!(m.name, "get" | "set")
            || m.args.is_empty()
            || !m.plain()
            || !no_spread(ast, m.args)
            || !self.same_receiver(m.obj)
        {
            return None;
        }
        let key = m.args[0];
        different_keys(self.cx, self.key, key).then_some(key)
    }

    fn collect(&self, root: NodeId, out: &mut Vec<NodeId>, depth: u32) {
        let ast = self.cx.ast();
        if depth > 2_000
            || is_skipped(ast, root)
            || self.is_nested_guard(root)
            || self.has_write(root)
        {
            return;
        }
        if let Some(k) = self.access_key(root) {
            out.push(k);
        }
        let mut children = Vec::new();
        ast.for_each_child(root, |c| children.push(c));
        for c in children {
            self.collect(c, out, depth + 1);
        }
    }
}

fn branches(ast: &Ast, id: NodeId) -> Vec<NodeId> {
    match *ast.node(id) {
        Node::Conditional { cons, alt, .. } => vec![cons, alt],
        Node::Binary { right, .. } => vec![right],
        Node::While { body, .. } => single_statement(ast, body).into_iter().collect(),
        Node::If { cons, alt, .. } => single_statement(ast, cons)
            .into_iter()
            .chain(
                alt.filter(|&a| !matches!(ast.node(a), Node::If { .. }))
                    .and_then(|a| single_statement(ast, a)),
            )
            .collect(),
        _ => Vec::new(),
    }
}

fn guard_call(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            left,
            ..
        } => map_has_call(ast, left),
        Node::Conditional { test, .. } | Node::If { test, .. } | Node::While { test, .. } => {
            map_has_call(ast, guard_test(ast, test))
        }
        _ => None,
    }
}

impl Rule for NoMismatchedMapKey {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut reported: Vec<NodeId> = Vec::new();
        let mut stack = vec![ast.root];
        let mut children = Vec::new();
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 10_000_000 {
                break;
            }
            if let Some(has) = guard_call(ast, n)
                && let Some(m) = method_call(ast, has)
                && !is_known_non_map(cx, m.obj)
            {
                let g = Guard {
                    cx,
                    receiver: m.obj,
                    key: m.args[0],
                };
                let mut keys = Vec::new();
                for b in branches(ast, n) {
                    g.collect(b, &mut keys, 0);
                }
                for k in keys {
                    if !reported.contains(&k) {
                        reported.push(k);
                    }
                }
            }
            children.clear();
            ast.for_each_child(n, |c| children.push(c));
            stack.extend(children.iter().rev());
        }
        for k in reported {
            let span = cx.span(skip_parens(ast, k));
            cx.report(span, MESSAGE).id("no-mismatched-map-key");
        }
    }
}
