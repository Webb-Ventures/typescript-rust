//! `unicorn/no-invalid-property-descriptor`: property descriptors passed to
//! `Object.defineProperty()` / `Reflect.defineProperty()` (third argument) and descriptor
//! maps passed to `Object.defineProperties()` / `Object.create()` (second argument).
//!
//! A descriptor (or map) is an object literal, possibly in TypeScript wrappers, or a
//! `const` (not exported) bound to one, declared before the use, whose every other
//! reference only hands it to another such call (a descriptor may also be a property value
//! of a map literal that is itself used that way). A descriptor is checked once. Problems:
//! a primitive in place of a descriptor; `get`/`set` with `value`/`writable`; an unknown
//! field (`writeable` gets a rename suggestion when there is no accessor and no
//! `writable`); a `get`/`set` field whose value is known not to be a function or
//! `undefined`. Keys are the static values of computed keys (any unknown key, spread or
//! non-primitive key skips the object); `__proto__: x` sets the prototype, so it is no
//! field, and on a descriptor it skips the check unless `x` is `null`.

use super::import_util::walk;
use super::prelude::*;
use super::unicorn_static::{Static, skip_ts_only, static_value_control_flow};
use rustc_hash::FxHashSet;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-property-descriptor",
        "Disallow invalid property descriptors.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoInvalidPropertyDescriptor>,
};

#[derive(Debug, Default)]
struct NoInvalidPropertyDescriptor;

const DESCRIPTOR_FIELDS: &[&str] = &[
    "value",
    "writable",
    "get",
    "set",
    "enumerable",
    "configurable",
];

/// A property key: a string, or a well-known symbol by its path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Str(String),
    Symbol(String),
}

/// Inside parentheses and TypeScript wrappers.
fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1024 {
        let next = skip_ts_only(ast, skip_parens(ast, id));
        if next == id {
            break;
        }
        id = next;
    }
    id
}

/// `id` with the parentheses and TypeScript wrappers around it.
fn outermost(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        match cx.ast().node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            _ => break,
        }
    }
    cur
}

fn is_primitive(v: &Static) -> bool {
    match v {
        Static::Builtin(p) => p.starts_with("Symbol."),
        _ => !matches!(
            v,
            Static::RegExp { .. } | Static::Array(_) | Static::Object(_)
        ),
    }
}

fn computed_key(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<Key> {
    let v = static_value_control_flow(cx, expr)?;
    if !is_primitive(&v) {
        return None;
    }
    match v {
        Static::Builtin(p) => Some(Key::Symbol(p)),
        v => v.to_js_string().map(Key::Str),
    }
}

/// The key of a property name node (`Ident`, literal or `ComputedKey`).
fn key_of(cx: &Ctx<'_, '_>, key: NodeId) -> Option<Key> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::Ident { name } => Some(Key::Str(ast.atom(name).to_owned())),
        Node::StringLit { value } => Some(Key::Str(ast.atom(value).to_owned())),
        Node::ComputedKey { expr } => computed_key(cx, expr),
        Node::PrivateName { .. } => None,
        _ => super::unicorn_static::static_value(cx, key)
            .and_then(|v| v.to_js_string())
            .map(Key::Str),
    }
}

/// The descriptor (or map) argument of a descriptor call, and whether it is a map.
fn descriptor_argument(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(NodeId, bool)> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    let (obj, method) = match *ast.node(callee) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } => (obj, ident_name(ast, prop).map(|s| Key::Str(s.to_owned()))),
        Node::Index {
            obj,
            optional: false,
            index,
        } => (obj, computed_key(cx, index)),
        _ => return None,
    };
    let object = ident_name(ast, obj)?;
    if object != "Object" && object != "Reflect" {
        return None;
    }
    let method = match method {
        Some(Key::Str(m)) => m,
        _ => return None,
    };
    let is_map = object == "Object" && (method == "defineProperties" || method == "create");
    if method != "defineProperty" && !is_map {
        return None;
    }
    let index = if is_map { 1 } else { 2 };
    let args = ast.list(args);
    if args.len() <= index
        || args[..=index]
            .iter()
            .any(|&a| ast.kind(a) == NodeKind::Spread)
    {
        return None;
    }
    Some((args[index], is_map))
}

/// The `const` (not exported) bound to an object literal that `id` names.
fn object_variable(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(SymbolId, NodeId, NodeId)> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(id)?;
    let [decl] = sem.symbol(s).decls.as_slice() else {
        return None;
    };
    if decl.kind != DeclKind::Const {
        return None;
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl.node)
    else {
        return None;
    };
    if name != decl.name {
        return None;
    }
    let stmt = cx.parent(decl.node)?;
    if let Node::VarDecl { modifiers, .. } = *ast.node(stmt)
        && modifiers.contains(Modifiers::EXPORT)
    {
        return None;
    }
    let init = unwrap(ast, init);
    (ast.kind(init) == NodeKind::ObjectLit).then_some((s, decl.node, init))
}

