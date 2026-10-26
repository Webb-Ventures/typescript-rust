//! `@typescript-eslint/prefer-for-of`: a `for (let i = 0; i < arr.length; i++)` loop whose
//! index is only used to read `arr[i]` should be a `for-of` loop.
//!
//! The loop must declare one non-`const` index starting at `0`, test `i < x.length` and step
//! with `i++`, `++i`, `i += 1`, `i = i + 1` or `i = 1 + i`. Inside the body every use of the
//! index must be `x[i]` with the same object text (not `this`), and not written to
//! (assigned, updated, deleted, or a target inside a destructuring, also through
//! `!`/`as`/`satisfies`/`<T>`).

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-for-of",
        "Enforce the use of `for-of` loop over the standard `for` loop where possible",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferForOf>,
};

#[derive(Debug, Default)]
struct PreferForOf;

fn is_number(ast: &Ast, id: NodeId, v: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == v)
}

fn is_name(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

fn is_increment(ast: &Ast, update: NodeId, name: &str) -> bool {
    match *ast.node(skip_parens(ast, update)) {
        Node::Update {
            op: UpdateOp::Increment,
            arg,
            ..
        } => is_name(ast, arg, name),
        Node::Assign { op, left, right } if is_name(ast, left, name) => match op {
            AssignOp::AddAssign => is_number(ast, right, 1.0),
            AssignOp::Assign => matches!(
                *ast.node(skip_parens(ast, right)),
                Node::Binary { op: BinaryOp::Add, left: l, right: r }
                    if (is_name(ast, l, name) && is_number(ast, r, 1.0))
                        || (is_number(ast, l, 1.0) && is_name(ast, r, name))
            ),
            _ => false,
        },
        _ => false,
    }
}

impl PreferForOf {
    /// The ESTree parent of `id` (parentheses skipped), with the child it came from.
    fn up(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<(NodeId, NodeId)> {
        let ast = cx.ast();
        for _ in 0..10_000 {
            let p = cx.parent(id)?;
            if matches!(ast.node(p), Node::Paren { .. }) {
                id = p;
                continue;
            }
            return Some((p, id));
        }
        None
    }

    /// `isAssignee`: whether the member expression `id` is written to.
    fn assignee(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        let ast = cx.ast();
        let mut node = id;
        for _ in 0..10_000 {
            let Some((p, child)) = Self::up(cx, node) else {
                return false;
            };
            match *ast.node(p) {
                Node::Assign { left, .. } => return left == child,
                Node::Unary {
                    op: UnaryOp::Delete,
                    ..
                }
                | Node::Update { .. } => return true,
                Node::ArrayLit { .. } | Node::Spread { .. } => {
                    return is_assignment_target(ast, cx.file.parents, child);
                }
                Node::Property { value, .. } if value == child => match cx.parent(p) {
                    Some(obj) if matches!(ast.node(obj), Node::ObjectLit { .. }) => node = obj,
                    _ => return false,
                },
                Node::NonNull { .. }
                | Node::As { .. }
                | Node::TypeAssertion { .. }
                | Node::Satisfies { .. } => node = p,
                _ => return false,
            }
        }
        false
    }
}

impl Rule for PreferForOf {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn exit_kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::For]
    }
    fn exit(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::For {
            init: Some(init),
            test: Some(test),
            update: Some(update),
            body,
        } = *ast.node(node)
        else {
            return;
        };
        let Node::VarDecl { kind, decls, .. } = *ast.node(init) else {
            return;
        };
        let [decl] = ast.list(decls) else { return };
        if kind == VarKind::Const {
            return;
        }
        let Node::VarDeclarator {
            name,
            init: Some(start),
            ..
        } = *ast.node(*decl)
        else {
            return;
        };
        if !is_number(ast, start, 0.0) || !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(index) = ident_name(ast, name) else {
            return;
        };
        let Node::Binary {
            op: BinaryOp::Lt,
            left,
            right,
        } = *ast.node(skip_parens(ast, test))
        else {
            return;
        };
        let right = skip_parens(ast, right);
        if !is_name(ast, left, index) || chain_has_optional(ast, right) {
            return;
        }
        let Node::Member {
            obj: array, prop, ..
        } = *ast.node(right)
        else {
            return;
        };
        if !is_name(ast, prop, "length") || !is_increment(ast, update, index) {
            return;
        }
        let array_text = cx.span(skip_parens(ast, array)).text(src);
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        let body_span = cx.span(body);
        let only_reads = sem.symbol(sym).refs.iter().all(|&r| {
            let id = sem.reference(r).node;
            let s = cx.span(id);
            if s.start < body_span.start || s.end > body_span.end {
                return true;
            }
            let Some((p, child)) = Self::up(cx, id) else {
                return false;
            };
            match *ast.node(p) {
                Node::Index { obj, index, .. } if index == child => {
                    let obj = skip_parens(ast, obj);
                    !matches!(ast.node(obj), Node::This {})
                        && cx.span(obj).text(src) == array_text
                        && !Self::assignee(cx, p)
                }
                _ => false,
            }
        });
        if only_reads {
            cx.report(
                cx.span(node),
                "Expected a `for-of` loop instead of a `for` loop with this simple iteration.",
            )
            .id("preferForOf");
        }
    }
}
