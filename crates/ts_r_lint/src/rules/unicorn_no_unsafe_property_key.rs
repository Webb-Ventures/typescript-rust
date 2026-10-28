//! `unicorn/no-unsafe-property-key`: a property key that is an object, array, function,
//! class, bigint or a number that does not survive the string round trip
//! (`9007199254740992`, `NaN`, `Infinity`) silently becomes an odd string key.
//!
//! Checked keys: computed member accesses (`a[k]`, `a?.[k]`) and the keys of object
//! properties, methods and class members when computed or written as a bigint or unsafe
//! number literal. A key is classified, without type information, as unsafe, safe or
//! unknown:
//!
//! - template literals are safe; object/array/function/class/`new` expressions and bigint
//!   or unsafe number literals are unsafe;
//! - a global built-in (`Array`, `Math`, `Date`, `parseInt`, the error classes, ...),
//!   also through `globalThis`/`self`/`window`, is unsafe; a TS enum member is safe;
//! - a name declared as a function, class or enum is unsafe, a `const` follows its
//!   initializer, and otherwise its TS annotation decides (resolved through local aliases
//!   and interfaces from the declaration's scope; object types with members, arrays,
//!   tuples, functions, constructors, `bigint`, `object` and classes are unsafe);
//! - `as`/`satisfies`/`<T>` look at the expression first, then at the written type;
//!   a conditional is unsafe when either branch is, safe when both are;
//! - otherwise a value known statically (without side effects, or a global's member)
//!   decides, and the shared array check applies.
//!
//! An unknown key is reported only when the shared type guess
//! ([`super::unicorn_types::Checker`]) says it is unsafe.

use super::prelude::*;
use super::unicorn_static::{Static, is_global_ident, static_value, static_value_no_side_effects};
use super::unicorn_types::{Checker, Guess, binding_annotation, is_array, type_definition};
use ts_r_ast::{Ast, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-property-key",
        "Disallow unsafe values as property keys.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUnsafePropertyKey>,
};

#[derive(Debug, Default)]
struct NoUnsafePropertyKey;

const GLOBAL_OBJECTS: [&str; 3] = ["globalThis", "self", "window"];

/// Built-ins that are objects or functions (constructors, namespaces, global functions).
const UNSAFE_GLOBALS: &[&str] = &[
    "BigInt",
    "Boolean",
    "Number",
    "String",
    "Symbol",
    "Object",
    "Array",
    "ArrayBuffer",
    "DataView",
    "Date",
    "Function",
    "Map",
    "WeakMap",
    "Set",
    "WeakSet",
    "Promise",
    "RegExp",
    "SharedArrayBuffer",
    "Proxy",
    "WeakRef",
    "FinalizationRegistry",
    "DisposableStack",
    "AsyncDisposableStack",
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
    "SuppressedError",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
    "Atomics",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "eval",
    "globalThis",
    "Infinity",
    "Intl",
    "isFinite",
    "isNaN",
    "JSON",
    "Math",
    "NaN",
    "parseFloat",
    "parseInt",
    "Reflect",
    "WebAssembly",
];

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    Unsafe,
    Safe,
    Unknown,
}

fn is_unsafe_number(v: f64) -> bool {
    let safe_integer = v.is_finite() && v.trunc() == v && v.abs() <= 9_007_199_254_740_991.0;
    !safe_integer && (!v.is_finite() || v.trunc() == v)
}

fn negated_literal(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => Some(arg),
        _ => None,
    }
}

fn is_bigint_literal(ast: &Ast, id: NodeId) -> bool {
    let lit = negated_literal(ast, id).unwrap_or(id);
    matches!(ast.node(lit), Node::BigIntLit { .. })
}

fn is_unsafe_number_literal(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::NumberLit { value } => is_unsafe_number(value.get()),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => match *ast.node(arg) {
            Node::NumberLit { value } => is_unsafe_number(-value.get()),
            _ => false,
        },
        _ => false,
    }
}

fn is_unsafe_node(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::New { .. }
    ) || is_bigint_literal(ast, id)
        || is_unsafe_number_literal(ast, id)
}

/// A TS type node that is unsafe by itself.
fn is_unsafe_type_node(ast: &Ast, ty: NodeId) -> bool {
    match *ast.node(ty) {
        Node::KeywordType {
            kind: KeywordTypeKind::BigInt | KeywordTypeKind::Object,
        }
        | Node::ArrayType { .. }
        | Node::TupleType { .. }
        | Node::FunctionType { .. }
        | Node::ConstructorType { .. } => true,
        _ => is_bigint_literal(ast, ty) || is_unsafe_number_literal(ast, ty),
    }
}

fn is_unsafe_static(v: &Static) -> bool {
    match v {
        Static::BigInt(_) | Static::Array(_) | Static::Object(_) | Static::RegExp { .. } => true,
        // Well-known symbols are symbols; every other built-in is an object or function.
        Static::Builtin(path) => !path
            .strip_prefix("Symbol.")
            .is_some_and(|s| WELL_KNOWN_SYMBOLS.contains(&s)),
        Static::Num(n) => is_unsafe_number(*n),
        _ => false,
    }
}

