//! `@typescript-eslint/prefer-nullish-coalescing`: prefer `??` / `??=` over `||`, `||=`,
//! nullish-check ternaries and `if (x == null) x = y`.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-nullish-coalescing",
        "Enforce using the nullish coalescing operator instead of logical assignments or chaining",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .suggestions(),
    create: create::<PreferNullishCoalescing>,
};

#[derive(Debug)]
struct PreferNullishCoalescing {
    ignore_conditional_tests: bool,
    ignore_ternary_tests: bool,
    ignore_mixed_logical: bool,
    ignore_boolean_coercion: bool,
    ignore_if_statements: bool,
    /// `ignorePrimitives`: string, number, boolean, bigint.
    primitives: [bool; 4],
    allow_without_strict_null_checks: bool,
}

impl FromOptions for PreferNullishCoalescing {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let primitives = match o.get("ignorePrimitives") {
            Some(Value::Bool(b)) => [*b; 4],
            Some(Value::Object(p)) => [
                bool_field(p, "string", false),
                bool_field(p, "number", false),
                bool_field(p, "boolean", false),
                bool_field(p, "bigint", false),
            ],
            _ => [false; 4],
        };
        Ok(PreferNullishCoalescing {
            ignore_conditional_tests: bool_field(o, "ignoreConditionalTests", true),
            ignore_ternary_tests: bool_field(o, "ignoreTernaryTests", false),
            ignore_mixed_logical: bool_field(o, "ignoreMixedLogicalExpressions", false),
            ignore_boolean_coercion: bool_field(o, "ignoreBooleanCoercion", false),
            ignore_if_statements: bool_field(o, "ignoreIfStatements", false),
            primitives,
            allow_without_strict_null_checks: bool_field(
                o,
                "allowRuleToRunWithoutStrictNullChecksIKnowWhatIAmDoing",
                false,
            ),
        })
    }
}

const OR: &str = "Prefer using nullish coalescing operator (`??`) instead of a logical or (`||`), as it is a safer operator.";
const OR_ASSIGN: &str = "Prefer using nullish coalescing operator (`??=`) instead of a logical assignment (`||=`), as it is a safer operator.";
const TERNARY: &str = "Prefer using nullish coalescing operator (`??`) instead of a ternary expression, as it is simpler to read.";
const ASSIGNMENT: &str = "Prefer using nullish coalescing operator (`??=`) instead of an assignment expression, as it is simpler to read.";
const FIX: &str = "Fix to nullish coalescing operator (`??`).";
const FIX_ASSIGN: &str = "Fix to nullish coalescing operator (`??=`).";

const NULLISH: TypeFlags = TypeFlags::NULL
    .union(TypeFlags::UNDEFINED)
    .union(TypeFlags::VOID);

const MAX_DEPTH: u32 = 200;

impl Rule for PreferNullishCoalescing {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Binary,
            NodeKind::Assign,
            NodeKind::Conditional,
            NodeKind::If,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !cx.types().strict_null_checks() {
            if !self.allow_without_strict_null_checks
                && matches!(cx.node(node), Node::Program { .. })
            {
                crate::type_util::report_no_strict_null_checks(cx);
            }
            return;
        }
        match *cx.node(node) {
            Node::Binary {
                left,
                op: BinaryOp::LogicalOr,
                ..
            } => self.check_or(cx, node, left, false),
            Node::Assign {
                left,
                op: AssignOp::OrAssign,
                ..
            } => self.check_or(cx, node, left, true),
            Node::Conditional { test, cons, alt } => self.check_ternary(cx, node, test, cons, alt),
            Node::If {
                test,
                cons,
                alt: None,
            } => self.check_if(cx, node, test, cons),
            _ => {}
        }
    }
}

