//! `unicorn/no-boolean-sort-comparator`: the comparator of `x.sort(c)` / `x.toSorted(c)`
//! (the name may be computed when statically known; optional chaining allowed; `x` not
//! known to be a non-indexed collection) returns a boolean.
//!
//! The comparator (parentheses and TS wrappers skipped) is reported when it is
//! - a non-async, non-generator function with a JSDoc `@returns` that is `boolean`, a
//!   boolean return annotation (`boolean`, `true`/`false`, a type predicate, aliases and
//!   unions of those) or a single returned expression that is boolean;
//! - a name bound to such a function: a never-reassigned function declaration (overloads:
//!   every signature; only in modules or nested scopes) or `const`/`let` initialized with a
//!   function, a JSDoc `@type`/`@param` callable returning `boolean`, or a TS annotation that
//!   is a function type returning boolean;
//! - `Boolean`, `isFinite`, `isNaN` or a known boolean static method (`Number.isNaN`,
//!   `Reflect.has`, ...) of the real global;
//! - wrapped in an assertion to a boolean-returning function type.
//!
//! Boolean expressions are comparisons, `in`/`instanceof`, `!`, `delete`, `&&`/`||`/`??` and
//! conditionals of booleans, boolean literals and static values, boolean-annotated or
//! `const` variables, boolean casts, `Boolean(…)`, boolean static methods, known boolean
//! instance methods (`includes`, `has`, `test`, ...) on receivers of a known kind, RegExp
//! flag properties and calls to boolean functions. The plugin's type-checker answers are
//! not used.
//!
//! A bare inline comparator `(a, b) => a > b` (two plain untyped parameters, no comments,
//! no type parameters or return type) comparing mirrored operands of the two parameters
//! gets a suggestion `(a, b) => a - b` (operands swapped for `<`/`<=`).

use super::prelude::*;
use super::unicorn_iter::unwrap_expression;
use super::unicorn_static::{Static, static_value, static_value_no_side_effects};
use super::unicorn_text::has_comment_in;
use super::unicorn_types::{
    BOOLEAN, Checker, DocAnnotation, Guess, binding_annotation, has_jsdoc_type_reference,
    has_native_type_assertion, is_indexed_collection, is_known_non_indexed_collection, is_regexp,
    is_string, jsdoc_callable_return, jsdoc_return_type, jsdoc_type_annotation, type_definition,
};
use super::unicorn_util::is_same_reference;
use crate::env::SourceType;
use crate::semantic::{RefFlags, Resolution, SymbolId};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-boolean-sort-comparator",
        "Disallow boolean-returning sort comparators.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoBooleanSortComparator>,
};

#[derive(Debug, Default)]
struct NoBooleanSortComparator;

const MAX_DEPTH: u32 = 64;

const BOOLEAN_GLOBAL_FUNCTIONS: &[&str] = &["isFinite", "isNaN"];

const BOOLEAN_STATIC_METHODS: &[(&str, &[&str])] = &[
    ("Array", &["isArray"]),
    ("ArrayBuffer", &["isView"]),
    ("Atomics", &["isLockFree"]),
    ("Error", &["isError"]),
    (
        "Number",
        &["isFinite", "isInteger", "isNaN", "isSafeInteger"],
    ),
    (
        "Object",
        &["hasOwn", "is", "isExtensible", "isFrozen", "isSealed"],
    ),
    (
        "Reflect",
        &[
            "defineProperty",
            "deleteProperty",
            "has",
            "isExtensible",
            "preventExtensions",
            "set",
            "setPrototypeOf",
        ],
    ),
    ("URL", &["canParse"]),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Array,
    KeyedCollection,
    RegExp,
    Set,
    String,
    WeakSet,
}

fn kind_methods(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Array => &["every", "includes", "some"],
        Kind::KeyedCollection => &["has"],
        Kind::RegExp => &["test"],
        Kind::Set => &["has", "isDisjointFrom", "isSubsetOf", "isSupersetOf"],
        Kind::String => &["endsWith", "includes", "isWellFormed", "startsWith"],
        Kind::WeakSet => &["has"],
    }
}

