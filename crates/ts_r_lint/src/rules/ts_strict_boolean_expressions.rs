//! `@typescript-eslint/strict-boolean-expressions`: conditions (`if`, loops, `?:`, `!`,
//! `&&`/`||` operands, `asserts x` arguments, array predicates) only of the types the options
//! allow, with suggestions that spell the check out.

use super::prelude::*;
use crate::type_util::{constrained, constrained_type_at, parts};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/strict-boolean-expressions",
        "Disallow certain types in boolean expressions",
        NO_PRESET,
        Category::Correctness,
    )
    .typed()
    .suggestions(),
    create: create::<StrictBooleanExpressions>,
};

#[derive(Debug)]
struct StrictBooleanExpressions {
    allow_any: bool,
    allow_nullable_boolean: bool,
    allow_nullable_enum: bool,
    allow_nullable_number: bool,
    allow_nullable_object: bool,
    allow_nullable_string: bool,
    allow_number: bool,
    allow_string: bool,
    allow_without_strict_null_checks: bool,
}

impl FromOptions for StrictBooleanExpressions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(StrictBooleanExpressions {
            allow_any: bool_field(o, "allowAny", false),
            allow_nullable_boolean: bool_field(o, "allowNullableBoolean", false),
            allow_nullable_enum: bool_field(o, "allowNullableEnum", false),
            allow_nullable_number: bool_field(o, "allowNullableNumber", false),
            allow_nullable_object: bool_field(o, "allowNullableObject", true),
            allow_nullable_string: bool_field(o, "allowNullableString", false),
            allow_number: bool_field(o, "allowNumber", true),
            allow_string: bool_field(o, "allowString", true),
            allow_without_strict_null_checks: bool_field(
                o,
                "allowRuleToRunWithoutStrictNullChecksIKnowWhatIAmDoing",
                false,
            ),
        })
    }
}

/// The kinds of union members the rule tells apart.
#[derive(Default, PartialEq, Eq)]
struct Variants {
    any: bool,
    boolean: bool,
    truthy_boolean: bool,
    enum_: bool,
    never: bool,
    nullish: bool,
    number: bool,
    truthy_number: bool,
    object: bool,
    string: bool,
    truthy_string: bool,
}

