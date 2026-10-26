//! `unicorn/no-loop-iterable-mutation`: a mutating method call on the collection a
//! (non-`await`) `for…of` loop is consuming. The live iterable is a reference (`items`,
//! `this.items`, `a[i]`, through parentheses and TS wrappers) or an argument-less
//! `.keys()` / `.values()` / `.entries()` call on one; nothing with optional chaining. Its
//! kind (array, set, map, or unknown) picks the mutating methods: `copyWithin fill pop push
//! reverse shift sort splice unshift` for arrays, `add clear delete` for sets, `clear delete
//! set` for maps, all of them when unknown. A call counts when its receiver is the same
//! reference with every identifier bound to the same variable. Nested functions and
//! classes are not searched. Reported at the method name, once per call.
//!
//! With a `const` loop binding some updates of the current entry are allowed: deleting the
//! current set value or map key (`for (const [key] of map) map.delete(key)`), and adding the
//! current value / setting the current key, unless a delete of the current entry may have
//! run earlier in the same iteration (an earlier statement in an enclosing statement list,
//! looking into blocks, `if` branches that don't always leave, `try` parts and labels that
//! don't end in a jump, and `switch` cases not followed by a loop exit). An inner loop over
//! the same collection allows its own current add/set.

use super::prelude::*;
use super::unicorn_iter::unwrap_expression;
use super::unicorn_no_mismatched_map_key::same_binding;
use super::unicorn_types::{is_array, is_map, is_set};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::static_property_name;
use rustc_hash::FxHashSet;
use ts_r_ast::{Ast, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-loop-iterable-mutation",
        "Disallow mutating a loop iterable during iteration.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoLoopIterableMutation>,
};

#[derive(Debug, Default)]
struct NoLoopIterableMutation;

const MAX_DEPTH: u32 = 500;

const ARRAY_METHODS: &[&str] = &[
    "copyWithin",
    "fill",
    "pop",
    "push",
    "reverse",
    "shift",
    "sort",
    "splice",
    "unshift",
];
const SET_METHODS: &[&str] = &["add", "clear", "delete"];
const MAP_METHODS: &[&str] = &["clear", "delete", "set"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Array,
    Set,
    Map,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Direct,
    Keys,
    Values,
    Entries,
}

fn is_mutation_method(name: &str, kind: Kind) -> bool {
    match kind {
        Kind::Array => ARRAY_METHODS.contains(&name),
        Kind::Set => SET_METHODS.contains(&name),
        Kind::Map => MAP_METHODS.contains(&name),
        Kind::Unknown => {
            ARRAY_METHODS.contains(&name)
                || SET_METHODS.contains(&name)
                || MAP_METHODS.contains(&name)
        }
    }
}

fn collection_kind(cx: &Ctx<'_, '_>, id: NodeId) -> Kind {
    if is_array(cx, id) {
        Kind::Array
    } else if is_set(cx, id) {
        Kind::Set
    } else if is_map(cx, id) {
        Kind::Map
    } else {
        Kind::Unknown
    }
}

fn is_reference(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_expression(ast, id)),
        Node::Ident { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::This {}
            | Node::Super {}
    )
}

fn contains_optional_chain(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            return true;
        }
        match *ast.node(n) {
            Node::Call { optional: true, .. }
            | Node::Member { optional: true, .. }
            | Node::Index { optional: true, .. } => return true,
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

#[derive(Debug, Clone, Copy)]
struct Iterable {
    kind: Kind,
    reference: NodeId,
    via: Via,
}

fn live_iterable(cx: &Ctx<'_, '_>, right: NodeId) -> Option<Iterable> {
    let ast = cx.ast();
    let node = unwrap_expression(ast, right);
    if contains_optional_chain(ast, node) {
        return None;
    }
    if is_reference(ast, node) {
        return Some(Iterable {
            kind: collection_kind(cx, node),
            reference: node,
            via: Via::Direct,
        });
    }
    let Node::Call { callee, args, .. } = *ast.node(node) else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    if !ast.list(args).is_empty() {
        return None;
    }
    let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
        return None;
    };
    let via = match static_property_name(cx, callee).as_deref() {
        Some("keys") => Via::Keys,
        Some("values") => Via::Values,
        Some("entries") => Via::Entries,
        _ => return None,
    };
    if !is_reference(ast, obj) {
        return None;
    }
    let reference = skip_parens(ast, obj);
    Some(Iterable {
        kind: collection_kind(cx, reference),
        reference,
        via,
    })
}

