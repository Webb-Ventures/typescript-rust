//! `unicorn/no-for-loop`: a `for (let i = 0; i < arr.length; i++)` loop (or one caching the
//! length, `for (let i = 0, j = arr.length; i < j; i++)`, either comparison direction)
//! whose block body only reads `arr[i]` becomes `for (const element of arr)`, or
//! `for (const [i, element] of arr.entries())` when `i` is used for something else.
//!
//! The update must be `i++`, `++i`, `i += 1`, `i = i + 1` or `i = 1 + i`; with a cached
//! length, `(i++, j)` is reported without a fix. Not reported: a body that writes `i`, any
//! `arr` use in the body other than reading `arr[i]` (`arr[i]++`, `delete arr[i]`,
//! `arr[i] = x`, destructuring and `for…in`/`for…of` targets count as writes), a cached
//! length written in the loop, and a value known not to be an array (a number, a boolean,
//! a static string). The element is named after a `const el = arr[i]` declaration in the
//! body (which is removed, unless it is a pattern used with other `arr[i]` reads), else
//! the singular of the array name, else `element`, made unique.
//!
//! No fix when the element name is taken in a nested scope, the element is reassigned
//! while `arr[i]` is read again, an `arr[i]` is called, tagged or deleted, a comment sits
//! in the loop head or a replaced range, a loop variable is used after the loop, the cached
//! length is read outside the test, the element declaration has a type annotation, or
//! `.entries()` is needed on a value without it (strings, `HTMLCollection`s,
//! `document.getElementsBy*()`).

use super::prelude::*;
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_no_useless_undefined::is_reference_context;
use super::unicorn_static::{
    Static, mutable_member_access, static_value, static_value_no_side_effects,
};
use super::unicorn_text::has_comment_in;
use super::unicorn_types::{binding_annotation, is_array, is_number};
use super::unicorn_util::{has_optional_chain, parenthesized_span};
use super::unicorn_util_h4::{
    available_name, fix_of, is_method_call, remove_statement, scope_within, singular,
};
use crate::semantic::{DeclKind, RefId, Reference, ScopeId, Semantic, SymbolId};
use ts_r_ast::{
    AssignOp, Ast, BinaryOp, KeywordTypeKind, TypeOperatorKind, UnaryOp, UpdateOp, VarKind,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-for-loop",
        "Do not use a `for` loop that can be replaced with a `for-of` loop.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoForLoop>,
};

const MESSAGE: &str = "Use a `for-of` loop instead of this `for` loop.";

#[derive(Debug, Default)]
struct NoForLoop;

impl FromOptions for NoForLoop {
    fn from_options(_: &[Value]) -> Result<Self, String> {
        Ok(NoForLoop)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Entries {
    Yes,
    No,
    Unknown,
}

fn union(all: impl Iterator<Item = Entries>) -> Entries {
    let all: Vec<Entries> = all.collect();
    if all.iter().all(|&e| e == Entries::Yes) {
        Entries::Yes
    } else if all.contains(&Entries::No) {
        Entries::No
    } else {
        Entries::Unknown
    }
}

fn intersection(all: impl Iterator<Item = Entries>) -> Entries {
    let all: Vec<Entries> = all.collect();
    if all.contains(&Entries::Yes) {
        Entries::Yes
    } else if all.iter().all(|&e| e == Entries::No) {
        Entries::No
    } else {
        Entries::Unknown
    }
}

const NO_ENTRIES_TYPES: &[&str] = &[
    "HTMLAllCollection",
    "HTMLCollection",
    "HTMLCollectionOf",
    "HTMLFormControlsCollection",
    "HTMLFormElement",
    "HTMLOptionsCollection",
    "HTMLSelectElement",
    "String",
];

fn is_zero(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

fn is_one(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 1.0)
}

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    ident_name(ast, skip_parens(ast, id)) == Some(name)
}

/// `arr.length` with `arr` an identifier: `arr`.
fn length_object(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let obj = skip_parens(ast, obj);
    (matches!(ast.node(obj), Node::Ident { .. }) && ident_name(ast, prop) == Some("length"))
        .then_some(obj)
}

/// `(lesser, greater)` of a strict `<` or `>` comparison.
fn strict_operands(ast: &Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Lt,
            right,
        } => Some((left, right)),
        Node::Binary {
            left,
            op: BinaryOp::Gt,
            right,
        } => Some((right, left)),
        _ => None,
    }
}

