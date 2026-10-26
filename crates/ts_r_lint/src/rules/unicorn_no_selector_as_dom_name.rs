//! `unicorn/no-selector-as-dom-name`: a `.class` or `#id` selector passed where a bare DOM
//! name belongs.
//!
//! The names are the arguments of `<x>.classList.add/remove(...)`, the first two of
//! `.replace(...)` (none when the first is a spread), the first of `.contains`/`.toggle`
//! (plain `classList` member, `?.` allowed), and the one argument of
//! `getElementById`/`getElementsByClassName`, on receivers that could be nodes (through TS
//! wrappers). A name (through TS wrappers) whose static value, or a template's first
//! chunk, starts with `.` or `#` is reported. The fix drops the prefix when it is the one
//! the method expects (`#` for `getElementById`, `.` otherwise), written literally, and the
//! rest is a simple name (`[#.]-?[A-Za-z_][A-Za-z0-9_-]*`), or the template is exactly
//! `` `.${x}` ``.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_util::is_value_not_dom_node;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-selector-as-dom-name",
        "Disallow selector syntax in DOM names.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoSelectorAsDomName>,
};

#[derive(Debug, Default)]
struct NoSelectorAsDomName;

fn cooked(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::TemplateElement {
            cooked,
            invalid_escape: false,
        } => Some(ast.atom(cooked).to_owned()),
        _ => None,
    }
}

/// The name's static value, or its template's first chunk.
fn dom_name_value(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => ast.list(parts).first().and_then(|&q| cooked(ast, q)),
        _ => None,
    }
}

fn is_simple_selector(s: &str) -> bool {
    let rest = &s[1..];
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    let mut chars = rest.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn fix(cx: &Ctx<'_, '_>, node: NodeId, value: &str, prefix: char) -> Option<Fix> {
    let ast = cx.ast();
    let start = cx.span(node).start + 1;
    if !value.starts_with(prefix)
        || cx.src().as_bytes().get(start as usize) != Some(&(prefix as u8))
    {
        return None;
    }
    if let Node::TemplateLit { parts } = *ast.node(node)
        && ast.list(parts).len() > 1
    {
        let parts = ast.list(parts);
        let one_expression = parts.len() == 3
            && cooked(ast, parts[0]).as_deref() == Some(&prefix.to_string())
            && cooked(ast, parts[2]).as_deref() == Some("");
        return one_expression.then(|| Fix::delete(Span::new(start, start + 1)));
    }
    is_simple_selector(value).then(|| Fix::delete(Span::new(start, start + 1)))
}

impl Rule for NoSelectorAsDomName {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(m) = method_call(ast, node) else {
            return;
        };
        let names: (&[NodeId], char) = match m.name {
            "add" | "remove" | "contains" | "toggle" | "replace" => {
                let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, m.obj)) else {
                    return;
                };
                if !matches!(ast.node(prop), Node::Ident { .. })
                    || ident_name(ast, prop) != Some("classList")
                    || is_value_not_dom_node(ast, skip_wrappers(ast, obj))
                {
                    return;
                }
                let n = match m.name {
                    "add" | "remove" => m.args.len(),
                    "replace" => {
                        if m.args
                            .first()
                            .is_some_and(|&a| matches!(ast.node(a), Node::Spread { .. }))
                        {
                            return;
                        }
                        2
                    }
                    _ => 1,
                };
                (&m.args[..n.min(m.args.len())], '.')
            }
            "getElementById" | "getElementsByClassName" => {
                if m.args.len() != 1
                    || matches!(ast.node(m.args[0]), Node::Spread { .. })
                    || is_value_not_dom_node(ast, skip_wrappers(ast, m.obj))
                {
                    return;
                }
                (m.args, if m.name == "getElementById" { '#' } else { '.' })
            }
            _ => return,
        };
        let (args, prefix) = names;
        let args = args.to_vec();
        for arg in args {
            if matches!(ast.node(arg), Node::Spread { .. }) {
                continue;
            }
            let name = skip_wrappers(ast, arg);
            let Some(value) = dom_name_value(ast, name) else {
                continue;
            };
            if !value.starts_with(['.', '#']) {
                continue;
            }
            let fix = fix(cx, name, &value, prefix);
            cx.report(cx.span(name), "Do not use selector syntax in DOM names.")
                .id("no-selector-as-dom-name")
                .fix_opt(fix);
        }
    }
}
