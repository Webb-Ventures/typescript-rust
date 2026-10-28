//! `unicorn/no-unused-builtin-method-return`: the result of a pure built-in method
//! (`array.map()`, `set.union()`, `date.add()`, ...) is the whole point of calling it, so
//! a call whose value is thrown away is a mistake.
//!
//! A call is discarded when it is an expression statement or a non-last comma operand, the
//! `init`/`update` of a `for`, possibly through `await` and TS wrappers (`void` counts as a
//! deliberate discard). The receiver is resolved through plain, unwritten-before
//! `VariableDeclarator` initializers and aliases; members, destructuring, defaulted or
//! optional parameters and anything written before the call leave it uncertain (skipped).
//! An annotation must be a supported type for the binding to count.
//!
//! - Array methods are reported unless the receiver is clearly not an array: a literal,
//!   object, function, class, template, binary expression, `undefined`, a `new` of
//!   anything but `Array`, a call of `String`/`Number`/... , a PascalCase name not known to
//!   be an array, or an `expect(...)` call. `.values()` needs a known array.
//! - Set methods need a known `Set`; Temporal methods a known Temporal type that has the
//!   method (`new Temporal.X()`, `Temporal.X.from()`, annotations); `.with()` also takes a
//!   known array.
//!
//! Lazy iterator helpers (`iterator.map(fn)`) are left to `no-unused-iterator-helper`.

use super::prelude::*;
use super::unicorn_ast::is_value_not_usable;
use super::unicorn_static::{Static, is_global_ident, static_value};
use super::unicorn_types::{ARRAY, Checker, SET, binding_annotation, is_array};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unused-builtin-method-return",
        "Disallow ignoring the return value of selected built-in methods.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoUnusedBuiltinMethodReturn>,
};

#[derive(Debug, Default)]
struct NoUnusedBuiltinMethodReturn;

const ARRAY_METHODS: &[&str] = &[
    "at",
    "concat",
    "entries",
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "some",
    "slice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "values",
    "with",
];

const SET_METHODS: &[&str] = &[
    "has",
    "union",
    "intersection",
    "difference",
    "symmetricDifference",
    "isSubsetOf",
    "isSupersetOf",
    "isDisjointFrom",
];

const ADD_TYPES: &[&str] = &[
    "Instant",
    "ZonedDateTime",
    "PlainDate",
    "PlainTime",
    "PlainDateTime",
    "PlainYearMonth",
    "Duration",
];
const ROUND_TYPES: &[&str] = &[
    "Instant",
    "ZonedDateTime",
    "PlainTime",
    "PlainDateTime",
    "Duration",
];
const WITH_TYPES: &[&str] = &[
    "ZonedDateTime",
    "PlainDate",
    "PlainTime",
    "PlainDateTime",
    "PlainYearMonth",
    "PlainMonthDay",
    "Duration",
];

/// `new Temporal.X(...)` or `Temporal.X.from(...)` for one of `types`.
fn temporal_construction(ast: &Ast, id: NodeId, types: &[&str]) -> bool {
    let id = skip_parens(ast, id);
    let ctor = match *ast.node(id) {
        Node::New { callee, .. } => skip_parens(ast, callee),
        Node::Call {
            callee,
            optional: false,
            ..
        } => match *ast.node(skip_parens(ast, callee)) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } if ident_name(ast, prop) == Some("from") => skip_parens(ast, obj),
            _ => return false,
        },
        _ => return false,
    };
    matches!(*ast.node(ctor), Node::Member { obj, optional: false, prop }
        if ident_name(ast, skip_parens(ast, obj)) == Some("Temporal")
            && ident_name(ast, prop).is_some_and(|p| types.contains(&p)))
}

macro_rules! temporal_checker {
    ($name:ident, $node:ident, $types:ident, [$($t:literal),*]) => {
        fn $node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
            temporal_construction(cx.ast(), id, $types)
        }
        static $name: Checker = Checker {
            target_names: &[$(concat!("Temporal.", $t)),*],
            target_node: Some($node),
            ..Checker::DEFAULT
        };
    };
}

