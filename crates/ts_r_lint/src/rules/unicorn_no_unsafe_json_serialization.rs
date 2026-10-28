//! `unicorn/no-unsafe-json-serialization`: values `JSON.stringify()` / `Response.json()`
//! silently turn into `{}`, drop or throw on: `Map`, `Set`, `WeakMap`, `WeakSet`, BigInt,
//! functions and classes, symbols, regexes, `undefined` and non-finite numbers.
//!
//! The argument is searched through array elements and the properties of plain object
//! literals (only when every key is a known, unique string other than `toJSON`/`__proto__`,
//! and only the keys an array replacer keeps); `undefined` is fine as a property value. A
//! `const` name is followed to its initializer, reported at the use and without
//! suggestions. A replacer that is a function or not statically known stops the check.
//!
//! Suggestions: `Array.from(…)` for Map/Set, `Object.fromEntries(…)` for Map, `String(…)`
//! for BigInt.

use super::prelude::*;
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_static::{Static, is_global_ident, static_value, static_value_no_side_effects};
use super::unicorn_text::call_argument_text;
use super::unicorn_text::paren_span as parenthesized_span;
use super::unicorn_types::{
    Checker, Guess, JType, is_bigint, is_map, is_regexp, is_set, is_weak_map, is_weak_set,
};
use crate::ast_util::number_to_string;
use ts_r_ast::{Ast, KeywordTypeKind, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-json-serialization",
        "Disallow known values that JSON serialization cannot represent faithfully.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoUnsafeJsonSerialization>,
};

#[derive(Debug, Default)]
struct NoUnsafeJsonSerialization;

fn symbol_for_call(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    matches!(*ast.node(callee), Node::Member { obj, optional: false, prop }
        if ident_name(ast, obj) == Some("Symbol") && ident_name(ast, prop) == Some("for"))
}

fn symbol_annotation(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Symbol
        }
    )
}

/// A well-known symbol (`Symbol.iterator`).
fn symbol_static_type(v: &Static, _: &Ast, _: NodeId) -> Guess {
    match v {
        Static::Builtin(path)
            if path.starts_with("Symbol.")
                && !matches!(path.as_str(), "Symbol.for" | "Symbol.keyFor") =>
        {
            Guess::Target
        }
        _ => Guess::Unknown,
    }
}

static SYMBOL: Checker = Checker {
    target_names: &["symbol"],
    static_type: Some(symbol_static_type),
    target_calls: &["Symbol"],
    target_node: Some(symbol_for_call),
    target_annotation: Some(symbol_annotation),
    ..Checker::DEFAULT
};

/// A function or class written out, or the name of a function/class declaration that is
/// never reassigned.
fn function_syntax(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::FunctionDecl { .. }
        | Node::ClassExpr { .. } => true,
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
                return false;
            };
            let s = sem.symbol(sym);
            let [d] = s.decls[..] else { return false };
            matches!(d.kind, DeclKind::Function | DeclKind::Class)
                && s.refs.iter().all(|&r| {
                    let r = sem.reference(r);
                    !r.is_write() || r.flags.contains(RefFlags::INIT)
                })
        }
        _ => false,
    }
}

fn function_annotation(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::FunctionType { .. } | Node::ConstructorType { .. }
    )
}

fn function_jsdoc(t: &JType) -> bool {
    matches!(t, JType::Function(_))
}

static FUNCTION: Checker = Checker {
    target_node: Some(function_syntax),
    target_annotation: Some(function_annotation),
    target_jsdoc: Some(function_jsdoc),
    ..Checker::DEFAULT
};

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

fn unsafe_type(cx: &Ctx<'_, '_>, id: NodeId) -> Option<&'static str> {
    type Test = fn(&Ctx<'_, '_>, NodeId) -> bool;
    let checks: [(&str, Test); 5] = [
        ("Map", is_map),
        ("Set", is_set),
        ("WeakMap", is_weak_map),
        ("WeakSet", is_weak_set),
        ("BigInt", is_bigint),
    ];
    if let Some((name, _)) = checks.iter().find(|(_, f)| f(cx, id)) {
        return Some(name);
    }
    if FUNCTION.is_target(cx, id) {
        return Some("function");
    }
    if SYMBOL.is_target(cx, id) {
        return Some("symbol");
    }
    if is_regexp(cx, id) {
        return Some("RegExp");
    }
    let ast = cx.ast();
    let inner = unwrap_ts(ast, id);
    if let Node::Unary {
        op: UnaryOp::Void, ..
    } = ast.node(inner)
    {
        return Some("undefined");
    }
    match static_value_no_side_effects(cx, inner)? {
        Static::Undefined => Some("undefined"),
        Static::Num(n) if !n.is_finite() => Some("non-finite number"),
        _ => None,
    }
}

/// The static name of an object literal member (`None` for spreads, getters' kin and
/// unknown computed keys).
fn member_key(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<String> {
    let ast = cx.ast();
    let key = match *ast.node(prop) {
        Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
        Node::ShorthandProp { name, .. } => name,
        _ => return None,
    };
    match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => Some(ast.atom(name).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::BigIntLit { value } => Some(ast.atom(value).to_owned()),
        Node::ComputedKey { expr } => static_value(cx, expr)?.to_js_string(),
        _ => None,
    }
}

struct Problem {
    node: NodeId,
    ty: &'static str,
    /// Found through a `const` initializer: no suggestions.
    shared: bool,
}

struct Search<'a> {
    names: Option<&'a [String]>,
    out: Vec<Problem>,
    first_only: bool,
}

