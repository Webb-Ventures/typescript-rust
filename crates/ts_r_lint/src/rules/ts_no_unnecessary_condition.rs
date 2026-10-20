//! `@typescript-eslint/no-unnecessary-condition`: disallow conditionals whose value the types
//! already decide.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-condition",
        "Disallow conditionals where the type is always truthy or always falsy",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create::<NoUnnecessaryCondition>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopMode {
    Never,
    Always,
    OnlyAllowedLiterals,
}

#[derive(Debug)]
struct NoUnnecessaryCondition {
    loops: LoopMode,
    check_predicates: bool,
}

impl FromOptions for NoUnnecessaryCondition {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let loops = match o.get("allowConstantLoopConditions") {
            Some(Value::Bool(true)) => LoopMode::Always,
            Some(Value::String(s)) if s == "always" => LoopMode::Always,
            Some(Value::String(s)) if s == "only-allowed-literals" => LoopMode::OnlyAllowedLiterals,
            _ => LoopMode::Never,
        };
        Ok(NoUnnecessaryCondition {
            loops,
            check_predicates: bool_field(o, "checkTypePredicates", false),
        })
    }
}

const ALWAYS_TRUTHY: &str = "Unnecessary conditional, value is always truthy.";
const ALWAYS_FALSY: &str = "Unnecessary conditional, value is always falsy.";
const NEVER: &str = "Unnecessary conditional, value is `never`.";
const ALWAYS_NULLISH: &str =
    "Unnecessary conditional, left-hand side of `??` operator is always `null` or `undefined`.";
const NEVER_NULLISH: &str = "Unnecessary conditional, expected left-hand side of `??` operator to be possibly null or undefined.";
const NO_OVERLAP: &str = "Unnecessary conditional, the types have no overlap.";
const NEVER_OPTIONAL_CHAIN: &str = "Unnecessary optional chain on a non-nullish value.";
const TRUTHY_FUNC: &str = "This callback should return a conditional, but return is always truthy.";
const FALSY_FUNC: &str = "This callback should return a conditional, but return is always falsy.";
const GUARD_ASSERTION: &str = "Unnecessary conditional, expression already has the type being checked by the assertion function.";
const GUARD_PREDICATE: &str =
    "Unnecessary conditional, expression already has the type being checked by the type guard.";

const NULLISH: TypeFlags = TypeFlags::NULL
    .union(TypeFlags::UNDEFINED)
    .union(TypeFlags::VOID);

const MAX_DEPTH: u32 = 100;

/// What the types say about a value's truthiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Truth {
    Truthy,
    Falsy,
    Maybe,
    Never,
}

impl Rule for NoUnnecessaryCondition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::If,
            NodeKind::While,
            NodeKind::DoWhile,
            NodeKind::For,
            NodeKind::Conditional,
            NodeKind::Binary,
            NodeKind::Assign,
            NodeKind::Member,
            NodeKind::Index,
            NodeKind::Call,
            NodeKind::Switch,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::If { test, .. } | Node::Conditional { test, .. } => {
                self.check_condition(cx, test, 0, true);
            }
            Node::While { test, .. } | Node::DoWhile { test, .. } => self.check_loop(cx, test),
            Node::For {
                test: Some(test), ..
            } => self.check_loop(cx, test),
            Node::Binary { left, op, right } => match op {
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                    // The left operand's own `&&`/`||` right side only matters for its
                    // truthiness when this chain's value is not the result itself.
                    let nested = op == BinaryOp::LogicalAnd || in_condition(cx, node);
                    self.check_condition(cx, left, 0, nested);
                }
                BinaryOp::Coalesce => check_nullish(cx, left),
                BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq => check_comparison(cx, node, left, op, right),
                _ => {}
            },
            Node::Assign { left, op, .. } => match op {
                AssignOp::AndAssign | AssignOp::OrAssign => self.check_condition(cx, left, 0, true),
                AssignOp::CoalesceAssign => check_nullish(cx, left),
                _ => {}
            },
            Node::Switch { disc, cases } => check_switch(cx, disc, cases),
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    check_optional_chain(cx, node, obj);
                }
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    check_optional_chain(cx, node, callee);
                }
                self.check_array_predicate(cx, node);
                if self.check_predicates {
                    check_type_predicate(cx, node);
                }
            }
            _ => {}
        }
    }
}

