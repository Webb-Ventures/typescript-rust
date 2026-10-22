//! Functions tested for truthiness without being called (TS2774), promises tested without
//! being awaited (TS2801), and enum members tested for truthiness (TS2845).
//!
//! Rules observed from tsc 7.0.2 (under `strictNullChecks`):
//! - A tested operand is an identifier or a property access chain (`f`, `o.a.b`, `this.m`;
//!   not `o["m"]`, `f!`, `f as any`, `!f`), possibly parenthesized, that is a leaf of
//!   `&&`/`||`/`??` operators and either sits (through those operators) in the left
//!   operand of a `&&`, or the whole operator tree is the test of an `if` or of a `?:`.
//!   `while`, `do`, `for` tests and a lone `b && f` right operand are not tested.
//! - Its type has call signatures and no `null`/`undefined` (every union member).
//! - It is reported unless the same reference (same symbol, or the same property chain on
//!   the same root) appears in its region: the right operands of the `&&`s it is on the
//!   left of, and the `if` body or the `?:` true branch when the tree is their test (the
//!   `else` branch, the right of `||`/`??` and code after the statement do not count).
//!   Uses in nested functions count; a shadowing declaration does not.
//! - A tested promise (every union member has a `then` taking a callback) reports TS2801
//!   with "Did you forget to use 'await'?" under the same rules, except that a tested
//!   call or `new` (no reference) is always reported, even when repeated in the region.
//! - A tested `E.A` whose object names an enum and whose type is an enum literal reports
//!   TS2845 (always true/false by its value) instead.