fn static_kind(v: &Static) -> K {
    if is_unsafe_static(v) {
        K::Unsafe
    } else {
        K::Safe
    }
}

fn checker_node(cx: &Ctx<'_, '_>, id: NodeId, _recurse: &dyn Fn(NodeId) -> bool) -> bool {
    is_unsafe_node(cx.ast(), id)
}

fn checker_static(v: &Static, _: &Ast, _: NodeId) -> Guess {
    if is_unsafe_static(v) {
        Guess::Target
    } else {
        Guess::NonTarget
    }
}

static UNSAFE_KEY: Checker = Checker {
    target_names: &["Array", "ReadonlyArray"],
    target_node: Some(checker_node),
    target_annotation: Some(is_unsafe_type_node),
    static_type: Some(checker_static),
    ..Checker::DEFAULT
};

struct An<'c, 'x, 'f> {
    cx: &'c Ctx<'x, 'f>,
    visited: Vec<SymbolId>,
    visited_types: Vec<SymbolId>,
    depth: u32,
}

impl An<'_, '_, '_> {
    fn symbol_of(&self, ident: NodeId) -> Option<SymbolId> {
        let sem = self.cx.semantic();
        sem.reference_at(ident)
            .and_then(|r| sem.reference(r).symbol())
    }

    /// The plugin's annotation check, names resolved from `scope`.
    fn unsafe_annotation(&mut self, ty: NodeId, scope: ScopeId) -> bool {
        if self.depth > 200 {
            return false;
        }
        self.depth += 1;
        let r = self.unsafe_annotation_inner(ty, scope);
        self.depth -= 1;
        r
    }

    fn unsafe_annotation_inner(&mut self, ty: NodeId, scope: ScopeId) -> bool {
        let ast = self.cx.ast();
        match *ast.node(ty) {
            Node::ParenType { ty } => self.unsafe_annotation(ty, scope),
            Node::TypeOperator {
                op: TypeOperatorKind::Readonly,
                ty,
            } => self.unsafe_annotation(ty, scope),
            Node::TypeOperator { .. } => false,
            Node::TypeLiteral { members } => !ast.list(members).is_empty(),
            Node::TypeRef { name, .. } => self.unsafe_type_name(name, scope),
            Node::UnionType { types } => ast
                .list(types)
                .iter()
                .any(|&t| self.unsafe_annotation(t, scope)),
            Node::IntersectionType { types } => ast
                .list(types)
                .iter()
                .all(|&t| self.unsafe_annotation(t, scope)),
            _ => is_unsafe_type_node(ast, ty),
        }
    }

    fn unsafe_type_name(&mut self, name: NodeId, scope: ScopeId) -> bool {
        let ast = self.cx.ast();
        let Node::Ident { name: atom } = *ast.node(name) else {
            return false;
        };
        let n = ast.atom(atom);
        if n == "Array" || n == "ReadonlyArray" {
            return true;
        }
        let Some((sym, decl)) = type_definition(self.cx, n, scope) else {
            return false;
        };
        if self.visited_types.contains(&sym) {
            return false;
        }
        self.visited_types.push(sym);
        let def_scope = self.cx.semantic().enclosing_scope(decl.name);
        let r = match (decl.kind, *ast.node(decl.node)) {
            (DeclKind::TypeAlias, Node::TypeAlias { ty, .. }) => {
                self.unsafe_annotation(ty, def_scope)
            }
            (DeclKind::Interface, Node::Interface { extends, body, .. }) => {
                !ast.list(body).is_empty()
                    || ast.list(extends).iter().any(|&e| {
                        let e = match *ast.node(e) {
                            Node::ExprWithTypeArgs { expr, .. } => expr,
                            _ => e,
                        };
                        self.unsafe_type_name(e, def_scope)
                    })
            }
            (DeclKind::Class, _) => true,
            _ => false,
        };
        self.visited_types.pop();
        r
    }

    fn kind(&mut self, id: NodeId) -> K {
        if self.depth > 200 {
            return K::Unknown;
        }
        self.depth += 1;
        let r = self.kind_inner(id);
        self.depth -= 1;
        r
    }

    fn kind_inner(&mut self, id: NodeId) -> K {
        let cx = self.cx;
        let ast = cx.ast();
        match *ast.node(id) {
            Node::TemplateLit { .. } => K::Safe,
            Node::Ident { .. } => self.ident_kind(id),
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let obj = skip_parens(ast, obj);
                let is_enum = matches!(ast.node(obj), Node::Ident { .. })
                    && self.symbol_of(obj).is_some_and(|s| {
                        cx.semantic()
                            .symbol(s)
                            .decls
                            .iter()
                            .any(|d| d.kind == DeclKind::Enum)
                    });
                if is_enum {
                    K::Safe
                } else {
                    self.expression_kind(id)
                }
            }
            Node::As { expr, ty }
            | Node::TypeAssertion { expr, ty }
            | Node::Satisfies { expr, ty } => {
                let k = self.kind(expr);
                if k != K::Unknown {
                    return k;
                }
                let scope = cx.semantic().enclosing_scope(id);
                if self.unsafe_annotation(ty, scope) {
                    K::Unsafe
                } else {
                    K::Unknown
                }
            }
            Node::NonNull { expr } | Node::Paren { expr } => self.kind(expr),
            Node::Seq { exprs } => match ast.list(exprs).last() {
                Some(&last) => self.kind(last),
                None => K::Unknown,
            },
            Node::Conditional { cons, alt, .. } => {
                let a = self.kind(cons);
                let b = self.kind(alt);
                if a == K::Unsafe || b == K::Unsafe {
                    K::Unsafe
                } else if a == K::Safe && b == K::Safe {
                    K::Safe
                } else {
                    K::Unknown
                }
            }
            _ => self.expression_kind(id),
        }
    }

    fn ident_kind(&mut self, id: NodeId) -> K {
        let cx = self.cx;
        let ast = cx.ast();
        if let Some(name) = ident_name(ast, id)
            && UNSAFE_GLOBALS.contains(&name)
            && is_global_ident(cx, id, name)
        {
            return K::Unsafe;
        }
        let Some(sym) = self.symbol_of(id) else {
            return K::Unknown;
        };
        let sem = cx.semantic();
        let decls = &sem.symbol(sym).decls;
        let Some(first) = decls.first().copied() else {
            return K::Unknown;
        };
        if matches!(
            first.kind,
            DeclKind::Function | DeclKind::Class | DeclKind::Enum
        ) {
            return K::Unsafe;
        }
        if decls.len() == 1
            && first.kind == DeclKind::Const
            && !self.visited.contains(&sym)
            && let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(first.node)
            && name == first.name
        {
            self.visited.push(sym);
            let k = self.kind(init);
            self.visited.pop();
            if k != K::Unknown {
                return k;
            }
        }
        let (ann, _) = binding_annotation(cx, first.name);
        let Some(ann) = ann else {
            return K::Unknown;
        };
        let scope = sem.enclosing_scope(first.name);
        if self.unsafe_annotation(ann, scope) {
            K::Unsafe
        } else {
            K::Unknown
        }
    }

    fn is_unsafe_global_member(&self, id: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let Some((obj, name)) = member_parts(ast, cx.src(), id) else {
            return false;
        };
        let obj = skip_parens(ast, obj);
        ident_name(ast, obj)
            .is_some_and(|o| GLOBAL_OBJECTS.contains(&o) && is_global_ident(cx, obj, o))
            && UNSAFE_GLOBALS.contains(&name.as_ref())
    }

    fn expression_kind(&mut self, id: NodeId) -> K {
        let cx = self.cx;
        let ast = cx.ast();
        let mut value = static_value_no_side_effects(cx, id);
        if value.is_none()
            && let Node::Member { obj, .. } | Node::Index { obj, .. } = *ast.node(id)
            && let obj = skip_parens(ast, obj)
            && let Some(o) = ident_name(ast, obj)
            && is_global_ident(cx, obj, o)
        {
            value = static_value(cx, id);
        }
        if is_unsafe_node(ast, id)
            || self.is_unsafe_global_member(id)
            || is_array(cx, id)
            || value.as_ref().is_some_and(is_unsafe_static)
        {
            return K::Unsafe;
        }
        value.as_ref().map_or(K::Unknown, static_kind)
    }
}