struct LoopInfo<'a> {
    array: NodeId,
    array_name: &'a str,
    index_name: &'a str,
    /// The cached length declarator's name, `j` in `let i = 0, j = arr.length`.
    cached_length: Option<NodeId>,
}

fn loop_info<'a>(ast: &'a Ast, init: Option<NodeId>, test: Option<NodeId>) -> Option<LoopInfo<'a>> {
    let Node::VarDecl { decls, .. } = *ast.node(init?) else {
        return None;
    };
    let declarator = |d: NodeId| match *ast.node(d) {
        Node::VarDeclarator { name, init, .. } => Some((name, init)),
        _ => None,
    };
    let (lesser, greater) = strict_operands(ast, test?)?;
    match *ast.list(decls) {
        [only] => {
            let (name, init) = declarator(only)?;
            let index_name = ident_name(ast, name)?;
            if !init.is_some_and(|i| is_zero(ast, i)) || !is_named(ast, lesser, index_name) {
                return None;
            }
            let array = length_object(ast, greater)?;
            Some(LoopInfo {
                array,
                array_name: ident_name(ast, array)?,
                index_name,
                cached_length: None,
            })
        }
        [index, cached] => {
            let (index_id, index_init) = declarator(index)?;
            let (cached_id, cached_init) = declarator(cached)?;
            let index_name = ident_name(ast, index_id)?;
            let cached_name = ident_name(ast, cached_id)?;
            if !index_init.is_some_and(|i| is_zero(ast, i)) {
                return None;
            }
            let array = length_object(ast, cached_init?)?;
            if !is_named(ast, lesser, index_name) || !is_named(ast, greater, cached_name) {
                return None;
            }
            Some(LoopInfo {
                array,
                array_name: ident_name(ast, array)?,
                index_name,
                cached_length: Some(cached_id),
            })
        }
        _ => None,
    }
}

fn increments(ast: &Ast, update: NodeId, index: &str) -> bool {
    match *ast.node(skip_parens(ast, update)) {
        Node::Update {
            op: UpdateOp::Increment,
            arg,
            ..
        } => is_named(ast, arg, index),
        Node::Assign { left, op, right } if is_named(ast, left, index) => match op {
            AssignOp::AddAssign => is_one(ast, right),
            AssignOp::Assign => match *ast.node(skip_parens(ast, right)) {
                Node::Binary {
                    left: l,
                    op: BinaryOp::Add,
                    right: r,
                } => {
                    (is_named(ast, l, index) && is_one(ast, r))
                        || (is_named(ast, r, index) && is_one(ast, l))
                }
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
}

/// `(i++, j)` or `(j, i++)`.
fn increments_and_reads(ast: &Ast, update: NodeId, index: &str, cached: &str) -> bool {
    let Node::Seq { exprs } = *ast.node(skip_parens(ast, update)) else {
        return false;
    };
    let &[a, b] = ast.list(exprs) else {
        return false;
    };
    (increments(ast, a, index) && is_named(ast, b, cached))
        || (increments(ast, b, index) && is_named(ast, a, cached))
}

fn inside(cx: &Ctx<'_, '_>, node: NodeId, outer: NodeId) -> bool {
    cx.span(outer).contains_span(cx.span(node))
}

/// `arr[i]` written through: an update or `delete` operand, an assignment, default or
/// `for…in`/`for…of` target, or an element of a destructuring target.
fn is_changed(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    let ast = cx.ast();
    let parents = cx.file.parents;
    // In ESTree an optional chain is wrapped in a chain expression, which is the parent the
    // plugin sees: `delete (arr?.[i])` reads.
    if has_optional_chain(ast, member) {
        return false;
    }
    let Some(p) = parent_skip_parens(ast, parents, member) else {
        return false;
    };
    if matches!(
        ast.node(p),
        Node::Update { .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                ..
            }
    ) {
        return true;
    }
    let mut cur = member;
    for _ in 0..256 {
        let Some(p) = parent_skip_parens(ast, parents, cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::ArrayLit { .. }
            | Node::ArrayPattern { .. }
            | Node::ObjectLit { .. }
            | Node::ObjectPattern { .. }
            | Node::Spread { .. }
            | Node::Rest { .. } => cur = p,
            Node::Property { value, .. } if skip_parens(ast, value) == cur => cur = p,
            Node::BindingProperty { value: Some(v), .. } if skip_parens(ast, v) == cur => cur = p,
            Node::Assign { left, .. } | Node::AssignPattern { left, .. } => {
                return skip_parens(ast, left) == cur;
            }
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => {
                return skip_parens(ast, left) == cur;
            }
            _ => return false,
        }
    }
    false
}

fn is_boolean_value(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::BoolLit { .. }
        | Node::Unary {
            op: UnaryOp::Not, ..
        } => true,
        Node::Binary { op, .. } => matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq
                | BinaryOp::In
                | BinaryOp::InstanceOf
        ),
        Node::Call { callee, .. } => {
            is_named(ast, callee, "Boolean")
                || is_method_call(
                    ast,
                    skip_parens(ast, id),
                    &[
                        "includes",
                        "startsWith",
                        "endsWith",
                        "some",
                        "every",
                        "has",
                        "test",
                        "isArray",
                    ],
                    None,
                    None,
                )
        }
        _ => false,
    }
}

fn is_static_non_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if let Some(init) = const_initializer(cx, id)
        && (is_number(cx, init) || is_boolean_value(ast, init))
    {
        return true;
    }
    if let Some(v) = static_value_no_side_effects(cx, id) {
        return !matches!(v, Static::Array(_));
    }
    let Some(v) = static_value(cx, id) else {
        return false;
    };
    if mutable_member_access(cx, id, &mut Vec::new()) {
        return matches!(v, Static::Str(_));
    }
    !matches!(v, Static::Array(_))
}