const REGEXP_FLAGS: &[&str] = &[
    "dotAll",
    "global",
    "hasIndices",
    "ignoreCase",
    "multiline",
    "sticky",
    "unicode",
    "unicodeSets",
];

static KEYED_COLLECTION: Checker = Checker {
    target_names: &[
        "Map",
        "ReadonlyMap",
        "WeakMap",
        "URLSearchParams",
        "Headers",
        "FormData",
    ],
    class_heritage: false,
    ..Checker::DEFAULT
};

static SET: Checker = Checker {
    target_names: &["Set", "ReadonlySet"],
    class_heritage: false,
    ..Checker::DEFAULT
};

static WEAK_SET: Checker = Checker {
    target_names: &["WeakSet"],
    class_heritage: false,
    ..Checker::DEFAULT
};

static MUTABLE_KEYED_COLLECTION: Checker = Checker {
    target_names: &["Map", "Set", "WeakMap", "WeakSet"],
    non_target_names: &["ReadonlyMap", "ReadonlySet"],
    target_ctors: &["Map", "Set", "WeakMap", "WeakSet"],
    class_heritage: false,
    ..Checker::DEFAULT
};

fn doc_is_boolean(a: &DocAnnotation) -> bool {
    BOOLEAN.jsdoc(a) == Guess::Target
}

fn symbol_of(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    match sem.reference(sem.reference_at(ident)?).resolution {
        Resolution::Symbol(s) => Some(s),
        _ => None,
    }
}

/// The static property name of `a.b` / `a[expr]`.
fn property_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => match ast.node(prop) {
            Node::Ident { .. } => ident_name(ast, prop).map(str::to_owned),
            _ => None,
        },
        Node::Index { index, .. } => static_value(cx, index)?.to_js_string(),
        _ => None,
    }
}

fn member_object(ast: &Ast, id: NodeId) -> Option<(NodeId, bool)> {
    match *ast.node(id) {
        Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
            Some((obj, optional))
        }
        _ => None,
    }
}

// ---- Type annotations -------------------------------------------------------------------

/// The alias or interface a type reference names.
fn referenced_type(cx: &Ctx<'_, '_>, ty: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::TypeRef { name, .. } = *ast.node(ty) else {
        return None;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return None;
    }
    let scope = cx.semantic().enclosing_scope(ty);
    let (_, decl) = type_definition(cx, ident_name(ast, name)?, scope)?;
    [decl.node, decl.name]
        .into_iter()
        .chain(cx.parent(decl.name))
        .find(|&n| matches!(ast.node(n), Node::TypeAlias { .. } | Node::Interface { .. }))
}

fn is_boolean_annotation(cx: &Ctx<'_, '_>, ty: Option<NodeId>, depth: u32) -> bool {
    let Some(ty) = ty else {
        return false;
    };
    if depth > MAX_DEPTH {
        return false;
    }
    let ast = cx.ast();
    match *ast.node(ty) {
        Node::ParenType { ty } => is_boolean_annotation(cx, Some(ty), depth + 1),
        Node::KeywordType {
            kind: KeywordTypeKind::Boolean,
        }
        | Node::BoolLit { .. } => true,
        Node::TypePredicate { asserts, .. } => !asserts,
        Node::UnionType { types } => ast
            .list(types)
            .iter()
            .all(|&t| is_boolean_annotation(cx, Some(t), depth + 1)),
        Node::TypeRef { .. } => match referenced_type(cx, ty).map(|d| *ast.node(d)) {
            Some(Node::TypeAlias { ty, .. }) => is_boolean_annotation(cx, Some(ty), depth + 1),
            _ => false,
        },
        _ => false,
    }
}

fn boolean_call_signatures(cx: &Ctx<'_, '_>, members: &[NodeId], depth: u32) -> bool {
    let ast = cx.ast();
    let signatures: Vec<Option<NodeId>> = members
        .iter()
        .filter_map(|&m| match *ast.node(m) {
            Node::CallSignature { return_type, .. } => Some(return_type),
            _ => None,
        })
        .collect();
    !signatures.is_empty()
        && signatures
            .into_iter()
            .all(|r| is_boolean_annotation(cx, r, depth + 1))
}