impl PreferNullishCoalescing {
    fn check_or(&self, cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, assign: bool) {
        let ti = cx.types();
        let Some(t) = ti.type_at(left) else {
            return;
        };
        if self.type_ignored(ti, t) != Some(false) {
            return;
        }
        if self.ignore_conditional_tests && in_test(cx, node) {
            return;
        }
        if self.ignore_boolean_coercion && in_boolean_call(cx, node) {
            return;
        }
        if !assign && self.ignore_mixed_logical && mixed_logical(cx, node) {
            return;
        }
        let src = cx.src();
        let op_start = skip_trivia_forward(src, cx.span(left).end as usize) as u32;
        let len = if assign { 3 } else { 2 };
        if src.get(op_start as usize..(op_start + len) as usize)
            != Some(if assign { "||=" } else { "||" })
        {
            return;
        }
        let op_span = Span::new(op_start, op_start + len);
        let mut fix = Fix::replace(op_span, if assign { "??=" } else { "??" });
        if !assign
            && outer_parent(cx, node).is_some_and(|p| {
                matches!(
                    cx.node(p),
                    Node::Binary {
                        op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd,
                        ..
                    }
                )
            })
        {
            // Inside another logical expression the result is wrapped; when the left side
            // is itself a logical expression the `(` goes before its right operand
            // (observed: `(a && b) || c || d` suggests `(a && (b) ?? c) || d`).
            let span = cx.span(node);
            let ast = cx.ast();
            let open = match *cx.node(skip_parens(ast, left)) {
                Node::Binary {
                    op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                    right,
                    ..
                } => cx.span(right).start,
                _ => span.start,
            };
            fix = Fix::insert(open, "(")
                .and(op_span, "??")
                .and(Span::new(span.end, span.end), ")");
        }
        let (msg, desc) = if assign {
            (OR_ASSIGN, FIX_ASSIGN)
        } else {
            (OR, FIX)
        };
        cx.report(op_span, msg)
            .id("preferNullishOverOr")
            .suggest("suggestNullish", desc, fix);
    }

    fn check_ternary(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        test: NodeId,
        cons: NodeId,
        alt: NodeId,
    ) {
        if self.ignore_ternary_tests {
            return;
        }
        let ast = cx.ast();
        let Some(check) = nullish_check(ast, cx.src(), test, true) else {
            return;
        };
        let (value, fallback) = if check.value_in_cons {
            (cons, alt)
        } else {
            (alt, cons)
        };
        if !same_ref(ast, cx.src(), check.subject, value, 0) {
            return;
        }
        if !self.check_covers(cx, &check) {
            return;
        }
        if self.ignore_conditional_tests && in_test(cx, node) {
            return;
        }
        // A ternary that is itself the `Boolean(..)` argument is still reported; one nested in
        // a logical chain there is not.
        if self.ignore_boolean_coercion
            && outer_parent(cx, node).is_some_and(|p| !matches!(cx.node(p), Node::Call { .. }))
            && in_boolean_call(cx, node)
        {
            return;
        }
        let subject = skip_parens(ast, check.subject);
        let left = operand_text(cx, subject);
        let right = operand_text(cx, fallback);
        let fix = Fix::replace(cx.span(node), format!("{left} ?? {right}"));
        cx.report_node(node, TERNARY)
            .id("preferNullishOverTernary")
            .suggest("suggestNullish", FIX, fix);
    }

