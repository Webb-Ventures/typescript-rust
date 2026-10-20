//! `@typescript-eslint/no-duplicate-enum-values`: disallow duplicate enum member values.

use super::prelude::*;
use super::ts_no_require_imports::plain_string;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-duplicate-enum-values",
        "Disallow duplicate enum member values",
        TS_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDuplicateEnumValues>,
};

#[derive(Debug, Default)]
struct NoDuplicateEnumValues;

#[derive(Clone, PartialEq)]
enum Val {
    Str(String),
    Num(f64),
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

fn eval(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<Val> {
    if depth > 20 {
        return None;
    }
    if let Some(s) = plain_string(cx, id) {
        return Some(Val::Str(s.to_owned()));
    }
    match *cx.node(id) {
        Node::NumberLit { value } => Some(Val::Num(value.get())),
        Node::Unary {
            op: op @ (UnaryOp::Plus | UnaryOp::Minus),
            arg,
        } => {
            let v = match eval(cx, arg, depth + 1)? {
                Val::Num(n) => n,
                Val::Str(s) => to_number(&s),
            };
            Some(Val::Num(if op == UnaryOp::Minus { -v } else { v }))
        }
        _ => None,
    }
}

fn same(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Str(x), Val::Str(y)) => x == y,
        // `Object.is`: NaN never repeats and -0 differs from 0.
        (Val::Num(x), Val::Num(y)) => {
            !x.is_nan() && x == y && x.is_sign_negative() == y.is_sign_negative()
        }
        _ => false,
    }
}

impl Rule for NoDuplicateEnumValues {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Enum]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Enum { members, .. } = *cx.node(node) else {
            return;
        };
        let mut seen: Vec<Val> = Vec::new();
        for &m in cx.ast().list(members) {
            let Node::EnumMember {
                init: Some(init), ..
            } = *cx.node(m)
            else {
                continue;
            };
            let Some(v) = eval(cx, init, 0) else {
                continue;
            };
            if seen.iter().any(|s| same(s, &v)) {
                let shown = match &v {
                    Val::Str(s) => s.clone(),
                    Val::Num(n) => number_to_string(*n),
                };
                cx.report_node(m, format!("Duplicate enum member value {shown}."))
                    .id("duplicateValue");
            } else {
                seen.push(v);
            }
        }
    }
}
