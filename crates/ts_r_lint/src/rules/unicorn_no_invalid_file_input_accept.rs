//! `unicorn/no-invalid-file-input-accept`: the `accept` attribute of a JSX `<input
//! type="file">` (the `type` a static string, any case) must list valid, normalised entries.
//!
//! The value is split at commas; each trimmed entry is a file extension (`.png`; a bare `png`
//! gets its dot), a wildcard MIME type (only `audio/*`, `image/*`, `video/*`) or a MIME type,
//! parsed as Node's `MIMEType` does and reduced to its lowercase `type/subtype` essence (with
//! a few common mistakes mapped, `image/jpg` -> `image/jpeg`). An empty entry or an invalid
//! one is reported with its reason; otherwise, when the lowercased, de-duplicated list joined
//! by `, ` differs from the value, the normalised value is suggested in the message, and
//! fixed in place for a string literal. A missing value is invalid; a value that is not a
//! static, side-effect-free string is reported as dynamic. HTML files are not linted here.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_util::decode_jsx_entities;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-file-input-accept",
        "Disallow invalid `accept` values on file inputs.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoInvalidFileInputAccept>,
};

#[derive(Debug, Default)]
struct NoInvalidFileInputAccept;

const EXTENSION_REASON: &str = "File extensions must start with `.` and cannot contain whitespace, commas, slashes, wildcards, or template markers.";
const WILDCARDS: &[&str] = &["audio/*", "image/*", "video/*"];
const MISTAKES: &[(&str, &str)] = &[
    ("application/x-rar-compressed", "application/vnd.rar"),
    ("application/x-zip-compressed", "application/zip"),
    ("image/jpg", "image/jpeg"),
    ("image/svg", "image/svg+xml"),
    ("image/x-icon", "image/vnd.microsoft.icon"),
];

/// Whitespace as JavaScript's `trim` sees it.
fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn bad_extension_char(s: &str) -> bool {
    s.chars()
        .any(|c| is_js_space(c) || matches!(c, ',' | '/' | '*' | '$' | '{' | '}'))
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
}

/// The `type/subtype` essence of a MIME type, or `None` when it does not parse. Parameters
/// after `;` never make it invalid.
fn mime_essence(s: &str) -> Option<String> {
    let is_http_space = |c: char| matches!(c, '\r' | '\n' | '\t' | ' ');
    let s = s.trim_start_matches(is_http_space);
    let (ty, rest) = s.split_once('/')?;
    let subtype = rest.split(';').next().unwrap_or("");
    let subtype = subtype.trim_end_matches(is_http_space);
    if ty.is_empty()
        || subtype.is_empty()
        || !ty.chars().all(is_token_char)
        || !subtype.chars().all(is_token_char)
    {
        return None;
    }
    Some(format!(
        "{}/{}",
        ty.to_ascii_lowercase(),
        subtype.to_ascii_lowercase()
    ))
}

/// The normal form of one trimmed entry, or why it is invalid.
fn normalize(token: &str) -> Result<String, &'static str> {
    if let Some(rest) = token.strip_prefix('.') {
        return if rest.is_empty() || bad_extension_char(rest) {
            Err(EXTENSION_REASON)
        } else {
            Ok(token.to_lowercase())
        };
    }
    if !token.contains('/') {
        return if bad_extension_char(token) {
            Err(EXTENSION_REASON)
        } else {
            Ok(format!(".{}", token.to_lowercase()))
        };
    }
    let lower = token.to_lowercase();
    if lower.contains('*') {
        return if WILDCARDS.contains(&lower.as_str()) {
            Ok(lower)
        } else {
            Err("Wildcard MIME types must be `audio/*`, `image/*`, or `video/*`.")
        };
    }
    let essence = mime_essence(token).ok_or("MIME types must use valid `type/subtype` syntax.")?;
    Ok(MISTAKES
        .iter()
        .find(|(from, _)| *from == essence)
        .map_or(essence, |(_, to)| (*to).to_owned()))
}

/// `Err(reason)` for an invalid value, `Ok(Some(normal))` when it should be rewritten.
fn check(value: &str) -> Result<Option<String>, &'static str> {
    let mut seen: Vec<String> = Vec::new();
    for raw in value.split(',') {
        let token = raw.trim_matches(is_js_space);
        if token.is_empty() {
            return Err("Empty entries are not allowed.");
        }
        let normal = normalize(token)?;
        if !seen.contains(&normal) {
            seen.push(normal);
        }
    }
    let joined = seen.join(", ");
    Ok((joined != value).then_some(joined))
}

/// The first attribute named exactly `name` (a plain identifier name).
fn attribute(ast: &Ast, attrs: &[NodeId], name: &str) -> Option<NodeId> {
    attrs.iter().copied().find(|&a| {
        matches!(*ast.node(a), Node::JsxAttr { name: n, .. }
            if matches!(ast.node(n), Node::Ident { .. }) && ident_name(ast, n) == Some(name))
    })
}

fn static_string(cx: &Ctx<'_, '_>, value: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(value) {
        Node::StringLit { value } => Some(decode_jsx_entities(ast.atom(value))),
        Node::JsxExprContainer { expr: Some(expr) } => match static_value_no_side_effects(cx, expr)
        {
            Some(Static::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

impl Rule for NoInvalidFileInputAccept {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::JsxOpening]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::JsxOpening { name, attrs, .. } = *ast.node(node) else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) || ident_name(ast, name) != Some("input") {
            return;
        }
        let attrs = ast.list(attrs);
        let is_file = attribute(ast, attrs, "type")
            .and_then(|a| match *ast.node(a) {
                Node::JsxAttr { value, .. } => value,
                _ => None,
            })
            .and_then(|v| static_string(cx, v))
            .is_some_and(|t| t.to_lowercase() == "file");
        if !is_file {
            return;
        }
        let Some(accept) = attribute(ast, attrs, "accept") else {
            return;
        };
        let Node::JsxAttr { value, .. } = *ast.node(accept) else {
            return;
        };
        let Some(value) = value else {
            cx.report_node(
                accept,
                "Invalid file input `accept` value: The `accept` attribute must have a value.",
            )
            .id("no-invalid-file-input-accept/invalid");
            return;
        };
        let Some(text) = static_string(cx, value) else {
            cx.report_node(value, "The file input `accept` value must be static.")
                .id("no-invalid-file-input-accept/static");
            return;
        };
        match check(&text) {
            Err(reason) => {
                cx.report_node(
                    value,
                    format!("Invalid file input `accept` value: {reason}"),
                )
                .id("no-invalid-file-input-accept/invalid");
            }
            Ok(Some(normal)) => {
                let span = cx.span(value);
                let fix = matches!(ast.node(value), Node::StringLit { .. })
                    .then(|| Fix::replace(Span::new(span.start + 1, span.end - 1), normal.clone()));
                cx.report(
                    span,
                    format!("Prefer `{normal}` as the file input `accept` value."),
                )
                .id("no-invalid-file-input-accept/normalize")
                .fix_opt(fix);
            }
            Ok(None) => {}
        }
    }
}
