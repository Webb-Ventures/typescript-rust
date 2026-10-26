//! `@typescript-eslint/consistent-type-assertions`: one assertion style (`as`, `<T>` or none),
//! and optionally no assertions on object and array literals.
//!
//! With `assertionStyle: "as"`, `<T>x` is reported and fixed to `x as T`; with
//! `"angle-bracket"`, `x as T` is reported (no fix); with `"never"` every assertion except
//! `as const`/`<const>` is reported. In the matching style, `objectLiteralTypeAssertions` and
//! `arrayLiteralTypeAssertions` (`allow`, `allow-as-parameter`, `never`) report `{..} as T`
//! and `[..] as T` unless `T` is `any`, `unknown` or `const`. `allow-as-parameter` lets the
//! assertion through as a call or `new` argument or callee, after `throw`, as a default value,
//! in a JSX expression or in a tagged template. The suggestions turn it into a declared type
//! (`const x: T = ..`, only on an untyped declarator) or `.. satisfies T`.
//!
//! The fix parenthesizes from a two-sided precedence check: the expression is wrapped unless
//! it binds tighter than `as` (a shift or tighter operator, unary, call, member or primary),
//! and the new `x as T` is wrapped (unless already in parentheses) when its parent binds at
//! least as tightly as `as`: a relational or tighter binary, a unary or update, a call or
//! `new`, a member access, another assertion, an array element or an arrow body.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-type-assertions",
        "Enforce consistent usage of type assertions",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<ConsistentTypeAssertions>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Style {
    #[default]
    As,
    AngleBracket,
    Never,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Literals {
    #[default]
    Allow,
    AllowAsParameter,
    Never,
}

#[derive(Debug, Default)]
struct ConsistentTypeAssertions {
    style: Style,
    objects: Literals,
    arrays: Literals,
}

fn literals(o: &serde_json::Map<String, Value>, k: &str) -> Literals {
    match str_field(o, k) {
        Some("never") => Literals::Never,
        Some("allow-as-parameter") => Literals::AllowAsParameter,
        _ => Literals::Allow,
    }
}

impl FromOptions for ConsistentTypeAssertions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(ConsistentTypeAssertions {
            style: match str_field(o, "assertionStyle") {
                Some("angle-bracket") => Style::AngleBracket,
                Some("never") => Style::Never,
                _ => Style::As,
            },
            objects: literals(o, "objectLiteralTypeAssertions"),
            arrays: literals(o, "arrayLiteralTypeAssertions"),
        })
    }
}

fn is_const(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Const
        }
    )
}

