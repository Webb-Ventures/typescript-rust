//! Size-oriented rewrites that never change behaviour.
//!
//! Expression rewrites: constant folding (via [`crate::fold::eval`]), `undefined` to
//! `void 0`, `true`/`false` to `!0`/`!1`, `a["b"]` to `a.b`, quoted identifier keys
//! unquoted, numbers reprinted in their shortest form, `!a ? b : c` to `a ? c : b`,
//! `x === null || x === void 0` to `x == null`, `x != null ? x : y` to `x ?? y`,
//! `typeof x == "undefined"` to `typeof x > "u"`, `a && (b && c)` to `a && b && c`, source
//! parentheses dropped (the printer adds the ones it needs; optional chains keep theirs).
//!
//! Statement-list rewrites: empty statements and pure expression statements dropped, code
//! after `return`/`throw`/`break`/`continue` dropped (keeping hoisted `var` names and
//! function declarations), blocks without lexical declarations flattened, consecutive
//! expression statements joined with `,` (and into a following `return`/`throw`/`if`/`for`),
//! consecutive declarations of the same kind merged (`var` also into a `for` head),
//! `if (a) b();` to `a && b();`, `if (a) b(); else c();` to `a ? b() : c();`,
//! `if (a) if (b) s` to `if (a && b) s`, `if (a) return x; return y` to
//! `return a ? x : y`, `return void 0` to `return`; a statement `x != null && x.f()` to
//! `x?.f()`. Joins repeat, so chains collapse.
//!
//! Function and loop bodies: a trailing `return;` / `continue;` dropped and
//! `if (a) return; rest` to `if (!a) { rest }` (usually `a || rest`). Unused catch bindings
//! dropped.
//!
//! Declaration and function rewrites: nested `const` to `let` when nothing assigns to the
//! binding (top-level `const` stays: V8 constant-folds it), `() => { return x; }` to
//! `() => x`, `typeof x === "y"` to `typeof x == "y"`.

use ts_r_ast::{Ast, BinaryOp, FnFlags, Modifiers, Node, NodeId, NodeList, Span, UnaryOp, VarKind};

use crate::build;
use crate::fold::{self, Const};
use crate::purity::Purity;
use crate::scope::{ScopeId, ScopeKind, Scopes, SymbolKind};

#[derive(Clone, Debug)]
pub struct CompressOptions {
    /// Rewrite `true`/`false` to `!0`/`!1`.
    pub booleans: bool,
    /// Join statements into sequences and conditionals.
    pub join: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        CompressOptions {
            booleans: true,
            join: true,
        }
    }
}

/// Compresses the whole tree in place. `scopes` must describe `ast` (identifier resolution
/// for `undefined` and purity).
pub fn compress(ast: &mut Ast, src: &str, scopes: &Scopes, opts: &CompressOptions) {
    let undefined_shadowed = {
        let a = ast.intern("undefined");
        scopes.declares_name(a)
    };
    let undef = ast.intern("undefined");
    let undefined_refs: rustc_hash::FxHashMap<u32, bool> = scopes
        .refs
        .iter()
        .filter(|r| {
            r.symbol.is_none() && matches!(*ast.node(r.node), Node::Ident { name } if name == undef)
        })
        .map(|r| (r.node.raw(), r.write))
        .collect();
    let mut c = Compressor {
        src,
        scopes,
        opts,
        undefined_shadowed,
        undefined_refs,
        depth: 0,
    };
    let root = ast.root;
    c.visit(ast, root);
}

struct Compressor<'a> {
    src: &'a str,
    scopes: &'a Scopes,
    opts: &'a CompressOptions,
    undefined_shadowed: bool,
    /// Unresolved `undefined` references and whether each is written.
    undefined_refs: rustc_hash::FxHashMap<u32, bool>,
    depth: u32,
}

const MAX_DEPTH: u32 = 10_000;

fn sp(at: u32) -> Span {
    Span::new(at, at)
}

