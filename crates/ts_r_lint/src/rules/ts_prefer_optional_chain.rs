//! `@typescript-eslint/prefer-optional-chain`: `foo?.bar?.baz` over `foo && foo.bar &&
//! foo.bar.baz`, `!foo?.bar` over `!foo || !foo.bar`, and `foo?.bar` over `(foo || {}).bar`.
//!
//! A logical expression is flattened into its `&&` (or `||`) operands. Each operand is a
//! *guard* on some expression (`x`, `x != null`, `x !== undefined`, `typeof x !==
//! 'undefined'`, `!x` and their `||` mirrors) or a final comparison (`x.y === 1`). A chain
//! is a run of operands where each guarded expression extends the previous one by member
//! accesses or calls. Chains of two or more operands are reported; the fix rewrites the
//! chain as one optional chain. When the rewrite can change the value the expression
//! produces, it is offered as a suggestion instead of a fix.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-optional-chain",
        "Enforce using concise optional chain expressions instead of chained logical ands, negated logical ors, or empty objects",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .fixable()
    .suggestions(),
    create: create::<PreferOptionalChain>,
};

const MSG: &str =
    "Prefer using an optional chain expression instead, as it's more concise and easier to read.";
const SUGGEST: &str = "Change to an optional chain.";

#[derive(Debug)]
struct PreferOptionalChain {
    check_any: bool,
    check_unknown: bool,
    check_string: bool,
    check_number: bool,
    check_boolean: bool,
    check_bigint: bool,
    require_nullish: bool,
    allow_unsafe: bool,
}

impl FromOptions for PreferOptionalChain {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferOptionalChain {
            check_any: bool_field(o, "checkAny", true),
            check_unknown: bool_field(o, "checkUnknown", true),
            check_string: bool_field(o, "checkString", true),
            check_number: bool_field(o, "checkNumber", true),
            check_boolean: bool_field(o, "checkBoolean", true),
            check_bigint: bool_field(o, "checkBigInt", true),
            require_nullish: bool_field(o, "requireNullish", false),
            allow_unsafe: bool_field(
                o,
                "allowPotentiallyUnsafeFixesThatModifyTheReturnTypeIKnowWhatImDoing",
                false,
            ),
        })
    }
}

impl Rule for PreferOptionalChain {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Member, NodeKind::Index]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Binary {
                op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
                ..
            } => {
                if !is_nested(cx, node, op) {
                    self.check_logical(cx, node, op == BinaryOp::LogicalAnd);
                }
            }
            Node::Member {
                obj,
                optional: false,
                prop,
            } => self.check_empty_object(cx, node, obj, prop, false),
            Node::Index {
                obj,
                optional: false,
                index,
            } => self.check_empty_object(cx, node, obj, index, true),
            _ => {}
        }
    }
}

/// True when `node` is an operand of an enclosing logical expression with the same operator
/// (the outermost one handles the whole run).
fn is_nested(cx: &Ctx<'_, '_>, node: NodeId, op: BinaryOp) -> bool {
    let mut p = cx.parent(node);
    while let Some(id) = p {
        match *cx.node(id) {
            Node::Paren { .. } => p = cx.parent(id),
            Node::Binary { op: o, .. } => return o == op,
            _ => return false,
        }
    }
    false
}

/// What an operand checks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// `x` (in `&&`).
    Truthy,
    /// `!x` (in `||`).
    Negated,
    /// `x != null` / `x != undefined`.
    NotEqNullish,
    /// `x == null` / `x == undefined`.
    EqNullish,
    /// `x !== null`.
    NotStrictNull,
    /// `x === null`.
    StrictNull,
    /// `x !== undefined` / `typeof x !== 'undefined'`.
    NotStrictUndef,
    /// `x === undefined` / `typeof x === 'undefined'`.
    StrictUndef,
    /// A comparison that ends the chain (`x.y === 1`).
    Terminal,
}

#[derive(Clone, Copy, Debug)]
struct Elem {
    /// The operand as flattened (may be parenthesized).
    node: NodeId,
    /// The operand without parentheses.
    inner: NodeId,
    /// The guarded expression.
    expr: NodeId,
    kind: Kind,
    /// Joined with the next operand into a full nullish check (`x !== null && x !==
    /// undefined`).
    paired: bool,
}

