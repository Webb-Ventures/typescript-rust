//! `unicorn/no-invalid-url-protocol-comparison`: `URL#protocol` is lowercase and ends with
//! `:`, so comparing it with `"https"` or `"HTTPS:"` never matches.
//!
//! A protocol read is `<url>.protocol` or `<url>["protocol"]` (through TS wrappers and `?.`)
//! on a known native URL (see `unicorn_url_type`). It is compared through `==`/`===`/`!=`/
//! `!==` (either side), a `switch` discriminant (each `case`), or the first argument of
//! `[...].includes/indexOf/lastIndexOf(x[, from])` or `new Set([...]).has(x)` on an array
//! literal (each element). A static string or substitution-free template (through TS
//! wrappers) that looks like a scheme (`[A-Za-z][A-Za-z0-9+.-]*` with an optional `:`) but
//! isn't already in normal form is reported and fixed to the lowercase name with `:`, unless,
//! in a `switch`, another case already normalizes to the same protocol.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_url_type::is_url;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-url-protocol-comparison",
        "Disallow invalid protocol strings in comparisons with `URL#protocol`.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoInvalidUrlProtocolComparison>,
};

#[derive(Debug, Default)]
struct NoInvalidUrlProtocolComparison;

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

fn normalized(ast: &Ast, id: NodeId) -> Option<String> {
    let value = static_string(ast, skip_wrappers(ast, id))?;
    let name = value.strip_suffix(':').unwrap_or(&value);
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some(format!("{}:", name.to_ascii_lowercase()))
}

fn is_protocol(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    let (obj, named) = match *ast.node(id) {
        Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop) == Some("protocol")),
        Node::Index { obj, index, .. } => (
            obj,
            static_string(ast, skip_wrappers(ast, index)).as_deref() == Some("protocol"),
        ),
        _ => return false,
    };
    named && is_url(cx, obj)
}

/// `(string node, normalized protocol)` when `id` should be written differently.
fn problem(ast: &Ast, id: NodeId) -> Option<(NodeId, String)> {
    let node = skip_wrappers(ast, id);
    let protocol = normalized(ast, node)?;
    (static_string(ast, node).as_deref() != Some(&protocol)).then_some((node, protocol))
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, protocol: &str, fixable: bool) {
    let span = cx.span(node);
    let fix = fixable.then(|| Fix::replace(Span::new(span.start + 1, span.end - 1), protocol));
    cx.report(
        span,
        format!("`URL#protocol` values are lowercase and end with `:`. Compare with `{protocol}`."),
    )
    .id("no-invalid-url-protocol-comparison")
    .fix_opt(fix);
}

fn no_spread(ast: &Ast, args: &[NodeId]) -> bool {
    !args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

impl Rule for NoInvalidUrlProtocolComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Switch, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Binary { left, op, right } => {
                if !matches!(
                    op,
                    BinaryOp::Eq | BinaryOp::StrictEq | BinaryOp::NotEq | BinaryOp::StrictNotEq
                ) {
                    return;
                }
                let other = if is_protocol(cx, left) {
                    right
                } else if is_protocol(cx, right) {
                    left
                } else {
                    return;
                };
                if let Some((n, p)) = problem(ast, other) {
                    report(cx, n, &p, true);
                }
            }
            Node::Switch {
                disc: discriminant,
                cases,
            } => {
                if !is_protocol(cx, discriminant) {
                    return;
                }
                let tests: Vec<Option<NodeId>> = ast
                    .list(cases)
                    .iter()
                    .map(|&c| match *ast.node(c) {
                        Node::SwitchCase { test, .. } => test,
                        _ => None,
                    })
                    .collect();
                let protocols: Vec<Option<String>> = tests
                    .iter()
                    .map(|t| t.and_then(|t| normalized(ast, t)))
                    .collect();
                for (i, test) in tests.iter().enumerate() {
                    let Some((n, p)) = test.and_then(|t| problem(ast, t)) else {
                        continue;
                    };
                    let duplicate = protocols
                        .iter()
                        .enumerate()
                        .any(|(j, q)| j != i && q.as_deref() == Some(p.as_str()));
                    report(cx, n, &p, !duplicate);
                }
            }
            Node::Call { .. } => {
                let Some(m) = method_call(ast, node) else {
                    return;
                };
                let array = match m.name {
                    "includes" | "indexOf" | "lastIndexOf"
                        if (1..=2).contains(&m.args.len()) && no_spread(ast, m.args) =>
                    {
                        skip_wrappers(ast, m.obj)
                    }
                    "has" if m.args.len() == 1 && no_spread(ast, m.args) => {
                        let receiver = skip_wrappers(ast, m.obj);
                        let Node::New { callee, args, .. } = *ast.node(receiver) else {
                            return;
                        };
                        let &[arg] = ast.list(args) else {
                            return;
                        };
                        if !matches!(ast.node(callee), Node::Ident { .. })
                            || ident_name(ast, callee) != Some("Set")
                            || matches!(ast.node(arg), Node::Spread { .. })
                        {
                            return;
                        }
                        skip_wrappers(ast, arg)
                    }
                    _ => return,
                };
                let Node::ArrayLit { elements } = *ast.node(array) else {
                    return;
                };
                if !is_protocol(cx, m.args[0]) {
                    return;
                }
                for &e in ast.list(elements) {
                    if let Some((n, p)) = problem(ast, e) {
                        report(cx, n, &p, true);
                    }
                }
            }
            _ => {}
        }
    }
}
