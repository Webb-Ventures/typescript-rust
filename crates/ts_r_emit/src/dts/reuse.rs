//! Types written from the source's syntax instead of the checker's type (tsc 7.0.2,
//! observed): the declaration keeps the annotation the author wrote when the type comes
//! straight from one.
//!
//! - `e as T` and `<T>e` (not `as const`) write `T`, as written (`'p'` keeps its quotes).
//! - A string or no-substitution template literal `as const` writes the literal as written
//!   (`'foo' as const` is `'foo'`; numbers are the checker's `16` for `0x10`).
//! - A function or arrow expression writes a function type: its type parameters and
//!   annotated parameters as written, unannotated ones inferred (a default that is itself
//!   reusable writes that type), and the annotated return type, else the reusable
//!   expression of its only `return` (or expression body), else the inferred return type.
//! - A function's or getter's only `return` of a reusable expression writes that type
//!   (`get x() { return null! as P }` is `get x(): P;`); two returns, or `return p` of a
//!   parameter, use the checker's type.
//!
//! Anything else (object and array literals, identifiers, calls) is the checker's type.

use ts_r_ast::visit::{Visitor, Walk, walk};
use ts_r_ast::{Ast, FnFlags, Node, NodeId};
use ts_r_binder::meaning;

use super::{Emitter, skip_parens};

/// Whether `ty` is the `const` of `as const`.
fn is_const_type(ast: &Ast, src: &str, ty: NodeId) -> bool {
    match *ast.node(ty) {
        Node::KeywordType {
            kind: ts_r_ast::KeywordTypeKind::Const,
        } => true,
        Node::TypeRef {
            name,
            type_args: None,
        } => {
            let s = ast.span(name);
            src.get(s.start as usize..s.end as usize) == Some("const")
        }
        _ => false,
    }
}

/// Whether `e` has a type tsc writes from its syntax.
pub(super) fn is_reusable(ast: &Ast, src: &str, e: NodeId) -> bool {
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::As { ty, expr } | Node::TypeAssertion { ty, expr } => {
            (!is_const_type(ast, src, ty) || const_string(ast, expr).is_some())
                && !ts_r_checker::typenode::has_assert_import_type(ast, ty)
        }
        Node::Arrow { flags, .. } | Node::FunctionExpr { flags, .. } => {
            !flags.contains(FnFlags::GENERATOR)
        }
        _ => false,
    }
}

/// The string or no-substitution template literal under `e`'s parentheses.
fn const_string(ast: &Ast, e: NodeId) -> Option<NodeId> {
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::StringLit { .. } => Some(e),
        Node::TemplateLit { parts } if ast.list(parts).len() == 1 => Some(e),
        _ => None,
    }
}

/// The expression of a function's only `return` (or its expression body), outside nested
/// functions and classes.
pub(super) fn single_return(ast: &Ast, f: NodeId) -> Option<NodeId> {
    let body = match *ast.node(f) {
        Node::Arrow { flags, body, .. } => {
            if flags.contains(FnFlags::EXPRESSION_BODY) {
                return Some(body);
            }
            body
        }
        Node::FunctionExpr { body, .. } | Node::FunctionDecl { body, .. } => body?,
        _ => return None,
    };
    struct Returns {
        root: NodeId,
        found: Vec<Option<NodeId>>,
    }
    impl Visitor for Returns {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if id == self.root {
                return Walk::Continue;
            }
            match *ast.node(id) {
                Node::Return { arg } => {
                    self.found.push(arg);
                    if self.found.len() > 1 {
                        return Walk::Stop;
                    }
                    Walk::SkipChildren
                }
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => Walk::SkipChildren,
                _ if is_expression(ast, id) => Walk::SkipChildren,
                _ => Walk::Continue,
            }
        }
    }
    let mut v = Returns {
        root: body,
        found: Vec::new(),
    };
    walk(ast, body, &mut v);
    match v.found.as_slice() {
        [Some(e)] => Some(*e),
        _ => None,
    }
}

/// Expressions cannot contain `return` statements outside nested functions, so the walk
/// skips them.
fn is_expression(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Call { .. }
            | Node::Binary { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::Member { .. }
            | Node::Assign { .. }
    )
}

