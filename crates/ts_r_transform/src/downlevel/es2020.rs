//! ES2020: optional chains and `??` (observed, tsc 7.0.2).
//!
//! - `a?.b.c` → `a === null || a === void 0 ? void 0 : a.b.c`; a non-simple object is
//!   captured: `(_a = a.b) === null || _a === void 0 ? void 0 : _a.c`. Simple means an
//!   identifier, `this` or a literal.
//! - Each further `?.` captures everything before it, conditionals included:
//!   `(_a = a === null || a === void 0 ? void 0 : a.c) === null || _a === void 0 ? void 0 : _a.d`.
//! - An optional call keeps its `this`: `a.b?.()` → `... : _a.call(a)`, capturing a
//!   non-simple object first (`(_b = (_a = x.y).b) === ... : _b.call(_a)`); `super.m?.()`
//!   passes `this`.
//! - A parenthesized chain called directly does too: `(a?.b)()` →
//!   `(a === null || a === void 0 ? void 0 : a.b).call(a)`.
//! - `delete a?.b` → `a === null || a === void 0 ? true : delete a.b`.
//! - `a ?? b` → `a !== null && a !== void 0 ? a : b`, capturing a non-simple left side.
//!
//! Temporaries of chains and of `??` go to two separate `var` statements, chains first.

use ts_r_ast::{BinaryOp, Node, NodeId, NodeKind, NodeList, UnaryOp};

use super::lower::{Group, Lower};
use crate::builder::{Build, clone_node};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Value,
    Delete,
    /// The chain is the parenthesized callee of a call: also return the `this` to pass.
    ThisCall,
}

#[derive(Clone, Copy)]
struct Link {
    node: NodeId,
    optional: bool,
}