use ts_r_ast::{BinaryOp, Node, NodeId};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_diagnostics::FileId;
use ts_r_types::{TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Nodes visited when searching a region for a use.
const MAX_REGION_NODES: u32 = 200_000;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks `n` (an identifier or property access) as a tested function (TS2774).
    pub(crate) fn check_uncalled_function_test(&mut self, file: FileId, n: NodeId) {
        if !self.c.opts.strict_null_checks || self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        if self.check_enum_member_test(file, n) {
            return;
        }
        let chain = self.is_reference_chain(file, n);
        if !chain && !matches!(ast.node(n), Node::Call { .. } | Node::New { .. }) {
            return;
        }
        let Some(region) = self.tested_region(file, n) else {
            return;
        };
        let t = self.quietly(|ck| ck.check_expr(Loc::new(file, n)));
        let function = self.is_always_defined_function(t);
        let promise = !function && self.is_always_defined_promise(t);
        let tested = promise || (chain && function);
        if !tested {
            return;
        }
        // A tested promise that is no reference (`if (f())`) is always reported.
        if promise && !chain {
            self.report_tested_promise(Loc::new(file, n), t);
            return;
        }
        let mut stack = region;
        let mut seen = 0u32;
        while let Some(r) = stack.pop() {
            seen += 1;
            if seen > MAX_REGION_NODES {
                return;
            }
            if r != n && self.same_reference(file, n, r) {
                return;
            }
            ast.for_each_child(r, |c| stack.push(c));
        }
        if promise {
            self.report_tested_promise(Loc::new(file, n), t);
            return;
        }
        self.error(
            Loc::new(file, n),
            dg::THIS_CONDITION_WILL_ALWAYS_RETURN_TRUE_SINCE_THIS_FUNCTION_IS_ALWAYS_DEFINED_DID_2774,
            &[],
        );
    }

    /// TS2801 for a tested promise `l` of type `t`, with "Did you forget to use 'await'?".
    fn report_tested_promise(&mut self, l: Loc, t: TypeId) {
        let shown = self.type_to_string(t);
        let mark = self.diags.len();
        self.error(
            l,
            dg::THIS_CONDITION_WILL_ALWAYS_RETURN_TRUE_SINCE_THIS_0_IS_ALWAYS_DEFINED_2801,
            &[&shown],
        );
        self.relate_last(mark, l, dg::DID_YOU_FORGET_TO_USE_AWAIT_2773, &[]);
    }

    /// A promise that is never `null` or `undefined`: every union member is an object whose
    /// `then` takes an `onfulfilled` callback (tsc: getPromisedTypeOfPromise).
    fn is_always_defined_promise(&mut self, t: TypeId) -> bool {
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return false;
        }
        let members = self.union_members(t);
        !members.is_empty()
            && members.iter().all(|&m| {
                self.store()
                    .flags(m)
                    .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                    && self.has_then_callback(m)
            })
    }

    /// `m.then` is callable with a first parameter that is itself callable.
    fn has_then_callback(&mut self, m: TypeId) -> bool {
        let Some(then) = self.quietly(|ck| ck.property_of_type(m, "then")) else {
            return false;
        };
        let tt = self.type_of_symbol(then);
        let sigs = self.signatures_of(tt, crate::members::SigKind::Call);
        sigs.into_iter().any(|sig| {
            let Some(p) = self.store().sig(sig).params.first().map(|p| p.ty) else {
                return false;
            };
            let p = self.non_nullable(p);
            !self.store().flags(p).intersects(TypeFlags::ANY)
                && !self
                    .signatures_of(p, crate::members::SigKind::Call)
                    .is_empty()
        })
    }

    /// A tested `E.A` (property access on an expression naming an enum, not an alias of one)
    /// whose type is an enum literal: TS2845 "always true/false" by the member's value.
    /// Returns whether it reported (tsc: checker/checker.go, checkTestingKnownTruthyType).
    fn check_enum_member_test(&mut self, file: FileId, n: NodeId) -> bool {
        let ast = self.ast(file);
        let Node::Member { obj, prop, .. } = *ast.node(n) else {
            return false;
        };
        if !matches!(ast.node(prop), Node::Ident { .. }) || self.tested_region(file, n).is_none() {
            return false;
        }
        let obj_sym = match *ast.node(obj) {
            Node::Ident { .. } => self.c.resolve_ident(Loc::new(file, obj), meaning::VALUE),
            Node::Member { .. } => self.property_symbol_of_access(Loc::new(file, obj)),
            _ => None,
        };
        if !obj_sym.is_some_and(|s| self.c.sym_flags(s).intersects(SymbolFlags::ENUM)) {
            return false;
        }
        let t = self.quietly(|ck| ck.check_expr(Loc::new(file, n)));
        if !self.store().flags(t).contains(TypeFlags::ENUM_LITERAL) {
            return false;
        }
        let truthy = match (self.store().number_value(t), self.store().string_value(t)) {
            (Some(v), _) => v != 0.0 && !v.is_nan(),
            (None, Some(v)) => !v.is_empty(),
            (None, None) => return false,
        };
        self.error(
            Loc::new(file, n),
            dg::THIS_CONDITION_WILL_ALWAYS_RETURN_0_2845,
            &[if truthy { "true" } else { "false" }],
        );
        true
    }

    /// `f`, `this.m` or `o.a.b` (a property access chain on an identifier or `this`), and
    /// `n` itself is not the property name of an access.
    fn is_reference_chain(&self, file: FileId, n: NodeId) -> bool {
        let ast = self.ast(file);
        let parents = self.c.file(file).parents();
        if let Some(p) = parents.parent(n)
            && let Node::Member { prop, .. } = *ast.node(p)
            && prop == n
        {
            return false;
        }
        let mut cur = n;
        for _ in 0..64 {
            match *ast.node(cur) {
                Node::Ident { .. } => return true,
                Node::This {} => return cur != n,
                Node::Member {
                    obj,
                    optional: false,
                    prop,
                } if matches!(ast.node(prop), Node::Ident { .. }) => cur = obj,
                _ => return false,
            }
        }
        false
    }

    /// The region searched for uses of tested operand `n`, or `None` when `n` is not
    /// tested.
    fn tested_region(&self, file: FileId, n: NodeId) -> Option<Vec<NodeId>> {
        let ast = self.ast(file);
        let parents = self.c.file(file).parents();
        let mut region = Vec::new();
        let mut tested = false;
        let mut cur = n;
        for _ in 0..4096 {
            let p = parents.parent(cur)?;
            match *ast.node(p) {
                Node::Paren { .. } => {}
                Node::Binary {
                    left,
                    op: BinaryOp::LogicalAnd,
                    right,
                } => {
                    if left == cur {
                        tested = true;
                        region.push(right);
                    }
                }
                Node::Binary {
                    op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
                    ..
                } => {}
                Node::If { test, cons, .. } if test == cur => {
                    region.push(cons);
                    return Some(region);
                }
                Node::Conditional { test, cons, .. } if test == cur => {
                    region.push(cons);
                    return Some(region);
                }
                _ => return tested.then_some(region),
            }
            cur = p;
        }
        None
    }

    /// A function type that is never `null` or `undefined`.
    fn is_always_defined_function(&mut self, t: TypeId) -> bool {
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return false;
        }
        let members = self.union_members(t);
        !members.is_empty()
            && members.iter().all(|&m| {
                let f = self.store().flags(m);
                f.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                    && !self
                        .signatures_of(m, crate::members::SigKind::Call)
                        .is_empty()
            })
    }

    /// `r` is the same reference as tested operand `n`.
    fn same_reference(&self, file: FileId, n: NodeId, r: NodeId) -> bool {
        let ast = self.ast(file);
        match (*ast.node(n), *ast.node(r)) {
            (Node::Ident { name: a }, Node::Ident { name: b }) => {
                if ast.atom(a) != ast.atom(b) || !self.is_value_reference(file, r) {
                    return false;
                }
                let sa = self.c.resolve_ident(Loc::new(file, n), meaning::VALUE);
                let sb = self.c.resolve_ident(Loc::new(file, r), meaning::VALUE);
                // `globalThis` (not declared anywhere) is one reference everywhere:
                // `if (globalThis.f) globalThis.f()` is a use.
                (sa.is_some() && sa == sb)
                    || (sa.is_none() && sb.is_none() && ast.atom(a) == "globalThis")
            }
            (Node::This {}, Node::This {}) => true,
            (
                Node::Member {
                    obj: oa, prop: pa, ..
                },
                Node::Member {
                    obj: ob, prop: pb, ..
                },
            ) => {
                matches!((*ast.node(pa), *ast.node(pb)),
                    (Node::Ident { name: x }, Node::Ident { name: y }) if ast.atom(x) == ast.atom(y))
                    && self.same_reference(file, oa, ob)
            }
            _ => false,
        }
    }

    /// Identifier `r` reads or writes a variable (not a property name or a declared name).
    fn is_value_reference(&self, file: FileId, r: NodeId) -> bool {
        let ast = self.ast(file);
        let Some(p) = self.c.file(file).parents().parent(r) else {
            return true;
        };
        !match *ast.node(p) {
            Node::Member { prop, .. } => prop == r,
            Node::Property { key, .. } | Node::MethodDef { key, .. } => key == r,
            Node::PropertyDef { key, .. } | Node::PropertySignature { key, .. } => key == r,
            Node::VarDeclarator { name, .. } => name == r,
            Node::Param { pattern, .. } => pattern == r,
            Node::FunctionDecl { name, .. } | Node::ClassDecl { name, .. } => name == Some(r),
            _ => false,
        }
    }
}