impl Emitter<'_, '_> {
    /// Names copied from a return expression must remain visible after its body is erased.
    pub(super) fn return_type_accessible(&self, f: NodeId, expr: NodeId) -> bool {
        let body = match *self.ast.node(f) {
            Node::Arrow { body, .. } => body,
            Node::FunctionExpr { body, .. } | Node::FunctionDecl { body, .. } => {
                let Some(body) = body else { return true };
                body
            }
            _ => return true,
        };
        struct Names<'e, 'c, 'a> {
            e: &'e Emitter<'c, 'a>,
            f: NodeId,
            body: ts_r_span::Span,
            expr: ts_r_span::Span,
        }
        impl Visitor for Names<'_, '_, '_> {
            fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
                let (mut name, m) = match *ast.node(id) {
                    Node::TypeRef { name, .. } => (name, meaning::TYPE),
                    Node::TypeQuery { expr_name, .. } => (expr_name, meaning::VALUE),
                    _ => return Walk::Continue,
                };
                let mut m = m;
                while let Node::QualifiedName { left, .. } | Node::Member { obj: left, .. } =
                    *ast.node(name)
                {
                    name = left;
                    m = meaning::NAMESPACE | meaning::VALUE;
                }
                let Some(sym) = self.e.c.resolve_ident(self.e.loc(name), m) else {
                    return Walk::Stop;
                };
                for d in self.e.c.decls(sym) {
                    if d.file != self.e.file {
                        continue;
                    }
                    let span = ast.span(d.node);
                    // Parameters of a copied function or inline type remain in its signature.
                    if self.expr.start <= span.start
                        && span.end <= self.expr.end
                        && matches!(
                            ast.node(d.node),
                            Node::TypeParam { .. } | Node::Param { .. }
                        )
                    {
                        return Walk::Continue;
                    }
                    if self.body.start <= span.start && span.start < self.body.end {
                        return Walk::Stop;
                    }
                }
                if self
                    .e
                    .ck
                    .resolve_name_at(self.e.loc(self.f), self.e.raw(name), m)
                    == Some(sym)
                {
                    Walk::Continue
                } else {
                    Walk::Stop
                }
            }
        }
        let root = match *self.ast.node(skip_parens(&self.ast, expr)) {
            Node::As { ty, .. } | Node::TypeAssertion { ty, .. }
                if is_const_type(&self.ast, self.src, ty) =>
            {
                return true;
            }
            Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => ty,
            _ => expr,
        };
        walk(
            &self.ast,
            root,
            &mut Names {
                e: self,
                f,
                body: self.ast.span(body),
                expr: self.ast.span(expr),
            },
        )
    }

    /// Writes the type of `e` from its syntax when tsc does (see the module docs).
    /// Returns false, writing nothing, otherwise.
    pub(super) fn reuse_expr_type(&mut self, e: NodeId) -> bool {
        if self.is_synth(e) || !is_reusable(&self.ast, self.src, e) {
            return false;
        }
        let e = skip_parens(&self.ast, e);
        match *self.ast.node(e) {
            Node::As { ty, expr } | Node::TypeAssertion { ty, expr } => {
                if is_const_type(&self.ast, self.src, ty)
                    && let Some(lit) = const_string(&self.ast, expr)
                {
                    let span = self.ast.span(lit);
                    let text = self.src[span.start as usize..span.end as usize].to_owned();
                    self.w(&text);
                } else {
                    self.type_node(ty);
                }
                true
            }
            Node::Arrow {
                type_params,
                params,
                return_type,
                ..
            }
            | Node::FunctionExpr {
                type_params,
                params,
                return_type,
                ..
            } => {
                self.type_params(type_params);
                self.rebuilt_params = true;
                self.params(params, e);
                self.w(" => ");
                match return_type {
                    Some(r) => self.type_node(r),
                    None => self.inferred_return(e),
                }
                true
            }
            _ => false,
        }
    }

    /// Writes the return type of function-like `f` without annotation from its only
    /// `return` when that is reusable. Returns false, writing nothing, otherwise.
    pub(super) fn reuse_return_type(&mut self, f: NodeId) -> bool {
        match self.plain_single_return(f) {
            Some(r) if self.return_type_accessible(f, r) => self.reuse_expr_type(r),
            _ => false,
        }
    }

    /// The only returned expression of source function `f` when `f` is neither async nor
    /// a generator (its return type is that expression's type).
    pub(super) fn plain_single_return(&self, f: NodeId) -> Option<NodeId> {
        if self.is_synth(f) {
            return None;
        }
        let flags = match *self.ast.node(f) {
            Node::Arrow { flags, .. }
            | Node::FunctionExpr { flags, .. }
            | Node::FunctionDecl { flags, .. } => flags,
            _ => return None,
        };
        if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) {
            return None;
        }
        single_return(&self.ast, f)
    }
}
