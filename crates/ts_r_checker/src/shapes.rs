//! Checks that read only the syntax of an expression: an unused comma operand without side
//! effects (TS2695), a condition that is always truthy or falsy (TS2872/TS2873), and a `??`
//! whose left side is always or never nullish (TS2871/TS2869).
//!
//! Rules observed from tsc 7.0.2 (`(e)`, `e as T`, `<T>e`, `e satisfies T` and `e!` are
//! "outer" wrappers below):
//! - **TS2695** for each `,` whose left side (everything before it) has no side effects:
//!   names, literals (templates with substitutions included), object and array literals,
//!   functions, arrows, class expressions, JSX elements (not fragments), `!`/`~`/`-`/`+`/
//!   `typeof` of anything, `e!` of anything, and parentheses, binary operators (not
//!   assignments), commas and conditionals (both branches; not the condition) whose operands
//!   are all free of side effects. Reported at the left side. `(0, o.m)()`, ``(0, o.m)`x` ``
//!   and `(0, eval)()` (a numeric literal with value 0, then a property or element access or
//!   `eval`, parenthesized as a callee or tag) are the indirect-call idiom and are fine.
//!   Not reported at all under `allowUnreachableCode: true`. A missing operand (`(, x)`)
//!   counts as a name; a left side starting inside the parser's TS2657 (adjacent JSX
//!   elements joined by recovery) is not reported (tsc: checker.go, comma operator case).
//! - **TS2872/TS2873** on a condition (`if`, loops, `?:`, the left of `&&`/`||`, the operand
//!   of `!`), classified through outer wrappers and reported at the condition as written
//!   (`satisfies` at its keyword; a named function or class expression at its name, here
//!   and for the other codes): always truthy are object and array literals, functions,
//!   arrows, classes, regular expressions, bigint literals (`0n` too), non-empty strings and
//!   substitution-free templates, and numbers other than 0 and 1; always falsy are `""`,
//!   an empty template, `null`, `undefined` and `void e`; a conditional whose branches agree.
//!   `true`, `false`, `0` and `1` are fine (`while (1)`).
//! - **TS2871/TS2869** on the left of `??`, classified through outer wrappers and reported
//!   there: always nullish are `null`, `undefined`; never nullish are literals (templates
//!   with substitutions too), object and array literals, functions, arrows, classes, every
//!   unary and update operator (`void` included), binary operators other than `&&`, `||` and
//!   `??`, and compound assignments other than `&&=`/`||=`/`??=`. A conditional whose
//!   branches agree, `a = b`, `a ??= b`, `a ?? b` and `(a, b)` take the class of `b`.

use ts_r_ast::{AssignOp, Ast, BinaryOp, Node, NodeId, Span, UnaryOp};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// What the syntax alone says about a value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Truthy,
    Falsy,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nullish {
    Always,
    Never,
    Unknown,
}

const MAX_DEPTH: u32 = 200;

/// `e` without parentheses, `as`, `satisfies`, type assertions and `!`.
fn skip_outer(ast: &Ast, mut e: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(e) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => e = expr,
            _ => break,
        }
    }
    e
}

/// Where tsc reports on expression `e`: the name of a named function or class expression,
/// else the whole expression.
fn report_span(ast: &Ast, e: NodeId) -> Span {
    match *ast.node(e) {
        Node::FunctionExpr { name: Some(n), .. } | Node::ClassExpr { name: Some(n), .. } => {
            ast.span(n)
        }
        _ => ast.span(e),
    }
}

fn is_undefined_name(ast: &Ast, e: NodeId) -> bool {
    matches!(*ast.node(e), Node::Ident { name } if ast.atom(name) == "undefined")
}