impl NoUnnecessaryCondition {
    fn check_loop(&self, cx: &mut Ctx<'_, '_>, test: NodeId) {
        match self.loops {
            LoopMode::Never => self.check_condition(cx, test, 0, true),
            LoopMode::Always => {
                if truth_of(cx, skip_parens(cx.ast(), test)) != Some(Truth::Truthy) {
                    self.check_condition(cx, test, 0, true);
                }
            }
            LoopMode::OnlyAllowedLiterals => {
                let allowed = match *cx.node(skip_parens(cx.ast(), test)) {
                    Node::BoolLit { .. } => true,
                    Node::NumberLit { value } => value.get() == 0.0 || value.get() == 1.0,
                    _ => false,
                };
                if !allowed {
                    self.check_condition(cx, test, 0, true);
                }
            }
        }
    }

    /// Reports `expr` when its truthiness is decided by its type. `&&`/`||` chains are judged
    /// by their right operand (the left one is checked where the chain itself is visited); `!`
    /// is looked through and flips the verdict.
    fn check_condition(&self, cx: &mut Ctx<'_, '_>, expr: NodeId, depth: u32, nested: bool) {
        if depth > MAX_DEPTH {
            return;
        }
        let ast = cx.ast();
        let outer = skip_parens(ast, expr);
        let mut inner = outer;
        let mut negated = false;
        for _ in 0..MAX_DEPTH {
            match *cx.node(inner) {
                Node::Unary {
                    op: UnaryOp::Not,
                    arg,
                } => {
                    inner = skip_parens(ast, arg);
                    negated = !negated;
                }
                _ => break,
            }
        }
        if let Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
            right,
            ..
        } = *cx.node(inner)
        {
            if nested {
                self.check_condition(cx, right, depth + 1, true);
            }
            return;
        }
        if is_array_index(cx, inner) {
            return;
        }
        let Some(mut truth) = truth_of(cx, inner) else {
            return;
        };
        if negated {
            truth = match truth {
                Truth::Truthy => Truth::Falsy,
                Truth::Falsy => Truth::Truthy,
                t => t,
            };
        }
        let (msg, id) = match truth {
            Truth::Truthy => (ALWAYS_TRUTHY, "alwaysTruthy"),
            Truth::Falsy => (ALWAYS_FALSY, "alwaysFalsy"),
            Truth::Never => (NEVER, "never"),
            Truth::Maybe => return,
        };
        cx.report_node(outer, msg).id(id);
    }

    /// `arr.filter(cb)` and friends: a callback whose result the types decide.
    fn check_array_predicate(&self, cx: &mut Ctx<'_, '_>, call: NodeId) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(call) else {
            return;
        };
        let Some((obj, name)) = member_parts(ast, cx.src(), callee) else {
            return;
        };
        if !matches!(
            &*name,
            "filter" | "find" | "findIndex" | "findLast" | "findLastIndex" | "some" | "every"
        ) {
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(obj) else {
            return;
        };
        let t = ti.non_nullable(constrained(ti, t));
        let ps = parts(ti, t);
        if ps.is_empty() || !ps.iter().all(|&p| ti.is_array(p) || ti.is_tuple(p)) {
            return;
        }
        let Some(&cb) = ast.list(args).first() else {
            return;
        };
        let cb_inner = skip_parens(ast, cb);
        match *cx.node(cb_inner) {
            Node::Arrow { body, .. } if !matches!(cx.node(body), Node::Block { .. }) => {
                self.check_condition(cx, body, 0, true);
                return;
            }
            Node::Arrow { body, .. }
            | Node::FunctionExpr {
                body: Some(body), ..
            } => {
                let mut returns = Vec::new();
                collect_returns(ast, body, &mut returns, 0);
                if let [only] = returns[..] {
                    self.check_condition(cx, only, 0, true);
                }
                return;
            }
            Node::Spread { .. } => return,
            _ => {}
        }
        let Some(ct) = ti.type_at(cb) else {
            return;
        };
        let sigs = ti.signatures(constrained(ti, ct), false);
        if sigs.is_empty() {
            return;
        }
        let mut verdict = None;
        for s in sigs {
            let r = ti.return_type(s);
            let Some(v) = truth_of_type(ti, r) else {
                return;
            };
            match verdict {
                None => verdict = Some(v),
                Some(prev) if prev == v => {}
                _ => return,
            }
        }
        let (msg, id) = match verdict {
            Some(Truth::Truthy) => (TRUTHY_FUNC, "alwaysTruthyFunc"),
            Some(Truth::Falsy) => (FALSY_FUNC, "alwaysFalsyFunc"),
            _ => return,
        };
        cx.report_node(cb, msg).id(id);
    }
}