fn is_boolean_function_annotation(cx: &Ctx<'_, '_>, ty: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let ast = cx.ast();
    match *ast.node(ty) {
        Node::ParenType { ty } => is_boolean_function_annotation(cx, ty, depth + 1),
        Node::FunctionType { return_type, .. } => {
            is_boolean_annotation(cx, Some(return_type), depth + 1)
        }
        Node::TypeLiteral { members } => boolean_call_signatures(cx, ast.list(members), depth),
        Node::UnionType { types } => ast
            .list(types)
            .iter()
            .all(|&t| is_boolean_function_annotation(cx, t, depth + 1)),
        Node::TypeRef { .. } => match referenced_type(cx, ty).map(|d| *ast.node(d)) {
            Some(Node::TypeAlias { ty, .. }) => is_boolean_function_annotation(cx, ty, depth + 1),
            Some(Node::Interface { body, .. }) => {
                boolean_call_signatures(cx, ast.list(body), depth)
            }
            _ => false,
        },
        _ => false,
    }
}

/// An assertion on the way to `node` (`x as F`, `<F>x`, `x satisfies F`) to a
/// boolean-returning function type.
fn has_boolean_function_assertion(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = node;
    for _ in 0..10_000 {
        match *ast.node(node) {
            Node::Paren { expr } | Node::NonNull { expr } => node = expr,
            Node::As { expr, ty }
            | Node::Satisfies { expr, ty }
            | Node::TypeAssertion { ty, expr } => {
                if is_boolean_function_annotation(cx, ty, 0) {
                    return true;
                }
                node = expr;
            }
            _ => return false,
        }
    }
    false
}

// ---- Boolean expressions ----------------------------------------------------------------

struct Booleans<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    visited: Vec<SymbolId>,
}

/// The single expression a function returns: a concise body or the only `return`.
fn return_expression(ast: &Ast, f: NodeId) -> Option<NodeId> {
    let body = match *ast.node(f) {
        Node::Arrow { body, .. } => body,
        Node::FunctionExpr { body, .. } | Node::FunctionDecl { body, .. } => body?,
        _ => return None,
    };
    let Node::Block { stmts } = *ast.node(body) else {
        return Some(body);
    };
    match *ast.list(stmts) {
        [only] => match *ast.node(only) {
            Node::Return { arg } => arg,
            _ => None,
        },
        _ => None,
    }
}

fn function_parts(ast: &Ast, f: NodeId) -> Option<(FnFlags, Option<NodeId>, bool)> {
    match *ast.node(f) {
        Node::Arrow {
            flags, return_type, ..
        } => Some((flags, return_type, true)),
        Node::FunctionExpr {
            flags,
            return_type,
            body,
            ..
        }
        | Node::FunctionDecl {
            flags,
            return_type,
            body,
            ..
        } => Some((flags, return_type, body.is_some())),
        _ => None,
    }
}

fn is_function_node(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::FunctionDecl { .. }
    )
}

