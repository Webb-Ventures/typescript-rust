//! `unicorn/no-collection-bracket-access`: `map[key]` on a value guessed to be a `Map`,
//! `WeakMap`, `Set` or `WeakSet` reads, writes or deletes a plain property, not an entry.
//! Keys that statically name a real member (`map["get"]`, `map[Symbol.iterator]`, every
//! branch of a conditional) are fine; keys whose value depends on a side effect or a getter
//! are skipped. A statement-level `map[k] = v` (maps only) or `delete c[k]` gets a
//! `set`/`delete` suggestion when the receiver is simple and no comment would go.

use super::prelude::*;
use super::unicorn_static::{
    Static, const_definition, has_side_effect_with_getters, static_value,
    static_value_control_flow, static_value_no_side_effects,
};
use super::unicorn_text::{
    call_argument_text, is_written, member_object_text, needs_semicolon, paren_span, paren_text,
    would_remove_comments,
};
use super::unicorn_types::{is_map, is_set, is_weak_map, is_weak_set};
use super::unicorn_util::skip_wrappers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-collection-bracket-access",
        "Disallow accessing `Map`, `Set`, `WeakMap`, and `WeakSet` entries with bracket notation.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoCollectionBracketAccess>,
};

#[derive(Debug, Default)]
struct NoCollectionBracketAccess;

/// Properties every object has through `Object.prototype`.
const OBJECT_MEMBERS: &[&str] = &[
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

struct Collection {
    name: &'static str,
    is: fn(&Ctx<'_, '_>, NodeId) -> bool,
    members: &'static [&'static str],
    symbols: &'static [&'static str],
    has_set: bool,
}

const COLLECTIONS: &[Collection] = &[
    Collection {
        name: "Map",
        is: is_map,
        members: &[
            "get", "set", "has", "delete", "clear", "entries", "forEach", "keys", "values", "size",
        ],
        symbols: &["iterator", "toStringTag"],
        has_set: true,
    },
    Collection {
        name: "WeakMap",
        is: is_weak_map,
        members: &["get", "set", "has", "delete"],
        symbols: &["toStringTag"],
        has_set: true,
    },
    Collection {
        name: "Set",
        is: is_set,
        members: &[
            "add",
            "clear",
            "delete",
            "entries",
            "forEach",
            "has",
            "keys",
            "size",
            "values",
            "union",
            "intersection",
            "difference",
            "symmetricDifference",
            "isSubsetOf",
            "isSupersetOf",
            "isDisjointFrom",
        ],
        symbols: &["iterator", "toStringTag"],
        has_set: false,
    },
    Collection {
        name: "WeakSet",
        is: is_weak_set,
        members: &["add", "delete", "has"],
        symbols: &["toStringTag"],
        has_set: false,
    },
];