/// A raw operand before validation.
struct Raw {
    inner: NodeId,
    expr: Option<NodeId>,
    kind: Option<Kind>,
    typeof_form: bool,
    /// Both sides when the operand is an equality comparison.
    cmp: Option<(NodeId, BinaryOp, NodeId)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rel {
    Equal,
    Subset,
    Invalid,
}

fn flatten(ast: &Ast, id: NodeId, op: BinaryOp, out: &mut Vec<NodeId>, depth: u32) {
    let inner = skip_parens(ast, id);
    if depth < 200
        && let Node::Binary { left, op: o, right } = *ast.node(inner)
        && o == op
    {
        flatten(ast, left, op, out, depth + 1);
        flatten(ast, right, op, out, depth + 1);
        return;
    }
    out.push(id);
}

fn is_null_lit(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::NullLit {})
}

fn classify(ast: &Ast, operand: NodeId) -> Raw {
    let inner = skip_parens(ast, operand);
    let mut raw = Raw {
        inner,
        expr: None,
        kind: None,
        typeof_form: false,
        cmp: None,
    };
    match *ast.node(inner) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => {
            raw.expr = Some(skip_parens(ast, arg));
            raw.kind = Some(Kind::Negated);
        }
        Node::Binary { left, op, right }
            if matches!(
                op,
                BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
            ) =>
        {
            let (l, r) = (skip_parens(ast, left), skip_parens(ast, right));
            raw.cmp = Some((l, op, r));
            let nullish = |x: NodeId| is_null_lit(ast, x) || is_undefined(ast, x);
            let (x, lit) = if nullish(r) && !nullish(l) {
                (l, r)
            } else if nullish(l) && !nullish(r) {
                (r, l)
            } else {
                // `typeof x === 'undefined'`
                let ty = |a: NodeId, b: NodeId| match (ast.node(a), ast.node(b)) {
                    (
                        Node::Unary {
                            op: UnaryOp::TypeOf,
                            arg,
                        },
                        Node::StringLit { value },
                    ) => Some((skip_parens(ast, *arg), ast.atom(*value) == "undefined")),
                    _ => None,
                };
                if let Some((x, is_undef)) = ty(l, r).or_else(|| ty(r, l)) {
                    raw.cmp = None;
                    if is_undef {
                        raw.expr = Some(x);
                        raw.typeof_form = true;
                        raw.kind = Some(match op {
                            BinaryOp::StrictNotEq | BinaryOp::NotEq => Kind::NotStrictUndef,
                            _ => Kind::StrictUndef,
                        });
                    }
                    return raw;
                }
                raw.kind = Some(Kind::Terminal);
                return raw;
            };
            let null = is_null_lit(ast, lit);
            raw.expr = Some(x);
            raw.kind = Some(match op {
                BinaryOp::NotEq => Kind::NotEqNullish,
                BinaryOp::Eq => Kind::EqNullish,
                BinaryOp::StrictNotEq if null => Kind::NotStrictNull,
                BinaryOp::StrictNotEq => Kind::NotStrictUndef,
                _ if null => Kind::StrictNull,
                _ => Kind::StrictUndef,
            });
        }
        _ => {
            raw.expr = Some(inner);
            raw.kind = Some(Kind::Truthy);
        }
    }
    raw
}

/// Splits a member/call chain into its root and its accessors (innermost first).
fn decompose(ast: &Ast, mut e: NodeId) -> (NodeId, Vec<NodeId>) {
    let mut segs = Vec::new();
    loop {
        match *ast.node(e) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                segs.push(e);
                e = obj;
            }
            Node::Call { callee, .. } => {
                segs.push(e);
                e = callee;
            }
            Node::NonNull { expr } => {
                segs.push(e);
                e = expr;
            }
            _ => break,
        }
        if segs.len() > 500 {
            break;
        }
    }
    segs.reverse();
    (e, segs)
}

fn is_non_null(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::NonNull { .. })
}

