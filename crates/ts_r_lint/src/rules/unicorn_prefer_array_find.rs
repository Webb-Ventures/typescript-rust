//! `unicorn/prefer-array-find`: `.find()` / `.findLast()` over the first or last element
//! of `.filter()`.
//!
//! A filter call is `x.filter(cb[, t])` (no spread among the two, no `?.(`), unless `x` is
//! known not to be an array or typed array. Reported at `filter`:
//! - `filter(..)[0]` (not written to), `.shift()`, `.at(0)`, and with `checkFromLast`
//!   `.pop()`, `.at(-1)`, `.slice(-1)[0]`, `.slice(-1).pop()` / `.shift()`;
//! - `const [a] = x.filter(..)` / `[a] = x.filter(..)` (one non-rest element; not `x?.`);
//! - `const items = x.filter(..)` (a plain, unannotated, non-exported declarator) whose
//!   every other use is `items[0]` or `[a] = items` / `const [a] = items`.
//!
//! The fix renames `filter` and removes what follows (`[0]`, `.shift()`, the `.slice(-1)`
//! call); destructuring keeps only the element (`[{a}] = ...` gains parentheses). In the
//! declaration case a singular name (`items` -> `item`, made free in the scope and its
//! children) replaces the variable everywhere, and the fix spans the whole file. No fix
//! when a comment would be lost, when an element has a default, when the pattern has a
//! type annotation, or when `[0]` is a callee, tag or `delete` operand. The fix is applied
//! only when the callback is an inline side-effect-free function; otherwise it is a
//! suggestion ("Use `.find(…)`.").

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_ast::method_call;
use super::unicorn_no_useless_undefined::is_reference_context;
use super::unicorn_text::has_comment_in;
use super::unicorn_text::is_parenthesized;
use super::unicorn_text::paren_span as parenthesized_span;
use super::unicorn_types::is_known_non_indexed_collection;
use super::unicorn_util::{available_name, rename_symbol_fix, scope_within, singular};
use super::unicorn_util::{is_side_effect_free_function, remove_method_call};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-find",
        "Prefer `.find(…)` and `.findLast(…)` over the first or last element from `.filter(…)`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferArrayFind>,
};

#[derive(Debug)]
struct PreferArrayFind {
    check_from_last: bool,
}

impl FromOptions for PreferArrayFind {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(PreferArrayFind {
            check_from_last: bool_field(obj_opt(opts, 0), "checkFromLast", true),
        })
    }
}

const SUGGESTION_ID: &str = "prefer-array-find/suggestion";

/// `x.filter(cb[, t])`: the filter call's property and callback.
fn filter_call(cx: &Ctx<'_, '_>, id: NodeId, allow_optional_member: bool) -> Option<NodeId> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let call = method_call(ast, id)?;
    if call.name != "filter"
        || call.optional_call
        || (!allow_optional_member && call.optional_member)
        || call.args.is_empty()
        || call.args.len() > 2
        || call
            .args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        || is_known_non_indexed_collection(cx, call.obj)
    {
        return None;
    }
    Some(id)
}

fn filter_prop(cx: &Ctx<'_, '_>, call: NodeId) -> (NodeId, NodeId) {
    let ast = cx.ast();
    let m = method_call(ast, call).expect("filter call");
    (m.prop, m.args[0])
}

/// A plain call `x.<name>()` / `x.<name>(arg)` (no optional chaining): its object.
fn plain_call(cx: &Ctx<'_, '_>, id: NodeId, names: &[&str], argc: usize) -> Option<NodeId> {
    let call = method_call(cx.ast(), id)?;
    (call.plain() && names.contains(&call.name) && call.args.len() == argc)
        .then_some(call.obj)
        .filter(|_| {
            !call
                .args
                .iter()
                .any(|&a| matches!(cx.node(a), Node::Spread { .. }))
        })
}

fn literal_raw_is(cx: &Ctx<'_, '_>, id: NodeId, raw: &str) -> bool {
    let id = skip_parens(cx.ast(), id);
    matches!(cx.node(id), Node::NumberLit { .. }) && cx.text(id) == raw
}

fn is_negative_one(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = skip_parens(cx.ast(), id);
    matches!(*cx.node(id), Node::Unary { op: UnaryOp::Minus, arg } if literal_raw_is(cx, arg, "1"))
}

/// `x.filter(..).slice(-1)`: the filter call.
fn filter_slice_last(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let id = skip_parens(cx.ast(), id);
    let call = method_call(cx.ast(), id)?;
    if !call.plain() || call.name != "slice" || call.args.len() != 1 {
        return None;
    }
    if !is_negative_one(cx, call.args[0]) {
        return None;
    }
    filter_call(cx, call.obj, true)
}

