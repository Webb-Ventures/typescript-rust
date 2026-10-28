//! `unicorn/prefer-optional-catch-binding`: drop a `catch` binding nothing reads.
//!
//! A binding is kept when any name it declares is referenced, when it has a default value
//! (ESLint counts the initialising write as a reference), or when a computed key in a
//! destructuring pattern would run code. The report is on the binding (with its type
//! annotation); the fix removes `(`, the binding, `)` and the spaces after `)`, keeping
//! comments between the parentheses, and is withheld when a comment sits inside the binding.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-optional-catch-binding",
        "Prefer omitting the `catch` binding parameter.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferOptionalCatchBinding>,
};

#[derive(Debug, Default)]
struct PreferOptionalCatchBinding;

impl Rule for PreferOptionalCatchBinding {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::CatchClause]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::CatchClause {
            param: Some(param),
            type_ann,
            body,
        } = *cx.node(node)
        else {
            return;
        };
        if has_default_or_computed_key(ast, param) {
            return;
        }
        let mut names = Vec::new();
        binding_idents(ast, param, &mut names);
        let sem = cx.semantic();
        let used = names.iter().any(|&n| {
            sem.symbol_of_ident(n)
                .is_some_and(|s| !sem.symbol(s).refs.is_empty())
        });
        if used {
            return;
        }
        let start = cx.span(param).start;
        let end = type_ann.map_or(cx.span(param).end, |t| cx.span(t).end);
        let binding = Span::new(start, end);
        let (id, message) = match ident_name(ast, param) {
            Some(name) => (
                "with-name",
                format!("Remove unused catch binding `{name}`."),
            ),
            None => ("without-name", "Remove unused catch binding.".to_owned()),
        };
        let fix = removal(cx, binding, cx.span(body).start);
        cx.report(binding, message).id(id).fix_opt(fix);
    }
}

/// Removes `(`, the binding and `)`, plus the spaces between `)` and the body.
fn removal(cx: &Ctx<'_, '_>, binding: Span, body_start: u32) -> Option<Fix> {
    let ast = cx.ast();
    if !ast.comments_in(binding).is_empty() {
        return None;
    }
    let src = cx.src();
    let open = skip_trivia_backward(ast, src, binding.start as usize).checked_sub(1)?;
    let close = find_char(src, binding.end, b')')?;
    if src.as_bytes().get(open) != Some(&b'(') || close >= body_start {
        return None;
    }
    let open = open as u32;
    let after = close + 1;
    let gap = &src[after as usize..body_start as usize];
    let spaces = (gap.len() - gap.trim_start().len()) as u32;
    Some(
        Fix::delete(Span::new(open, open + 1))
            .and(binding, "")
            .and(Span::new(close, after + spaces), ""),
    )
}

/// A default value anywhere in the pattern, or a computed key outside a default.
fn has_default_or_computed_key(ast: &Ast, pattern: NodeId) -> bool {
    match *ast.node(pattern) {
        Node::ObjectPattern { props } => ast.list(props).iter().any(|&p| match *ast.node(p) {
            Node::BindingProperty { key, value, init } => {
                init.is_some()
                    || matches!(ast.node(key), Node::ComputedKey { .. })
                    || value.is_some_and(|v| has_default_or_computed_key(ast, v))
            }
            _ => has_default_or_computed_key(ast, p),
        }),
        Node::ArrayPattern { elements } => ast
            .list(elements)
            .iter()
            .any(|&e| has_default_or_computed_key(ast, e)),
        Node::AssignPattern { .. } => true,
        Node::Rest { arg } => has_default_or_computed_key(ast, arg),
        _ => false,
    }
}
