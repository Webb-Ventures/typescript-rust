//! `@typescript-eslint/prefer-find`: `arr.find(f)` over `arr.filter(f)[0]` and
//! `arr.filter(f).at(0)`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-find",
        "Enforce the use of Array.prototype.find() over Array.prototype.filter() followed by [0] when looking for a single result",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .suggestions(),
    create: create_default::<PreferFind>,
};

#[derive(Debug, Default)]
struct PreferFind;

const MESSAGE: &str = "Prefer .find(...) instead of .filter(...)[0].";

impl Rule for PreferFind {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index, NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // `X[0]` or `X.at(0)`: `obj` is X, `cut` the position where `[0]`/`.at(0)` starts.
        let obj = match *cx.node(node) {
            Node::Index {
                obj,
                optional: false,
                index,
            } => {
                if !static_value(cx, index, 0).is_some_and(|v| v.is_zero_index()) {
                    return;
                }
                obj
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let [arg] = ast.list(args)[..] else {
                    return;
                };
                if matches!(cx.node(arg), Node::Spread { .. }) {
                    return;
                }
                let obj = match *cx.node(callee) {
                    Node::Member {
                        obj,
                        optional: false,
                        prop,
                    } if ident_name(ast, prop) == Some("at") => obj,
                    Node::Index {
                        obj,
                        optional: false,
                        index,
                    } if static_string_eval(cx, index).as_deref() == Some("at") => obj,
                    _ => return,
                };
                if !static_value(cx, arg, 0).is_some_and(|v| v.is_zero_at()) {
                    return;
                }
                obj
            }
            _ => return,
        };
        let mut props = Vec::new();
        if !filter_calls(cx, obj, &mut props, 0) {
            return;
        }
        let src = cx.src();
        let cut = skip_trivia_forward(src, cx.span(obj).end as usize) as u32;
        let mut fix = Fix::delete(Span::new(cut, cx.span(node).end));
        for (p, computed) in props {
            fix = fix.and(cx.span(p), if computed { "\"find\"" } else { "find" });
        }
        cx.report_node(node, MESSAGE).id("preferFind").suggest(
            "preferFindSuggestion",
            "Use .find(...) instead of .filter(...)[0].",
            fix,
        );
    }
}

/// Collects the property nodes of the `filter` calls `e` evaluates to (through parens, the
/// last expression of a sequence and both branches of a conditional). False when some path
/// is not an array `filter` call.
fn filter_calls(cx: &Ctx<'_, '_>, e: NodeId, out: &mut Vec<(NodeId, bool)>, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *cx.node(e) {
        Node::Seq { exprs } => match ast.list(exprs).last() {
            Some(&last) => filter_calls(cx, last, out, depth + 1),
            None => false,
        },
        Node::Conditional { cons, alt, .. } => {
            filter_calls(cx, cons, out, depth + 1) && filter_calls(cx, alt, out, depth + 1)
        }
        Node::Call {
            callee,
            optional: false,
            ..
        } => {
            let callee = skip_parens(ast, callee);
            let (obj, prop, computed) = match *cx.node(callee) {
                Node::Member { obj, prop, .. } if ident_name(ast, prop) == Some("filter") => {
                    (obj, prop, false)
                }
                Node::Index { obj, index, .. }
                    if static_string_eval(cx, index).as_deref() == Some("filter") =>
                {
                    (obj, index, true)
                }
                _ => return false,
            };
            let ti = cx.types();
            let Some(t) = ti.type_at(obj) else {
                return false;
            };
            if !is_array_like(ti, t) {
                return false;
            }
            out.push((prop, computed));
            true
        }
        _ => false,
    }
}

/// Every non-nullish union member (at least one) is an array or tuple (an intersection
/// with one counts).
fn is_array_like(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED || is_any(ti, t) {
        return false;
    }
    let mut any = false;
    for p in parts(ti, t) {
        if ti.flags(p).intersects(TypeFlags::NULLABLE) {
            continue;
        }
        let p = constrained(ti, p);
        let arr = |m: Ty| m != Ty::UNRESOLVED && (ti.is_array(m) || ti.is_tuple(m));
        let ok = if ti.flags(p).contains(TypeFlags::INTERSECTION) {
            ti.intersection_members(p).into_iter().any(arr)
        } else if ti.flags(p).contains(TypeFlags::UNION) {
            is_array_like(ti, p)
        } else {
            arr(p)
        };
        if !ok {
            return false;
        }
        any = true;
    }
    any
}

/// A statically known value (a subset of ESLint's `getStaticValue`).
enum StaticVal {
    Num(f64),
    Str(String),
    BigInt(bool),
}

impl StaticVal {
    /// `x[v]` reads element 0.
    fn is_zero_index(&self) -> bool {
        match self {
            StaticVal::Num(n) => *n == 0.0,
            StaticVal::Str(s) => s == "0",
            StaticVal::BigInt(zero) => *zero,
        }
    }

    /// `x.at(v)` reads element 0 (`ToIntegerOrInfinity(v) == 0`).
    fn is_zero_at(&self) -> bool {
        let n = match self {
            StaticVal::Num(n) => *n,
            StaticVal::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else {
                    t.parse::<f64>().unwrap_or(f64::NAN)
                }
            }
            StaticVal::BigInt(_) => return false,
        };
        n.is_nan() || n.trunc() == 0.0
    }
}

fn static_value(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> Option<StaticVal> {
    if depth > 16 {
        return None;
    }
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *cx.node(e) {
        Node::NumberLit { value } => Some(StaticVal::Num(value.get())),
        Node::BigIntLit { value } => {
            let v = parse_bigint(ast.atom(value))?;
            Some(StaticVal::BigInt(v.trim_start_matches('-') == "0"))
        }
        Node::StringLit { .. } | Node::TemplateLit { .. } => {
            static_string_eval(cx, e).map(StaticVal::Str)
        }
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => match static_value(cx, arg, depth + 1)? {
            StaticVal::Num(n) => Some(StaticVal::Num(-n)),
            StaticVal::BigInt(z) => Some(StaticVal::BigInt(z)),
            StaticVal::Str(_) => None,
        },
        Node::Ident { .. } => {
            let name = ident_name(ast, e)?;
            if (name == "NaN" || name == "Infinity") && cx.is_global_ref(e, name) {
                return Some(StaticVal::Num(if name == "NaN" {
                    f64::NAN
                } else {
                    f64::INFINITY
                }));
            }
            static_value(cx, const_init(cx, e)?, depth + 1)
        }
        _ => None,
    }
}
