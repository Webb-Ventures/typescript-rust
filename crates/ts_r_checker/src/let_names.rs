//! `let` as a binding name in block-scoped declarations (TS2480).
//!
//! Rules observed from tsc 7.0.2: every binding named `let` in a `let`, `const`, `using`
//! or `await using` declaration (including `for` heads and names nested in object and
//! array patterns) is TS2480 at the name, next to whatever the parser reports. `var`
//! declarations and property keys (`const { let: y } = o`) are fine. Like tsc's other
//! grammar errors it is not reported in a file with syntax errors.
//!
//! `var` names shadowed by an enclosing `let`/`const` (TS2481), observed from tsc 7.0.2:
//! a `var` binding (with or without an initializer, in a pattern, in a `for` head) whose
//! name, looked up from the declaration, is first found as a `let`/`const` of a block,
//! `for` head or `switch` scope between it and the scope the `var` hoists to, is reported
//! at the binding name. A class or function of that name, a catch parameter, or a
//! `let` in the hoisting scope itself (the binder's TS2451) is not.

use ts_r_ast::{Node, NodeId, VarKind};
use ts_r_binder::{ScopeKind, SymbolFlags};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the binding names of the variable declaration `n`.
    pub(crate) fn check_let_binding_names(&mut self, file: FileId, n: NodeId) {
        let ast = self.ast(file);
        let Node::VarDecl { kind, decls, .. } = *ast.node(n) else {
            return;
        };
        if kind == VarKind::Var {
            return;
        }
        let mut stack: Vec<NodeId> = Vec::new();
        for &d in ast.list(decls) {
            if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                stack.push(name);
            }
        }
        let mut found = Vec::new();
        let mut guard = 0u32;
        while let Some(p) = stack.pop() {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            match *ast.node(p) {
                Node::Ident { name } if ast.atom(name) == "let" => found.push(p),
                Node::ObjectPattern { props } => stack.extend(ast.list(props).iter().copied()),
                Node::ArrayPattern { elements } => stack.extend(ast.list(elements).iter().copied()),
                Node::BindingProperty { key, value, .. } => stack.push(value.unwrap_or(key)),
                Node::AssignPattern { left, .. } => stack.push(left),
                Node::Rest { arg } => stack.push(arg),
                _ => {}
            }
        }
        if found.is_empty() || self.has_syntax_errors(file) {
            return;
        }
        for p in found {
            self.error(
                Loc::new(file, p),
                dg::LET_IS_NOT_ALLOWED_TO_BE_USED_AS_A_NAME_IN_LET_OR_CONST_DECLARATIONS_2480,
                &[],
            );
        }
    }

    /// Checks the names of the `var` declaration `n` for enclosing block-scoped variables
    /// of the same name (TS2481).
    pub(crate) fn check_var_shadowing_block_scoped(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let Node::VarDecl { kind, decls, .. } = *ast.node(n) else {
            return;
        };
        if kind != VarKind::Var {
            return;
        }
        let mut stack: Vec<NodeId> = Vec::new();
        for &d in ast.list(decls) {
            if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                stack.push(name);
            }
        }
        let b = &self.c.file(file).bound;
        let parents = self.c.file(file).parents();
        // The innermost scope around the declaration.
        let mut first = None;
        let mut cur = Some(n);
        for _ in 0..10_000 {
            let Some(node) = cur else { break };
            if let Some(sc) = b.scope_of(node) {
                first = Some(sc);
                break;
            }
            cur = parents.parent(node);
        }
        let Some(first) = first else {
            return;
        };
        let mut found = Vec::new();
        let mut guard = 0u32;
        while let Some(p) = stack.pop() {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            match *ast.node(p) {
                Node::Ident { name } => {
                    let Some(name) = b.names.find(ast, ast.atom(name)) else {
                        continue;
                    };
                    let mut scope = Some(first);
                    for _ in 0..10_000 {
                        let Some(id) = scope else { break };
                        let sc = b.scopes[id.0 as usize];
                        if !matches!(
                            sc.kind,
                            ScopeKind::Block | ScopeKind::For | ScopeKind::Switch
                        ) {
                            break;
                        }
                        if let Some(s) = b.lookup(sc.locals, name) {
                            if b.symbol(s)
                                .flags
                                .contains(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                            {
                                found.push(p);
                            }
                            break;
                        }
                        scope = sc.parent;
                    }
                }
                Node::ObjectPattern { props } => stack.extend(ast.list(props).iter().copied()),
                Node::ArrayPattern { elements } => stack.extend(ast.list(elements).iter().copied()),
                Node::BindingProperty { key, value, .. } => stack.push(value.unwrap_or(key)),
                Node::AssignPattern { left, .. } => stack.push(left),
                Node::Rest { arg } => stack.push(arg),
                _ => {}
            }
        }
        for p in found {
            let Node::Ident { name } = *ast.node(p) else {
                continue;
            };
            let text = ast.atom(name);
            self.error(
                Loc::new(file, p),
                dg::CANNOT_INITIALIZE_OUTER_SCOPED_VARIABLE_0_IN_THE_SAME_SCOPE_AS_BLOCK_SCOPED_DECL_2481,
                &[text, text],
            );
        }
    }

    /// The file has syntax errors (other than the parser's checker-grammar errors), so tsc
    /// reports no grammar errors from the checker in it.
    pub(crate) fn has_syntax_errors(&self, file: FileId) -> bool {
        let f = self.c.file(file);
        ts_r_parser::without_grammar(&f.syntax, &f.grammar)
            .iter()
            .any(|d| d.is_error() && !crate::driver::CHECKER_GRAMMAR_CODES.contains(&d.code))
    }
}
