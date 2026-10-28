//! `unicorn/no-shorthand-property-overrides`, the JSX part (CSS files are another
//! language): in `style={{...}}` on an intrinsic element (a lowercase tag name), whose
//! object (through TS wrappers) holds only plain `key: value` properties, a shorthand
//! property that resets a longhand set earlier in the object. Keys keep the position of
//! their first occurrence and the value of their last, as object literals do. Only static
//! values count: a finite number (optionally signed) or a non-blank string without
//! `!important`. camelCase names become CSS names (`msX` gets its leading `-`); a vendor
//! prefix (`-webkit-`) must match on both sides. Each overridden longhand is reported at
//! the shorthand's key.
//!
//! The shorthand table lists each CSS shorthand's components and the extra properties it
//! resets; nested shorthands expand to theirs.

use super::prelude::*;
use super::unicorn_static::{Static, static_value};
use super::unicorn_text::static_string_value;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-shorthand-property-overrides",
        "Disallow shorthand properties that override related longhand properties.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoShorthandPropertyOverrides>,
};

#[derive(Debug, Default)]
struct NoShorthandPropertyOverrides;

/// `(shorthand, components, other properties it resets)`, from the CSS specifications.
const SHORTHANDS: &[(&str, &str, &str)] = &[
    (
        "animation",
        "animation-duration animation-timing-function animation-delay animation-iteration-count \
         animation-direction animation-fill-mode animation-play-state animation-name \
         animation-timeline",
        "animation-range",
    ),
    (
        "animation-range",
        "animation-range-start animation-range-end",
        "",
    ),
    (
        "background",
        "background-image background-position background-size background-repeat \
         background-attachment background-origin background-clip background-color",
        "",
    ),
    (
        "border",
        "border-width border-style border-color",
        "border-image",
    ),
    (
        "border-block",
        "border-block-width border-block-style border-block-color",
        "",
    ),
    (
        "border-block-color",
        "border-block-start-color border-block-end-color",
        "",
    ),
    (
        "border-block-end",
        "border-block-end-width border-block-end-style border-block-end-color",
        "",
    ),
    (
        "border-block-start",
        "border-block-start-width border-block-start-style border-block-start-color",
        "",
    ),
    (
        "border-block-style",
        "border-block-start-style border-block-end-style",
        "",
    ),
    (
        "border-block-width",
        "border-block-start-width border-block-end-width",
        "",
    ),
    (
        "border-bottom",
        "border-bottom-width border-bottom-style border-bottom-color",
        "",
    ),
    (
        "border-color",
        "border-top-color border-right-color border-bottom-color border-left-color",
        "",
    ),
    (
        "border-image",
        "border-image-source border-image-slice border-image-width border-image-outset \
         border-image-repeat",
        "",
    ),
    (
        "border-inline",
        "border-inline-width border-inline-style border-inline-color",
        "",
    ),
    (
        "border-inline-color",
        "border-inline-start-color border-inline-end-color",
        "",
    ),
    (
        "border-inline-end",
        "border-inline-end-width border-inline-end-style border-inline-end-color",
        "",
    ),
    (
        "border-inline-start",
        "border-inline-start-width border-inline-start-style border-inline-start-color",
        "",
    ),
    (
        "border-inline-style",
        "border-inline-start-style border-inline-end-style",
        "",
    ),
    (
        "border-inline-width",
        "border-inline-start-width border-inline-end-width",
        "",
    ),
    (
        "border-left",
        "border-left-width border-left-style border-left-color",
        "",
    ),
    (
        "border-radius",
        "border-top-left-radius border-top-right-radius border-bottom-right-radius \
         border-bottom-left-radius",
        "",
    ),
    (
        "border-right",
        "border-right-width border-right-style border-right-color",
        "",
    ),
    (
        "border-style",
        "border-top-style border-right-style border-bottom-style border-left-style",
        "",
    ),
    (
        "border-top",
        "border-top-width border-top-style border-top-color",
        "",
    ),
    (
        "border-width",
        "border-top-width border-right-width border-bottom-width border-left-width",
        "",
    ),
    (
        "column-rule",
        "column-rule-width column-rule-style column-rule-color",
        "",
    ),
    ("columns", "column-width column-count column-height", ""),
    ("flex", "flex-grow flex-shrink flex-basis", ""),
    ("flex-flow", "flex-direction flex-wrap", ""),
    (
        "font",
        "font-style font-variant font-weight font-stretch font-size line-height font-family",
        "font-feature-settings font-kerning font-language-override font-optical-sizing \
         font-size-adjust font-variation-settings",
    ),
    (
        "font-synthesis",
        "font-synthesis-weight font-synthesis-style font-synthesis-small-caps \
         font-synthesis-position",
        "",
    ),
    (
        "font-variant",
        "font-variant-ligatures font-variant-position font-variant-caps font-variant-numeric \
         font-variant-alternates font-variant-east-asian font-variant-emoji",
        "",
    ),
    ("gap", "row-gap column-gap", ""),
    (
        "grid",
        "grid-template-rows grid-template-columns grid-template-areas grid-auto-rows \
         grid-auto-columns grid-auto-flow",
        "",
    ),
    (
        "grid-area",
        "grid-row-start grid-column-start grid-row-end grid-column-end",
        "",
    ),
    ("grid-column", "grid-column-start grid-column-end", ""),
    ("grid-gap", "grid-row-gap grid-column-gap", ""),
    ("grid-row", "grid-row-start grid-row-end", ""),
    (
        "grid-template",
        "grid-template-rows grid-template-columns grid-template-areas",
        "",
    ),
    ("inset", "top right bottom left", ""),
    ("inset-block", "inset-block-start inset-block-end", ""),
    ("inset-inline", "inset-inline-start inset-inline-end", ""),
    (
        "list-style",
        "list-style-type list-style-position list-style-image",
        "",
    ),
    (
        "margin",
        "margin-top margin-right margin-bottom margin-left",
        "",
    ),
    ("margin-block", "margin-block-start margin-block-end", ""),
    ("margin-inline", "margin-inline-start margin-inline-end", ""),
    (
        "mask",
        "mask-image mask-position mask-size mask-repeat mask-origin mask-clip mask-composite \
         mask-mode",
        "mask-border",
    ),
    (
        "mask-border",
        "mask-border-source mask-border-slice mask-border-width mask-border-outset \
         mask-border-repeat mask-border-mode",
        "",
    ),
    ("outline", "outline-width outline-style outline-color", ""),
    ("overflow", "overflow-x overflow-y", ""),
    (
        "overscroll-behavior",
        "overscroll-behavior-x overscroll-behavior-y",
        "",
    ),
    (
        "padding",
        "padding-top padding-right padding-bottom padding-left",
        "",
    ),
    ("padding-block", "padding-block-start padding-block-end", ""),
    (
        "padding-inline",
        "padding-inline-start padding-inline-end",
        "",
    ),
    ("place-content", "align-content justify-content", ""),
    ("place-items", "align-items justify-items", ""),
    ("place-self", "align-self justify-self", ""),
    (
        "scroll-margin",
        "scroll-margin-top scroll-margin-right scroll-margin-bottom scroll-margin-left",
        "",
    ),
    (
        "scroll-margin-block",
        "scroll-margin-block-start scroll-margin-block-end",
        "",
    ),
    (
        "scroll-margin-inline",
        "scroll-margin-inline-start scroll-margin-inline-end",
        "",
    ),
    (
        "scroll-padding",
        "scroll-padding-top scroll-padding-right scroll-padding-bottom scroll-padding-left",
        "",
    ),
    (
        "scroll-padding-block",
        "scroll-padding-block-start scroll-padding-block-end",
        "",
    ),
    (
        "scroll-padding-inline",
        "scroll-padding-inline-start scroll-padding-inline-end",
        "",
    ),
    (
        "text-decoration",
        "text-decoration-line text-decoration-style text-decoration-color \
         text-decoration-thickness",
        "",
    ),
    (
        "text-emphasis",
        "text-emphasis-style text-emphasis-color",
        "",
    ),
    (
        "transition",
        "transition-property transition-duration transition-timing-function transition-delay \
         transition-behavior",
        "",
    ),
];