fn variants(ti: &dyn TypeInfo, types: &[Ty]) -> Variants {
    let mut v = Variants::default();
    let has = |f: TypeFlags| types.iter().any(|&t| ti.flags(t).intersects(f));
    v.nullish = has(TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID_LIKE);
    let booleans: Vec<Ty> = types
        .iter()
        .copied()
        .filter(|&t| ti.flags(t).intersects(TypeFlags::BOOLEAN_LIKE))
        .collect();
    match booleans[..] {
        [b] if ti.flags(b).contains(TypeFlags::BOOLEAN_LITERAL)
            && !ti.flags(b).contains(TypeFlags::BOOLEAN)
            && ti.type_to_string(b) == "true" =>
        {
            v.truthy_boolean = true
        }
        [] => {}
        _ => v.boolean = true,
    }
    let strings: Vec<Ty> = types
        .iter()
        .copied()
        .filter(|&t| ti.flags(t).intersects(TypeFlags::STRING_LIKE))
        .collect();
    if !strings.is_empty() {
        let truthy = strings.iter().all(|&t| {
            ti.flags(t).contains(TypeFlags::STRING_LITERAL)
                && ti.literal_text(t).is_some_and(|s| !s.is_empty())
        });
        if truthy {
            v.truthy_string = true;
        } else {
            v.string = true;
        }
    }
    let numbers: Vec<Ty> = types
        .iter()
        .copied()
        .filter(|&t| {
            ti.flags(t)
                .intersects(TypeFlags::NUMBER_LIKE | TypeFlags::BIGINT_LIKE)
        })
        .collect();
    if !numbers.is_empty() {
        let truthy = numbers.iter().all(|&t| {
            ti.flags(t).contains(TypeFlags::NUMBER_LITERAL)
                && ti
                    .literal_text(t)
                    .and_then(|s| s.parse::<f64>().ok())
                    .is_some_and(|n| n != 0.0)
        });
        if truthy {
            v.truthy_number = true;
        } else {
            v.number = true;
        }
    }
    v.enum_ = has(TypeFlags::ENUM_LIKE);
    let other = TypeFlags::NULL
        | TypeFlags::UNDEFINED
        | TypeFlags::VOID_LIKE
        | TypeFlags::BOOLEAN_LIKE
        | TypeFlags::STRING_LIKE
        | TypeFlags::NUMBER_LIKE
        | TypeFlags::BIGINT_LIKE
        | TypeFlags::TYPE_PARAMETER
        | TypeFlags::ANY
        | TypeFlags::UNKNOWN
        | TypeFlags::NEVER;
    if types.iter().any(|&t| !ti.flags(t).intersects(other)) {
        // `boolean & { __brand: ... }` is still a boolean.
        let branded = types.iter().any(|&t| {
            ti.flags(t).contains(TypeFlags::INTERSECTION)
                && ti.intersection_members(t).iter().any(|&m| {
                    ti.flags(m)
                        .intersects(TypeFlags::BOOLEAN | TypeFlags::BOOLEAN_LITERAL)
                })
        });
        if branded {
            v.boolean = true;
        } else {
            v.object = true;
        }
    }
    v.any = has(TypeFlags::TYPE_PARAMETER | TypeFlags::ANY | TypeFlags::UNKNOWN);
    v.never = has(TypeFlags::NEVER);
    v
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Error {
    Any,
    NullableBoolean,
    NullableEnum,
    NullableNumber,
    NullableObject,
    NullableString,
    Nullish,
    Number,
    Object,
    Other,
    String,
}

impl Error {
    fn id(self) -> &'static str {
        match self {
            Error::Any => "conditionErrorAny",
            Error::NullableBoolean => "conditionErrorNullableBoolean",
            Error::NullableEnum => "conditionErrorNullableEnum",
            Error::NullableNumber => "conditionErrorNullableNumber",
            Error::NullableObject => "conditionErrorNullableObject",
            Error::NullableString => "conditionErrorNullableString",
            Error::Nullish => "conditionErrorNullish",
            Error::Number => "conditionErrorNumber",
            Error::Object => "conditionErrorObject",
            Error::Other => "conditionErrorOther",
            Error::String => "conditionErrorString",
        }
    }

    fn message(self, context: &str) -> String {
        match self {
            Error::Any => format!(
                "Unexpected any value in {context}. An explicit comparison or type conversion is required."
            ),
            Error::NullableBoolean => format!(
                "Unexpected nullable boolean value in {context}. Please handle the nullish case explicitly."
            ),
            Error::NullableEnum => format!(
                "Unexpected nullable enum value in {context}. Please handle the nullish/zero/NaN cases explicitly."
            ),
            Error::NullableNumber => format!(
                "Unexpected nullable number value in {context}. Please handle the nullish/zero/NaN cases explicitly."
            ),
            Error::NullableObject => format!(
                "Unexpected nullable object value in {context}. An explicit null check is required."
            ),
            Error::NullableString => format!(
                "Unexpected nullable string value in {context}. Please handle the nullish/empty cases explicitly."
            ),
            Error::Nullish => {
                "Unexpected nullish value in conditional. The condition is always false.".into()
            }
            Error::Number => format!(
                "Unexpected number value in {context}. An explicit zero/NaN check is required."
            ),
            Error::Object => {
                format!("Unexpected object value in {context}. The condition is always true.")
            }
            Error::Other => {
                "Unexpected value in conditional. A boolean expression is required.".into()
            }
            Error::String => format!(
                "Unexpected string value in {context}. An explicit empty string check is required."
            ),
        }
    }
}