/// The argument at `node` is the descriptor (or map, per `is_map`) of a descriptor call.
fn is_descriptor_use(cx: &Ctx<'_, '_>, node: NodeId, is_map: bool) -> bool {
    let node = outermost(cx, node);
    cx.parent(node)
        .and_then(|p| descriptor_argument(cx, p))
        .is_some_and(|(arg, m)| m == is_map && arg == node)
}

fn is_safe_map_object(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
    let ast = cx.ast();
    if is_descriptor_use(cx, obj, true) {
        return true;
    }
    let node = outermost(cx, obj);
    let Some(decl) = cx.parent(node) else {
        return false;
    };
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl)
    else {
        return false;
    };
    init == node
        && object_variable(cx, name).is_some_and(|(s, ..)| is_safe_object_variable(cx, s, true))
}

fn is_safe_object_use(cx: &Ctx<'_, '_>, ident: NodeId, is_map: bool) -> bool {
    let ast = cx.ast();
    if is_descriptor_use(cx, ident, is_map) {
        return true;
    }
    if is_map {
        return false;
    }
    let node = outermost(cx, ident);
    let Some(prop) = cx.parent(node) else {
        return false;
    };
    let is_value = match *ast.node(prop) {
        Node::Property { value, .. } => value == node,
        Node::ShorthandProp {
            name, init: None, ..
        } => name == node,
        _ => false,
    };
    is_value
        && cx
            .parent(prop)
            .is_some_and(|o| ast.kind(o) == NodeKind::ObjectLit && is_safe_map_object(cx, o))
}

/// Read somewhere, and every reference is the initializer or a read that hands the object
/// to a descriptor call.
fn is_safe_object_variable(cx: &Ctx<'_, '_>, s: SymbolId, is_map: bool) -> bool {
    let sem = cx.semantic();
    let refs = &sem.symbol(s).refs;
    refs.iter().any(|&r| sem.reference(r).is_read())
        && refs.iter().all(|&r| {
            let r = sem.reference(r);
            r.flags.contains(RefFlags::INIT)
                || (r.is_read() && !r.is_write() && is_safe_object_use(cx, r.node, is_map))
        })
}

fn object_expression(cx: &Ctx<'_, '_>, node: NodeId, is_map: bool) -> Option<NodeId> {
    let ast = cx.ast();
    let node = unwrap(ast, node);
    if ast.kind(node) == NodeKind::ObjectLit {
        return Some(node);
    }
    let (s, decl, init) = object_variable(cx, node)?;
    if cx.span(decl).end > cx.span(node).start || !is_safe_object_variable(cx, s, is_map) {
        return None;
    }
    Some(init)
}

/// One field of an object literal: its key node, value node (absent for methods and
/// accessors) and whether it is a plain `key: value` (or shorthand) property.
#[derive(Clone, Copy)]
struct Field {
    prop: NodeId,
    key: NodeId,
    value: Option<NodeId>,
}

fn effective_properties(cx: &Ctx<'_, '_>, obj: NodeId, is_map: bool) -> Option<Vec<(Key, Field)>> {
    let ast = cx.ast();
    let Node::ObjectLit { props } = *ast.node(obj) else {
        return None;
    };
    let mut out: Vec<(Key, Field)> = Vec::new();
    for &p in ast.list(props) {
        let field = match *ast.node(p) {
            Node::Property { key, value, .. } => Field {
                prop: p,
                key,
                value: Some(value),
            },
            Node::ShorthandProp { name, .. } => Field {
                prop: p,
                key: name,
                value: Some(name),
            },
            Node::MethodDef { key, .. } => Field {
                prop: p,
                key,
                value: None,
            },
            _ => return None,
        };
        let key = key_of(cx, field.key)?;
        if key == Key::Str("__proto__".into())
            && let Node::Property { key: k, value, .. } = *ast.node(p)
            && ast.kind(k) != NodeKind::ComputedKey
        {
            if !is_map && ast.kind(unwrap(ast, value)) != NodeKind::NullLit {
                return None;
            }
            continue;
        }
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = field,
            None => out.push((key, field)),
        }
    }
    Some(out)
}

fn is_primitive_expression(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
        ),
        Node::TemplateLit { .. } | Node::Unary { .. } | Node::Update { .. } => true,
        _ => false,
    }
}

fn is_non_object_descriptor(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let node = unwrap(ast, node);
    if is_primitive_expression(ast, node)
        || matches!(
            ast.kind(node),
            NodeKind::NullLit
                | NodeKind::BoolLit
                | NodeKind::NumberLit
                | NodeKind::BigIntLit
                | NodeKind::StringLit
        )
    {
        return true;
    }
    static_value_control_flow(cx, node).is_some_and(|v| is_primitive(&v))
}

