//! `unicorn/prefer-prototype-methods`: borrow methods from the prototype, not an instance.
//!
//! The method node is the first argument of `Reflect.apply(...)` or the object of a
//! `.apply`/`.bind`/`.call` call (non-optional, non-computed method names). It is reported
//! when it is
//! - `[].x` / `{}.x` (an empty literal, non-optional member): the literal becomes
//!   `Array.prototype` / `Object.prototype`. The method name comes from the key, evaluated
//!   statically with `const` bindings and well-known symbols (`Symbol(Symbol.iterator)`);
//!   without a name the message only names the prototype;
//! - a read of one of `Object.prototype`'s methods as a global: a configured global
//!   (`/* globals toString */`) or a member of the global object (`globalThis.x`,
//!   `window["x"]`, an alias `const _ = globalThis`), with a key that is static without
//!   variables. The whole node becomes `Object.prototype.<name>`.
//!
//! No fix when a comment is inside the replaced node.

use super::prelude::*;
use super::unicorn_util::static_member_name;
use super::unicorn_util::{is_global_object, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-prototype-methods",
        "Prefer borrowing methods from the prototype instead of the instance.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferPrototypeMethods>,
};

#[derive(Debug, Default)]
struct PreferPrototypeMethods;

const OBJECT_PROTOTYPE_METHODS: &[&str] = &[
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

impl Rule for PreferPrototypeMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        let args = ast.list(args);
        let method_node = match ident_name(ast, prop) {
            Some("apply")
                if ident_name(ast, skip_parens(ast, obj)) == Some("Reflect")
                    && !args.is_empty() =>
            {
                if matches!(ast.node(args[0]), Node::Spread { .. }) {
                    return;
                }
                args[0]
            }
            Some("apply" | "bind" | "call") => obj,
            _ => return,
        };
        let method_node = skip_parens(ast, method_node);
        if let Some(name) = global_method(cx, method_node) {
            let span = cx.span(method_node);
            let fix = ast.comments_in(span).is_empty().then(|| {
                with_edits(
                    Fix::replace(span, format!("Object.prototype.{name}")),
                    space_around_keyword(cx, node),
                )
            });
            cx.report(span, format!("Prefer using `Object.prototype.{name}`."))
                .id("known-method")
                .fix_opt(fix);
            return;
        }
        let (object, key) = match *ast.node(method_node) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } => (obj, ident_name(ast, prop).map(str::to_owned)),
            Node::Index {
                obj,
                optional: false,
                index,
            } => (obj, key_value(cx, index, true)),
            _ => return,
        };
        let object = skip_parens(ast, object);
        let constructor = match *ast.node(object) {
            Node::ArrayLit { elements } if ast.list(elements).is_empty() => "Array",
            Node::ObjectLit { props } if ast.list(props).is_empty() => "Object",
            _ => return,
        };
        let span = cx.span(object);
        let fix = ast.comments_in(span).is_empty().then(|| {
            with_edits(
                Fix::replace(span, format!("{constructor}.prototype")),
                space_around_keyword(cx, node),
            )
        });
        let r = match key {
            Some(name) => cx
                .report_node(
                    method_node,
                    format!("Prefer using `{constructor}.prototype.{name}`."),
                )
                .id("known-method"),
            None => cx
                .report_node(
                    method_node,
                    format!("Prefer using method from `{constructor}.prototype`."),
                )
                .id("unknown-method"),
        };
        r.fix_opt(fix);
    }
}

/// The `Object.prototype` method `node` reads as a global, if it does.
fn global_method(cx: &Ctx<'_, '_>, node: NodeId) -> Option<&'static str> {
    let ast = cx.ast();
    let name = match *ast.node(node) {
        Node::Ident { .. } => {
            let name = ident_name(ast, node)?;
            let m = OBJECT_PROTOTYPE_METHODS.iter().find(|m| **m == name)?;
            return cx.is_global_ref(node, m).then_some(*m);
        }
        Node::Member { obj, .. } if is_global_object(cx, obj) => {
            static_member_name(ast, node)?.to_owned()
        }
        Node::Index { obj, index, .. } if is_global_object(cx, obj) => key_value(cx, index, false)?,
        _ => return None,
    };
    OBJECT_PROTOTYPE_METHODS
        .iter()
        .find(|m| **m == name)
        .copied()
}

/// The static string value of a computed key: literals, templates, `+`, well-known
/// symbols and (with `consts`) `const` bindings.
fn key_value(cx: &Ctx<'_, '_>, id: NodeId, consts: bool) -> Option<String> {
    fn eval(cx: &Ctx<'_, '_>, id: NodeId, consts: bool, depth: u32) -> Option<String> {
        if depth > 16 {
            return None;
        }
        let ast = cx.ast();
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            Node::NumberLit { value } => Some(number_to_string(value.get())),
            Node::TemplateLit { parts } => {
                let mut s = String::new();
                for &p in ast.list(parts) {
                    match *ast.node(p) {
                        Node::TemplateElement {
                            cooked,
                            invalid_escape: false,
                        } => s.push_str(ast.atom(cooked)),
                        Node::TemplateElement { .. } => return None,
                        _ => s.push_str(&eval(cx, p, consts, depth + 1)?),
                    }
                }
                Some(s)
            }
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            } => {
                let mut s = eval(cx, left, consts, depth + 1)?;
                s.push_str(&eval(cx, right, consts, depth + 1)?);
                Some(s)
            }
            Node::Member {
                obj,
                optional: false,
                prop,
            } if cx.is_global_ref(obj, "Symbol") => {
                let name = ident_name(ast, prop)?;
                WELL_KNOWN_SYMBOLS
                    .contains(&name)
                    .then(|| format!("Symbol(Symbol.{name})"))
            }
            Node::Ident { .. } if consts => eval(cx, const_init(cx, id)?, consts, depth + 1),
            _ => None,
        }
    }
    eval(cx, id, consts, 0)
}

const WELL_KNOWN_SYMBOLS: &[&str] = &[
    "asyncDispose",
    "asyncIterator",
    "dispose",
    "hasInstance",
    "isConcatSpreadable",
    "iterator",
    "match",
    "matchAll",
    "replace",
    "search",
    "species",
    "split",
    "toPrimitive",
    "toStringTag",
    "unscopables",
];
