//! `unicorn/no-conflicting-constraints`, the JSX part (the CSS and HTML parts need those
//! languages, which ts-r-lint does not lint): on a spread-free `<input>` or `<textarea>`,
//! a minimum greater than its maximum. `<textarea>` and text-like `<input type>`s (`text`,
//! the default, `search`, `url`, `tel`, `email`, `password`) compare `minLength` /
//! `maxLength` as non-negative safe integers; `number` and `range` inputs compare `min` /
//! `max` as decimal numbers (sign, fraction, exponent); any other or unknown `type` (not a
//! static string, or written twice) is skipped. Values are attribute strings (numeric
//! character references decoded) or expressions with a control-flow-safe static string or
//! number. An attribute written twice has no value. The maximum's value is reported.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_util::decode_jsx_numeric_entities;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-conflicting-constraints",
        "Disallow conflicting CSS query and HTML form constraints.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoConflictingConstraints>,
};

#[derive(Debug, Default)]
struct NoConflictingConstraints;

const NAMES: [&str; 5] = ["type", "min", "max", "minLength", "maxLength"];
const TEXT_TYPES: [&str; 6] = ["text", "search", "url", "tel", "email", "password"];

/// One attribute: the node to report and its static value.
#[derive(Clone)]
struct Attr {
    node: NodeId,
    value: Option<Static>,
}

/// `^-?(\d+(\.\d+)?|\.\d+)(e[+-]?\d+)?$`, case-insensitive.
fn is_html_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > start
    };
    if digits(&mut i) {
        if b.get(i) == Some(&b'.') {
            i += 1;
            if !digits(&mut i) {
                return false;
            }
        }
    } else if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    } else {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

fn html_number(value: Option<&Static>, is_length: bool) -> Option<f64> {
    let text = match value? {
        Static::Str(s) => s.clone(),
        v @ Static::Num(_) => v.to_js_string()?,
        _ => return None,
    };
    let ok = if is_length {
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
    } else {
        is_html_number(&text)
    };
    if !ok {
        return None;
    }
    let n: f64 = text.parse().ok()?;
    let safe = n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0;
    (n.is_finite() && (!is_length || safe)).then_some(n)
}

impl Rule for NoConflictingConstraints {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::JsxOpening]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::JsxOpening { name, attrs, .. } = *cx.node(node) else {
            return;
        };
        let tag = match *ast.node(name) {
            Node::Ident { name } => ast.atom(name),
            _ => return,
        };
        if tag != "input" && tag != "textarea" {
            return;
        }
        let attrs = ast.list(attrs);
        if attrs
            .iter()
            .any(|&a| matches!(ast.node(a), Node::JsxSpreadAttr { .. }))
        {
            return;
        }
        // `None` once an attribute is written twice.
        let mut found: [(bool, Option<Attr>); 5] = Default::default();
        for &a in attrs {
            let Node::JsxAttr { name, value } = *ast.node(a) else {
                continue;
            };
            let Node::Ident { name } = *ast.node(name) else {
                continue;
            };
            let Some(slot) = NAMES.iter().position(|&n| n == ast.atom(name)) else {
                continue;
            };
            if found[slot].0 {
                found[slot].1 = None;
                continue;
            }
            let attr = match value.map(|v| (v, ast.node(v))) {
                Some((v, Node::StringLit { value })) => Attr {
                    node: v,
                    value: Some(Static::Str(decode_jsx_numeric_entities(ast.atom(*value)))),
                },
                Some((v, Node::JsxExprContainer { expr })) => match *expr {
                    Some(e) => Attr {
                        node: e,
                        value: static_value_control_flow(cx, e),
                    },
                    None => Attr {
                        node: v,
                        value: None,
                    },
                },
                Some((v, _)) => Attr {
                    node: v,
                    value: None,
                },
                None => Attr {
                    node: a,
                    value: None,
                },
            };
            found[slot] = (true, Some(attr));
        }
        let mut is_length = true;
        if tag == "input" {
            let ty = match &found[0] {
                (false, _) => String::new(),
                (
                    true,
                    Some(Attr {
                        value: Some(Static::Str(s)),
                        ..
                    }),
                ) => s.to_ascii_lowercase(),
                _ => return,
            };
            let ty = if ty.is_empty() { "text" } else { ty.as_str() };
            is_length = TEXT_TYPES.contains(&ty);
            if !is_length && ty != "number" && ty != "range" {
                return;
            }
        }
        let (min, max, min_name, max_name) = if is_length {
            (3, 4, "minLength", "maxLength")
        } else {
            (1, 2, "min", "max")
        };
        let value = |slot: usize| found[slot].1.as_ref().and_then(|a| a.value.as_ref());
        let (Some(lo), Some(hi)) = (
            html_number(value(min), is_length),
            html_number(value(max), is_length),
        ) else {
            return;
        };
        if lo <= hi {
            return;
        }
        let Some(target) = found[max].1.as_ref().map(|a| a.node) else {
            return;
        };
        cx.report_node(
            target,
            format!("`{min_name}` must not be greater than `{max_name}`."),
        )
        .id("no-conflicting-constraints/html");
    }
}
