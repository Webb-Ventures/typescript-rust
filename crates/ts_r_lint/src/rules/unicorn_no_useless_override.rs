//! `unicorn/no-useless-override`: a class method (in a class with `extends`; not a getter,
//! setter, constructor, generator, overload, abstract or `declare` member, and without
//! accessibility, `?`, decorators or parameter decorators) whose whole body forwards its
//! plain parameters (and a trailing rest as a spread) to `super.<same name>(...)`.
//! `return super.m(...)` counts unless the method is `async` and the call is not known to
//! return a promise; a bare `super.m(...)` statement only when the method is not `async`
//! and the call is known to return `void`/`undefined`. Without type information neither is
//! known, so those stay unreported. The fix removes the method and the spaces before it
//! (keeping one line break), unless it holds a comment.

use super::prelude::*;
use super::unicorn_ast::superclass;
use super::unicorn_static::{Static, static_value};
use super::unicorn_text::has_possible_side_effect;
use crate::ast_util::key_name;
use ts_r_ast::{Ast, MethodKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-override",
        "Disallow useless overrides of class methods.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessOverride>,
};

#[derive(Debug, Default)]
struct NoUselessOverride;

/// A method key's static name; private names and dynamic keys have none.
fn method_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::PrivateName { .. } => None,
        Node::ComputedKey { expr } => computed_name(cx, expr),
        _ => key_name(ast, cx.src(), key).map(|c| c.into_owned()),
    }
}

/// `getStaticPropertyName` of a computed key: its constant value as a property key
/// (`Symbol.iterator` stands for the symbol), when evaluating it has no side effect.
fn computed_name(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<String> {
    if has_possible_side_effect(cx, expr) {
        return None;
    }
    match static_value(cx, expr)? {
        Static::Builtin(path) => Some(format!("Symbol({path})")),
        v => v.to_js_string(),
    }
}

fn forwards(ast: &Ast, args: &[NodeId], params: &[NodeId]) -> bool {
    args.len() == params.len()
        && params.iter().zip(args).all(|(&p, &a)| {
            let Node::Param {
                decorators: _,
                rest,
                pattern,
                init: None,
                modifiers,
                ..
            } = *ast.node(p)
            else {
                return false;
            };
            if !modifiers.is_empty() || !matches!(ast.node(pattern), Node::Ident { .. }) {
                return false;
            }
            let name = ident_name(ast, pattern);
            if rest {
                matches!(*ast.node(a), Node::Spread { arg }
                    if matches!(ast.node(arg), Node::Ident { .. }) && ident_name(ast, arg) == name)
            } else {
                matches!(ast.node(a), Node::Ident { .. }) && ident_name(ast, a) == name
            }
        })
}

impl Rule for NoUselessOverride {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::MethodDef {
            decorators,
            modifiers,
            kind: MethodKind::Method,
            key,
            optional: false,
            value,
        } = *cx.node(node)
        else {
            return;
        };
        if !ast.list(decorators).is_empty()
            || modifiers.intersects(Modifiers::PUBLIC | Modifiers::PRIVATE | Modifiers::PROTECTED)
            || modifiers.intersects(Modifiers::ABSTRACT | Modifiers::DECLARE)
        {
            return;
        }
        let Some(class) = cx.parent(node) else {
            return;
        };
        if !matches!(
            ast.node(class),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        ) || superclass(ast, class).is_none()
        {
            return;
        }
        let Node::FunctionExpr {
            flags,
            params,
            body: Some(body),
            ..
        } = *ast.node(value)
        else {
            return;
        };
        let params = ast.list(params);
        if flags.contains(FnFlags::GENERATOR)
            || params
                .iter()
                .any(|&p| matches!(*ast.node(p), Node::Param { decorators, .. } if !ast.list(decorators).is_empty()))
        {
            return;
        }
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let [stmt] = *ast.list(stmts) else {
            return;
        };
        let (call, returned) = match *ast.node(stmt) {
            Node::Return { arg: Some(a) } => (a, true),
            Node::ExprStmt { expr } => (expr, false),
            _ => return,
        };
        let call = skip_parens(ast, call);
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(call)
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let called = match *ast.node(callee) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } if matches!(ast.node(obj), Node::Super {}) => method_name(cx, prop),
            Node::Index {
                obj,
                optional: false,
                index,
            } if matches!(ast.node(obj), Node::Super {}) => computed_name(cx, index),
            _ => return,
        };
        let Some(name) = method_name(cx, key) else {
            return;
        };
        if called.as_deref() != Some(name.as_str()) || !forwards(ast, ast.list(args), params) {
            return;
        }
        let is_static = modifiers.contains(Modifiers::STATIC);
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *ast.node(class)
        else {
            return;
        };
        let overloaded = ast.list(members).iter().any(|&m| {
            m != node
                && matches!(*ast.node(m), Node::MethodDef { kind: MethodKind::Method, modifiers: mm, key: k, value: v, .. }
                    if mm.contains(Modifiers::STATIC) == is_static
                        && matches!(*ast.node(v), Node::FunctionExpr { body: None, .. })
                        && method_name(cx, k).as_deref() == Some(name.as_str()))
        });
        if overloaded {
            return;
        }
        // Without type information the call's return type is unknown: an `async` forward
        // might wrap a non-promise, and a bare call might drop a value.
        if !returned || flags.contains(FnFlags::ASYNC) {
            return;
        }
        let span = cx.span(node);
        let fix = cx.ast().comments_in(span).is_empty().then(|| {
            let before = &src[..span.start as usize];
            let trimmed = before.trim_end_matches(|c: char| c.is_whitespace());
            let spaces = &before[trimmed.len()..];
            let line_break = if spaces.starts_with("\r\n") {
                "\r\n"
            } else if spaces.starts_with('\n') {
                "\n"
            } else if spaces.starts_with('\r') {
                "\r"
            } else {
                ""
            };
            Fix::replace(Span::new(trimmed.len() as u32, span.end), line_break)
        });
        let key_inner = match *ast.node(key) {
            Node::ComputedKey { expr } => expr,
            _ => key,
        };
        let key_span = cx.span(key_inner);
        let super_call = cx.text(callee).to_owned();
        cx.report(
            key_span,
            format!("Useless override; this method only forwards to `{super_call}(…)`."),
        )
        .fix_opt(fix);
    }
}
