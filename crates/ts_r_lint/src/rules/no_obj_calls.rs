//! `no-obj-calls`: disallow calling global object properties as functions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-obj-calls",
        "Disallow calling global object properties as functions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoObjCalls>,
};

#[derive(Debug, Default)]
struct NoObjCalls;

const NON_CALLABLE: &[&str] = &["Atomics", "JSON", "Math", "Reflect", "Intl", "Temporal"];

/// The non-callable global `id` refers to: directly, through `globalThis.X`, or through a
/// `const`-like variable initialised from one (possibly via `?:` / `||`).
fn global_target(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<(&'static str, bool)> {
    if depth > 5 {
        return None;
    }
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    for name in NON_CALLABLE {
        if cx.is_global_ref(id, name) {
            return Some((name, false));
        }
    }
    match *ast.node(id) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            let o = skip_parens(ast, obj);
            if cx.is_global_ref(o, "globalThis")
                || cx.is_global_ref(o, "window")
                || cx.is_global_ref(o, "self")
            {
                let n = member_name(ast, cx.src(), id)?;
                let name = NON_CALLABLE.iter().find(|x| **x == n)?;
                if cx.semantic().is_global(name) {
                    return Some((name, false));
                }
            }
            None
        }
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let sym = sem.symbol_of_ident(id)?;
            let s = sem.symbol(sym);
            if s.decls.len() != 1
                || !matches!(
                    s.decls[0].kind,
                    DeclKind::Const | DeclKind::Let | DeclKind::Var
                )
            {
                return None;
            }
            // Never reassigned.
            let mut init = None;
            for r in &s.refs {
                let r = sem.reference(*r);
                if r.is_write() {
                    if !r.flags.contains(RefFlags::INIT) || init.is_some() {
                        return None;
                    }
                    init = r.write_value;
                }
            }
            let init = init?;
            init_target(cx, init, depth).map(|n| (n, true))
        }
        _ => None,
    }
}

fn init_target(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> Option<&'static str> {
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::Conditional { cons, alt, .. } => {
            init_target(cx, cons, depth + 1).or_else(|| init_target(cx, alt, depth + 1))
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            right,
        } => init_target(cx, left, depth + 1).or_else(|| init_target(cx, right, depth + 1)),
        _ => global_target(cx, e, depth + 1).map(|(n, _)| n),
    }
}

impl Rule for NoObjCalls {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (Node::Call { callee, .. } | Node::New { callee, .. }) = *cx.node(node) else {
            return;
        };
        let Some((name, via_ref)) = global_target(cx, callee, 0) else {
            return;
        };
        if via_ref {
            let text = cx.text(skip_parens(cx.ast(), callee)).to_owned();
            cx.report_node(
                node,
                format!("'{text}' is reference to '{name}', which is not a function."),
            )
            .id("unexpectedRefCall");
        } else {
            cx.report_node(node, format!("'{name}' is not a function."))
                .id("unexpectedCall");
        }
    }
}
