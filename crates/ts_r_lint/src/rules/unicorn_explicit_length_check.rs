//! `unicorn/explicit-length-check`: a `.length` / `.size` read (non-computed, not optional,
//! not on `this`, not inside an optional chain) that is compared with zero in another
//! form, or used for its truthiness, is reported with the explicit comparison:
//! `=== 0` for a zero check, and `> 0` (or `!== 0` with `"non-zero": "not-equal"`) for a
//! non-zero check.
//!
//! - Comparisons: `=== 0`, `== 0`, `< 1`, `<= 0` (and the mirrored `0 === x.length`, ...) are
//!   zero checks; `!== 0`, `!= 0`, `> 0`, `>= 1` (and mirrors) are non-zero checks. `!` and
//!   `Boolean(...)` around the check flip / keep its meaning, and the outermost of them is
//!   what gets replaced.
//! - A bare `x.length` counts in a boolean context (`if`, `?:`, loops, `!`, `Boolean()`,
//!   `&&`/`||` chains of those), and as an `&&` operand (suggestion only), except when the
//!   same chain already checks the same length for non-zero, or checks `x.width`,
//!   `x.height` or `x.depth` (a shape object).
//! - Skipped: lengths known not to be collection sizes — a static value that isn't a
//!   non-negative integer, or a variable initialised with an object literal whose property
//!   is not one (absent, an accessor, or changed in ways the rule can't follow).
//! - The fix keeps the member text, adds parentheses or a `;` where needed, and is withheld
//!   when the replaced code holds a comment; `!x.length` as a binary left operand gets no
//!   fix or suggestion.

use super::prelude::*;
use super::unicorn_static::{
    Static, static_value, static_value_control_flow, static_value_no_side_effects,
};
use super::unicorn_text::{
    has_comment_in, is_parenthesized, is_written, needs_parens_at_expression_start, needs_semicolon,
};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::{space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/explicit-length-check",
        "Enforce explicitly comparing the `length` or `size` property of a value.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ExplicitLengthCheck>,
};

#[derive(Debug)]
struct ExplicitLengthCheck {
    not_equal: bool,
}

impl FromOptions for ExplicitLengthCheck {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let style = o
            .get("non-zero")
            .and_then(Value::as_str)
            .unwrap_or("greater-than");
        Ok(ExplicitLengthCheck {
            not_equal: style == "not-equal",
        })
    }
}

/// The ESTree parent: the first non-parenthesis ancestor, with the child on that path.
fn parent_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
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

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

fn is_logical(ast: &Ast, id: NodeId, ops: &[BinaryOp]) -> bool {
    matches!(*ast.node(id), Node::Binary { op, .. } if ops.contains(&op))
}

const AND_OR: &[BinaryOp] = &[BinaryOp::LogicalAnd, BinaryOp::LogicalOr];

/// `x.length` / `x.size`: non-computed, not optional.
fn is_length_or_size(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::Member { optional: false, prop, .. }
        if matches!(ident_name(ast, prop), Some("length" | "size")))
}

fn is_number(ast: &Ast, id: NodeId, value: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value: v } if v.get() == value)
}

fn compare_right(ast: &Ast, id: NodeId, op: BinaryOp, value: f64) -> bool {
    matches!(*ast.node(id), Node::Binary { op: o, right, .. } if o == op && is_number(ast, right, value))
}

fn compare_left(ast: &Ast, id: NodeId, op: BinaryOp, value: f64) -> bool {
    matches!(*ast.node(id), Node::Binary { op: o, left, .. } if o == op && is_number(ast, left, value))
}