impl Booleans<'_, '_, '_> {
    fn enter(&mut self, sym: SymbolId) -> bool {
        if self.visited.contains(&sym) || self.visited.len() > MAX_DEPTH as usize {
            return false;
        }
        self.visited.push(sym);
        true
    }

    fn leave(&mut self, sym: SymbolId) {
        if let Some(i) = self.visited.iter().rposition(|&s| s == sym) {
            self.visited.remove(i);
        }
    }

    fn function(&mut self, f: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let Some((flags, return_type, has_body)) = function_parts(ast, f) else {
            return false;
        };
        if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) {
            return false;
        }
        if let Some(a) = jsdoc_return_type(cx, f) {
            return doc_is_boolean(&a);
        }
        if is_boolean_annotation(cx, return_type, 0) {
            return true;
        }
        if !has_body {
            return false;
        }
        match return_expression(ast, f) {
            Some(e) => self.expression(e),
            None => false,
        }
    }

    fn stable_function_name(&self, sym: SymbolId) -> bool {
        let cx = self.cx;
        let sem = cx.semantic();
        let symbol = sem.symbol(sym);
        let all_functions =
            !symbol.decls.is_empty() && symbol.decls.iter().all(|d| d.kind == DeclKind::Function);
        if !all_functions {
            return false;
        }
        let expression_name = symbol
            .decls
            .iter()
            .all(|d| matches!(cx.ast().node(d.node), Node::FunctionExpr { .. }));
        expression_name || cx.env().source_type == SourceType::Module
    }

    fn function_reference(&mut self, node: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        if !matches!(ast.node(node), Node::Ident { .. }) {
            return false;
        }
        let Some(sym) = symbol_of(cx, node) else {
            return false;
        };
        if !self.enter(sym) {
            return false;
        }
        let result = self.function_reference_of(sym);
        self.leave(sym);
        result
    }

    fn function_reference_of(&mut self, sym: SymbolId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let symbol = sem.symbol(sym);
        let writes = |init_ok: bool| {
            symbol.refs.iter().any(|&r| {
                let r = sem.reference(r);
                r.is_write() && !(init_ok && r.flags.contains(RefFlags::INIT))
            })
        };
        if symbol.decls.len() > 1 {
            if !self.stable_function_name(sym) || writes(false) {
                return false;
            }
            let overloads: Vec<NodeId> = symbol
                .decls
                .iter()
                .map(|d| d.node)
                .filter(|&n| matches!(ast.node(n), Node::FunctionDecl { body: None, .. }))
                .collect();
            let definitions: Vec<NodeId> = if overloads.is_empty() {
                symbol.decls.iter().map(|d| d.node).collect()
            } else {
                overloads
            };
            return definitions.into_iter().all(|f| self.function(f));
        }
        let [decl] = symbol.decls[..] else {
            return false;
        };
        if let Some(a) = jsdoc_type_annotation(cx, decl.name) {
            let initializer = match *ast.node(decl.node) {
                Node::VarDeclarator { init, .. } => init,
                Node::Param { init, .. } => init,
                _ => None,
            };
            let value = initializer.map(|i| unwrap_expression(ast, i));
            let parts = value.and_then(|v| function_parts(ast, v));
            let is_param = matches!(ast.node(decl.node), Node::Param { .. });
            if parts.is_some_and(|(flags, _, _)| {
                flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR)
            }) || (is_param
                && (parts.is_some_and(|(_, r, _)| r.is_some())
                    || initializer.is_some_and(|i| has_native_type_assertion(ast, i))))
                || writes(true)
            {
                return false;
            }
            if let Some((_, Some(r), _)) = parts {
                return is_boolean_annotation(cx, Some(r), 0);
            }
            let ret = a.ty.as_ref().and_then(jsdoc_callable_return);
            return !a.optional
                && ret.is_some_and(|t| {
                    doc_is_boolean(&DocAnnotation {
                        ty: Some(t),
                        optional: false,
                    })
                });
        }
        if let (Some(t), _) = binding_annotation(cx, decl.name) {
            return is_boolean_function_annotation(cx, t, 0);
        }
        let function = if self.stable_function_name(sym) {
            (!writes(false)).then_some(decl.node)
        } else {
            match *ast.node(decl.node) {
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } if name == decl.name && !writes(true) => {
                    let mut init = skip_parens(ast, init);
                    for _ in 0..10_000 {
                        match *ast.node(init) {
                            Node::Satisfies { expr, .. } | Node::NonNull { expr } => {
                                init = skip_parens(ast, expr);
                            }
                            _ => break,
                        }
                    }
                    matches!(
                        ast.node(init),
                        Node::Arrow { .. } | Node::FunctionExpr { .. }
                    )
                    .then_some(init)
                }
                _ => None,
            }
        };
        let Some(f) = function.filter(|&f| is_function_node(ast, f)) else {
            return false;
        };
        let return_type = function_parts(ast, f).and_then(|(_, r, _)| r);
        (return_type.is_none() || is_boolean_annotation(cx, return_type, 0)) && self.function(f)
    }

    fn variable_value(&mut self, ident: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let Some(sym) = symbol_of(cx, ident) else {
            return false;
        };
        let sem = cx.semantic();
        let [decl] = sem.symbol(sym).decls[..] else {
            return false;
        };
        if !self.enter(sym) {
            return false;
        }
        let (annotation, optional) = binding_annotation(cx, decl.name);
        let result = if annotation.is_some() {
            !optional && is_boolean_annotation(cx, annotation, 0)
        } else {
            let is_const = decl.kind == DeclKind::Const;
            match *ast.node(decl.node) {
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } if is_const && name == decl.name => self.expression(init),
                _ => false,
            }
        };
        self.leave(sym);
        result
    }

    fn known_kind(&mut self, id: NodeId) -> Option<Kind> {
        let cx = self.cx;
        let ast = cx.ast();
        match *ast.node(id) {
            Node::ArrayLit { .. } => Some(Kind::Array),
            Node::StringLit { .. } | Node::TemplateLit { .. } => Some(Kind::String),
            Node::RegexLit { .. } => Some(Kind::RegExp),
            Node::NumberLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::BigIntLit { .. } => None,
            Node::New { callee, .. } => {
                let global = |n: &str| {
                    matches!(ast.node(callee), Node::Ident { .. }) && cx.is_global_ref(callee, n)
                };
                if global("RegExp") {
                    Some(Kind::RegExp)
                } else if ["Map", "WeakMap", "URLSearchParams", "Headers", "FormData"]
                    .into_iter()
                    .any(global)
                {
                    Some(Kind::KeyedCollection)
                } else if global("Set") {
                    Some(Kind::Set)
                } else if global("WeakSet") {
                    Some(Kind::WeakSet)
                } else {
                    self.type_checked_kind(id)
                }
            }
            Node::Ident { .. } => {
                let kind = self.type_checked_kind(id);
                if kind.is_some() || jsdoc_type_annotation(cx, id).is_some() {
                    return kind;
                }
                let sym = symbol_of(cx, id)?;
                let sem = cx.semantic();
                let [decl] = sem.symbol(sym).decls[..] else {
                    return None;
                };
                let Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } = *ast.node(decl.node)
                else {
                    return None;
                };
                if decl.kind != DeclKind::Const || name != decl.name || !self.enter(sym) {
                    return None;
                }
                let kind = self.known_kind(init);
                self.leave(sym);
                kind
            }
            Node::Paren { expr } | Node::NonNull { expr } | Node::Satisfies { expr, .. } => {
                self.known_kind(expr)
            }
            _ => self.type_checked_kind(id),
        }
    }

    fn type_checked_kind(&self, id: NodeId) -> Option<Kind> {
        let cx = self.cx;
        if is_indexed_collection(cx, id) {
            Some(Kind::Array)
        } else if KEYED_COLLECTION.is_target(cx, id) {
            Some(Kind::KeyedCollection)
        } else if is_regexp(cx, id) {
            Some(Kind::RegExp)
        } else if SET.is_target(cx, id) {
            Some(Kind::Set)
        } else if is_string(cx, id) {
            Some(Kind::String)
        } else if WEAK_SET.is_target(cx, id) {
            Some(Kind::WeakSet)
        } else {
            None
        }
    }

    fn call(&mut self, call: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(call) else {
            return false;
        };
        let callee = skip_parens(ast, callee);
        if let Some((obj, _)) = member_object(ast, callee) {
            if is_known_boolean_static_member(cx, callee, true) {
                return true;
            }
            let Some(method) = property_name(cx, callee) else {
                return false;
            };
            if method == "delete" {
                return MUTABLE_KEYED_COLLECTION.is_target(cx, obj);
            }
            return self
                .known_kind(skip_parens(ast, obj))
                .is_some_and(|k| kind_methods(k).contains(&method.as_str()));
        }
        if !matches!(ast.node(callee), Node::Ident { .. }) {
            return false;
        }
        if cx.is_global_ref(callee, "Boolean")
            || BOOLEAN_GLOBAL_FUNCTIONS
                .iter()
                .any(|n| cx.is_global_ref(callee, n))
        {
            return true;
        }
        self.function_reference(callee)
    }

    fn expression(&mut self, id: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let id = skip_parens(ast, id);
        if self.visited.len() > MAX_DEPTH as usize {
            return false;
        }
        if let Some(a) = jsdoc_type_annotation(cx, id) {
            return doc_is_boolean(&a);
        }
        if is_boolean_static_value(cx, id) {
            return true;
        }
        match *ast.node(id) {
            Node::Ident { .. } => self.variable_value(id),
            Node::Unary { op, .. } => matches!(op, UnaryOp::Not | UnaryOp::Delete),
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right,
            } => self.expression(left) && self.expression(right),
            Node::Binary { op, .. } => matches!(
                op,
                BinaryOp::Gt
                    | BinaryOp::GtEq
                    | BinaryOp::Lt
                    | BinaryOp::LtEq
                    | BinaryOp::Eq
                    | BinaryOp::StrictEq
                    | BinaryOp::NotEq
                    | BinaryOp::StrictNotEq
                    | BinaryOp::In
                    | BinaryOp::InstanceOf
            ),
            Node::Conditional { cons, alt, .. } => self.expression(cons) && self.expression(alt),
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                is_boolean_annotation(cx, Some(ty), 0)
                    || (matches!(
                        ast.node(ty),
                        Node::KeywordType {
                            kind: KeywordTypeKind::Any | KeywordTypeKind::Unknown
                        }
                    ) && self.expression(expr))
            }
            Node::Satisfies { expr, ty } => {
                is_boolean_annotation(cx, Some(ty), 0) || self.expression(expr)
            }
            Node::Call { .. } => self.call(id),
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                !optional
                    && property_name(cx, id).is_some_and(|p| REGEXP_FLAGS.contains(&p.as_str()))
                    && self.known_kind(skip_parens(ast, obj)) == Some(Kind::RegExp)
            }
            Node::Await { arg } => self.expression(arg),
            Node::NonNull { expr } => self.expression(expr),
            Node::Seq { exprs } => ast.list(exprs).last().is_some_and(|&e| self.expression(e)),
            Node::Assign {
                op: AssignOp::Assign,
                right,
                ..
            } => self.expression(right),
            _ => false,
        }
    }
}