/// What a loop over the iterable allows.
#[derive(Debug, Clone, Copy)]
struct LoopInfo {
    kind: Kind,
    iterable: NodeId,
    body: NodeId,
    add: Option<NodeId>,
    set_key: Option<NodeId>,
    delete: Option<NodeId>,
}

fn identifier_of(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let id = unwrap_expression(ast, id);
    matches!(ast.node(id), Node::Ident { .. }).then_some(id)
}

fn first_element_identifier(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let Node::ArrayPattern { elements } = *ast.node(unwrap_expression(ast, id)) else {
        return None;
    };
    identifier_of(ast, *ast.list(elements).first()?)
}

fn loop_info(cx: &Ctx<'_, '_>, left: NodeId, body: NodeId, iterable: Iterable) -> LoopInfo {
    let ast = cx.ast();
    let mut info = LoopInfo {
        kind: iterable.kind,
        iterable: iterable.reference,
        body,
        add: None,
        set_key: None,
        delete: None,
    };
    let Node::VarDecl {
        kind: VarKind::Const,
        decls,
        ..
    } = *ast.node(left)
    else {
        return info;
    };
    let Some(&decl) = ast.list(decls).first() else {
        return info;
    };
    let Node::VarDeclarator { name: binding, .. } = *ast.node(decl) else {
        return info;
    };
    let ident = identifier_of(ast, binding);
    let first = first_element_identifier(ast, binding);
    info.add = match iterable.via {
        Via::Entries => first,
        Via::Direct if iterable.kind == Kind::Map => None,
        _ => ident,
    };
    let current_key = match iterable.via {
        Via::Keys => ident,
        Via::Entries => first,
        Via::Direct if iterable.kind != Kind::Set => first,
        _ => None,
    };
    info.set_key = current_key;
    info.delete = if iterable.kind == Kind::Set && matches!(iterable.via, Via::Direct | Via::Values)
    {
        ident
    } else if matches!(iterable.via, Via::Direct | Via::Entries) {
        ident.or(first)
    } else {
        current_key
    };
    info
}

fn reference_identifiers(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > MAX_DEPTH {
        return;
    }
    let id = unwrap_expression(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => out.push(id),
        Node::Member { obj, .. } => reference_identifiers(ast, obj, out, depth + 1),
        Node::Index { obj, index, .. } => {
            reference_identifiers(ast, obj, out, depth + 1);
            reference_identifiers(ast, index, out, depth + 1);
        }
        _ => {}
    }
}

/// The same reference, with each identifier on both sides bound to the same variable.
fn same_reference_binding(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    if !is_same_reference(cx, a, b) {
        return false;
    }
    let ast = cx.ast();
    let (mut left, mut right) = (Vec::new(), Vec::new());
    reference_identifiers(ast, a, &mut left, 0);
    reference_identifiers(ast, b, &mut right, 0);
    if left.len() != right.len() || left.is_empty() {
        return left.len() == right.len();
    }
    left.iter()
        .zip(&right)
        .all(|(&x, &y)| same_binding(cx, x, y))
}

fn same_first_argument(cx: &Ctx<'_, '_>, call: NodeId, ident: Option<NodeId>) -> bool {
    let Node::Call { args, .. } = *cx.node(call) else {
        return false;
    };
    match (ident, cx.ast().list(args).first()) {
        (Some(ident), Some(&arg)) => same_reference_binding(cx, arg, ident),
        _ => false,
    }
}

