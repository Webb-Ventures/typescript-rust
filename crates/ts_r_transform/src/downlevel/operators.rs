//! Operator lowerings (observed, tsc 7.0.2):
//!
//! - ES2021 logical assignment: `a ||= b` → `a || (a = b)`; a non-simple object or key of the
//!   target is captured: `(_a = a.b).c || (_a.c = 1)`, `a[_a = k.j] ?? (a[_a] = c)` (then
//!   lowered as `??` below ES2020).
//! - ES2021 numeric separators: the literal is printed by value (`0xFF_FF` → `65535`).
//! - ES2019 optional `catch` binding: `catch (_a)` (a temporary named in print order).
//! - ES2016 `**`: `Math.pow(a, b)`; `x **= y` → `x = Math.pow(x, y)`, the object and key of a
//!   property target always captured: `(_a = a)[_b = k] = Math.pow(_a[_b], 2)`.

use ts_r_ast::{AssignOp, BinaryOp, Node, NodeId};

use super::lower::{Group, Lower};
use crate::builder::Build;

impl Lower<'_, '_> {
    /// `a ||= b`, `a &&= b`, `a ??= b`.
    pub fn logical_assign(&mut self, id: NodeId) {
        let Node::Assign { left, op, right } = self.n(id) else {
            return;
        };
        let at = self.at(id);
        // Parentheses around either side are dropped (observed: `(a.b) &&= (c ||= d)` →
        // `a.b && (a.b = c || (c = d))`).
        let left = self.skip_parens(left);
        let right = self.skip_parens(right);
        let (read, write) = self.split_target(left, false, Group::Es2021);
        self.visit(right);
        let write_assign = self.assign(write, right, at);
        let paren = self.paren(write_assign, at);
        let bop = match op {
            AssignOp::OrAssign => BinaryOp::LogicalOr,
            AssignOp::AndAssign => BinaryOp::LogicalAnd,
            _ => BinaryOp::Coalesce,
        };
        let r = if bop == BinaryOp::Coalesce && self.below(ts_r_config::ScriptTarget::ES2020) {
            self.build_coalesce(id, read, paren, false)
        } else {
            self.binary(read, bop, paren, at)
        };
        self.put(id, r);
    }

    /// `x **= y`
    pub fn exponent_assign(&mut self, id: NodeId) {
        let Node::Assign { left, right, .. } = self.n(id) else {
            return;
        };
        let at = self.at(id);
        // The value first: in `a[i] **= a[j] **= 2` the inner target's temporaries come
        // first (observed).
        self.visit(right);
        let (write, read) = self.split_target(left, true, Group::Es2016);
        let pow = self.math_pow(read, right, at);
        let r = self.assign(write, pow, at);
        self.put(id, r);
    }

    /// `a ** b`
    pub fn exponent(&mut self, id: NodeId) {
        let Node::Binary { left, right, .. } = self.n(id) else {
            return;
        };
        self.visit(left);
        self.visit(right);
        let at = self.at(id);
        let r = self.math_pow(left, right, at);
        self.put(id, r);
    }

    pub(super) fn math_pow(&mut self, a: NodeId, b: NodeId, at: u32) -> NodeId {
        let math = self.ident("Math", at);
        let pow = self.member(math, "pow", at);
        self.call(pow, &[a, b], at)
    }

    /// The first and second use of an assignment target, its parts visited: the first
    /// captures the object and key (`(_a = o)[_b = k]`), the second reads the temporaries
    /// (`_a[_b]`). `always` captures simple parts too (ES2016); otherwise only non-simple ones.
    fn split_target(&mut self, left: NodeId, always: bool, group: Group) -> (NodeId, NodeId) {
        match self.n(left) {
            Node::Member { obj, prop, .. } => {
                self.visit(obj);
                let (o1, o2) = self.capture_part(obj, always, group);
                let at = self.at(left);
                let p2 = self.copy_prop(prop);
                self.tx.set(
                    left,
                    Node::Member {
                        obj: o1,
                        optional: false,
                        prop,
                    },
                );
                let second = self.mk(
                    Node::Member {
                        obj: o2,
                        optional: false,
                        prop: p2,
                    },
                    at,
                );
                (left, second)
            }
            Node::Index { obj, index, .. } => {
                self.visit(obj);
                let (o1, o2) = self.capture_part(obj, always, group);
                self.visit(index);
                let (k1, k2) = self.capture_part(index, always, group);
                let at = self.at(left);
                self.tx.set(
                    left,
                    Node::Index {
                        obj: o1,
                        optional: false,
                        index: k1,
                    },
                );
                let second = self.mk(
                    Node::Index {
                        obj: o2,
                        optional: false,
                        index: k2,
                    },
                    at,
                );
                (left, second)
            }
            _ => {
                self.visit(left);
                let c = self.copy(left);
                (left, c)
            }
        }
    }

    fn capture_part(&mut self, e: NodeId, always: bool, group: Group) -> (NodeId, NodeId) {
        if always && !matches!(self.n(e), Node::Super {}) {
            self.capture(e, group)
        } else {
            self.capture_unless_simple(e, group)
        }
    }

    fn copy_prop(&mut self, prop: NodeId) -> NodeId {
        let at = self.at(prop);
        match self.n(prop) {
            Node::Ident { name } => self.ident_atom(name, at),
            Node::PrivateName { name } => self.mk(Node::PrivateName { name }, at),
            _ => crate::builder::clone_node(self.tx.ast, prop),
        }
    }

    /// `catch { }` → `catch (_a) { }`
    pub fn optional_catch(&mut self, id: NodeId) {
        let Node::CatchClause { type_ann, body, .. } = self.n(id) else {
            return;
        };
        let t = self.local_temp();
        let at = self.at(id);
        let param = self.ident_atom(t, at);
        self.tx.set(
            id,
            Node::CatchClause {
                param: Some(param),
                type_ann,
                body,
            },
        );
        self.visit(body);
    }

    /// `1_000` → `1000` (printed from its value).
    pub fn numeric_separators(&mut self, id: NodeId) {
        let s = self.tx.span(id);
        let raw = self
            .tx
            .src
            .get(s.start as usize..s.end as usize)
            .unwrap_or("");
        if !raw.contains('_') {
            return;
        }
        let at = s.start;
        let n = match self.n(id) {
            Node::NumberLit { value } => Node::NumberLit { value },
            Node::BigIntLit { .. } => {
                let text: String = raw
                    .trim_end_matches('n')
                    .chars()
                    .filter(|&c| c != '_')
                    .collect();
                let value = self.tx.ast.intern(&text);
                Node::BigIntLit { value }
            }
            _ => return,
        };
        let new = self.mk(n, at);
        self.put(id, new);
    }
}