struct EntriesCx<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    visited: Vec<SymbolId>,
    depth: u32,
}

impl EntriesCx<'_, '_, '_> {
    fn annotation(&mut self, ty: Option<NodeId>) -> Entries {
        let Some(ty) = ty else {
            return Entries::Unknown;
        };
        let ast = self.cx.ast();
        self.depth += 1;
        let r = if self.depth > 64 {
            Entries::Unknown
        } else {
            match *ast.node(ty) {
                Node::ParenType { ty } => self.annotation(Some(ty)),
                Node::ArrayType { .. } | Node::TupleType { .. } => Entries::Yes,
                Node::TypeOperator {
                    op: TypeOperatorKind::Readonly,
                    ty,
                } => self.annotation(Some(ty)),
                Node::TypeRef { name, .. } => self.type_ref(name),
                Node::UnionType { types } => {
                    let types = ast.list(types).to_vec();
                    union(types.into_iter().map(|t| self.annotation(Some(t))))
                }
                Node::IntersectionType { types } => {
                    let types = ast.list(types).to_vec();
                    intersection(types.into_iter().map(|t| self.annotation(Some(t))))
                }
                Node::KeywordType {
                    kind:
                        KeywordTypeKind::BigInt
                        | KeywordTypeKind::Boolean
                        | KeywordTypeKind::Never
                        | KeywordTypeKind::Null
                        | KeywordTypeKind::Number
                        | KeywordTypeKind::String
                        | KeywordTypeKind::Symbol
                        | KeywordTypeKind::Undefined
                        | KeywordTypeKind::Void,
                }
                | Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BoolLit { .. }
                | Node::BigIntLit { .. }
                | Node::Unary { .. } => Entries::No,
                _ => Entries::Unknown,
            }
        };
        self.depth -= 1;
        r
    }

    fn type_ref(&mut self, name: NodeId) -> Entries {
        let cx = self.cx;
        let ast = cx.ast();
        let Some(n) = ident_name(ast, name) else {
            return Entries::Unknown;
        };
        if n == "Array" || n == "ReadonlyArray" {
            return Entries::Yes;
        }
        if NO_ENTRIES_TYPES.contains(&n) {
            return Entries::No;
        }
        let sem = cx.semantic();
        let Some(sym) = sem
            .reference_at(name)
            .and_then(|r| sem.reference(r).symbol())
            .or_else(|| sem.lookup(sem.enclosing_scope(name), n))
        else {
            return Entries::Unknown;
        };
        if self.visited.contains(&sym) {
            return Entries::Unknown;
        }
        let [decl] = sem.symbol(sym).decls.as_slice() else {
            return Entries::Unknown;
        };
        let ty = match (decl.kind, *ast.node(decl.node)) {
            (DeclKind::TypeAlias, Node::TypeAlias { ty, .. }) => Some(ty),
            (DeclKind::TypeParam, Node::TypeParam { constraint, .. }) => constraint,
            _ => return Entries::Unknown,
        };
        self.visited.push(sym);
        let r = self.annotation(ty);
        self.visited.pop();
        r
    }

    fn expr(&mut self, id: NodeId) -> Entries {
        let cx = self.cx;
        let ast = cx.ast();
        let id = skip_parens(ast, id);
        if is_array(cx, id) {
            return Entries::Yes;
        }
        self.depth += 1;
        let r = if self.depth > 64 {
            Entries::Unknown
        } else {
            match *ast.node(id) {
                Node::Ident { .. } => self.variable(id),
                Node::As { expr, ty } | Node::TypeAssertion { expr, ty } => {
                    match self.annotation(Some(ty)) {
                        Entries::Unknown => self.expr(expr),
                        known => known,
                    }
                }
                Node::Satisfies { expr, .. } | Node::NonNull { expr } => self.expr(expr),
                Node::Seq { exprs } => match ast.list(exprs).last() {
                    Some(&last) => self.expr(last),
                    None => Entries::Unknown,
                },
                Node::Conditional { cons, alt, .. } => {
                    let c = self.expr(cons);
                    let a = self.expr(alt);
                    union([c, a].into_iter())
                }
                Node::Call {
                    callee,
                    optional: false,
                    ..
                } if is_method_call(
                    ast,
                    id,
                    &[
                        "getElementsByClassName",
                        "getElementsByTagName",
                        "getElementsByTagNameNS",
                    ],
                    None,
                    None,
                ) =>
                {
                    let Node::Member { obj, .. } = *ast.node(skip_parens(ast, callee)) else {
                        return Entries::Unknown;
                    };
                    let obj = skip_parens(ast, obj);
                    if is_named(ast, obj, "document") && cx.semantic().refers_to_global(obj) {
                        Entries::No
                    } else {
                        Entries::Unknown
                    }
                }
                _ => Entries::Unknown,
            }
        };
        self.depth -= 1;
        r
    }

    fn variable(&mut self, id: NodeId) -> Entries {
        let cx = self.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
            return Entries::Unknown;
        };
        if self.visited.contains(&sym) {
            return Entries::Unknown;
        }
        let [decl] = sem.symbol(sym).decls.as_slice() else {
            return Entries::Unknown;
        };
        let decl = *decl;
        self.visited.push(sym);
        let mut r = self.annotation(binding_annotation(cx, decl.name).0);
        if r == Entries::Unknown
            && let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(decl.node)
            && name == decl.name
        {
            r = self.expr(init);
        }
        self.visited.pop();
        r
    }
}

