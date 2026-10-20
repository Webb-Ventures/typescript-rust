//! `yield` in parameter initializers (TS2523) and `yield` results that are implicitly
//! `any` (TS7057).
//!
//! TS2523 (tsc 7.0.2): any `yield` inside a parameter of its nearest function (a function
//! nested in the initializer is a new scope; a parameter decorator is outside), generator
//! or not, after any TS1163. `await` follows the same rule (TS2524, after any TS1308).
//!
//! TS7057, observed under `noImplicitAny`:
//! - The `yield` does not delegate and its result is used. A result is unused as an
//!   expression statement, a `void` operand, a `for` initializer or update, or a non-final
//!   comma operand; parentheses and final comma operands pass this on.
//! - Its nearest function (classes in between do not count, so a `yield` in a property
//!   initializer is TS1163 and TS7057; a method's computed name counts as inside the
//!   method) is a generator with neither a return-type
//!   annotation nor a contextual signature.
//! - Its own contextual type is missing, `any`, or the type parameter of a called
//!   signature (`f(yield)` with `f<T>(x: T)`; a written `const a: T = yield` is fine).
//!   `import(yield x)`, `@(yield x)` and destructuring (`const [a] = yield`,
//!   `[a] = yield`) are fine.

use ts_r_ast::{FnFlags, Node, UnaryOp};
use ts_r_types::TypeFlags;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::fn_parts;

/// Parent steps the "is the result used" walk follows.
const MAX_STEPS: u32 = 256;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the placement and result of `yield` expression `e` (after TS1163).
    pub(crate) fn check_yield_expression(&mut self, e: Loc, delegate: bool) {
        if self.reporting(e.file) && self.in_parameter_initializer_of_container(e) {
            self.error(
                e,
                dg::YIELD_EXPRESSIONS_CANNOT_BE_USED_IN_A_PARAMETER_INITIALIZER_2523,
                &[],
            );
        }
        if !delegate {
            self.check_implicit_any_yield(e);
        }
    }

    /// Whether `e` is inside a parameter of its nearest function (TS2523, and TS2524 for
    /// `await`).
    pub(crate) fn in_parameter_initializer_of_container(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::Param { .. } => return true,
                // A parameter decorator is not part of the initializer.
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::Decorator { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// Checks non-delegating `yield` expression `e`.
    fn check_implicit_any_yield(&mut self, e: Loc) {
        if !self.c.opts.no_implicit_any || !self.reporting(e.file) || !self.yield_result_used(e) {
            return;
        }
        let Some(func) = self.yield_container(e) else {
            return;
        };
        let Some(parts) = fn_parts(self.ast(e.file), func.node) else {
            return;
        };
        if !parts.flags.contains(FnFlags::GENERATOR)
            || parts.return_type.is_some()
            || self.quietly(|ck| ck.contextual_signature(func)).is_some()
        {
            return;
        }
        // The argument of `import()` is a module specifier, a decorator is called. A type parameter counts only as
        // the parameter type of a call being inferred, not when written (`const a: T`).
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut outer = e.node;
        while let Some(p) = parents.parent(outer)
            && matches!(ast.node(p), Node::Paren { .. })
        {
            outer = p;
        }
        let in_call = match parents.parent(outer).map(|p| ast.node(p)) {
            Some(Node::ImportCall { .. } | Node::Decorator { .. }) => return,
            Some(Node::Call { args, .. } | Node::New { args, .. }) => {
                ast.list(*args).contains(&outer)
            }
            // A destructuring target types its source.
            Some(
                Node::VarDeclarator { name: target, .. }
                | Node::Param {
                    pattern: target, ..
                }
                | Node::Assign { left: target, .. },
            ) if matches!(
                ast.node(*target),
                Node::ObjectPattern { .. }
                    | Node::ArrayPattern { .. }
                    | Node::ObjectLit { .. }
                    | Node::ArrayLit { .. }
            ) =>
            {
                return;
            }
            _ => false,
        };
        let ct = self.quietly(|ck| ck.contextual_type(e));
        let implicit = match ct {
            None => true,
            Some(t) => {
                let f = self.store().flags(t);
                f.intersects(TypeFlags::ANY) || in_call && f.intersects(TypeFlags::TYPE_PARAMETER)
            }
        };
        if implicit {
            self.error(
                e,
                dg::YIELD_EXPRESSION_IMPLICITLY_RESULTS_IN_AN_ANY_TYPE_BECAUSE_ITS_CONTAINING_GENERA_7057,
                &[],
            );
        }
    }

    /// The nearest function around `e`, looking through classes (a `yield` in a property
    /// initializer of a class inside a generator is TS1163 and also TS7057). A method's
    /// computed name counts as inside the method (`{ *[yield 0]() {} }` is TS7057,
    /// `{ [yield 0]() {} }` is not).
    fn yield_container(&self, e: Loc) -> Option<Loc> {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        let mut child = e.node;
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    return Some(Loc::new(e.file, n));
                }
                // The computed name of a method belongs to the method.
                Node::MethodDef { key, value, .. } if key == child => {
                    return Some(Loc::new(e.file, value));
                }
                _ => {}
            }
            child = n;
            cur = parents.parent(n);
        }
        None
    }

    /// Whether the value of expression `e` is used.
    fn yield_result_used(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut child = e.node;
        for _ in 0..MAX_STEPS {
            let Some(p) = parents.parent(child) else {
                return true;
            };
            match *ast.node(p) {
                Node::Paren { .. } => {}
                Node::Seq { exprs } => {
                    if ast.list(exprs).last() != Some(&child) {
                        return false;
                    }
                }
                Node::ExprStmt { .. } => return false,
                Node::Unary {
                    op: UnaryOp::Void, ..
                } => return false,
                Node::For { init, update, .. } => {
                    return init != Some(child) && update != Some(child);
                }
                _ => return true,
            }
            child = p;
        }
        true
    }
}