impl Search<'_> {
    fn visit(
        &mut self,
        cx: &Ctx<'_, '_>,
        original: NodeId,
        allow_undefined: bool,
        path: &mut Vec<NodeId>,
    ) {
        if self.first_only && !self.out.is_empty() {
            return;
        }
        let ast = cx.ast();
        let original = skip_parens(ast, original);
        let node = unwrap_ts(ast, original);
        if path.contains(&node) || path.len() > 200 {
            return;
        }
        path.push(node);
        match *ast.node(node) {
            Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    if !matches!(ast.node(e), Node::Spread { .. } | Node::Hole {}) {
                        self.visit(cx, e, false, path);
                    }
                }
            }
            Node::ObjectLit { props } => {
                let props = ast.list(props);
                let keys: Vec<Option<String>> = props.iter().map(|&p| member_key(cx, p)).collect();
                let mut seen: Vec<&str> = Vec::new();
                let plain = keys.iter().all(|k| match k.as_deref() {
                    Some(k) if k != "toJSON" && k != "__proto__" && !seen.contains(&k) => {
                        seen.push(k);
                        true
                    }
                    _ => false,
                });
                if plain {
                    for (&p, key) in props.iter().zip(&keys) {
                        let value = match *ast.node(p) {
                            Node::Property { value, .. } => value,
                            Node::ShorthandProp { name, .. } => name,
                            Node::MethodDef {
                                kind: MethodKind::Method,
                                value,
                                ..
                            } => value,
                            _ => continue,
                        };
                        let key = key.as_deref().unwrap_or_default();
                        if self.names.is_none_or(|n| n.iter().any(|x| x == key)) {
                            self.visit(cx, value, true, path);
                        }
                    }
                }
            }
            _ => {
                if let Some(ty) = unsafe_type(cx, original) {
                    if ty != "undefined" || !allow_undefined {
                        self.out.push(Problem {
                            node: original,
                            ty,
                            shared: false,
                        });
                    }
                } else if let Some(init) = const_initializer(cx, node) {
                    let mut inner = Search {
                        names: self.names,
                        out: Vec::new(),
                        first_only: true,
                    };
                    inner.visit(cx, init, allow_undefined, path);
                    if let Some(p) = inner.out.first() {
                        self.out.push(Problem {
                            node: original,
                            ty: p.ty,
                            shared: true,
                        });
                    }
                }
            }
        }
        path.pop();
    }
}

/// `typeof value === "function"` for a statically known builtin.
fn is_function_value(v: &Static) -> bool {
    match v {
        Static::Builtin(path) => !matches!(
            path.as_str(),
            "Math" | "JSON" | "Reflect" | "Atomics" | "Intl" | "globalThis" | "console"
        ),
        _ => false,
    }
}

impl Rule for NoUnsafeJsonSerialization {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let args = ast.list(args);
        if args.is_empty()
            || args.len() > 3
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let callee = skip_parens(ast, callee);
        let (obj, name) = match *ast.node(callee) {
            Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop).map(str::to_owned)),
            Node::Index { obj, index, .. } => {
                (obj, static_value(cx, index).and_then(|v| v.to_js_string()))
            }
            _ => return,
        };
        let Some(object) = ident_name(ast, obj) else {
            return;
        };
        if !matches!(object, "JSON" | "Response") || !is_global_ident(cx, obj, object) {
            return;
        }
        let method = format!("{object}.{}", name.unwrap_or_default());
        if method != "JSON.stringify" && (method != "Response.json" || args.len() > 2) {
            return;
        }
        let mut names: Option<Vec<String>> = None;
        if method == "JSON.stringify"
            && let Some(&replacer) = args.get(1)
        {
            match static_value_no_side_effects(cx, replacer) {
                None => return,
                Some(v) if is_function_value(&v) => return,
                Some(Static::Array(items)) => {
                    names = Some(
                        items
                            .iter()
                            .filter(|v| matches!(v, Static::Str(_) | Static::Num(_)))
                            .filter_map(Static::to_js_string)
                            .collect(),
                    );
                }
                Some(_) => {}
            }
        }
        let mut search = Search {
            names: names.as_deref(),
            out: Vec::new(),
            first_only: false,
        };
        search.visit(cx, args[0], false, &mut Vec::new());
        for p in search.out {
            let msg = format!("`{method}()` cannot faithfully serialize {} values.", p.ty);
            let conversions: &[(&'static str, &str, &str)] = match (p.shared, p.ty) {
                (false, "Map") => &[
                    (
                        "no-unsafe-json-serialization/array",
                        "Array.from",
                        "Convert the Map to an array.",
                    ),
                    (
                        "no-unsafe-json-serialization/object",
                        "Object.fromEntries",
                        "Convert the Map to an object (requires string keys).",
                    ),
                ],
                (false, "Set") => &[(
                    "no-unsafe-json-serialization/array",
                    "Array.from",
                    "Convert the Set to an array.",
                )],
                (false, "BigInt") => &[(
                    "no-unsafe-json-serialization/string",
                    "String",
                    "Convert the BigInt to a string.",
                )],
                _ => &[],
            };
            let shorthand = cx
                .parent(p.node)
                .is_some_and(|q| matches!(cx.node(q), Node::ShorthandProp { .. }));
            let span = parenthesized_span(cx, p.node);
            let fixes: Vec<_> = conversions
                .iter()
                .map(|&(id, conversion, desc)| {
                    let mut text = format!("{conversion}({})", call_argument_text(cx, p.node));
                    if shorthand {
                        text = format!("{}: {text}", cx.text(p.node));
                    }
                    (id, desc, Fix::replace(span, text))
                })
                .collect();
            let mut r = cx
                .report_node(p.node, msg)
                .id("no-unsafe-json-serialization");
            for (id, desc, fix) in fixes {
                r = r.suggest(id, desc, fix);
            }
        }
    }
}