/// The value of `node` is only used for its truthiness (an `if`/loop/conditional test, a
/// negation, or the left side of `&&`), possibly through `&&`/`||` chains.
fn in_condition(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::Paren { .. } => {}
            Node::Unary {
                op: UnaryOp::Not, ..
            } => return true,
            Node::Binary {
                left,
                op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
                ..
            } => {
                if op == BinaryOp::LogicalAnd && left == cur {
                    return true;
                }
            }
            Node::If { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. }
            | Node::Conditional { test, .. } => return test == cur,
            Node::For { test, .. } => return test == Some(cur),
            _ => return false,
        }
        cur = p;
    }
    false
}

/// `return x` statements of a function body (not of nested functions).
fn collect_returns(ast: &Ast, n: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > MAX_DEPTH || out.len() > 1 {
        return;
    }
    match *ast.node(n) {
        Node::Return { arg } => {
            if let Some(a) = arg {
                out.push(a);
            } else {
                // A bare `return` makes the result `undefined` on that path: don't judge.
                out.push(n);
                out.push(n);
            }
        }
        _ if is_function(ast.node(n).kind())
            || matches!(ast.node(n), Node::ClassDecl { .. } | Node::ClassExpr { .. }) => {}
        _ => {
            let mut kids = Vec::new();
            ast.for_each_child(n, |c| kids.push(c));
            for c in kids {
                collect_returns(ast, c, out, depth + 1);
            }
        }
    }
}

/// The verdict for an expression's type; `None` when the type says nothing (any, unknown,
/// generic, unresolved).
fn truth_of(cx: &Ctx<'_, '_>, n: NodeId) -> Option<Truth> {
    let ti = cx.types();
    let t = ti.type_at(n)?;
    truth_of_type(ti, t)
}

fn truth_of_type(ti: &dyn TypeInfo, t: Ty) -> Option<Truth> {
    let t = constrained(ti, t);
    if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
        return None;
    }
    if ti.flags(t).contains(TypeFlags::NEVER) {
        return Some(Truth::Never);
    }
    let mut all_truthy = true;
    let mut all_falsy = true;
    for p in parts(ti, t) {
        let v = part_truth(ti, p, 0)?;
        match v {
            Truth::Truthy => all_falsy = false,
            Truth::Falsy => all_truthy = false,
            _ => {
                all_truthy = false;
                all_falsy = false;
            }
        }
    }
    Some(if all_truthy {
        Truth::Truthy
    } else if all_falsy {
        Truth::Falsy
    } else {
        Truth::Maybe
    })
}

/// Truthiness of one union member. Objects count as truthy only when no primitive member of
/// an intersection keeps it open (`string & {}` may be `''`).
fn part_truth(ti: &dyn TypeInfo, p: Ty, depth: u32) -> Option<Truth> {
    if depth > MAX_DEPTH {
        return None;
    }
    let p = constrained(ti, p);
    if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
        return None;
    }
    let f = ti.flags(p);
    if f.intersects(TypeFlags::INSTANTIABLE) {
        return None;
    }
    if f.contains(TypeFlags::INTERSECTION) {
        let mut literal_truthy = false;
        let mut maybe = false;
        for q in intersection_parts(ti, p) {
            match primitive_truth(ti, q) {
                Some(Truth::Falsy) => return Some(Truth::Falsy),
                Some(Truth::Truthy) => literal_truthy = true,
                Some(_) => maybe = true,
                None => {
                    let qf = ti.flags(q);
                    if qf.intersects(TypeFlags::UNION) {
                        match part_truth(ti, q, depth + 1)? {
                            Truth::Falsy => return Some(Truth::Falsy),
                            Truth::Truthy => {}
                            _ => maybe = true,
                        }
                    } else if !qf.intersects(
                        TypeFlags::OBJECT
                            | TypeFlags::NON_PRIMITIVE
                            | TypeFlags::ES_SYMBOL
                            | TypeFlags::UNIQUE_ES_SYMBOL,
                    ) {
                        return None;
                    }
                }
            }
        }
        return Some(if literal_truthy {
            Truth::Truthy
        } else if maybe {
            Truth::Maybe
        } else {
            Truth::Truthy
        });
    }
    if f.contains(TypeFlags::UNION) {
        let mut all_truthy = true;
        let mut all_falsy = true;
        for q in parts(ti, p) {
            match part_truth(ti, q, depth + 1)? {
                Truth::Truthy => all_falsy = false,
                Truth::Falsy => all_truthy = false,
                _ => {
                    all_truthy = false;
                    all_falsy = false;
                }
            }
        }
        return Some(if all_truthy {
            Truth::Truthy
        } else if all_falsy {
            Truth::Falsy
        } else {
            Truth::Maybe
        });
    }
    if let Some(v) = primitive_truth(ti, p) {
        return Some(v);
    }
    if f.intersects(
        TypeFlags::OBJECT
            | TypeFlags::NON_PRIMITIVE
            | TypeFlags::ES_SYMBOL
            | TypeFlags::UNIQUE_ES_SYMBOL,
    ) {
        return Some(Truth::Truthy);
    }
    if f.contains(TypeFlags::NEVER) {
        return Some(Truth::Falsy);
    }
    None
}