/// True for expressions that can be the subject of an optional chain.
fn is_chain_expr(ast: &Ast, e: NodeId) -> bool {
    let (root, segs) = decompose(ast, e);
    let real = segs.iter().filter(|&&s| !is_non_null(ast, s)).count();
    let root_ok = match ast.node(root) {
        Node::Ident { .. } | Node::MetaProperty { .. } => true,
        Node::This {} | Node::Super {} | Node::Paren { .. } => real > 0,
        _ => false,
    };
    root_ok
        && segs.iter().all(|&s| match *ast.node(s) {
            Node::Member { prop, .. } => !matches!(ast.node(prop), Node::PrivateName { .. }),
            _ => true,
        })
}

/// Expressions that never compare equal to another occurrence (side effects or fresh
/// values).
fn has_unstable(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return true;
    }
    match ast.kind(id) {
        NodeKind::Update
        | NodeKind::Assign
        | NodeKind::Yield
        | NodeKind::New
        | NodeKind::Arrow
        | NodeKind::FunctionExpr
        | NodeKind::ClassExpr
        | NodeKind::ObjectLit
        | NodeKind::JsxElement
        | NodeKind::JsxFragment => return true,
        _ => {}
    }
    let mut bad = false;
    ast.for_each_child(id, |c| {
        if !bad && has_unstable(ast, c, depth + 1) {
            bad = true;
        }
    });
    bad
}

/// True when `id` contains an expression equal to `target`.
fn mentions(ast: &Ast, id: NodeId, target: NodeId, depth: u32) -> bool {
    if depth > 200 || expr_equal(ast, id, target) {
        return true;
    }
    let mut hit = false;
    ast.for_each_child(id, |c| {
        if !hit && mentions(ast, c, target, depth + 1) {
            hit = true;
        }
    });
    hit
}

fn expr_equal(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    same_tree(ast, a, b) && !has_unstable(ast, a, 0)
}

fn type_args_equal(ast: &Ast, src: &str, a: Option<NodeId>, b: Option<NodeId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            let t = |n: NodeId| -> String {
                ast.span(n)
                    .text(src)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect()
            };
            t(x) == t(y)
        }
        _ => false,
    }
}

fn seg_equal(ast: &Ast, src: &str, a: NodeId, b: NodeId) -> bool {
    match (*ast.node(a), *ast.node(b)) {
        (Node::Member { prop: p, .. }, Node::Member { prop: q, .. }) => same_tree(ast, p, q),
        (Node::Index { index: i, .. }, Node::Index { index: j, .. }) => expr_equal(ast, i, j),
        (
            Node::Call {
                type_args: ta,
                args: aa,
                ..
            },
            Node::Call {
                type_args: tb,
                args: ab,
                ..
            },
        ) => {
            let (aa, ab) = (ast.list(aa), ast.list(ab));
            aa.len() == ab.len()
                && aa.iter().zip(ab).all(|(&x, &y)| expr_equal(ast, x, y))
                && type_args_equal(ast, src, ta, tb)
        }
        _ => false,
    }
}

/// How `b` relates to `a`: the same expression, `a` followed by more accessors, or neither.
fn compare(ast: &Ast, src: &str, a: NodeId, b: NodeId) -> Rel {
    let (ra, sa) = decompose(ast, a);
    let (rb, sb) = decompose(ast, b);
    if !expr_equal(ast, ra, rb)
        && !matches!(
            (ast.node(ra), ast.node(rb)),
            (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {})
        )
    {
        return Rel::Invalid;
    }
    let sa: Vec<NodeId> = sa.into_iter().filter(|&s| !is_non_null(ast, s)).collect();
    let sb: Vec<NodeId> = sb.into_iter().filter(|&s| !is_non_null(ast, s)).collect();
    if sb.len() < sa.len() {
        return Rel::Invalid;
    }
    if !sa.iter().zip(&sb).all(|(&x, &y)| seg_equal(ast, src, x, y)) {
        return Rel::Invalid;
    }
    if sb.len() == sa.len() {
        Rel::Equal
    } else {
        Rel::Subset
    }
}

/// The constrained type of `e` without the `undefined` an optional chain adds.
fn non_optional_type(ti: &dyn TypeInfo, e: NodeId) -> Option<Ty> {
    ti.non_optional_type_at(e).map(|t| constrained(ti, t))
}

fn flags_any(ti: &dyn TypeInfo, t: Ty, f: TypeFlags) -> bool {
    parts(ti, t).iter().any(|&p| ti.flags(p).intersects(f))
}