const CAST_BOOLEAN: (&str, &str) = (
    "conditionFixCastBoolean",
    "Explicitly convert value to a boolean (`Boolean(value)`)",
);
const LENGTH_NONZERO: (&str, &str) = (
    "conditionFixCompareArrayLengthNonzero",
    "Change condition to check array's length (`value.length > 0`)",
);
const LENGTH_ZERO: (&str, &str) = (
    "conditionFixCompareArrayLengthZero",
    "Change condition to check array's length (`value.length === 0`)",
);
const EMPTY_STRING: (&str, &str) = (
    "conditionFixCompareEmptyString",
    "Change condition to check for empty string (`value !== \"\"`)",
);
const COMPARE_FALSE: (&str, &str) = (
    "conditionFixCompareFalse",
    "Change condition to check if false (`value === false`)",
);
const COMPARE_NAN: (&str, &str) = (
    "conditionFixCompareNaN",
    "Change condition to check for NaN (`!Number.isNaN(value)`)",
);
const COMPARE_NULLISH: (&str, &str) = (
    "conditionFixCompareNullish",
    "Change condition to check for null/undefined (`value != null`)",
);
const STRING_LENGTH: (&str, &str) = (
    "conditionFixCompareStringLength",
    "Change condition to check string's length (`value.length !== 0`)",
);
const COMPARE_TRUE: (&str, &str) = (
    "conditionFixCompareTrue",
    "Change condition to check if true (`value === true`)",
);
const COMPARE_ZERO: (&str, &str) = (
    "conditionFixCompareZero",
    "Change condition to check for 0 (`value !== 0`)",
);
const DEFAULT_EMPTY_STRING: (&str, &str) = (
    "conditionFixDefaultEmptyString",
    "Explicitly treat nullish value the same as an empty string (`value ?? \"\"`)",
);
const DEFAULT_FALSE: (&str, &str) = (
    "conditionFixDefaultFalse",
    "Explicitly treat nullish value the same as false (`value ?? false`)",
);
const DEFAULT_ZERO: (&str, &str) = (
    "conditionFixDefaultZero",
    "Explicitly treat nullish value the same as 0 (`value ?? 0`)",
);

/// Expressions that never need parentheses as an operand.
fn is_strong(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match ast.node(id) {
        Node::Member { .. } | Node::Index { .. } | Node::Call { .. } => {
            !chain_has_optional(ast, id)
        }
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::Ident { .. }
        | Node::ArrayLit { .. }
        | Node::ObjectLit { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::Instantiation { .. } => true,
        _ => false,
    }
}

/// `node`'s parent binds tighter than a binary operator around `node` would.
fn has_weak_parent(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = parent_skip_parens(ast, cx.file.parents, node) else {
        return false;
    };
    let is = |id: NodeId| skip_parens(ast, id) == node;
    match *ast.node(p) {
        Node::Update { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::Conditional { .. }
        | Node::Await { .. } => true,
        Node::Member { obj, .. } | Node::Index { obj, .. } => is(obj),
        Node::Call { callee, .. } | Node::New { callee, .. } => is(callee),
        Node::TaggedTemplate { tag, .. } => is(tag),
        _ => false,
    }
}

/// Replaces `node` with `wrap(inner)`, adding the parentheses either side needs.
fn wrapping_fix(cx: &Ctx<'_, '_>, node: NodeId, inner: NodeId, wrap: &str) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let mut code = cx.span(inner).text(src).to_owned();
    let arrow_object = matches!(ast.node(inner), Node::ObjectLit { .. })
        && parent_skip_parens(ast, cx.file.parents, node).is_some_and(
            |p| matches!(*ast.node(p), Node::Arrow { body, .. } if skip_parens(ast, body) == node),
        );
    if !is_strong(ast, inner) || arrow_object {
        code = format!("({code})");
    }
    let mut text = wrap.replace("{}", &code);
    let parenthesized = cx
        .file
        .parents
        .parent(node)
        .is_some_and(|p| matches!(ast.node(p), Node::Paren { .. }));
    if has_weak_parent(cx, node) && !parenthesized {
        text = format!("({text})");
    }
    if text.starts_with(['(', '[', '`']) && missing_semicolon_before(cx, node) {
        text = format!(";{text}");
    }
    Fix::replace(cx.span(node), text)
}

/// `node` starts an expression statement (through left-hand operands) that follows a
/// statement not ending in `;`, so text starting with `(` would continue that statement.
fn missing_semicolon_before(cx: &Ctx<'_, '_>, mut node: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..10_000 {
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, node) else {
            return false;
        };
        if let Node::ExprStmt { .. } = ast.node(parent)
            && let Some(block) = cx.file.parents.parent(parent)
            && let Node::Program { stmts } | Node::Block { stmts } = *ast.node(block)
        {
            let stmts = ast.list(stmts);
            if let Some(i) = stmts.iter().position(|&s| s == parent)
                && i > 0
            {
                let end = cx.span(stmts[i - 1]).end as usize;
                if cx.src().as_bytes().get(end.wrapping_sub(1)) != Some(&b';') {
                    return true;
                }
            }
        }
        let is = |id: NodeId| skip_parens(ast, id) == node;
        let left_hand = match *ast.node(parent) {
            Node::Update { .. } => true,
            Node::Binary { left, .. } | Node::Assign { left, .. } => is(left),
            Node::Conditional { test, .. } => is(test),
            Node::Call { callee, .. } => is(callee),
            Node::TaggedTemplate { tag, .. } => is(tag),
            _ => false,
        };
        if !left_hand {
            return false;
        }
        node = parent;
    }
    false
}

