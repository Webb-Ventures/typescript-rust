//! `unicorn/prefer-private-class-fields`: class members named `_x` (plain fields, methods,
//! accessors and `accessor` fields with an identifier key and no TypeScript modifier or
//! decorator) should be private `#x` members, when `#x` is a valid private name the class
//! does not already have.
//!
//! Two members of one name (other than a getter/setter pair) are reported without a fix.
//! Otherwise the fix renames the members and every `this._x` that reaches them, unless
//! something blocks it: a computed or destructured `_x` anywhere in the file, an unknown
//! computed key read off `this` (in a member access or a destructuring of `this`),
//! another member that may have the name, `this` observed whole (spread, object rest,
//! `Object.keys/values/entries/assign`, `JSON.stringify`), a superclass on this class or
//! on any class with a member of the name, or an access the rename cannot follow: not on
//! `this`, `delete`d, optional, in a member key or decorator, in another class (unless
//! that class has its own candidate of the name), static, or in a field initializer at or
//! before the field. A method that is assigned to (`this._m = this._m.bind(this)`) gets
//! no fix.

use super::import_util::walk;
use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_util::is_identifier_name;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-private-class-fields",
        "Prefer private class fields over the underscore-prefix convention.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferPrivateClassFields>,
};

#[derive(Debug, Default)]
struct PreferPrivateClassFields;

fn is_class(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.kind(id), NodeKind::ClassDecl | NodeKind::ClassExpr)
}

fn members(ast: &Ast, class: NodeId) -> &[NodeId] {
    match *ast.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
        _ => &[],
    }
}

/// Parentheses, TypeScript wrappers (and so optional chains, which have no node here).
fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1024 {
        let next = match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => expr,
            _ => return id,
        };
        id = next;
    }
    id
}

fn is_this(ast: &Ast, id: NodeId) -> bool {
    ast.kind(unwrap(ast, id)) == NodeKind::This
}

/// `id` with the parentheses and TypeScript wrappers around it, and its parent.
fn outermost(cx: &Ctx<'_, '_>, id: NodeId) -> (NodeId, Option<NodeId>) {
    let ast = cx.ast();
    let mut cur = id;
    loop {
        let Some(p) = cx.parent(cur) else {
            return (cur, None);
        };
        let wraps = match *ast.node(p) {
            Node::Paren { .. } => true,
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => expr == cur,
            _ => false,
        };
        if !wraps {
            return (cur, Some(p));
        }
        cur = p;
    }
}

/// A static string: a string literal or a template without expressions.
fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(unwrap(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[e] => match *ast.node(e) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The static name of a member key (`foo`, `'foo'`, `['foo']`), and whether it is computed.
fn key_name(ast: &Ast, key: NodeId) -> (Option<String>, bool) {
    match *ast.node(key) {
        Node::Ident { name } => (Some(ast.atom(name).to_owned()), false),
        Node::ComputedKey { expr } => (static_string(ast, expr), true),
        Node::StringLit { value } => (Some(ast.atom(value).to_owned()), false),
        _ => (None, false),
    }
}

fn member_key(ast: &Ast, m: NodeId) -> Option<NodeId> {
    match *ast.node(m) {
        Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => Some(key),
        _ => None,
    }
}

fn member_modifiers(ast: &Ast, m: NodeId) -> Modifiers {
    ast.node(m).modifiers()
}

fn may_have_member_named(ast: &Ast, m: NodeId, name: &str) -> bool {
    let Some(key) = member_key(ast, m) else {
        return false;
    };
    match key_name(ast, key) {
        (Some(n), _) => n == name,
        (None, computed) => computed,
    }
}

fn is_candidate(ast: &Ast, m: NodeId) -> Option<String> {
    let (key, decorators) = match *ast.node(m) {
        Node::PropertyDef {
            key, decorators, ..
        }
        | Node::MethodDef {
            key, decorators, ..
        } => (key, decorators),
        _ => return None,
    };
    let Node::Ident { name } = *ast.node(key) else {
        return None;
    };
    let name = ast.atom(name);
    if !name.starts_with('_') || !ast.list(decorators).is_empty() {
        return None;
    }
    let blocked = Modifiers::PUBLIC
        | Modifiers::PRIVATE
        | Modifiers::PROTECTED
        | Modifiers::DECLARE
        | Modifiers::READONLY
        | Modifiers::OVERRIDE
        | Modifiers::ABSTRACT;
    (!member_modifiers(ast, m).intersects(blocked)).then(|| name.to_owned())
}

/// The class whose instance `this` at `node` is (its member contains `node`, through
/// arrow functions and method bodies), or `None`.
fn this_owner(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        match ast.kind(p) {
            NodeKind::ClassDecl | NodeKind::ClassExpr if members(ast, p).contains(&cur) => {
                return Some(p);
            }
            NodeKind::FunctionExpr | NodeKind::FunctionDecl => {
                let method_body = cx.parent(p).is_some_and(|m| {
                    matches!(ast.node(m), Node::MethodDef { value, .. } if *value == p)
                        && cx.parent(m).is_some_and(|c| is_class(ast, c))
                });
                if !method_body {
                    return None;
                }
            }
            _ => {}
        }
        cur = p;
    }
    None
}

/// The member of the closest class that contains `node`.
fn containing_element(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        if is_class(ast, p) && members(ast, p).contains(&cur) {
            return Some(cur);
        }
        cur = p;
    }
    None
}