/// The comparison with zero around `node`: `(is_zero_check, comparison)`.
fn length_check_node(cx: &Ctx<'_, '_>, node: NodeId, allow_ts: bool) -> Option<(bool, NodeId)> {
    let ast = cx.ast();
    let (mut parent, _) = parent_of(cx, node)?;
    if allow_ts {
        while is_ts_wrapper(ast, parent) {
            parent = parent_of(cx, parent)?.0;
        }
    }
    use BinaryOp::*;
    let p = parent;
    if compare_right(ast, p, StrictEq, 0.0)
        || compare_right(ast, p, Eq, 0.0)
        || compare_right(ast, p, Lt, 1.0)
        || compare_right(ast, p, LtEq, 0.0)
        || compare_left(ast, p, StrictEq, 0.0)
        || compare_left(ast, p, Eq, 0.0)
        || compare_left(ast, p, Gt, 1.0)
        || compare_left(ast, p, GtEq, 0.0)
    {
        return Some((true, p));
    }
    if compare_right(ast, p, StrictNotEq, 0.0)
        || compare_right(ast, p, NotEq, 0.0)
        || compare_right(ast, p, Gt, 0.0)
        || compare_right(ast, p, GtEq, 1.0)
        || compare_left(ast, p, StrictNotEq, 0.0)
        || compare_left(ast, p, NotEq, 0.0)
        || compare_left(ast, p, Lt, 0.0)
        || compare_left(ast, p, LtEq, 1.0)
    {
        return Some((false, p));
    }
    None
}

/// `Boolean(x)` with the global `Boolean` and one non-spread argument.
pub(crate) fn is_global_boolean_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let &[arg] = ast.list(args) else {
        return false;
    };
    cx.is_global_ref(callee, "Boolean") && !matches!(ast.node(arg), Node::Spread { .. })
}

fn is_not(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Unary {
            op: UnaryOp::Not,
            ..
        }
    )
}

fn is_not_argument(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    parent_of(cx, id).is_some_and(|(p, _)| is_not(cx.ast(), p))
}

fn is_boolean_call_argument(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    parent_of(cx, id).is_some_and(|(p, _)| is_global_boolean_call(cx, p))
}

/// `getBooleanAncestor`: the outermost `!` / `Boolean()` around `node`, and whether an odd
/// number of `!` was crossed.
pub(crate) fn boolean_ancestor(cx: &Ctx<'_, '_>, mut node: NodeId) -> (NodeId, bool) {
    let mut negative = false;
    for _ in 0..10_000 {
        if is_not_argument(cx, node) {
            negative = !negative;
        } else if !is_boolean_call_argument(cx, node) {
            break;
        }
        node = match parent_of(cx, node) {
            Some((p, _)) => p,
            None => break,
        };
    }
    (node, negative)
}