fn is_boolean_static_value(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match ast.node(id) {
        Node::BoolLit { .. } => true,
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::New { .. }
        | Node::ObjectLit { .. }
        | Node::TaggedTemplate { .. }
        | Node::TemplateLit { .. } => false,
        _ => {
            matches!(static_value_no_side_effects(cx, id), Some(Static::Bool(_)))
                && !has_jsdoc_type_reference(cx, id)
        }
    }
}

/// `Global.method` (non-optional) of a boolean static method on the real global.
fn is_known_boolean_static_member(cx: &Ctx<'_, '_>, member: NodeId, allow_optional: bool) -> bool {
    let ast = cx.ast();
    let Some((obj, optional)) = member_object(ast, member) else {
        return false;
    };
    let obj = skip_parens(ast, obj);
    if (optional && !allow_optional) || !matches!(ast.node(obj), Node::Ident { .. }) {
        return false;
    }
    let Some(name) = ident_name(ast, obj) else {
        return false;
    };
    let Some(&(_, methods)) = BOOLEAN_STATIC_METHODS.iter().find(|(o, _)| *o == name) else {
        return false;
    };
    cx.is_global_ref(obj, name)
        && property_name(cx, member).is_some_and(|m| methods.contains(&m.as_str()))
}

fn is_known_boolean_function_reference(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    if matches!(ast.node(node), Node::Ident { .. }) {
        return cx.is_global_ref(node, "Boolean")
            || BOOLEAN_GLOBAL_FUNCTIONS
                .iter()
                .any(|n| cx.is_global_ref(node, n));
    }
    is_known_boolean_static_member(cx, node, false)
}