fn is_static_member(ast: &Ast, m: NodeId) -> bool {
    member_modifiers(ast, m).contains(Modifiers::STATIC)
}

fn within(cx: &Ctx<'_, '_>, node: NodeId, outer: NodeId) -> bool {
    let (s, o) = (cx.span(node), cx.span(outer));
    s.start >= o.start && s.end <= o.end
}

fn in_element_definition(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(el) = containing_element(cx, node) else {
        return false;
    };
    let (key, decorators) = match *ast.node(el) {
        Node::PropertyDef {
            key, decorators, ..
        }
        | Node::MethodDef {
            key, decorators, ..
        } => (key, decorators),
        _ => return false,
    };
    within(cx, node, key) || ast.list(decorators).iter().any(|&d| within(cx, node, d))
}

fn in_static_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    containing_element(cx, node)
        .is_some_and(|el| ast.kind(el) == NodeKind::StaticBlock || is_static_member(ast, el))
}

fn in_field_initializer_at_or_before(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    member: NodeId,
    class: NodeId,
) -> bool {
    let ast = cx.ast();
    if is_static_member(ast, member) {
        return false;
    }
    let Some(el) = containing_element(cx, node) else {
        return false;
    };
    let list = members(ast, class);
    let pos = |m: NodeId| list.iter().position(|&x| x == m);
    ast.kind(el) == NodeKind::PropertyDef
        && !is_static_member(ast, el)
        && match (pos(el), pos(member)) {
            (Some(a), Some(b)) => a <= b,
            (None, _) => true,
            _ => false,
        }
}

fn super_class(ast: &Ast, class: NodeId) -> bool {
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return false;
    };
    ast.list(heritage).iter().any(|&h| {
        matches!(
            ast.node(h),
            Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                ..
            }
        )
    })
}

fn is_rest_holder(ast: &Ast, pattern: NodeId) -> bool {
    match *ast.node(pattern) {
        Node::ObjectPattern { props } => ast
            .list(props)
            .iter()
            .any(|&p| ast.kind(p) == NodeKind::Rest),
        Node::ObjectLit { props } => ast
            .list(props)
            .iter()
            .any(|&p| ast.kind(p) == NodeKind::Spread),
        _ => false,
    }
}

fn member_prop_name(cx: &Ctx<'_, '_>, callee: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(callee) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => static_string(ast, index),
        _ => None,
    }
}

/// `this` (at `expr`, its outermost wrapper) is observed whole by `parent`.
fn is_observation(cx: &Ctx<'_, '_>, expr: NodeId, parent: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(parent) {
        Node::Spread { .. } => true,
        Node::VarDeclarator { name, .. } => is_rest_holder(ast, name),
        Node::Assign { left, .. } => is_rest_holder(ast, skip_parens(ast, left)),
        Node::Call { callee, args, .. } => {
            if !ast.list(args).contains(&expr) {
                return false;
            }
            let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
                return false;
            };
            let method = member_prop_name(cx, callee);
            match ident_name(ast, obj) {
                Some("Object") => method
                    .as_deref()
                    .is_some_and(|m| matches!(m, "assign" | "entries" | "keys" | "values")),
                Some("JSON") => method.as_deref() == Some("stringify"),
                _ => false,
            }
        }
        _ => false,
    }
}