/// The member callee of a call (through parentheses) and its static method name.
fn callee_method(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(NodeId, NodeId, String)> {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *ast.node(call) else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    let (obj, prop) = match *ast.node(callee) {
        Node::Member { obj, prop, .. } => (obj, prop),
        Node::Index { obj, index, .. } => (obj, index),
        _ => return None,
    };
    Some((obj, prop, static_property_name(cx, callee)?))
}

fn is_current_add_or_set(cx: &Ctx<'_, '_>, call: NodeId, info: &LoopInfo, method: &str) -> bool {
    (method == "add" && same_first_argument(cx, call, info.add))
        || (method == "set" && same_first_argument(cx, call, info.set_key))
}

// ---- Whether a branch always leaves -------------------------------------------------------

/// Whether a `break` inside `root` (not in a nested function, and for an unlabeled one not
/// in a nested loop or switch) leaves the statement `target` labeled `label`.
fn has_break_to(cx: &Ctx<'_, '_>, root: NodeId, label: Option<&str>, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return true;
    }
    match *ast.node(root) {
        Node::Break { label: None } => label.is_none(),
        Node::Break { label: Some(l) } => label.is_some_and(|name| cx.text(l) == name),
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. } => false,
        Node::For { .. }
        | Node::ForIn { .. }
        | Node::ForOf { .. }
        | Node::While { .. }
        | Node::DoWhile { .. }
        | Node::Switch { .. }
            if label.is_none() =>
        {
            false
        }
        _ => {
            let mut found = false;
            ast.for_each_child(root, |c| {
                if !found && ast.node(c).is_statement() {
                    found = has_break_to(cx, c, label, depth + 1);
                }
            });
            found
        }
    }
}

/// A structural "control never reaches the code after this statement".
fn always_exits(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return false;
    }
    match *ast.node(id) {
        Node::Return { .. } | Node::Throw { .. } | Node::Break { .. } | Node::Continue { .. } => {
            true
        }
        Node::Block { stmts } => ast
            .list(stmts)
            .iter()
            .any(|&s| always_exits(cx, s, depth + 1)),
        Node::If {
            cons,
            alt: Some(alt),
            ..
        } => always_exits(cx, cons, depth + 1) && always_exits(cx, alt, depth + 1),
        Node::Labeled { label, body } => {
            always_exits(cx, body, depth + 1) && !has_break_to(cx, body, Some(cx.text(label)), 0)
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            if finalizer.is_some_and(|f| always_exits(cx, f, depth + 1)) {
                return true;
            }
            let handler_exits = match handler {
                Some(h) => match *ast.node(h) {
                    Node::CatchClause { body, .. } => always_exits(cx, body, depth + 1),
                    _ => false,
                },
                None => true,
            };
            always_exits(cx, block, depth + 1) && handler_exits
        }
        Node::While { test, body } => is_truthy_literal(ast, test) && !has_loop_break(cx, body),
        Node::DoWhile { body, test } => {
            (always_exits(cx, body, depth + 1) && !has_break_to(cx, body, None, 0))
                || (is_truthy_literal(ast, test) && !has_loop_break(cx, body))
        }
        Node::For { test, body, .. } => {
            test.is_none_or(|t| is_truthy_literal(ast, t)) && !has_loop_break(cx, body)
        }
        Node::Switch { cases, .. } => {
            let cases = ast.list(cases);
            let has_default = cases
                .iter()
                .any(|&c| matches!(ast.node(c), Node::SwitchCase { test: None, .. }));
            let last_exits = cases.last().is_some_and(|&c| match *ast.node(c) {
                Node::SwitchCase { body, .. } => ast
                    .list(body)
                    .iter()
                    .any(|&s| always_exits(cx, s, depth + 1)),
                _ => false,
            });
            has_default && last_exits && !cases.iter().any(|&c| has_break_to(cx, c, None, 0))
        }
        _ => false,
    }
}

fn is_truthy_literal(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::BoolLit { value } => value,
        Node::NumberLit { value } => value.get() != 0.0 && !value.get().is_nan(),
        Node::StringLit { value } => !ast.atom(value).is_empty(),
        Node::ObjectLit { .. } | Node::ArrayLit { .. } => true,
        _ => false,
    }
}

/// An unlabeled `break` of this loop (labeled breaks are conservatively counted too).
fn has_loop_break(cx: &Ctx<'_, '_>, body: NodeId) -> bool {
    has_break_to(cx, body, None, 0) || has_any_labeled_break(cx, body, 0)
}

