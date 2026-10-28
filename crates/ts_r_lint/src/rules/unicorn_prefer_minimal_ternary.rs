//! `unicorn/prefer-minimal-ternary`: `test ? X : Y` where both branches have the same
//! outer shape and only one part varies, so the ternary can move into that part. The
//! shapes (branches compared through parentheses; `a.b` and `a[b]` count as one kind):
//! - calls without optional links or spreads: the same simple callee (identifier, literal,
//!   `this`, `super`, or a member of one with a dot or literal key) with exactly one
//!   differing argument, or the same arguments with a differing callee (opt-in: a
//!   differing identifier or receiver with `checkVaryingBase`, a differing static method
//!   name with `checkComputedMemberAccess`);
//! - binary operators (not logical, not `#x in`), the same on exactly one side; a shared
//!   left side must be simple;
//! - member access on the same simple receiver with differing dynamic keys (always), or
//!   differing static names (`checkComputedMemberAccess`), or the same static name on
//!   differing receivers (`checkVaryingBase`, not `super`);
//! - objects with the same plain keys in order, arrays without holes/spreads, and
//!   `new Name(...)` with the same type arguments: one differing value, the values before
//!   it simple.
//!
//! The differing item may not itself be a ternary. "The same" is: equal static strings,
//! equal text, the same computed static key on the same object, or the same tokens
//! (comments and whitespace ignored, a directly compared object's trailing comma too, the
//! contents of strings, templates, regexes and JSX text kept) when neither side holds a
//! function or class.
//!
//! The fix rebuilds the expression around `test ? x : y`, but only when the moved shared
//! parts are literals or (with the condition) side-effect-free simple expressions; objects
//! whose varying value is a function, class or TS wrapper, comments inside the ternary, a
//! member read in a callee/tag/`delete`/TS-wrapper position, and `eval` callees get no fix.
//! In TypeScript (file or parser) an object whose shared values are not all literals gets
//! a suggestion instead. The plugin's type-checker branches (type safety, `const enum`
//! receivers) are not implemented.

use super::prelude::*;
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{is_parenthesized, needs_semicolon, paren_text, static_string_value};
use super::unicorn_util::static_property_name;
use ts_r_ast::{Ast, BinaryOp, NodeKind as K, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-minimal-ternary",
        "Prefer moving ternaries into the minimal varying part of an expression.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferMinimalTernary>,
};

const MESSAGE: &str = "Move the ternary into the varying part of the expression.";
const MAX_DEPTH: u32 = 200;

#[derive(Debug, Default, Clone, Copy)]
struct PreferMinimalTernary {
    check_varying_base: bool,
    check_computed_member_access: bool,
}

impl FromOptions for PreferMinimalTernary {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut out = PreferMinimalTernary::default();
        if let Some(o) = opts.first() {
            let Some(obj) = o.as_object() else {
                return Err("expected an object".into());
            };
            for (k, v) in obj {
                let Some(b) = v.as_bool() else {
                    return Err(format!("`{k}` must be a boolean"));
                };
                match k.as_str() {
                    "checkVaryingBase" => out.check_varying_base = b,
                    "checkComputedMemberAccess" => out.check_computed_member_access = b,
                    _ => return Err(format!("unknown option `{k}`")),
                }
            }
        }
        Ok(out)
    }
}

fn sp(ast: &Ast, id: NodeId) -> NodeId {
    skip_parens(ast, id)
}

/// The ESTree node type, with `a.b` and `a[b]` as one kind.
fn shape(ast: &Ast, id: NodeId) -> K {
    match ast.kind(id) {
        K::Index => K::Member,
        k => k,
    }
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    static_string_value(ast, sp(ast, id))
}

fn contains_function_or_class(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            return true;
        }
        if matches!(
            ast.node(n),
            Node::FunctionExpr { .. }
                | Node::FunctionDecl { .. }
                | Node::Arrow { .. }
                | Node::ClassExpr { .. }
                | Node::ClassDecl { .. }
        ) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// The tokens of `id` as one string: comments and whitespace between tokens dropped (a