/// The value of `undefined <op> v`, when the type of `v` decides it.
fn undefined_compared(ti: &dyn TypeInfo, v: NodeId, op: BinaryOp) -> Option<bool> {
    let t = constrained_type_at(ti, v)?;
    let ps = parts(ti, t);
    let unsure = TypeFlags::ANY
        | TypeFlags::UNKNOWN
        | TypeFlags::VOID
        | TypeFlags::TYPE_PARAMETER
        | TypeFlags::INDEXED_ACCESS
        | TypeFlags::CONDITIONAL
        | TypeFlags::SUBSTITUTION
        | TypeFlags::INDEX;
    let mut has_undef = false;
    let mut has_null = false;
    let mut all_undef = true;
    let mut all_nullish = true;
    for &p in &ps {
        let f = ti.flags(p);
        if f.intersects(unsure) {
            return None;
        }
        let u = f.contains(TypeFlags::UNDEFINED);
        let n = f.contains(TypeFlags::NULL);
        has_undef |= u;
        has_null |= n;
        all_undef &= u;
        all_nullish &= u || n;
    }
    let strict = if !has_undef {
        Some(false)
    } else if all_undef {
        Some(true)
    } else {
        None
    };
    let loose = if !has_undef && !has_null {
        Some(false)
    } else if all_nullish {
        Some(true)
    } else {
        None
    };
    match op {
        BinaryOp::StrictEq => strict,
        BinaryOp::StrictNotEq => strict.map(|b| !b),
        BinaryOp::Eq => loose,
        BinaryOp::NotEq => loose.map(|b| !b),
        _ => None,
    }
}

impl PreferOptionalChain {
    /// Whether a truthiness check on `expr` can become part of an optional chain: no falsy
    /// literal other than `null`/`undefined`, and only kinds the `check*` options allow.
    fn truthy_ok(&self, ti: &dyn TypeInfo, expr: NodeId) -> bool {
        let Some(t) = non_optional_type(ti, expr) else {
            return true;
        };
        let ps = parts(ti, t);
        let is_bool_lit = |p: Ty, v: &str| {
            ti.flags(p).contains(TypeFlags::BOOLEAN_LITERAL) && ti.type_to_string(p) == v
        };
        let has_true = ps.iter().any(|&p| is_bool_lit(p, "true"));
        ps.iter().all(|&p| {
            let f = ti.flags(p);
            let falsy = (is_bool_lit(p, "false") && !has_true)
                || (f.contains(TypeFlags::STRING_LITERAL)
                    && ti.literal_text(p).is_some_and(|s| s.is_empty()))
                || (f.contains(TypeFlags::NUMBER_LITERAL)
                    && ti
                        .literal_text(p)
                        .and_then(|s| s.parse::<f64>().ok())
                        .is_some_and(|v| v == 0.0))
                || (f.contains(TypeFlags::BIGINT_LITERAL) && ti.type_to_string(p) == "0n");
            !falsy && self.kind_allowed(ti, p, 0)
        })
    }

    fn kind_allowed(&self, ti: &dyn TypeInfo, p: Ty, depth: u32) -> bool {
        let f = ti.flags(p);
        if f.contains(TypeFlags::INTERSECTION) {
            return depth < 8
                && intersection_parts(ti, p)
                    .iter()
                    .all(|&m| self.kind_allowed(ti, m, depth + 1));
        }
        if f.intersects(
            TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::OBJECT | TypeFlags::NON_PRIMITIVE,
        ) {
            true
        } else if f.contains(TypeFlags::ANY) {
            self.check_any
        } else if f.contains(TypeFlags::UNKNOWN) {
            self.check_unknown
        } else if f.intersects(
            TypeFlags::STRING_LIKE | TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING,
        ) {
            self.check_string
        } else if f.intersects(TypeFlags::NUMBER_LIKE | TypeFlags::ENUM_LIKE) {
            self.check_number
        } else if f.intersects(TypeFlags::BOOLEAN_LIKE) {
            self.check_boolean
        } else if f.intersects(TypeFlags::BIGINT_LIKE) {
            self.check_bigint
        } else {
            false
        }
    }

