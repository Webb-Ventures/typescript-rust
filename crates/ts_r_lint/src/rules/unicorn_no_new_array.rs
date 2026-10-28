//! `unicorn/no-new-array`: `new Array(x)` with one argument is ambiguous (a length or a
//! single element), so it is reported.
//!
//! The fix to `[x]` is offered only when `x` has a known value that is not a number
//! ([`static_kind`]); a spread argument gets a `[...x]` suggestion instead. Neither is
//! offered when a comment outside the argument would be lost. One pair of parentheses
//! around the argument is kept, and a `;` is added when the `[` would join the previous
//! statement.

use super::prelude::*;
use super::unicorn_util::{joins_previous_statement, skip_wrappers};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-new-array",
        "Disallow `new Array()`.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoNewArray>,
};

#[derive(Debug, Default)]
struct NoNewArray;

impl Rule for NoNewArray {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let [arg] = ast.list(args) else {
            return;
        };
        if ident_name(ast, callee) != Some("Array") {
            return;
        }
        let message = "`new Array()` is unclear in intent; use an array literal or `Array.from()`.";
        let inner = skip_parens(ast, *arg);
        let inner_span = cx.span(inner);
        let span = cx.span(node);
        let lost = ast
            .comments_in(span)
            .iter()
            .any(|c| c.span.start < inner_span.start || c.span.end > inner_span.end);
        if lost {
            cx.report(span, message).id("error");
            return;
        }
        let mut text = cx.text(inner).to_owned();
        if inner != *arg {
            text = format!("({text})");
        }
        let semi = if joins_previous_statement(cx, node) {
            ";"
        } else {
            ""
        };
        let fix = Fix::replace(span, format!("{semi}[{text}]"));
        if matches!(ast.node(inner), Node::Spread { .. }) {
            cx.report(span, message)
                .id("error")
                .suggest("spread", "Spread the argument.", fix);
            return;
        }
        let budget = std::cell::Cell::new(MAX_STEPS);
        let fixable = static_kind(cx, inner, 0, &budget).is_some_and(|k| k != Kind::Number);
        cx.report(span, message)
            .id("error")
            .fix_opt(fixable.then_some(fix));
    }
}

/// Nodes one [`static_kind`] evaluation may visit.
const MAX_STEPS: u32 = 10_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Number,
    BigInt,
    String,
    Bool,
    Null,
    Undefined,
    Object,
}

/// The type of `e`'s value when ESLint's static evaluation would know it: literals,
/// substitution-free or fully static templates, unary and binary operators on known
/// values, the last expression of a side-effect-free sequence, array and object literals of
/// known values, `undefined`/`NaN`/`Infinity`, and `const` bindings declared before use.
/// Anything with possible side effects, branches, member reads or calls is unknown.
///
/// `const` initializers are evaluated again at each use, so `budget` caps the nodes one
/// evaluation visits: `const a1 = (a0, a0), a2 = (a1, a1), ...` would double the work per
/// line. Past it the value is unknown.
fn static_kind(
    cx: &Ctx<'_, '_>,
    e: NodeId,
    depth: u32,
    budget: &std::cell::Cell<u32>,
) -> Option<Kind> {
    if depth > 32 || budget.get() == 0 {
        return None;
    }
    budget.set(budget.get() - 1);
    let ast = cx.ast();
    let e = skip_wrappers(ast, e);
    let next = |id| static_kind(cx, id, depth + 1, budget);
    Some(match *ast.node(e) {
        Node::NumberLit { .. } => Kind::Number,
        Node::BigIntLit { .. } => Kind::BigInt,
        Node::StringLit { .. } => Kind::String,
        Node::BoolLit { .. } => Kind::Bool,
        Node::NullLit {} => Kind::Null,
        Node::RegexLit { .. } => Kind::Object,
        Node::TemplateLit { parts } => {
            let parts = ast.list(parts);
            for &p in parts.iter().skip(1).step_by(2) {
                next(p)?;
            }
            Kind::String
        }
        Node::Unary { op, arg } => {
            let k = next(arg)?;
            match op {
                UnaryOp::TypeOf => Kind::String,
                UnaryOp::Not => Kind::Bool,
                UnaryOp::Void => Kind::Undefined,
                UnaryOp::Minus | UnaryOp::BitNot if k == Kind::BigInt => Kind::BigInt,
                UnaryOp::Minus | UnaryOp::BitNot => Kind::Number,
                UnaryOp::Plus if k == Kind::BigInt => return None,
                UnaryOp::Plus => Kind::Number,
                UnaryOp::Delete => return None,
            }
        }
        Node::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
            ) {
                return None;
            }
            let (l, r) = (next(left)?, next(right)?);
            match op {
                BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq => Kind::Bool,
                BinaryOp::In | BinaryOp::InstanceOf => return None,
                BinaryOp::Add
                    if [l, r]
                        .iter()
                        .any(|k| matches!(k, Kind::String | Kind::Object)) =>
                {
                    Kind::String
                }
                _ if l == Kind::BigInt && r == Kind::BigInt => Kind::BigInt,
                _ if l == Kind::BigInt || r == Kind::BigInt => return None,
                _ => Kind::Number,
            }
        }
        Node::Seq { exprs } => {
            let mut last = None;
            for &x in ast.list(exprs) {
                last = Some(next(x)?);
            }
            last?
        }
        Node::ArrayLit { elements } => {
            for &el in ast.list(elements) {
                if matches!(ast.node(el), Node::Spread { .. } | Node::Hole {}) {
                    return None;
                }
                next(el)?;
            }
            Kind::Object
        }
        Node::ObjectLit { props } => {
            for &p in ast.list(props) {
                let Node::Property { key, value, .. } = *ast.node(p) else {
                    return None;
                };
                if !matches!(
                    ast.node(key),
                    Node::Ident { .. } | Node::StringLit { .. } | Node::NumberLit { .. }
                ) || key_name(ast, cx.src(), key).as_deref() == Some("__proto__")
                {
                    return None;
                }
                next(value)?;
            }
            Kind::Object
        }
        Node::Ident { .. } => {
            if cx.is_global_ref(e, "undefined") {
                return Some(Kind::Undefined);
            }
            if cx.is_global_ref(e, "NaN") || cx.is_global_ref(e, "Infinity") {
                return Some(Kind::Number);
            }
            let init = const_declared_before(cx, e)?;
            next(init)?
        }
        _ => return None,
    })
}

/// The initializer of the `const` (single declaration, plain name) `ident` refers to, when
/// the declaration ends before `ident`.
fn const_declared_before(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(ident)?).symbol()?;
    let s = sem.symbol(sym);
    let [decl] = s.decls.as_slice() else {
        return None;
    };
    if decl.kind != DeclKind::Const {
        return None;
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl.node)
    else {
        return None;
    };
    (name == decl.name && ast.span(decl.node).end <= cx.span(ident).start).then_some(init)
}
