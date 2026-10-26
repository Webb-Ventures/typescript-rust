//! `unicorn/prefer-classlist-toggle`: `el.classList.toggle(name, condition)` over choosing
//! between `el.classList.add(name)` and `el.classList.remove(name)`.
//!
//! Three shapes: an `if`/`else` whose branches are (alone, or alone in a block) the two
//! calls; a `?:` whose arms are the two calls; and `el.classList[c ? "add" : "remove"](name)`
//! (static strings, the `?:` the computed key of a non-optional member called with one
//! argument). The calls are `<x>.classList.add|remove(<one argument>)` without optional
//! call or member (`<x>?.classList` is fine), on the same `classList` reference and the
//! same class name.
//!
//! The condition is passed as the second argument, negated with `!` when the true branch
//! removes, or dropped when it is (or negates) a matching `classList.contains(name)` call.
//! A `?.` on any of the `classList` members is kept. The fix is withheld when a comment is
//! inside the replaced node (the computed member, for the third shape); it is a suggestion
//! unless the node is an `if`, or the `?:`/call is a whole expression statement.

use super::prelude::*;
use super::unicorn_text::{
    expression_text, is_same_reference, needs_semicolon, paren_span, paren_text,
};
use super::unicorn_util::space_around_keyword;
use super::unicorn_util_h1::static_string;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-classlist-toggle",
        "Prefer using `Element#classList.toggle()` to toggle class names.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferClasslistToggle>,
};

#[derive(Debug, Default)]
struct PreferClasslistToggle;

const MESSAGE: &str = "Prefer using `Element#classList.toggle()` to toggle class names.";

/// A `<x>.classList.<method>(<arg>)` call.
#[derive(Clone, Copy)]
struct ClassListCall {
    /// The `<x>.classList` member.
    class_list: NodeId,
    /// `<x>`.
    element: NodeId,
    method_remove: bool,
    arg: NodeId,
}

fn is_class_list(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    (ident_name(ast, prop) == Some("classList")).then_some(obj)
}

fn class_list_call(ast: &Ast, id: NodeId, methods: &[&str]) -> Option<ClassListCall> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let name = ident_name(ast, prop).filter(|_| matches!(ast.node(prop), Node::Ident { .. }))?;
    if !methods.contains(&name) {
        return None;
    }
    let &[arg] = ast.list(args) else {
        return None;
    };
    if matches!(ast.node(arg), Node::Spread { .. }) {
        return None;
    }
    let class_list = skip_parens(ast, obj);
    let element = is_class_list(ast, class_list)?;
    Some(ClassListCall {
        class_list,
        element,
        method_remove: name == "remove",
        arg,
    })
}

fn optional_member(ast: &Ast, member: NodeId) -> bool {
    matches!(ast.node(member), Node::Member { optional: true, .. })
}

fn same_element_and_name(cx: &Ctx<'_, '_>, a: &ClassListCall, b: &ClassListCall) -> bool {
    is_same_reference(cx, a.class_list, b.class_list) && is_same_reference(cx, a.arg, b.arg)
}

/// The statement alone in a branch: itself, or the only statement of a block.
fn single_statement(ast: &Ast, id: Option<NodeId>) -> Option<NodeId> {
    let id = id?;
    match *ast.node(id) {
        Node::Block { stmts } => match ast.list(stmts) {
            &[only] => Some(only),
            _ => None,
        },
        _ => Some(id),
    }
}

/// A matching `classList.contains(name)` test (possibly negated) that makes the condition
/// redundant.
fn contains_call(
    cx: &Ctx<'_, '_>,
    cond: NodeId,
    negative: bool,
    call: &ClassListCall,
    depth: u32,
) -> Option<ClassListCall> {
    let ast = cx.ast();
    let cond = skip_parens(ast, cond);
    if !negative {
        if depth < 100
            && let Node::Unary {
                op: UnaryOp::Not,
                arg,
            } = *ast.node(cond)
        {
            return contains_call(cx, arg, true, call, depth + 1);
        }
        return None;
    }
    let c = class_list_call(ast, cond, &["contains"])?;
    same_element_and_name(cx, &c, call).then_some(c)
}

/// The condition as the second argument of `toggle`.
fn condition_text(cx: &Ctx<'_, '_>, cond: NodeId, negative: bool) -> String {
    let ast = cx.ast();
    if negative {
        let inner = skip_parens(ast, cond);
        let parens = precedence(ast, inner) < 16
            || matches!(
                ast.node(inner),
                Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
            );
        return format!("!{}", expression_text(cx, inner, parens));
    }
    let text = paren_text(cx, cond).to_owned();
    let inner = skip_parens(ast, cond);
    if inner == cond && matches!(ast.node(cond), Node::Seq { .. }) {
        return format!("({text})");
    }
    text
}