/// Whether `id` (parentheses skipped) binds tighter than `as`, so `id as T` needs no
/// parentheses around it.
fn binds_tighter_than_as(ast: &Ast, id: NodeId) -> bool {
    use ts_r_ast::BinaryOp::*;
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary { op, .. } => {
            matches!(op, Shl | Shr | UShr | Add | Sub | Mul | Div | Mod | Exp)
        }
        Node::TypeAssertion { .. }
        | Node::NonNull { .. }
        | Node::Unary { .. }
        | Node::Await { .. }
        | Node::Update { .. }
        | Node::Call { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::MetaProperty { .. }
        | Node::This {}
        | Node::Super {}
        | Node::Ident { .. }
        | Node::PrivateName { .. }
        | Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::StringLit { .. }
        | Node::RegexLit { .. }
        | Node::ArrayLit { .. }
        | Node::ObjectLit { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::TemplateLit { .. }
        | Node::JsxElement { .. }
        | Node::JsxFragment { .. } => true,
        _ => false,
    }
}

/// Whether an `x as T` written as the child `child` of `parent` needs parentheses.
fn parent_binds_as_tightly(ast: &Ast, parent: NodeId, child: NodeId) -> bool {
    use ts_r_ast::BinaryOp::*;
    match *ast.node(parent) {
        Node::Binary { op, .. } => matches!(
            op,
            Lt | LtEq
                | Gt
                | GtEq
                | In
                | InstanceOf
                | Shl
                | Shr
                | UShr
                | Add
                | Sub
                | Mul
                | Div
                | Mod
                | Exp
        ),
        Node::TypeAssertion { .. }
        | Node::NonNull { .. }
        | Node::Unary { .. }
        | Node::Await { .. }
        | Node::Update { .. }
        | Node::Call { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::As { .. }
        | Node::Satisfies { .. }
        | Node::ArrayLit { .. } => true,
        Node::Arrow { body, .. } => body == child,
        _ => false,
    }
}

impl ConsistentTypeAssertions {
    /// The parent of `id` with parentheses skipped, and the child it was reached from.
    fn up(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<(NodeId, NodeId)> {
        let ast = cx.ast();
        for _ in 0..10_000 {
            let p = cx.parent(id)?;
            if matches!(ast.node(p), Node::Paren { .. }) {
                id = p;
                continue;
            }
            return Some((p, id));
        }
        None
    }

    fn as_parameter(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let ast = cx.ast();
        let Some((p, child)) = Self::up(cx, node) else {
            return false;
        };
        match *ast.node(p) {
            Node::New { .. }
            | Node::Call { .. }
            | Node::Throw { .. }
            | Node::JsxExprContainer { .. } => true,
            Node::Param { init, .. } => init == Some(child),
            Node::AssignPattern { right, .. } => right == child,
            Node::BindingProperty { init, .. } | Node::ShorthandProp { init, .. } => {
                init == Some(child)
            }
            Node::Assign { right, .. } => {
                right == child && is_assignment_target(ast, cx.file.parents, p)
            }
            Node::TemplateLit { .. } => cx
                .parent(p)
                .is_some_and(|g| matches!(ast.node(g), Node::TaggedTemplate { .. })),
            _ => false,
        }
    }

    fn check_literal(&self, node: NodeId, expr: NodeId, ty: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (option, message, id, annotation, satisfies, shape) = match ast.node(expr) {
            Node::ObjectLit { .. } => (
                self.objects,
                "Always prefer const x: T = { ... }.",
                "unexpectedObjectTypeAssertion",
                "replaceObjectTypeAssertionWithAnnotation",
                "replaceObjectTypeAssertionWithSatisfies",
                "{ ... }",
            ),
            Node::ArrayLit { .. } => (
                self.arrays,
                "Always prefer const x: T[] = [ ... ].",
                "unexpectedArrayTypeAssertion",
                "replaceArrayTypeAssertionWithAnnotation",
                "replaceArrayTypeAssertionWithSatisfies",
                "[ ... ]",
            ),
            _ => return,
        };
        if option == Literals::Allow
            || (option == Literals::AllowAsParameter && Self::as_parameter(cx, node))
        {
            return;
        }
        let allowed = match ast.node(ty) {
            Node::KeywordType { kind } => {
                matches!(
                    kind,
                    KeywordTypeKind::Any | KeywordTypeKind::Unknown | KeywordTypeKind::Const
                )
            }
            _ => false,
        };
        if allowed {
            return;
        }
        let src = cx.src();
        let span = cx.span(node);
        let cast = cx.span(ty).text(src);
        let expr_text = cx.span(expr_with_parens(cx, node)).text(src);
        let mut suggestions = Vec::new();
        if let Some((p, child)) = Self::up(cx, node)
            && let Node::VarDeclarator {
                name,
                type_ann: None,
                init: Some(init),
                ..
            } = *ast.node(p)
            && init == child
        {
            suggestions.push((
                annotation,
                format!("Use const x: {cast} = {shape} instead."),
                Fix::insert(cx.span(name).end, format!(": {cast}")).and(span, expr_text),
            ));
        }
        suggestions.push((
            satisfies,
            format!("Use const x = {shape} satisfies {cast} instead."),
            Fix::replace(span, expr_text)
                .and(Span::new(span.end, span.end), format!(" satisfies {cast}")),
        ));
        let mut report = cx.report(span, message).id(id);
        for (id, desc, fix) in suggestions {
            report = report.suggest(id, desc, fix);
        }
    }
}

/// The assertion's operand with any parentheses written around it inside the assertion.
fn expr_with_parens(cx: &Ctx<'_, '_>, node: NodeId) -> NodeId {
    match *cx.node(node) {
        Node::As { expr, .. } | Node::TypeAssertion { expr, .. } => expr,
        _ => node,
    }
}

impl Rule for ConsistentTypeAssertions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::As, NodeKind::TypeAssertion]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let (expected, expr, ty) = match *ast.node(node) {
            Node::As { expr, ty } => (Style::As, expr, ty),
            Node::TypeAssertion { ty, expr } => (Style::AngleBracket, expr, ty),
            _ => return,
        };
        if self.style == expected {
            self.check_literal(node, skip_parens(ast, expr), ty, cx);
            return;
        }
        let span = cx.span(node);
        let cast = cx.span(ty).text(src);
        match self.style {
            Style::Never => {
                if !is_const(ast, ty) {
                    cx.report(span, "Do not use any type assertions.")
                        .id("never");
                }
            }
            Style::AngleBracket => {
                cx.report(span, format!("Use '<{cast}>' instead of 'as {cast}'."))
                    .id("angle-bracket");
            }
            Style::As => {
                let inner = skip_parens(ast, expr);
                let expr_text = cx.span(inner).text(src);
                let mut text = if binds_tighter_than_as(ast, inner) {
                    format!("{expr_text} as {cast}")
                } else {
                    format!("({expr_text}) as {cast}")
                };
                if let Some(p) = cx.parent(node)
                    && !matches!(ast.node(p), Node::Paren { .. })
                    && parent_binds_as_tightly(ast, p, node)
                {
                    text = format!("({text})");
                }
                cx.report(span, format!("Use 'as {cast}' instead of '<{cast}>'."))
                    .id("as")
                    .fix(Fix::replace(span, text));
            }
        }
    }
}