fn is_boolean_expression(cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> bool {
    if depth > 1000 {
        return false;
    }
    if is_not(cx.ast(), node)
        || is_not_argument(cx, node)
        || is_global_boolean_call(cx, node)
        || is_boolean_call_argument(cx, node)
    {
        return true;
    }
    match parent_of(cx, node) {
        Some((p, _)) if is_logical(cx.ast(), p, AND_OR) => is_boolean_expression(cx, p, depth + 1),
        _ => false,
    }
}

pub(crate) fn is_control_flow_test(cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> bool {
    if depth > 1000 {
        return false;
    }
    let ast = cx.ast();
    let Some((p, child)) = parent_of(cx, node) else {
        return false;
    };
    let test = match *ast.node(p) {
        Node::If { test, .. }
        | Node::Conditional { test, .. }
        | Node::While { test, .. }
        | Node::DoWhile { test, .. } => Some(test),
        Node::For { test, .. } => test,
        _ => None,
    };
    if test == Some(child) {
        return true;
    }
    is_logical(ast, p, AND_OR) && is_control_flow_test(cx, p, depth + 1)
}

pub(crate) fn is_boolean_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    is_boolean_expression(cx, node, 0) || is_control_flow_test(cx, node, 0)
}

/// The outermost `op` chain containing `node` as an operand (or `node`).
fn logical_root(cx: &Ctx<'_, '_>, mut node: NodeId, op: BinaryOp) -> NodeId {
    while let Some((p, _)) = parent_of(cx, node) {
        if is_logical(cx.ast(), p, &[op]) {
            node = p;
        } else {
            break;
        }
    }
    node
}

fn logical_operands(ast: &Ast, node: NodeId, op: BinaryOp, out: &mut Vec<NodeId>, depth: u32) {
    let node = skip_parens(ast, node);
    match *ast.node(node) {
        Node::Binary { left, op: o, right } if o == op && depth < 1000 => {
            logical_operands(ast, left, op, out, depth + 1);
            logical_operands(ast, right, op, out, depth + 1);
        }
        _ => out.push(node),
    }
}

/// The `.length` member a comparison (through `!` and `Boolean()`) reads.
fn compared_length_member(ast: &Ast, node: NodeId, depth: u32) -> Option<NodeId> {
    if depth > 1000 {
        return None;
    }
    let node = skip_parens(ast, node);
    match *ast.node(node) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => compared_length_member(ast, arg, depth + 1),
        Node::Call { callee, args, .. } if ident_name(ast, callee) == Some("Boolean") => {
            match ast.list(args) {
                &[a] => compared_length_member(ast, a, depth + 1),
                _ => None,
            }
        }
        Node::Binary { left, right, .. } => {
            let l = unwrap_ts(ast, left);
            if is_length_or_size(ast, l) {
                return Some(l);
            }
            let r = unwrap_ts(ast, right);
            is_length_or_size(ast, r).then_some(r)
        }
        _ => None,
    }
}

fn is_same_length_non_zero_check(cx: &Ctx<'_, '_>, node: NodeId, length: NodeId) -> bool {
    let Some(member) = compared_length_member(cx.ast(), node, 0) else {
        return false;
    };
    if !is_same_reference(cx, member, length) {
        return false;
    }
    let Some((zero, check)) = length_check_node(cx, member, true) else {
        return false;
    };
    let (ancestor, negative) = boolean_ancestor(cx, check);
    ancestor == node && negative == zero
}

fn guarded_by_non_zero_check(cx: &Ctx<'_, '_>, length: NodeId) -> bool {
    let root = logical_root(cx, length, BinaryOp::LogicalAnd);
    if !is_logical(cx.ast(), root, &[BinaryOp::LogicalAnd]) {
        return false;
    }
    let mut ops = Vec::new();
    logical_operands(cx.ast(), root, BinaryOp::LogicalAnd, &mut ops, 0);
    ops.iter()
        .any(|&o| o != length && is_same_length_non_zero_check(cx, o, length))
}

pub(crate) fn has_shape_property_check(cx: &Ctx<'_, '_>, node: NodeId, length: NodeId) -> bool {
    let ast = cx.ast();
    let root = logical_root(cx, node, BinaryOp::LogicalAnd);
    if !is_logical(ast, root, &[BinaryOp::LogicalAnd]) {
        return false;
    }
    let Node::Member {
        obj: length_obj, ..
    } = *ast.node(length)
    else {
        return false;
    };
    let mut ops = Vec::new();
    logical_operands(ast, root, BinaryOp::LogicalAnd, &mut ops, 0);
    ops.iter().any(|&o| {
        o != node
            && matches!(*ast.node(o), Node::Member { obj, optional: false, prop }
                if matches!(ident_name(ast, prop), Some("depth" | "height" | "width"))
                    && is_same_reference(cx, obj, length_obj))
    })
}