    fn check_logical(&self, cx: &mut Ctx<'_, '_>, node: NodeId, and: bool) {
        let ast = cx.ast();
        let src = cx.src();
        let ti = cx.types();
        let op = if and {
            BinaryOp::LogicalAnd
        } else {
            BinaryOp::LogicalOr
        };
        let mut ops = Vec::new();
        flatten(ast, node, op, &mut ops, 0);
        let raws: Vec<Raw> = ops.iter().map(|&o| classify(ast, o)).collect();
        let n = ops.len();
        let mut chain: Vec<Elem> = Vec::new();
        let mut found: Vec<Vec<Elem>> = Vec::new();
        let finish = |chain: &mut Vec<Elem>, found: &mut Vec<Vec<Elem>>| {
            if and
                && chain
                    .last()
                    .is_some_and(|e| e.kind == Kind::NotStrictNull && !e.paired)
            {
                chain.pop();
            }
            if chain.len() >= 2 {
                found.push(std::mem::take(chain));
            } else {
                chain.clear();
            }
        };
        let mut i = 0;
        while i < n {
            let raw = &raws[i];
            let mut consumed = 1;
            let mut kind = raw.kind;
            let mut paired = false;
            let expr_ok = raw.expr.is_some_and(|e| is_chain_expr(ast, e));
            match (and, kind) {
                (true, Some(Kind::Truthy)) | (false, Some(Kind::Negated)) => {
                    let e = raw.expr.unwrap_or(raw.inner);
                    if !expr_ok || (i + 1 < n && !self.truthy_ok(ti, e)) {
                        kind = None;
                    }
                }
                (true, Some(Kind::NotEqNullish)) | (false, Some(Kind::EqNullish)) => {
                    if !expr_ok {
                        kind = None;
                    }
                }
                (true, Some(k @ (Kind::NotStrictNull | Kind::NotStrictUndef)))
                | (false, Some(k @ (Kind::StrictNull | Kind::StrictUndef))) => {
                    let complement = match k {
                        Kind::NotStrictNull => Kind::NotStrictUndef,
                        Kind::NotStrictUndef => Kind::NotStrictNull,
                        Kind::StrictNull => Kind::StrictUndef,
                        _ => Kind::StrictNull,
                    };
                    let x = raw.expr.unwrap_or(raw.inner);
                    if !expr_ok {
                        kind = None;
                    } else if let Some(next) = raws.get(i + 1)
                        && next.kind == Some(complement)
                        && next
                            .expr
                            .is_some_and(|y| compare(ast, src, x, y) == Rel::Equal)
                    {
                        paired = true;
                        consumed = 2;
                    } else if raw.typeof_form {
                        if let Node::Ident { .. } = ast.node(x) {
                            let sem = cx.semantic();
                            let local = sem
                                .reference_at(x)
                                .map(|r| sem.reference(r).resolution)
                                .is_some_and(|r| matches!(r, Resolution::Symbol(_)));
                            if !local {
                                kind = None;
                            }
                        }
                    } else {
                        let other = if matches!(k, Kind::NotStrictNull | Kind::StrictNull) {
                            TypeFlags::UNDEFINED
                        } else {
                            TypeFlags::NULL
                        };
                        let bad = non_optional_type(ti, x).is_none_or(|t| {
                            flags_any(ti, t, other | TypeFlags::ANY | TypeFlags::UNKNOWN)
                        });
                        if bad {
                            kind = if raw.cmp.is_some() {
                                Some(Kind::Terminal)
                            } else {
                                None
                            };
                        }
                    }
                }
                (_, Some(Kind::Terminal)) => {}
                (_, Some(_)) => {
                    kind = if raw.cmp.is_some() && !raw.typeof_form {
                        Some(Kind::Terminal)
                    } else {
                        None
                    };
                }
                (_, None) => {}
            }
            let Some(kind) = kind else {
                finish(&mut chain, &mut found);
                i += 1;
                continue;
            };
            if kind == Kind::Terminal {
                let Some(last) = chain.last().copied() else {
                    i += 1;
                    continue;
                };
                let Some((l, cop, r)) = raw.cmp else {
                    finish(&mut chain, &mut found);
                    i += 1;
                    continue;
                };
                let rel_l = if is_chain_expr(ast, l) {
                    compare(ast, src, last.expr, l)
                } else {
                    Rel::Invalid
                };
                let rel_r = if is_chain_expr(ast, r) {
                    compare(ast, src, last.expr, r)
                } else {
                    Rel::Invalid
                };
                let pick = if rel_l == Rel::Subset {
                    Some((l, r))
                } else if rel_r == Rel::Subset {
                    Some((r, l))
                } else {
                    None
                };
                let root = decompose(ast, chain[0].expr).0;
                match pick {
                    Some((x, v))
                        if undefined_compared(ti, v, cop) == Some(!and)
                            && !mentions(ast, v, root, 0) =>
                    {
                        chain.push(Elem {
                            node: ops[i],
                            inner: raw.inner,
                            expr: x,
                            kind,
                            paired: false,
                        });
                        finish(&mut chain, &mut found);
                    }
                    _ if rel_l == Rel::Equal || rel_r == Rel::Equal => {}
                    _ => finish(&mut chain, &mut found),
                }
                i += 1;
                continue;
            }
            let elem = Elem {
                node: ops[i],
                inner: raw.inner,
                expr: raw.expr.unwrap_or(raw.inner),
                kind,
                paired,
            };
            match chain.last() {
                None => chain.push(elem),
                Some(last) => match compare(ast, src, last.expr, elem.expr) {
                    Rel::Subset => chain.push(elem),
                    Rel::Equal => {}
                    Rel::Invalid => {
                        finish(&mut chain, &mut found);
                        chain.push(elem);
                    }
                },
            }
            i += consumed;
        }
        finish(&mut chain, &mut found);
        for c in found {
            self.report_chain(cx, node, &c);
        }
    }