/// space kept between two word characters), literal contents verbatim.
fn token_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(id);
    let mut verbatim: Vec<(u32, u32)> = Vec::new();
    let mut stack = vec![id];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        match ast.node(n) {
            Node::StringLit { .. }
            | Node::TemplateElement { .. }
            | Node::RegexLit { .. }
            | Node::JsxText {} => {
                let s = cx.span(n);
                verbatim.push((s.start, s.end));
            }
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    let mut cuts: Vec<(u32, u32, bool)> = verbatim.into_iter().map(|(s, e)| (s, e, true)).collect();
    for c in ast.comments_in(span) {
        cuts.push((c.span.start, c.span.end, false));
    }
    cuts.sort_unstable();
    let mut out = String::new();
    let mut pending_space = false;
    let mut pos = span.start;
    let push_plain = |out: &mut String, text: &str, pending: &mut bool| {
        for ch in text.chars() {
            if ch.is_whitespace() || ch == '\u{feff}' {
                *pending = true;
                continue;
            }
            if *pending && out.chars().next_back().is_some_and(is_word_char) && is_word_char(ch) {
                out.push(' ');
            }
            *pending = false;
            out.push(ch);
        }
    };
    for (s, e, keep) in cuts {
        if s < pos || e > span.end {
            continue;
        }
        push_plain(&mut out, &src[pos as usize..s as usize], &mut pending_space);
        if keep {
            out.push('\u{1}');
            out.push_str(&src[s as usize..e as usize]);
            out.push('\u{1}');
            pending_space = false;
        } else {
            pending_space = true;
        }
        pos = e;
    }
    push_plain(
        &mut out,
        &src[pos as usize..span.end as usize],
        &mut pending_space,
    );
    if matches!(ast.node(id), Node::ObjectLit { .. }) && out.ends_with(",}") {
        out.truncate(out.len() - 2);
        out.push('}');
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#' || c == '\\' || !c.is_ascii()
}

fn same_expression(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let (a, b) = (sp(ast, a), sp(ast, b));
    if let Some(s) = static_string(ast, a) {
        return static_string(ast, b) == Some(s);
    }
    if cx.text(a) == cx.text(b) {
        return true;
    }
    if depth < MAX_DEPTH
        && let (
            Node::Index {
                obj: o1,
                optional: p1,
                index: i1,
            },
            Node::Index {
                obj: o2,
                optional: p2,
                index: i2,
            },
        ) = (*ast.node(a), *ast.node(b))
        && p1 == p2
        && let Some(k) = static_string(ast, i1)
        && static_string(ast, i2) == Some(k)
    {
        return same_expression(cx, o1, o2, depth + 1);
    }
    token_text(cx, a) == token_text(cx, b)
        && !contains_function_or_class(ast, a)
        && !contains_function_or_class(ast, b)
}

fn is_safe_shared(ast: &Ast, id: NodeId) -> bool {
    let id = sp(ast, id);
    matches!(
        ast.node(id),
        Node::Ident { .. } | Node::This {} | Node::Super {}
    ) || is_literal(ast, id)
        || static_string(ast, id).is_some()
}

fn literal_or_static_key(ast: &Ast, index: NodeId) -> bool {
    let index = sp(ast, index);
    is_literal(ast, index) || static_string(ast, index).is_some()
}

fn is_safe_shared_callee(ast: &Ast, id: NodeId) -> bool {
    let id = sp(ast, id);
    is_safe_shared(ast, id)
        || match *ast.node(id) {
            Node::Member { obj, .. } => is_safe_shared(ast, obj),
            Node::Index { obj, index, .. } => {
                is_safe_shared(ast, obj) && literal_or_static_key(ast, index)
            }
            _ => false,
        }
}

fn has_optional_chain_element(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            _ => return false,
        }
    }
    false
}

fn member_parts(ast: &Ast, id: NodeId) -> Option<(NodeId, bool, NodeId)> {
    match *ast.node(sp(ast, id)) {
        Node::Member { obj, prop, .. } => Some((sp(ast, obj), false, prop)),
        Node::Index { obj, index, .. } => Some((sp(ast, obj), true, index)),
        _ => None,
    }
}