fn check(cx: &mut Ctx<'_, '_>, key: NodeId) {
    let key = skip_parens(cx.ast(), key);
    let report_at = key;
    let mut an = An {
        cx: &*cx,
        visited: Vec::new(),
        visited_types: Vec::new(),
        depth: 0,
    };
    let k = an.kind(key);
    if k == K::Safe || (k == K::Unknown && !UNSAFE_KEY.is_target(cx, key)) {
        return;
    }
    cx.report_node(
        report_at,
        "Do not use an unsafe value as a property key. Use an explicit string or symbol key instead.",
    )
    .id("no-unsafe-property-key");
}

impl Rule for NoUnsafePropertyKey {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Index,
            NodeKind::Property,
            NodeKind::MethodDef,
            NodeKind::PropertyDef,
            NodeKind::BindingProperty,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let key = match *ast.node(node) {
            Node::Index { index, .. } => {
                check(cx, index);
                return;
            }
            Node::Property { key, .. }
            | Node::MethodDef { key, .. }
            | Node::PropertyDef { key, .. }
            | Node::BindingProperty { key, .. } => key,
            _ => return,
        };
        match *ast.node(key) {
            Node::ComputedKey { expr } => check(cx, expr),
            _ if is_bigint_literal(ast, key) || is_unsafe_number_literal(ast, key) => {
                check(cx, key)
            }
            _ => {}
        }
    }
}