fn is_non_callable_accessor(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let node = unwrap(ast, node);
    if let Node::Unary {
        op: UnaryOp::Void, ..
    } = *ast.node(node)
    {
        return false;
    }
    if is_primitive_expression(ast, node)
        || matches!(
            ast.kind(node),
            NodeKind::NullLit
                | NodeKind::BoolLit
                | NodeKind::NumberLit
                | NodeKind::BigIntLit
                | NodeKind::StringLit
                | NodeKind::RegexLit
                | NodeKind::ArrayLit
                | NodeKind::ObjectLit
        )
    {
        return true;
    }
    static_value_control_flow(cx, node)
        .is_some_and(|v| v != Static::Undefined && v.type_of() != "function")
}

/// The span ESTree gives a property's key (the expression inside `[...]`).
fn key_span(cx: &Ctx<'_, '_>, key: NodeId) -> Span {
    match *cx.ast().node(key) {
        Node::ComputedKey { expr } => cx.span(skip_parens(cx.ast(), expr)),
        _ => cx.span(key),
    }
}

fn writeable_suggestion(cx: &Ctx<'_, '_>, field: Field) -> Option<Fix> {
    let ast = cx.ast();
    let span = key_span(cx, field.key);
    if !ast.comments_in(span).is_empty() {
        return None;
    }
    let text = match *ast.node(field.prop) {
        Node::ShorthandProp { .. } => format!("writable: {}", cx.text(field.key)),
        _ if matches!(
            ast.kind(field.key),
            NodeKind::ComputedKey | NodeKind::StringLit | NodeKind::NumberLit
        ) =>
        {
            "'writable'".to_owned()
        }
        _ => "writable".to_owned(),
    };
    Some(Fix::replace(span, text))
}

fn check_descriptor(cx: &mut Ctx<'_, '_>, node: NodeId, checked: &mut FxHashSet<NodeId>) {
    let ast = cx.ast();
    let Some(descriptor) = object_expression(cx, node, false) else {
        if is_non_object_descriptor(cx, node) {
            cx.report_node(
                skip_parens(ast, node),
                "A property descriptor must be an object.",
            )
            .id("non-object-descriptor");
        }
        return;
    };
    if !checked.insert(descriptor) {
        return;
    }
    let Some(props) = effective_properties(cx, descriptor, false) else {
        return;
    };
    let has = |name: &str| props.iter().any(|(k, _)| *k == Key::Str(name.to_owned()));
    let has_accessor = has("get") || has("set");
    if has_accessor && (has("value") || has("writable")) {
        cx.report_node(
            descriptor,
            "A property descriptor cannot combine `get` or `set` with `value` or `writable`.",
        )
        .id("incompatible-fields");
    }
    let has_writable = has("writable");
    for (key, field) in &props {
        let name = match key {
            Key::Str(s) if DESCRIPTOR_FIELDS.contains(&s.as_str()) => s,
            _ => {
                let shown = match key {
                    Key::Str(s) => s.clone(),
                    Key::Symbol(_) => {
                        let s = key_span(cx, field.key);
                        cx.src()[s.start as usize..s.end as usize].to_owned()
                    }
                };
                let suggestion =
                    (*key == Key::Str("writeable".into()) && !has_accessor && !has_writable)
                        .then(|| writeable_suggestion(cx, *field))
                        .flatten();
                let span = key_span(cx, field.key);
                let r = cx
                    .report(
                        span,
                        format!("Unknown property descriptor field `{shown}`."),
                    )
                    .id("unknown-field");
                if let Some(fix) = suggestion {
                    r.suggest("rename-writeable", "Rename `writeable` to `writable`.", fix);
                }
                continue;
            }
        };
        if (name == "get" || name == "set")
            && let Some(value) = field.value
            && is_non_callable_accessor(cx, value)
        {
            cx.report_node(
                skip_parens(ast, value),
                format!("The `{name}` field must be a function or `undefined`."),
            )
            .id("non-callable-accessor");
        }
    }
}

impl Rule for NoInvalidPropertyDescriptor {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut calls = Vec::new();
        walk(ast, |id| {
            if ast.kind(id) == NodeKind::Call {
                calls.push(id);
            }
        });
        let mut checked: FxHashSet<NodeId> = FxHashSet::default();
        for call in calls {
            let Some((arg, is_map)) = descriptor_argument(cx, call) else {
                continue;
            };
            if !is_map {
                check_descriptor(cx, arg, &mut checked);
                continue;
            }
            let Some(props) =
                object_expression(cx, arg, true).and_then(|o| effective_properties(cx, o, true))
            else {
                continue;
            };
            for (_, field) in props {
                if ast.kind(field.prop) != NodeKind::MethodDef
                    && let Some(value) = field.value
                {
                    check_descriptor(cx, value, &mut checked);
                }
            }
        }
    }
}