fn static_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let member = sp(cx.ast(), member);
    if let Node::Member { prop, .. } = *cx.node(member)
        && matches!(cx.node(prop), Node::PrivateName { .. })
    {
        return None;
    }
    static_property_name(cx, member)
}

impl PreferMinimalTernary {
    fn same_object_different_static_property(
        &self,
        cx: &Ctx<'_, '_>,
        a: NodeId,
        b: NodeId,
    ) -> bool {
        let ast = cx.ast();
        let (Some((o1, _, _)), Some((o2, _, _))) = (member_parts(ast, a), member_parts(ast, b))
        else {
            return false;
        };
        if has_optional_chain_element(ast, a)
            || has_optional_chain_element(ast, b)
            || !is_safe_shared(ast, o1)
            || !same_expression(cx, o1, o2, 0)
        {
            return false;
        }
        matches!((static_name(cx, a), static_name(cx, b)), (Some(x), Some(y)) if x != y)
    }

    fn same_static_property_different_object(
        &self,
        cx: &Ctx<'_, '_>,
        a: NodeId,
        b: NodeId,
    ) -> bool {
        let ast = cx.ast();
        let (Some((o1, _, _)), Some((o2, _, _))) = (member_parts(ast, a), member_parts(ast, b))
        else {
            return false;
        };
        if has_optional_chain_element(ast, a)
            || has_optional_chain_element(ast, b)
            || matches!(ast.node(o1), Node::Super {})
            || matches!(ast.node(o2), Node::Super {})
        {
            return false;
        }
        matches!((static_name(cx, a), static_name(cx, b)), (Some(x), Some(y)) if x == y)
            && !same_expression(cx, o1, o2, 0)
    }

    fn minimal_callee_difference(&self, cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
        let ast = cx.ast();
        let (a, b) = (sp(ast, a), sp(ast, b));
        (self.check_varying_base
            && matches!(
                (ast.node(a), ast.node(b)),
                (Node::Ident { .. }, Node::Ident { .. })
            )
            && cx.text(a) != cx.text(b))
            || (self.check_computed_member_access
                && self.same_object_different_static_property(cx, a, b))
            || (self.check_varying_base && self.same_static_property_different_object(cx, a, b))
    }

    fn is_minimal_call(&self, cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
        let ast = cx.ast();
        let (
            Node::Call {
                callee: c1,
                args: a1,
                ..
            },
            Node::Call {
                callee: c2,
                args: a2,
                ..
            },
        ) = (*ast.node(a), *ast.node(b))
        else {
            return false;
        };
        let (a1, a2) = (ast.list(a1), ast.list(a2));
        if has_optional_chain_element(ast, a)
            || has_optional_chain_element(ast, b)
            || a1.iter().chain(a2).any(|&x| is_spread(ast, x))
        {
            return false;
        }
        if same_expression(cx, c1, c2, 0)
            && is_safe_shared_callee(ast, c1)
            && different_index(cx, a1, a2).is_some()
        {
            return true;
        }
        same_items(cx, a1, a2) && self.minimal_callee_difference(cx, c1, c2)
    }

    fn is_minimal_member(&self, cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
        same_object_different_dynamic_key(cx, a, b)
            || (self.check_computed_member_access
                && self.same_object_different_static_property(cx, a, b))
            || (self.check_varying_base && self.same_static_property_different_object(cx, a, b))
    }

    fn is_minimal(&self, cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
        let ast = cx.ast();
        if shape(ast, a) != shape(ast, b) {
            return false;
        }
        self.is_minimal_call(cx, a, b)
            || is_minimal_binary(cx, a, b)
            || (shape(ast, a) == K::Member && self.is_minimal_member(cx, a, b))
            || is_minimal_object(cx, a, b)
            || is_minimal_array(cx, a, b)
            || is_minimal_new(cx, a, b)
    }
}

fn is_spread(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Spread { .. })
}

fn different_index(cx: &Ctx<'_, '_>, a: &[NodeId], b: &[NodeId]) -> Option<usize> {
    let ast = cx.ast();
    if a.len() != b.len() {
        return None;
    }
    let mut found = None;
    for (i, (&x, &y)) in a.iter().zip(b).enumerate() {
        if same_expression(cx, x, y, 0) {
            continue;
        }
        if found.is_some()
            || matches!(ast.node(sp(ast, x)), Node::Conditional { .. })
            || matches!(ast.node(sp(ast, y)), Node::Conditional { .. })
        {
            return None;
        }
        found = Some(i);
    }
    found
}

