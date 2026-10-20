//! `@typescript-eslint/no-unsafe-enum-comparison`: compare enum values only with values of
//! the same enum.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-enum-comparison",
        "Disallow comparing an enum value with a non-enum value",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create_default::<NoUnsafeEnumComparison>,
};

#[derive(Debug, Default)]
struct NoUnsafeEnumComparison;

const CONDITION: &str = "The two values in this comparison do not have a shared enum type.";
const CASE: &str = "The case statement does not have a shared enum type with the switch predicate.";

impl Rule for NoUnsafeEnumComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Switch]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Binary { left, op, right } => {
                let equality = matches!(
                    op,
                    BinaryOp::Eq | BinaryOp::StrictEq | BinaryOp::NotEq | BinaryOp::StrictNotEq
                );
                if !equality
                    && !matches!(
                        op,
                        BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq
                    )
                {
                    return;
                }
                if !mismatched(cx, left, right) {
                    return;
                }
                let fix = if equality {
                    replace_with_member(cx, left, right)
                        .or_else(|| replace_with_member(cx, right, left))
                } else {
                    None
                };
                let r = cx.report_node(node, CONDITION).id("mismatchedCondition");
                if let Some(f) = fix {
                    r.suggest(
                        "replaceValueWithEnum",
                        "Replace with an enum value comparison.",
                        f,
                    );
                }
            }
            Node::Switch { disc, cases } => {
                for &c in cx.ast().list(cases).to_vec().iter() {
                    let Node::SwitchCase {
                        test: Some(test), ..
                    } = *cx.node(c)
                    else {
                        continue;
                    };
                    if mismatched(cx, disc, test) {
                        cx.report_node(c, CASE).id("mismatchedCase");
                    }
                }
            }
            _ => {}
        }
    }
}

/// The enum of an enum member type (or computed enum type).
fn enum_of(ti: &dyn TypeInfo, t: Ty) -> Option<Sym> {
    let f = ti.flags(t);
    if f.contains(TypeFlags::ENUM_LITERAL) {
        let member = ti.type_symbol(t)?;
        return ti.symbol_parent(member);
    }
    if f.contains(TypeFlags::ENUM) {
        return ti.type_symbol(t);
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Number,
    String,
}

fn enum_value_kind(ti: &dyn TypeInfo, t: Ty) -> Option<ValueKind> {
    let f = ti.flags(t);
    if !f.intersects(TypeFlags::ENUM_LIKE) {
        return None;
    }
    if f.contains(TypeFlags::STRING_LITERAL) {
        Some(ValueKind::String)
    } else {
        Some(ValueKind::Number)
    }
}

/// Every part of `t` is number-like (string-like); intersections count when a member is.
fn all_like(ti: &dyn TypeInfo, t: Ty, like: TypeFlags) -> bool {
    let ps = parts(ti, t);
    !ps.is_empty()
        && ps.iter().all(|&p| {
            if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                ti.intersection_members(p)
                    .into_iter()
                    .any(|m| ti.flags(m).intersects(like))
            } else {
                ti.flags(p).intersects(like)
            }
        })
}

/// The same type, treating fresh and regular literal types alike.
fn same_type(ti: &dyn TypeInfo, a: Ty, b: Ty) -> bool {
    if a == b {
        return true;
    }
    let (fa, fb) = (ti.flags(a), ti.flags(b));
    let lit = TypeFlags::LITERAL | TypeFlags::ENUM_LITERAL;
    if !fa.intersects(lit) || (fa & lit) != (fb & lit) {
        return false;
    }
    if fa.contains(TypeFlags::ENUM_LITERAL) {
        return ti.type_symbol(a) == ti.type_symbol(b);
    }
    ti.literal_text(a).is_some() && ti.literal_text(a) == ti.literal_text(b)
}