impl Lower<'_, '_> {
    /// True if `id` is a member access or call chain (not crossing parentheses) with a `?.`.
    pub fn chain_has_optional(&mut self, mut id: NodeId) -> bool {
        let mut links = Vec::new();
        loop {
            if self.ordinary_chains.contains(&id) {
                break;
            }
            match self.n(id) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional {
                        return true;
                    }
                    links.push(id);
                    id = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    if optional {
                        return true;
                    }
                    links.push(id);
                    id = callee;
                }
                _ => break,
            }
        }
        self.ordinary_chains.extend(links);
        false
    }

    pub fn optional_chain(&mut self, id: NodeId) {
        let span = self.tx.span(id);
        let (r, _) = self.lower_chain(id, Mode::Value);
        self.put(id, r);
        self.map_whole(id, span);
    }

    /// Source maps (tsc's): the lowered expression in slot `id` maps to the source `span`.
    fn map_whole(&mut self, id: NodeId, span: ts_r_ast::Span) {
        if span.start < span.end {
            self.tx.map_range(id, span);
        }
    }

    pub fn optional_delete(&mut self, id: NodeId, arg: NodeId) {
        let arg = self.skip_parens(arg);
        let (r, _) = self.lower_chain(arg, Mode::Delete);
        self.put(id, r);
    }

    /// `(a?.b)` as a callee: the chain ends in a property access.
    pub fn paren_chain_callee(&mut self, callee: NodeId) -> bool {
        let Node::Paren { expr } = self.n(callee) else {
            return false;
        };
        matches!(self.tx.kind(expr), NodeKind::Member | NodeKind::Index)
            && self.chain_has_optional(expr)
    }

    /// `(a?.b)(x)` → `(a === null || a === void 0 ? void 0 : a.b).call(a, x)`.
    pub fn paren_chain_call(&mut self, id: NodeId, callee: NodeId, args: NodeList) {
        let Node::Paren { expr } = self.n(callee) else {
            return;
        };
        let (r, this_arg) = self.lower_chain(expr, Mode::ThisCall);
        self.put(expr, r);
        self.visit_list(args);
        let at = self.at(id);
        let moved = clone_node(self.tx.ast, callee);
        let call = self.member(moved, "call", at);
        let mut items = Vec::with_capacity(self.tx.ast.list(args).len() + 1);
        if let Some(t) = this_arg {
            items.push(t);
        }
        items.extend(self.tx.items(args));
        let args = self.new_list(&items);
        self.tx.set(
            id,
            Node::Call {
                callee: call,
                optional: false,
                type_args: None,
                args,
            },
        );
    }

    /// Lowers the chain rooted at `id` into a new node (the slot `id` is left for the caller).
    fn lower_chain(&mut self, id: NodeId, mode: Mode) -> (NodeId, Option<NodeId>) {
        let mut links = Vec::new();
        let mut cur = id;
        loop {
            match self.n(cur) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    links.push(Link {
                        node: cur,
                        optional,
                    });
                    cur = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    links.push(Link {
                        node: cur,
                        optional,
                    });
                    cur = callee;
                }
                _ => break,
            }
        }
        links.reverse();
        let at = self.at(id);
        // `super.x?.y` in a lowered async method: the first link becomes `_super.x` (or
        // `_superIndex(k).value`), and a call on it passes `this` (observed).
        let mut super_base = None;
        if self.n(cur) == (Node::Super {})
            && self.super_ctx().is_some()
            && links.first().is_some_and(|l| {
                !l.optional && matches!(self.tx.kind(l.node), NodeKind::Member | NodeKind::Index)
            })
        {
            let first = links.remove(0);
            self.super_access(first.node);
            cur = first.node;
            super_base = Some(first.node);
        } else {
            // `(super.f)?.()`: the parenthesized access becomes `(_super.f)` too, and a call
            // on it still passes `this`, not the `_super` object.
            let inner = self.skip_parens(cur);
            let paren_super = inner != cur
                && self.super_ctx().is_some()
                && matches!(self.n(inner), Node::Member { obj, .. } | Node::Index { obj, .. }
                    if self.n(obj) == Node::Super {});
            self.visit(cur);
            if paren_super {
                super_base = Some(self.skip_parens(cur));
            }
        }
        let mut open: Option<NodeId> = None;
        let last = links.len().saturating_sub(1);
        let mut this_arg = None;
        for (i, link) in links.iter().enumerate() {
            let mut call_this = None;
            let on_super = super_base.is_some_and(|b| self.skip_parens(cur) == b);
            if on_super && self.tx.kind(link.node) == NodeKind::Call {
                call_this = Some(self.this(at));
            }
            if link.optional {
                if self.tx.kind(link.node) == NodeKind::Call && !on_super {
                    call_this = self.this_of(cur);
                }
                if let Some(test) = open.take() {
                    let v = self.void0(at);
                    cur = self.mk(
                        Node::Conditional {
                            test,
                            cons: v,
                            alt: cur,
                        },
                        at,
                    );
                }
                let (first, r) = self.capture_unless_simple(cur, Group::Chain);
                let r2 = self.copy(r);
                let test = self.null_or_undefined(first, r2, at);
                self.map_capture(test, cur);
                open = Some(test);
                cur = r;
            }
            cur = self.apply_link(*link, cur, call_this);
            if mode == Mode::ThisCall && i == last {
                this_arg = self.this_of(cur);
            }
        }
        if let Some(test) = open {
            let (cons, alt) = if mode == Mode::Delete {
                let t = self.bool_lit(true, at);
                let d = self.unary(UnaryOp::Delete, cur, at);
                (t, d)
            } else {
                (self.void0(at), cur)
            };
            cur = self.mk(Node::Conditional { test, cons, alt }, at);
        }
        (cur, this_arg)
    }

    /// The `this` of a call on `built` = `obj.x` / `obj[x]`, capturing a non-simple `obj` in
    /// place (`super.x` passes `this`).
    fn this_of(&mut self, built: NodeId) -> Option<NodeId> {
        // Through parentheses kept for comments (`(/*c*/ foo.m as any)?.()`).
        let built = self.skip_parens(built);
        match self.n(built) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                if self.tx.kind(obj) == NodeKind::Super {
                    let at = self.at(obj);
                    return Some(self.this(at));
                }
                if self.is_simple(obj) {
                    Some(self.copy(obj))
                } else {
                    let (a, r) = self.capture(obj, Group::Chain);
                    self.set_object(built, a);
                    Some(r)
                }
            }
            _ => None,
        }
    }

    fn set_object(&mut self, id: NodeId, obj: NodeId) {
        match self.n(id) {
            Node::Member { optional, prop, .. } => self.tx.set(
                id,
                Node::Member {
                    obj,
                    optional,
                    prop,
                },
            ),
            Node::Index {
                optional, index, ..
            } => self.tx.set(
                id,
                Node::Index {
                    obj,
                    optional,
                    index,
                },
            ),
            _ => {}
        }
    }

    /// A copy of `link` applied to `cur`, without `?.`; a call passes `this_arg` with
    /// `.call(this_arg, ...)`.
    fn apply_link(&mut self, link: Link, cur: NodeId, this_arg: Option<NodeId>) -> NodeId {
        let node = clone_node(self.tx.ast, link.node);
        // Source maps (tsc's): the rebuilt access or call maps its end only.
        let span = self.tx.span(link.node);
        if span.start < span.end {
            self.tx
                .map_range(node, ts_r_ast::Span::new(ts_r_codegen::NO_MAP, span.end));
        }
        match self.n(node) {
            Node::Member { prop, .. } => {
                self.tx.set(
                    node,
                    Node::Member {
                        obj: cur,
                        optional: false,
                        prop,
                    },
                );
            }
            Node::Index { index, .. } => {
                self.visit(index);
                self.tx.set(
                    node,
                    Node::Index {
                        obj: cur,
                        optional: false,
                        index,
                    },
                );
            }
            Node::Call {
                type_args, args, ..
            } => {
                self.visit_list(args);
                let (callee, args) = match this_arg {
                    Some(t) => {
                        let at = self.at(link.node);
                        let c = self.member(cur, "call", at);
                        let mut items = vec![t];
                        items.extend(self.tx.items(args));
                        (c, self.new_list(&items))
                    }
                    None => (cur, args),
                };
                self.tx.set(
                    node,
                    Node::Call {
                        callee,
                        optional: false,
                        type_args,
                        args,
                    },
                );
            }
            _ => {}
        }
        node
    }

    /// Source maps (tsc's): the test `(_a = e) === null || ...` of an optional link maps
    /// its start to `e`'s.
    fn map_capture(&mut self, test: NodeId, e: NodeId) {
        let span = self.tx.span(e);
        if span.start < span.end {
            self.tx
                .map_range(test, ts_r_ast::Span::new(span.start, ts_r_codegen::NO_MAP));
        }
    }

    /// `first === null || r === void 0`
    fn null_or_undefined(&mut self, first: NodeId, r: NodeId, at: u32) -> NodeId {
        let null = self.null(at);
        let a = self.binary(first, BinaryOp::StrictEq, null, at);
        let v = self.void0(at);
        let b = self.binary(r, BinaryOp::StrictEq, v, at);
        self.binary(a, BinaryOp::LogicalOr, b, at)
    }

    /// `a ?? b`
    pub fn coalesce(&mut self, id: NodeId) {
        let Node::Binary { left, right, .. } = self.n(id) else {
            return;
        };
        let span = self.tx.span(id);
        self.visit(left);
        let r = self.build_coalesce(id, left, right, true);
        self.put(id, r);
        self.map_whole(id, span);
    }

    /// `left !== null && left !== void 0 ? left : right` with a captured non-simple `left`
    /// (visited already); `right` is visited when `visit_right`.
    pub fn build_coalesce(
        &mut self,
        id: NodeId,
        left: NodeId,
        right: NodeId,
        visit_right: bool,
    ) -> NodeId {
        let at = self.at(id);
        let (first, r) = self.capture_unless_simple(left, Group::Coalesce);
        if visit_right {
            self.visit(right);
        }
        let null = self.null(at);
        let a = self.binary(first, BinaryOp::StrictNotEq, null, at);
        let r2 = self.copy(r);
        let v = self.void0(at);
        let b = self.binary(r2, BinaryOp::StrictNotEq, v, at);
        let test = self.binary(a, BinaryOp::LogicalAnd, b, at);
        self.mk(
            Node::Conditional {
                test,
                cons: r,
                alt: right,
            },
            at,
        )
    }
}