fn same_items(cx: &Ctx<'_, '_>, a: &[NodeId], b: &[NodeId]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| same_expression(cx, x, y, 0))
}

fn one_minimal_value_difference(cx: &Ctx<'_, '_>, a: &[NodeId], b: &[NodeId]) -> bool {
    different_index(cx, a, b).is_some_and(|i| a[..i].iter().all(|&x| is_safe_shared(cx.ast(), x)))
}

fn is_minimal_binary(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    let (
        Node::Binary {
            left: l1,
            op: op1,
            right: r1,
        },
        Node::Binary {
            left: l2,
            op: op2,
            right: r2,
        },
    ) = (*ast.node(a), *ast.node(b))
    else {
        return false;
    };
    if op1 != op2 || is_logical(op1) {
        return false;
    }
    let private_brand =
        |l: NodeId| op1 == BinaryOp::In && matches!(ast.node(l), Node::PrivateName { .. });
    if private_brand(l1) || private_brand(l2) {
        return false;
    }
    let left_same = same_expression(cx, l1, l2, 0);
    let right_same = same_expression(cx, r1, r2, 0);
    if left_same == right_same {
        return false;
    }
    right_same || is_safe_shared(ast, l1)
}

fn is_logical(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
    )
}

fn same_object_different_dynamic_key(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    let (
        Node::Index {
            obj: o1, index: i1, ..
        },
        Node::Index {
            obj: o2, index: i2, ..
        },
    ) = (*ast.node(a), *ast.node(b))
    else {
        return false;
    };
    if has_optional_chain_element(ast, a)
        || has_optional_chain_element(ast, b)
        || !is_safe_shared(ast, o1)
        || !same_expression(cx, o1, o2, 0)
    {
        return false;
    }
    static_name(cx, a).is_none() && static_name(cx, b).is_none() && !same_expression(cx, i1, i2, 0)
}

/// An object literal property as `(key name, value, shorthand)` when it is a plain one.
fn ordinary_property(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<(String, NodeId, bool)> {
    let ast = cx.ast();
    match *ast.node(prop) {
        Node::ShorthandProp {
            name, init: None, ..
        } => Some((cx.text(name).to_string(), name, true)),
        Node::Property { key, value, .. } => {
            let name = match *ast.node(key) {
                Node::Ident { .. } => cx.text(key).to_string(),
                Node::StringLit { value } => ast.atom(value).to_string(),
                Node::NumberLit { .. } | Node::BigIntLit { .. } => {
                    super::unicorn_static::static_value(cx, key)?.to_js_string()?
                }
                _ => return None,
            };
            (name != "__proto__").then_some((name, value, false))
        }
        _ => None,
    }
}

fn object_values(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Vec<(String, NodeId, bool)>> {
    let Node::ObjectLit { props } = *cx.node(id) else {
        return None;
    };
    cx.ast()
        .list(props)
        .iter()
        .map(|&p| ordinary_property(cx, p))
        .collect()
}

fn is_minimal_object(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let (Some(pa), Some(pb)) = (object_values(cx, a), object_values(cx, b)) else {
        return false;
    };
    pa.len() == pb.len()
        && pa.iter().zip(&pb).all(|(x, y)| x.0 == y.0)
        && one_minimal_value_difference(
            cx,
            &pa.iter().map(|p| p.1).collect::<Vec<_>>(),
            &pb.iter().map(|p| p.1).collect::<Vec<_>>(),
        )
}

fn array_elements(ast: &Ast, id: NodeId) -> Option<&[NodeId]> {
    let Node::ArrayLit { elements } = *ast.node(id) else {
        return None;
    };
    let elements = ast.list(elements);
    (!elements
        .iter()
        .any(|&e| matches!(ast.node(e), Node::Hole {} | Node::Spread { .. })))
    .then_some(elements)
}

fn is_minimal_array(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    match (array_elements(ast, a), array_elements(ast, b)) {
        (Some(x), Some(y)) => one_minimal_value_difference(cx, x, y),
        _ => false,
    }
}

fn same_type_args(cx: &Ctx<'_, '_>, a: Option<NodeId>, b: Option<NodeId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => token_text(cx, x) == token_text(cx, y),
        _ => false,
    }
}

fn call_type_args(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::Call { type_args, .. } | Node::New { type_args, .. } => type_args,
        _ => None,
    }
}