temporal_checker!(
    TEMPORAL_ADD,
    add_node,
    ADD_TYPES,
    [
        "Instant",
        "ZonedDateTime",
        "PlainDate",
        "PlainTime",
        "PlainDateTime",
        "PlainYearMonth",
        "Duration"
    ]
);
temporal_checker!(
    TEMPORAL_ROUND,
    round_node,
    ROUND_TYPES,
    [
        "Instant",
        "ZonedDateTime",
        "PlainTime",
        "PlainDateTime",
        "Duration"
    ]
);
temporal_checker!(
    TEMPORAL_WITH,
    with_node,
    WITH_TYPES,
    [
        "ZonedDateTime",
        "PlainDate",
        "PlainTime",
        "PlainDateTime",
        "PlainYearMonth",
        "PlainMonthDay",
        "Duration"
    ]
);

fn temporal_checker(method: &str) -> Option<&'static Checker> {
    match method {
        "add" | "subtract" => Some(&TEMPORAL_ADD),
        "round" => Some(&TEMPORAL_ROUND),
        "with" => Some(&TEMPORAL_WITH),
        _ => None,
    }
}

/// What a receiver resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Recv {
    Node(NodeId),
    Uncertain,
}

struct Check<'c, 'x, 'f> {
    cx: &'c Ctx<'x, 'f>,
    method: &'c str,
}

impl Check<'_, '_, '_> {
    fn requires_known(&self) -> bool {
        SET_METHODS.contains(&self.method) || temporal_checker(self.method).is_some()
    }

    fn known_receiver(&self, id: NodeId) -> bool {
        let cx = self.cx;
        if SET_METHODS.contains(&self.method) {
            return SET.is_target(cx, id);
        }
        if self.method == "with" && is_array(cx, id) {
            return true;
        }
        temporal_checker(self.method).is_some_and(|c| c.is_target(cx, id))
    }

    fn supported(&self, id: NodeId) -> bool {
        if self.requires_known() {
            self.known_receiver(id)
        } else {
            is_array(self.cx, id)
        }
    }

    fn supported_annotation(&self, ty: NodeId) -> bool {
        let cx = self.cx;
        let target = |c: &Checker| c.annotation(cx, ty) == super::unicorn_types::Guess::Target;
        if !self.requires_known() {
            return target(&ARRAY);
        }
        if SET_METHODS.contains(&self.method) {
            return target(&SET);
        }
        (self.method == "with" && target(&ARRAY))
            || temporal_checker(self.method).is_some_and(target)
    }

    /// `None` keeps the identifier itself.
    fn variable_value(&self, ident: NodeId) -> Result<Option<NodeId>, ()> {
        let cx = self.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(sym) = sem
            .reference_at(ident)
            .and_then(|r| sem.reference(r).symbol())
        else {
            return Ok(None);
        };
        let s = sem.symbol(sym);
        if s.flags.contains(SymbolFlags::COMMENT_GLOBAL) || s.decls.is_empty() {
            return Ok(None);
        }
        let [d] = s.decls[..] else {
            return Err(());
        };
        let at = cx.span(ident).start;
        if s.refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(RefFlags::INIT) && cx.span(r.node).start < at
        }) {
            return Err(());
        }
        match (d.kind, *ast.node(d.node)) {
            (
                DeclKind::Param,
                Node::Param {
                    pattern,
                    rest: false,
                    init: None,
                    optional,
                    type_ann,
                    modifiers,
                    ..
                },
            ) if pattern == d.name
                && !modifiers.intersects(
                    Modifiers::PUBLIC
                        | Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::READONLY
                        | Modifiers::OVERRIDE,
                ) =>
            {
                if optional || type_ann.is_some_and(|t| !self.supported_annotation(t)) {
                    Err(())
                } else {
                    Ok(None)
                }
            }
            (
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using,
                Node::VarDeclarator { name, init, .. },
            ) if name == d.name && matches!(ast.node(d.parent), Node::VarDecl { .. }) => {
                let (ann, _) = binding_annotation(cx, d.name);
                if let Some(t) = ann {
                    return if self.supported_annotation(t) {
                        Ok(None)
                    } else {
                        Err(())
                    };
                }
                // A `for (const x of …)` declarator has no initializer.
                let in_loop = cx.parent(d.parent).is_some_and(|p| {
                    matches!(ast.node(p), Node::ForIn { .. } | Node::ForOf { .. })
                });
                match init {
                    Some(i) if !in_loop => Ok(Some(i)),
                    _ => Err(()),
                }
            }
            _ => Err(()),
        }
    }

    fn resolve(&self, id: NodeId, visited: &mut Vec<NodeId>) -> Recv {
        let ast = self.cx.ast();
        if visited.contains(&id) || visited.len() > 100 {
            return Recv::Node(id);
        }
        visited.push(id);
        match *ast.node(id) {
            Node::Ident { .. } => match self.variable_value(id) {
                Err(()) => Recv::Uncertain,
                Ok(None) => Recv::Node(id),
                Ok(Some(init)) => self.resolve(init, visited),
            },
            Node::Paren { expr } | Node::NonNull { expr } | Node::Satisfies { expr, .. } => {
                self.resolve(expr, visited)
            }
            Node::Member { .. } | Node::Index { .. } => Recv::Uncertain,
            Node::As { .. } | Node::TypeAssertion { .. } => {
                if self.supported(id) {
                    Recv::Node(id)
                } else {
                    Recv::Uncertain
                }
            }
            _ => Recv::Node(id),
        }
    }

    fn is_definitely_non_array(&self, id: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        match *ast.node(id) {
            Node::Ident { .. } => is_global_ident(cx, id, "undefined"),
            Node::ObjectLit { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::Binary { .. }
            | Node::TemplateLit { .. }
            | Node::Arrow { .. }
            | Node::FunctionExpr { .. }
            | Node::ClassExpr { .. } => true,
            Node::New { callee, .. } => !is_global_ident(cx, callee, "Array"),
            Node::Call { callee, .. } => ident_name(ast, callee).is_some_and(|n| {
                matches!(
                    n,
                    "BigInt" | "Boolean" | "Number" | "RegExp" | "String" | "Symbol"
                ) && is_global_ident(cx, callee, n)
            }),
            _ => false,
        }
    }

    fn should_skip(&self, receiver: NodeId) -> bool {
        let cx = self.cx;
        let ast = cx.ast();
        let resolved = match self.resolve(receiver, &mut Vec::new()) {
            Recv::Uncertain => return true,
            Recv::Node(n) => n,
        };
        if self.requires_known() {
            return !self.supported(resolved);
        }
        if is_expect_call(ast, resolved) {
            return true;
        }
        if self.method == "values" {
            return !is_array(cx, resolved);
        }
        self.is_definitely_non_array(resolved)
            || (ident_name(ast, resolved)
                .is_some_and(|n| n.chars().next().is_some_and(char::is_uppercase))
                && !is_array(cx, resolved))
    }
}