impl StrictBooleanExpressions {
    fn report_type(&self, v: &Variants) -> Option<Error> {
        let set = [
            v.any,
            v.boolean,
            v.truthy_boolean,
            v.enum_,
            v.never,
            v.nullish,
            v.number,
            v.truthy_number,
            v.object,
            v.string,
            v.truthy_string,
        ];
        let count = set.iter().filter(|&&b| b).count();
        let is = |wanted: &[bool]| wanted.len() == count && wanted.iter().all(|&b| b);
        if is(&[v.boolean]) || is(&[v.truthy_boolean]) || is(&[v.never]) {
            return None;
        }
        if is(&[v.nullish]) {
            return Some(Error::Nullish);
        }
        if is(&[v.nullish, v.truthy_boolean]) {
            return None;
        }
        if is(&[v.nullish, v.boolean]) {
            return (!self.allow_nullable_boolean).then_some(Error::NullableBoolean);
        }
        if (self.allow_number && is(&[v.nullish, v.truthy_number]))
            || (self.allow_string && is(&[v.nullish, v.truthy_string]))
        {
            return None;
        }
        if is(&[v.string]) || is(&[v.truthy_string]) {
            return (!self.allow_string).then_some(Error::String);
        }
        if is(&[v.nullish, v.string]) {
            return (!self.allow_nullable_string).then_some(Error::NullableString);
        }
        if is(&[v.number]) || is(&[v.truthy_number]) {
            return (!self.allow_number).then_some(Error::Number);
        }
        if is(&[v.nullish, v.number]) {
            return (!self.allow_nullable_number).then_some(Error::NullableNumber);
        }
        if is(&[v.object]) {
            return Some(Error::Object);
        }
        if is(&[v.nullish, v.object]) {
            return (!self.allow_nullable_object).then_some(Error::NullableObject);
        }
        let nullable_enum = v.nullish
            && v.enum_
            && !v.any
            && !v.boolean
            && !v.truthy_boolean
            && !v.never
            && !v.object
            && (v.number || v.truthy_number || v.string || v.truthy_string)
            && !(v.number && v.truthy_number)
            && !(v.string && v.truthy_string);
        if nullable_enum {
            return (!self.allow_nullable_enum).then_some(Error::NullableEnum);
        }
        if is(&[v.any]) {
            return (!self.allow_any).then_some(Error::Any);
        }
        Some(Error::Other)
    }

