//! `unicorn/no-undeclared-class-members`: `this.name` inside a class (without `extends`)
//! must name a declared instance member: a field, method or accessor (also `declare` and
//! abstract ones), a constructor parameter property, or a `this.x = ...` in the constructor
//! itself (not in nested functions). Static members and static code, keys and decorators,
//! and `this` rebound by a nested non-arrow function are not checked. The first plain
//! `this.x = value` outside the constructor gets a suggestion declaring `x;` before the
//! first member.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_text::static_string_value;
use super::unicorn_util::skip_wrappers;
use super::unicorn_util::{indent_unit, line_indent, line_start};
use ts_r_ast::{Ast, HeritageKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-undeclared-class-members",
        "Require class members to be declared.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUndeclaredClassMembers>,
};

#[derive(Debug, Default)]
struct NoUndeclaredClassMembers;

/// The static name of a class member key (`foo`, `'foo'`, `['foo']`, `` [`foo`] ``).
fn static_key_name(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::ComputedKey { expr } => static_string_value(ast, skip_parens(ast, expr)),
        _ => static_string_value(ast, key),
    }
}

/// `this.name` (through optional chains and TS wrappers on `this`): the name.
fn this_member_name(ast: &Ast, id: NodeId) -> Option<&str> {
    let Node::Member { obj, prop, .. } = *ast.node(id) else {
        return None;
    };
    if !matches!(ast.node(skip_wrappers(ast, obj)), Node::This {}) {
        return None;
    }
    match ast.node(prop) {
        Node::Ident { .. } => ident_name(ast, prop),
        _ => None,
    }
}

fn class_parts(ast: &Ast, class: NodeId) -> Option<(&[NodeId], &[NodeId])> {
    match *ast.node(class) {
        Node::ClassDecl {
            heritage, members, ..
        }
        | Node::ClassExpr {
            heritage, members, ..
        } => Some((ast.list(heritage), ast.list(members))),
        _ => None,
    }
}

fn member_modifiers(ast: &Ast, member: NodeId) -> Option<Modifiers> {
    match *ast.node(member) {
        Node::MethodDef { modifiers, .. } | Node::PropertyDef { modifiers, .. } => Some(modifiers),
        _ => None,
    }
}

/// The class `this` at `node` belongs to and the member of that class containing `node`, or
/// `None` when a non-method function rebinds `this` first or there is no class.
fn this_owner(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let mut prev = node;
    let mut cur = cx.parent(node)?;
    loop {
        match *ast.node(cur) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                if class_parts(ast, cur).is_some_and(|(_, m)| m.contains(&prev)) {
                    return Some((cur, prev));
                }
            }
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } => {
                let parent = cx.parent(cur)?;
                let is_method = matches!(*ast.node(parent), Node::MethodDef { value, .. } if value == cur)
                    && cx
                        .parent(parent)
                        .is_some_and(|c| class_parts(ast, c).is_some());
                if !is_method {
                    return None;
                }
            }
            _ => {}
        }
        prev = cur;
        cur = cx.parent(cur)?;
    }
}

fn contains(cx: &Ctx<'_, '_>, outer: NodeId, inner: NodeId) -> bool {
    let (o, i) = (cx.span(outer), cx.span(inner));
    o.start <= i.start && i.end <= o.end
}

/// `node` is in the key or a decorator of `member`, evaluated outside the instance.
fn in_member_definition(cx: &Ctx<'_, '_>, member: NodeId, node: NodeId) -> bool {
    let ast = cx.ast();
    let (key, decorators) = match *ast.node(member) {
        Node::MethodDef {
            key, decorators, ..
        }
        | Node::PropertyDef {
            key, decorators, ..
        } => (key, ast.list(decorators)),
        _ => return false,
    };
    contains(cx, key, node) || decorators.iter().any(|&d| contains(cx, d, node))
}

fn is_constructor(ast: &Ast, member: NodeId) -> bool {
    matches!(
        ast.node(member),
        Node::MethodDef {
            kind: MethodKind::Constructor,
            ..
        }
    )
}

fn declared_names(cx: &Ctx<'_, '_>, members: &[NodeId]) -> Vec<String> {
    let ast = cx.ast();
    let mut names = vec!["constructor".to_owned()];
    for &m in members {
        let key = match *ast.node(m) {
            Node::MethodDef { key, kind, .. } if kind != MethodKind::Constructor => key,
            Node::PropertyDef { key, .. } => key,
            _ => continue,
        };
        if member_modifiers(ast, m).is_some_and(|f| f.contains(Modifiers::STATIC)) {
            continue;
        }
        names.extend(static_key_name(ast, key));
    }
    let ctor = members.iter().find_map(|&m| match *ast.node(m) {
        Node::MethodDef {
            kind: MethodKind::Constructor,
            modifiers,
            value,
            ..
        } if !modifiers.contains(Modifiers::STATIC) => match *ast.node(value) {
            Node::FunctionExpr {
                params,
                body: Some(body),
                ..
            } => Some((params, body)),
            _ => None,
        },
        _ => None,
    });
    let Some((params, body)) = ctor else {
        return names;
    };
    for &p in ast.list(params) {
        if let Node::Param {
            modifiers, pattern, ..
        } = *ast.node(p)
            && modifiers.intersects(
                Modifiers::PUBLIC
                    | Modifiers::PRIVATE
                    | Modifiers::PROTECTED
                    | Modifiers::READONLY
                    | Modifiers::OVERRIDE,
            )
            && let Some(n) = ident_name(ast, pattern)
        {
            names.push(n.to_owned());
        }
    }
    let mut stack = vec![body];
    while let Some(id) = stack.pop() {
        match *ast.node(id) {
            Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Arrow { .. }
            | Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. } => continue,
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } => names.extend(this_member_name(ast, skip_parens(ast, left)).map(str::to_owned)),
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    names
}