impl Rule for ExplicitLengthCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Member { obj, prop, .. } = *cx.node(node) else {
            return;
        };
        if !is_length_or_size(ast, node)
            || matches!(ast.node(skip_parens(ast, obj)), Node::This {})
            || chain_has_optional(ast, node)
            || known_non_collection_length(cx, node)
        {
            return;
        }
        let mut auto_fix = true;
        let target;
        let zero;
        if let Some((z, check)) = length_check_node(cx, node, false) {
            let (ancestor, negative) = boolean_ancestor(cx, check);
            target = ancestor;
            zero = z != negative;
        } else {
            let (ancestor, negative) = boolean_ancestor(cx, node);
            if is_boolean_context(cx, ancestor) {
                zero = negative;
                target = ancestor;
            } else if parent_of(cx, node)
                .is_some_and(|(p, _)| is_logical(ast, p, &[BinaryOp::LogicalAnd]))
            {
                zero = negative;
                target = node;
                auto_fix = false;
            } else {
                return;
            }
        }
        if (target == node && guarded_by_non_zero_check(cx, node))
            || has_shape_property_check(cx, target, node)
        {
            return;
        }
        let unsafe_negation = is_not(ast, target)
            && parent_of(cx, target).is_some_and(|(p, child)| {
                matches!(*ast.node(p), Node::Binary { left, op, .. }
                    if left == child && !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce))
            });
        self.report(
            cx,
            target,
            zero,
            node,
            prop,
            auto_fix && !unsafe_negation,
            !unsafe_negation,
        );
    }
}

impl ExplicitLengthCheck {
    #[allow(clippy::too_many_arguments)]
    fn report(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        zero: bool,
        length: NodeId,
        prop: NodeId,
        auto_fix: bool,
        suggest: bool,
    ) {
        let ast = cx.ast();
        let (code, op, already) = if zero {
            (
                "=== 0",
                "===",
                compare_right(ast, node, BinaryOp::StrictEq, 0.0),
            )
        } else if self.not_equal {
            (
                "!== 0",
                "!==",
                compare_right(ast, node, BinaryOp::StrictNotEq, 0.0),
            )
        } else {
            ("> 0", ">", compare_right(ast, node, BinaryOp::Gt, 0.0))
        };
        if already {
            return;
        }
        let comparison = format!("{} {code}", cx.text(length));
        let parens = comparison_needs_parens(cx, node, op)
            || needs_parens_at_expression_start(cx, length, node);
        let mut fixed = if parens {
            format!("({comparison})")
        } else {
            comparison
        };
        if needs_semicolon(cx, cx.span(node).start, &fixed) {
            fixed.insert(0, ';');
        }
        let property = cx.text(prop).to_owned();
        let comment = has_comment_in(cx, cx.span(node));
        let fix = with_edits(
            Fix::replace(cx.span(node), fixed),
            space_around_keyword(cx, node),
        );
        let (id, what) = if zero {
            ("zero", "is zero")
        } else {
            ("non-zero", "is not zero")
        };
        let msg = format!("Use `.{property} {code}` when checking {property} {what}.");
        let report = cx.report_node(node, msg).id(id);
        if comment {
            return;
        }
        if auto_fix {
            report.fix(fix);
        } else if suggest {
            report.suggest(
                "suggestion",
                format!("Replace `.{property}` with `.{property} {code}`."),
                fix,
            );
        }
    }
}

fn binary_precedence(op: &str) -> u8 {
    match op {
        "===" | "!==" => 9,
        _ => 10,
    }
}

/// `shouldAddParenthesesToComparison`: a comparison put where `node` is needs grouping.
fn comparison_needs_parens(cx: &Ctx<'_, '_>, node: NodeId, op: &str) -> bool {
    if is_parenthesized(cx, node) {
        return false;
    }
    let ast = cx.ast();
    let Some((parent, child)) = parent_of(cx, node) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == child,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == child,
        Node::TaggedTemplate { tag, .. } => tag == child,
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            ..
        } => false,
        Node::Binary { right, .. }
        | Node::As { expr: right, .. }
        | Node::Satisfies { expr: right, .. } => {
            let mine = binary_precedence(op);
            let theirs = match *ast.node(parent) {
                Node::Binary {
                    op: BinaryOp::Exp, ..
                } => 15,
                _ => precedence(ast, parent),
            };
            theirs > mine
                || (theirs == mine
                    && matches!(ast.node(parent), Node::Binary { .. })
                    && right == child)
        }
        Node::Await { .. }
        | Node::NonNull { .. }
        | Node::TypeAssertion { .. }
        | Node::Unary { .. } => true,
        _ => false,
    }
}