fn is_expect_call(ast: &Ast, id: NodeId) -> bool {
    let Node::Call { callee, .. } = *ast.node(id) else {
        return false;
    };
    let callee = skip_parens(ast, callee);
    match *ast.node(callee) {
        Node::Ident { .. } => ident_name(ast, callee) == Some("expect"),
        Node::Member { obj, prop, .. } => {
            ident_name(ast, skip_parens(ast, obj)) == Some("expect")
                && matches!(ident_name(ast, prop), Some("element" | "poll" | "soft"))
        }
        _ => false,
    }
}

// ---- Lazy iterator helpers (the receiver half of `no-unused-iterator-helper`) ----

const ITERATOR_TYPES: &[&str] = &[
    "ArrayIterator",
    "Generator",
    "IterableIterator",
    "Iterator",
    "IteratorObject",
    "MapIterator",
    "RegExpStringIterator",
    "SetIterator",
    "StringIterator",
];

static ITERATOR: Checker = Checker {
    target_names: ITERATOR_TYPES,
    nullish_in_mixed_union: true,
    prefer_definitions: true,
    ..Checker::DEFAULT
};

fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

/// `(object, name, args)` of a plain-named method call.
fn named_call(ast: &Ast, id: NodeId) -> Option<(NodeId, &str, &[NodeId], bool)> {
    let Node::Call {
        callee,
        args,
        optional,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
        return None;
    };
    Some((obj, ident_name(ast, prop)?, ast.list(args), optional))
}