/// `node` is the target of a plain `=` (through parentheses).
fn is_simple_assignment_target(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        match *ast.node(p) {
            Node::Paren { .. } => cur = p,
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } => return left == cur,
            _ => return false,
        }
    }
    false
}

/// Inserts `<indent><name>;` on its own line before the first member (or the comment right
/// before it), unless that member starts on the line of the class's `{`.
fn declare_field(
    cx: &Ctx<'_, '_>,
    class: NodeId,
    first: NodeId,
    name: &str,
    unit: &mut Option<String>,
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let first_start = cx.span(first).start as usize;
    let brace_end = skip_trivia_backward(ast, src, first_start);
    if brace_end == 0 || src.as_bytes()[brace_end - 1] != b'{' {
        return None;
    }
    let target = ast
        .comments_in(Span::new(brace_end as u32, first_start as u32))
        .iter()
        .find(|c| c.span.end as usize <= first_start)
        .map_or(first_start, |c| c.span.start as usize);
    let target_line = line_start(src, target);
    if target_line < brace_end {
        return None;
    }
    let before = &src[line_start(src, first_start)..first_start];
    let own = &before[before.trim_end_matches(char::is_whitespace).len()..];
    let indent = if own.is_empty() {
        let class_indent = line_indent(src, cx.span(class).start as usize);
        let unit = unit.get_or_insert_with(|| indent_unit(cx));
        format!("{class_indent}{unit}")
    } else {
        own.to_owned()
    };
    Some(Fix::insert(
        target_line as u32,
        format!("{indent}{name};{}", linebreak(src)),
    ))
}

impl Rule for NoUndeclaredClassMembers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (mut accesses, mut classes) = (Vec::new(), Vec::new());
        let mut stack = vec![node];
        while let Some(id) = stack.pop() {
            match ast.node(id) {
                Node::Member { .. } => {
                    if let Some(name) = this_member_name(ast, id) {
                        accesses.push((id, name));
                    }
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => classes.push(id),
                _ => {}
            }
            let at = stack.len();
            ast.for_each_child(id, |c| stack.push(c));
            stack[at..].reverse();
        }
        // Accesses by the class their `this` belongs to (looked up per class, in order).
        let mut by_class: rustc_hash::FxHashMap<NodeId, Vec<(NodeId, &str, NodeId)>> =
            Default::default();
        for &(id, name) in &accesses {
            if let Some((owner, member)) = this_owner(cx, id) {
                by_class.entry(owner).or_default().push((id, name, member));
            }
        }
        let mut unit = None;
        let mut reports: Vec<(Span, String, Option<Fix>)> = Vec::new();
        for &class in &classes {
            let Some((heritage, members)) = class_parts(ast, class) else {
                continue;
            };
            let extends = heritage.iter().any(|&h| {
                matches!(
                    ast.node(h),
                    Node::HeritageClause {
                        kind: HeritageKind::Extends,
                        ..
                    }
                )
            });
            if extends {
                continue;
            }
            let declared = declared_names(cx, members);
            let mut suggested: Vec<&str> = Vec::new();
            for &(access, name, member) in by_class.get(&class).map_or(&[][..], Vec::as_slice) {
                let is_static = matches!(ast.node(member), Node::StaticBlock { .. })
                    || member_modifiers(ast, member).is_some_and(|f| f.contains(Modifiers::STATIC));
                if is_static
                    || in_member_definition(cx, member, access)
                    || declared.iter().any(|d| d == name)
                {
                    continue;
                }
                let mut fix = None;
                if is_simple_assignment_target(cx, access)
                    && !is_constructor(ast, member)
                    && !suggested.contains(&name)
                {
                    fix = declare_field(cx, class, members[0], name, &mut unit);
                    if fix.is_some() {
                        suggested.push(name);
                    }
                }
                reports.push((cx.span(access), name.to_owned(), fix));
            }
        }
        for (span, name, fix) in reports {
            let report = cx
                .report(
                    span,
                    format!("Class member `{name}` is used but not declared."),
                )
                .id("no-undeclared-class-members");
            if let Some(fix) = fix {
                report.suggest(
                    "no-undeclared-class-members/suggestion",
                    format!("Declare `{name}` as a class field."),
                    fix,
                );
            }
        }
    }
}