/// Well-known symbols (`Symbol.iterator`, ...), by name.
const WELL_KNOWN_SYMBOLS: &[&str] = &[
    "asyncIterator",
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

/// A static key value: a string, a well-known symbol, or anything else.
enum Key {
    Str(String),
    Symbol(String),
    Other,
}

fn key_of(v: Static) -> Key {
    match v {
        Static::Str(s) => Key::Str(s),
        Static::Builtin(path) => match path.strip_prefix("Symbol.") {
            Some(name) if WELL_KNOWN_SYMBOLS.contains(&name) => Key::Symbol(name.to_owned()),
            _ => Key::Other,
        },
        _ => Key::Other,
    }
}

/// `Symbol.x` / `Symbol[k]` on the global `Symbol` (directly or as a `const`
/// initializer): the symbol it names.
fn well_known_symbol(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Key> {
    let ast = cx.ast();
    let mut node = skip_parens(ast, id);
    if let Some((_, init)) = const_definition(cx, node) {
        node = init;
    }
    let node = skip_wrappers(ast, node);
    let obj = match *ast.node(node) {
        Node::Member { obj, .. } => obj,
        Node::Index { obj, index, .. } => {
            static_value_no_side_effects(cx, index)?;
            obj
        }
        _ => return None,
    };
    if !matches!(ast.node(obj), Node::Ident { .. }) || !cx.is_global_ref(obj, "Symbol") {
        return None;
    }
    static_value(cx, node).map(key_of)
}

/// The static values the key can take: `Err(())` to skip the check, `Ok(None)` when unknown.
fn property_values(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Result<Option<Vec<Key>>, ()> {
    let ast = cx.ast();
    if depth > 100 {
        return Ok(None);
    }
    let node = skip_wrappers(ast, id);
    let branches = match *ast.node(node) {
        Node::Conditional { cons, alt, .. } => Some([cons, alt]),
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } => Some([left, right]),
        _ => None,
    };
    if (branches.is_some() || matches!(ast.node(node), Node::Seq { .. }))
        && has_side_effect_with_getters(ast, node)
    {
        return Err(());
    }
    let value = if branches.is_some() {
        static_value_control_flow(cx, node)
    } else {
        static_value_no_side_effects(cx, node)
    };
    if let Some(v) = value {
        return Ok(Some(vec![key_of(v)]));
    }
    if let Some(k) = well_known_symbol(cx, node) {
        return Ok(Some(vec![k]));
    }
    let Some(children) = branches else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for c in children {
        match property_values(cx, c, depth + 1)? {
            Some(v) => out.extend(v),
            None => return Ok(None),
        }
    }
    Ok(Some(out))
}

/// The parent with parentheses skipped, and the child on the path to it.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

impl Rule for NoCollectionBracketAccess {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Index {
            obj,
            index,
            optional,
        } = *ast.node(node)
        else {
            return;
        };
        let Some(collection) = COLLECTIONS.iter().find(|c| (c.is)(cx, obj)) else {
            return;
        };
        match property_values(cx, index, 0) {
            Err(()) => return,
            Ok(Some(values))
                if values.iter().all(|v| match v {
                    Key::Str(s) => {
                        collection.members.contains(&s.as_str())
                            || OBJECT_MEMBERS.contains(&s.as_str())
                    }
                    Key::Symbol(s) => collection.symbols.contains(&s.as_str()),
                    Key::Other => false,
                }) =>
            {
                return;
            }
            Ok(_) => {}
        }
        let type_name = collection.name;
        let Some((parent, child)) = parent_np(cx, node) else {
            return;
        };
        let object = skip_parens(ast, obj);
        let key = call_argument_text(cx, index);
        let mut kept = vec![paren_span(cx, object), paren_span(cx, index)];
        let (message, suggestion) = match *ast.node(parent) {
            Node::Unary {
                op: UnaryOp::Delete,
                arg,
            } if arg == child => (
                format!("Do not delete from a `{type_name}` using bracket notation."),
                Some((
                    "no-collection-bracket-access/suggestion-delete",
                    format!("Switch to `{type_name}#delete()`."),
                    format!("{}.delete({key})", member_object_text(cx, object, parent)),
                )),
            ),
            _ if is_written(cx, node) => {
                let suggestion = match *ast.node(parent) {
                    Node::Assign {
                        left,
                        op: AssignOp::Assign,
                        right,
                    } if collection.has_set && left == child => {
                        kept.push(paren_span(cx, right));
                        Some((
                            "no-collection-bracket-access/suggestion-set",
                            format!("Switch to `{type_name}#set()`."),
                            format!(
                                "{}.set({key}, {})",
                                member_object_text(cx, object, parent),
                                paren_text(cx, right)
                            ),
                        ))
                    }
                    _ => None,
                };
                (
                    format!("Do not write to a `{type_name}` using bracket notation."),
                    suggestion,
                )
            }
            _ => (
                format!("Do not read from a `{type_name}` using bracket notation."),
                None,
            ),
        };
        let is_delete =
            matches!(*ast.node(parent), Node::Unary { op: UnaryOp::Delete, arg } if arg == child);
        let message_id = if is_delete {
            "no-collection-bracket-access/delete"
        } else if is_written(cx, node) {
            "no-collection-bracket-access/write"
        } else {
            "no-collection-bracket-access/read"
        };
        let statement_level = parent_np(cx, parent)
            .is_some_and(|(gp, _)| matches!(cx.node(gp), Node::ExprStmt { .. }));
        let simple_object = matches!(
            ast.node(object),
            Node::Ident { .. } | Node::This {} | Node::Member { .. } | Node::Index { .. }
        );
        let suggestion = suggestion.filter(|_| {
            !optional
                && simple_object
                && statement_level
                && !would_remove_comments(cx, cx.span(parent), &kept)
        });
        let suggestion = suggestion.map(|(id, desc, replacement)| {
            let span = cx.span(parent);
            let prefix = if needs_semicolon(cx, span.start, &replacement) {
                ";"
            } else {
                ""
            };
            (
                id,
                desc,
                Fix::replace(span, format!("{prefix}{replacement}")),
            )
        });
        let report = cx.report_node(node, message).id(message_id);
        if let Some((id, desc, fix)) = suggestion {
            report.suggest(id, desc, fix);
        }
    }
}
