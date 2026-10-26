//! `unicorn/prefer-query-selector`: `.querySelector()` / `.querySelectorAll()` over
//! `getElementById`, `getElementsByClassName`, `getElementsByTagName`, `getElementsByName`.
//!
//! - A non-optional `obj.method(arg)` call with exactly one non-spread argument, `obj` not a
//!   value that can't be a DOM node, is reported on the method name. `allowWithVariables`
//!   skips `ById` / `ByClassName` calls whose argument (through TS wrappers) isn't `null`, a
//!   string, a binary expression, an expression-less template, or a template with a
//!   non-blank text part.
//! - A `querySelectorAll` call followed by `[0]`, `.at(0)` or `.item(0)` (not written to, no
//!   comment between the call and the end of the access) becomes `querySelector` and the
//!   access goes.
//! - Fixable when the argument is `null` or a non-blank string / expression-less template,
//!   and its value is a selector the new method reads the same: a simple CSS identifier for
//!   `ById` and `ByTagName` (`*` too), simple identifiers for every class word, and for
//!   `ByName` no quote, backslash or line break in the value or raw text.
//! - The text becomes `#id`, `.a.b`, the tag, or `[name="v"]` (the quote opposite to the
//!   string's own; `'` inside a template), prefixed by `:scope ` when the receiver isn't
//!   `document` / `window.document` / `globalThis.document`. `null` and unscoped tag names
//!   keep their argument.

use super::prelude::*;
use super::unicorn_util::parenthesized_span;
use super::unicorn_util_h5::{
    is_left_hand_side, is_number_literal, is_value_not_dom_node, method_call_of,
    parent_through_parens, remove_method_call, unwrap_ts, would_remove_comments,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-query-selector",
        "Prefer `.querySelector()` and `.querySelectorAll()` over older DOM query methods.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferQuerySelector>,
};

#[derive(Debug, Default)]
struct PreferQuerySelector {
    allow_with_variables: bool,
}

impl FromOptions for PreferQuerySelector {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferQuerySelector {
            allow_with_variables: bool_field(o, "allowWithVariables", false),
        })
    }
}

const METHODS: &[&str] = &[
    "getElementById",
    "getElementsByClassName",
    "getElementsByTagName",
    "getElementsByName",
];

/// A selector argument read statically: its value and the raw text between the quotes.
struct StaticArg<'a> {
    value: &'a str,
    raw: &'a str,
    template: bool,
}

