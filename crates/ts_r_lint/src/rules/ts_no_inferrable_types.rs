//! `@typescript-eslint/no-inferrable-types`: no explicit type on a variable, parameter
//! default or class property initialized with a value of that obvious type
//! (`const a: number = 5`).
//!
//! Inferrable pairs: `number` with a number literal, `Infinity`, `NaN` or `Number(..)`
//! (optionally after unary `+`/`-`); `bigint` with any literal or `BigInt(..)` (optionally
//! after `-`); `boolean` with a boolean literal, `!x` or `Boolean(..)`; `string` with a
//! string literal, a template literal or `String(..)`; `symbol` with `Symbol(..)`; `null`
//! with `null`; `undefined` with `undefined` or `void x`; `RegExp` with a regex literal,
//! `RegExp(..)` or `new RegExp(..)`. Parameters are checked on functions with bodies;
//! `readonly` and optional properties are skipped. The fix removes the `: T` annotation (and
//! the `?`/`!` before it on an optional parameter or a definite property).

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-inferrable-types",
        "Disallow explicit type declarations for variables or parameters initialized to a number, string, or boolean",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoInferrableTypes>,
};

#[derive(Debug, Default)]
struct NoInferrableTypes {
    ignore_parameters: bool,
    ignore_properties: bool,
}

impl FromOptions for NoInferrableTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let empty = serde_json::Map::new();
        let o = opts.first().and_then(Value::as_object).unwrap_or(&empty);
        Ok(NoInferrableTypes {
            ignore_parameters: bool_field(o, "ignoreParameters", false),
            ignore_properties: bool_field(o, "ignoreProperties", false),
        })
    }
}

fn is_call(ast: &Ast, id: NodeId, name: &str) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Call { callee, .. }
        if matches!(ast.node(callee), Node::Ident { .. }) && ident_name(ast, callee) == Some(name))
}

/// The operand of a unary `op` (parentheses skipped), or `id` itself.
fn unwrap_unary(ast: &Ast, id: NodeId, ops: &[UnaryOp]) -> NodeId {
    match *ast.node(id) {
        Node::Unary { op, arg } if ops.contains(&op) => skip_parens(ast, arg),
        _ => id,
    }
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::BigIntLit { .. }
            | Node::RegexLit { .. }
    )
}

/// The type's name in the message when `init` makes it inferrable.
fn inferrable(ast: &Ast, ty: NodeId, init: NodeId) -> Option<&'static str> {
    let init = skip_parens(ast, init);
    let mut ty = ty;
    for _ in 0..1000 {
        match *ast.node(ty) {
            Node::ParenType { ty: t } => ty = t,
            _ => break,
        }
    }
    match *ast.node(ty) {
        Node::KeywordType { kind } => {
            let ok = match kind {
                KeywordTypeKind::BigInt => {
                    let u = unwrap_unary(ast, init, &[UnaryOp::Minus]);
                    is_call(ast, u, "BigInt") || is_literal(ast, u)
                }
                KeywordTypeKind::Boolean => {
                    matches!(
                        ast.node(init),
                        Node::Unary {
                            op: UnaryOp::Not,
                            ..
                        } | Node::BoolLit { .. }
                    ) || is_call(ast, init, "Boolean")
                }
                KeywordTypeKind::Number => {
                    let u = unwrap_unary(ast, init, &[UnaryOp::Plus, UnaryOp::Minus]);
                    matches!(ast.node(u), Node::NumberLit { .. })
                        || matches!(ident_name(ast, u), Some("Infinity" | "NaN"))
                            && matches!(ast.node(u), Node::Ident { .. })
                        || is_call(ast, u, "Number")
                }
                KeywordTypeKind::Null => matches!(ast.node(init), Node::NullLit {}),
                KeywordTypeKind::String => {
                    matches!(
                        ast.node(init),
                        Node::StringLit { .. } | Node::TemplateLit { .. }
                    ) || is_call(ast, init, "String")
                }
                KeywordTypeKind::Symbol => is_call(ast, init, "Symbol"),
                KeywordTypeKind::Undefined => {
                    matches!(
                        ast.node(init),
                        Node::Unary {
                            op: UnaryOp::Void,
                            ..
                        }
                    ) || matches!(ast.node(init), Node::Ident { .. })
                        && ident_name(ast, init) == Some("undefined")
                }
                _ => false,
            };
            ok.then_some(match kind {
                KeywordTypeKind::BigInt => "bigint",
                KeywordTypeKind::Boolean => "boolean",
                KeywordTypeKind::Number => "number",
                KeywordTypeKind::Null => "null",
                KeywordTypeKind::String => "string",
                KeywordTypeKind::Symbol => "symbol",
                _ => "undefined",
            })
        }
        Node::TypeRef { name, .. }
            if matches!(ast.node(name), Node::Ident { .. })
                && ident_name(ast, name) == Some("RegExp") =>
        {
            let ok = matches!(ast.node(init), Node::RegexLit { .. })
                || is_call(ast, init, "RegExp")
                || matches!(*ast.node(init), Node::New { callee, .. }
                    if matches!(ast.node(callee), Node::Ident { .. })
                        && ident_name(ast, callee) == Some("RegExp"));
            ok.then_some("RegExp")
        }
        _ => None,
    }
}