/// Truthiness of a primitive or literal type; `None` for anything else.
fn primitive_truth(ti: &dyn TypeInfo, p: Ty) -> Option<Truth> {
    let f = ti.flags(p);
    if f.intersects(NULLISH) {
        return Some(Truth::Falsy);
    }
    if f.intersects(TypeFlags::LITERAL | TypeFlags::ENUM_LITERAL) && !f.contains(TypeFlags::UNION) {
        return Some(match literal(ti, p)? {
            Lit::Str(s) => truthy(!s.is_empty()),
            Lit::Num(v) => truthy(v != 0.0 && !v.is_nan()),
            Lit::Big(b) => truthy(b != "0"),
            Lit::Bool(b) => truthy(b),
            Lit::Null | Lit::Undefined => Truth::Falsy,
        });
    }
    if f.intersects(
        TypeFlags::STRING
            | TypeFlags::NUMBER
            | TypeFlags::BIGINT
            | TypeFlags::BOOLEAN
            | TypeFlags::TEMPLATE_LITERAL
            | TypeFlags::STRING_MAPPING
            | TypeFlags::ENUM,
    ) {
        return Some(Truth::Maybe);
    }
    None
}

fn truthy(b: bool) -> Truth {
    if b { Truth::Truthy } else { Truth::Falsy }
}

/// A literal type's value.
#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Str(String),
    Num(f64),
    /// Normalized decimal digits with an optional leading `-`.
    Big(String),
    Bool(bool),
    Null,
    Undefined,
}

fn literal(ti: &dyn TypeInfo, t: Ty) -> Option<Lit> {
    let f = ti.flags(t);
    if f.contains(TypeFlags::UNION) {
        return None;
    }
    if f.contains(TypeFlags::NULL) {
        return Some(Lit::Null);
    }
    if f.contains(TypeFlags::UNDEFINED) {
        return Some(Lit::Undefined);
    }
    if f.contains(TypeFlags::BOOLEAN_LITERAL) {
        return Some(Lit::Bool(ti.type_to_string(t) == "true"));
    }
    if f.contains(TypeFlags::BIGINT_LITERAL) {
        let s = ti.type_to_string(t);
        let s = s.strip_suffix('n')?;
        let (neg, digits) = match s.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, s),
        };
        let norm = parse_bigint(digits)?;
        return Some(Lit::Big(if neg && norm != "0" {
            format!("-{norm}")
        } else {
            norm
        }));
    }
    if f.intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL | TypeFlags::ENUM_LITERAL)
    {
        let text = ti.literal_text(t)?;
        let is_string = f.contains(TypeFlags::STRING_LITERAL)
            || (!f.contains(TypeFlags::NUMBER_LITERAL)
                && ti
                    .flags(ti.base_of_literal(t))
                    .intersects(TypeFlags::STRING | TypeFlags::STRING_LITERAL));
        return Some(if is_string {
            Lit::Str(text)
        } else {
            Lit::Num(text.parse().ok()?)
        });
    }
    None
}