// ---- Lengths that are not collection sizes ----------------------------------------------

fn is_safe_non_negative_integer(v: &Static) -> bool {
    matches!(v, Static::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= 9_007_199_254_740_991.0)
}

/// The static key of an object-literal member (`a`, `"a"`, `[k]` with a static `k`).
fn property_key(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<String> {
    let ast = cx.ast();
    let key = match *ast.node(prop) {
        Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
        Node::ShorthandProp { name, .. } => return ident_name(ast, name).map(str::to_owned),
        _ => return None,
    };
    match *ast.node(key) {
        Node::ComputedKey { expr } => static_value(cx, expr)?.to_js_string(),
        _ => key_name(ast, cx.src(), key).map(|k| k.into_owned()),
    }
}

/// The member's value (`a: v` → `v`, shorthand → its name, method → the function).
fn property_value(ast: &Ast, prop: NodeId) -> Option<NodeId> {
    match *ast.node(prop) {
        Node::Property { value, .. } | Node::MethodDef { value, .. } => Some(value),
        Node::ShorthandProp { name, .. } => Some(name),
        _ => None,
    }
}

/// The last member named `name` (none past a spread or an unknown computed key).
fn last_object_property(cx: &Ctx<'_, '_>, obj: NodeId, name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::ObjectLit { props } = *ast.node(skip_parens(ast, obj)) else {
        return None;
    };
    for &p in ast.list(props).iter().rev() {
        if matches!(ast.node(p), Node::Spread { .. }) {
            return None;
        }
        let key = property_key(cx, p)?;
        if key == name {
            return Some(p);
        }
    }
    None
}

fn has_unknown_object_property(cx: &Ctx<'_, '_>, obj: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let Some(Node::ObjectLit { props }) = obj.map(|o| *ast.node(skip_parens(ast, o))) else {
        return true;
    };
    ast.list(props).iter().any(|&p| {
        matches!(ast.node(p), Node::Spread { .. })
            || (matches!(*ast.node(p), Node::Property { key, .. } if matches!(ast.node(key), Node::ComputedKey { .. }))
                && property_key(cx, p).is_none())
    })
}

/// A descriptor object with a `get` or `set` member.
fn is_accessor_descriptor(cx: &Ctx<'_, '_>, node: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let Some(Node::ObjectLit { props }) = node.map(|n| *ast.node(skip_parens(ast, n))) else {
        return false;
    };
    let props = ast.list(props);
    props
        .iter()
        .all(|&p| !matches!(ast.node(p), Node::Spread { .. }))
        && props
            .iter()
            .any(|&p| matches!(property_key(cx, p).as_deref(), Some("get" | "set")))
}

/// `Object.<method>(ref, ...)` with the global `Object`: the call and the method name.
fn object_method_call<'a>(cx: &Ctx<'a, '_>, reference: NodeId) -> Option<(NodeId, String)> {
    let ast = cx.ast();
    let call = cx.parent(reference)?;
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
    } = *ast.node(callee)
    else {
        return None;
    };
    if !cx.is_global_ref(obj, "Object") || ast.list(args).first() != Some(&reference) {
        return None;
    }
    Some((call, ident_name(ast, prop)?.to_owned()))
}

fn call_args(ast: &Ast, call: NodeId) -> &[NodeId] {
    match *ast.node(call) {
        Node::Call { args, .. } => ast.list(args),
        _ => &[],
    }
}