/// Removes `[0]` (or `.x`) from a member: from the end of its object's parentheses to its
/// end.
fn remove_member_property(cx: &Ctx<'_, '_>, member: NodeId) -> (Span, String) {
    let ast = cx.ast();
    let obj = match *cx.node(member) {
        Node::Index { obj, .. } | Node::Member { obj, .. } => obj,
        _ => return (Span::at(cx.span(member).end), String::new()),
    };
    let start = parenthesized_span(cx, skip_parens(ast, obj)).end;
    (Span::new(start, cx.span(member).end), String::new())
}

/// A comment between the end of the filter call (with parentheses) and the end of `node`.
fn comment_after(cx: &Ctx<'_, '_>, filter: NodeId, node: NodeId) -> bool {
    let start = parenthesized_span(cx, filter).end;
    let end = cx.span(node).end;
    start < end && has_comment_in(cx, Span::new(start, end))
}

fn fix_of(edits: Vec<(Span, String)>) -> Fix {
    let mut edits = edits;
    edits.sort_by_key(|(s, _)| (s.start, s.end));
    Fix {
        edits: edits
            .into_iter()
            .map(|(span, text)| crate::diagnostic::Edit { span, text })
            .collect(),
    }
}

fn emit(
    cx: &mut Ctx<'_, '_>,
    filter: NodeId,
    id: &'static str,
    message: &str,
    fix: Option<Fix>,
    method: &str,
) {
    let (prop, callback) = filter_prop(cx, filter);
    let span = cx.span(prop);
    let free = is_side_effect_free_function(cx, callback);
    let r = cx.report(span, message).id(id);
    if let Some(f) = fix {
        if free {
            r.fix(f);
        } else {
            r.suggest(SUGGESTION_ID, format!("Use `.{method}(…)`."), f);
        }
    }
}

fn trailing_call(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    filter: NodeId,
    id: &'static str,
    message: &str,
    method: &str,
) {
    let fix = (!comment_after(cx, filter, node))
        .then(|| {
            let (prop, _) = filter_prop(cx, filter);
            remove_method_call(cx, node).map(|f| f.and(cx.span(prop), method.to_owned()))
        })
        .flatten();
    emit(cx, filter, id, message, fix, method);
}

/// The array pattern (or array literal target) and the right side of a destructuring
/// declarator or assignment.
fn destructuring(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(NodeId, NodeId)> {
    match *cx.node(node) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } => Some((name, init)),
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } => Some((left, right)),
        _ => None,
    }
}

/// The single element of `[a]` (pattern or assignment target): not a hole, not a rest.
fn single_element(cx: &Ctx<'_, '_>, pattern: NodeId) -> Option<NodeId> {
    let elements = match *cx.node(pattern) {
        Node::ArrayPattern { elements } | Node::ArrayLit { elements } => elements,
        _ => return None,
    };
    let &[e] = cx.ast().list(elements) else {
        return None;
    };
    (!matches!(
        cx.node(e),
        Node::Hole {} | Node::Rest { .. } | Node::Spread { .. }
    ))
    .then_some(e)
}

fn has_default(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    destructuring(cx, node)
        .and_then(|(left, _)| single_element(cx, left))
        .is_some_and(|e| {
            matches!(
                cx.node(e),
                Node::AssignPattern { .. }
                    | Node::Assign {
                        op: AssignOp::Assign,
                        ..
                    }
            )
        })
}

/// The edits that keep only the element of a destructuring; `None` aborts the fix.
fn fix_destructuring(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Vec<(Span, String)>> {
    let (left, _) = destructuring(cx, node)?;
    let element = single_element(cx, left)?;
    if has_comment_in(cx, cx.span(left)) {
        return None;
    }
    if matches!(
        *cx.node(node),
        Node::VarDeclarator {
            type_ann: Some(_),
            ..
        }
    ) {
        return None;
    }
    let mut edits = vec![(cx.span(left), cx.text(element).to_owned())];
    if matches!(cx.node(node), Node::Assign { .. })
        && !is_parenthesized(cx, node)
        && matches!(
            cx.node(element),
            Node::ObjectLit { .. } | Node::ObjectPattern { .. }
        )
    {
        let s = cx.span(node);
        edits.push((Span::at(s.start), "(".into()));
        edits.push((Span::at(s.end), ")".into()));
    }
    Some(edits)
}

const MSG_DESTRUCTURING: &str = "Prefer `.find(…)` over destructuring `.filter(…)`.";

impl Rule for PreferArrayFind {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Index,
            NodeKind::Call,
            NodeKind::VarDeclarator,
            NodeKind::Assign,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Index {
                obj,
                optional,
                index,
            } => self.index(cx, node, obj, optional, index),
            Node::Call { .. } => self.call(cx, node),
            Node::VarDeclarator { .. } => {
                self.destructuring(cx, node);
                self.declaration(cx, node);
            }
            Node::Assign { .. } => self.destructuring(cx, node),
            _ => {}
        }
    }
}

