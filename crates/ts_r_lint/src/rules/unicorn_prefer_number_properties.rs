//! `unicorn/prefer-number-properties`: reads of the globals `parseInt`, `parseFloat`,
//! `isNaN`, `isFinite` (and, with `checkNaN` / `checkInfinity`, `NaN` and `Infinity`) are
//! reported with `Number.<name>` as the replacement, directly or as a property of the global
//! object (`globalThis.parseInt`, through aliases of it).
//!
//! - Write targets (`parseInt = f`, `delete -Infinity`, destructuring targets) are skipped,
//!   and so is `parseInt(x)` / `parseInt(x, 10)`, where nothing changes.
//! - `-Infinity` is reported on the negation as `Number.NEGATIVE_INFINITY` (fixed only
//!   when no comment is inside it); `Infinity` alone is `Number.POSITIVE_INFINITY`.
//! - `parseInt`, `parseFloat`, `NaN` and `Infinity` are fixed. `isNaN` / `isFinite` coerce
//!   their argument, so they are fixed only for a single argument known to be a number
//!   (`unicorn_types::is_number`); otherwise the change is a suggestion.
//! - A shorthand property `{parseInt}` becomes `{parseInt: Number.parseInt}`.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_types::is_number;
use super::unicorn_util::{is_global_object, is_modified_global, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-number-properties",
        "Prefer `Number` static methods over global functions and optionally static properties over global constants.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferNumberProperties>,
};

#[derive(Debug)]
struct PreferNumberProperties {
    check_infinity: bool,
    check_nan: bool,
}

impl FromOptions for PreferNumberProperties {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferNumberProperties {
            check_infinity: bool_field(o, "checkInfinity", false),
            check_nan: bool_field(o, "checkNaN", false),
        })
    }
}

impl PreferNumberProperties {
    fn tracked(&self, name: &str) -> bool {
        match name {
            "parseInt" | "parseFloat" | "isNaN" | "isFinite" => true,
            "NaN" => self.check_nan,
            "Infinity" => self.check_infinity,
            _ => false,
        }
    }
}

impl Rule for PreferNumberProperties {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident, NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let name = match *cx.node(node) {
            Node::Ident { .. } => {
                let Some(name) = ident_name(ast, node) else {
                    return;
                };
                if !self.tracked(name) || !is_global_read(cx, node, name) {
                    return;
                }
                name
            }
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let Some(name) = member_name(ast, cx.src(), node) else {
                    return;
                };
                if let Node::Index { index, .. } = *ast.node(node)
                    && !matches!(
                        ast.node(skip_parens(ast, index)),
                        Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. }
                    )
                {
                    return;
                }
                let name: &str = &name;
                let Some(name) = [
                    "parseInt",
                    "parseFloat",
                    "isNaN",
                    "isFinite",
                    "NaN",
                    "Infinity",
                ]
                .into_iter()
                .find(|n| *n == name) else {
                    return;
                };
                if !self.tracked(name) || !is_global_object(cx, obj) {
                    return;
                }
                name
            }
            _ => return,
        };
        if is_left_hand_side(cx, node)
            || (name == "Infinity" && negation(cx, node).is_some_and(|n| is_left_hand_side(cx, n)))
            || (name == "parseInt" && is_base10_or_no_radix_call(cx, node))
        {
            return;
        }
        report(cx, node, name);
    }
}

/// A read of the unmodified global `name` (not a local binding).
fn is_global_read(cx: &Ctx<'_, '_>, ident: NodeId, name: &str) -> bool {
    let sem = cx.semantic();
    let Some(r) = sem.reference_at(ident) else {
        return false;
    };
    let r = sem.reference(r);
    if !r.is_read() {
        return false;
    }
    let global = match r.resolution {
        Resolution::Global => true,
        Resolution::Symbol(s) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
        Resolution::Unresolved => false,
    };
    global && !is_modified_global(cx, name)
}

/// The parent, skipping parentheses.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