fn is_lazy_helper_call(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    named_call(ast, id).is_some_and(|(obj, name, args, _)| {
        matches!(name, "drop" | "filter" | "flatMap" | "map" | "take")
            && !args.is_empty()
            && is_iterator_expression(cx, obj, visited)
    })
}

fn immutable_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let sym = sem
        .reference_at(id)
        .and_then(|r| sem.reference(r).symbol())?;
    let s = sem.symbol(sym);
    let [d] = s.decls[..] else { return None };
    if s.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && !r.flags.contains(RefFlags::INIT)
    }) {
        return None;
    }
    match (d.kind, *ast.node(d.node)) {
        (DeclKind::Function, Node::FunctionDecl { .. }) => Some(d.node),
        (
            DeclKind::Const,
            Node::VarDeclarator {
                name,
                type_ann: None,
                init,
                ..
            },
        ) if name == d.name => init,
        _ => None,
    }
}

fn is_iterator_expression(cx: &Ctx<'_, '_>, expr: NodeId, visited: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    let node = unwrap(ast, expr);
    if visited.contains(&node) || visited.len() > 100 {
        return false;
    }
    visited.push(node);
    if let Some((obj, name, args, _)) = named_call(ast, node) {
        let obj = unwrap(ast, obj);
        let global_iterator = is_global_ident(cx, obj, "Iterator")
            || matches!(*ast.node(obj), Node::Member { obj: o, optional: false, prop }
                if ident_name(ast, prop) == Some("Iterator")
                    && is_global_ident(cx, unwrap(ast, o), "globalThis"));
        if global_iterator && matches!(name, "concat" | "from" | "zip" | "zipKeyed") {
            return true;
        }
        if (matches!(name, "entries" | "keys" | "values") && args.is_empty())
            || (name == "matchAll" && args.len() <= 1)
        {
            return true;
        }
    }
    if is_lazy_helper_call(cx, node, visited) {
        return true;
    }
    if matches!(ast.node(node), Node::Ident { .. })
        && let Some(v) = immutable_value(cx, node)
        && is_iterator_expression(cx, v, visited)
    {
        return true;
    }
    if let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(node)
    {
        let mut f = Some(callee);
        for _ in 0..100 {
            let Some(c) = f else { break };
            let c = unwrap(ast, c);
            match *ast.node(c) {
                Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. } => {
                    if flags.contains(FnFlags::GENERATOR) && !flags.contains(FnFlags::ASYNC) {
                        return true;
                    }
                    break;
                }
                Node::Ident { .. } => f = immutable_value(cx, c),
                _ => break,
            }
        }
    }
    !is_array(cx, expr) && ITERATOR.is_target(cx, expr)
}

/// The value is thrown away (directly, through `await`/TS wrappers, or as a `for`
/// init/update).
fn is_discarded(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = call;
    for _ in 0..1000 {
        if is_value_not_usable(cx, cur) {
            return true;
        }
        let Some(mut p) = cx.parent(cur) else {
            return false;
        };
        while let Node::Paren { .. } = ast.node(p) {
            cur = p;
            match cx.parent(p) {
                Some(q) => p = q,
                None => return false,
            }
        }
        match *ast.node(p) {
            Node::For { init, update, .. } if init == Some(cur) || update == Some(cur) => {
                return true;
            }
            Node::Await { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            _ => return false,
        }
    }
    false
}

impl Rule for NoUnusedBuiltinMethodReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let (obj, name, prop) = match *ast.node(callee) {
            Node::Member { obj, prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => (obj, ast.atom(name).to_owned(), prop),
                _ => return,
            },
            Node::Index { obj, index, .. } => match static_value(cx, index) {
                Some(Static::Str(s)) => (obj, s, index),
                _ => return,
            },
            _ => return,
        };
        let name = name.as_str();
        if !(ARRAY_METHODS.contains(&name)
            || SET_METHODS.contains(&name)
            || temporal_checker(name).is_some())
            || !is_discarded(cx, node)
            || is_lazy_helper_call(cx, node, &mut Vec::new())
        {
            return;
        }
        let check = Check { cx, method: name };
        if check.should_skip(obj) {
            return;
        }
        let msg = format!("Do not ignore the return value of `.{name}(…)`.");
        cx.report_node(prop, msg)
            .id("no-unused-builtin-method-return");
    }
}
