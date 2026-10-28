//! `unicorn/no-ineffective-csp-directives`: browsers ignore `frame-ancestors`, `sandbox` and
//! `report-uri` in a `<meta http-equiv="Content-Security-Policy">` policy. Only JSX is
//! linted here (the plugin's HTML half needs an HTML language): a lowercase `<meta>` without
//! spreads, exactly one `httpEquiv`/`http-equiv` and one `content` attribute, each a string
//! or an expression whose static value (through `const` bindings) is a string. Every
//! ineffective directive is reported once, on the `content` value.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_util::decode_jsx_entities;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-ineffective-csp-directives",
        "Disallow ineffective CSP directives in `<meta>` elements.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoIneffectiveCsp>,
};

#[derive(Debug, Default)]
struct NoIneffectiveCsp;

const INEFFECTIVE: &[&str] = &["frame-ancestors", "sandbox", "report-uri"];

/// The directive names of a policy, lowercased, in order.
fn directives(policy: &str) -> impl Iterator<Item = String> + '_ {
    let space = |c: char| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ');
    policy.split(';').filter_map(move |token| {
        let token = token.trim_start_matches(space);
        let name = token.split(space).next().filter(|n| !n.is_empty())?;
        Some(name.to_lowercase())
    })
}

fn attr_value(cx: &Ctx<'_, '_>, value: Option<NodeId>) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(value?) {
        Node::StringLit { value } => Some(decode_jsx_entities(ast.atom(value))),
        Node::JsxExprContainer { expr: Some(expr) } => match static_value_control_flow(cx, expr)? {
            Static::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

impl Rule for NoIneffectiveCsp {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::JsxOpening]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::JsxOpening { name, attrs, .. } = *ast.node(node) else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) || ident_name(ast, name) != Some("meta") {
            return;
        }
        let (mut header, mut content) = (Vec::new(), Vec::new());
        for &attr in ast.list(attrs) {
            let Node::JsxAttr { name, value } = *ast.node(attr) else {
                return;
            };
            if !matches!(ast.node(name), Node::Ident { .. }) {
                continue;
            }
            match ident_name(ast, name) {
                Some("httpEquiv" | "http-equiv") => header.push(value),
                Some("content") => content.push(value),
                _ => {}
            }
        }
        let ([header], [content]) = (&header[..], &content[..]) else {
            return;
        };
        if attr_value(cx, *header).is_none_or(|h| h.to_lowercase() != "content-security-policy") {
            return;
        }
        let (Some(policy), Some(value)) = (attr_value(cx, *content), *content) else {
            return;
        };
        let span = cx.span(value);
        let mut seen: Vec<String> = Vec::new();
        for d in directives(&policy) {
            if !INEFFECTIVE.contains(&d.as_str()) || seen.contains(&d) {
                continue;
            }
            cx.report(
                span,
                format!(
                    "The CSP directive `{d}` is ignored in `<meta>` elements. Deliver it through the `Content-Security-Policy` HTTP response header instead."
                ),
            )
            .id("no-ineffective-csp-directives");
            seen.push(d);
        }
    }
}