// ---- The suggestion ---------------------------------------------------------------------

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

fn is_parameter_mirror(
    cx: &Ctx<'_, '_>,
    left: NodeId,
    right: NodeId,
    a: &str,
    b: &str,
    depth: u32,
) -> bool {
    let ast = cx.ast();
    let left = unwrap_expression(ast, left);
    let right = unwrap_expression(ast, right);
    if depth > 1000 {
        return false;
    }
    match (*ast.node(left), *ast.node(right)) {
        (Node::Ident { .. }, Node::Ident { .. }) => {
            let (l, r) = (ident_name(ast, left), ident_name(ast, right));
            (l == Some(a) && r == Some(b)) || (l == Some(b) && r == Some(a))
        }
        (
            Node::Member {
                obj: o1, prop: p1, ..
            },
            Node::Member {
                obj: o2, prop: p2, ..
            },
        ) => {
            ident_name(ast, p1) == ident_name(ast, p2)
                && is_parameter_mirror(cx, o1, o2, a, b, depth + 1)
        }
        (
            Node::Index {
                obj: o1, index: p1, ..
            },
            Node::Index {
                obj: o2, index: p2, ..
            },
        ) => is_same_reference(cx, p1, p2) && is_parameter_mirror(cx, o1, o2, a, b, depth + 1),
        _ => false,
    }
}

