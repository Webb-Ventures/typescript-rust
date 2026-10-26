//! `unicorn/prefer-class-fields`: a constructor whose first statement assigns a literal (or
//! a substitution-free template) to `this.x` / `this.#x` can declare a field instead.
//! Not fixed when a parameter property shares the name, a non-static method or accessor (or,
//! for a private name, a static member) has it, a comment is inside the statement, or the
//! existing field is duplicated, `declare`, `abstract` or definite. The fix removes the
//! statement (its whole line when alone there) and gives the existing field the value (a
//! suggestion when it already has one) or adds `x = value;` before the class's `}` with the
//! constructor's indentation. A private name is written with its `#`.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_text::static_string_value;
use ts_r_ast::{Ast, MethodKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-class-fields",
        "Prefer class field declarations over `this` assignments in constructors.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferClassFields>,
};

#[derive(Debug, Default)]
struct PreferClassFields;

const MESSAGE: &str =
    "Prefer class field declaration over `this` assignment in constructor for static values.";

/// `key` names the property `name` (private when `private`): the same kind of name, or a
/// string literal for a public one.
fn same_key(ast: &Ast, key: NodeId, name: &str, private: bool) -> bool {
    match *ast.node(key) {
        Node::Ident { .. } => !private && ident_name(ast, key) == Some(name),
        Node::PrivateName { name: n } => private && ast.atom(n) == name,
        Node::StringLit { value } => !private && ast.atom(value) == name,
        _ => false,
    }
}

impl Rule for PreferClassFields {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, class: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *cx.node(class)
        else {
            return;
        };
        let members = ast.list(members);
        let Some((ctor, ctor_value)) = members.iter().find_map(|&m| match *ast.node(m) {
            Node::MethodDef {
                kind: MethodKind::Constructor,
                modifiers,
                key,
                value,
                ..
            } if !modifiers.contains(Modifiers::STATIC)
                && !matches!(ast.node(key), Node::ComputedKey { .. }) =>
            {
                Some((m, value))
            }
            _ => None,
        }) else {
            return;
        };
        let Node::FunctionExpr {
            params,
            body: Some(body),
            ..
        } = *ast.node(ctor_value)
        else {
            return;
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let Some(&stmt) = ast
            .list(stmts)
            .iter()
            .find(|&&s| !matches!(ast.node(s), Node::Empty {}))
        else {
            return;
        };
        let Node::ExprStmt { expr } = *ast.node(stmt) else {
            return;
        };
        let Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } = *ast.node(skip_parens(ast, expr))
        else {
            return;
        };
        let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, left)) else {
            return;
        };
        if !matches!(ast.node(skip_parens(ast, obj)), Node::This {}) {
            return;
        }
        let (name, private) = match *ast.node(prop) {
            Node::Ident { .. } => (ident_name(ast, prop).unwrap_or(""), false),
            Node::PrivateName { name } => (ast.atom(name), true),
            _ => return,
        };
        let right = skip_parens(ast, right);
        let literal = matches!(
            ast.node(right),
            Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::NullLit {}
                | Node::RegexLit { .. }
        );
        if !literal && static_string_value(ast, right).is_none() {
            return;
        }
        let value_text = cx.text(right);
        let param_property = ast.list(params).iter().any(|&p| {
            matches!(*ast.node(p), Node::Param { modifiers, pattern, .. }
                if !modifiers.is_empty() && same_key(ast, pattern, name, private))
        });
        if param_property {
            return;
        }
        let conflicting = members.iter().any(|&m| match *ast.node(m) {
            Node::MethodDef { modifiers, key, .. } => {
                same_key(ast, key, name, private)
                    && (!modifiers.contains(Modifiers::STATIC) || private)
            }
            Node::PropertyDef { modifiers, key, .. } => {
                same_key(ast, key, name, private)
                    && private
                    && modifiers.contains(Modifiers::STATIC)
            }
            _ => false,
        });
        let existing: Vec<NodeId> = members
            .iter()
            .copied()
            .filter(|&m| {
                matches!(*ast.node(m), Node::PropertyDef { modifiers, key, .. }
                    if !modifiers.contains(Modifiers::STATIC) && same_key(ast, key, name, private))
            })
            .collect();
        let span = cx.span(stmt);
        let no_init = |m: NodeId| {
            matches!(*ast.node(m), Node::PropertyDef { modifiers, definite, .. }
                if definite || modifiers.intersects(Modifiers::DECLARE | Modifiers::ABSTRACT))
        };
        if existing.len() > 1
            || existing.first().is_some_and(|&m| no_init(m))
            || conflicting
            || !ast.comments_in(span).is_empty()
        {
            cx.report(span, MESSAGE).id("prefer-class-fields/error");
            return;
        }
        // Remove the statement, or its whole line when nothing else is on it.
        let line_start = src[..span.start as usize].rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[span.end as usize..]
            .find('\n')
            .map_or(src.len(), |i| span.end as usize + i);
        let mut fix = if src[line_start..line_end].trim() == span.text(src) {
            let next = if line_end < src.len() {
                line_end + 1
            } else {
                line_end
            };
            Fix::replace(Span::new(line_start as u32, next as u32), "")
        } else {
            Fix::replace(span, "")
        };
        let mut suggestion = false;
        match existing.first() {
            Some(&field) => {
                let Node::PropertyDef { value, .. } = *ast.node(field) else {
                    return;
                };
                let fspan = cx.span(field);
                if let Some(v) = value {
                    fix = fix.and(cx.span(skip_parens(ast, v)), value_text);
                    suggestion = true;
                } else if fspan.text(src).ends_with(';') {
                    fix = fix.and(Span::at(fspan.end - 1), format!(" = {value_text}"));
                } else if src[fspan.end as usize..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with(';')
                {
                    fix = fix.and(Span::at(fspan.end), format!(" = {value_text}"));
                } else {
                    fix = fix.and(Span::at(fspan.end), format!(" = {value_text};"));
                }
            }
            None => {
                let close = cx.span(class).end - 1;
                let cstart = cx.span(ctor).start as usize;
                let before = &src[src[..cstart].rfind('\n').map_or(0, |i| i + 1)..cstart];
                let indent = &before[before.trim_end().len()..];
                let lb = linebreak(src);
                let field_name = if private { cx.text(prop) } else { name };
                let mut text = format!("{indent}{field_name} = {value_text};{lb}");
                if close == 0 || src.as_bytes()[close as usize - 1] != b'\n' {
                    text = format!("{lb}{text}");
                }
                if let Some(&last) = members.last()
                    && matches!(ast.node(last), Node::PropertyDef { .. })
                    && !cx.span(last).text(src).ends_with(';')
                {
                    text = format!(";{text}");
                }
                fix = fix.and(Span::at(close), text);
            }
        }
        let report = cx.report(span, MESSAGE).id("prefer-class-fields/error");
        if suggestion {
            report.suggest(
                "prefer-class-fields/suggestion",
                "Encountered same-named class field declaration and `this` assignment in constructor. Replace the class field declaration with the value from `this` assignment.",
                fix,
            );
        } else {
            report.fix(fix);
        }
    }
}
