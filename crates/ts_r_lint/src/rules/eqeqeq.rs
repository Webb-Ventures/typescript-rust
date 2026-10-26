//! `eqeqeq`: require `===` and `!==`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "eqeqeq",
        "Require the use of `===` and `!==`",
        NO_PRESET,
        Category::Pedantic,
    )
    .fixable()
    .suggestions(),
    create: create::<Eqeqeq>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NullMode {
    Always,
    Never,
    Ignore,
}

#[derive(Debug)]
struct Eqeqeq {
    smart: bool,
    null: NullMode,
}

impl FromOptions for Eqeqeq {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = opts.first().and_then(Value::as_str).unwrap_or("always");
        let null = match obj_opt(opts, 1).get("null").and_then(Value::as_str) {
            Some("never") => NullMode::Never,
            Some("ignore") => NullMode::Ignore,
            _ => NullMode::Always,
        };
        Ok(Eqeqeq {
            smart: mode == "smart",
            null: if mode == "allow-null" {
                NullMode::Ignore
            } else {
                null
            },
        })
    }
}

/// The `typeof` of a literal operand (static templates count as strings).
fn literal_type(ast: &Ast, id: NodeId) -> Option<&'static str> {
    Some(match ast.node(skip_parens(ast, id)) {
        Node::StringLit { .. } => "string",
        Node::TemplateLit { parts } if ast.list(*parts).len() <= 1 => "string",
        Node::NumberLit { .. } => "number",
        Node::BoolLit { .. } => "boolean",
        Node::BigIntLit { .. } => "bigint",
        Node::NullLit {} | Node::RegexLit { .. } => "object",
        _ => return None,
    })
}

fn is_typeof(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::Unary {
            op: UnaryOp::TypeOf,
            ..
        }
    )
}

fn is_null(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::NullLit {})
}

use ts_r_ast::Ast;

impl Rule for Eqeqeq {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let strict = match op {
            BinaryOp::Eq | BinaryOp::NotEq => false,
            BinaryOp::StrictEq | BinaryOp::StrictNotEq => true,
            _ => return,
        };
        let has_null = is_null(ast, left) || is_null(ast, right);
        let typeof_cmp = is_typeof(ast, left) || is_typeof(ast, right);
        let same_literals = matches!(
            (literal_type(ast, left), literal_type(ast, right)),
            (Some(a), Some(b)) if a == b
        );
        let want_strict = if has_null && !self.smart {
            match self.null {
                NullMode::Ignore => return,
                NullMode::Always => true,
                NullMode::Never => false,
            }
        } else {
            if self.smart && (typeof_cmp || same_literals || has_null) {
                return;
            }
            true
        };
        if strict == want_strict {
            return;
        }
        let actual = op.as_str();
        let expected = match op {
            BinaryOp::Eq => "===",
            BinaryOp::NotEq => "!==",
            BinaryOp::StrictEq => "==",
            _ => "!=",
        };
        let start = skip_trivia_forward(cx.src(), cx.span(left).end as usize) as u32;
        let span = Span::new(start, start + actual.len() as u32);
        let msg = format!("Expected '{expected}' and instead saw '{actual}'.");
        let fix = Fix::replace(span, expected);
        if typeof_cmp || same_literals {
            cx.report(span, msg).id("unexpected").fix(fix);
        } else {
            cx.report(span, msg).id("unexpected").suggest(
                "replaceOperator",
                format!("Use '{expected}' instead of '{actual}'."),
                fix,
            );
        }
    }
}