/// Properties a shorthand also resets that the table above leaves out (they don't take part
/// in nesting), appended after the expanded ones.
const EXTRA: &[(&str, &str)] = &[
    ("animation", "animation-trigger"),
    (
        "background",
        "background-position-x background-position-y background-repeat-x background-repeat-y",
    ),
    (
        "background-position",
        "background-position-x background-position-y",
    ),
    (
        "background-repeat",
        "background-repeat-x background-repeat-y",
    ),
    ("caret", "caret-color caret-animation caret-shape"),
    ("position-try", "position-try-order position-try-fallbacks"),
    ("text-box", "text-box-trim text-box-edge"),
    (
        "text-decoration-skip",
        "text-decoration-skip-self text-decoration-skip-box text-decoration-skip-spaces \
         text-decoration-skip-ink",
    ),
    ("text-wrap", "text-wrap-mode text-wrap-style"),
    (
        "white-space",
        "white-space-collapse text-wrap-mode white-space-trim",
    ),
];

/// Every property `shorthand` sets, nested shorthands expanded, in table order.
fn affected(shorthand: &str) -> Option<Vec<&'static str>> {
    fn add(name: &str, out: &mut Vec<&'static str>, depth: u32) {
        let Some(&(_, components, resets)) = SHORTHANDS.iter().find(|e| e.0 == name) else {
            return;
        };
        if depth > 8 {
            return;
        }
        for p in components
            .split_whitespace()
            .chain(resets.split_whitespace())
        {
            if !out.contains(&p) {
                out.push(p);
            }
            add(p, out, depth + 1);
        }
    }
    let extra = EXTRA.iter().find(|e| e.0 == shorthand).map(|e| e.1);
    if extra.is_none() && !SHORTHANDS.iter().any(|e| e.0 == shorthand) {
        return None;
    }
    let mut out = Vec::new();
    add(shorthand, &mut out, 0);
    for p in extra.into_iter().flat_map(str::split_whitespace) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Some(out)
}