fn has_any_labeled_break(cx: &Ctx<'_, '_>, root: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return true;
    }
    match *ast.node(root) {
        Node::Break { label: Some(_) } => true,
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. } => false,
        _ => {
            let mut found = false;
            ast.for_each_child(root, |c| {
                if !found && ast.node(c).is_statement() {
                    found = has_any_labeled_break(cx, c, depth + 1);
                }
            });
            found
        }
    }
}

// ---- An earlier delete of the current entry -----------------------------------------------

fn is_jump(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Return { .. } | Node::Throw { .. } | Node::Break { .. } | Node::Continue { .. }
    )
}

fn ends_with_jump(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Block { stmts } => ast.list(stmts).last().is_some_and(|&s| is_jump(ast, s)),
        _ => is_jump(ast, id),
    }
}

/// A `case` body has a statement matching `pred`, also inside a plain block.
fn case_has(ast: &Ast, stmts: &[NodeId], pred: &dyn Fn(NodeId) -> bool, depth: u32) -> bool {
    depth < MAX_DEPTH
        && stmts.iter().any(|&s| match *ast.node(s) {
            Node::Block { stmts } => case_has(ast, ast.list(stmts), pred, depth + 1),
            _ => pred(s),
        })
}

fn case_body(ast: &Ast, case: NodeId) -> &[NodeId] {
    match *ast.node(case) {
        Node::SwitchCase { body, .. } => ast.list(body),
        _ => &[],
    }
}

fn followed_by_loop_exit(ast: &Ast, cases: &[NodeId]) -> bool {
    for &case in cases {
        let body = case_body(ast, case);
        let leaves = |s: NodeId| {
            matches!(
                ast.node(s),
                Node::Continue { .. } | Node::Return { .. } | Node::Throw { .. }
            )
        };
        if case_has(ast, body, &leaves, 0) {
            return true;
        }
        let breaks = |s: NodeId| matches!(ast.node(s), Node::Break { label: None });
        if case_has(ast, body, &breaks, 0) {
            return false;
        }
    }
    false
}

impl LoopInfo {
    fn is_current_delete(&self, cx: &Ctx<'_, '_>, call: NodeId) -> bool {
        callee_method(cx, call).is_some_and(|(obj, _, name)| {
            name == "delete"
                && same_reference_binding(cx, obj, self.iterable)
                && same_first_argument(cx, call, self.delete)
        })
    }

    fn direct_current_delete(&self, cx: &Ctx<'_, '_>, stmt: NodeId, depth: u32) -> bool {
        let ast = cx.ast();
        if depth > MAX_DEPTH {
            return false;
        }
        match *ast.node(stmt) {
            Node::ExprStmt { expr }
                if matches!(ast.node(unwrap_expression(ast, expr)), Node::Call { .. }) =>
            {
                self.is_current_delete(cx, unwrap_expression(ast, expr))
            }
            Node::Block { stmts } => ast
                .list(stmts)
                .iter()
                .any(|&s| self.direct_current_delete(cx, s, depth + 1)),
            Node::If { cons, alt, .. } => {
                (!always_exits(cx, cons, 0) && self.direct_current_delete(cx, cons, depth + 1))
                    || alt.is_some_and(|alt| {
                        !always_exits(cx, alt, 0) && self.direct_current_delete(cx, alt, depth + 1)
                    })
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                if finalizer.is_some_and(|f| ends_with_jump(ast, f)) {
                    return false;
                }
                let handler_body = handler.and_then(|h| match *ast.node(h) {
                    Node::CatchClause { body, .. } => Some(body),
                    _ => None,
                });
                [Some(block), handler_body, finalizer]
                    .into_iter()
                    .flatten()
                    .any(|c| {
                        !ends_with_jump(ast, c) && self.direct_current_delete(cx, c, depth + 1)
                    })
            }
            Node::Labeled { body, .. } => {
                !ends_with_jump(ast, body) && self.direct_current_delete(cx, body, depth + 1)
            }
            Node::Switch { cases, .. } => {
                let cases = ast.list(cases);
                cases.iter().enumerate().any(|(i, &case)| {
                    !followed_by_loop_exit(ast, &cases[i..])
                        && case_body(ast, case)
                            .iter()
                            .any(|&s| self.direct_current_delete(cx, s, depth + 1))
                })
            }
            _ => false,
        }
    }