    fn suggestions(
        cx: &Ctx<'_, '_>,
        node: NodeId,
        error: Error,
    ) -> Vec<(&'static str, &'static str, Fix)> {
        let ast = cx.ast();
        let negated = parent_skip_parens(ast, cx.file.parents, node).filter(|&p| {
            matches!(
                ast.node(p),
                Node::Unary {
                    op: UnaryOp::Not,
                    ..
                }
            )
        });
        let on = |s: (&'static str, &'static str), wrap: &str| {
            (s.0, s.1, wrapping_fix(cx, node, node, wrap))
        };
        let neg = |s: (&'static str, &'static str), p: NodeId, wrap: &str| {
            (s.0, s.1, wrapping_fix(cx, p, node, wrap))
        };
        match (error, negated) {
            (Error::Any, _) => vec![on(CAST_BOOLEAN, "Boolean({})")],
            (Error::NullableBoolean, Some(p)) => vec![
                on(DEFAULT_FALSE, "{} ?? false"),
                neg(COMPARE_FALSE, p, "{} === false"),
            ],
            (Error::NullableBoolean, None) => vec![
                on(DEFAULT_FALSE, "{} ?? false"),
                on(COMPARE_TRUE, "{} === true"),
            ],
            (Error::NullableEnum | Error::NullableObject, Some(p)) => {
                vec![neg(COMPARE_NULLISH, p, "{} == null")]
            }
            (Error::NullableEnum | Error::NullableObject, None) => {
                vec![on(COMPARE_NULLISH, "{} != null")]
            }
            (Error::NullableNumber, Some(p)) => vec![
                neg(COMPARE_NULLISH, p, "{} == null"),
                on(DEFAULT_ZERO, "{} ?? 0"),
                neg(CAST_BOOLEAN, p, "!Boolean({})"),
            ],
            (Error::NullableNumber, None) => vec![
                on(COMPARE_NULLISH, "{} != null"),
                on(DEFAULT_ZERO, "{} ?? 0"),
                on(CAST_BOOLEAN, "Boolean({})"),
            ],
            (Error::NullableString, Some(p)) => vec![
                neg(COMPARE_NULLISH, p, "{} == null"),
                on(DEFAULT_EMPTY_STRING, "{} ?? \"\""),
                neg(CAST_BOOLEAN, p, "!Boolean({})"),
            ],
            (Error::NullableString, None) => vec![
                on(COMPARE_NULLISH, "{} != null"),
                on(DEFAULT_EMPTY_STRING, "{} ?? \"\""),
                on(CAST_BOOLEAN, "Boolean({})"),
            ],
            (Error::Number, _) if is_array_length(cx, node) => match negated {
                Some(p) => vec![neg(LENGTH_ZERO, p, "{} === 0")],
                None => vec![on(LENGTH_NONZERO, "{} > 0")],
            },
            (Error::Number, Some(p)) => vec![
                neg(COMPARE_ZERO, p, "{} === 0"),
                neg(COMPARE_NAN, p, "Number.isNaN({})"),
                neg(CAST_BOOLEAN, p, "!Boolean({})"),
            ],
            (Error::Number, None) => vec![
                on(COMPARE_ZERO, "{} !== 0"),
                on(COMPARE_NAN, "!Number.isNaN({})"),
                on(CAST_BOOLEAN, "Boolean({})"),
            ],
            (Error::String, Some(p)) => vec![
                neg(STRING_LENGTH, p, "{}.length === 0"),
                neg(EMPTY_STRING, p, "{} === \"\""),
                neg(CAST_BOOLEAN, p, "!Boolean({})"),
            ],
            (Error::String, None) => vec![
                on(STRING_LENGTH, "{}.length > 0"),
                on(EMPTY_STRING, "{} !== \"\""),
                on(CAST_BOOLEAN, "Boolean({})"),
            ],
            (Error::Object | Error::Nullish | Error::Other, _) => Vec::new(),
        }
    }