fn truthiness_shape(ast: &Ast, e: NodeId, depth: u32) -> Shape {
    if depth > MAX_DEPTH {
        return Shape::Unknown;
    }
    let e = skip_outer(ast, e);
    match *ast.node(e) {
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. } => Shape::Truthy,
        Node::StringLit { value } => {
            if ast.atom(value).is_empty() {
                Shape::Falsy
            } else {
                Shape::Truthy
            }
        }
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement { cooked, .. } if ast.atom(cooked).is_empty() => Shape::Falsy,
                _ => Shape::Truthy,
            },
            _ => Shape::Unknown,
        },
        Node::NumberLit { value } => {
            let v = value.get();
            if v == 0.0 || v == 1.0 {
                Shape::Unknown
            } else {
                Shape::Truthy
            }
        }
        Node::NullLit {}
        | Node::Unary {
            op: UnaryOp::Void, ..
        } => Shape::Falsy,
        Node::Ident { .. } if is_undefined_name(ast, e) => Shape::Falsy,
        Node::Conditional { cons, alt, .. } => {
            let a = truthiness_shape(ast, cons, depth + 1);
            if a != Shape::Unknown && a == truthiness_shape(ast, alt, depth + 1) {
                a
            } else {
                Shape::Unknown
            }
        }
        _ => Shape::Unknown,
    }
}

fn nullish_shape(ast: &Ast, e: NodeId, depth: u32) -> Nullish {
    if depth > MAX_DEPTH {
        return Nullish::Unknown;
    }
    let e = skip_outer(ast, e);
    match *ast.node(e) {
        Node::NullLit {} => Nullish::Always,
        Node::Ident { .. } if is_undefined_name(ast, e) => Nullish::Always,
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::RegexLit { .. }
        | Node::TemplateLit { .. }
        | Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::Unary { .. }
        | Node::Update { .. } => Nullish::Never,
        Node::Binary { op, right, .. } => match op {
            BinaryOp::Coalesce => nullish_shape(ast, right, depth + 1),
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => Nullish::Unknown,
            _ => Nullish::Never,
        },
        Node::Assign { op, right, .. } => match op {
            AssignOp::Assign | AssignOp::CoalesceAssign => nullish_shape(ast, right, depth + 1),
            AssignOp::AndAssign | AssignOp::OrAssign => Nullish::Unknown,
            _ => Nullish::Never,
        },
        Node::Seq { exprs } => match ast.list(exprs).last() {
            Some(&last) => nullish_shape(ast, last, depth + 1),
            None => Nullish::Unknown,
        },
        Node::Conditional { cons, alt, .. } => {
            let a = nullish_shape(ast, cons, depth + 1);
            if a != Nullish::Unknown && a == nullish_shape(ast, alt, depth + 1) {
                a
            } else {
                Nullish::Unknown
            }
        }
        _ => Nullish::Unknown,
    }
}