/// The object a destructuring pattern takes apart (`const {..} = x`, `({..} = x)`).
fn destructured_object(cx: &Ctx<'_, '_>, pattern: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (cur, parent) = outermost(cx, pattern);
    match *ast.node(parent?) {
        Node::VarDeclarator { name, init, .. } if name == cur => init,
        Node::Assign { left, right, .. } if left == cur => Some(right),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Access {
    Blocked,
    Convertible,
    Ignored,
    Written,
}

struct Candidates {
    class: NodeId,
    /// Name to its members, in first-seen order.
    by_name: Vec<(String, Vec<NodeId>)>,
}

impl Rule for PreferPrivateClassFields {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut accesses: Vec<(String, NodeId)> = Vec::new();
        let mut blocking: Vec<String> = Vec::new();
        let mut observed: Vec<NodeId> = Vec::new();
        let mut unknown_computed = false;
        let mut classes: Vec<NodeId> = Vec::new();
        let mut candidates: Vec<Candidates> = Vec::new();
        let mut nodes = Vec::new();
        walk(ast, |id| nodes.push(id));
        for &id in &nodes {
            match *ast.node(id) {
                Node::Member { prop, .. } => {
                    if let Some(n) = ident_name(ast, prop)
                        && n.starts_with('_')
                    {
                        accesses.push((n.to_owned(), id));
                    }
                }
                Node::Index { obj, index, .. } => match static_string(ast, index) {
                    Some(n) if n.starts_with('_') => blocking.push(n),
                    Some(_) => {}
                    None => unknown_computed |= is_this(ast, obj),
                },
                Node::This {} => {
                    let (expr, parent) = outermost(cx, id);
                    if let Some(p) = parent
                        && is_observation(cx, expr, p)
                        && let Some(owner) = this_owner(cx, id)
                    {
                        observed.push(owner);
                    }
                }
                Node::BindingProperty { key, .. } => {
                    let (name, computed) = key_name(ast, key);
                    match name {
                        Some(n) if n.starts_with('_') => blocking.push(n),
                        None if computed => {
                            if let Some(pat) = cx.parent(id)
                                && destructured_object(cx, pat).is_some_and(|o| is_this(ast, o))
                            {
                                unknown_computed = true;
                            }
                        }
                        _ => {}
                    }
                }
                Node::Property { key, .. } | Node::ShorthandProp { name: key, .. } => {
                    let Some(obj) = cx.parent(id) else { continue };
                    if ast.kind(obj) != NodeKind::ObjectLit
                        || !crate::ast_util::is_assignment_target(ast, cx.file.parents, obj)
                    {
                        continue;
                    }
                    let (name, computed) = key_name(ast, key);
                    match name {
                        Some(n) if n.starts_with('_') => blocking.push(n),
                        None if computed => {
                            if destructured_object(cx, obj).is_some_and(|o| is_this(ast, o)) {
                                unknown_computed = true;
                            }
                        }
                        _ => {}
                    }
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    classes.push(id);
                    let mut by_name: Vec<(String, Vec<NodeId>)> = Vec::new();
                    let list = members(ast, id);
                    for &m in list {
                        let Some(name) = is_candidate(ast, m) else {
                            continue;
                        };
                        let new_name = &name[1..];
                        if new_name == "constructor" || !is_identifier_name(new_name) {
                            continue;
                        }
                        let has_private = list.iter().any(|&o| {
                            member_key(ast, o).is_some_and(|k| {
                                matches!(*ast.node(k), Node::PrivateName { name } if ast.atom(name) == new_name)
                            })
                        });
                        if has_private {
                            continue;
                        }
                        match by_name.iter_mut().find(|(n, _)| *n == name) {
                            Some(slot) => slot.1.push(m),
                            None => by_name.push((name, vec![m])),
                        }
                    }
                    if !by_name.is_empty() {
                        candidates.push(Candidates { class: id, by_name });
                    }
                }
                _ => {}
            }
        }

        for c in &candidates {
            for (name, ms) in &c.by_name {
                let replacement = format!("#{}", &name[1..]);
                let msg = format!(
                    "Prefer the private class field `{replacement}` over the underscore-prefixed `{name}`."
                );
                let Some(first_key) = member_key(ast, ms[0]) else {
                    continue;
                };
                let pair = ms.len() == 2 && {
                    let kinds: Vec<Option<MethodKind>> = ms
                        .iter()
                        .map(|&m| match *ast.node(m) {
                            Node::MethodDef { kind, .. } => Some(kind),
                            _ => None,
                        })
                        .collect();
                    is_static_member(ast, ms[0]) == is_static_member(ast, ms[1])
                        && matches!(
                            (kinds[0], kinds[1]),
                            (Some(MethodKind::Get), Some(MethodKind::Set))
                                | (Some(MethodKind::Set), Some(MethodKind::Get))
                        )
                };
                if ms.len() > 1 && !pair {
                    cx.report_node(first_key, msg)
                        .id("prefer-private-class-fields");
                    continue;
                }
                let class = c.class;
                let list = members(ast, class);
                let mut blocked = unknown_computed
                    || blocking.contains(name)
                    || list
                        .iter()
                        .any(|&m| !ms.contains(&m) && may_have_member_named(ast, m, name))
                    || observed.contains(&class);
                if !blocked {
                    blocked = super_class(ast, class)
                        || classes.iter().any(|&other| {
                            other != class
                                && members(ast, other)
                                    .iter()
                                    .any(|&m| may_have_member_named(ast, m, name))
                                && super_class(ast, other)
                        });
                }
                let member = ms[0];
                let mut convertible = Vec::new();
                let mut writes: Vec<NodeId> = Vec::new();
                for &(ref n, access) in &accesses {
                    if n != name {
                        continue;
                    }
                    match access_state(cx, access, name, member, class, &candidates) {
                        Access::Blocked => blocked = true,
                        Access::Convertible => convertible.push(access),
                        Access::Written => writes.push(access),
                        Access::Ignored => {}
                    }
                }
                if !blocked && !writes.is_empty() {
                    let suggestion =
                        bound_method_fix(cx, member, &writes, &convertible, name, &replacement);
                    let r = cx
                        .report_node(first_key, msg)
                        .id("prefer-private-class-fields");
                    if let Some(fix) = suggestion {
                        r.suggest(
                            "prefer-private-class-fields/bound-method",
                            "Convert to a private arrow-function field.",
                            fix,
                        );
                    }
                    continue;
                }
                let fix = (!blocked).then(|| {
                    let mut fix = Fix::replace(cx.span(first_key), replacement.clone());
                    for &m in &ms[1..] {
                        if let Some(k) = member_key(ast, m) {
                            fix = fix.and(cx.span(k), replacement.clone());
                        }
                    }
                    for &a in &convertible {
                        if let Node::Member { prop, .. } = *ast.node(a) {
                            fix = fix.and(cx.span(prop), replacement.clone());
                        }
                    }
                    fix
                });
                cx.report_node(first_key, msg)
                    .id("prefer-private-class-fields")
                    .fix_opt(fix);
            }
        }
    }
}

fn access_state(
    cx: &Ctx<'_, '_>,
    access: NodeId,
    name: &str,
    member: NodeId,
    class: NodeId,
    candidates: &[Candidates],
) -> Access {
    let ast = cx.ast();
    let Node::Member { obj, optional, .. } = *ast.node(access) else {
        return Access::Blocked;
    };
    let (_, parent) = outermost(cx, access);
    let deleted = parent.is_some_and(|p| {
        matches!(
            ast.node(p),
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            }
        )
    });
    if !is_this(ast, obj) || deleted || optional || in_element_definition(cx, access) {
        return Access::Blocked;
    }
    let owner = this_owner(cx, access);
    if owner != Some(class) {
        let has_own = owner.is_some_and(|o| {
            candidates
                .iter()
                .any(|c| c.class == o && c.by_name.iter().any(|(n, _)| n == name))
        });
        return if has_own {
            Access::Ignored
        } else {
            Access::Blocked
        };
    }
    if is_static_member(ast, member)
        || in_static_context(cx, access)
        || (ast.kind(member) == NodeKind::PropertyDef
            && in_field_initializer_at_or_before(cx, access, member, class))
    {
        return Access::Blocked;
    }
    if matches!(
        ast.node(member),
        Node::MethodDef {
            kind: MethodKind::Method,
            ..
        }
    ) && is_left_hand_side(cx, access)
    {
        return Access::Written;
    }
    Access::Convertible
}