    /// A delete of the current entry may run before `node` in the same iteration.
    fn earlier_current_delete(&self, cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let ast = cx.ast();
        let mut node = node;
        for _ in 0..10_000 {
            // The statement holding `node` in a block or `case` body.
            let mut stmt = node;
            let list_parent = loop {
                match cx.parent(stmt) {
                    Some(p)
                        if matches!(ast.node(p), Node::Block { .. } | Node::SwitchCase { .. }) =>
                    {
                        break Some(p);
                    }
                    Some(p) => stmt = p,
                    None => break None,
                }
            };
            let Some(parent) = list_parent else {
                return false;
            };
            let stmts = match *ast.node(parent) {
                Node::Block { stmts } => ast.list(stmts),
                _ => case_body(ast, parent),
            };
            let index = stmts.iter().position(|&s| s == stmt).unwrap_or(0);
            if stmts[..index]
                .iter()
                .any(|&s| self.direct_current_delete(cx, s, 0))
            {
                return true;
            }
            if parent == self.body {
                return false;
            }
            node = parent;
        }
        false
    }
}

fn mutation(
    cx: &Ctx<'_, '_>,
    call: NodeId,
    info: &LoopInfo,
    nested: Option<&LoopInfo>,
) -> Option<NodeId> {
    let (obj, prop, method) = callee_method(cx, call)?;
    if !is_mutation_method(&method, info.kind) || !same_reference_binding(cx, obj, info.iterable) {
        return None;
    }
    if method == "delete" && same_first_argument(cx, call, info.delete) {
        return None;
    }
    if is_current_add_or_set(cx, call, info, &method) && !info.earlier_current_delete(cx, call) {
        return None;
    }
    if nested.is_some_and(|n| is_current_add_or_set(cx, call, n, &method)) {
        return None;
    }
    Some(prop)
}

fn collect(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    info: &LoopInfo,
    nested: Option<&LoopInfo>,
    out: &mut Vec<(NodeId, NodeId)>,
    depth: u32,
) {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return;
    }
    let mut child_nested = nested.copied();
    match *ast.node(node) {
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. } => return,
        Node::ForOf {
            is_await: false,
            left,
            right,
            body,
        } => {
            if let Some(iterable) = live_iterable(cx, right)
                && same_reference_binding(cx, iterable.reference, info.iterable)
            {
                child_nested = Some(loop_info(cx, left, body, iterable));
            }
        }
        Node::Call { .. } => {
            if let Some(prop) = mutation(cx, node, info, nested) {
                out.push((node, prop));
            }
        }
        _ => {}
    }
    let mut children = Vec::new();
    ast.for_each_child(node, |c| children.push(c));
    for c in children {
        collect(cx, c, info, child_nested.as_ref(), out, depth + 1);
    }
}

impl Rule for NoLoopIterableMutation {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut loops = Vec::new();
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 10_000_000 {
                break;
            }
            if matches!(
                ast.node(n),
                Node::ForOf {
                    is_await: false,
                    ..
                }
            ) {
                loops.push(n);
            }
            let start = stack.len();
            ast.for_each_child(n, |c| stack.push(c));
            stack[start..].reverse();
        }
        // Inner loops first, as their exits come first.
        let mut reported: FxHashSet<NodeId> = FxHashSet::default();
        let mut problems: Vec<(NodeId, Span, String)> = Vec::new();
        for &l in loops.iter().rev() {
            let Node::ForOf {
                left, right, body, ..
            } = *ast.node(l)
            else {
                continue;
            };
            let Some(iterable) = live_iterable(cx, right) else {
                continue;
            };
            let info = loop_info(cx, left, body, iterable);
            let mut found = Vec::new();
            collect(cx, body, &info, None, &mut found, 0);
            for (call, prop) in found {
                if reported.insert(call) {
                    let span = cx.span(skip_parens(ast, prop));
                    problems.push((call, span, cx.text(info.iterable).to_string()));
                }
            }
        }
        problems.sort_by_key(|(_, span, _)| (span.start, span.end));
        for (_, span, iterable) in problems {
            cx.report(
                span,
                format!("Do not mutate `{iterable}` while iterating over it."),
            )
            .id("no-loop-iterable-mutation");
        }
    }
}