/// `ToNumber` of a literal (for loose and relational comparisons).
fn to_number(l: &Lit) -> f64 {
    match l {
        Lit::Num(v) => *v,
        Lit::Bool(b) => f64::from(u8::from(*b)),
        Lit::Null => 0.0,
        Lit::Undefined => f64::NAN,
        Lit::Big(b) => b.parse().unwrap_or(f64::NAN),
        Lit::Str(s) => {
            let s = s.trim();
            if s.is_empty() {
                0.0
            } else if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u64::from_str_radix(h, 16).map_or(f64::NAN, |v| v as f64)
            } else if s == "Infinity" || s == "+Infinity" {
                f64::INFINITY
            } else if s == "-Infinity" {
                f64::NEG_INFINITY
            } else if s.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)) {
                s.parse().unwrap_or(f64::NAN)
            } else {
                f64::NAN
            }
        }
    }
}

fn big_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (an, ad) = a.strip_prefix('-').map_or((false, a), |d| (true, d));
    let (bn, bd) = b.strip_prefix('-').map_or((false, b), |d| (true, d));
    let mag = ad.len().cmp(&bd.len()).then_with(|| ad.cmp(bd));
    match (an, bn) {
        (false, false) => mag,
        (true, true) => mag.reverse(),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
    }
}

fn strict_eq(a: &Lit, b: &Lit) -> bool {
    match (a, b) {
        (Lit::Num(x), Lit::Num(y)) => x == y,
        (Lit::Big(x), Lit::Big(y)) => x == y,
        _ => a == b,
    }
}

fn loose_eq(a: &Lit, b: &Lit) -> bool {
    match (a, b) {
        (Lit::Null | Lit::Undefined, Lit::Null | Lit::Undefined) => true,
        (Lit::Null | Lit::Undefined, _) | (_, Lit::Null | Lit::Undefined) => false,
        (Lit::Str(_), Lit::Str(_))
        | (Lit::Num(_), Lit::Num(_))
        | (Lit::Big(_), Lit::Big(_))
        | (Lit::Bool(_), Lit::Bool(_)) => strict_eq(a, b),
        _ => {
            let (x, y) = (to_number(a), to_number(b));
            x == y
        }
    }
}

/// `a < b` style comparison: `Some(ordering)` or `None` when either side is `NaN`.
fn relational(a: &Lit, b: &Lit) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Lit::Str(x), Lit::Str(y)) => {
            let xs: Vec<u16> = x.encode_utf16().collect();
            let ys: Vec<u16> = y.encode_utf16().collect();
            Some(xs.cmp(&ys))
        }
        (Lit::Big(x), Lit::Big(y)) => Some(big_cmp(x, y)),
        _ => to_number(a).partial_cmp(&to_number(b)),
    }
}

fn evaluate(op: BinaryOp, a: &Lit, b: &Lit) -> Option<bool> {
    use std::cmp::Ordering::*;
    Some(match op {
        BinaryOp::StrictEq => strict_eq(a, b),
        BinaryOp::StrictNotEq => !strict_eq(a, b),
        BinaryOp::Eq => loose_eq(a, b),
        BinaryOp::NotEq => !loose_eq(a, b),
        BinaryOp::Lt => relational(a, b) == Some(Less),
        BinaryOp::LtEq => matches!(relational(a, b), Some(Less | Equal)),
        BinaryOp::Gt => relational(a, b) == Some(Greater),
        BinaryOp::GtEq => matches!(relational(a, b), Some(Greater | Equal)),
        _ => return None,
    })
}

fn literal_at(cx: &Ctx<'_, '_>, n: NodeId) -> Option<(Lit, String)> {
    let ti = cx.types();
    let t = ti.type_at(n)?;
    let t = constrained(ti, t);
    if t == Ty::UNRESOLVED {
        return None;
    }
    Some((literal(ti, t)?, ti.type_to_string(t)))
}

fn report_literal_comparison(
    cx: &mut Ctx<'_, '_>,
    at: NodeId,
    op: BinaryOp,
    l: (Lit, String),
    r: (Lit, String),
) {
    let Some(result) = evaluate(op, &l.0, &r.0) else {
        return;
    };
    let msg = format!(
        "Unnecessary conditional, comparison is always {result}, since `{} {} {}` is {result}.",
        l.1,
        op.as_str(),
        r.1
    );
    cx.report_node(at, msg).id("comparisonBetweenLiteralTypes");
}