fn reference_symbol(sem: &dyn Semantic, ident: NodeId) -> Option<SymbolId> {
    sem.reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
}

impl Rule for NoForLoop {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::For]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let src = cx.src();
        let Node::For {
            init,
            test,
            update,
            body,
        } = *ast.node(node)
        else {
            return;
        };
        let Some(info) = loop_info(ast, init, test) else {
            return;
        };
        if is_static_non_array(cx, info.array) {
            return;
        }
        let array_symbol = reference_symbol(sem, info.array);
        let standard_update = update.is_some_and(|u| increments(ast, u, info.index_name));
        let reportable = standard_update
            || info.cached_length.is_some_and(|c| {
                update.is_some_and(|u| {
                    increments_and_reads(ast, u, info.index_name, ident_name(ast, c).unwrap_or(""))
                })
            });
        if !reportable || !matches!(ast.node(body), Node::Block { .. }) {
            return;
        }
        let Some(body_scope) = sem.scope_of_node(body) else {
            return;
        };
        let Some(index_symbol) = sem.lookup(body_scope, info.index_name) else {
            return;
        };
        let refs_of = |sym: SymbolId| -> Vec<Reference> {
            sem.symbol(sym)
                .refs
                .iter()
                .map(|&r| *sem.reference(r))
                .collect()
        };
        let index_refs = refs_of(index_symbol);
        if index_refs
            .iter()
            .any(|r| r.is_write() && scope_within(sem, r.scope, body_scope))
        {
            return;
        }
        let array_refs: Vec<RefId> = (0..sem.references().len())
            .map(|i| RefId(i as u32))
            .filter(|&r| {
                let r = sem.reference(r);
                !r.is_type()
                    && scope_within(sem, r.scope, body_scope)
                    && ast.atom(r.name) == info.array_name
            })
            .collect();
        let member_of = |r: RefId| parent_skip_parens(ast, cx.file.parents, sem.reference(r).node);
        let only_reads = array_refs.iter().all(|&r| {
            let reference = sem.reference(r);
            let Some(m) = member_of(r) else {
                return false;
            };
            let Node::Index { obj, index, .. } = *ast.node(m) else {
                return false;
            };
            let index = skip_parens(ast, index);
            reference.symbol() == array_symbol
                && skip_parens(ast, obj) == reference.node
                && matches!(ast.node(index), Node::Ident { .. })
                && reference_symbol(sem, index) == Some(index_symbol)
                && !is_changed(cx, m)
        });
        if array_refs.is_empty() || !only_reads {
            return;
        }
        let cached_symbol = info.cached_length.and_then(|c| sem.symbol_of_decl(c));
        let cached_refs: Vec<Reference> = cached_symbol
            .map(&refs_of)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| Some(r.node) != info.cached_length)
            .collect();
        let in_test = |n: NodeId| test.is_some_and(|t| inside(cx, n, t));
        if cached_refs
            .iter()
            .any(|r| inside(cx, r.node, node) && !in_test(r.node) && r.is_write())
        {
            return;
        }

        let node_span = cx.span(node);
        let header_end = skip_trivia_backward(ast, src, cx.span(body).start as usize) as u32;
        let report_span = Span::new(node_span.start, header_end);

        // The `const el = arr[i]` declaration in the body.
        let element_ref = array_refs.iter().copied().find(|&r| {
            member_of(r)
                .and_then(|m| parent_skip_parens(ast, cx.file.parents, m))
                .is_some_and(|d| matches!(ast.node(d), Node::VarDeclarator { .. }))
        });
        let element_declarator = element_ref
            .and_then(|r| member_of(r).and_then(|m| parent_skip_parens(ast, cx.file.parents, m)));
        let element_id = element_declarator.and_then(|d| match *ast.node(d) {
            Node::VarDeclarator { name, .. } => Some(name),
            _ => None,
        });
        let element_name = element_id.and_then(|id| ident_name(ast, id));
        let element_symbol = element_name.and_then(|n| sem.lookup(body_scope, n));
        let scopes: Vec<ScopeId> = (0..sem.scopes().len())
            .map(|i| ScopeId(i as u32))
            .filter(|&s| scope_within(sem, s, body_scope))
            .collect();
        let shadowed = element_name.is_some_and(|n| {
            scopes
                .iter()
                .any(|&s| s != body_scope && sem.lookup(s, n) != element_symbol)
        });
        let generate_index = index_refs.iter().any(|r| {
            if !scope_within(sem, r.scope, body_scope) {
                return false;
            }
            match parent_skip_parens(ast, cx.file.parents, r.node).map(|p| *ast.node(p)) {
                Some(Node::Index { obj, .. }) | Some(Node::Member { obj, .. }) => {
                    !is_named(ast, obj, info.array_name)
                }
                _ => true,
            }
        });
        let entries = if generate_index {
            EntriesCx {
                cx,
                visited: Vec::new(),
                depth: 0,
            }
            .expr(info.array)
        } else {
            Entries::Unknown
        };
        let element_refs = element_symbol.map(&refs_of).unwrap_or_default();
        let element_reassigned = array_refs.len() > 1
            && element_refs
                .iter()
                .any(|r| r.is_write() && Some(r.node) != element_id);
        let opening = skip_trivia_forward(src, node_span.start as usize + 3) as u32;
        let header_comment = has_comment_in(cx, Span::new(opening, header_end));
        let leaks = |refs: &[Reference]| refs.iter().any(|r| !inside(cx, r.node, node));
        let element_annotated = element_declarator.is_some_and(|d| {
            matches!(
                *ast.node(d),
                Node::VarDeclarator {
                    type_ann: Some(_),
                    ..
                }
            )
        });
        let should_fix = standard_update
            && !shadowed
            && !element_reassigned
            && array_refs
                .iter()
                .all(|&r| member_of(r).is_some_and(|m| !is_reference_context(cx, m)))
            && !header_comment
            && !leaks(&index_refs)
            && !leaks(&element_refs)
            && !leaks(&cached_refs)
            && !cached_refs.iter().any(|r| !in_test(r.node))
            && !element_annotated
            && !(generate_index && entries == Entries::No);

        let fix = should_fix
            .then(|| {
                self.fix(
                    cx,
                    &info,
                    init?,
                    update?,
                    &array_refs,
                    element_ref,
                    element_declarator,
                    element_name,
                    &scopes,
                    generate_index,
                )
            })
            .flatten();
        cx.report(report_span, MESSAGE)
            .id("no-for-loop")
            .fix_opt(fix);
    }
}

