//! `unicorn/no-invalid-character-comparison`: `==`/`===`/`!=`/`!==` between one character
//! read from a string (`s.charAt(i)`, `s.at(i)` with at most one argument and no optional
//! chain, or `s[k]` with a numeric literal or canonical index string key) and a static
//! string longer than one UTF-16 code unit. The receiver must be provably a string
//! (`isString`, typed or inferred).

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::static_string_value;
use super::unicorn_types::is_string;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-character-comparison",
        "Disallow comparing a single character from a string to a multi-character string.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoInvalidCharacterComparison>,
};

#[derive(Debug, Default)]
struct NoInvalidCharacterComparison;

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    static_string_value(ast, skip_parens(ast, id))
}

fn is_static_index(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    if matches!(ast.node(id), Node::NumberLit { .. }) {
        return true;
    }
    let Some(value) = static_string(ast, id) else {
        return false;
    };
    value
        .parse::<u64>()
        .is_ok_and(|n| n < (1 << 53) && n.to_string() == value)
}

fn is_single_character_access(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Some(call) = method_call(ast, id) {
        return matches!(call.name, "charAt" | "at")
            && call.plain()
            && call.args.len() <= 1
            && !call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            && is_string(cx, call.obj);
    }
    match *ast.node(id) {
        Node::Index {
            obj,
            index,
            optional: false,
        } => is_static_index(ast, index) && is_string(cx, obj),
        _ => false,
    }
}

impl Rule for NoInvalidCharacterComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let negated = match op {
            BinaryOp::Eq | BinaryOp::StrictEq => false,
            BinaryOp::NotEq | BinaryOp::StrictNotEq => true,
            _ => return,
        };
        for (access, string) in [(left, right), (right, left)] {
            let Some(value) = static_string(ast, string) else {
                continue;
            };
            if value.encode_utf16().count() > 1 && is_single_character_access(cx, access) {
                let result = if negated { "true" } else { "false" };
                cx.report(
                    cx.span(node),
                    format!(
                        "A single character can never equal the multi-character string `{value}`, so this comparison is always {result}."
                    ),
                )
                .id("no-invalid-character-comparison");
                return;
            }
        }
    }
}