fn suggestion(cx: &Ctx<'_, '_>, comparator: NodeId) -> Option<(String, Fix)> {
    let ast = cx.ast();
    let return_expr = return_expression(ast, comparator)?;
    let (params, type_params, return_type) = match *ast.node(comparator) {
        Node::Arrow {
            params,
            type_params,
            return_type,
            ..
        }
        | Node::FunctionExpr {
            params,
            type_params,
            return_type,
            ..
        } => (params, type_params, return_type),
        _ => return None,
    };
    let names: Vec<&str> = ast
        .list(params)
        .iter()
        .map(|&p| match *ast.node(p) {
            Node::Param {
                rest: false,
                init: None,
                type_ann: None,
                optional: false,
                pattern,
                ..
            } if matches!(ast.node(pattern), Node::Ident { .. }) => ident_name(ast, pattern),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let [a, b] = names[..] else {
        return None;
    };
    if a == b
        || type_params.is_some()
        || return_type.is_some()
        || has_comment_in(cx, cx.span(comparator))
    {
        return None;
    }
    let Node::Binary { left, op, right } = *ast.node(unwrap_expression(ast, return_expr)) else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Gt | BinaryOp::GtEq | BinaryOp::Lt | BinaryOp::LtEq
    ) || is_ts_wrapper(ast, left)
        || is_ts_wrapper(ast, right)
        || !is_parameter_mirror(cx, left, right, a, b, 0)
    {
        return None;
    }
    let (minuend, subtrahend) = if matches!(op, BinaryOp::Gt | BinaryOp::GtEq) {
        (left, right)
    } else {
        (right, left)
    };
    let replacement = format!(
        "({a}, {b}) => {} - {}",
        cx.text(skip_parens(ast, minuend)),
        cx.text(skip_parens(ast, subtrahend))
    );
    let fix = Fix::replace(cx.span(comparator), replacement.clone());
    Some((replacement, fix))
}

impl NoBooleanSortComparator {
    /// `Some(suggestion)` when the comparator returns a boolean.
    fn problem(&self, cx: &Ctx<'_, '_>, comparator: NodeId) -> Option<Option<(String, Fix)>> {
        let ast = cx.ast();
        let unwrapped = unwrap_expression(ast, comparator);
        let mut booleans = Booleans {
            cx,
            visited: Vec::new(),
        };
        if is_function_node(ast, unwrapped) {
            if !booleans.function(unwrapped) && !has_boolean_function_assertion(cx, comparator) {
                return None;
            }
            let bare = skip_parens(ast, comparator) == unwrapped;
            return Some(if bare {
                suggestion(cx, unwrapped)
            } else {
                None
            });
        }
        (booleans.function_reference(unwrapped)
            || is_known_boolean_function_reference(cx, unwrapped)
            || has_boolean_function_assertion(cx, comparator))
        .then_some(None)
    }
}

impl Rule for NoBooleanSortComparator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(id) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Some((obj, _)) = member_object(ast, callee) else {
            return;
        };
        let Some(&comparator) = ast.list(args).first() else {
            return;
        };
        if matches!(ast.node(comparator), Node::Spread { .. })
            || !property_name(cx, callee).is_some_and(|m| m == "sort" || m == "toSorted")
        {
            return;
        }
        let Some(suggestion) = self.problem(cx, comparator) else {
            return;
        };
        if is_known_non_indexed_collection(cx, obj) {
            return;
        }
        let r = cx
            .report(
                cx.span(skip_parens(ast, comparator)),
                "Do not return a boolean from a sort comparator.",
            )
            .id("no-boolean-sort-comparator");
        if let Some((replacement, fix)) = suggestion {
            r.suggest(
                "no-boolean-sort-comparator/suggestion",
                format!("Replace with `{replacement}`."),
                fix,
            );
        }
    }
}