fn is_minimal_new(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    let (
        Node::New {
            callee: c1,
            type_args: t1,
            args: a1,
            ..
        },
        Node::New {
            callee: c2,
            type_args: t2,
            args: a2,
            ..
        },
    ) = (*ast.node(a), *ast.node(b))
    else {
        return false;
    };
    let (c1, c2) = (sp(ast, c1), sp(ast, c2));
    let (a1, a2) = (ast.list(a1), ast.list(a2));
    matches!(
        (ast.node(c1), ast.node(c2)),
        (Node::Ident { .. }, Node::Ident { .. })
    ) && cx.text(c1) == cx.text(c2)
        && !a1.iter().chain(a2).any(|&x| is_spread(ast, x))
        && same_type_args(cx, t1, t2)
        && one_minimal_value_difference(cx, a1, a2)
}

// ---- The fix -------------------------------------------------------------------------------

fn is_safe_to_reorder(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let id = unwrap_ts(ast, id);
    if is_safe_shared(ast, id) {
        return true;
    }
    match *ast.node(id) {
        Node::Member { obj, .. } => is_safe_to_reorder(ast, obj, depth + 1),
        Node::Index { obj, index, .. } => {
            is_safe_to_reorder(ast, obj, depth + 1) && literal_or_static_key(ast, index)
        }
        Node::Unary { op, arg } => {
            matches!(op, UnaryOp::Not | UnaryOp::TypeOf | UnaryOp::Void)
                && is_safe_to_reorder(ast, arg, depth + 1)
        }
        Node::Binary { left, op, right } => {
            matches!(
                op,
                BinaryOp::StrictEq
                    | BinaryOp::StrictNotEq
                    | BinaryOp::LogicalAnd
                    | BinaryOp::LogicalOr
                    | BinaryOp::Coalesce
            ) && is_safe_to_reorder(ast, left, depth + 1)
                && is_safe_to_reorder(ast, right, depth + 1)
        }
        Node::Conditional { test, cons, alt } => {
            is_safe_to_reorder(ast, test, depth + 1)
                && is_safe_to_reorder(ast, cons, depth + 1)
                && is_safe_to_reorder(ast, alt, depth + 1)
        }
        _ => false,
    }
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

fn can_move_before_condition(ast: &Ast, id: NodeId) -> bool {
    is_safe_to_reorder(ast, id, 0) && !has_side_effect(ast, id)
}

struct Builder<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    condition: NodeId,
    condition_text: String,
}