    fn check_if(&self, cx: &mut Ctx<'_, '_>, node: NodeId, test: NodeId, cons: NodeId) {
        if self.ignore_if_statements {
            return;
        }
        let ast = cx.ast();
        let stmt = match *cx.node(cons) {
            Node::Block { stmts } => match ast.list(stmts) {
                [s] => *s,
                _ => return,
            },
            _ => cons,
        };
        let Node::ExprStmt { expr } = *cx.node(stmt) else {
            return;
        };
        let assign = skip_parens(ast, expr);
        let Node::Assign { left, op, right } = *cx.node(assign) else {
            return;
        };
        if !matches!(
            op,
            AssignOp::Assign | AssignOp::OrAssign | AssignOp::CoalesceAssign
        ) {
            return;
        }
        let Some(check) = nullish_check(ast, cx.src(), test, false) else {
            return;
        };
        if check.value_in_cons {
            return;
        }
        if !matches!(
            cx.node(skip_parens(ast, left)),
            Node::Ident { .. } | Node::Member { .. } | Node::Index { .. }
        ) || !same_ref(ast, cx.src(), check.subject, left, 0)
        {
            return;
        }
        if !self.check_covers(cx, &check) {
            return;
        }
        let src = cx.src();
        // A parenthesized target keeps one pair (observed: `((((foo).a))) = x` becomes
        // `((foo).a) ??= x`).
        let target = cx.span(skip_parens(ast, left)).text(src);
        let target = if skip_parens(ast, left) != left {
            format!("({target})")
        } else {
            target.to_string()
        };
        let assignment = format!("{target} ??= {};", cx.span(right).text(src));
        // Comments inside the statement survive, in order, around the new assignment.
        let span = cx.span(node);
        let stmt_span = cx.span(stmt);
        let mut items: Vec<(Span, String)> = Vec::new();
        let mut placed = false;
        for c in ast.comments_in(span) {
            if !placed && c.span.start >= stmt_span.start {
                items.push((stmt_span, assignment.clone()));
                placed = true;
            }
            items.push((c.span, c.span.text(src).to_string()));
        }
        if !placed {
            items.push((stmt_span, assignment));
        }
        let mut out = String::new();
        let mut prev: Option<Span> = None;
        for (s, text) in items {
            if let Some(p) = prev {
                let gap = src.get(p.end as usize..s.start as usize).unwrap_or("");
                out.push(if gap.contains('\n') { '\n' } else { ' ' });
            }
            out.push_str(&text);
            prev = Some(s);
        }
        let fix = Fix::replace(span, out);
        cx.report_node(node, ASSIGNMENT)
            .id("preferNullishOverAssignment")
            .suggest("suggestNullish", FIX_ASSIGN, fix);
    }

    /// The test covers every nullish value the subject can have; truthiness tests also need a
    /// nullable subject type that is not ignored.
    fn check_covers(&self, cx: &Ctx<'_, '_>, check: &Check) -> bool {
        let ti = cx.types();
        if check.truthy {
            let Some(t) = ti.type_at(check.subject) else {
                return false;
            };
            return self.type_ignored(ti, t) == Some(false);
        }
        if check.null && check.undefined {
            return true;
        }
        // One strict check: fine when the type cannot hold the other nullish value.
        let Some(t) = ti.type_at(check.subject) else {
            return false;
        };
        let t = constrained(ti, t);
        if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
            return false;
        }
        let mut other = false;
        for p in parts(ti, t) {
            let p = constrained(ti, p);
            if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
                return false;
            }
            let f = ti.flags(p);
            if check.null && f.intersects(TypeFlags::UNDEFINED | TypeFlags::VOID) {
                other = true;
            }
            if check.undefined && f.intersects(TypeFlags::NULL) {
                other = true;
            }
        }
        !other
    }

    /// `Some(false)` when `t` may be nullish and is not excluded by `ignorePrimitives`.
    fn type_ignored(&self, ti: &dyn TypeInfo, t: Ty) -> Option<bool> {
        let t = constrained(ti, t);
        if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
            return None;
        }
        let any_prim = self.primitives.iter().any(|&b| b);
        let mut nullish = false;
        for p in parts(ti, t) {
            let p = constrained(ti, p);
            if p == Ty::UNRESOLVED {
                return None;
            }
            if is_any(ti, p) || is_unknown(ti, p) {
                if any_prim {
                    return Some(true);
                }
                nullish = true;
                continue;
            }
            if ti.flags(p).intersects(NULLISH) {
                nullish = true;
                continue;
            }
            for q in intersection_parts(ti, p) {
                if self.primitive_ignored(ti, q) {
                    return Some(true);
                }
            }
        }
        Some(!nullish)
    }

    fn primitive_ignored(&self, ti: &dyn TypeInfo, t: Ty) -> bool {
        let mut f = ti.flags(t);
        if f.contains(TypeFlags::ENUM_LITERAL)
            && !f.intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
        {
            f = ti.flags(ti.base_of_literal(t));
        }
        let [string, number, boolean, bigint] = self.primitives;
        (string
            && f.intersects(
                TypeFlags::STRING
                    | TypeFlags::STRING_LITERAL
                    | TypeFlags::TEMPLATE_LITERAL
                    | TypeFlags::STRING_MAPPING,
            ))
            || (number && f.intersects(TypeFlags::NUMBER | TypeFlags::NUMBER_LITERAL))
            || (boolean && f.intersects(TypeFlags::BOOLEAN | TypeFlags::BOOLEAN_LITERAL))
            || (bigint && f.intersects(TypeFlags::BIGINT | TypeFlags::BIGINT_LITERAL))
    }
}