/// The parent of `id`, through parentheses.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some(p);
        }
    }
}

fn is_statement_expression(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    parent_np(cx, id).is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }))
}

fn report(cx: &mut Ctx<'_, '_>, span: Span, auto: bool, fix: Option<Fix>) {
    let r = cx.report(span, MESSAGE).id("prefer-classlist-toggle/error");
    match fix {
        Some(f) if auto => {
            r.fix(f);
        }
        Some(f) => {
            r.suggest(
                "prefer-classlist-toggle/suggestion",
                "Replace with `Element#classList.toggle()`.",
                f,
            );
        }
        None => {}
    }
}

impl Rule for PreferClasslistToggle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If, NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (test, cons, alt, is_if) = match *ast.node(node) {
            Node::If { test, cons, alt } => (test, Some(cons), alt, true),
            Node::Conditional { test, cons, alt } => (test, Some(cons), Some(alt), false),
            _ => return,
        };
        let clause = |id: Option<NodeId>| {
            let mut s = single_statement(ast, id)?;
            if let Node::ExprStmt { expr } = *ast.node(s) {
                s = expr;
            }
            class_list_call(ast, s, &["add", "remove"])
        };
        if let (Some(c), Some(a)) = (clause(cons), clause(alt)) {
            if c.method_remove == a.method_remove || !same_element_and_name(cx, &c, &a) {
                return;
            }
            let span = cx.span(node);
            let fix = (ast.comments_in(span).is_empty()).then(|| {
                let negative = c.method_remove;
                let contains = contains_call(cx, test, negative, &c, 0);
                let cond = if contains.is_some() {
                    String::new()
                } else {
                    condition_text(cx, test, negative)
                };
                let optional = optional_member(ast, c.class_list)
                    || optional_member(ast, a.class_list)
                    || contains.is_some_and(|k| optional_member(ast, k.class_list));
                let mut text = format!(
                    "{}{}.classList.toggle({}{})",
                    paren_text(cx, c.element),
                    if optional { "?" } else { "" },
                    paren_text(cx, c.arg),
                    if cond.is_empty() {
                        String::new()
                    } else {
                        format!(", {cond}")
                    }
                );
                if is_if {
                    text.push(';');
                }
                if needs_semicolon(cx, span.start, &text) {
                    text.insert(0, ';');
                }
                let mut fix = Fix::replace(span, text);
                if !is_if {
                    for (s, t) in space_around_keyword(cx, node) {
                        fix = fix.and(s, t);
                    }
                }
                fix
            });
            let auto = is_if || is_statement_expression(cx, node);
            report(cx, span, auto, fix);
            return;
        }
        if !is_if {
            self.computed(cx, node, test, cons.unwrap_or(node), alt.unwrap_or(node));
        }
    }
}

impl PreferClasslistToggle {
    /// `el.classList[c ? "add" : "remove"](name)`.
    fn computed(
        &self,
        cx: &mut Ctx<'_, '_>,
        cond_expr: NodeId,
        test: NodeId,
        cons: NodeId,
        alt: NodeId,
    ) {
        let ast = cx.ast();
        let (Some(m1), Some(m2)) = (
            static_string(ast, skip_parens(ast, cons)),
            static_string(ast, skip_parens(ast, alt)),
        ) else {
            return;
        };
        if !matches!(m1.as_str(), "add" | "remove")
            || !matches!(m2.as_str(), "add" | "remove")
            || m1 == m2
        {
            return;
        }
        let Some(member) = parent_np(cx, cond_expr) else {
            return;
        };
        let Node::Index {
            obj,
            optional: false,
            index,
        } = *ast.node(member)
        else {
            return;
        };
        if skip_parens(ast, index) != cond_expr || is_class_list(ast, obj).is_none() {
            return;
        }
        let Some(call) = parent_np(cx, member) else {
            return;
        };
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(call)
        else {
            return;
        };
        if skip_parens(ast, callee) != member {
            return;
        }
        let &[arg] = ast.list(args) else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let member_span = cx.span(member);
        let fix = ast.comments_in(member_span).is_empty().then(|| {
            let negative = m1 == "remove";
            let class_list = skip_parens(ast, obj);
            let call_info = ClassListCall {
                class_list,
                element: is_class_list(ast, class_list).unwrap_or(class_list),
                method_remove: negative,
                arg,
            };
            let contains = contains_call(cx, test, negative, &call_info, 0);
            let mut fix = Fix::replace(
                Span::new(paren_span(cx, obj).end, member_span.end),
                ".toggle",
            );
            if contains.is_none() {
                let cond = condition_text(cx, test, negative);
                fix = fix.and(Span::at(paren_span(cx, arg).end), format!(", {cond}"));
            }
            fix
        });
        let auto = is_statement_expression(cx, call);
        report(cx, cx.span(cond_expr), auto, fix);
    }
}