/// `-webkit-` in `-webkit-transition`.
fn vendor_prefix(property: &str) -> &str {
    let Some(rest) = property.strip_prefix('-') else {
        return "";
    };
    let word = rest
        .bytes()
        .take_while(|&b| b == b'_' || b.is_ascii_alphanumeric())
        .count();
    if word > 0 && rest.as_bytes().get(word) == Some(&b'-') {
        &property[..word + 2]
    } else {
        ""
    }
}

fn has_important(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.match_indices('!').any(|(i, _)| {
        let rest = lower[i + 1..].trim_start_matches(|c: char| c.is_whitespace());
        rest.strip_prefix("important").is_some_and(|after| {
            !after
                .bytes()
                .next()
                .is_some_and(|b| b == b'_' || b.is_ascii_alphanumeric())
        })
    })
}

fn is_static_style_value(cx: &Ctx<'_, '_>, ast: &Ast, value: NodeId) -> bool {
    let value = skip_wrappers(ast, value);
    let numeric = match *ast.node(value) {
        Node::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            arg,
        } => skip_wrappers(ast, arg),
        _ => value,
    };
    if let Node::NumberLit { .. } = ast.node(numeric) {
        return matches!(static_value(cx, numeric), Some(Static::Num(n)) if n.is_finite());
    }
    static_string_value(ast, value).is_some_and(|s| {
        !s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            .is_empty()
            && !has_important(&s)
    })
}

impl Rule for NoShorthandPropertyOverrides {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::JsxAttr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::JsxAttr {
            name,
            value: Some(value),
        } = *cx.node(node)
        else {
            return;
        };
        if ident_name(ast, name) != Some("style") {
            return;
        }
        let Some(Node::JsxOpening { name: tag, .. }) = cx.parent(node).map(|p| ast.node(p)) else {
            return;
        };
        if !ident_name(ast, *tag).is_some_and(|t| t.starts_with(|c: char| c.is_ascii_lowercase())) {
            return;
        }
        let Node::JsxExprContainer { expr: Some(expr) } = *ast.node(value) else {
            return;
        };
        let Node::ObjectLit { props } = *ast.node(skip_wrappers(ast, expr)) else {
            return;
        };
        // (name, last key, last value) in first-occurrence order.
        let mut entries: Vec<(String, NodeId, Option<NodeId>)> = Vec::new();
        for &p in ast.list(props) {
            let (key, value) = match *ast.node(p) {
                Node::Property { key, value, .. } => (key, Some(value)),
                Node::ShorthandProp { name, .. } => (name, None),
                _ => return,
            };
            let name = match *ast.node(key) {
                Node::Ident { name } | Node::StringLit { value: name } => ast.atom(name).to_owned(),
                Node::ComputedKey { .. } => return,
                // A numeric key is not a style name.
                _ => continue,
            };
            match entries.iter_mut().find(|e| e.0 == name) {
                Some(e) => {
                    e.1 = key;
                    e.2 = value;
                }
                None => entries.push((name, key, value)),
            }
        }
        let mut declared: Vec<(String, &str)> = Vec::new();
        let mut reports = Vec::new();
        for (name, key, value) in &entries {
            if !value.is_some_and(|v| is_static_style_value(cx, ast, v)) {
                continue;
            }
            let mut property = String::with_capacity(name.len() + 4);
            for c in name.chars() {
                if c.is_ascii_uppercase() {
                    property.push('-');
                    property.push(c.to_ascii_lowercase());
                } else {
                    property.push(c);
                }
            }
            if property.starts_with("ms-") {
                property.insert(0, '-');
            }
            let prefix = vendor_prefix(&property).to_owned();
            let longhands = affected(&property[prefix.len()..]);
            match declared.iter_mut().find(|d| d.0 == property) {
                Some(d) => d.1 = name,
                None => declared.push((property.clone(), name)),
            }
            for longhand in longhands.into_iter().flatten() {
                let full = format!("{prefix}{longhand}");
                if let Some(&(_, original)) = declared.iter().find(|d| d.0 == full) {
                    reports.push((
                        *key,
                        format!(
                            "The shorthand property `{name}` overrides the previously declared `{original}` property."
                        ),
                    ));
                }
            }
        }
        for (key, message) in reports {
            cx.report_node(key, message)
                .id("no-shorthand-property-overrides");
        }
    }
}