fn static_arg<'a>(cx: &Ctx<'a, '_>, id: NodeId) -> Option<StaticArg<'a>> {
    let ast = cx.ast();
    let text = cx.text(id);
    let raw = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
    match *ast.node(id) {
        Node::StringLit { value } => Some(StaticArg {
            value: ast.atom(value),
            raw,
            template: false,
        }),
        Node::TemplateLit { parts } => match *ast.list(parts) {
            [only] => match *ast.node(only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(StaticArg {
                    value: ast.atom(cooked),
                    raw,
                    template: true,
                }),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `isNonLiteralArgument` for `allowWithVariables`.
fn is_non_literal_argument(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::NullLit {} | Node::StringLit { .. } | Node::Binary { .. } => false,
        Node::TemplateLit { parts } => {
            let parts = ast.list(parts);
            parts.len() != 1
                && !parts.iter().any(|&p| {
                    matches!(*ast.node(p), Node::TemplateElement { cooked, .. }
                        if !ast.atom(cooked).trim().is_empty())
                })
        }
        _ => true,
    }
}

fn is_simple_css_identifier(value: &str) -> bool {
    let rest = value.strip_prefix('-').unwrap_or(value);
    let mut chars = rest.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn css_words(value: &str) -> impl Iterator<Item = &str> {
    value.split(char::is_whitespace).filter(|w| !w.is_empty())
}

fn needs_escaping(value: &str, raw: &str) -> bool {
    let line_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    value
        .chars()
        .any(|c| line_break(c) || matches!(c, '"' | '\'' | '\\'))
        || raw.contains('\\')
        || raw.chars().any(line_break)
}

fn can_be_fixed(
    ast: &ts_r_ast::Ast,
    arg: Option<&StaticArg<'_>>,
    id: NodeId,
    method: &str,
) -> bool {
    let Some(arg) = arg else {
        return matches!(ast.node(id), Node::NullLit {});
    };
    let selector_ok = match method {
        "getElementById" => is_simple_css_identifier(arg.value),
        "getElementsByTagName" => arg.value == "*" || is_simple_css_identifier(arg.value),
        "getElementsByClassName" => {
            css_words(arg.value).next().is_some()
                && css_words(arg.value).all(is_simple_css_identifier)
        }
        _ => !needs_escaping(arg.value, arg.raw),
    };
    selector_ok && !arg.value.trim().is_empty()
}

fn is_document_object(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => ident_name(ast, id) == Some("document"),
        Node::Member {
            obj,
            prop,
            optional: _,
        } => {
            matches!(ident_name(ast, obj), Some("globalThis" | "window"))
                && matches!(ast.node(prop), Node::Ident { .. })
                && ident_name(ast, prop) == Some("document")
        }
        _ => false,
    }
}

/// The `[0]`, `.at(0)` or `.item(0)` right after the call, as a removal.
fn first_element_access(cx: &Ctx<'_, '_>, call: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let (parent, child) = parent_through_parens(cx, call)?;
    let call_end = parenthesized_span(cx, call).end;
    let clean = |access: NodeId| {
        !is_left_hand_side(cx, access)
            && !would_remove_comments(cx, Span::new(call_end, cx.span(access).end), &[])
    };
    match *ast.node(parent) {
        Node::Index {
            obj,
            index,
            optional: false,
        } if obj == child && is_number_literal(cx, index, 0.0) => {
            clean(parent).then(|| Fix::delete(Span::new(call_end, cx.span(parent).end)))
        }
        Node::Member { obj, .. } if obj == child => {
            let (outer, _) = parent_through_parens(cx, parent)?;
            let Some((_, _, &[arg])) =
                method_call_of(ast, outer, &["at", "item"], 1, Some(1), Some(false))
            else {
                return None;
            };
            let Node::Call { callee, .. } = *ast.node(outer) else {
                return None;
            };
            if skip_parens(ast, callee) != parent
                || !is_number_literal(cx, arg, 0.0)
                || !clean(outer)
            {
                return None;
            }
            remove_method_call(cx, outer)
        }
        _ => None,
    }
}

impl Rule for PreferQuerySelector {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((obj, prop, &[arg])) = method_call_of(ast, node, METHODS, 1, Some(1), Some(false))
        else {
            return;
        };
        if is_value_not_dom_node(ast, obj) {
            return;
        }
        let method = ident_name(ast, prop).unwrap_or_default();
        let inner = unwrap_ts(ast, arg);
        if self.allow_with_variables
            && matches!(method, "getElementById" | "getElementsByClassName")
            && is_non_literal_argument(ast, inner)
        {
            return;
        }
        let access = if method == "getElementById" {
            None
        } else {
            first_element_access(cx, node)
        };
        let preferred = if method == "getElementById" || access.is_some() {
            "querySelector"
        } else {
            "querySelectorAll"
        };
        let parsed = static_arg(cx, inner);
        let fix = can_be_fixed(ast, parsed.as_ref(), inner, method).then(|| {
            let mut fix = Fix::replace(cx.span(prop), preferred);
            if let Some(arg) = &parsed {
                let scope = !is_document_object(ast, obj);
                if scope || method != "getElementsByTagName" {
                    let quote = cx.text(inner).chars().next().unwrap_or('"');
                    let name_quote = if !arg.template && quote == '\'' {
                        '"'
                    } else {
                        '\''
                    };
                    let selector = match method {
                        "getElementById" => format!("#{}", arg.value),
                        "getElementsByClassName" => {
                            css_words(arg.value).map(|w| format!(".{w}")).collect()
                        }
                        "getElementsByName" => {
                            format!("[name={name_quote}{}{name_quote}]", arg.value)
                        }
                        _ => arg.value.to_owned(),
                    };
                    let scope = if scope { ":scope " } else { "" };
                    let close = cx.text(inner).chars().last().unwrap_or(quote);
                    fix = fix.and(cx.span(inner), format!("{quote}{scope}{selector}{close}"));
                }
            }
            for edit in access.iter().flat_map(|a| a.edits.iter()) {
                fix = fix.and(edit.span, edit.text.clone());
            }
            fix
        });
        cx.report_node(prop, format!("Prefer `.{preferred}()` over `.{method}()`."))
            .id("prefer-query-selector")
            .fix_opt(fix);
    }
}