/// `-node`: the negation.
fn negation(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let (p, _) = parent_np(cx, node)?;
    matches!(
        cx.node(p),
        Node::Unary {
            op: UnaryOp::Minus,
            ..
        }
    )
    .then_some(p)
}

/// `isLeftHandSide`: the node (through parentheses and TS wrappers) is written, deleted or
/// a destructuring target.
fn is_left_hand_side(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    let parent = loop {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            _ => break p,
        }
    };
    match *ast.node(parent) {
        Node::Assign { left, .. } | Node::AssignPattern { left, .. } => left == cur,
        Node::Update { .. } => true,
        Node::Unary {
            op: UnaryOp::Delete,
            ..
        } => true,
        Node::ForIn { left, .. } | Node::ForOf { left, .. } => left == cur,
        Node::ArrayPattern { .. } | Node::Rest { .. } => true,
        Node::ArrayLit { .. } | Node::Spread { .. } | Node::ShorthandProp { .. } => {
            is_assignment_target(ast, cx.file.parents, parent)
        }
        Node::Property { value, .. } => {
            value == cur
                && cx
                    .parent(parent)
                    .is_some_and(|o| is_assignment_target(ast, cx.file.parents, o))
        }
        _ => false,
    }
}

/// `parseInt(x)` / `parseInt(x, 10)`: the callee of a call with no radix or radix 10.
fn is_base10_or_no_radix_call(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some((call, callee)) = parent_np(cx, node) else {
        return false;
    };
    let Node::Call {
        callee: c, args, ..
    } = *ast.node(call)
    else {
        return false;
    };
    if c != callee {
        return false;
    }
    let Some(&radix) = ast.list(args).get(1) else {
        return true;
    };
    if matches!(ast.node(radix), Node::Spread { .. }) {
        return false;
    }
    matches!(static_value_no_side_effects(cx, radix), Some(Static::Num(n)) if n == 10.0)
}

/// `isNaN(x)` / `isFinite(x)` with one argument that is a number.
fn is_call_with_number_argument(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some((call, callee)) = parent_np(cx, node) else {
        return false;
    };
    let Node::Call {
        callee: c, args, ..
    } = *ast.node(call)
    else {
        return false;
    };
    let [arg] = ast.list(args) else {
        return false;
    };
    c == callee && !matches!(ast.node(*arg), Node::Spread { .. }) && is_number(cx, *arg)
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, name: &str) {
    let ast = cx.ast();
    if name == "Infinity"
        && let Some(neg) = negation(cx, node)
    {
        let span = cx.span(neg);
        let fix = ast.comments_in(span).is_empty().then(|| {
            with_edits(
                Fix::replace(span, "Number.NEGATIVE_INFINITY"),
                space_around_keyword(cx, neg),
            )
        });
        cx.report_node(neg, "Prefer `Number.NEGATIVE_INFINITY` over `-Infinity`.")
            .id("error")
            .fix_opt(fix);
        return;
    }
    let property = if name == "Infinity" {
        "POSITIVE_INFINITY"
    } else {
        name
    };
    let replacement = format!("Number.{property}");
    let fix = match cx.parent(node).map(|p| cx.node(p)) {
        Some(Node::ShorthandProp { .. }) => {
            Fix::replace(cx.span(node), format!("{}: {replacement}", cx.text(node)))
        }
        Some(Node::ExportSpecifier { exported: None, .. }) => {
            Fix::replace(cx.span(node), format!("{replacement} as {}", cx.text(node)))
        }
        _ => Fix::replace(cx.span(node), replacement),
    };
    let safe = matches!(name, "parseInt" | "parseFloat" | "NaN" | "Infinity")
        || is_call_with_number_argument(cx, node);
    let msg = format!("Prefer `Number.{property}` over `{name}`.");
    let report = cx.report_node(node, msg).id("error");
    if safe {
        report.fix(fix);
    } else {
        report.suggest(
            "suggestion",
            format!("Replace `{name}` with `Number.{property}`."),
            fix,
        );
    }
}