    fn check_node(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, node) else {
            return;
        };
        if t == Ty::UNRESOLVED {
            return;
        }
        let v = variants(ti, &parts(ti, t));
        let Some(error) = self.report_type(&v) else {
            return;
        };
        let sugg = Self::suggestions(cx, node, error);
        let mut r = cx
            .report_node(node, error.message("conditional"))
            .id(error.id());
        for (id, desc, fix) in sugg {
            r = r.suggest(id, desc, fix);
        }
    }

    /// Checks `node` as a condition; `&&`/`||` pass the check on to their operands (the
    /// right one only when the whole expression is itself a condition).
    fn traverse(&self, cx: &mut Ctx<'_, '_>, node: NodeId, is_condition: bool, depth: u32) {
        let ast = cx.ast();
        let node = skip_parens(ast, node);
        if depth > 500 {
            return;
        }
        if let Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
            left,
            right,
        } = *ast.node(node)
        {
            self.traverse(cx, left, true, depth + 1);
            self.traverse(cx, right, is_condition, depth + 1);
            return;
        }
        if is_condition {
            self.check_node(cx, node);
        }
    }

    fn check_predicate(&self, cx: &mut Ctx<'_, '_>, pred: NodeId) {
        let ast = cx.ast();
        let pred = skip_parens(ast, pred);
        let func = match *ast.node(pred) {
            Node::Arrow {
                flags,
                body,
                return_type,
                ..
            }
            | Node::FunctionExpr {
                flags,
                body: Some(body),
                return_type,
                ..
            } => Some((flags, body, return_type)),
            _ => None,
        };
        if let Some((flags, ..)) = func
            && flags.contains(FnFlags::ASYNC)
        {
            cx.report_node(
                pred,
                "Predicate function should not be 'async'; expected a boolean return type.",
            )
            .id("predicateCannotBeAsync");
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(pred) else {
            return;
        };
        let mut types: Vec<Ty> = Vec::new();
        for sig in ti.signatures(t, false) {
            let mut r = ti.return_type(sig);
            if ti.flags(r).contains(TypeFlags::TYPE_PARAMETER) {
                let c = ti.base_constraint(r);
                if c != Ty::UNRESOLVED {
                    r = c;
                }
            }
            for p in parts(ti, r) {
                if !types.contains(&p) {
                    types.push(p);
                }
            }
        }
        if types.contains(&Ty::UNRESOLVED) {
            return;
        }
        let Some(error) = self.report_type(&variants(ti, &types)) else {
            return;
        };
        let mut sugg = Vec::new();
        if let Some((_, body, return_type)) = func {
            if !matches!(ast.node(body), Node::Block { .. }) {
                sugg = Self::suggestions(cx, skip_parens(ast, body), error);
            }
            if return_type.is_none()
                && let Some(fix) = boolean_return_fix(cx, pred)
            {
                sugg.push((
                    "explicitBooleanReturnType",
                    "Add an explicit `boolean` return type annotation.",
                    fix,
                ));
            }
        }
        let mut r = cx
            .report_node(pred, error.message("array predicate return type"))
            .id(error.id());
        for (id, desc, fix) in sugg {
            r = r.suggest(id, desc, fix);
        }
    }

    /// `assert(x)` where `assert` is declared `asserts x` (no type): the argument.
    fn asserted_argument(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
        let ast = cx.ast();
        let Node::Call { args, .. } = *ast.node(call) else {
            return None;
        };
        let args: Vec<NodeId> = ast
            .list(args)
            .iter()
            .copied()
            .take_while(|&a| !matches!(ast.node(a), Node::Spread { .. }))
            .collect();
        if args.is_empty() {
            return None;
        }
        let ti = cx.types();
        let pred = ti.type_predicate(ti.resolved_signature(call)?)?;
        if !pred.asserts || pred.this || pred.ty.is_some() {
            return None;
        }
        args.get(pred.param_index).copied()
    }

    fn is_array_predicate_call(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(call) else {
            return false;
        };
        let callee = skip_parens(ast, callee);
        if !matches!(ast.node(callee), Node::Member { .. } | Node::Index { .. }) {
            return false;
        }
        let Some((obj, name)) = member_parts(ast, cx.src(), callee) else {
            return false;
        };
        if !matches!(
            &*name,
            "every" | "filter" | "find" | "findIndex" | "findLast" | "findLastIndex" | "some"
        ) {
            return false;
        }
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, obj) else {
            return false;
        };
        parts(ti, t).into_iter().any(|p| {
            let members = if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                ti.intersection_members(p)
            } else {
                vec![p]
            };
            members.iter().any(|&m| ti.is_array(m) || ti.is_tuple(m))
        })
    }

    /// Whether `node` (a `&&`/`||`) is already handled from an enclosing condition.
    fn logical_is_covered(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let ast = cx.ast();
        let Some(p) = parent_skip_parens(ast, cx.file.parents, node) else {
            return false;
        };
        let is = |id: NodeId| skip_parens(ast, id) == node;
        match *ast.node(p) {
            Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                ..
            }
            | Node::Unary {
                op: UnaryOp::Not, ..
            } => true,
            Node::If { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. }
            | Node::Conditional { test, .. } => is(test),
            Node::For { test: Some(t), .. } => is(t),
            Node::Call { .. } => Self::asserted_argument(cx, p).is_some_and(is),
            _ => false,
        }
    }
}

