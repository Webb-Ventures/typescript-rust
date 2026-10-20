//! `for-direction`: enforce a `for` loop update clause moving the counter in the right direction.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "for-direction",
        "Enforce `for` loop update clause moving the counter in the right direction",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<ForDirection>,
};

#[derive(Debug, Default)]
struct ForDirection;

/// +1 / -1 for a numeric literal (or negated literal), 0 when unknown.
fn literal_sign(cx: &Ctx<'_, '_>, id: NodeId) -> i32 {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Some(v) = static_number(cx, id) {
        return if v > 0.0 {
            1
        } else if v < 0.0 {
            -1
        } else {
            0
        };
    }
    match *ast.node(id) {
        Node::NumberLit { value } => {
            let v = value.get();
            if v > 0.0 {
                1
            } else if v < 0.0 {
                -1
            } else {
                0
            }
        }
        Node::BigIntLit { value } => {
            let t = ast.atom(value);
            if t.trim_start_matches(['0', '_', 'x', 'X', 'o', 'O', 'b', 'B'])
                .is_empty()
            {
                0
            } else {
                1
            }
        }
        Node::BoolLit { value } => i32::from(value),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => -literal_sign(cx, arg),
        Node::Unary {
            op: UnaryOp::Plus,
            arg,
        } => literal_sign(cx, arg),
        _ => 0,
    }
}

impl Rule for ForDirection {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::For]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::For {
            test: Some(test),
            update: Some(update),
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, test)) else {
            return;
        };
        let mut updates: Vec<NodeId> = Vec::new();
        flatten_seq(ast, update, &mut updates);
        let Node::Binary {
            left: tl,
            right: tr,
            ..
        } = *ast.node(skip_parens(ast, test))
        else {
            return;
        };
        let names = [
            ident_name(ast, skip_parens(ast, tl)),
            ident_name(ast, skip_parens(ast, tr)),
        ];
        let matching: Vec<NodeId> = updates
            .into_iter()
            .filter(|&u| {
                let target = match *ast.node(skip_parens(ast, u)) {
                    Node::Update { arg, .. } => arg,
                    Node::Assign { left, .. } => left,
                    _ => return false,
                };
                let n = ident_name(ast, skip_parens(ast, target));
                n.is_some() && names.contains(&n)
            })
            .collect();
        let Some(&update) = matching.first() else {
            return;
        };
        if matching.len() > 1 {
            // Several updates of the counter: only certain when they all agree.
            let signs: Vec<i32> = matching
                .iter()
                .map(|&u| match *ast.node(skip_parens(ast, u)) {
                    Node::Update { op, .. } => {
                        if op == UpdateOp::Increment {
                            1
                        } else {
                            -1
                        }
                    }
                    Node::Assign {
                        op: AssignOp::AddAssign,
                        right,
                        ..
                    } => literal_sign(cx, right),
                    Node::Assign {
                        op: AssignOp::SubAssign,
                        right,
                        ..
                    } => -literal_sign(cx, right),
                    _ => 0,
                })
                .map(|s| if s == 0 { 2 } else { s })
                .map(|s| match s {
                    2 => i32::MAX,
                    s => s,
                })
                .collect();
            if signs.iter().any(|&s| s != signs[0]) {
                return;
            }
        }
        let (counter, dir) = match *ast.node(skip_parens(ast, update)) {
            Node::Update { op, arg, .. } => (
                skip_parens(ast, arg),
                if op == UpdateOp::Increment { 1 } else { -1 },
            ),
            Node::Assign {
                left,
                op: AssignOp::AddAssign,
                right,
            } => (skip_parens(ast, left), literal_sign(cx, right)),
            Node::Assign {
                left,
                op: AssignOp::SubAssign,
                right,
            } => (skip_parens(ast, left), -literal_sign(cx, right)),
            _ => return,
        };
        let Some(name) = ident_name(ast, counter) else {
            return;
        };
        if dir == 0 {
            return;
        }
        let (l, r) = (skip_parens(ast, left), skip_parens(ast, right));
        let wanted = if ident_name(ast, l) == Some(name) {
            match op {
                BinaryOp::Lt | BinaryOp::LtEq => 1,
                BinaryOp::Gt | BinaryOp::GtEq => -1,
                _ => return,
            }
        } else if ident_name(ast, r) == Some(name) {
            match op {
                BinaryOp::Lt | BinaryOp::LtEq => -1,
                BinaryOp::Gt | BinaryOp::GtEq => 1,
                _ => return,
            }
        } else {
            return;
        };
        if wanted != dir {
            let start = cx.span(node).start;
            let Node::For {
                update: Some(full_update),
                ..
            } = *cx.node(node)
            else {
                return;
            };
            let end = find_char(cx.src(), ast.span(full_update).end, b')')
                .map_or(cx.span(node).end, |c| c + 1);
            cx.report(
                Span::new(start, end),
                "The update clause in this loop moves the variable in the wrong direction.",
            )
            .id("incorrectDirection");
        }
    }
}

fn flatten_seq(ast: &ts_r_ast::Ast, id: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(skip_parens(ast, id)) {
        Node::Seq { exprs } => {
            for &e in ast.list(exprs) {
                flatten_seq(ast, e, out);
            }
        }
        _ => out.push(id),
    }
}