/// A nullish test of `subject`.
struct Check {
    subject: NodeId,
    /// The non-nullish value is the consequent (`x != null ? x : y`), not the alternate.
    value_in_cons: bool,
    /// A truthiness test (`x ? x : y`, `!x ? y : x`).
    truthy: bool,
    null: bool,
    undefined: bool,
}

/// One comparison against `null`/`undefined`: (subject, negated, covers null, covers undefined).
fn comparison(ast: &Ast, id: NodeId) -> Option<(NodeId, bool, bool, bool)> {
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let (negated, loose) = match op {
        BinaryOp::Eq => (false, true),
        BinaryOp::NotEq => (true, true),
        BinaryOp::StrictEq => (false, false),
        BinaryOp::StrictNotEq => (true, false),
        _ => return None,
    };
    let kind = |n: NodeId| {
        let n = skip_parens(ast, n);
        if matches!(ast.node(n), Node::NullLit {}) {
            Some(true)
        } else if is_undefined(ast, n) {
            Some(false)
        } else {
            None
        }
    };
    let (subject, is_null) = match (kind(left), kind(right)) {
        (None, Some(k)) => (left, k),
        (Some(k), None) => (right, k),
        _ => return None,
    };
    Some((subject, negated, loose || is_null, loose || !is_null))
}

fn nullish_check(ast: &Ast, src: &str, test: NodeId, allow_truthy: bool) -> Option<Check> {
    let t = skip_parens(ast, test);
    if let Some((subject, negated, null, undefined)) = comparison(ast, t) {
        return Some(Check {
            subject,
            value_in_cons: negated,
            truthy: false,
            null,
            undefined,
        });
    }
    match *ast.node(t) {
        Node::Binary {
            left,
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
            right,
        } => {
            let (s1, n1, null1, u1) = comparison(ast, left)?;
            let (s2, n2, null2, u2) = comparison(ast, right)?;
            let negated = op == BinaryOp::LogicalAnd;
            if n1 != negated || n2 != negated || !same_ref(ast, src, s1, s2, 0) {
                return None;
            }
            Some(Check {
                subject: s1,
                value_in_cons: negated,
                truthy: false,
                null: null1 || null2,
                undefined: u1 || u2,
            })
        }
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } if allow_truthy || is_ref(ast, arg) => Some(Check {
            subject: arg,
            value_in_cons: false,
            truthy: true,
            null: true,
            undefined: true,
        }),
        _ if allow_truthy => Some(Check {
            subject: t,
            value_in_cons: true,
            truthy: true,
            null: true,
            undefined: true,
        }),
        _ => None,
    }
}

fn is_ref(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::Ident { .. } | Node::Member { .. } | Node::Index { .. } | Node::This {}
    )
}