/// `this.<name> = this.<name>.bind(this);` as a statement directly in the constructor body.
fn constructor_binding_statement(cx: &Ctx<'_, '_>, access: NodeId, name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let assign = cx.parent(access)?;
    let Node::Assign {
        left,
        op: AssignOp::Assign,
        right,
    } = *ast.node(assign)
    else {
        return None;
    };
    let Node::Member { obj, .. } = *ast.node(access) else {
        return None;
    };
    if left != access || ast.kind(obj) != NodeKind::This {
        return None;
    }
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(right)
    else {
        return None;
    };
    let Node::Member {
        obj: bound,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return None;
    };
    if ident_name(ast, prop) != Some("bind") {
        return None;
    }
    let &[arg] = ast.list(args) else {
        return None;
    };
    if ast.kind(arg) != NodeKind::This {
        return None;
    }
    let Node::Member {
        obj: inner,
        optional: false,
        prop: p2,
    } = *ast.node(bound)
    else {
        return None;
    };
    if ident_name(ast, p2) != Some(name) || ast.kind(inner) != NodeKind::This {
        return None;
    }
    let stmt = cx.parent(assign)?;
    let block = cx.parent(stmt)?;
    let func = cx.parent(block)?;
    let ctor = cx.parent(func)?;
    let ok = ast.kind(stmt) == NodeKind::ExprStmt
        && ast.kind(block) == NodeKind::Block
        && matches!(*ast.node(func), Node::FunctionExpr { body: Some(b), .. } if b == block)
        && matches!(*ast.node(ctor), Node::MethodDef { kind: MethodKind::Constructor, value, .. } if value == func);
    ok.then_some(stmt)
}

