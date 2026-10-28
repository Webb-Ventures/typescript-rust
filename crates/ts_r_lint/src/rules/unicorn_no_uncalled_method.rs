//! `unicorn/no-uncalled-method`: an array or string method read off a receiver that is
//! known to be an array (string) without calling it (`const f = array.map`, `if (s.trim)`).
//!
//! The receiver is an array when it is the name `array` (or a `const` alias chain ending
//! in it) or is known to be an array; likewise `string` for strings. Not reported: callees
//! (calls, `new`, tag position), `.call`/`.apply`/`.bind` that are called, the first
//! argument of `Reflect.apply()`, `typeof` operands, assignment targets, and
//! `Array.prototype.x` / `String.prototype.x`. Parentheses and TypeScript wrappers around
//! the member are looked through.

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_static::skip_ts_only;
use super::unicorn_types::{is_array, is_string};
use super::unicorn_util::static_property_name;
use rustc_hash::FxHashSet;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-uncalled-method",
        "Disallow referencing methods without calling them.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUncalledMethod>,
};

#[derive(Debug, Default)]
struct NoUncalledMethod;

const ARRAY_METHODS: &[&str] = &[
    "at",
    "concat",
    "copyWithin",
    "entries",
    "every",
    "fill",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "pop",
    "push",
    "reduce",
    "reduceRight",
    "reverse",
    "shift",
    "slice",
    "some",
    "sort",
    "splice",
    "toLocaleString",
    "toReversed",
    "toSorted",
    "toSpliced",
    "toString",
    "unshift",
    "values",
    "with",
];

const STRING_METHODS: &[&str] = &[
    "at",
    "anchor",
    "big",
    "blink",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "concat",
    "endsWith",
    "fixed",
    "fontcolor",
    "fontsize",
    "includes",
    "indexOf",
    "isWellFormed",
    "italics",
    "lastIndexOf",
    "link",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "small",
    "split",
    "startsWith",
    "strike",
    "sub",
    "substr",
    "substring",
    "sup",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "toLowerCase",
    "toString",
    "toUpperCase",
    "toWellFormed",
    "trim",
    "trimEnd",
    "trimLeft",
    "trimRight",
    "trimStart",
    "valueOf",
];

/// Inside parentheses and TypeScript wrappers.
fn unwrap(ast: &ts_r_ast::Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1024 {
        let next = skip_ts_only(ast, skip_parens(ast, id));
        if next == id {
            break;
        }
        id = next;
    }
    id
}

/// `id` with the parentheses and TypeScript wrappers around it, and its parent.
fn outermost(cx: &Ctx<'_, '_>, id: NodeId) -> (NodeId, Option<NodeId>) {
    let ast = cx.ast();
    let mut cur = id;
    loop {
        let Some(p) = cx.parent(cur) else {
            return (cur, None);
        };
        match ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            _ => return (cur, Some(p)),
        }
    }
}

fn is_callee(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let (cur, parent) = outermost(cx, id);
    parent.is_some_and(|p| match *cx.ast().node(p) {
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == cur,
        Node::TaggedTemplate { tag, .. } => tag == cur,
        _ => false,
    })
}

fn should_skip(cx: &Ctx<'_, '_>, node: NodeId, obj: NodeId) -> bool {
    let ast = cx.ast();
    if is_callee(cx, node) || is_left_hand_side(cx, node) {
        return true;
    }
    let (cur, parent) = outermost(cx, node);
    if let Some(p) = parent {
        match *ast.node(p) {
            Node::Member { obj: o, .. } | Node::Index { obj: o, .. }
                if o == cur
                    && static_property_name(cx, p)
                        .is_some_and(|m| matches!(m.as_str(), "apply" | "bind" | "call"))
                    && is_callee(cx, p) =>
            {
                return true;
            }
            Node::Call { callee, args, .. }
                if ast.list(args).first() == Some(&cur)
                    && matches!(*ast.node(skip_parens(ast, callee)),
                        Node::Member { obj: r, prop, .. }
                            if ident_name(ast, r) == Some("Reflect")
                                && ident_name(ast, prop) == Some("apply")) =>
            {
                return true;
            }
            Node::Unary {
                op: UnaryOp::TypeOf,
                ..
            } => return true,
            _ => {}
        }
    }
    // `Array.prototype.x`, `String["prototype"].x`.
    matches!(ast.kind(obj), NodeKind::Member | NodeKind::Index)
        && static_property_name(cx, obj).as_deref() == Some("prototype")
        && matches!(*ast.node(obj), Node::Member { obj: o, .. } | Node::Index { obj: o, .. }
            if matches!(ident_name(ast, o), Some("Array" | "String")))
}

/// The receiver is the name `name`, or a `const` alias chain of it.
fn is_named_or_const_alias(cx: &Ctx<'_, '_>, node: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut visited: FxHashSet<SymbolId> = FxHashSet::default();
    let mut cur = node;
    loop {
        cur = unwrap(ast, cur);
        let Some(n) = ident_name(ast, cur) else {
            return false;
        };
        if n == name {
            return true;
        }
        let Some(s) = sem.symbol_of_ident(cur) else {
            return false;
        };
        if !visited.insert(s) {
            return false;
        }
        let [decl] = sem.symbol(s).decls.as_slice() else {
            return false;
        };
        if decl.kind != DeclKind::Const {
            return false;
        }
        match *ast.node(decl.node) {
            Node::VarDeclarator {
                name: id,
                init: Some(init),
                ..
            } if id == decl.name => cur = init,
            _ => return false,
        }
    }
}

impl Rule for NoUncalledMethod {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (obj, prop) = match *cx.node(node) {
            Node::Member { obj, prop, .. } => (obj, prop),
            Node::Index { obj, index, .. } => (obj, index),
            _ => return,
        };
        if should_skip(cx, node, obj) {
            return;
        }
        let Some(method) = static_property_name(cx, node) else {
            return;
        };
        let report = (ARRAY_METHODS.contains(&method.as_str())
            && (is_named_or_const_alias(cx, obj, "array") || is_array(cx, obj)))
            || (STRING_METHODS.contains(&method.as_str())
                && (is_named_or_const_alias(cx, obj, "string") || is_string(cx, obj)));
        if report {
            cx.report_node(
                prop,
                format!("Call `{method}()` instead of referencing the method."),
            )
            .id("no-uncalled-method");
        }
    }
}