fn side_effect_free(ast: &Ast, e: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match *ast.node(e) {
        // A missing operand is tsc's empty identifier (`(, x)`).
        Node::Ident { .. }
        | Node::Missing {}
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::TemplateLit { .. }
        | Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::JsxElement { .. }
        | Node::NonNull { .. } => true,
        Node::Unary { op, .. } => !matches!(op, UnaryOp::Void | UnaryOp::Delete),
        Node::Paren { expr } => side_effect_free(ast, expr, depth + 1),
        Node::Binary { left, right, .. } => {
            side_effect_free(ast, left, depth + 1) && side_effect_free(ast, right, depth + 1)
        }
        Node::Seq { exprs } => ast
            .list(exprs)
            .iter()
            .all(|&x| side_effect_free(ast, x, depth + 1)),
        Node::Conditional { cons, alt, .. } => {
            side_effect_free(ast, cons, depth + 1) && side_effect_free(ast, alt, depth + 1)
        }
        _ => false,
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2695 for the comma expression `e` (`Node::Seq`).
    pub(crate) fn check_comma_left_sides(&mut self, e: Loc) {
        if !self.reporting(e.file) || self.c.opts.allow_unreachable_code == Some(true) {
            return;
        }
        let ast = self.ast(e.file);
        let Node::Seq { exprs } = *ast.node(e.node) else {
            return;
        };
        let exprs = ast.list(exprs);
        if exprs.len() == 2 && self.is_indirect_call_idiom(e, exprs[0], exprs[1]) {
            return;
        }
        let Some(&first) = exprs.first() else {
            return;
        };
        let start = ast.span(first).start;
        for &x in &exprs[..exprs.len().saturating_sub(1)] {
            if !side_effect_free(ast, x, 0) {
                // Every later left side holds this one.
                break;
            }
            let span = if exprs[0] == x {
                report_span(ast, x)
            } else {
                Span::new(start, ast.span(x).end)
            };
            // Adjacent JSX elements the parser joined with a comma (TS2657 there).
            if self
                .c
                .file(e.file)
                .syntax
                .iter()
                .any(|d| d.code == 2657 && d.span.start <= span.start && span.start < d.span.end)
            {
                continue;
            }
            self.error_span(
                e.file,
                span,
                dg::LEFT_SIDE_OF_COMMA_OPERATOR_IS_UNUSED_AND_HAS_NO_SIDE_EFFECTS_2695,
                &[],
            );
        }
    }

    /// `(0, o.m)(...)`, ``(0, o.m)`...` `` or `(0, eval)(...)`.
    fn is_indirect_call_idiom(&self, e: Loc, left: NodeId, right: NodeId) -> bool {
        let ast = self.ast(e.file);
        if !matches!(*ast.node(left), Node::NumberLit { value } if value.get() == 0.0) {
            return false;
        }
        let callee_like = match *ast.node(right) {
            Node::Member { .. } | Node::Index { .. } => true,
            Node::Ident { name } => ast.atom(name) == "eval",
            _ => false,
        };
        if !callee_like {
            return false;
        }
        let Some(p) = self.c.parent(e) else {
            return false;
        };
        if !matches!(*ast.node(p.node), Node::Paren { .. }) {
            return false;
        }
        match self.c.parent(p).map(|g| *ast.node(g.node)) {
            Some(Node::Call { callee, .. }) => callee == p.node,
            Some(Node::TaggedTemplate { tag, .. }) => tag == p.node,
            _ => false,
        }
    }

    /// TS2872/TS2873 for condition `e` whose syntax makes it always truthy or falsy.
    pub(crate) fn check_condition_shape(&mut self, e: Loc) {
        if !self.reporting(e.file) {
            return;
        }
        let ast = self.ast(e.file);
        let info = match truthiness_shape(ast, e.node, 0) {
            Shape::Truthy => dg::THIS_KIND_OF_EXPRESSION_IS_ALWAYS_TRUTHY_2872,
            Shape::Falsy => dg::THIS_KIND_OF_EXPRESSION_IS_ALWAYS_FALSY_2873,
            Shape::Unknown => return,
        };
        let mut span = report_span(ast, e.node);
        if let Node::Satisfies { expr, .. } = *ast.node(e.node) {
            // At the `satisfies` keyword.
            let text = &self.c.file(e.file).text;
            let from = ast.span(expr).end as usize;
            if let Some(off) = text
                .get(from..span.end as usize)
                .and_then(|s| s.find("satisfies"))
            {
                span = Span::new((from + off) as u32, span.end);
            }
        }
        self.error_span(e.file, span, info, &[]);
    }

    /// TS2871/TS2869 for `left` of `left ?? right`.
    pub(crate) fn check_nullish_left_shape(&mut self, left: Loc) {
        if !self.reporting(left.file) {
            return;
        }
        let ast = self.ast(left.file);
        let info = match nullish_shape(ast, left.node, 0) {
            Nullish::Always => dg::THIS_EXPRESSION_IS_ALWAYS_NULLISH_2871,
            Nullish::Never => {
                dg::RIGHT_OPERAND_OF_IS_UNREACHABLE_BECAUSE_THE_LEFT_OPERAND_IS_NEVER_NULLISH_2869
            }
            Nullish::Unknown => return,
        };
        let at = skip_outer(ast, left.node);
        self.error_span(left.file, report_span(ast, at), info, &[]);
    }
}
