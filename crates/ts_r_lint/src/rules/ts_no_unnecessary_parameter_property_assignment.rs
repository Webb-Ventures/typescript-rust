//! `@typescript-eslint/no-unnecessary-parameter-property-assignment`: `this.x = x` in a
//! constructor where `x` is already a parameter property.

use rustc_hash::FxHashSet;
use ts_r_ast::ParentMap;
use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;
use crate::semantic::DeclKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-parameter-property-assignment",
        "Disallow unnecessary assignment of constructor property parameter",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoUnnecessaryParameterPropertyAssignment>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryParameterPropertyAssignment;

const PARAM_PROPERTY: Modifiers = Modifiers::PUBLIC
    .union(Modifiers::PRIVATE)
    .union(Modifiers::PROTECTED)
    .union(Modifiers::READONLY)
    .union(Modifiers::OVERRIDE);

/// Assignments evaluated directly in a function body (or an arrow called in place there),
/// in source order.
struct Assigns<'p> {
    parents: &'p ParentMap,
    /// Arrow IIFEs are only entered when they sit directly in the walked code (and, when
    /// set, are called by this call).
    allow_iife: bool,
    iife_call: Option<NodeId>,
    in_iife: bool,
    root: NodeId,
    found: Vec<NodeId>,
}

impl Visitor for Assigns<'_> {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match ast.node(id) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => Walk::SkipChildren,
            Node::Arrow { .. } if id != self.root => {
                let iife = parent_skip_parens(ast, self.parents, id).is_some_and(|p| {
                    matches!(ast.node(p), Node::Call { .. })
                        && self.iife_call.is_none_or(|c| c == p)
                });
                if iife && self.allow_iife && !self.in_iife {
                    self.in_iife = true;
                    Walk::Continue
                } else {
                    Walk::SkipChildren
                }
            }
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } if id != self.root => {
                Walk::SkipChildren
            }
            Node::Assign { .. } => {
                self.found.push(id);
                Walk::Continue
            }
            _ => Walk::Continue,
        }
    }
    fn leave(&mut self, ast: &ts_r_ast::Ast, id: NodeId) {
        if matches!(ast.node(id), Node::Arrow { .. }) && id != self.root {
            self.in_iife = false;
        }
    }
}

/// `this.x` / `this["x"]`: `x`.
fn this_property<'a>(ast: &'a ts_r_ast::Ast, src: &'a str, n: NodeId) -> Option<String> {
    match *ast.node(n) {
        Node::Member { obj, prop, .. } if matches!(ast.node(obj), Node::This {}) => {
            ident_name(ast, prop).map(String::from)
        }
        Node::Index { obj, index, .. } if matches!(ast.node(obj), Node::This {}) => {
            static_value_string(ast, src, index).map(|s| s.into_owned())
        }
        _ => None,
    }
}

fn is_param_ref(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    sem.symbol_of_ident(ident).is_some_and(|s| {
        sem.symbol(s)
            .decls
            .first()
            .is_some_and(|d| d.kind == DeclKind::Param)
    })
}

impl Rule for NoUnnecessaryParameterPropertyAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *cx.node(node)
        else {
            return;
        };
        let members = ast.list(members);
        // Names assigned while field initializers run.
        let mut before_ctor: FxHashSet<String> = FxHashSet::default();
        for &m in members {
            let Node::PropertyDef { value: Some(v), .. } = *ast.node(m) else {
                continue;
            };
            let v = skip_parens(ast, v);
            let mut w = Assigns {
                parents: cx.file.parents,
                allow_iife: true,
                iife_call: Some(v),
                in_iife: false,
                root: v,
                found: Vec::new(),
            };
            if !matches!(ast.node(v), Node::Arrow { .. } | Node::FunctionExpr { .. }) {
                walk(ast, v, &mut w);
            }
            for a in w.found {
                if let Node::Assign { left, .. } = *ast.node(a)
                    && let Some(n) = this_property(ast, src, skip_parens(ast, left))
                {
                    before_ctor.insert(n);
                }
            }
        }
        let mut reports = Vec::new();
        for &m in members {
            let Node::MethodDef {
                kind: MethodKind::Constructor,
                value,
                ..
            } = *ast.node(m)
            else {
                continue;
            };
            let Node::FunctionExpr {
                params,
                body: Some(body),
                ..
            } = *ast.node(value)
            else {
                continue;
            };
            let mut w = Assigns {
                parents: cx.file.parents,
                allow_iife: true,
                iife_call: None,
                in_iife: false,
                root: value,
                found: Vec::new(),
            };
            walk(ast, body, &mut w);
            let mut assigned_before: FxHashSet<String> = FxHashSet::default();
            for a in w.found {
                let Node::Assign { left, op, right } = *ast.node(a) else {
                    continue;
                };
                let left = skip_parens(ast, left);
                if let Node::Ident { .. } = ast.node(left) {
                    if is_param_ref(cx, left) {
                        assigned_before.insert(ident_name(ast, left).unwrap_or_default().into());
                    }
                    continue;
                }
                let Some(name) = this_property(ast, src, left) else {
                    continue;
                };
                if !matches!(
                    op,
                    AssignOp::Assign
                        | AssignOp::AndAssign
                        | AssignOp::OrAssign
                        | AssignOp::CoalesceAssign
                ) {
                    assigned_before.insert(name);
                    continue;
                }
                let mut r = skip_parens(ast, right);
                while let Node::As { expr, .. } | Node::NonNull { expr } = *ast.node(r) {
                    r = skip_parens(ast, expr);
                }
                if !matches!(ast.node(r), Node::Ident { .. })
                    || ident_name(ast, r) != Some(name.as_str())
                    || !is_param_ref(cx, r)
                {
                    continue;
                }
                let has_property = ast.list(params).iter().any(|&p| {
                    matches!(*ast.node(p), Node::Param { modifiers, pattern, .. }
                        if modifiers.intersects(PARAM_PROPERTY) && ident_name(ast, pattern) == Some(name.as_str()))
                });
                if has_property && !assigned_before.contains(&name) && !before_ctor.contains(&name)
                {
                    reports.push(a);
                }
            }
        }
        for a in reports {
            cx.report_node(
                a,
                "This assignment is unnecessary since it is already assigned by a parameter property.",
            )
            .id("unnecessaryAssign");
        }
    }
}