impl Compressor<'_> {
    /// A `var` of the catch parameter `sym`'s name hoists past its catch clause: its
    /// initializer inside the clause assigns the parameter, a write no reference counts.
    fn var_shadows(&self, sym: crate::scope::SymbolId) -> bool {
        let s = self.scopes.symbol(sym);
        let mut scope = self.scopes.scope(s.scope).parent;
        while let Some(id) = scope {
            let sc = self.scopes.scope(id);
            if sc.kind.is_var_target() {
                return self.scopes.lookup_in(id, s.name).is_some();
            }
            scope = sc.parent;
        }
        false
    }

    fn pure(&self, ast: &Ast, id: NodeId) -> bool {
        Purity {
            ast,
            src: self.src,
            scopes: Some(self.scopes),
            annotations: true,
        }
        .expr(id)
    }

    fn visit(&mut self, ast: &mut Ast, id: NodeId) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        if let Node::TaggedTemplate { tag, quasi, .. } = *ast.node(id) {
            // The quasi must stay a template literal.
            self.visit(ast, tag);
            for k in ast.children(quasi) {
                self.visit(ast, k);
            }
        } else {
            let kids = ast.children(id);
            for k in kids {
                self.visit(ast, k);
            }
        }
        self.depth -= 1;
        self.strip_parens(ast, id);
        self.node(ast, id);
    }

    /// Drops source parentheses around the children of `id`; the printer adds back the ones
    /// precedence or statement starts need. Kept: optional chains (`(a?.b).c` ends the
    /// chain), a string expression statement (not a directive), `let`/`async` identifiers,
    /// and everything under `for in`/`for of` heads and decorators.
    fn strip_parens(&self, ast: &mut Ast, id: NodeId) {
        let mut any = false;
        ast.for_each_child(id, |c| any |= matches!(ast.node(c), Node::Paren { .. }));
        if !any {
            return;
        }
        let parent = *ast.node(id);
        if matches!(
            parent,
            Node::ForIn { .. } | Node::ForOf { .. } | Node::Decorator { .. }
        ) {
            return;
        }
        let stmt = matches!(parent, Node::ExprStmt { .. });
        let new = ast.map_children(id, &mut |ast, c| {
            let mut c = c;
            while let Node::Paren { expr } = *ast.node(c) {
                let keep = match *ast.node(expr) {
                    Node::StringLit { .. } => stmt,
                    Node::Ident { name } => matches!(ast.atom(name), "let" | "async"),
                    _ => has_optional_chain(ast, expr),
                };
                if keep {
                    break;
                }
                c = expr;
            }
            c
        });
        *ast.node_mut(id) = new;
    }

    fn node(&mut self, ast: &mut Ast, id: NodeId) {
        match *ast.node(id) {
            Node::Program { stmts } => {
                let new = self.stmt_list(ast, stmts, true);
                if let Node::Program { stmts } = ast.node_mut(id) {
                    *stmts = new;
                }
            }
            Node::Block { stmts } => {
                let new = self.stmt_list(ast, stmts, false);
                if let Node::Block { stmts } = ast.node_mut(id) {
                    *stmts = new;
                }
            }
            Node::SwitchCase { test, body } => {
                let new = self.stmt_list(ast, body, false);
                *ast.node_mut(id) = Node::SwitchCase { test, body: new };
            }
            Node::StaticBlock { body } => {
                let new = self.stmt_list(ast, body, false);
                *ast.node_mut(id) = Node::StaticBlock { body: new };
            }
            Node::If { test, cons, alt } => self.if_stmt(ast, id, test, cons, alt),
            Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::While { body, .. }
            | Node::DoWhile { body, .. } => {
                self.tail(ast, body, true);
                if let Some(s) = self.unwrap_block(ast, body) {
                    build::replace(ast, body, s);
                }
            }
            Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => {
                self.tail(ast, b, false)
            }
            Node::ExprStmt { expr } if self.opts.join => {
                let e = self.discarded(ast, expr);
                *ast.node_mut(id) = Node::ExprStmt { expr: e };
            }
            Node::CatchClause {
                param: Some(p),
                body,
                ..
            } if !self.scopes.has_direct_eval
                && matches!(ast.node(p), Node::Ident { .. })
                && self
                    .scopes
                    .symbol_of(p)
                    .is_some_and(|s| self.scopes.symbol(s).refs == 0 && !self.var_shadows(s)) =>
            {
                // Optional catch binding (ES2019).
                *ast.node_mut(id) = Node::CatchClause {
                    param: None,
                    type_ann: None,
                    body,
                };
            }
            Node::Binary {
                left,
                op: op @ (BinaryOp::LogicalOr | BinaryOp::LogicalAnd),
                right,
            } => {
                self.logical(ast, id, left, op, right);
                self.fold_expr(ast, id);
            }
            Node::Return { arg: Some(a) } => {
                if matches!(
                    fold::eval(ast, a, Some(self.scopes)),
                    Some(Const::Undefined)
                ) && self.pure(ast, a)
                {
                    *ast.node_mut(id) = Node::Return { arg: None };
                }
            }
            Node::Ident { name } => {
                if !self.undefined_shadowed
                    && ast.atom(name) == "undefined"
                    && self.scopes.symbol_of(id).is_none()
                    && self.is_value_position(ast, id)
                {
                    let at = ast.span(id).start;
                    let v = build::undefined(ast, at);
                    build::replace(ast, id, v);
                }
            }
            Node::BoolLit { value } if self.opts.booleans => {
                let at = ast.span(id).start;
                let n = build::number(ast, if value { 0.0 } else { 1.0 }, at);
                let not = build::unary(ast, UnaryOp::Not, n, at);
                build::replace(ast, id, not);
            }
            Node::NumberLit { value } => {
                let span = ast.span(id);
                if span.start != span.end {
                    let raw = span.text(self.src);
                    let short = ts_r_codegen::literals::js_number(value.get());
                    if value.get().is_finite() && short.len() < raw.len() && !raw.contains('n') {
                        ast.set_span(id, sp(span.start));
                    }
                }
            }
            Node::Index {
                obj,
                optional,
                index,
            } => {
                if let Node::StringLit { value } = *ast.node(index) {
                    let s = ast.atom(value).to_owned();
                    if ts_r_codegen::literals::is_identifier_name(&s) && s.is_ascii() {
                        let at = ast.span(index).start;
                        let prop = build::ident(ast, &s, at);
                        *ast.node_mut(id) = Node::Member {
                            obj,
                            optional,
                            prop,
                        };
                    }
                }
            }
            Node::Property {
                modifiers,
                key,
                value,
            } => {
                if let Node::StringLit { value: k } = *ast.node(key) {
                    let s = ast.atom(k).to_owned();
                    let at = ast.span(key).start;
                    let nk = if ts_r_codegen::literals::is_identifier_name(&s) && s.is_ascii() {
                        Some(build::ident(ast, &s, at))
                    } else {
                        // `{ "10": x }` is `{ 10: x }`.
                        canonical_index(&s).map(|n| build::number(ast, n, at))
                    };
                    if let Some(nk) = nk {
                        *ast.node_mut(id) = Node::Property {
                            modifiers,
                            key: nk,
                            value,
                        };
                    }
                }
            }
            Node::Conditional { test, cons, alt } => {
                if let Node::Unary {
                    op: UnaryOp::Not,
                    arg,
                } = *ast.node(test)
                {
                    *ast.node_mut(id) = Node::Conditional {
                        test: arg,
                        cons: alt,
                        alt: cons,
                    };
                }
                self.coalesce(ast, id);
                self.fold_expr(ast, id);
            }
            Node::Binary { op, left, right }
                if matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq)
                    && (is_typeof(ast, left) && is_string(ast, right)
                        || is_string(ast, left) && is_typeof(ast, right)) =>
            {
                // `typeof x === "y"`: both sides are strings, so `==` means the same.
                let op = if op == BinaryOp::StrictEq {
                    BinaryOp::Eq
                } else {
                    BinaryOp::NotEq
                };
                *ast.node_mut(id) = Node::Binary { op, left, right };
                self.typeof_undefined(ast, id);
                self.fold_expr(ast, id);
            }
            Node::Binary {
                op: BinaryOp::Eq | BinaryOp::NotEq,
                left,
                right,
            } if is_typeof(ast, left) || is_typeof(ast, right) => {
                self.typeof_undefined(ast, id);
                self.fold_expr(ast, id);
            }
            Node::Binary { .. } | Node::Unary { .. } | Node::TemplateLit { .. } => {
                self.fold_expr(ast, id)
            }
            Node::Arrow {
                flags,
                type_params,
                params,
                return_type,
                body,
            } if !flags.contains(FnFlags::EXPRESSION_BODY) => {
                self.tail(ast, body, false);
                // `() => { return x; }` to `() => x`.
                if let Node::Block { stmts } = *ast.node(body)
                    && let [only] = *ast.list(stmts)
                    && let Node::Return { arg: Some(arg) } = *ast.node(only)
                {
                    *ast.node_mut(id) = Node::Arrow {
                        flags: flags | FnFlags::EXPRESSION_BODY,
                        type_params,
                        params,
                        return_type,
                        body: arg,
                    };
                }
            }
            Node::VarDecl {
                modifiers,
                kind: VarKind::Const,
                decls,
            } if self.never_written(ast, decls) && !self.top_level(ast, decls) => {
                // `let` is shorter and the same when nothing assigns to the binding. Top-level
                // `const` stays: V8 constant-folds module and script `const` bindings (calls
                // through them inline), not `let` ones.
                *ast.node_mut(id) = Node::VarDecl {
                    modifiers,
                    kind: VarKind::Let,
                    decls,
                };
            }
            _ => {}
        }
    }

    /// Function and loop bodies end in an implicit `return;` / `continue;`: a trailing one
    /// goes, and `if (a) return; rest` becomes `if (!a) { rest }` (then `a || rest`).
    fn tail(&mut self, ast: &mut Ast, block: NodeId, in_loop: bool) {
        if !self.opts.join {
            return;
        }
        let Node::Block { stmts } = *ast.node(block) else {
            return;
        };
        let mut list = ast.list(stmts).to_vec();
        let exit = |ast: &Ast, s: NodeId| match *ast.node(s) {
            Node::Return { arg: None } => !in_loop,
            Node::Continue { label: None } => in_loop,
            _ => false,
        };
        let mut changed = false;
        // Every round removes an exit statement.
        for _ in 0..=list.len() {
            if let Some(&last) = list.last()
                && exit(ast, last)
            {
                list.pop();
                changed = true;
                continue;
            }
            let Some(i) = list.iter().rposition(
                |&s| matches!(*ast.node(s), Node::If { cons, alt: None, .. } if exit(ast, cons)),
            ) else {
                break;
            };
            let rest = list[i + 1..].to_vec();
            if rest.iter().any(|&s| declares_block_scoped(ast, s)) {
                break;
            }
            let Node::If { test, .. } = *ast.node(list[i]) else {
                break;
            };
            let at = ast.span(list[i]).start;
            list.truncate(i);
            if rest.is_empty() {
                if !self.pure(ast, test) {
                    let s = build::expr_stmt(ast, test, at);
                    self.push_joined(ast, &mut list, s);
                }
            } else {
                let neg = negate(ast, test, at);
                let body = match rest[..] {
                    [only] => only,
                    _ => build::block(ast, &rest, at),
                };
                let s = ast.add(
                    Node::If {
                        test: neg,
                        cons: body,
                        alt: None,
                    },
                    sp(at),
                );
                self.if_stmt(ast, s, neg, body, None);
                self.push_joined(ast, &mut list, s);
            }
            changed = true;
        }
        if changed {
            let stmts = ast.add_list(&list);
            *ast.node_mut(block) = Node::Block { stmts };
        }
    }

    /// Pushes `s`, joining it with the statements before it while a rule applies.
    fn push_joined(&self, ast: &mut Ast, out: &mut Vec<NodeId>, mut s: NodeId) {
        if self.opts.join {
            while let Some(&prev) = out.last()
                && let Some(merged) = self.join(ast, prev, s)
            {
                out.pop();
                s = merged;
            }
        }
        out.push(s);
    }

    /// Rewrites for an expression whose value is unused (each item of a sequence too).
    fn discarded(&self, ast: &mut Ast, e: NodeId) -> NodeId {
        if let Node::Seq { exprs } = *ast.node(e) {
            let items = ast.list(exprs).to_vec();
            let mut changed = false;
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let n = self.optional_call(ast, it).unwrap_or(it);
                changed |= n != it;
                out.push(n);
            }
            if changed {
                let exprs = ast.add_list(&out);
                *ast.node_mut(e) = Node::Seq { exprs };
            }
            return e;
        }
        self.optional_call(ast, e).unwrap_or(e)
    }

    /// `x != null && x.f()` / `x == null || x.f()` as a statement: `x?.f()`.
    fn optional_call(&self, ast: &mut Ast, expr: NodeId) -> Option<NodeId> {
        let Node::Binary { left, op, right } = *ast.node(expr) else {
            return None;
        };
        let want = match op {
            BinaryOp::LogicalAnd => BinaryOp::NotEq,
            BinaryOp::LogicalOr => BinaryOp::Eq,
            _ => return None,
        };
        let x = self.null_check(ast, left, want)?;
        let mut e = right;
        loop {
            match *ast.node(e) {
                Node::Member { obj, prop, .. } => {
                    if self.same_ident(ast, obj, x) {
                        *ast.node_mut(e) = Node::Member {
                            obj,
                            optional: true,
                            prop,
                        };
                        return Some(right);
                    }
                    e = obj;
                }
                Node::Index { obj, index, .. } => {
                    if self.same_ident(ast, obj, x) {
                        *ast.node_mut(e) = Node::Index {
                            obj,
                            optional: true,
                            index,
                        };
                        return Some(right);
                    }
                    e = obj;
                }
                Node::Call {
                    callee,
                    type_args,
                    args,
                    ..
                } => {
                    if self.same_ident(ast, callee, x) {
                        *ast.node_mut(e) = Node::Call {
                            callee,
                            optional: true,
                            type_args,
                            args,
                        };
                        return Some(right);
                    }
                    e = callee;
                }
                _ => return None,
            }
        }
    }

    /// The identifier `x` of `x == null` (`op` `==`) or `x != null` (`op` `!=`), either
    /// order, `null` or `void 0`.
    fn null_check(&self, ast: &Ast, e: NodeId, op: BinaryOp) -> Option<NodeId> {
        let Node::Binary { left, op: o, right } = *ast.node(e) else {
            return None;
        };
        if o != op {
            return None;
        }
        let ident = |n: NodeId| matches!(ast.node(n), Node::Ident { .. });
        if ident(left) && nullish(ast, right).is_some() {
            Some(left)
        } else if nullish(ast, left).is_some() && ident(right) {
            Some(right)
        } else {
            None
        }
    }

    /// Two identifiers naming the same binding (or the same global).
    fn same_ident(&self, ast: &Ast, a: NodeId, b: NodeId) -> bool {
        match (*ast.node(a), *ast.node(b)) {
            (Node::Ident { name: x }, Node::Ident { name: y }) => {
                x == y && self.scopes.symbol_of(a) == self.scopes.symbol_of(b)
            }
            _ => false,
        }
    }

    /// `x === null || x === void 0` to `x == null` (`&&`/`!==`: `x != null`), and
    /// `a && (b && c)` to `a && b && c`.
    fn logical(&self, ast: &mut Ast, id: NodeId, left: NodeId, op: BinaryOp, right: NodeId) {
        let (strict, loose) = if op == BinaryOp::LogicalOr {
            (BinaryOp::StrictEq, BinaryOp::Eq)
        } else {
            (BinaryOp::StrictNotEq, BinaryOp::NotEq)
        };
        let (head, l) = match *ast.node(left) {
            Node::Binary {
                left: h,
                op: o,
                right: r,
            } if o == op => (Some(h), r),
            _ => (None, left),
        };
        if let Some(x) = self.null_pair(ast, l, right, strict) {
            let at = ast.span(id).start;
            let null = build::null(ast, at);
            match head {
                None => {
                    *ast.node_mut(id) = Node::Binary {
                        left: x,
                        op: loose,
                        right: null,
                    };
                }
                Some(h) => {
                    let cmp = build::binary(ast, x, loose, null, at);
                    *ast.node_mut(id) = Node::Binary {
                        left: h,
                        op,
                        right: cmp,
                    };
                }
            }
            return;
        }
        if let Node::Binary { op: o, .. } = *ast.node(right)
            && o == op
        {
            let at = ast.span(id).start;
            let chain = logical_chain(ast, left, op, right, at);
            *ast.node_mut(id) = *ast.node(chain);
        }
    }

    /// `x === null` and `x === void 0` (`op` `===`; `!==` likewise) on the same identifier,
    /// in either order: that identifier.
    fn null_pair(&self, ast: &Ast, a: NodeId, b: NodeId, op: BinaryOp) -> Option<NodeId> {
        let side = |e: NodeId| -> Option<(NodeId, bool)> {
            let Node::Binary { left, op: o, right } = *ast.node(e) else {
                return None;
            };
            if o != op {
                return None;
            }
            let ident = |n: NodeId| matches!(ast.node(n), Node::Ident { .. });
            if ident(left) {
                nullish(ast, right).map(|k| (left, k))
            } else if ident(right) {
                nullish(ast, left).map(|k| (right, k))
            } else {
                None
            }
        };
        let (x, kx) = side(a)?;
        let (y, ky) = side(b)?;
        (kx != ky && self.same_ident(ast, x, y)).then_some(x)
    }

    /// `x != null ? x : y` and `x == null ? y : x` to `x ?? y`.
    fn coalesce(&self, ast: &mut Ast, id: NodeId) {
        let Node::Conditional { test, cons, alt } = *ast.node(id) else {
            return;
        };
        let (left, right) = if let Some(x) = self.null_check(ast, test, BinaryOp::NotEq)
            && self.same_ident(ast, x, cons)
        {
            (cons, alt)
        } else if let Some(x) = self.null_check(ast, test, BinaryOp::Eq)
            && self.same_ident(ast, x, alt)
        {
            (alt, cons)
        } else {
            return;
        };
        *ast.node_mut(id) = Node::Binary {
            left,
            op: BinaryOp::Coalesce,
            right,
        };
    }

    /// `typeof x == "undefined"` to `typeof x > "u"` (every other `typeof` result sorts
    /// before `"u"`); `!=` to `<`.
    fn typeof_undefined(&self, ast: &mut Ast, id: NodeId) {
        let Node::Binary { left, op, right } = *ast.node(id) else {
            return;
        };
        let undef = |n: NodeId| matches!(*ast.node(n), Node::StringLit { value } if ast.atom(value) == "undefined");
        let typeof_left = if is_typeof(ast, left) && undef(right) {
            true
        } else if undef(left) && is_typeof(ast, right) {
            false
        } else {
            return;
        };
        let at = ast.span(id).start;
        let u = build::string(ast, "u", at);
        let op = if (op == BinaryOp::Eq) == typeof_left {
            BinaryOp::Gt
        } else {
            BinaryOp::Lt
        };
        *ast.node_mut(id) = if typeof_left {
            Node::Binary { left, op, right: u }
        } else {
            Node::Binary { left: u, op, right }
        };
    }

    /// `decls` declares top-level (module or script scope) bindings.
    fn top_level(&self, ast: &Ast, decls: NodeList) -> bool {
        let mut ids = Vec::new();
        for &d in ast.list(decls) {
            if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                fold::binding_idents(ast, name, &mut ids);
            }
        }
        ids.iter().any(|&i| {
            self.scopes
                .symbol_of(i)
                .is_some_and(|s| self.scopes.symbol(s).scope == ScopeId::MODULE)
        })
    }

    /// No declared name of `decls` is ever assigned (and no direct `eval` could).
    fn never_written(&self, ast: &Ast, decls: NodeList) -> bool {
        if self.scopes.has_direct_eval || self.scopes.has_with {
            return false;
        }
        let mut ids = Vec::new();
        for &d in ast.list(decls) {
            if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                fold::binding_idents(ast, name, &mut ids);
            }
        }
        ids.iter().all(|&i| {
            self.scopes
                .symbol_of(i)
                .is_some_and(|s| self.scopes.symbol(s).writes == 0)
        })
    }

    /// `undefined` may be rewritten only where it is read as a value (not an assignment
    /// target, property key or declaration). Declarations and keys are not plain `Ident`
    /// references, so we only need to exclude writes.
    fn is_value_position(&self, _ast: &Ast, id: NodeId) -> bool {
        self.undefined_refs.get(&id.raw()) == Some(&false)
    }

    fn fold_expr(&mut self, ast: &mut Ast, id: NodeId) {
        let Some(c) = fold::eval(ast, id, Some(self.scopes)) else {
            return;
        };
        if !self.pure(ast, id) {
            return;
        }
        let at = ast.span(id).start;
        let new = match c {
            Const::Undefined => build::undefined(ast, at),
            Const::Null => build::null(ast, at),
            Const::Bool(b) => {
                if self.opts.booleans {
                    let n = build::number(ast, if b { 0.0 } else { 1.0 }, at);
                    build::unary(ast, UnaryOp::Not, n, at)
                } else {
                    build::boolean(ast, b, at)
                }
            }
            Const::Num(n) => {
                if !n.is_finite() {
                    return;
                }
                let n_abs = build::number(ast, n.abs(), at);
                if n.is_sign_negative() && n != 0.0 {
                    build::unary(ast, UnaryOp::Minus, n_abs, at)
                } else if n == 0.0 && n.is_sign_negative() {
                    return;
                } else {
                    n_abs
                }
            }
            Const::Str(s) => {
                // Only fold when the result is not longer than the input.
                let span = ast.span(id);
                if span.start != span.end && s.len() + 2 > (span.end - span.start) as usize {
                    return;
                }
                build::string(ast, &s, at)
            }
        };
        build::replace(ast, id, new);
    }

    fn if_stmt(
        &mut self,
        ast: &mut Ast,
        id: NodeId,
        test: NodeId,
        cons: NodeId,
        alt: Option<NodeId>,
    ) {
        let mut test = test;
        let mut cons = self.unwrap_block(ast, cons).unwrap_or(cons);
        let alt = alt.map(|a| self.unwrap_block(ast, a).unwrap_or(a));
        let alt = alt.filter(|&a| !matches!(ast.node(a), Node::Empty {}));
        let at = ast.span(id).start;
        if self.opts.join {
            // `if (a) if (b) s` (no `else` on either): `if (a && b) s`.
            if alt.is_none() {
                let inner = match *ast.node(cons) {
                    Node::If { alt: None, .. } => Some(cons),
                    Node::Block { stmts } => match *ast.list(stmts) {
                        [only] if matches!(ast.node(only), Node::If { alt: None, .. }) => {
                            Some(only)
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(inner) = inner
                    && let Node::If {
                        test: t2, cons: c2, ..
                    } = *ast.node(inner)
                {
                    test = logical_chain(ast, test, BinaryOp::LogicalAnd, t2, at);
                    cons = c2;
                }
            }
            let ce = expr_of(ast, cons);
            let ae = alt.map(|a| expr_of(ast, a));
            match (ce, ae) {
                (Some(c), None) => {
                    let e = self.and_or(ast, test, c, at);
                    let e = self.discarded(ast, e);
                    *ast.node_mut(id) = Node::ExprStmt { expr: e };
                    return;
                }
                (Some(c), Some(Some(a))) => {
                    let e = conditional(ast, test, c, a, at);
                    *ast.node_mut(id) = Node::ExprStmt { expr: e };
                    return;
                }
                _ => {}
            }
            if let Some(a) = alt {
                match (*ast.node(cons), *ast.node(a)) {
                    // `if (a) return x; else return y;`: `return a ? x : y;`
                    (Node::Return { arg: x }, Node::Return { arg: y })
                        if x.is_some() || y.is_some() =>
                    {
                        let x = x.unwrap_or_else(|| build::undefined(ast, at));
                        let y = y.unwrap_or_else(|| build::undefined(ast, at));
                        let arg = conditional(ast, test, x, y, at);
                        *ast.node_mut(id) = Node::Return { arg: Some(arg) };
                        return;
                    }
                    (Node::Throw { arg: x }, Node::Throw { arg: y }) => {
                        let arg = conditional(ast, test, x, y, at);
                        *ast.node_mut(id) = Node::Throw { arg };
                        return;
                    }
                    _ => {}
                }
            }
            if matches!(ast.node(cons), Node::Empty {}) && alt.is_none() {
                *ast.node_mut(id) = Node::ExprStmt { expr: test };
                return;
            }
        }
        *ast.node_mut(id) = Node::If { test, cons, alt };
    }

    /// `test && expr`, or `x || expr` when `test` is `!x`; a sequence test keeps its
    /// leading expressions outside (`a, b && c`).
    fn and_or(&self, ast: &mut Ast, test: NodeId, expr: NodeId, at: u32) -> NodeId {
        if let Node::Seq { exprs } = *ast.node(test) {
            let mut items = ast.list(exprs).to_vec();
            if let Some(last) = items.pop() {
                let e = self.and_or(ast, last, expr, at);
                items.push(e);
                return build::seq(ast, &items, at);
            }
        }
        if let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *ast.node(test)
        {
            return logical_chain(ast, arg, BinaryOp::LogicalOr, expr, at);
        }
        logical_chain(ast, test, BinaryOp::LogicalAnd, expr, at)
    }

    /// The single simple statement inside a block, if the block can be dropped.
    fn unwrap_block(&self, ast: &Ast, id: NodeId) -> Option<NodeId> {
        let Node::Block { stmts } = *ast.node(id) else {
            return None;
        };
        let list = ast.list(stmts);
        if list.is_empty() {
            return None;
        }
        if list.len() != 1 {
            return None;
        }
        let s = list[0];
        // Only statements that can't end in a dangling `if` or declare block-scoped names.
        match *ast.node(s) {
            Node::ExprStmt { .. }
            | Node::Return { .. }
            | Node::Throw { .. }
            | Node::Break { .. }
            | Node::Continue { .. }
            | Node::Debugger {} => Some(s),
            Node::VarDecl {
                kind: VarKind::Var, ..
            } => Some(s),
            _ => None,
        }
    }

    fn stmt_list(&mut self, ast: &mut Ast, list: NodeList, top: bool) -> NodeList {
        let input = ast.list(list).to_vec();
        let mut out: Vec<NodeId> = Vec::with_capacity(input.len());
        let mut terminated = false;
        let mut prologue = top;
        let mut flat: Vec<NodeId> = Vec::with_capacity(input.len());
        for s in input {
            self.flatten_into(ast, s, &mut flat);
        }
        for s in flat {
            let node = *ast.node(s);
            if !matches!(node, Node::Directive { .. }) {
                prologue = false;
            }
            let _ = prologue;
            if terminated {
                // Keep hoisted declarations only.
                match node {
                    Node::FunctionDecl { .. } => out.push(s),
                    _ => {
                        let mut names = Vec::new();
                        fold::collect_var_names(ast, s, true, &mut names);
                        if !names.is_empty() {
                            let decls: Vec<_> = names.into_iter().map(|n| (n, None)).collect();
                            let at = ast.span(s).start;
                            out.push(build::var_decl_many(ast, VarKind::Var, &decls, at));
                        }
                    }
                }
                continue;
            }
            let s = match node {
                Node::VarDecl {
                    modifiers,
                    kind: kind @ (VarKind::Let | VarKind::Const),
                    decls,
                } if !modifiers.contains(Modifiers::EXPORT) => match self.drop_unused(ast, decls) {
                    Some(kept) if kept == decls => s,
                    Some(kept) => ast.add(
                        Node::VarDecl {
                            modifiers,
                            kind,
                            decls: kept,
                        },
                        ast.span(s),
                    ),
                    None => continue,
                },
                _ => s,
            };
            match node {
                Node::Empty {} => continue,
                Node::ExprStmt { expr } if self.pure(ast, expr) => continue,
                Node::Return { .. }
                | Node::Throw { .. }
                | Node::Break { .. }
                | Node::Continue { .. } => terminated = true,
                _ => {}
            }
            self.push_joined(ast, &mut out, s);
        }
        ast.add_list(&out)
    }

    /// The declarators of a `let`/`const` list minus unused function-local bindings with
    /// pure initializers (`None` when none are left). Module-scope bindings stay: other
    /// modules of a bundle may refer to them.
    fn drop_unused(&self, ast: &mut Ast, decls: NodeList) -> Option<NodeList> {
        if !self.opts.join || self.scopes.has_direct_eval || self.scopes.has_with {
            return Some(decls);
        }
        let list = ast.list(decls).to_vec();
        let kept: Vec<NodeId> = list
            .iter()
            .copied()
            .filter(|&d| {
                let Node::VarDeclarator { name, init, .. } = *ast.node(d) else {
                    return true;
                };
                let unused = self.scopes.symbol_of(name).filter(|&s| {
                    let sym = self.scopes.symbol(s);
                    matches!(ast.node(name), Node::Ident { .. })
                        && sym.refs == 0
                        && sym.scope != ScopeId::MODULE
                });
                let Some(unused) = unused else {
                    return true;
                };
                let reader = self.scopes.symbol(unused).scope;
                !init.is_none_or(|i| self.pure(ast, i) && self.initialized_reads(ast, i, reader))
            })
            .collect();
        if kept.is_empty() {
            None
        } else if kept.len() == list.len() {
            Some(decls)
        } else {
            Some(ast.add_list(&kept))
        }
    }

    /// Every binding `expr` reads while it runs (in `reader`) is initialized by then: no read
    /// of a `let`/`const`/`class` before its declaration ran (its temporal dead zone), which
    /// throws. Proven only for one declared earlier in the same function, outside a `switch`
    /// (whose cases can be entered past it); imports may be uninitialized in a cycle. Function
    /// bodies in `expr` don't run, so their reads don't count.
    fn initialized_reads(&self, ast: &Ast, expr: NodeId, reader: ScopeId) -> bool {
        let var_target = |mut s: ScopeId| loop {
            let sc = self.scopes.scope(s);
            match sc.parent {
                Some(p) if !sc.kind.is_var_target() => s = p,
                _ => break s,
            }
        };
        let mut ok = true;
        let mut stack = vec![expr];
        while let Some(id) = stack.pop() {
            match ast.node(id) {
                Node::FunctionExpr { .. } | Node::Arrow { .. } => continue,
                Node::Ident { .. } => {
                    let Some(s) = self.scopes.symbol_of(id) else {
                        continue;
                    };
                    let sym = self.scopes.symbol(s);
                    let initialized = match sym.kind {
                        SymbolKind::Let
                        | SymbolKind::Const
                        | SymbolKind::Using
                        | SymbolKind::Class => sym.decl.is_some_and(|d| {
                            ast.span(d).end <= ast.span(id).start
                                && self.scopes.scope(sym.scope).kind != ScopeKind::Switch
                                && var_target(sym.scope) == var_target(reader)
                        }),
                        SymbolKind::Import => false,
                        _ => true,
                    };
                    if !initialized {
                        ok = false;
                        break;
                    }
                }
                _ => ast.for_each_child(id, |c| stack.push(c)),
            }
        }
        ok
    }

    /// Inlines nested blocks that declare nothing block-scoped.
    fn flatten_into(&self, ast: &Ast, s: NodeId, out: &mut Vec<NodeId>) {
        if let Node::Block { stmts } = *ast.node(s) {
            let list = ast.list(stmts);
            let lexical = list.iter().any(|&x| match *ast.node(x) {
                Node::VarDecl { kind, .. } => kind != VarKind::Var,
                Node::ClassDecl { .. } | Node::FunctionDecl { .. } => true,
                _ => false,
            });
            if !lexical {
                for &x in list {
                    self.flatten_into(ast, x, out);
                }
                return;
            }
        }
        out.push(s);
    }

    /// Joins two adjacent statements into one, if a rule applies.
    fn join(&self, ast: &mut Ast, prev: NodeId, next: NodeId) -> Option<NodeId> {
        let at = ast.span(prev).start;
        match (*ast.node(prev), *ast.node(next)) {
            (Node::ExprStmt { expr: a }, Node::ExprStmt { expr: b }) => {
                let e = seq_join(ast, a, b, at);
                Some(build::expr_stmt(ast, e, at))
            }
            (Node::ExprStmt { expr: a }, Node::Return { arg: Some(b) }) => {
                let e = seq_join(ast, a, b, at);
                Some(build::ret(ast, Some(e), at))
            }
            (Node::ExprStmt { expr: a }, Node::Throw { arg: b }) => {
                let e = seq_join(ast, a, b, at);
                Some(ast.add(Node::Throw { arg: e }, sp(at)))
            }
            (Node::ExprStmt { expr: a }, Node::If { test, cons, alt }) => {
                let e = seq_join(ast, a, test, at);
                Some(ast.add(Node::If { test: e, cons, alt }, sp(ast.span(next).start)))
            }
            // `if (a) return x; return y;`: `return a ? x : y;`
            (
                Node::If {
                    test,
                    cons,
                    alt: None,
                },
                Node::Return { arg: y },
            ) if matches!(*ast.node(cons), Node::Return { arg: x } if x.is_some() || y.is_some()) =>
            {
                let Node::Return { arg: x } = *ast.node(cons) else {
                    return None;
                };
                let x = x.unwrap_or_else(|| build::undefined(ast, at));
                let y = y.unwrap_or_else(|| build::undefined(ast, at));
                let arg = conditional(ast, test, x, y, at);
                Some(build::ret(ast, Some(arg), at))
            }
            // `e; for (;;)`: `for (e;;)`; `var a; for (var b;;)`: `for (var a, b;;)`.
            (
                Node::ExprStmt { expr: a },
                Node::For {
                    init,
                    test,
                    update,
                    body,
                },
            ) if init.is_none_or(|i| !matches!(ast.node(i), Node::VarDecl { .. })) => {
                let init = Some(match init {
                    Some(i) => seq_join(ast, a, i, at),
                    None => a,
                });
                Some(ast.add(
                    Node::For {
                        init,
                        test,
                        update,
                        body,
                    },
                    ast.span(next),
                ))
            }
            (
                Node::VarDecl {
                    modifiers,
                    kind: VarKind::Var,
                    decls: d1,
                },
                Node::For {
                    init,
                    test,
                    update,
                    body,
                },
            ) if !modifiers.contains(Modifiers::EXPORT)
                && init.is_none_or(|i| {
                    matches!(*ast.node(i), Node::VarDecl { modifiers: m2, kind: VarKind::Var, .. } if m2 == modifiers)
                }) =>
            {
                let decls = match init.map(|i| *ast.node(i)) {
                    Some(Node::VarDecl { decls: d2, .. }) => {
                        let mut all = ast.list(d1).to_vec();
                        all.extend_from_slice(ast.list(d2));
                        ast.add_list(&all)
                    }
                    _ => d1,
                };
                let init = ast.add(
                    Node::VarDecl {
                        modifiers,
                        kind: VarKind::Var,
                        decls,
                    },
                    ast.span(prev),
                );
                Some(ast.add(
                    Node::For {
                        init: Some(init),
                        test,
                        update,
                        body,
                    },
                    ast.span(next),
                ))
            }
            (
                Node::VarDecl {
                    modifiers: m1,
                    kind: k1,
                    decls: d1,
                },
                Node::VarDecl {
                    modifiers: m2,
                    kind: k2,
                    decls: d2,
                },
            ) if k1 == k2
                && m1 == m2
                && !m1.contains(Modifiers::EXPORT)
                && !matches!(k1, VarKind::Using | VarKind::AwaitUsing) =>
            {
                let mut all = ast.list(d1).to_vec();
                all.extend_from_slice(ast.list(d2));
                let decls = ast.add_list(&all);
                Some(ast.add(
                    Node::VarDecl {
                        modifiers: m1,
                        kind: k1,
                        decls,
                    },
                    ast.span(prev),
                ))
            }
            _ => None,
        }
    }
}

/// The expression of an expression statement.
fn is_typeof(ast: &Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(id),
        Node::Unary {
            op: UnaryOp::TypeOf,
            ..
        }
    )
}

fn is_string(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::StringLit { .. })
}

