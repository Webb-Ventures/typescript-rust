//! Decorator placement (TS1206, TS1249).
//!
//! Rules observed from tsc 7.0.2 (reported once per decorated node, at its first `@`):
//! - Decorators on anything but a class or class member (`var`, `function`, `enum`,
//!   `interface`, `type`, `namespace`, imports and exports other than classes, static
//!   blocks): TS1206.
//! - Constructors: TS1206. Methods without a body (overloads, abstract and ambient
//!   methods): TS1249.
//! - Standard decorators (no `experimentalDecorators`): `abstract` and `declare` fields
//!   (including `abstract accessor`) and every parameter decorator: TS1206.
//! - `experimentalDecorators`: class expressions and members with `#private` names: TS1206;
//!   parameter decorators are valid only on parameters of class constructors, methods and
//!   accessors that have a body: TS1206 elsewhere.

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId, NodeList};
use ts_r_diagnostics::FileId;
use ts_r_span::Span;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::fn_parts;
use ts_r_types::TypeId;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the decorators written on node `n` of `file`.
    pub(crate) fn check_decorator_placement(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let exp = self.c.opts.experimental_decorators;
        match *ast.node(n) {
            // Decorators the parser accepted and dropped: the node starts at the `@`.
            Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::Enum { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Module { .. }
            | Node::ImportDecl { .. }
            | Node::ImportEquals { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportAssignment { .. }
            | Node::StaticBlock { .. } => {
                if self.c.text(Loc::new(file, n)).starts_with('@') {
                    let s = ast.span(n).start;
                    self.decorator_error(file, s, dg::DECORATORS_ARE_NOT_VALID_HERE_1206);
                }
            }
            Node::ClassDecl { decorators, .. } => {
                // `export @dec default class C {}`: decorators between `export` and `default`.
                if let Some(&last) = ast.list(decorators).last() {
                    let text = &self.c.file(file).text;
                    let rest = text.get(ast.span(last).end as usize..).unwrap_or("");
                    let rest = rest.trim_start();
                    if rest.starts_with("default")
                        && rest[7..].starts_with(|c: char| c.is_ascii_whitespace())
                    {
                        self.first_decorator_error(
                            file,
                            decorators,
                            dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                        );
                    }
                }
            }
            Node::ClassExpr { decorators, .. } if exp => {
                self.first_decorator_error(
                    file,
                    decorators,
                    dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                );
            }
            Node::MethodDef {
                decorators,
                modifiers,
                kind,
                key,
                value,
                ..
            } => {
                if ast.list(decorators).is_empty() {
                    return;
                }
                if kind == MethodKind::Constructor
                    || (exp
                        && (self.in_class_expr(file, n)
                            || matches!(ast.node(key), Node::PrivateName { .. })))
                {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                    );
                } else if matches!(kind, MethodKind::Get | MethodKind::Set) {
                    // Accessors without a body: abstract ones are invalid under standard
                    // decorators; other cases are not modelled.
                    if !exp
                        && modifiers.contains(Modifiers::ABSTRACT)
                        && fn_parts(ast, value).is_some_and(|p| p.body.is_none())
                    {
                        self.first_decorator_error(
                            file,
                            decorators,
                            dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                        );
                    }
                } else if fn_parts(ast, value).is_some_and(|p| p.body.is_none()) {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::A_DECORATOR_CAN_ONLY_DECORATE_A_METHOD_IMPLEMENTATION_NOT_AN_OVERLOAD_1249,
                    );
                }
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                ..
            } => {
                if ast.list(decorators).is_empty() {
                    return;
                }
                let bad = if exp {
                    matches!(ast.node(key), Node::PrivateName { .. }) || self.in_class_expr(file, n)
                } else {
                    modifiers.intersects(Modifiers::ABSTRACT | Modifiers::DECLARE)
                };
                if bad {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                    );
                }
            }
            Node::Param { decorators, .. } => {
                let list = ast.list(decorators);
                let (Some(&first), Some(&last)) = (list.first(), list.last()) else {
                    return;
                };
                if crate::nodes::is_this_param(ast, n) {
                    // tsc's span starts after the previous token (leading trivia included).
                    let text = self.c.file(file).text.as_bytes();
                    let mut start = ast.span(first).start as usize;
                    while start > 0 && text.get(start - 1).is_some_and(|b| b.is_ascii_whitespace())
                    {
                        start -= 1;
                    }
                    let span = Span::new(start as u32, ast.span(last).end);
                    self.error_span(
                        file,
                        span,
                        dg::NEITHER_DECORATORS_NOR_MODIFIERS_MAY_BE_APPLIED_TO_THIS_PARAMETERS_1433,
                        &[],
                    );
                    return;
                }
                if !exp || !self.param_decorators_allowed(Loc::new(file, n)) {
                    self.first_decorator_error(
                        file,
                        decorators,
                        dg::DECORATORS_ARE_NOT_VALID_HERE_1206,
                    );
                }
            }
            _ => {}
        }
    }

    /// Checks the expressions of the decorators written on class, member or parameter `n`
    /// (`@dec(C)` reads `C` like any expression: TS2304, TS2449, ...).
    pub(crate) fn check_decorator_expressions(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let decorators = match *ast.node(n) {
            // Invalid there (TS1206): tsc does not check the expressions.
            Node::ClassExpr { .. } if self.c.opts.experimental_decorators => return,
            Node::Param { .. }
                if crate::nodes::is_this_param(ast, n)
                    || !self.c.opts.experimental_decorators
                    || !self.param_decorators_allowed(Loc::new(file, n)) =>
            {
                return;
            }
            Node::ClassDecl { decorators, .. }
            | Node::ClassExpr { decorators, .. }
            | Node::PropertyDef { decorators, .. }
            | Node::MethodDef { decorators, .. }
            | Node::Param { decorators, .. } => decorators,
            _ => return,
        };
        for &d in ast.list(decorators) {
            if let Node::Decorator { expr } = *ast.node(d) {
                // A function decorator is contextually typed by the decorator signature;
                // its parameters are not implicitly `any` (modelled as an `any` context).
                let inner = crate::nodes::skip_parens(ast, expr);
                // `@await` in a module is an `await` with no operand (a parse error), not
                // a name.
                let head = match *ast.node(inner) {
                    Node::Call { callee, .. } => crate::nodes::skip_parens(ast, callee),
                    _ => inner,
                };
                if matches!(ast.node(head), Node::Ident { .. })
                    && self.c.text(Loc::new(file, head)) == "await"
                {
                    continue;
                }
                let t = if matches!(
                    ast.node(inner),
                    Node::FunctionExpr { .. } | Node::Arrow { .. }
                ) {
                    self.check_expr_with_context(Loc::new(file, inner), TypeId::ANY)
                } else {
                    self.check_expr(Loc::new(file, expr))
                };
                self.check_decorator_call(file, d, n, t);
            }
        }
    }

    /// `experimentalDecorators`: parameter `p` belongs to a class constructor, method or
    /// accessor with a body. Unknown owners count as allowed (no report).
    fn param_decorators_allowed(&self, p: Loc) -> bool {
        let Some(f) = self.c.parent(p) else {
            return true;
        };
        let ast = self.ast(p.file);
        match *ast.node(f.node) {
            Node::FunctionDecl { .. } => false,
            Node::FunctionExpr { .. } => {
                let Some(m) = self.c.parent(f) else {
                    return true;
                };
                if !matches!(ast.node(m.node), Node::MethodDef { .. }) {
                    return true;
                }
                // Class members only (object literal methods are left alone).
                let in_class = self.c.parent(m).is_some_and(|c| {
                    matches!(
                        ast.node(c.node),
                        Node::ClassDecl { .. } | Node::ClassExpr { .. }
                    )
                });
                !in_class || fn_parts(ast, f.node).is_none_or(|x| x.body.is_some())
            }
            _ => true,
        }
    }

    /// Whether class member `m` belongs to a class expression.
    fn in_class_expr(&self, file: FileId, m: NodeId) -> bool {
        self.c
            .parent(Loc::new(file, m))
            .is_some_and(|c| matches!(self.node(c), Node::ClassExpr { .. }))
    }

    fn first_decorator_error(
        &mut self,
        file: FileId,
        decorators: NodeList,
        info: dg::DiagnosticInfo,
    ) {
        let ast = self.ast(file);
        if let Some(&d) = ast.list(decorators).first() {
            let s = ast.span(d).start;
            self.decorator_error(file, s, info);
        }
    }

    fn decorator_error(&mut self, file: FileId, start: u32, info: dg::DiagnosticInfo) {
        self.error_span(file, Span::new(start, start + 1), info, &[]);
    }
}
