//! `@typescript-eslint/prefer-includes`: `x.includes(y)` over `x.indexOf(y) !== -1`, and
//! `s.includes('lit')` over `/lit/.test(s)`.

use super::prelude::*;
use crate::regex::{self, RKind};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-includes",
        "Enforce `includes` method over `indexOf` method",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<PreferIncludes>,
};

#[derive(Debug, Default)]
struct PreferIncludes;

impl Rule for PreferIncludes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Binary { left, op, right } => check_index_of(cx, node, left, op, right),
            Node::Call { .. } => check_regex_test(cx, node),
            _ => {}
        }
    }
}

/// `x.indexOf(..) !== -1` and its relatives.
fn check_index_of(cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, op: BinaryOp, right: NodeId) {
    let ast = cx.ast();
    let Some(v) = static_number(cx, right) else {
        return;
    };
    if !matches!(
        *cx.node(skip_parens(ast, right)),
        Node::NumberLit { .. } | Node::Unary { .. }
    ) {
        return;
    }
    let (minus_one, zero) = (v == -1.0, v == 0.0 && v.is_sign_positive());
    let negate = match op {
        BinaryOp::StrictNotEq | BinaryOp::NotEq | BinaryOp::Gt if minus_one => false,
        BinaryOp::GtEq if zero => false,
        BinaryOp::StrictEq | BinaryOp::Eq | BinaryOp::LtEq if minus_one => true,
        BinaryOp::Lt if zero => true,
        _ => return,
    };
    let call = skip_parens(ast, left);
    let Node::Call {
        callee,
        optional: call_optional,
        ..
    } = *cx.node(call)
    else {
        return;
    };
    let (obj, prop, member_optional) = match *cx.node(callee) {
        Node::Member {
            obj,
            prop,
            optional,
        } if ident_name(ast, prop) == Some("indexOf") => (obj, prop, optional),
        _ => return,
    };
    let ti = cx.types();
    let Some(t) = ti.type_at(obj) else {
        return;
    };
    if !has_matching_includes(ti, t) {
        return;
    }
    let fix = if call_optional || member_optional || chain_has_optional(ast, obj) {
        None
    } else {
        let src = cx.src();
        let call_span = cx.span(call);
        let prop_span = cx.span(prop);
        let mut text = String::new();
        if negate {
            text.push('!');
        }
        text.push_str(&src[call_span.start as usize..prop_span.start as usize]);
        text.push_str("includes");
        text.push_str(&src[prop_span.end as usize..call_span.end as usize]);
        Some(Fix::replace(cx.span(node), text))
    };
    cx.report_node(node, "Use 'includes()' method instead.")
        .id("preferIncludes")
        .fix_opt(fix);
}

/// Every union member of `t` has `indexOf` and an `includes` taking the same parameters.
fn has_matching_includes(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED || is_any(ti, t) {
        return false;
    }
    let t = constrained(ti, ti.non_nullable(t));
    let ps = parts(ti, t);
    !ps.is_empty()
        && ps.into_iter().all(|p| {
            let p = ti.apparent_type(constrained(ti, p));
            let (Some(io), Some(inc)) = (
                ti.property_of_type(p, "indexOf"),
                ti.property_of_type(p, "includes"),
            ) else {
                return false;
            };
            let io_sigs = ti.signatures(ti.type_of_symbol(io), false);
            let inc_sigs = ti.signatures(ti.type_of_symbol(inc), false);
            !io_sigs.is_empty()
                && io_sigs
                    .iter()
                    .all(|&a| inc_sigs.iter().any(|&b| same_params(ti, a, b)))
        })
}

fn same_params(ti: &dyn TypeInfo, a: Sig, b: Sig) -> bool {
    let (na, ra) = ti.param_count(a);
    let (nb, rb) = ti.param_count(b);
    if na != nb || ra != rb || ti.min_arg_count(a) != ti.min_arg_count(b) {
        return false;
    }
    (0..na).all(|i| match (ti.param_type(a, i), ti.param_type(b, i)) {
        (Some(x), Some(y)) => {
            x == y || (x != Ty::UNRESOLVED && ti.is_assignable(x, y) && ti.is_assignable(y, x))
        }
        _ => false,
    })
}