/// The descriptor `Object.defineProperty(ref, name, d)` / `defineProperties(ref, {name: d})`
/// gives the property.
fn property_definition(cx: &Ctx<'_, '_>, reference: NodeId, name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let (call, method) = object_method_call(cx, reference)?;
    let args = call_args(ast, call);
    if method == "defineProperty" {
        let key = *args.get(1)?;
        return (static_value_control_flow(cx, key)
            .and_then(|v| match v {
                Static::Str(s) => Some(s),
                _ => None,
            })
            .as_deref()
            == Some(name))
        .then(|| args.get(2).copied())
        .flatten();
    }
    if method != "defineProperties" {
        return None;
    }
    let defs = *args.get(1)?;
    if !matches!(ast.node(defs), Node::ObjectLit { .. }) {
        return None;
    }
    property_value(ast, last_object_property(cx, defs, name)?)
}

/// `Object.assign(ref, {name: v})`: `v`.
fn assigned_property_value(cx: &Ctx<'_, '_>, reference: NodeId, name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let (call, method) = object_method_call(cx, reference)?;
    let args = call_args(ast, call);
    let &[_, source] = args else {
        return None;
    };
    let Node::ObjectLit { props } = *ast.node(source) else {
        return None;
    };
    if method != "assign"
        || ast
            .list(props)
            .iter()
            .any(|&p| matches!(ast.node(p), Node::Spread { .. }))
    {
        return None;
    }
    property_value(ast, last_object_property(cx, source, name)?)
}

/// The member chain a reference is the object of (`ref.a.b`).
fn referenced_member(cx: &Ctx<'_, '_>, reference: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut node = reference;
    while let Some(p) = cx.parent(node) {
        match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == node => node = p,
            _ => break,
        }
    }
    (node != reference).then_some(node)
}

fn member_static_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => static_value(cx, index)?.to_js_string(),
        _ => None,
    }
}

/// The value assigned to `node` by a plain `=` (through destructuring paths).
fn assignment_value(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    enum Step {
        Index(usize),
        Key(Option<String>),
    }
    let mut path = Vec::new();
    let mut cur = node;
    for _ in 0..1000 {
        let parent = cx.parent(cur)?;
        match *ast.node(parent) {
            Node::Paren { .. } => {}
            Node::Assign { left, op, right } => {
                // A default inside a destructuring target only applies to `undefined`.
                if left != cur
                    || op != AssignOp::Assign
                    || is_assignment_target(ast, cx.file.parents, parent)
                {
                    return None;
                }
                let mut value = right;
                for step in path.iter().rev() {
                    value = skip_parens(ast, value);
                    value = match step {
                        Step::Index(i) => match *ast.node(value) {
                            Node::ArrayLit { elements } => *ast.list(elements).get(*i)?,
                            _ => return None,
                        },
                        Step::Key(k) => {
                            property_value(ast, last_object_property(cx, value, k.as_deref()?)?)?
                        }
                    };
                }
                return Some(value);
            }
            Node::ArrayLit { elements } | Node::ArrayPattern { elements } => {
                let i = ast.list(elements).iter().position(|&e| e == cur)?;
                path.push(Step::Index(i));
            }
            Node::Property { value, .. } if value == cur => {
                path.push(Step::Key(property_key(cx, parent)));
            }
            Node::ObjectLit { .. } | Node::ObjectPattern { .. } => {}
            _ => return None,
        }
        cur = parent;
    }
    None
}

fn is_known_non_negative_integer(cx: &Ctx<'_, '_>, node: Option<NodeId>) -> bool {
    node.and_then(|n| static_value_control_flow(cx, n))
        .is_some_and(|v| is_safe_non_negative_integer(&v))
}