fn check_comparison(cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, op: BinaryOp, right: NodeId) {
    if let (Some(l), Some(r)) = (literal_at(cx, left), literal_at(cx, right)) {
        report_literal_comparison(cx, node, op, l, r);
        return;
    }
    let strict = match op {
        BinaryOp::StrictEq | BinaryOp::StrictNotEq => true,
        BinaryOp::Eq | BinaryOp::NotEq => false,
        _ => return,
    };
    let ast = cx.ast();
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
    let (other, is_null) = match (kind(left), kind(right)) {
        (None, Some(k)) => (left, k),
        (Some(k), None) => (right, k),
        _ => return,
    };
    let ti = cx.types();
    let Some(t) = ti.type_at(other) else {
        return;
    };
    let Some((has_null, has_undefined)) = nullish_parts(ti, t) else {
        return;
    };
    let overlap = if !strict {
        has_null || has_undefined
    } else if is_null {
        has_null
    } else {
        has_undefined
    };
    if !overlap {
        cx.report_node(node, NO_OVERLAP)
            .id("noOverlapBooleanExpression");
    }
}

/// Whether a type has `null` and `undefined`/`void` members; `None` when it is any, unknown,
/// generic or unresolved.
fn nullish_parts(ti: &dyn TypeInfo, t: Ty) -> Option<(bool, bool)> {
    let t = constrained(ti, t);
    if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
        return None;
    }
    let mut null = false;
    let mut undefined = false;
    for p in parts(ti, t) {
        let p = constrained(ti, p);
        if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
            return None;
        }
        let f = ti.flags(p);
        if f.intersects(TypeFlags::INSTANTIABLE) {
            return None;
        }
        null |= f.contains(TypeFlags::NULL);
        undefined |= f.intersects(TypeFlags::UNDEFINED | TypeFlags::VOID);
    }
    Some((null, undefined))
}

fn check_switch(cx: &mut Ctx<'_, '_>, disc: NodeId, cases: ts_r_ast::NodeList) {
    let Some(d) = literal_at(cx, disc) else {
        return;
    };
    let ast = cx.ast();
    for &c in ast.list(cases) {
        let Node::SwitchCase {
            test: Some(test), ..
        } = *cx.node(c)
        else {
            continue;
        };
        if let Some(r) = literal_at(cx, test) {
            report_literal_comparison(cx, test, BinaryOp::StrictEq, d.clone(), r);
        }
    }
}

fn check_nullish(cx: &mut Ctx<'_, '_>, left: NodeId) {
    let ast = cx.ast();
    let inner = skip_parens(ast, left);
    let array_index = chain_has_array_index(cx, inner) || unnamed_index_read(cx, inner);
    let ti = cx.types();
    let Some(t) = ti.type_at(inner) else {
        return;
    };
    let c = constrained(ti, t);
    if c != Ty::UNRESOLVED && ti.flags(c).contains(TypeFlags::NEVER) {
        cx.report_node(left, NEVER).id("never");
        return;
    }
    let Some(ps) = possibly_nullish_parts(ti, t) else {
        return;
    };
    let (nullish, total) = ps;
    if nullish == 0 && !array_index {
        cx.report_node(left, NEVER_NULLISH).id("neverNullish");
    } else if nullish == total {
        cx.report_node(left, ALWAYS_NULLISH).id("alwaysNullish");
    }
}

/// (nullish members, all members); `None` when the type says nothing.
fn possibly_nullish_parts(ti: &dyn TypeInfo, t: Ty) -> Option<(usize, usize)> {
    let t = constrained(ti, t);
    if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
        return None;
    }
    let ps = parts(ti, t);
    let mut nullish = 0;
    for &p in &ps {
        let p = constrained(ti, p);
        if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
            return None;
        }
        let f = ti.flags(p);
        if f.intersects(TypeFlags::INSTANTIABLE) {
            return None;
        }
        if f.intersects(NULLISH) {
            nullish += 1;
        }
    }
    Some((nullish, ps.len()))
}

/// `arr[i]` on an array (or a tuple with a computed index): its type ignores out-of-bounds
/// reads unless `noUncheckedIndexedAccess` is on.
fn is_array_index(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ti = cx.types();
    if ti.option("noUncheckedIndexedAccess") == Some(true) {
        return false;
    }
    let ast = cx.ast();
    let Node::Index { obj, index, .. } = *ast.node(skip_parens(ast, n)) else {
        return false;
    };
    let Some(t) = link_type(cx, obj, 0) else {
        return false;
    };
    let t = ti.non_nullable(constrained(ti, t));
    let literal_index = matches!(ast.node(skip_parens(ast, index)), Node::NumberLit { .. });
    parts(ti, t)
        .into_iter()
        .any(|p| ti.is_array(p) || (ti.is_tuple(p) && !literal_index))
}

