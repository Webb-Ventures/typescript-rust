//! `yoda`: require or disallow literals on the left side of comparisons.

use super::prelude::*;
use std::cmp::Ordering;
use ts_r_ast::{Ast, ParentMap};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "yoda",
        "Require or disallow \"Yoda\" conditions",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<Yoda>,
};

#[derive(Debug, Default)]
struct Yoda {
    always: bool,
    except_range: bool,
    only_equality: bool,
}

impl FromOptions for Yoda {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let always = opts.first().and_then(Value::as_str) == Some("always");
        let o = opts.get(1).and_then(Value::as_object);
        Ok(Yoda {
            always,
            except_range: o.is_some_and(|o| bool_field(o, "exceptRange", false)),
            only_equality: o.is_some_and(|o| bool_field(o, "onlyEquality", false)),
        })
    }
}

impl Rule for Yoda {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        use BinaryOp::*;
        if !matches!(
            op,
            Eq | NotEq | StrictEq | StrictNotEq | Lt | LtEq | Gt | GtEq
        ) {
            return;
        }
        if self.only_equality && !matches!(op, Eq | StrictEq) {
            return;
        }
        let ast = cx.ast();
        let src = cx.src();
        let (expected_lit, expected_other) = if self.always {
            (left, right)
        } else {
            (right, left)
        };
        if !looks_like_literal(ast, expected_other) || looks_like_literal(ast, expected_lit) {
            return;
        }
        if self.except_range && is_range_test(ast, src, cx.file.parents, node) {
            return;
        }
        let msg = format!(
            "Expected literal to be on the {} side of {}.",
            if self.always { "left" } else { "right" },
            op.as_str()
        );
        let fix = flipped(ast, src, node, left, op, right);
        cx.report(cx.span(node), msg).id("expected").fix_opt(fix);
    }
}

pub(crate) fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::RegexLit { .. }
    )
}

fn negative_number(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => {
            let arg = skip_parens(ast, arg);
            matches!(
                ast.node(arg),
                Node::NumberLit { .. } | Node::BigIntLit { .. }
            )
            .then_some(arg)
        }
        _ => None,
    }
}

fn static_template(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::TemplateLit { parts } => {
            let p = ast.list(parts);
            (p.len() == 1).then(|| p[0])
        }
        _ => None,
    }
}

fn looks_like_literal(ast: &Ast, id: NodeId) -> bool {
    is_literal(ast, id) || negative_number(ast, id).is_some() || static_template(ast, id).is_some()
}

/// A literal's value, as far as JS relational comparison needs it.
enum Val {
    Num(f64),
    Big(f64),
    Str(String),
}

fn bigint_value(ast: &Ast, value: ts_r_ast::Atom) -> f64 {
    let raw = ast.atom(value);
    parse_bigint(raw)
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::NAN)
}

fn literal_value(ast: &Ast, src: &str, id: NodeId) -> Option<Val> {
    if let Some(arg) = negative_number(ast, id) {
        return Some(match *ast.node(arg) {
            Node::NumberLit { value } => Val::Num(-value.get()),
            Node::BigIntLit { value } => Val::Big(-bigint_value(ast, value)),
            _ => return None,
        });
    }
    if let Some(el) = static_template(ast, id) {
        let Node::TemplateElement { cooked, .. } = *ast.node(el) else {
            return None;
        };
        return Some(Val::Str(ast.atom(cooked).to_owned()));
    }
    let id = skip_parens(ast, id);
    Some(match *ast.node(id) {
        Node::NumberLit { value } => Val::Num(value.get()),
        Node::BigIntLit { value } => Val::Big(bigint_value(ast, value)),
        Node::StringLit { value } => Val::Str(ast.atom(value).to_owned()),
        Node::BoolLit { value } => Val::Num(if value { 1.0 } else { 0.0 }),
        Node::NullLit {} => Val::Num(0.0),
        Node::RegexLit { .. } => Val::Str(ast.span(id).text(src).to_owned()),
        _ => return None,
    })
}

