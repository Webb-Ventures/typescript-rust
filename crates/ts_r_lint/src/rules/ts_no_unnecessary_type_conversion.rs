//! `@typescript-eslint/no-unnecessary-type-conversion`: no `String(str)`, `+num`, `!!bool`
//! and friends on values that already have the type.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-type-conversion",
        "Disallow conversion idioms when they do not change the type or value of the expression",
        TS_STRICT_TC,
        Category::Style,
    )
    .typed()
    .suggestions(),
    create: create_default::<NoUnnecessaryTypeConversion>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryTypeConversion;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    String,
    Number,
    Boolean,
    BigInt,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::String => "string",
            Kind::Number => "number",
            Kind::Boolean => "boolean",
            Kind::BigInt => "bigint",
        }
    }

    fn flags(self) -> TypeFlags {
        match self {
            Kind::String => TypeFlags::STRING_LIKE,
            Kind::Number => TypeFlags::NUMBER_LIKE,
            Kind::Boolean => TypeFlags::BOOLEAN_LIKE,
            Kind::BigInt => TypeFlags::BIGINT_LIKE,
        }
    }
}

impl Rule for NoUnnecessaryTypeConversion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Call,
            NodeKind::Unary,
            NodeKind::Binary,
            NodeKind::Assign,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        // (reported span, the value kept, its kind, message)
        let (span, inner, kind, message) = match *cx.node(node) {
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                let callee = skip_parens(ast, callee);
                match *cx.node(callee) {
                    Node::Ident { .. } => {
                        let kind = match ident_name(ast, callee) {
                            Some("String") => Kind::String,
                            Some("Number") => Kind::Number,
                            Some("Boolean") => Kind::Boolean,
                            Some("BigInt") => Kind::BigInt,
                            _ => return,
                        };
                        let [arg] = args else { return };
                        if matches!(cx.node(*arg), Node::Spread { .. }) || !is_global(cx, callee) {
                            return;
                        }
                        let fname = ident_name(ast, callee).unwrap_or_default();
                        let n = kind.name();
                        (
                            cx.span(callee),
                            *arg,
                            kind,
                            format!(
                                "Passing {} {n} to {fname}() does not change the type or value of the {n}.",
                                article(n)
                            ),
                        )
                    }
                    Node::Member { obj, prop, .. } => {
                        if ident_name(ast, prop) != Some("toString") || !args.is_empty() {
                            return;
                        }
                        (
                            Span::new(cx.span(prop).start, cx.span(node).end),
                            obj,
                            Kind::String,
                            "Calling a string's .toString() method does not change the type or value of the string.".to_owned(),
                        )
                    }
                    _ => return,
                }
            }
            Node::Unary { op, arg } => match op {
                UnaryOp::Plus => (
                    Span::new(cx.span(node).start, cx.span(node).start + 1),
                    arg,
                    Kind::Number,
                    "Using the unary + operator on a number does not change the type or value of the number.".to_owned(),
                ),
                UnaryOp::Not | UnaryOp::BitNot => {
                    let Node::Unary { op: inner_op, arg: inner } = *cx.node(arg) else {
                        return;
                    };
                    if inner_op != op {
                        return;
                    }
                    let start = cx.span(node).start;
                    let span = Span::new(start, cx.span(arg).start + 1);
                    if op == UnaryOp::Not {
                        (
                            span,
                            inner,
                            Kind::Boolean,
                            "Using !! on a boolean does not change the type or value of the boolean.".to_owned(),
                        )
                    } else {
                        if !is_integer(cx, inner) {
                            return;
                        }
                        (
                            span,
                            inner,
                            Kind::Number,
                            "Using ~~ on an integer does not change the type or value of the number.".to_owned(),
                        )
                    }
                }
                _ => return,
            },
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            } => {
                if is_empty_string(cx, left) {
                    (
                        Span::new(cx.span(left).start, cx.span(right).start),
                        right,
                        Kind::String,
                        "Concatenating '' with a string does not change the type or value of the string.".to_owned(),
                    )
                } else if is_empty_string(cx, right) {
                    (
                        Span::new(cx.span(left).end, cx.span(right).end),
                        left,
                        Kind::String,
                        "Concatenating a string with '' does not change the type or value of the string.".to_owned(),
                    )
                } else {
                    return;
                }
            }
            Node::Assign {
                left,
                op: AssignOp::AddAssign,
                right,
            } => {
                if !is_empty_string(cx, right) {
                    return;
                }
                (
                    cx.span(node),
                    left,
                    Kind::String,
                    "Concatenating a string with '' does not change the type or value of the string.".to_owned(),
                )
            }
            _ => return,
        };
        if !has_kind(cx, inner, kind) {
            return;
        }
        let inner = skip_parens(ast, inner);
        let text = cx.span(inner).text(src);
        let kept = if is_simple(cx.node(inner)) {
            text.to_string()
        } else {
            format!("({text})")
        };
        let statement = cx
            .parent(node)
            .filter(|&p| matches!(cx.node(p), Node::ExprStmt { .. }));
        let remove = match (cx.node(node), statement) {
            (Node::Assign { .. }, Some(stmt)) => Fix::replace(cx.span(stmt), ""),
            _ => Fix::replace(cx.span(node), kept.clone()),
        };
        let satisfies = format!("{kept} satisfies {}", kind.name());
        let satisfies = if needs_parens_in_parent(cx, node) {
            format!("({satisfies})")
        } else {
            satisfies
        };
        let satisfies = Fix::replace(cx.span(node), satisfies);
        cx.report(span, message)
            .id("unnecessaryTypeConversion")
            .suggest("suggestRemove", "Remove the type conversion.", remove)
            .suggest(
                "suggestSatisfies",
                format!(
                    "Instead, assert that the value satisfies the {} type.",
                    kind.name()
                ),
                satisfies,
            );
    }
}