impl PreferArrayFind {
    fn index(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        obj: NodeId,
        optional: bool,
        index: NodeId,
    ) {
        if !literal_raw_is(cx, index, "0") || is_left_hand_side(cx, node) {
            return;
        }
        if let Some(filter) = filter_call(cx, obj, true) {
            let fix =
                (!is_reference_context(cx, node) && !comment_after(cx, filter, node)).then(|| {
                    let (prop, _) = filter_prop(cx, filter);
                    fix_of(vec![
                        (cx.span(prop), "find".into()),
                        remove_member_property(cx, node),
                    ])
                });
            emit(
                cx,
                filter,
                "error-zero-index",
                "Prefer `.find(…)` over `.filter(…)[0]`.",
                fix,
                "find",
            );
            return;
        }
        if !self.check_from_last || optional {
            return;
        }
        let slice = skip_parens(cx.ast(), obj);
        let Some(filter) = filter_slice_last(cx, slice) else {
            return;
        };
        let fix = (!is_reference_context(cx, node) && !comment_after(cx, filter, node))
            .then(|| {
                let (prop, _) = filter_prop(cx, filter);
                let slice_fix = remove_method_call(cx, slice)?;
                let mut edits: Vec<(Span, String)> = slice_fix
                    .edits
                    .into_iter()
                    .map(|e| (e.span, e.text))
                    .collect();
                edits.push((cx.span(prop), "findLast".into()));
                edits.push(remove_member_property(cx, node));
                Some(fix_of(edits))
            })
            .flatten();
        emit(
            cx,
            filter,
            "error-slice-minus-one",
            "Prefer `.findLast(…)` over `.filter(…).slice(-1)`.",
            fix,
            "findLast",
        );
    }

    fn call(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        if let Some(obj) = plain_call(cx, node, &["shift"], 0)
            && let Some(filter) = filter_call(cx, obj, true)
        {
            trailing_call(
                cx,
                node,
                filter,
                "error-shift",
                "Prefer `.find(…)` over `.filter(…).shift()`.",
                "find",
            );
        }
        if let Some(obj) = plain_call(cx, node, &["at"], 1)
            && let Some(m) = method_call(ast, node)
            && literal_raw_is(cx, m.args[0], "0")
            && let Some(filter) = filter_call(cx, obj, true)
        {
            trailing_call(
                cx,
                node,
                filter,
                "error-at-zero",
                "Prefer `.find(…)` over `.filter(…).at(0)`.",
                "find",
            );
        }
        if !self.check_from_last {
            return;
        }
        if let Some(obj) = plain_call(cx, node, &["pop"], 0)
            && let Some(filter) = filter_call(cx, obj, true)
        {
            trailing_call(
                cx,
                node,
                filter,
                "error-pop",
                "Prefer `.findLast(…)` over `.filter(…).pop()`.",
                "findLast",
            );
        }
        if let Some(obj) = plain_call(cx, node, &["at"], 1)
            && let Some(m) = method_call(ast, node)
            && is_negative_one(cx, m.args[0])
            && let Some(filter) = filter_call(cx, obj, true)
        {
            trailing_call(
                cx,
                node,
                filter,
                "error-at-minus-one",
                "Prefer `.findLast(…)` over `.filter(…).at(-1)`.",
                "findLast",
            );
        }
        if let Some(obj) = plain_call(cx, node, &["pop", "shift"], 0) {
            let slice = skip_parens(ast, obj);
            if let Some(filter) = filter_slice_last(cx, slice) {
                let fix = (!comment_after(cx, filter, node))
                    .then(|| {
                        let (prop, _) = filter_prop(cx, filter);
                        let mut edits: Vec<(Span, String)> = Vec::new();
                        for f in [
                            remove_method_call(cx, slice)?,
                            remove_method_call(cx, node)?,
                        ] {
                            edits.extend(f.edits.into_iter().map(|e| (e.span, e.text)));
                        }
                        edits.push((cx.span(prop), "findLast".into()));
                        Some(fix_of(edits))
                    })
                    .flatten();
                emit(
                    cx,
                    filter,
                    "error-slice-minus-one",
                    "Prefer `.findLast(…)` over `.filter(…).slice(-1)`.",
                    fix,
                    "findLast",
                );
            }
        }
    }