impl Builder<'_, '_, '_> {
    /// `getText`: the text with its parentheses; a bare sequence gets new ones.
    fn text(&self, id: NodeId) -> String {
        let ast = self.cx.ast();
        let id = sp(ast, id);
        let text = paren_text(self.cx, id).to_string();
        if matches!(ast.node(id), Node::Seq { .. }) && !is_parenthesized(self.cx, id) {
            format!("({text})")
        } else {
            text
        }
    }

    fn conditional(&self, a: NodeId, b: NodeId) -> String {
        format!(
            "{} ? {} : {}",
            self.condition_text,
            self.text(a),
            self.text(b)
        )
    }

    /// The text of `whole` with `part` replaced.
    fn replace(&self, whole: NodeId, part: NodeId, text: &str) -> String {
        let src = self.cx.src();
        let w = self.cx.span(sp(self.cx.ast(), whole));
        let p = self.cx.span(sp(self.cx.ast(), part));
        format!(
            "{}{text}{}",
            &src[w.start as usize..p.start as usize],
            &src[p.end as usize..w.end as usize]
        )
    }

    fn can_move_before_test(&self, exprs: &[NodeId]) -> bool {
        let ast = self.cx.ast();
        exprs.iter().all(|&e| {
            let e = sp(ast, e);
            is_literal(ast, e) || static_string(ast, e).is_some()
        }) || (can_move_before_condition(ast, self.condition)
            && exprs.iter().all(|&e| can_move_before_condition(ast, e)))
    }

    fn minimal_text(&self, left: NodeId, right: NodeId, depth: u32) -> Option<String> {
        let cx = self.cx;
        let ast = cx.ast();
        let (left, right) = (sp(ast, left), sp(ast, right));
        if depth > 8 {
            return None;
        }
        if matches!(ast.node(left), Node::Ident { .. }) {
            if cx.text(left) == "eval" || cx.text(right) == "eval" {
                return None;
            }
            return Some(format!("({})", self.conditional(left, right)));
        }
        if let (Some((o1, c1, k1)), Some((o2, c2, k2))) =
            (member_parts(ast, left), member_parts(ast, right))
        {
            if !same_expression(cx, o1, o2, 0) {
                if [(c1, k1), (c2, k2)]
                    .iter()
                    .any(|&(computed, key)| computed && !literal_or_static_key(ast, key))
                {
                    return None;
                }
                return Some(self.replace(left, o1, &format!("({})", self.conditional(o1, o2))));
            }
            if !self.can_move_before_test(&[o1]) {
                return None;
            }
            let key = |computed: bool, key: NodeId| {
                if computed {
                    self.text(key)
                } else {
                    serde_json::to_string(cx.text(key)).unwrap_or_default()
                }
            };
            return Some(format!(
                "{}[{} ? {} : {}]",
                self.text(o1),
                self.condition_text,
                key(c1, k1),
                key(c2, k2)
            ));
        }
        if let (
            Node::Binary {
                left: l1,
                right: r1,
                ..
            },
            Node::Binary {
                left: l2,
                right: r2,
                ..
            },
        ) = (*ast.node(left), *ast.node(right))
        {
            let left_same = same_expression(cx, l1, l2, 0);
            if left_same && !self.can_move_before_test(&[l1]) {
                return None;
            }
            let (x, y) = if left_same { (r1, r2) } else { (l1, l2) };
            return Some(self.replace(left, x, &format!("({})", self.conditional(x, y))));
        }
        let (items_left, items_right): (Vec<NodeId>, Vec<NodeId>);
        let mut shorthand = Vec::new();
        match (*ast.node(left), *ast.node(right)) {
            (Node::Call { callee: c1, .. }, Node::Call { callee: c2, .. })
            | (Node::New { callee: c1, .. }, Node::New { callee: c2, .. }) => {
                if !same_type_args(cx, call_type_args(ast, left), call_type_args(ast, right)) {
                    return None;
                }
                if !same_expression(cx, c1, c2, 0) {
                    let callee = self.minimal_text(c1, c2, depth + 1)?;
                    return Some(self.replace(left, c1, &callee));
                }
                if !self.can_move_before_test(&[c1]) {
                    return None;
                }
                let args = |id: NodeId| match *ast.node(id) {
                    Node::Call { args, .. } | Node::New { args, .. } => ast.list(args).to_vec(),
                    _ => Vec::new(),
                };
                items_left = args(left);
                items_right = args(right);
            }
            (Node::ObjectLit { .. }, Node::ObjectLit { .. }) => {
                let pa = object_values(cx, left)?;
                let pb = object_values(cx, right)?;
                shorthand = pa.iter().map(|p| p.2).collect();
                items_left = pa.iter().map(|p| p.1).collect();
                items_right = pb.iter().map(|p| p.1).collect();
            }
            (Node::ArrayLit { elements: e1 }, Node::ArrayLit { elements: e2 }) => {
                items_left = ast.list(e1).to_vec();
                items_right = ast.list(e2).to_vec();
            }
            _ => return None,
        }
        let index = different_index(cx, &items_left, &items_right)?;
        if !self.can_move_before_test(&items_left[..index]) {
            return None;
        }
        let object = matches!(ast.node(left), Node::ObjectLit { .. });
        if object
            && [items_left[index], items_right[index]].iter().any(|&item| {
                matches!(
                    ast.node(unwrap_ts(ast, item)),
                    Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::ClassExpr { .. }
                )
            })
        {
            return None;
        }
        let conditional = self.conditional(items_left[index], items_right[index]);
        if object && shorthand[index] {
            let Node::ObjectLit { props } = *ast.node(left) else {
                return None;
            };
            let prop = ast.list(props)[index];
            let name = items_left[index];
            return Some(self.replace(left, prop, &format!("{}: {conditional}", cx.text(name))));
        }
        Some(self.replace(left, items_left[index], &conditional))
    }
}