/// The value of an optional-chain link without the `undefined` that short-circuiting adds
/// (`a?.b` in `a?.b?.c` has the type of `b`, not `b | undefined`).
fn link_type(cx: &Ctx<'_, '_>, n: NodeId, depth: u32) -> Option<Ty> {
    let ti = cx.types();
    let ast = cx.ast();
    if depth > MAX_DEPTH || !short_circuits(ast, n) {
        return ti.type_at(n);
    }
    match *ast.node(n) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            let base = link_type(cx, obj, depth + 1)?;
            let base = ti.non_nullable(constrained(ti, base));
            let keys = member_keys(cx, n)?;
            let mut out = Vec::new();
            for p in parts(ti, base) {
                for k in &keys {
                    out.push(property_type(ti, p, k)?);
                }
            }
            Some(ti.union_of(&out))
        }
        Node::Call { .. } => {
            let sig = ti.resolved_signature(n)?;
            Some(ti.return_type(sig))
        }
        Node::NonNull { expr } => {
            let t = link_type(cx, expr, depth + 1)?;
            Some(ti.non_nullable(t))
        }
        _ => ti.type_at(n),
    }
}

/// Some link below `n` (in the same chain) is optional.
fn short_circuits(ast: &Ast, mut n: NodeId) -> bool {
    for _ in 0..10_000 {
        match *ast.node(n) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                n = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                n = callee;
            }
            Node::NonNull { expr } => n = expr,
            _ => return false,
        }
    }
    false
}

/// The property names `a.b` / `a['b']` / `a[k]` can read (`None` for a non-literal key).
fn member_keys(cx: &Ctx<'_, '_>, n: NodeId) -> Option<Vec<String>> {
    let ast = cx.ast();
    if let Some(name) = member_name(ast, cx.src(), n) {
        return Some(vec![name.into_owned()]);
    }
    let Node::Index { index, .. } = *ast.node(n) else {
        return None;
    };
    let ti = cx.types();
    let t = constrained(ti, ti.type_at(index)?);
    let mut keys = Vec::new();
    for p in parts(ti, t) {
        if !ti
            .flags(p)
            .intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
        {
            return None;
        }
        keys.push(ti.literal_text(p)?);
    }
    Some(keys)
}

fn property_type(ti: &dyn TypeInfo, t: Ty, key: &str) -> Option<Ty> {
    if let Some(s) = ti.property_of_type(t, key) {
        return Some(ti.type_of_symbol(s));
    }
    let numeric = key.parse::<f64>().is_ok();
    if numeric && let Some(v) = ti.index_type(t, true) {
        return Some(v);
    }
    ti.index_type(t, false)
}

/// `o[k]` the left side of `??` may be missing at run time (observed): the key is not a
/// literal (`o[key]` with `key: string`), or it is a literal that names no property of `o`
/// while `o` has a `string` index signature (`record['zz']`). A numeric literal through a
/// `number` index signature alone (`byNumber[1]`) and dot access (`record.zz`) are not.
fn unnamed_index_read(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    let n = skip_parens(ast, n);
    let Node::Index { obj, index, .. } = *ast.node(n) else {
        return false;
    };
    let ti = cx.types();
    let Some(kt) = ti.type_at(index) else {
        return false;
    };
    // An `any` or error-typed key reads the index signature like any other value (observed:
    // `record[anyKey] ?? x` is reported).
    if kt == Ty::UNRESOLVED || any_kind(ti, kt).is_some() {
        return false;
    }
    let Some(keys) = member_keys(cx, n) else {
        return true;
    };
    let Some(base) = link_type(cx, obj, 0) else {
        return false;
    };
    let base = ti.non_nullable(constrained(ti, base));
    keys.iter().any(|k| {
        ti.property_of_type(base, k).is_none()
            && parts(ti, base)
                .into_iter()
                .any(|p| ti.index_type(p, false).is_some())
    })
}

