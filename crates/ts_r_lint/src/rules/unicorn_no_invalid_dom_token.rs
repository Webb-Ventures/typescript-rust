//! `unicorn/no-invalid-dom-token`: an empty token, or one with ASCII whitespace (tab, LF,
//! FF, CR, space), passed to `add`/`remove` (every argument), `toggle` (the first) or
//! `replace` (the first two) of a DOM token list: a `classList`, `relList`, `sandbox`,
//! `part`, `sizes`, `blocking`, `htmlFor` or `controlsList` member (static names, through
//! optional chains and TS wrappers) of something that may be a DOM node.
//!
//! A token's value is a string or expression-free template, else its control-flow static
//! value; a template with expressions has whitespace when a literal piece does. A spread
//! ends the check of fixed-position methods. For `add`/`remove`, a literal with whitespace
//! gets a suggestion splitting it into separate quoted tokens (unless that would drop a
//! comment). Type information (a `DOMTokenList`-typed receiver) is not used.

use super::prelude::*;
use super::unicorn_conditions::unwrap_ts_and_parens;
use super::unicorn_static::static_value;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util::{escape_string, is_value_not_dom_node, static_string};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-dom-token",
        "Disallow invalid DOM tokens.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoInvalidDomToken>,
};

#[derive(Debug, Default)]
struct NoInvalidDomToken;

const TOKEN_LIST_PROPERTIES: &[&str] = &[
    "classList",
    "relList",
    "sandbox",
    "part",
    "sizes",
    "blocking",
    "htmlFor",
    "controlsList",
];

fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

fn has_template_whitespace(ast: &Ast, id: NodeId) -> bool {
    let Node::TemplateLit { parts } = *ast.node(id) else {
        return false;
    };
    ast.list(parts).iter().any(|&p| {
        matches!(*ast.node(p), Node::TemplateElement { cooked, invalid_escape: false }
            if ast.atom(cooked).contains(is_ascii_whitespace))
    })
}

/// `getStaticPropertyName` without a scope: a plain name, or a computed key whose value is
/// known without reading any variable.
fn property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => {
            let mut stack = vec![index];
            while let Some(n) = stack.pop() {
                if matches!(ast.node(n), Node::Ident { .. }) {
                    return None;
                }
                ast.for_each_child(n, |c| stack.push(c));
            }
            static_value(cx, index).and_then(|v| v.to_js_string())
        }
        _ => None,
    }
}

fn split_suggestion(cx: &Ctx<'_, '_>, arg: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let value = static_string(ast, unwrap_ts_and_parens(ast, arg))?;
    let tokens: Vec<&str> = value
        .split(is_ascii_whitespace)
        .filter(|t| !t.is_empty())
        .collect();
    let range = paren_span(cx, arg);
    if tokens.is_empty() || would_remove_comments(cx, range, &[]) {
        return None;
    }
    let text = tokens
        .iter()
        .map(|t| escape_string(t, None))
        .collect::<Vec<_>>()
        .join(", ");
    Some(Fix::replace(range, text))
}

impl Rule for NoInvalidDomToken {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee = unwrap_ts_and_parens(ast, callee);
        let max = match property_name(cx, callee).as_deref() {
            Some("add" | "remove") => usize::MAX,
            Some("toggle") => 1,
            Some("replace") => 2,
            _ => return,
        };
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
            return;
        };
        let receiver = unwrap_ts_and_parens(ast, obj);
        let is_token_list = property_name(cx, receiver)
            .is_some_and(|p| TOKEN_LIST_PROPERTIES.contains(&p.as_str()))
            && match *ast.node(receiver) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => {
                    !is_value_not_dom_node(ast, unwrap_ts_and_parens(ast, obj))
                }
                _ => false,
            };
        if !is_token_list {
            return;
        }
        for &arg in ast.list(args).iter().take(max) {
            if matches!(ast.node(arg), Node::Spread { .. }) {
                if max != usize::MAX {
                    break;
                }
                continue;
            }
            let token = unwrap_ts_and_parens(ast, arg);
            let value = match static_string(ast, token) {
                Some(s) => Some(s.to_owned()),
                None => match static_value_control_flow(cx, token) {
                    Some(Static::Str(s)) => Some(s),
                    _ => None,
                },
            };
            let has_whitespace = match &value {
                Some(s) => s.contains(is_ascii_whitespace),
                None => has_template_whitespace(ast, token),
            };
            let empty = value.as_deref() == Some("");
            if !empty && !has_whitespace {
                continue;
            }
            let reported = skip_parens(ast, arg);
            let suggestion = (max == usize::MAX && has_whitespace)
                .then(|| split_suggestion(cx, arg))
                .flatten();
            let (id, msg) = if empty {
                (
                    "no-invalid-dom-token/empty",
                    "DOM tokens must not be empty.",
                )
            } else {
                (
                    "no-invalid-dom-token/whitespace",
                    "DOM tokens must not contain ASCII whitespace.",
                )
            };
            let r = cx.report_node(reported, msg).id(id);
            if let Some(fix) = suggestion {
                r.suggest(
                    "no-invalid-dom-token/suggestion",
                    "Replace with separate tokens.",
                    fix,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-invalid-dom-token": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn malformed_and_many_arguments() {
        let args = vec!["'a b'"; 2_000].join(", ");
        assert_eq!(lint(&format!("el.classList.add({args});")), 2_000);
        for src in [
            "el.classList.add(",
            "el.classList[",
            "el.classList.add('a b'",
            "x.add(``",
        ] {
            let _ = lint(src);
        }
        assert_eq!(lint("el.classList.toggle('', '');"), 1);
    }
}