/// `getConditionalExpressionChildText` of the condition.
fn condition_text(cx: &Ctx<'_, '_>, test: NodeId) -> String {
    let ast = cx.ast();
    let test = sp(ast, test);
    let text = paren_text(cx, test).to_string();
    if !is_parenthesized(cx, test)
        && matches!(
            ast.node(test),
            Node::Await { .. }
                | Node::Assign { .. }
                | Node::Yield { .. }
                | Node::Seq { .. }
                | Node::As { .. }
                | Node::TypeAssertion { .. }
        )
    {
        format!("({text})")
    } else {
        text
    }
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
    None
}

fn fix(cx: &Ctx<'_, '_>, node: NodeId, test: NodeId, left: NodeId, right: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let span = cx.span(node);
    if !ast.comments_in(span).is_empty() {
        return None;
    }
    if shape(ast, left) == K::Member
        && let Some((p, held)) = parent_np(cx, node)
    {
        let blocked = match *ast.node(p) {
            Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => true,
            Node::Call { callee, .. } => callee == held,
            Node::TaggedTemplate { tag, .. } => tag == held,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => true,
            _ => false,
        };
        if blocked {
            return None;
        }
    }
    let builder = Builder {
        cx,
        condition: test,
        condition_text: condition_text(cx, test),
    };
    let mut text = builder.minimal_text(left, right, 0)?;
    if let Some(rest) = text.strip_prefix("let")
        && rest.trim_start().starts_with('[')
    {
        return None;
    }
    if matches!(ast.node(left), Node::ObjectLit { .. })
        || matches!(
            ast.node(left),
            Node::Binary {
                op: BinaryOp::In,
                ..
            }
        )
    {
        text = format!("({text})");
    }
    if !is_parenthesized(cx, node) && needs_semicolon(cx, span.start, &text) {
        text = format!(";{text}");
    }
    Some(Fix::replace(span, text))
}

/// `isSyntaxSafeToMinimize`: in TypeScript, an object's shared values must be literals.
fn syntax_safe(cx: &Ctx<'_, '_>, left: NodeId, right: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(left), Node::ObjectLit { .. })
        || !(cx.env().typescript || super::unicorn_prefer_spread::is_ts_file(cx))
    {
        return true;
    }
    let (Some(pa), Some(pb)) = (object_values(cx, left), object_values(cx, right)) else {
        return true;
    };
    let a: Vec<NodeId> = pa.iter().map(|p| p.1).collect();
    let b: Vec<NodeId> = pb.iter().map(|p| p.1).collect();
    let index = different_index(cx, &a, &b);
    a.iter().enumerate().all(|(i, &item)| {
        let item = sp(ast, item);
        Some(i) == index || is_literal(ast, item) || static_string(ast, item).is_some()
    })
}

impl Rule for PreferMinimalTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Conditional { test, cons, alt } = *ast.node(id) else {
            return;
        };
        let (left, right) = (sp(ast, cons), sp(ast, alt));
        if !self.is_minimal(cx, left, right) {
            return;
        }
        let safe = syntax_safe(cx, left, right);
        let object = matches!(ast.node(left), Node::ObjectLit { .. });
        let fix = if safe || object {
            fix(cx, id, test, left, right)
        } else {
            None
        };
        let span = cx.span(id);
        let r = cx.report(span, MESSAGE).id("prefer-minimal-ternary");
        if let Some(fix) = fix {
            if safe {
                r.fix(fix);
            } else {
                r.suggest("prefer-minimal-ternary", MESSAGE.to_string(), fix);
            }
        }
    }
}
