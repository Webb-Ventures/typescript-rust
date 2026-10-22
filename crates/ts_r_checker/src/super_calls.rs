//! `super` placement (TS2335, TS2336, TS2337, TS2660) and `super()` in constructors of
//! derived classes (TS2377, TS17005, TS17009, TS17011).
//!
//! Rules observed from tsc 7.0.2, for the constructor (with a body) of a class whose
//! `extends` expression is not `null`:
//! - No `super(...)` call in the body (outside nested functions, arrows and classes, where
//!   it is TS2337): TS2377 at the constructor.
//! - `this` (TS17009) or `super.x` (TS17011) where `super()` has not been called on every
//!   path from the start of the constructor (parameter initializers included). Only `this`
//!   whose container is the constructor itself counts: arrow and function bodies do not,
//!   decorators of a class nested in the constructor do. Unreachable code is fine.
//!
//! - A class that `extends null`: the first `super(...)` call is TS17005.
//!
//! The flow graph records calls only as statements, so a constructor with a `super(...)`
//! call inside a larger expression (`super() || this`) is not checked for TS17009/TS17011.

use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_binder::{FlowId, FlowKind};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Parent steps from `this` to its constructor.
const MAX_STEPS: u32 = 4096;
/// Flow nodes visited per check.
const MAX_FLOW: usize = 100_000;
/// Nodes visited when searching a constructor body for `super(...)` calls.
const MAX_NODES: usize = 1_000_000;

