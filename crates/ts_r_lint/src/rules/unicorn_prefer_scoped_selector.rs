//! `unicorn/prefer-scoped-selector`: `:scope` in combined selectors passed to an element's
//! `querySelector`/`querySelectorAll`.
//!
//! The call is `<x>.querySelector(selector)` (non-computed, `?.` allowed, exactly one
//! non-spread argument) whose receiver is not `document`, `window.document`/
//! `globalThis.document`, a non-element root (`.shadowRoot`, `.contentDocument`,
//! `.ownerDocument`, or a plain call of `attachShadow`, `createShadowRoot`,
//! `createDocumentFragment`, `createContextualFragment`, `extractContents`,
//! `cloneContents`, `parseFromString`, `getRootNode`, through TS wrappers) or a value that
//! can't be a node. The selector is a string literal or substitution-free template, not
//! blank.
//!
//! The selector is split into top-level branches (strings and comments dropped, escapes
//! replaced by an identifier character, commas inside `()`/`[]`/`{}` ignored). It is
//! reported on the method name when some branch has a top-level combinator (`>`, `+`, `~`,
//! or whitespace between two compound parts, with nested groups as one part) and no
//! `:scope` token (case-insensitive, not after `:`, not followed by a name character). A
//! single-branch selector gets a suggestion that prefixes `:scope `.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_util::is_value_not_dom_node;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-scoped-selector",
        "Prefer `:scope` when using element query selector methods.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<PreferScopedSelector>,
};

#[derive(Debug, Default)]
struct PreferScopedSelector;

const NON_ELEMENT_ROOT_METHODS: &[&str] = &[
    "attachShadow",
    "cloneContents",
    "createContextualFragment",
    "createDocumentFragment",
    "createShadowRoot",
    "extractContents",
    "getRootNode",
    "parseFromString",
];

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[quasi] => match *ast.node(quasi) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn is_ident(ast: &Ast, id: NodeId, names: &[&str]) -> bool {
    matches!(ast.node(id), Node::Ident { .. })
        && ident_name(ast, id).is_some_and(|n| names.contains(&n))
}

fn is_excluded_receiver(ast: &Ast, obj: NodeId) -> bool {
    let plain = skip_parens(ast, obj);
    if is_ident(ast, plain, &["document"]) {
        return true;
    }
    if let Node::Member { obj, prop, .. } = *ast.node(plain)
        && is_ident(ast, skip_parens(ast, obj), &["globalThis", "window"])
        && ident_name(ast, prop) == Some("document")
    {
        return true;
    }
    let object = skip_wrappers(ast, obj);
    match *ast.node(object) {
        Node::Member { prop, .. } => {
            matches!(
                ident_name(ast, prop),
                Some("shadowRoot" | "contentDocument" | "ownerDocument")
            ) && matches!(ast.node(prop), Node::Ident { .. })
        }
        Node::Call { .. } => method_call(ast, object)
            .is_some_and(|m| m.plain() && NON_ELEMENT_ROOT_METHODS.contains(&m.name)),
        _ => false,
    }
}

/// The selector's top-level branches, keeping only structural text.
fn branches(selector: &str) -> Vec<String> {
    let chars: Vec<char> = selector.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/');
            i = end.map_or(chars.len(), |e| e + 2);
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '\\' => {
                i += 1;
                current.push('a');
            }
            '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
        i += 1;
    }
    out.push(current);
    out
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn has_combinator(branch: &str) -> bool {
    let mut depth = 0i64;
    let mut stripped = Vec::new();
    for c in branch.chars() {
        match c {
            '(' | '[' | '{' => {
                if depth == 0 {
                    stripped.push('*');
                }
                depth += 1;
            }
            ')' | ']' | '}' => depth -= 1,
            _ if depth == 0 => stripped.push(c),
            _ => {}
        }
    }
    if stripped.iter().any(|c| matches!(c, '+' | '>' | '~')) {
        return true;
    }
    // Whitespace between two non-whitespace characters.
    let mut seen_text = false;
    let mut gap = false;
    for &c in &stripped {
        if is_space(c) {
            gap |= seen_text;
        } else {
            if gap {
                return true;
            }
            seen_text = true;
        }
    }
    false
}

fn has_scope(branch: &str) -> bool {
    let lower = branch.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices(":scope").any(|(i, _)| {
        (i == 0 || bytes[i - 1] != b':')
            && bytes
                .get(i + 6)
                .is_none_or(|&b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    })
}

impl Rule for PreferScopedSelector {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !matches!(call.name, "querySelector" | "querySelectorAll")
            || call.args.len() != 1
            || matches!(ast.node(call.args[0]), Node::Spread { .. })
            || is_excluded_receiver(ast, call.obj)
            || is_value_not_dom_node(ast, call.obj)
        {
            return;
        }
        let selector_node = skip_parens(ast, call.args[0]);
        let Some(selector) = static_string(ast, selector_node) else {
            return;
        };
        if selector.trim_matches(is_space).is_empty() {
            return;
        }
        let branches = branches(&selector);
        if branches.iter().all(|b| !has_combinator(b) || has_scope(b)) {
            return;
        }
        let start = cx.span(selector_node).start + 1;
        let r = cx
            .report(
                cx.span(call.prop),
                "Use `:scope` in element selector queries.",
            )
            .id("prefer-scoped-selector/error");
        if branches.len() == 1 {
            r.suggest(
                "prefer-scoped-selector/suggestion",
                "Prefix the selector with `:scope`.",
                Fix::insert(start, ":scope "),
            );
        }
    }
}
