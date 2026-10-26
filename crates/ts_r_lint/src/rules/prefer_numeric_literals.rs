//! `prefer-numeric-literals`: `parseInt("1F7", 16)` -> `0x1F7`.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-numeric-literals",
        "Disallow `parseInt()` and `Number.parseInt()` in favor of binary, octal, and hexadecimal literals",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferNumericLiterals>,
};

#[derive(Debug, Default)]
struct PreferNumericLiterals;

/// The string an argument spells out: a string literal or a template without
/// substitutions.
fn literal_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => {
            let mut s = String::new();
            for &p in ast.list(parts) {
                match *ast.node(p) {
                    Node::TemplateElement {
                        cooked,
                        invalid_escape: false,
                    } => s.push_str(ast.atom(cooked)),
                    _ => return None,
                }
            }
            Some(s)
        }
        _ => None,
    }
}

/// `parseInt` or `Number.parseInt` naming the globals.
fn is_parse_int(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let ast = cx.ast();
    let callee = skip_parens(ast, callee);
    if ident_name(ast, callee) == Some("parseInt") {
        return cx.is_global_ref(callee, "parseInt");
    }
    let Some((obj, name)) = member_parts(ast, cx.src(), callee) else {
        return false;
    };
    let obj = skip_parens(ast, obj);
    name == "parseInt" && ident_name(ast, obj) == Some("Number") && cx.is_global_ref(obj, "Number")
}

impl Rule for PreferNumericLiterals {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let &[str_node, radix_node] = ast.list(args) else {
            return;
        };
        let str_node = skip_parens(ast, str_node);
        let radix_node = skip_parens(ast, radix_node);
        let Some(text) = literal_string(ast, str_node) else {
            return;
        };
        let Node::NumberLit { value } = *ast.node(radix_node) else {
            return;
        };
        let (system, prefix, radix) = match value.get() {
            2.0 => ("binary", "0b", 2),
            8.0 => ("octal", "0o", 8),
            16.0 => ("hexadecimal", "0x", 16),
            _ => return,
        };
        if !is_parse_int(cx, callee) {
            return;
        }
        let src = cx.src();
        let span = cx.span(node);
        let callee_span = ast.span(skip_parens(ast, callee));
        let name = &src[callee_span.start as usize..callee_span.end as usize];
        let msg = format!("Use {system} literals instead of {name}().");
        // The literal must mean the same number `parseInt` computes: every character a
        // digit of the radix (no separators, signs or blanks).
        let exact = !text.is_empty() && text.chars().all(|c| c.is_digit(radix));
        let fix = (exact && ast.comments_in(span).is_empty()).then(|| {
            let mut out = space_before(src, span.start, format!("{prefix}{text}"));
            if src[span.end as usize..]
                .chars()
                .next()
                .is_some_and(is_ident_char)
            {
                out.push(' ');
            }
            Fix::replace(span, out)
        });
        cx.report(span, msg).id("useLiteral").fix_opt(fix);
    }
}