    fn report_chain(&self, cx: &mut Ctx<'_, '_>, top: NodeId, chain: &[Elem]) {
        let ast = cx.ast();
        let src = cx.src();
        let ti = cx.types();
        let (Some(first), Some(last)) = (chain.first(), chain.last()) else {
            return;
        };
        if self.require_nullish
            && !chain[..chain.len() - 1].iter().any(|e| {
                constrained_type_at(ti, e.expr)
                    .is_some_and(|t| flags_any(ti, t, TypeFlags::NULL | TypeFlags::UNDEFINED))
            })
        {
            return;
        }
        let fixable = self.allow_unsafe
            || match last.kind {
                Kind::NotEqNullish
                | Kind::EqNullish
                | Kind::NotStrictUndef
                | Kind::StrictUndef
                | Kind::Negated => true,
                Kind::Terminal => false,
                _ => chain.iter().any(|e| {
                    e.kind == Kind::Truthy
                        && non_optional_type(ti, e.expr).is_some_and(|t| {
                            flags_any(
                                ti,
                                t,
                                TypeFlags::UNDEFINED | TypeFlags::ANY | TypeFlags::UNKNOWN,
                            )
                        })
                }),
            };
        let code = chain_code(ast, src, chain);
        let tail = if last.kind == Kind::Truthy {
            code
        } else {
            let (is, ie) = (ast.span(last.inner), ast.span(last.expr));
            format!(
                "{}{}{}",
                &src[is.start as usize..ie.start as usize],
                code,
                &src[ie.end as usize..is.end as usize]
            )
        };
        // The replaced range, grown over parentheses that only wrap the ends of the chain.
        let mut start = ast.span(first.node).start;
        let mut end = ast.span(last.node).end;
        let parens_of = |id: NodeId| -> Vec<Span> {
            let mut v = Vec::new();
            let mut p = cx.parent(id);
            while let Some(x) = p {
                if x == top {
                    break;
                }
                if matches!(ast.node(x), Node::Paren { .. }) {
                    v.push(ast.span(x));
                }
                p = cx.parent(x);
            }
            v
        };
        let first_parens = parens_of(first.node);
        let last_parens = parens_of(last.node);
        for p in &first_parens {
            if p.start < start && src[p.start as usize + 1..start as usize].trim().is_empty() {
                start = p.start;
            } else {
                break;
            }
        }
        for p in &last_parens {
            if p.end > end
                && p.end >= 1
                && skip_trivia_forward(src, end as usize) >= p.end as usize - 1
            {
                end = p.end;
            } else {
                break;
            }
        }
        let range = Span::new(start, end);
        let mut fix = Fix::replace(range, tail);
        for p in &first_parens {
            if p.start < start && p.end > start && p.end <= end {
                fix = fix.and(Span::new(p.start, p.start + 1), "");
            }
        }
        for p in &last_parens {
            if p.start >= start && p.start < end && p.end > end {
                fix = fix.and(Span::new(p.end - 1, p.end), "");
            }
        }
        let r = cx.report(range, MSG).id("preferOptionalChain");
        if fixable {
            r.fix(fix);
        } else {
            r.suggest("optionalChainSuggest", SUGGEST, fix);
        }
    }