/// `a?.[k]` (an element access inside an optional chain) reads through a `string`/`number`
/// index signature (or with a key the types cannot name), so the value may be missing at
/// run time. Outside a chain, and for dot access, typescript-eslint reports such reads,
/// unless `noUncheckedIndexedAccess` adds `undefined` to every index signature read.
fn via_index_signature(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    let n = skip_parens(ast, n);
    let unchecked = cx.types().option("noUncheckedIndexedAccess") == Some(true);
    let obj = match *ast.node(n) {
        Node::Member { obj, .. } if unchecked => obj,
        Node::Index { obj, .. } if unchecked || short_circuits(ast, n) => obj,
        _ => return false,
    };
    let Some(keys) = member_keys(cx, n) else {
        return true;
    };
    let ti = cx.types();
    let Some(base) = link_type(cx, obj, 0) else {
        return false;
    };
    let base = ti.non_nullable(constrained(ti, base));
    for p in parts(ti, base) {
        for k in &keys {
            if ti.property_of_type(p, k).is_some() {
                continue;
            }
            let numeric = k.parse::<f64>().is_ok();
            if ti.index_type(p, false).is_some() || (numeric && ti.index_type(p, true).is_some()) {
                return true;
            }
        }
    }
    false
}

/// Some `arr[i]` link of the chain below `n` is an unchecked array read.
fn chain_has_array_index(cx: &Ctx<'_, '_>, mut n: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..10_000 {
        n = skip_parens(ast, n);
        if is_array_index(cx, n) {
            return true;
        }
        match *ast.node(n) {
            Node::Member {
                obj,
                optional: true,
                ..
            }
            | Node::Index {
                obj,
                optional: true,
                ..
            } => n = obj,
            Node::Call {
                callee,
                optional: true,
                ..
            } => n = callee,
            _ => return false,
        }
    }
    false
}

fn check_optional_chain(cx: &mut Ctx<'_, '_>, node: NodeId, obj: NodeId) {
    if chain_has_array_index(cx, obj) || via_index_signature(cx, obj) {
        return;
    }
    let ti = cx.types();
    let Some(t) = link_type(cx, skip_parens(cx.ast(), obj), 0) else {
        return;
    };
    let Some((null, undefined)) = nullish_parts(ti, t) else {
        return;
    };
    if null || undefined {
        return;
    }
    let src = cx.src();
    let start = skip_trivia_forward(src, cx.span(obj).end as usize) as u32;
    if src.get(start as usize..start as usize + 2) != Some("?.") {
        return;
    }
    let op = Span::new(start, start + 2);
    let fix = match cx.node(node) {
        Node::Member { .. } => Fix::replace(op, "."),
        _ => Fix::delete(op),
    };
    cx.report(op, NEVER_OPTIONAL_CHAIN)
        .id("neverOptionalChain")
        .suggest(
            "suggestRemoveOptionalChain",
            "Remove unnecessary optional chain",
            fix,
        );
}

/// With `checkTypePredicates`: `assert(x)` on a value whose truthiness is known, or a type
/// guard on a value that already has the guarded type.
fn check_type_predicate(cx: &mut Ctx<'_, '_>, call: NodeId) {
    let ti = cx.types();
    let ast = cx.ast();
    let Node::Call { args, .. } = *cx.node(call) else {
        return;
    };
    let Some(sig) = ti.resolved_signature(call) else {
        return;
    };
    let Some(pred) = ti.type_predicate(sig) else {
        return;
    };
    if pred.this {
        return;
    }
    let args = ast.list(args);
    if args
        .iter()
        .take(pred.param_index + 1)
        .any(|&a| matches!(cx.node(a), Node::Spread { .. }))
    {
        return;
    }
    let Some(&arg) = args.get(pred.param_index) else {
        return;
    };
    let Some(pt) = pred.ty else {
        if pred.asserts {
            NoUnnecessaryCondition {
                loops: LoopMode::Never,
                check_predicates: true,
            }
            .check_condition(cx, arg, 0, true);
        }
        return;
    };
    let Some(at) = ti.type_at(arg) else {
        return;
    };
    let at = constrained(ti, at);
    if at == Ty::UNRESOLVED
        || has_unresolved_part(ti, at)
        || has_unresolved_part(ti, pt)
        || is_any(ti, at)
        || is_unknown(ti, at)
        || ti.flags(at).intersects(TypeFlags::INSTANTIABLE)
        || matches!(
            cx.node(skip_parens(ast, arg)),
            Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::TemplateLit { .. }
        )
    {
        return;
    }
    if ti.is_assignable(at, pt) {
        let msg = if pred.asserts {
            GUARD_ASSERTION
        } else {
            GUARD_PREDICATE
        };
        cx.report_node(arg, msg).id("typeGuardAlreadyIsType");
    }
}