fn expr_of(ast: &Ast, s: NodeId) -> Option<NodeId> {
    match *ast.node(s) {
        Node::ExprStmt { expr } => Some(expr),
        _ => None,
    }
}

/// The number a canonical array-index string names (`"0"`, `"42"`; not `"01"`, `"1.5"`), small
/// enough to print back as the same digits.
fn canonical_index(s: &str) -> Option<f64> {
    let ok = !s.is_empty()
        && s.len() <= 15
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    if ok { s.parse().ok() } else { None }
}

/// `null` (`Some(true)`) or `void <number>` (`Some(false)`).
fn nullish(ast: &Ast, n: NodeId) -> Option<bool> {
    match *ast.node(n) {
        Node::NullLit {} => Some(true),
        Node::Unary {
            op: UnaryOp::Void,
            arg,
        } if matches!(ast.node(arg), Node::NumberLit { .. }) => Some(false),
        _ => None,
    }
}

/// A `?.` anywhere along the object/callee path of a member or call chain.
fn has_optional_chain(ast: &Ast, mut e: NodeId) -> bool {
    loop {
        match *ast.node(e) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                e = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                e = callee;
            }
            Node::NonNull { expr } => e = expr,
            _ => return false,
        }
    }
}

/// A statement whose bindings would change scope inside a new block.
fn declares_block_scoped(ast: &Ast, s: NodeId) -> bool {
    match *ast.node(s) {
        Node::VarDecl { kind, .. } => kind != VarKind::Var,
        Node::FunctionDecl { .. } | Node::ClassDecl { .. } => true,
        _ => false,
    }
}