/// `x.length` on an array (or a union of arrays).
fn is_array_length(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Member { obj, prop, .. } = *ast.node(node) else {
        return false;
    };
    if ident_name(ast, prop) != Some("length") {
        return false;
    }
    let ti = cx.types();
    constrained_type_at(ti, obj).is_some_and(|t| {
        let ps = parts(ti, constrained(ti, t));
        !ps.is_empty() && ps.iter().all(|&p| ti.is_array(p))
    })
}

/// Adds `: boolean` after the parameter list (parenthesizing a lone arrow parameter).
fn boolean_return_fix(cx: &Ctx<'_, '_>, func: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let params = match *ast.node(func) {
        Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => ast.list(params),
        _ => return None,
    };
    let fspan = cx.span(func);
    if let Node::Arrow { .. } = ast.node(func)
        && let [p] = params
        && cx.span(*p).start == fspan.start
    {
        let ps = cx.span(*p);
        return Some(Fix::insert(ps.start, "(").and(Span::new(ps.end, ps.end), "): boolean"));
    }
    let from = params.last().map_or(fspan.start, |&p| cx.span(p).end);
    let close = find_char(src, from, b')')?;
    Some(Fix::insert(close + 1, ": boolean"))
}

impl Rule for StrictBooleanExpressions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Call,
            NodeKind::Conditional,
            NodeKind::DoWhile,
            NodeKind::For,
            NodeKind::If,
            NodeKind::While,
            NodeKind::Binary,
            NodeKind::Unary,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Program { .. } => {
                if !self.allow_without_strict_null_checks && !cx.types().strict_null_checks() {
                    crate::type_util::report_no_strict_null_checks(cx);
                }
            }
            Node::If { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. }
            | Node::Conditional { test, .. }
            | Node::For {
                test: Some(test), ..
            } => {
                self.traverse(cx, test, true, 0);
            }
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } => self.traverse(cx, arg, true, 0),
            Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                left,
                right,
            } => {
                if !Self::logical_is_covered(cx, node) {
                    self.traverse(cx, left, true, 0);
                    self.traverse(cx, right, false, 0);
                }
            }
            Node::Call { args, .. } => {
                if let Some(arg) = Self::asserted_argument(cx, node) {
                    self.traverse(cx, arg, true, 0);
                }
                if Self::is_array_predicate_call(cx, node)
                    && let Some(&pred) = cx.ast().list(args).first()
                {
                    self.check_predicate(cx, pred);
                }
            }
            _ => {}
        }
    }
}