    fn destructuring(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let Some((left, right)) = destructuring(cx, node) else {
            return;
        };
        let is_decl = matches!(cx.node(node), Node::VarDeclarator { .. });
        if (is_decl && !matches!(cx.node(left), Node::ArrayPattern { .. }))
            || (!is_decl && !matches!(cx.node(left), Node::ArrayLit { .. }))
            || single_element(cx, left).is_none()
        {
            return;
        }
        let Some(filter) = filter_call(cx, right, false) else {
            return;
        };
        let fix = (!has_default(cx, node))
            .then(|| {
                let (prop, _) = filter_prop(cx, filter);
                let mut edits = fix_destructuring(cx, node)?;
                edits.push((cx.span(prop), "find".into()));
                Some(fix_of(edits))
            })
            .flatten();
        let id = if is_decl {
            "error-destructuring-declaration"
        } else {
            "error-destructuring-assignment"
        };
        emit(cx, filter, id, MSG_DESTRUCTURING, fix, "find");
    }

    fn declaration(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(filter) = filter_call(cx, init, false) else {
            return;
        };
        let Some(decl) = cx.parent(node) else {
            return;
        };
        let Node::VarDecl { modifiers, .. } = *cx.node(decl) else {
            return;
        };
        if modifiers.contains(Modifiers::EXPORT) {
            return;
        }
        if cx
            .parent(decl)
            .is_some_and(|p| matches!(cx.node(p), Node::ExportNamed { .. }))
        {
            return;
        }
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        let symbol = sem.symbol(sym);
        let mut idents: Vec<NodeId> = symbol.decls.iter().map(|d| d.name).collect();
        for &r in &symbol.refs {
            let n = sem.reference(r).node;
            if !idents.contains(&n) {
                idents.push(n);
            }
        }
        idents.retain(|&i| i != name);
        if idents.is_empty() {
            return;
        }
        let mut zero_index = Vec::new();
        let mut destructurings = Vec::new();
        for &ident in &idents {
            let parent = parent_skip_parens(ast, cx.file.parents, ident);
            let Some(parent) = parent else {
                return;
            };
            match *cx.node(parent) {
                Node::Index { obj, index, .. }
                    if skip_parens(ast, obj) == ident
                        && literal_raw_is(cx, index, "0")
                        && !is_left_hand_side(cx, parent)
                        && !is_reference_context(cx, parent) =>
                {
                    zero_index.push(parent);
                }
                _ => match destructuring(cx, parent) {
                    Some((left, right))
                        if skip_parens(ast, right) == ident
                            && matches!(
                                cx.node(left),
                                Node::ArrayPattern { .. } | Node::ArrayLit { .. }
                            )
                            && single_element(cx, left).is_some() =>
                    {
                        destructurings.push(parent);
                    }
                    _ => return,
                },
            }
        }
        let fixable = destructurings.iter().all(|&d| !has_default(cx, d))
            && zero_index
                .iter()
                .chain(&destructurings)
                .all(|&n| !has_comment_in(cx, cx.span(n)));
        let fix = fixable
            .then(|| {
                let (prop, _) = filter_prop(cx, filter);
                let mut edits = vec![(cx.span(prop), "find".to_owned())];
                let mut whole_file = false;
                if let Some(single) = singular(cx.text(name)) {
                    let scope = sem.enclosing_scope(node);
                    let scopes: Vec<ScopeId> = (0..sem.scopes().len() as u32)
                        .map(ScopeId)
                        .filter(|&s| scope_within(sem, s, scope))
                        .collect();
                    if let Some(new) = available_name(cx, &single, &scopes, &|_| true) {
                        edits.extend(
                            rename_symbol_fix(cx, sym, &new)
                                .edits
                                .into_iter()
                                .map(|e| (e.span, e.text)),
                        );
                        whole_file = true;
                    }
                }
                for &n in &zero_index {
                    edits.push(remove_member_property(cx, n));
                }
                for &d in &destructurings {
                    edits.extend(fix_destructuring(cx, d)?);
                }
                if whole_file {
                    edits.push((Span::at(0), String::new()));
                    edits.push((Span::at(cx.src().len() as u32), String::new()));
                }
                Some(fix_of(edits))
            })
            .flatten();
        emit(
            cx,
            filter,
            "error-variable",
            "Prefer `.find(…)` over `.filter(…)`.",
            fix,
            "find",
        );
    }
}