/// Both nodes name the same value: identifiers, `this` and property chains, ignoring
/// parentheses and optional chaining (`a?.b` is `a.b`, `a['b']` is `a.b`).
fn same_ref(ast: &Ast, src: &str, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let a = skip_parens(ast, a);
    let b = skip_parens(ast, b);
    match (ast.node(a), ast.node(b)) {
        (Node::Ident { name: x }, Node::Ident { name: y }) => x == y,
        (Node::This {}, Node::This {}) => true,
        (
            Node::Member { obj: o1, .. } | Node::Index { obj: o1, .. },
            Node::Member { obj: o2, .. } | Node::Index { obj: o2, .. },
        ) => {
            let (o1, o2) = (*o1, *o2);
            let keys = match (member_name(ast, src, a), member_name(ast, src, b)) {
                (Some(x), Some(y)) => x == y,
                (None, None) => match (ast.node(a), ast.node(b)) {
                    (Node::Index { index: i1, .. }, Node::Index { index: i2, .. }) => {
                        same_ref(ast, src, *i1, *i2, depth + 1)
                    }
                    _ => false,
                },
                _ => false,
            };
            keys && same_ref(ast, src, o1, o2, depth + 1)
        }
        _ => false,
    }
}

/// Source text of an operand of `??`, parenthesized when it binds looser than `??` or mixes
/// with it.
fn operand_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let text = cx.text(id);
    let low = match cx.node(id) {
        Node::Binary { op, .. } => matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        _ => precedence(cx.ast(), id) < 4,
    };
    if low {
        format!("({text})")
    } else {
        text.to_string()
    }
}

/// `node` is (part of a logical chain, negation or branch that is) the test of an `if`, loop or
/// conditional.
fn in_test(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::If { test, .. } | Node::While { test, .. } | Node::DoWhile { test, .. } => {
                return test == cur;
            }
            Node::For { test, .. } => return test == Some(cur),
            Node::Conditional { test, .. } if test == cur => return true,
            _ if passes_through(cx, p, cur) => {}
            _ => return false,
        }
        cur = p;
    }
    false
}

/// The nearest parent of `node` that is not a parenthesis.
fn outer_parent(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = node;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if !matches!(cx.node(p), Node::Paren { .. }) {
            return Some(p);
        }
        cur = p;
    }
    None
}

/// `node` is (part of a logical chain, negation or branch that is) the argument of a global
/// `Boolean(..)` call.
fn in_boolean_call(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::Call { callee, args, .. } => {
                return cx.ast().list(args).first() == Some(&cur)
                    && cx.is_global_ref(callee, "Boolean");
            }
            _ if passes_through(cx, p, cur) => {}
            _ => return false,
        }
        cur = p;
    }
    false
}

/// Parents that keep a value's truthiness meaningful to an enclosing test.
fn passes_through(cx: &Ctx<'_, '_>, p: NodeId, child: NodeId) -> bool {
    match *cx.node(p) {
        Node::Paren { .. }
        | Node::Binary {
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            ..
        }
        | Node::Unary {
            op: UnaryOp::Not, ..
        } => true,
        Node::Conditional { test, .. } => test != child,
        Node::Seq { exprs } => cx.ast().list(exprs).last() == Some(&child),
        _ => false,
    }
}

/// The `||` is part of a chain of `||`/`&&` that also contains `&&`.
fn mixed_logical(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let is_logical = |n: NodeId| {
        matches!(
            ast.node(n),
            Node::Paren { .. }
                | Node::Binary {
                    op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd,
                    ..
                }
        )
    };
    let mut root = node;
    for _ in 0..10_000 {
        match cx.parent(root) {
            Some(p) if is_logical(p) => root = p,
            _ => break,
        }
    }
    let mut stack = vec![root];
    let mut budget = 10_000;
    while let Some(n) = stack.pop() {
        budget -= 1;
        if budget == 0 {
            return false;
        }
        match *ast.node(n) {
            Node::Paren { expr } => stack.push(expr),
            Node::Binary {
                left,
                op: op @ (BinaryOp::LogicalOr | BinaryOp::LogicalAnd),
                right,
            } => {
                if op == BinaryOp::LogicalAnd {
                    return true;
                }
                stack.push(left);
                stack.push(right);
            }
            _ => {}
        }
    }
    false
}