impl NoForLoop {
    #[allow(clippy::too_many_arguments)]
    fn fix(
        &self,
        cx: &Ctx<'_, '_>,
        info: &LoopInfo<'_>,
        init: NodeId,
        update: NodeId,
        array_refs: &[RefId],
        element_ref: Option<RefId>,
        element_declarator: Option<NodeId>,
        element_name: Option<&str>,
        scopes: &[ScopeId],
        generate_index: bool,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let sem = cx.semantic();
        let parents = cx.file.parents;
        let element = match element_name {
            Some(n) => n.to_owned(),
            None => {
                let base = singular(info.array_name).unwrap_or_else(|| "element".into());
                available_name(cx, &base, scopes, &|_| true)?
            }
        };
        let mut declaration_element = element.clone();
        let mut declaration_kind = "const";
        let mut remove_declaration = true;
        let declaration = element_declarator.and_then(|d| cx.parent(d));
        if let Some(d) = element_declarator
            && let Node::VarDeclarator { name, .. } = *ast.node(d)
        {
            if matches!(
                ast.node(name),
                Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
            ) {
                remove_declaration = array_refs.len() == 1;
            }
            if remove_declaration {
                declaration_kind = match declaration.map(|v| ast.node(v)) {
                    Some(Node::VarDecl {
                        kind: VarKind::Var, ..
                    }) => "var",
                    Some(Node::VarDecl {
                        kind: VarKind::Let, ..
                    }) => "let",
                    _ => {
                        let text = cx.span(declaration?).text(cx.src());
                        if text.starts_with("const") {
                            "const"
                        } else {
                            return None;
                        }
                    }
                };
                declaration_element = cx.span(name).text(cx.src()).to_owned();
            }
        }
        let replacement = if generate_index {
            format!(
                "{declaration_kind} [{}, {declaration_element}] of {}.entries()",
                info.index_name, info.array_name
            )
        } else {
            format!(
                "{declaration_kind} {declaration_element} of {}",
                info.array_name
            )
        };
        let mut edits = vec![(
            Span::new(cx.span(init).start, parenthesized_span(cx, update).end),
            replacement,
        )];
        for &r in array_refs {
            if Some(r) == element_ref {
                continue;
            }
            let member = parent_skip_parens(ast, parents, sem.reference(r).node)?;
            let span = cx.span(member);
            if has_comment_in(cx, span) {
                return None;
            }
            edits.push((span, element.clone()));
        }
        if let (Some(d), Some(declaration)) = (element_declarator, declaration) {
            let Node::VarDecl { decls, .. } = *ast.node(declaration) else {
                return None;
            };
            let Node::VarDeclarator {
                init: Some(d_init), ..
            } = *ast.node(d)
            else {
                return None;
            };
            let decls = ast.list(decls);
            let range = if !remove_declaration {
                cx.span(skip_parens(ast, d_init))
            } else if decls.len() == 1 {
                cx.span(declaration)
            } else {
                let i = decls.iter().position(|&x| x == d)?;
                if i == 0 {
                    Span::new(cx.span(d).start, cx.span(decls[1]).start)
                } else {
                    Span::new(cx.span(decls[i - 1]).end, cx.span(d).end)
                }
            };
            if has_comment_in(cx, range) {
                return None;
            }
            if !remove_declaration {
                edits.push((range, element));
            } else if decls.len() == 1 {
                edits.push(remove_statement(cx, declaration));
            } else {
                edits.push((range, String::new()));
            }
        }
        Some(fix_of(edits))
    }
}
