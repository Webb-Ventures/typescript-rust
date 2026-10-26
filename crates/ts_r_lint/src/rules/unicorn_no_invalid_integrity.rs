//! `unicorn/no-invalid-integrity`: malformed subresource integrity metadata on native JSX
//! `<script>`/`<link>` elements.
//!
//! The last `integrity` prop (not followed by a spread) with a string value (JSX entities
//! decoded) or a statically known string (`getStaticValueForControlFlow`) is split on HTML
//! ASCII whitespace. Each entry whose algorithm (the text before the first `-` or `?`) is
//! `sha256`/`sha384`/`sha512`/`ed25519` must be `<algorithm>-<base64>[=|==][?<options>]`:
//! base64 from either alphabet (`+/` or `-_`) with 43, 64, 86 or 43 characters, and options
//! of printable ASCII only. Every malformed entry is reported on the value; nonempty metadata
//! without a recognized algorithm is reported once.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_util_h2::decode_jsx_entities;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-integrity",
        "Disallow invalid subresource integrity metadata.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoInvalidIntegrity>,
};

#[derive(Debug, Default)]
struct NoInvalidIntegrity;

const ALGORITHMS: &[(&str, usize)] = &[
    ("sha256", 43),
    ("sha384", 64),
    ("sha512", 86),
    ("ed25519", 43),
];

fn problem(token: &str, algorithm: &str, length: usize) -> Option<String> {
    let Some(value) = token[algorithm.len()..].strip_prefix('-') else {
        return Some("The algorithm must be followed by `-` and a base64 value.".into());
    };
    let (digest, options) = match value.split_once('?') {
        Some((d, o)) => (d, Some(o)),
        None => (value, None),
    };
    if options.is_some_and(|o| o.chars().any(|c| !('!'..='~').contains(&c))) {
        return Some("Options must contain only printable ASCII characters.".into());
    }
    let body = digest.trim_end_matches('=');
    let padding = digest.len() - body.len();
    if body.is_empty()
        || padding > 2
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-' | b'/'))
    {
        return Some(
            "The value must contain base64 characters followed by at most two `=` padding characters."
                .into(),
        );
    }
    (body.len() != length).then(|| {
        format!(
            "The `{algorithm}` value must contain {length} base64 characters, excluding padding."
        )
    })
}

fn report(cx: &mut Ctx<'_, '_>, span: Span, value: &str) {
    let tokens: Vec<&str> = value
        .split(['\t', '\n', '\u{c}', '\r', ' '])
        .filter(|t| !t.is_empty())
        .collect();
    let mut recognized = false;
    for token in &tokens {
        let end = token.find(['-', '?']).unwrap_or(token.len());
        let Some(&(algorithm, length)) = ALGORITHMS.iter().find(|(a, _)| *a == &token[..end])
        else {
            continue;
        };
        recognized = true;
        if let Some(reason) = problem(token, algorithm, length) {
            cx.report(
                span,
                format!("Invalid `integrity` metadata `{token}`: {reason}"),
            )
            .id("invalid");
        }
    }
    if !tokens.is_empty() && !recognized {
        cx.report(
            span,
            "The `integrity` value must contain at least one recognized algorithm: `sha256`, `sha384`, `sha512`, or `ed25519`.",
        )
        .id("unrecognized");
    }
}

impl Rule for NoInvalidIntegrity {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::JsxOpening]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::JsxOpening { name, attrs, .. } = *ast.node(node) else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. })
            || !matches!(ident_name(ast, name), Some("script" | "link"))
        {
            return;
        }
        let attrs = ast.list(attrs);
        let Some(index) = attrs.iter().rposition(|&a| {
            matches!(*ast.node(a), Node::JsxAttr { name, .. }
                if matches!(ast.node(name), Node::Ident { .. }) && ident_name(ast, name) == Some("integrity"))
        }) else {
            return;
        };
        if attrs[index + 1..]
            .iter()
            .any(|&a| matches!(ast.node(a), Node::JsxSpreadAttr { .. }))
        {
            return;
        }
        let Node::JsxAttr {
            value: Some(value), ..
        } = *ast.node(attrs[index])
        else {
            return;
        };
        let text = match *ast.node(value) {
            Node::StringLit { value } => decode_jsx_entities(ast.atom(value)),
            Node::JsxExprContainer { expr: Some(expr) } => {
                match static_value_control_flow(cx, expr) {
                    Some(Static::Str(s)) => s.to_string(),
                    _ => return,
                }
            }
            _ => return,
        };
        report(cx, cx.span(value), &text);
    }
}