/// `!test`, without double negation; equality operators flip.
fn negate(ast: &mut Ast, test: NodeId, at: u32) -> NodeId {
    match *ast.node(test) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => arg,
        Node::Binary { left, op, right }
            if matches!(
                op,
                BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
            ) =>
        {
            let op = match op {
                BinaryOp::Eq => BinaryOp::NotEq,
                BinaryOp::NotEq => BinaryOp::Eq,
                BinaryOp::StrictEq => BinaryOp::StrictNotEq,
                _ => BinaryOp::StrictEq,
            };
            build::binary(ast, left, op, right, at)
        }
        _ => build::unary(ast, UnaryOp::Not, test, at),
    }
}

/// The operands of a left-leaning `op` chain, in order.
fn chain_operands(ast: &Ast, e: NodeId, op: BinaryOp, out: &mut Vec<NodeId>) {
    let mut right_parts = Vec::new();
    let mut e = e;
    while let Node::Binary { left, op: o, right } = *ast.node(e) {
        if o != op {
            break;
        }
        right_parts.push(right);
        e = left;
    }
    out.push(e);
    out.extend(right_parts.into_iter().rev());
}

/// `a op b` as a left-leaning chain: `a && (b && c)` becomes `a && b && c`.
fn logical_chain(ast: &mut Ast, a: NodeId, op: BinaryOp, b: NodeId, at: u32) -> NodeId {
    let mut items = vec![a];
    chain_operands(ast, b, op, &mut items);
    let mut acc = items[0];
    for &it in &items[1..] {
        acc = build::binary(ast, acc, op, it, at);
    }
    acc
}