/// JS `ToNumber` for a string.
fn to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    for (p, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(d) = t.strip_prefix(p) {
            return u64::from_str_radix(d, radix).map_or(f64::NAN, |v| v as f64);
        }
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if t.bytes()
        .any(|b| !(b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')))
    {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// JS `a <= b`.
fn less_or_equal(a: &Val, b: &Val) -> bool {
    let ord = match (a, b) {
        (Val::Str(x), Val::Str(y)) => Some(x.encode_utf16().cmp(y.encode_utf16())),
        _ => {
            let num = |v: &Val| match v {
                Val::Num(n) | Val::Big(n) => *n,
                Val::Str(s) => to_number(s),
            };
            num(a).partial_cmp(&num(b))
        }
    };
    matches!(ord, Some(Ordering::Less | Ordering::Equal))
}

/// Whether `a` and `b` name the same thing (`x.y` and `x["y"]` do).
fn same_reference(ast: &Ast, src: &str, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    match (*ast.node(a), *ast.node(b)) {
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (Node::Ident { name: x }, Node::Ident { name: y })
        | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => {
            ast.atom(x) == ast.atom(y)
        }
        (
            Node::Member { obj: oa, .. } | Node::Index { obj: oa, .. },
            Node::Member { obj: ob, .. } | Node::Index { obj: ob, .. },
        ) => {
            let (oa, ob) = (oa, ob);
            if let Some(name) = static_name(ast, src, a) {
                return same_reference(ast, src, oa, ob, depth + 1)
                    && static_name(ast, src, b).as_deref() == Some(&*name);
            }
            match (*ast.node(a), *ast.node(b)) {
                (Node::Member { prop: pa, .. }, Node::Member { prop: pb, .. })
                | (Node::Index { index: pa, .. }, Node::Index { index: pb, .. }) => {
                    same_reference(ast, src, oa, ob, depth + 1)
                        && same_reference(ast, src, pa, pb, depth + 1)
                }
                _ => false,
            }
        }
        _ if is_literal(ast, a) && is_literal(ast, b) => same_literal(ast, a, b),
        _ => false,
    }
}

fn static_name<'a>(ast: &'a Ast, src: &'a str, id: NodeId) -> Option<std::borrow::Cow<'a, str>> {
    if let Node::Member { prop, .. } = *ast.node(id)
        && matches!(ast.node(prop), Node::PrivateName { .. })
    {
        return None;
    }
    member_name(ast, src, id)
}

pub(crate) fn same_literal(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    match (*ast.node(a), *ast.node(b)) {
        (
            Node::RegexLit {
                pattern: p1,
                flags: f1,
            },
            Node::RegexLit {
                pattern: p2,
                flags: f2,
            },
        ) => ast.atom(p1) == ast.atom(p2) && ast.atom(f1) == ast.atom(f2),
        (Node::BigIntLit { value: x }, Node::BigIntLit { value: y }) => {
            parse_bigint(ast.atom(x)) == parse_bigint(ast.atom(y))
        }
        (Node::StringLit { value: x }, Node::StringLit { value: y }) => ast.atom(x) == ast.atom(y),
        (Node::NumberLit { value: x }, Node::NumberLit { value: y }) => x.get() == y.get(),
        (Node::BoolLit { value: x }, Node::BoolLit { value: y }) => x == y,
        (Node::NullLit {}, Node::NullLit {}) => true,
        _ => false,
    }
}

/// `0 <= x && x < 1` / `x < 0 || 1 <= x`, wrapped in parentheses, around `node`.
fn is_range_test(ast: &Ast, src: &str, parents: &ParentMap, node: NodeId) -> bool {
    let Some(parent) = parent_skip_parens(ast, parents, node) else {
        return false;
    };
    let Node::Binary { left, op, right } = *ast.node(parent) else {
        return false;
    };
    let range_op = |id: NodeId| match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Lt | BinaryOp::LtEq,
            right,
        } => Some((left, right)),
        _ => None,
    };
    let (Some((ll, lr)), Some((rl, rr))) = (range_op(left), range_op(right)) else {
        return false;
    };
    let ordered =
        |lo: NodeId, hi: NodeId| match (literal_value(ast, src, lo), literal_value(ast, src, hi)) {
            (None, None) => false,
            (Some(a), Some(b)) => less_or_equal(&a, &b),
            _ => true,
        };
    let matches = match op {
        BinaryOp::LogicalAnd => same_reference(ast, src, lr, rl, 0) && ordered(ll, rr),
        BinaryOp::LogicalOr => same_reference(ast, src, ll, rr, 0) && ordered(lr, rl),
        _ => false,
    };
    matches && is_parenthesised(ast, src, parent)
}

/// Skips whitespace and comments from `pos` forwards.
fn skip_trivia(src: &str, mut pos: usize) -> usize {
    let b = src.as_bytes();
    while pos < b.len() {
        let rest = &src[pos..];
        if let Some(c) = rest.chars().next()
            && c.is_whitespace()
        {
            pos += c.len_utf8();
        } else if let Some(body) = rest.strip_prefix("/*") {
            pos = body.find("*/").map_or(b.len(), |i| pos + 2 + i + 2);
        } else if rest.starts_with("//") {
            pos = rest.find('\n').map_or(b.len(), |i| pos + i);
        } else {
            break;
        }
    }
    pos
}

/// Previous non-whitespace character before `pos` (comments ending there are not skipped,
/// which is enough for `(` detection in practice).
fn prev_significant(src: &str, pos: usize) -> Option<char> {
    let mut s = src[..pos].trim_end();
    while let Some(head) = s.strip_suffix("*/") {
        let Some(i) = head.rfind("/*") else { break };
        s = head[..i].trim_end();
    }
    s.chars().next_back()
}

fn is_parenthesised(ast: &Ast, src: &str, id: NodeId) -> bool {
    let span = ast.span(id);
    prev_significant(src, span.start as usize) == Some('(')
        && src[skip_trivia(src, span.end as usize)..].starts_with(')')
}

fn adjacent_merge(a: char, b: char) -> bool {
    (is_ident_char(a) && is_ident_char(b)) || (a == '+' && b == '+') || (a == '-' && b == '-')
}

fn flipped(
    ast: &Ast,
    src: &str,
    node: NodeId,
    left: NodeId,
    op: BinaryOp,
    right: NodeId,
) -> Option<Fix> {
    let span = ast.span(node);
    let (start, end) = (span.start as usize, span.end as usize);
    let left_end = ast.span(left).end as usize;
    let right_start = ast.span(right).start as usize;
    let op_text = op.as_str();
    let op_start = skip_trivia(src, left_end);
    if op_start > right_start || !src[op_start..].starts_with(op_text) {
        return None;
    }
    let op_end = op_start + op_text.len();
    let flipped_op = match op {
        BinaryOp::Lt => ">",
        BinaryOp::Gt => "<",
        BinaryOp::LtEq => ">=",
        BinaryOp::GtEq => "<=",
        _ => op_text,
    };
    let left_text = &src[start..left_end];
    let right_text = &src[right_start..end];
    let mut out = String::new();
    if let (Some(before), Some(first)) =
        (src[..start].chars().next_back(), right_text.chars().next())
        && adjacent_merge(before, first)
    {
        out.push(' ');
    }
    out.push_str(right_text);
    out.push_str(&src[left_end..op_start]);
    out.push_str(flipped_op);
    out.push_str(&src[op_end..right_start]);
    out.push_str(left_text);
    if let (Some(last), Some(after)) = (left_text.chars().next_back(), src[end..].chars().next())
        && adjacent_merge(last, after)
    {
        out.push(' ');
    }
    Some(Fix::replace(span, out))
}