fn is_known_numeric_mutation(cx: &Ctx<'_, '_>, reference: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    if let Some(d) = property_definition(cx, reference, name)
        && matches!(ast.node(skip_parens(ast, d)), Node::ObjectLit { .. })
    {
        let value = last_object_property(cx, d, "value").and_then(|p| property_value(ast, p));
        return is_known_non_negative_integer(cx, value);
    }
    let Some(member) = referenced_member(cx, reference) else {
        return is_known_non_negative_integer(cx, assigned_property_value(cx, reference, name));
    };
    if let Some(p) = cx.parent(member)
        && let Node::ForOf { right, .. } = *ast.node(p)
    {
        return matches!(static_value_control_flow(cx, right), Some(Static::Array(items))
            if !items.is_empty() && items.iter().all(is_safe_non_negative_integer));
    }
    is_known_non_negative_integer(cx, assignment_value(cx, member))
}

fn is_property_mutation(cx: &Ctx<'_, '_>, reference: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    if let Some(d) = property_definition(cx, reference, name) {
        return !is_accessor_descriptor(cx, Some(d));
    }
    if let Some((call, method)) = object_method_call(cx, reference) {
        let args = call_args(ast, call);
        match method.as_str() {
            "assign" => {
                if args.len() < 2 {
                    return false;
                }
                if assigned_property_value(cx, reference, name).is_some() {
                    return true;
                }
                return args.len() != 2 || has_unknown_object_property(cx, args.get(1).copied());
            }
            "defineProperty" => {
                return match args.get(1).and_then(|&k| static_value_control_flow(cx, k)) {
                    None => true,
                    Some(v) => {
                        v.to_js_string().as_deref() == Some(name) && matches!(v, Static::Str(_))
                    }
                };
            }
            "defineProperties" => return has_unknown_object_property(cx, args.get(1).copied()),
            _ => {}
        }
    }
    let Some(member) = referenced_member(cx, reference) else {
        return false;
    };
    member_static_name(cx, member).as_deref() == Some(name) && is_written(cx, member)
}

fn is_for_in_mutation(cx: &Ctx<'_, '_>, reference: NodeId, name: &str) -> bool {
    let Some(member) = referenced_member(cx, reference) else {
        return false;
    };
    member_static_name(cx, member).as_deref() == Some(name)
        && cx
            .parent(member)
            .is_some_and(|p| matches!(*cx.node(p), Node::ForIn { left, .. } if left == member))
}

fn is_property_read(cx: &Ctx<'_, '_>, reference: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let member = referenced_member(cx, reference)
        .filter(|&m| member_static_name(cx, m).as_deref() == Some(name));
    let Some(member) = member else {
        return object_method_call(cx, reference).is_some_and(|(_, m)| {
            matches!(m.as_str(), "assign" | "defineProperty" | "defineProperties")
        });
    };
    let Some((p, child)) = parent_of(cx, member) else {
        return true;
    };
    !match *ast.node(p) {
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == child,
        Node::TaggedTemplate { tag, .. } => tag == child,
        _ => false,
    }
}

/// The function, method, field or static block whose code `node` belongs to.
fn execution_context(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = cx.parent(node);
    while let Some(c) = cur {
        if matches!(
            cx.node(c),
            Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::PropertyDef { .. }
                | Node::StaticBlock { .. }
        ) {
            return Some(c);
        }
        cur = cx.parent(c);
    }
    None
}