/// `/lit/.test(s)` with a plain-string pattern and a string argument.
fn check_regex_test(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *cx.node(node)
    else {
        return;
    };
    let [arg] = ast.list(args)[..] else {
        return;
    };
    let Node::Member {
        obj,
        prop,
        optional: false,
    } = *cx.node(callee)
    else {
        return;
    };
    if ident_name(ast, prop) != Some("test") {
        return;
    }
    let Some(lit) = regex_literal_string(cx, obj, 0) else {
        return;
    };
    let ti = cx.types();
    let Some(at) = ti.type_at(arg) else {
        return;
    };
    if !is_string_like_type(ti, at) {
        return;
    }
    let src = cx.src();
    let inner = skip_parens(ast, arg);
    let arg_text = cx.span(inner).text(src);
    let target = if needs_parens_for_member(ast, inner) {
        format!("({arg_text})")
    } else {
        arg_text.to_string()
    };
    let text = format!("{target}.includes({})", js_single_quoted(&lit));
    let span = cx.span(node);
    cx.report_node(
        node,
        "Use `String#includes()` method with a string instead.",
    )
    .id("preferStringIncludes")
    .fix(Fix::replace(span, text));
}

/// The plain string a regex (a flagless literal, `new RegExp('lit')`, or a `const` bound to
/// one) matches.
fn regex_literal_string(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *cx.node(e) {
        Node::RegexLit { pattern, flags } => {
            if !ast.atom(flags).is_empty() {
                return None;
            }
            plain_pattern_string(ast.atom(pattern))
        }
        Node::New { .. } => {
            let [a] = regexp_call_args(cx, e, false, false)? else {
                return None;
            };
            let Node::StringLit { value } = *cx.node(*a) else {
                return None;
            };
            plain_pattern_string(ast.atom(value))
        }
        Node::Ident { .. } => regex_literal_string(cx, const_init(cx, e)?, depth + 1),
        _ => None,
    }
}

/// The string a pattern made only of literal characters matches.
pub(crate) fn plain_pattern_string(pattern: &str) -> Option<String> {
    let re = regex::parse(pattern, regex::Flags::default()).ok()?;
    let root = re.node(re.root);
    let [alt] = root.children[..] else {
        return None;
    };
    let mut units: Vec<u16> = Vec::new();
    for &c in &re.node(alt).children {
        let RKind::Char { value } = *re.kind(c) else {
            return None;
        };
        if value > 0xFFFF {
            let ch = char::from_u32(value)?;
            let mut buf = [0u16; 2];
            units.extend_from_slice(ch.encode_utf16(&mut buf));
        } else {
            units.push(value as u16);
        }
    }
    String::from_utf16(&units).ok()
}

/// A single-quoted JavaScript string literal for `s`.
pub(crate) fn js_single_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\0' => out.push_str("\\0"),
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{b}' => out.push_str("\\v"),
            '\t' => out.push_str("\\t"),
            '\u{c}' => out.push_str("\\f"),
            '\u{8}' => out.push_str("\\b"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// An expression that needs parentheses as the object of a member access.
pub(crate) fn needs_parens_for_member(ast: &Ast, e: NodeId) -> bool {
    !matches!(
        *ast.node(e),
        Node::Ident { .. }
            | Node::This {}
            | Node::Super {}
            | Node::StringLit { .. }
            | Node::TemplateLit { .. }
            | Node::RegexLit { .. }
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::BigIntLit { .. }
            | Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::Paren { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::Call { .. }
            | Node::TaggedTemplate { .. }
            | Node::NonNull { .. }
            | Node::MetaProperty { .. }
            | Node::ImportCall { .. }
    )
}