/// Reads `arguments`, `eval` or `new.target` anywhere inside.
fn has_method_only_reference(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        match *ast.node(id) {
            Node::Ident { name } if matches!(ast.atom(name), "arguments" | "eval") => return true,
            Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::NewTarget,
            } => return true,
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// The suggestion turning a method bound in the constructor into an arrow-function field.
fn bound_method_fix(
    cx: &Ctx<'_, '_>,
    member: NodeId,
    writes: &[NodeId],
    accesses: &[NodeId],
    name: &str,
    replacement: &str,
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let &[write] = writes else {
        return None;
    };
    let Node::MethodDef {
        key,
        optional: false,
        value,
        ..
    } = *ast.node(member)
    else {
        return None;
    };
    let Node::FunctionExpr {
        flags,
        type_params: None,
        params,
        return_type,
        body: Some(_),
        ..
    } = *ast.node(value)
    else {
        return None;
    };
    if flags.contains(ts_r_ast::FnFlags::GENERATOR) {
        return None;
    }
    let class = cx.parent(member)?;
    let params = ast.list(params);
    let bad_param = params.iter().any(|&p| match *ast.node(p) {
        Node::Param {
            decorators,
            pattern,
            ..
        } => {
            !ast.list(decorators).is_empty()
                || matches!(ast.node(pattern), Node::This {})
                || ident_name(ast, pattern) == Some("this")
        }
        _ => false,
    });
    if bad_param
        || accesses
            .iter()
            .any(|&a| in_field_initializer_at_or_before(cx, a, member, class))
        || has_method_only_reference(ast, value)
    {
        return None;
    }
    let member_span = cx.span(member);
    let signature_start =
        crate::ast_util::skip_trivia_forward(src, cx.span(key).end as usize) as u32;
    if ast
        .comments_in(member_span)
        .iter()
        .any(|c| c.span.start < signature_start)
    {
        return None;
    }
    let statement = constructor_binding_statement(cx, write, name)?;
    let stmt_span = cx.span(statement);
    if !ast.comments_in(stmt_span).is_empty() {
        return None;
    }
    // The end of the token before the body: the return type, or the `)` of the parameters.
    let before_body = match return_type {
        Some(t) => cx.span(t).end,
        None => {
            let from = params
                .last()
                .map_or(signature_start + 1, |&p| cx.span(p).end);
            let close = crate::ast_util::skip_trivia_forward(src, from as usize);
            if !src[close..].starts_with(')') {
                return None;
            }
            close as u32 + 1
        }
    };
    let is_async = flags.contains(ts_r_ast::FnFlags::ASYNC);
    let mut fix = Fix::replace(
        Span::new(member_span.start, signature_start),
        format!("{replacement} = {}", if is_async { "async " } else { "" }),
    )
    .and(Span::new(before_body, before_body), " =>")
    .and(Span::new(member_span.end, member_span.end), ";");
    for &a in accesses {
        if !within(cx, a, statement)
            && let Node::Member { prop, .. } = *ast.node(a)
        {
            fix = fix.and(cx.span(prop), replacement.to_owned());
        }
    }
    let next = crate::ast_util::skip_trivia_forward(src, stmt_span.end as usize);
    let next_char: String = src[next..].chars().take(1).collect();
    let preserve = super::unicorn_text::needs_semicolon(cx, stmt_span.start, &next_char);
    let removal = super::unicorn_ast::remove_statements(cx, statement, statement, preserve);
    for e in removal.edits {
        fix = fix.and(e.span, e.text);
    }
    Some(fix)
}