/// `test ? a : b`, with `!t ? a : b` as `t ? b : a` and `(x, t) ? a : b` as
/// `x, t ? a : b`.
fn conditional(ast: &mut Ast, test: NodeId, a: NodeId, b: NodeId, at: u32) -> NodeId {
    if let Node::Seq { exprs } = *ast.node(test) {
        let mut items = ast.list(exprs).to_vec();
        if let Some(last) = items.pop() {
            let c = conditional(ast, last, a, b, at);
            items.push(c);
            return build::seq(ast, &items, at);
        }
    }
    let (test, a, b) = match *ast.node(test) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => (arg, b, a),
        _ => (test, a, b),
    };
    ast.add(
        Node::Conditional {
            test,
            cons: a,
            alt: b,
        },
        sp(at),
    )
}

/// `a, b` flattening nested sequences.
fn seq_join(ast: &mut Ast, a: NodeId, b: NodeId, at: u32) -> NodeId {
    let mut items = Vec::new();
    for x in [a, b] {
        match *ast.node(x) {
            Node::Seq { exprs } => items.extend_from_slice(ast.list(exprs)),
            _ => items.push(x),
        }
    }
    build::seq(ast, &items, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;
    use ts_r_codegen::{CodegenOptions, Layout};

    fn run(src: &str) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, true);
        crate::fold::fold(&mut ast, src, &scopes);
        compress(&mut ast, src, &scopes, &CompressOptions::default());
        let o = CodegenOptions {
            layout: Layout::Compact,
            ..CodegenOptions::default()
        };
        ts_r_codegen::print(&ast, src, &o).code
    }

    #[test]
    fn statements() {
        assert_eq!(run("a(); b(); return_(); ;;"), "a(),b(),return_();");
        assert_eq!(run("if (x) { y(); }"), "x&&y();");
        assert_eq!(run("if (!x) y(); else z();"), "x?z():y();");
        assert_eq!(run("var a = 1; var b = 2;"), "var a=1,b=2;");
        assert_eq!(
            run("function f() { g(); return 1; h(); var q = 2; function k() {} }"),
            "function f(){return g(),1;var q;function k(){}}"
        );
        assert_eq!(run("if (a) { if (b) c(); } else d();"), "a?b&&c():d();");
    }

    #[test]
    fn unused_bindings_keep_reads_in_their_dead_zone() {
        let cases = [
            // Initialized before the read: dropped.
            (
                "function f() { let a = 1; let u = a; const v = () => later; let later = 2; return a + later; }",
                "function f(){let a=1,later=2;return a+later}",
            ),
            // Read before its declaration ran, in a switch, or from an import: kept.
            (
                "function f() { let u = later; let later = 1; return later; }",
                "function f(){let u=later,later=1;return later}",
            ),
            (
                "function f(k) { switch (k) { case 0: let x = 1; case 1: let u = x; return x; } }",
                "function f(k){switch(k){case 0:let x=1;case 1:let u=x;return x}}",
            ),
        ];
        for (src, want) in cases {
            assert_eq!(run(src), want, "{src}");
        }
    }

    #[test]
    fn control_flow() {
        let cases = [
            (
                "function f(a){ if (a) return 1; return 2; }",
                "function f(a){return a?1:2}",
            ),
            (
                "function f(a, b){ if (a) return 1; if (b) return 2; return 3; }",
                "function f(a,b){return a?1:b?2:3}",
            ),
            (
                "function f(a, b){ if (a) { if (b) return 1; } return 0; }",
                "function f(a,b){return a&&b?1:0}",
            ),
            (
                "function f(a){ if (a) return; g(); }",
                "function f(a){a||g()}",
            ),
            (
                "function f(a){ if (!a) return; g(); h(); }",
                "function f(a){a&&(g(),h())}",
            ),
            (
                "function f(a){ g(); if (a) return; let x = 1; h(x); }",
                "function f(a){if(g(),a)return;let x=1;h(x)}",
            ),
            ("function f(){ g(); return; }", "function f(){g()}"),
            ("for (;;) { if (a) continue; g(); }", "for(;;)a||g();"),
            (
                "function f(a){ if (a) throw 1; else throw 2; }",
                "function f(a){throw a?1:2}",
            ),
            (
                "function f(){ g(); if (a) return 1; return 2; }",
                "function f(){return g(),a?1:2}",
            ),
            (
                "var a = []; for (var i = 0; i < 3; i++) a.push(i);",
                "for(var a=[],i=0;i<3;i++)a.push(i);",
            ),
            ("g(); for (; i < 3; i++) h();", "for(g();i<3;i++)h();"),
            ("try { g(); } catch (e) { h(); }", "try{g()}catch{h()}"),
            ("try { g(); } catch (e) { h(e); }", "try{g()}catch(e){h(e)}"),
            (
                "function f(){ let a = 1, b = g(), c = Symbol('x'); const d = 2; return d; } const top = 1;",
                "function f(){let b=g(),d=2;return d}const top=1;",
            ),
        ];
        for (src, want) in cases {
            assert_eq!(run(src), want, "{src}");
        }
    }

    #[test]
    fn nullish_and_parens() {
        let cases = [
            ("x = a === null || a === undefined;", "x=a==null;"),
            ("x = void 0 !== a && null !== a;", "x=a!=null;"),
            ("x = b || a === null || a === void 0;", "x=b||a==null;"),
            (
                "x = a === null || b === undefined;",
                "x=a===null||b===void 0;",
            ),
            ("x = a != null ? a : b;", "x=a??b;"),
            ("x = a == null ? b : a;", "x=a??b;"),
            ("x = a.b != null ? a.b : c;", "x=a.b!=null?a.b:c;"),
            ("if (a != null) a.b.c();", "a?.b.c();"),
            ("a == null || a();", "a?.();"),
            ("a != null && (a.b = 1);", "a!=null&&(a.b=1);"),
            (
                "x = typeof a === 'undefined', y = 'undefined' != typeof b;",
                r#"x=typeof a>"u",y="u">typeof b;"#,
            ),
            ("x = a && (b && c);", "x=a&&b&&c;"),
            ("x = (a || b) && (c || d);", "x=(a||b)&&(c||d);"),
            ("x = (a, b);", "x=(a,b);"),
            ("x = (a + b) * c, y = (a * b) + c;", "x=(a+b)*c,y=a*b+c;"),
            ("x = (a?.b).c, y = (a?.b)();", "x=(a?.b).c,y=(a?.b)();"),
            ("('use strict');", ""),
            (
                "x = new (f())(), y = new (a.b)();",
                "x=new(f())(),y=new a.b();",
            ),
            (
                "x = { '0': a, '10': b, '01': c, '1.5': d, '1e3': e };",
                "x={0:a,10:b,'01':c,'1.5':d,'1e3':e};",
            ),
        ];
        for (src, want) in cases {
            assert_eq!(run(src), want, "{src}");
        }
    }

    #[test]
    fn expressions() {
        assert_eq!(
            run("x = undefined; y = true; z = a['b'];"),
            "x=void 0,y=!0,z=a.b;"
        );
        assert_eq!(run("x = 1 + 2 * 3; y = 'a' + 'b';"), "x=7,y=\"ab\";");
        assert_eq!(
            run("o = { 'key': 1, 'not-id': 2 };"),
            "o={key:1,'not-id':2};"
        );
        assert_eq!(run("x = 1.50;"), "x=1.5;");
        assert_eq!(
            run("f = () => { g(); return { a: 1 }; }; h = (x) => { return x; };"),
            "f=()=>(g(),{a:1}),h=x=>x;"
        );
        assert_eq!(
            run("x = typeof a === 'b'; y = 'c' !== typeof d; z = a === 'b';"),
            "x=typeof a=='b',y='c'!=typeof d,z=a==='b';"
        );
        // Top-level `const` stays (V8 constant-folds it); nested never-written ones become `let`.
        assert_eq!(
            run("const a = 1; const b = 2; a; const c = 3; c = 4;"),
            "const a=1,b=2,c=3;c=4;"
        );
        assert_eq!(
            run("{ const a = 1; const b = 2; f(a, b); const c = 3; c = 4; }"),
            "{let a=1,b=2;f(a,b);const c=3;c=4}"
        );
        assert_eq!(
            run("function f(undefined) { return undefined; }"),
            "function f(undefined){return undefined}"
        );
    }
}
