//! `no-implicit-coercion`: `!!x`, `~s.indexOf(c)`, `+x`, `- -x`, `x * 1`, `x - 0`, `"" + x`,
//! `x += ""` and (optionally) `` `${x}` `` instead of explicit conversions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-implicit-coercion",
        "Disallow shorthand type conversions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoImplicitCoercion>,
};

#[derive(Debug)]
struct NoImplicitCoercion {
    boolean: bool,
    number: bool,
    string: bool,
    template: bool,
    allow: Vec<String>,
}

impl FromOptions for NoImplicitCoercion {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoImplicitCoercion {
            boolean: bool_field(o, "boolean", true),
            number: bool_field(o, "number", true),
            string: bool_field(o, "string", true),
            template: bool_field(o, "disallowTemplateShorthand", false),
            allow: o
                .get("allow")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

fn is_number_one(cx: &Ctx<'_, '_>, n: NodeId, v: f64) -> bool {
    matches!(*cx.node(skip_parens(cx.ast(), n)), Node::NumberLit { value } if value.get() == v)
}

/// A number literal or a `Number()` / `parseInt()` / `parseFloat()` call.
fn is_numeric(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    match *cx.node(skip_parens(ast, n)) {
        Node::NumberLit { .. } => true,
        Node::Call { callee, .. } => matches!(
            ident_name(ast, skip_parens(ast, callee)),
            Some("Number" | "parseInt" | "parseFloat")
        ),
        _ => false,
    }
}

/// An ESTree `BinaryExpression` (not a logical one).
fn is_binary(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    matches!(*cx.node(skip_parens(cx.ast(), n)), Node::Binary { op, .. }
        if !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce))
}

/// A string literal or template (`astUtils.isStringLiteral`), or a `String()` call.
fn is_string_type(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    match *cx.node(skip_parens(ast, n)) {
        Node::StringLit { .. } | Node::TemplateLit { .. } => true,
        Node::Call { callee, .. } => {
            let c = skip_parens(ast, callee);
            matches!(cx.node(c), Node::Ident { .. }) && ident_name(ast, c) == Some("String")
        }
        _ => false,
    }
}

fn is_empty_string(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    match *cx.node(skip_parens(ast, n)) {
        Node::StringLit { value } => ast.atom(value).is_empty(),
        Node::TemplateLit { parts } => match *ast.list(parts) {
            [e] => {
                matches!(*cx.node(e), Node::TemplateElement { cooked, .. } if ast.atom(cooked).is_empty())
            }
            _ => false,
        },
        _ => false,
    }
}

/// The operand's text, parenthesized when it is a sequence.
fn operand_text(cx: &Ctx<'_, '_>, n: NodeId) -> String {
    let n = skip_parens(cx.ast(), n);
    let text = cx.text(n);
    if matches!(cx.node(n), Node::Seq { .. }) {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// Part of an optional chain (ESTree wraps it in a `ChainExpression`).
fn in_optional_chain(cx: &Ctx<'_, '_>, mut n: NodeId) -> bool {
    for _ in 0..256 {
        match *cx.node(n) {
            Node::Call {
                optional, callee, ..
            } => {
                if optional {
                    return true;
                }
                n = callee;
            }
            Node::Member { optional, obj, .. } | Node::Index { optional, obj, .. } => {
                if optional {
                    return true;
                }
                n = obj;
            }
            Node::NonNull { expr } => n = expr,
            _ => return false,
        }
    }
    false
}

impl NoImplicitCoercion {
    fn allowed(&self, op: &str) -> bool {
        self.allow.iter().any(|a| a == op)
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, node: NodeId, rec: String, suggest: bool, fix: bool) {
        let span = cx.ast().span(node);
        let src = cx.src();
        let before = skip_trivia_backward(cx.ast(), src, span.start as usize);
        let spaced = before == span.start as usize && src[..before].ends_with(is_ident_char);
        let text = if spaced {
            format!(" {rec}")
        } else {
            rec.clone()
        };
        let r = cx
            .report(
                span,
                format!("Unexpected implicit coercion encountered. Use `{rec}` instead."),
            )
            .id("implicitCoercion");
        if fix {
            r.fix(Fix::replace(span, text));
        } else if suggest {
            r.suggest(
                "useRecommendation",
                format!("Use `{rec}` instead."),
                Fix::replace(span, text),
            );
        }
    }

    /// `Boolean` is the global, not shadowed.
    fn boolean_is_global(&self, cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let sem = cx.semantic();
        match sem.lookup(sem.enclosing_scope(node), "Boolean") {
            Some(s) => sem
                .symbol(s)
                .decls
                .iter()
                .all(|d| d.kind == DeclKind::CommentGlobal),
            None => sem.is_global("Boolean"),
        }
    }
}

impl Rule for NoImplicitCoercion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary, NodeKind::Assign, NodeKind::TemplateLit]
    }
    fn exit_kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Unary { op, arg } => {
                let inner = skip_parens(ast, arg);
                let inner_unary = match *cx.node(inner) {
                    Node::Unary { op, arg } => Some((op, arg)),
                    _ => None,
                };
                if op == UnaryOp::Not
                    && self.boolean
                    && !self.allowed("!!")
                    && let Some((UnaryOp::Not, a)) = inner_unary
                {
                    let rec = format!("Boolean({})", operand_text(cx, a));
                    let fix = self.boolean_is_global(cx, node);
                    self.report(cx, node, rec, true, fix);
                }
                if op == UnaryOp::BitNot
                    && self.boolean
                    && !self.allowed("~")
                    && let Node::Call { callee, .. } = *cx.node(inner)
                    && member_name(ast, cx.src(), skip_parens(ast, callee))
                        .is_some_and(|m| m == "indexOf" || m == "lastIndexOf")
                {
                    let cmp = if in_optional_chain(cx, inner) {
                        ">= 0"
                    } else {
                        "!== -1"
                    };
                    let rec = format!("{} {cmp}", cx.text(inner));
                    self.report(cx, node, rec, false, false);
                }
                if op == UnaryOp::Plus && self.number && !self.allowed("+") && !is_numeric(cx, arg)
                {
                    let rec = format!("Number({})", operand_text(cx, arg));
                    self.report(cx, node, rec, true, false);
                }
                if op == UnaryOp::Minus
                    && self.number
                    && !self.allowed("- -")
                    && let Some((UnaryOp::Minus, a)) = inner_unary
                    && !is_numeric(cx, a)
                {
                    let rec = format!("Number({})", operand_text(cx, a));
                    self.report(cx, node, rec, true, false);
                }
            }
            Node::Assign {
                op: AssignOp::AddAssign,
                left,
                right,
            } if self.string && !self.allowed("+") && is_empty_string(cx, right) => {
                let code = cx.text(skip_parens(ast, left));
                let rec = format!("{code} = String({code})");
                self.report(cx, node, rec, true, false);
            }
            Node::TemplateLit { parts } if self.template => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                let [q0, e, q1] = *ast.list(parts) else {
                    return;
                };
                let empty = |q: NodeId| matches!(*cx.node(q), Node::TemplateElement { cooked, .. } if ast.atom(cooked).is_empty());
                if empty(q0) && empty(q1) && !is_string_type(cx, e) {
                    let rec = format!("String({})", operand_text(cx, e));
                    self.report(cx, node, rec, true, false);
                }
            }
            _ => {}
        }
    }
    fn exit(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary { op, left, right } = *cx.node(node) else {
            return;
        };
        if op == BinaryOp::Mul && self.number && !self.allowed("*") {
            let by_one = is_number_one(cx, left, 1.0) || is_number_one(cx, right, 1.0);
            // `a * 1 / b` is a fraction, not a coercion.
            let fraction = is_number_one(cx, right, 1.0)
                && cx.parent(node).is_some_and(|p| {
                    matches!(*cx.node(p), Node::Binary { op: BinaryOp::Div, left: l, .. } if l == node)
                });
            let operand = if !is_binary(cx, right) && !is_numeric(cx, right) {
                Some(right)
            } else if !is_binary(cx, left) && !is_numeric(cx, left) {
                Some(left)
            } else {
                None
            };
            if by_one
                && !fraction
                && let Some(o) = operand
            {
                let rec = format!("Number({})", operand_text(cx, o));
                self.report(cx, node, rec, true, false);
            }
        }
        if op == BinaryOp::Sub
            && self.number
            && !self.allowed("-")
            && is_number_one(cx, right, 0.0)
            && !is_numeric(cx, left)
        {
            let rec = format!("Number({})", operand_text(cx, left));
            self.report(cx, node, rec, true, false);
        }
        if op == BinaryOp::Add && self.string && !self.allowed("+") {
            let concat = (is_empty_string(cx, left) && !is_string_type(cx, right))
                || (is_empty_string(cx, right) && !is_string_type(cx, left));
            if concat {
                let other = if is_empty_string(cx, left) {
                    right
                } else {
                    left
                };
                let rec = format!("String({})", operand_text(cx, other));
                self.report(cx, node, rec, true, false);
            }
        }
    }
}