/// Every part of the expression's type is of `kind` (never an enum, `any` or unresolved).
fn has_kind(cx: &Ctx<'_, '_>, e: NodeId, kind: Kind) -> bool {
    let ti = cx.types();
    let Some(t) = ti.type_at(e) else { return false };
    if has_unresolved_part(ti, t) || is_any(ti, t) {
        return false;
    }
    let ps = parts(ti, constrained(ti, t));
    !ps.is_empty()
        && ps.iter().all(|&p| {
            let p = constrained(ti, p);
            let f = ti.flags(p);
            p != Ty::UNRESOLVED && f.intersects(kind.flags()) && !f.intersects(TypeFlags::ENUM_LIKE)
        })
}

/// The expression's type is integer number literals.
fn is_integer(cx: &Ctx<'_, '_>, e: NodeId) -> bool {
    let ti = cx.types();
    let Some(t) = ti.type_at(e) else { return false };
    if has_unresolved_part(ti, t) {
        return false;
    }
    let ps = parts(ti, t);
    !ps.is_empty()
        && ps.iter().all(|&p| {
            ti.flags(p).contains(TypeFlags::NUMBER_LITERAL)
                && !ti.flags(p).intersects(TypeFlags::ENUM_LIKE)
                && ti
                    .literal_text(p)
                    .and_then(|s| s.parse::<f64>().ok())
                    .is_some_and(|v| v.is_finite() && v.fract() == 0.0)
        })
}

fn is_empty_string(cx: &Ctx<'_, '_>, e: NodeId) -> bool {
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *cx.node(e) {
        Node::StringLit { value } => ast.atom(value).is_empty(),
        Node::TemplateLit { parts } => {
            let list = ast.list(parts);
            list.len() == 1
                && matches!(*cx.node(list[0]), Node::TemplateElement { cooked, .. } if ast.atom(cooked).is_empty())
        }
        _ => false,
    }
}

fn is_global(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ti = cx.types();
    match ti.symbol_at(ident) {
        None => true,
        Some(s) => ti.symbol_decls(s).iter().all(|d| d.in_lib),
    }
}

/// Expressions that stay one operand wherever they are put.
fn is_simple(n: &Node) -> bool {
    matches!(
        n,
        Node::Ident { .. }
            | Node::This {}
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::TemplateLit { .. }
            | Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::Call { .. }
            | Node::New { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::TaggedTemplate { .. }
            | Node::Paren { .. }
    )
}

/// `x satisfies T` replacing `node` needs parentheses.
fn needs_parens_in_parent(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    cx.parent(node).is_some_and(|p| match *cx.node(p) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == node,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == node,
        Node::Binary { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Await { .. }
        | Node::TaggedTemplate { .. }
        | Node::Conditional { .. } => true,
        _ => false,
    })
}