fn is_conditionally_executed(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..100_000 {
        let Some(parent) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(parent) {
            Node::If { test, cons, alt } => {
                if cons == cur || alt == Some(cur) {
                    match static_value_control_flow(cx, test) {
                        Some(v) if v.truthy() == (cons == cur) => {}
                        _ => return true,
                    }
                }
            }
            Node::Conditional { test, cons, alt } => {
                if cons == cur || alt == cur {
                    match static_value_control_flow(cx, test) {
                        Some(v) if v.truthy() == (cons == cur) => {}
                        _ => return true,
                    }
                }
            }
            Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right,
                ..
            } if right == cur => return true,
            Node::For { body, update, .. } if body == cur || update == Some(cur) => return true,
            Node::While { body, .. } | Node::ForIn { body, .. } | Node::ForOf { body, .. }
                if body == cur =>
            {
                return true;
            }
            Node::SwitchCase { test, body } if ast.list(body).contains(&cur) => {
                let unconditional = test.is_none()
                    && cx.parent(parent).is_some_and(|s| {
                        matches!(*ast.node(s), Node::Switch { cases, .. } if ast.list(cases).len() == 1)
                    });
                if !unconditional {
                    return true;
                }
            }
            Node::CatchClause { body, .. } if body == cur => return true,
            Node::Try { block, .. } if block == cur => return true,
            Node::AssignPattern { right, .. } if right == cur => return true,
            Node::Param { init: Some(i), .. } if i == cur => return true,
            Node::BindingProperty { init: Some(i), .. } if i == cur => return true,
            Node::ShorthandProp { init: Some(i), .. } if i == cur => return true,
            Node::Index { index, .. } if index == cur && chain_has_optional(ast, parent) => {
                return true;
            }
            Node::Call {
                callee,
                optional,
                args,
                ..
            } if ast.list(args).contains(&cur) && (optional || chain_has_optional(ast, callee)) => {
                return true;
            }
            _ => {}
        }
        cur = parent;
    }
    false
}

/// `isKnownNonCollectionLengthOrSize`.
pub(crate) fn known_non_collection_length(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    let ast = cx.ast();
    if let Some(v) = static_value_no_side_effects(cx, member) {
        return !is_safe_non_negative_integer(&v);
    }
    let Node::Member { obj, prop, .. } = *ast.node(member) else {
        return false;
    };
    let object = skip_parens(ast, obj);
    if !matches!(ast.node(object), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_ident(object) else {
        return false;
    };
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls.as_slice() else {
        return false;
    };
    if !matches!(
        decl.kind,
        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
    ) {
        return false;
    }
    let Node::VarDeclarator {
        init: Some(init), ..
    } = *ast.node(decl.node)
    else {
        return false;
    };
    let Node::ObjectLit { props } = *ast.node(init) else {
        return false;
    };
    let Some(name) = ident_name(ast, prop) else {
        return false;
    };
    if ast.list(props).iter().any(|&p| {
        matches!(
            *ast.node(p),
            Node::MethodDef {
                kind: MethodKind::Get | MethodKind::Set,
                ..
            }
        ) && property_key(cx, p).as_deref() == Some(name)
    }) {
        return true;
    }
    let context = execution_context(cx, member);
    let member_start = cx.span(member).start;
    let mut unknown_effect = false;
    let mut numeric_mutation = false;
    let mut accessor = false;
    for &r in &symbol.refs {
        let reference = sem.reference(r);
        if reference.flags.contains(RefFlags::INIT) {
            continue;
        }
        let ident = reference.node;
        let unknown_context =
            execution_context(cx, ident) != context || is_conditionally_executed(cx, ident);
        if !unknown_context && cx.span(ident).start > member_start {
            continue;
        }
        if is_for_in_mutation(cx, ident, name) {
            unknown_effect = true;
            continue;
        }
        if is_property_mutation(cx, ident, name) {
            if !unknown_context && is_known_numeric_mutation(cx, ident, name) {
                numeric_mutation = true;
            } else {
                unknown_effect = true;
            }
            continue;
        }
        if property_definition(cx, ident, name).is_some_and(|d| is_accessor_descriptor(cx, Some(d)))
        {
            if unknown_context {
                unknown_effect = true;
            } else {
                accessor = true;
            }
            continue;
        }
        if !is_property_read(cx, ident, name) {
            if unknown_context {
                unknown_effect = true;
                continue;
            }
            return false;
        }
    }
    if unknown_effect {
        return true;
    }
    if numeric_mutation {
        return false;
    }
    if accessor {
        return true;
    }
    match static_value_no_side_effects(cx, init) {
        Some(Static::Object(entries)) => !entries
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .is_some_and(|(_, v)| is_safe_non_negative_integer(v)),
        Some(_) => true,
        None => false,
    }
}