fn mismatched(cx: &Ctx<'_, '_>, left: NodeId, right: NodeId) -> bool {
    let ti = cx.types();
    let (Some(lt), Some(rt)) = (ti.type_at(left), ti.type_at(right)) else {
        return false;
    };
    if has_unresolved_part(ti, lt) || has_unresolved_part(ti, rt) {
        return false;
    }
    let lp = parts(ti, lt);
    let rp = parts(ti, rt);
    let le: Vec<Sym> = lp.iter().filter_map(|&p| enum_of(ti, p)).collect();
    let re: Vec<Sym> = rp.iter().filter_map(|&p| enum_of(ti, p)).collect();
    if le.is_empty() && re.is_empty() {
        return false;
    }
    if le.iter().any(|e| re.contains(e)) {
        return false;
    }
    let any_like = TypeFlags::ANY | TypeFlags::UNKNOWN | TypeFlags::NULLABLE;
    if lp
        .iter()
        .chain(rp.iter())
        .any(|&p| ti.flags(p).intersects(any_like))
    {
        return false;
    }
    // `Enum | number` / `Enum | string`: any number (string) is fine to compare.
    let wide = |ps: &[Ty]| {
        ps.iter().any(|&p| {
            let f = ti.flags(p);
            f == TypeFlags::NUMBER || f == TypeFlags::STRING
        })
    };
    if (!le.is_empty() && wide(&lp)) || (!re.is_empty() && wide(&rp)) {
        return false;
    }
    if lp.iter().any(|&a| rp.iter().any(|&b| same_type(ti, a, b))) {
        return false;
    }
    violates(ti, &lp, rt) || violates(ti, &rp, lt)
}

/// `parts` holds number (string) enum members and `other` is number-like (string-like).
fn violates(ti: &dyn TypeInfo, parts: &[Ty], other: Ty) -> bool {
    let kinds: Vec<ValueKind> = parts
        .iter()
        .filter_map(|&p| enum_value_kind(ti, p))
        .collect();
    (kinds.contains(&ValueKind::Number) && all_like(ti, other, TypeFlags::NUMBER_LIKE))
        || (kinds.contains(&ValueKind::String) && all_like(ti, other, TypeFlags::STRING_LIKE))
}

/// Replaces the constant `value` with the member of `enum_side`'s enum that has its value.
fn replace_with_member(cx: &Ctx<'_, '_>, enum_side: NodeId, value: NodeId) -> Option<Fix> {
    let ti = cx.types();
    let et = ti.type_at(enum_side)?;
    let enums: Vec<Sym> = parts(ti, et)
        .into_iter()
        .filter_map(|p| enum_of(ti, p))
        .collect();
    let first = *enums.first()?;
    if enums.iter().any(|&e| e != first) {
        return None;
    }
    let possible = parts(ti, et);
    let text = if let Some(s) = static_string_eval(cx, value) {
        (s, true)
    } else {
        (number_to_string(static_number(cx, value)?), false)
    };
    let enum_obj = ti.type_of_symbol(first);
    for m in ti.properties_of(enum_obj) {
        let mt = ti.type_of_symbol(m);
        let is_string = ti.flags(mt).contains(TypeFlags::STRING_LITERAL);
        if is_string != text.1 || ti.literal_text(mt).as_deref() != Some(text.0.as_str()) {
            continue;
        }
        // The member must be a value the enum side can hold.
        if !possible
            .iter()
            .any(|&p| same_type(ti, p, mt) || !ti.flags(p).contains(TypeFlags::ENUM_LITERAL))
        {
            return None;
        }
        let enum_name = ti.symbol_name(first);
        let key = ti.symbol_name(m);
        let access = if is_identifier_name(&key) {
            format!("{enum_name}.{key}")
        } else {
            format!(
                "{enum_name}['{}']",
                key.replace('\\', "\\\\").replace('\'', "\\'")
            )
        };
        return Some(Fix::replace(cx.span(value), access));
    }
    None
}

fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c == '$' || c.is_alphabetic())
        && chars.all(is_ident_char)
}
