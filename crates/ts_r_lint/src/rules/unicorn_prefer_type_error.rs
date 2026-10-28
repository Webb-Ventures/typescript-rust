//! `unicorn/prefer-type-error`: throw `TypeError`, not `Error`, when a type check fails.
//!
//! The `throw new Error(…)` must be the whole consequent of an `if` (alone or as the only
//! statement of its block). The test is a type check when it is `typeof x` (other than an
//! existence check against `'undefined'`), `x instanceof Y` with `Y` not an error class, a
//! call of a lodash/underscore-style `is*` method (`_.isArray(x)`, `a.b.isString(x)`), a
//! global `isNaN(x)`/`isFinite(x)` call, `!` of one of those, a comparison either side of
//! which is one, or `&&`/`||`/`??` with type checks on both sides. The fix prefixes the
//! `Error` callee with `Type`.

use super::prelude::*;
use super::unicorn_util::is_error_name;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-type-error",
        "Enforce throwing `TypeError` in type checking conditions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferTypeError>,
};

const TYPE_CHECK_METHODS: &[&str] = &[
    "isArguments",
    "isArray",
    "isArrayBuffer",
    "isArrayLike",
    "isArrayLikeObject",
    "isBigInt",
    "isBoolean",
    "isBuffer",
    "isDate",
    "isElement",
    "isError",
    "isFinite",
    "isFunction",
    "isInteger",
    "isLength",
    "isMap",
    "isNaN",
    "isNative",
    "isNil",
    "isNull",
    "isNumber",
    "isObject",
    "isObjectLike",
    "isPlainObject",
    "isPrototypeOf",
    "isRegExp",
    "isSafeInteger",
    "isSet",
    "isString",
    "isSymbol",
    "isTypedArray",
    "isUndefined",
    "isView",
    "isWeakMap",
    "isWeakSet",
    "isWindow",
    "isXMLDoc",
];

#[derive(Debug, Default)]
struct PreferTypeError;

impl Rule for PreferTypeError {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Throw]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Throw { arg } = *cx.node(node) else {
            return;
        };
        let Node::New { callee, .. } = *ast.node(skip_parens(ast, arg)) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        if ident_name(ast, callee) != Some("Error") {
            return;
        }
        let Some(mut consequent) = cx.parent(node) else {
            return;
        };
        if let Node::Block { stmts } = *ast.node(consequent) {
            if ast.list(stmts).len() != 1 {
                return;
            }
        } else {
            consequent = node;
        }
        let Some(if_stmt) = cx.parent(consequent) else {
            return;
        };
        let Node::If { test, cons, .. } = *ast.node(if_stmt) else {
            return;
        };
        if cons != consequent || !is_type_check(ast, cx.src(), test, None, 0) {
            return;
        }
        let at = cx.span(callee).start;
        cx.report_node(
            callee,
            "`new Error()` is too unspecific for a type check. Use `new TypeError()` instead.",
        )
        .id("prefer-type-error")
        .fix(Fix::insert(at, "Type"));
    }
}

/// `call` is the call whose callee `e` is (an `is*` name counts only when called with
/// arguments).
fn is_type_check(ast: &Ast, src: &str, e: NodeId, call: Option<NodeId>, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::Ident { .. } => {
            called_with_args(ast, call) && matches!(ident_name(ast, e), Some("isNaN" | "isFinite"))
        }
        Node::Member { .. } | Node::Index { .. } => is_type_check_member(ast, e, call),
        Node::Call { callee, .. } => is_type_check(ast, src, callee, Some(e), depth + 1),
        Node::Unary {
            op: UnaryOp::TypeOf,
            ..
        } => true,
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => is_type_check(ast, src, arg, None, depth + 1),
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } => {
            is_type_check(ast, src, left, call, depth + 1)
                && is_type_check(ast, src, right, call, depth + 1)
        }
        Node::Binary {
            op: BinaryOp::InstanceOf,
            right,
            ..
        } => !is_error_constructor(ast, right),
        Node::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
            ) && (is_typeof_undefined(ast, src, left, right)
                || is_typeof_undefined(ast, src, right, left))
            {
                return false;
            }
            is_type_check(ast, src, left, call, depth + 1)
                || is_type_check(ast, src, right, call, depth + 1)
        }
        _ => false,
    }
}

fn called_with_args(ast: &Ast, call: Option<NodeId>) -> bool {
    call.is_some_and(
        |c| matches!(*ast.node(c), Node::Call { args, .. } if !ast.list(args).is_empty()),
    )
}

/// `a.isString`, `a.b.isArray`, `a[isArray]` (an identifier property, computed or not),
/// anywhere along the member chain.
fn is_type_check_member(ast: &Ast, e: NodeId, call: Option<NodeId>) -> bool {
    let mut cur = e;
    for _ in 0..100_000 {
        let (obj, prop) = match *ast.node(cur) {
            Node::Member { obj, prop, .. } => (obj, prop),
            Node::Index { obj, index, .. } => (obj, skip_parens(ast, index)),
            _ => return false,
        };
        if called_with_args(ast, call)
            && ident_name(ast, prop).is_some_and(|n| TYPE_CHECK_METHODS.contains(&n))
        {
            return true;
        }
        cur = skip_parens(ast, obj);
    }
    false
}

/// `typeof x` on one side and the string `'undefined'` on the other.
fn is_typeof_undefined(ast: &Ast, src: &str, a: NodeId, b: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, a)),
        Node::Unary {
            op: UnaryOp::TypeOf,
            ..
        }
    ) && static_value_string(ast, src, b).as_deref() == Some("undefined")
}

fn is_error_constructor(ast: &Ast, e: NodeId) -> bool {
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::Ident { .. } => ident_name(ast, e).is_some_and(is_error_name),
        Node::Member {
            optional: false,
            prop,
            ..
        } => ident_name(ast, prop).is_some_and(is_error_name),
        _ => false,
    }
}