impl NoInferrableTypes {
    /// Reports `node` (shown as `span`) when its annotation `ty` is inferrable from
    /// `init`; `marker` also drops the `?`/`!` before the annotation.
    fn check(&self, cx: &mut Ctx<'_, '_>, span: Span, ty: NodeId, init: NodeId, marker: bool) {
        let ast = cx.ast();
        let src = cx.src();
        let Some(name) = inferrable(ast, ty, init) else {
            return;
        };
        // The annotation as ESTree has it: from its `:` to the type's end.
        let after_colon = skip_trivia_backward(ast, src, ast.span(ty).start as usize);
        let colon = after_colon.saturating_sub(1);
        let mut fix = Fix::delete(Span::new(colon as u32, ast.span(ty).end));
        if marker {
            let end = skip_trivia_backward(ast, src, colon);
            if end > 0 && matches!(src.as_bytes()[end - 1], b'?' | b'!') {
                fix = fix.and(Span::new(end as u32 - 1, end as u32), "");
            }
        }
        cx.report(
            span,
            format!(
                "Type {name} trivially inferred from a {name} literal, remove type annotation."
            ),
        )
        .id("noInferrableType")
        .fix(fix);
    }
}

impl Rule for NoInferrableTypes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDeclarator,
            NodeKind::PropertyDef,
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::VarDeclarator {
                type_ann: Some(ty),
                init: Some(init),
                ..
            } => self.check(cx, cx.span(node), ty, init, false),
            Node::PropertyDef {
                modifiers,
                optional,
                definite,
                type_ann: Some(ty),
                value: Some(init),
                ..
            } => {
                if self.ignore_properties
                    || optional
                    || modifiers.intersects(Modifiers::READONLY | Modifiers::ABSTRACT)
                {
                    return;
                }
                self.check(cx, cx.span(node), ty, init, definite);
            }
            Node::FunctionDecl {
                params,
                body: Some(_),
                ..
            }
            | Node::FunctionExpr {
                params,
                body: Some(_),
                ..
            }
            | Node::Arrow { params, .. } => {
                if self.ignore_parameters {
                    return;
                }
                for &p in ast.list(params) {
                    let Node::Param {
                        pattern,
                        optional,
                        type_ann: Some(ty),
                        init: Some(init),
                        ..
                    } = *ast.node(p)
                    else {
                        continue;
                    };
                    let span = Span::new(ast.span(pattern).start, ast.span(init).end);
                    self.check(cx, span, ty, init, optional);
                }
            }
            _ => {}
        }
    }
}