    /// `(foo || {}).bar` and `(foo ?? {}).bar`.
    fn check_empty_object(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        obj: NodeId,
        key: NodeId,
        computed: bool,
    ) {
        let ast = cx.ast();
        if !matches!(ast.node(obj), Node::Paren { .. }) {
            return;
        }
        let Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } = *ast.node(skip_parens(ast, obj))
        else {
            return;
        };
        let Node::ObjectLit { props } = *ast.node(skip_parens(ast, right)) else {
            return;
        };
        if !ast.list(props).is_empty() {
            return;
        }
        if self.require_nullish {
            let ti = cx.types();
            let nullable = constrained_type_at(ti, left)
                .is_some_and(|t| flags_any(ti, t, TypeFlags::NULL | TypeFlags::UNDEFINED));
            if !nullable {
                return;
            }
        }
        let left_text = cx.text(left);
        let base = if precedence(ast, left) < 18 {
            format!("({left_text})")
        } else {
            left_text.to_owned()
        };
        let text = if computed {
            format!("{base}?.[{}]", cx.text(key))
        } else {
            format!("{base}?.{}", cx.text(key))
        };
        let span = cx.span(node);
        cx.report(span, MSG).id("preferOptionalChain").suggest(
            "optionalChainSuggest",
            SUGGEST,
            Fix::replace(span, text),
        );
    }
}

/// The text of one accessor; `force` makes it optional.
fn seg_text(ast: &Ast, src: &str, seg: NodeId, force: bool) -> String {
    match *ast.node(seg) {
        Node::Member { prop, optional, .. } => {
            let dot = if force || optional { "?." } else { "." };
            format!("{dot}{}", ast.span(prop).text(src))
        }
        Node::Index {
            index, optional, ..
        } => {
            let open = if force || optional { "?.[" } else { "[" };
            format!("{open}{}]", ast.span(index).text(src))
        }
        Node::Call {
            callee,
            optional,
            type_args,
            ..
        } => {
            let span = ast.span(seg);
            let from = type_args.map_or(ast.span(callee).end, |t| ast.span(t).end) as usize;
            let mut p = skip_trivia_forward(src, from);
            if src[p..].starts_with("?.") {
                p = skip_trivia_forward(src, p + 2);
            }
            let args = if src[p..].starts_with('(') && (span.end as usize) > p {
                &src[p + 1..span.end as usize - 1]
            } else {
                ""
            };
            let targs = type_args.map_or("", |t| ast.span(t).text(src));
            let q = if force || optional { "?." } else { "" };
            format!("{q}{targs}({args})")
        }
        Node::NonNull { .. } => "!".to_owned(),
        _ => String::new(),
    }
}

/// The optional chain that replaces the guarded expressions of `chain`.
fn chain_code(ast: &Ast, src: &str, chain: &[Elem]) -> String {
    let (root, segs) = decompose(ast, chain[0].expr);
    let mut code = ast.span(root).text(src).to_owned();
    for &s in &segs {
        code.push_str(&seg_text(ast, src, s, false));
    }
    for w in chain.windows(2) {
        let (_, prev) = decompose(ast, w[0].expr);
        let k = prev.iter().filter(|&&s| !is_non_null(ast, s)).count();
        let (_, cur) = decompose(ast, w[1].expr);
        let mut pos = 0;
        let mut cnt = 0;
        while cnt < k && pos < cur.len() {
            if !is_non_null(ast, cur[pos]) {
                cnt += 1;
            }
            pos += 1;
        }
        while pos < cur.len() && is_non_null(ast, cur[pos]) {
            pos += 1;
        }
        for (j, &s) in cur[pos..].iter().enumerate() {
            code.push_str(&seg_text(ast, src, s, j == 0));
        }
    }
    code
}