/// What a constructor body says about its `super(...)` calls.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuperCalls {
    None,
    /// Every call is an expression statement.
    Statements,
    /// Some call is part of a larger expression.
    Nested,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS17009/TS17011 for `this` or `super` expression `e`.
    pub(crate) fn check_super_called_before(&mut self, e: Loc, is_super: bool) {
        if !self.reporting(e.file) {
            return;
        }
        let Some(ctor) = self.derived_constructor_of(e) else {
            return;
        };
        if self.super_calls(ctor) == SuperCalls::Nested {
            return;
        }
        let graph = &self.c.bound(e.file).flow;
        let Some(start) = graph.flow_at(e.node) else {
            return;
        };
        if self.super_maybe_uncalled(e.file, start) {
            let info = if is_super {
                dg::SUPER_MUST_BE_CALLED_BEFORE_ACCESSING_A_PROPERTY_OF_SUPER_IN_THE_CONSTRUCTOR_OF_A_17011
            } else {
                dg::SUPER_MUST_BE_CALLED_BEFORE_ACCESSING_THIS_IN_THE_CONSTRUCTOR_OF_A_DERIVED_CLASS_17009
            };
            self.error(e, info, &[]);
        }
    }

    /// TS2377 for constructor method `m` (a `MethodDef`) without any `super(...)` call.
    pub(crate) fn check_derived_constructor_super(&mut self, m: Loc) {
        if !self.reporting(m.file) || self.c.file(m.file).is_js() || self.is_ambient_node(m) {
            return;
        }
        let ast = self.ast(m.file);
        let Node::MethodDef {
            kind: MethodKind::Constructor,
            value,
            ..
        } = *ast.node(m.node)
        else {
            return;
        };
        let Node::FunctionExpr { body: Some(_), .. } = *ast.node(value) else {
            return;
        };
        let parents = self.c.file(m.file).parents();
        let Some(class) = parents.parent(m.node) else {
            return;
        };
        if !self.extends_non_null(Loc::new(m.file, class)) {
            // A class that `extends null` has no base constructor to call: TS17005 at the
            // first `super(...)` call.
            if self.has_extends_clause(Loc::new(m.file, class))
                && let Some(call) = self.first_super_call(Loc::new(m.file, value))
            {
                self.error(
                    Loc::new(m.file, call),
                    dg::A_CONSTRUCTOR_CANNOT_CONTAIN_A_SUPER_CALL_WHEN_ITS_CLASS_EXTENDS_NULL_17005,
                    &[],
                );
            }
            return;
        }
        if self.super_calls(Loc::new(m.file, value)) == SuperCalls::None {
            self.error(
                m,
                dg::CONSTRUCTORS_FOR_DERIVED_CLASSES_MUST_CONTAIN_A_SUPER_CALL_2377,
                &[],
            );
        }
    }

    /// Where `super` expression `e` may not appear: TS2335 (in a class without `extends`),
    /// TS2336 (in constructor parameters), TS2337 (a call outside a constructor body, arrows
    /// included) or TS2660 (a property access outside class members and object literal
    /// methods; arrows are transparent).
    pub(crate) fn super_placement_error(
        &self,
        e: Loc,
    ) -> Option<crate::diagnostics_generated::DiagnosticInfo> {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let is_call = parents
            .parent(e.node)
            .is_some_and(|p| matches!(*ast.node(p), Node::Call { callee, .. } if callee == e.node));
        let call_or = |other| {
            Some(if is_call {
                dg::SUPER_CALLS_ARE_NOT_PERMITTED_OUTSIDE_CONSTRUCTORS_OR_IN_NESTED_FUNCTIONS_INSIDE_2337
            } else {
                other
            })
        };
        let not_derived = dg::SUPER_CAN_ONLY_BE_REFERENCED_IN_A_DERIVED_CLASS_2335;
        let outside = dg::SUPER_CAN_ONLY_BE_REFERENCED_IN_MEMBERS_OF_DERIVED_CLASSES_OR_OBJECT_LITERAL_EXP_2660;
        // A parameter of the constructor itself (not of an arrow in it): TS2336.
        let mut in_params = false;
        let mut through_arrow = false;
        let mut cur = parents.parent(e.node);
        for _ in 0..MAX_STEPS {
            let Some(n) = cur else {
                return call_or(outside);
            };
            match *ast.node(n) {
                Node::Arrow { .. } if is_call => return call_or(outside),
                Node::Arrow { .. } => through_arrow = true,
                Node::Param { .. } if !through_arrow => in_params = true,
                Node::FunctionExpr { .. } => {
                    let method = parents.parent(n).filter(
                        |&m| matches!(*ast.node(m), Node::MethodDef { value, .. } if value == n),
                    );
                    let Some(m) = method else {
                        return call_or(outside);
                    };
                    let Node::MethodDef { kind, .. } = *ast.node(m) else {
                        return None;
                    };
                    let owner = parents.parent(m)?;
                    if matches!(ast.node(owner), Node::ObjectLit { .. }) {
                        return if is_call { call_or(outside) } else { None };
                    }
                    if kind == MethodKind::Constructor {
                        if in_params {
                            return Some(
                                dg::SUPER_CANNOT_BE_REFERENCED_IN_CONSTRUCTOR_ARGUMENTS_2336,
                            );
                        }
                        return (!self.has_extends(Loc::new(e.file, owner))).then_some(not_derived);
                    }
                    if is_call {
                        return call_or(outside);
                    }
                    return (!self.has_extends(Loc::new(e.file, owner))).then_some(not_derived);
                }
                Node::FunctionDecl { .. } => return call_or(outside),
                Node::PropertyDef { .. } | Node::StaticBlock { .. } => {
                    let owner = parents.parent(n)?;
                    if is_call {
                        return call_or(outside);
                    }
                    return (!self.has_extends(Loc::new(e.file, owner))).then_some(not_derived);
                }
                // Heritage clauses, computed names and decorators are left alone.
                Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::ComputedKey { .. }
                | Node::Decorator { .. } => {
                    return None;
                }
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    /// Whether `class` has an `extends` clause.
    fn has_extends(&self, class: Loc) -> bool {
        let ast = self.ast(class.file);
        let heritage = match *ast.node(class.node) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return false,
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

    /// The constructor function (`FunctionExpr`) of a derived class that is the `this`
    /// container of `e`.
    fn derived_constructor_of(&self, e: Loc) -> Option<Loc> {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut decorated = false;
        let mut cur = parents.parent(e.node);
        for _ in 0..MAX_STEPS {
            let n = cur?;
            match *ast.node(n) {
                Node::Decorator { .. } => decorated = true,
                Node::ClassDecl { .. } | Node::ClassExpr { .. } if decorated => decorated = false,
                Node::FunctionExpr { .. } => {
                    let m = parents.parent(n)?;
                    let Node::MethodDef {
                        kind: MethodKind::Constructor,
                        ..
                    } = *ast.node(m)
                    else {
                        return None;
                    };
                    let class = parents.parent(m)?;
                    return self
                        .extends_non_null(Loc::new(e.file, class))
                        .then_some(Loc::new(e.file, n));
                }
                Node::Arrow { .. }
                | Node::FunctionDecl { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::PropertyDef { .. }
                | Node::StaticBlock { .. } => return None,
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    /// Whether `class` is a class with an `extends` clause whose expression is not `null`.
    fn extends_non_null(&self, class: Loc) -> bool {
        let ast = self.ast(class.file);
        let heritage = match *ast.node(class.node) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return false,
        };
        ast.list(heritage).iter().any(|&h| match *ast.node(h) {
            Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } => ast.list(types).first().is_some_and(|&t| {
                let expr = match *ast.node(t) {
                    Node::ExprWithTypeArgs { expr, .. } => expr,
                    _ => t,
                };
                !matches!(
                    ast.node(crate::nodes::skip_parens(ast, expr)),
                    Node::NullLit { .. }
                )
            }),
            _ => false,
        })
    }

    /// Whether `class` has an `extends` clause.
    fn has_extends_clause(&self, class: Loc) -> bool {
        let ast = self.ast(class.file);
        let heritage = match *ast.node(class.node) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return false,
        };
        ast.list(heritage).iter().any(|&h| {
            matches!(
                *ast.node(h),
                Node::HeritageClause {
                    kind: ts_r_ast::HeritageKind::Extends,
                    ..
                }
            )
        })
    }

    /// The first `super(...)` call in source order in a constructor function, outside
    /// nested functions, arrows and classes.
    fn first_super_call(&self, ctor: Loc) -> Option<NodeId> {
        let ast = self.ast(ctor.file);
        let mut best: Option<(u32, NodeId)> = None;
        let mut stack: Vec<NodeId> = Vec::new();
        ast.for_each_child(ctor.node, |c| stack.push(c));
        let mut seen = 0usize;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > MAX_NODES {
                break;
            }
            match *ast.node(n) {
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::Param { .. } => continue,
                Node::Call { callee, .. } if matches!(ast.node(callee), Node::Super { .. }) => {
                    let start = ast.span(n).start;
                    if best.is_none_or(|(b, _)| start < b) {
                        best = Some((start, n));
                    }
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        best.map(|(_, n)| n)
    }

    /// The `super(...)` calls of a constructor function (not those in nested functions,
    /// arrows or classes).
    pub(crate) fn super_calls(&self, ctor: Loc) -> SuperCalls {
        let ast = self.ast(ctor.file);
        let parents = self.c.file(ctor.file).parents();
        let mut result = SuperCalls::None;
        let mut stack: Vec<NodeId> = Vec::new();
        ast.for_each_child(ctor.node, |c| stack.push(c));
        let mut seen = 0usize;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > MAX_NODES {
                break;
            }
            match *ast.node(n) {
                // (A call in a parameter is TS2336.)
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::Param { .. } => continue,
                Node::Call { callee, .. } if matches!(ast.node(callee), Node::Super { .. }) => {
                    let statement = parents
                        .parent(n)
                        .is_some_and(|p| matches!(ast.node(p), Node::ExprStmt { .. }));
                    if !statement {
                        return SuperCalls::Nested;
                    }
                    result = SuperCalls::Statements;
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        result
    }

    /// Whether some path from the start of the function to flow node `start` does not
    /// pass a `super(...)` call statement.
    fn super_maybe_uncalled(&self, file: ts_r_diagnostics::FileId, start: FlowId) -> bool {
        let ast = self.ast(file);
        let graph = &self.c.bound(file).flow;
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![start];
        while let Some(f) = stack.pop() {
            if f.is_unreachable() || !seen.insert(f) {
                continue;
            }
            if seen.len() > MAX_FLOW {
                return false;
            }
            let Some(n) = graph.nodes.get(f.index()).copied() else {
                continue;
            };
            match n.kind {
                FlowKind::Unreachable => {}
                FlowKind::Start => return true,
                FlowKind::BranchLabel | FlowKind::LoopLabel => {
                    stack.extend_from_slice(graph.antecedents(f));
                }
                FlowKind::Call => {
                    let is_super_call = n.node.is_some_and(|c| {
                        matches!(*ast.node(c), Node::Call { callee, .. }
                            if matches!(ast.node(callee), Node::Super { .. }))
                    });
                    if !is_super_call {
                        stack.push(FlowId(n.a));
                    }
                }
                _ => stack.push(FlowId(n.a)),
            }
        }
        false
    }
}
