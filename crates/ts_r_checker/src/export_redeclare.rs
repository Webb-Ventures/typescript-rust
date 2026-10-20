//! Exports of a module file declared more than once (TS2323).
//!
//! Rules observed from tsc 7.0.2, for module files (TypeScript sources and declaration
//! files; not ambient `declare module "x"` bodies, namespaces or scripts):
//! - An export that merged several declarations (`export var a; export var a;`, two
//!   `export default` functions, `export default x` followed by `export default class`)
//!   is TS2323 "Cannot redeclare exported variable" at every declaration when more than one
//!   of them counts. Interfaces and overload signatures (functions without a body) do not
//!   count; interfaces are still reported, overload signatures are not.
//! - Namespaces and enums never report; a type alias needs three counted declarations.
//! - Declarations that conflicted in the binder (TS2300, TS2451, TS2528) are separate
//!   symbols and take no part.
//! - Two default-exported function implementations are also TS2393 at each (unless their
//!   local names clash too, which reports them already).
//! - The error is at the declaration's name, or at the start of a nameless declaration
//!   (`export default function () {}`, `export default x`).

use ts_r_ast::{Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2323 for the exports of the module file `file`.
    pub(crate) fn check_redeclared_exports(&mut self, file: FileId) {
        let f = self.c.file(file);
        if !self.reporting(file) || f.is_js() || !f.bound.is_module {
            return;
        }
        let ast = &f.ast;
        let bound = &f.bound;
        let mut errors = Vec::new();
        let mut duplicate_impls = Vec::new();
        for &(name, sym) in bound.exports() {
            let flags = bound.symbol(sym).flags;
            if flags.intersects(SymbolFlags::MODULE | SymbolFlags::ENUM) {
                continue;
            }
            let decls = bound.decls(sym);
            let overload = |d| matches!(ast.node(d), Node::FunctionDecl { body: None, .. });
            let counted = decls
                .iter()
                .filter(|&&d| !overload(d) && !matches!(ast.node(d), Node::Interface { .. }))
                .count();
            if counted < 2 || (flags.intersects(SymbolFlags::TYPE_ALIAS) && counted < 3) {
                continue;
            }
            let text = bound.name_text(ast, name).to_owned();
            if text == "default" {
                let impls: Vec<_> = decls
                    .iter()
                    .copied()
                    .filter(|&d| matches!(ast.node(d), Node::FunctionDecl { body: Some(_), .. }))
                    .collect();
                if impls.len() > 1 {
                    duplicate_impls.extend(impls);
                }
            }
            for &d in decls.iter().filter(|&&d| !overload(d)) {
                let at = match ast.node(d) {
                    Node::ExportDefaultExpr { .. } => d,
                    _ => ts_r_binder::decl_name_node(ast, d).unwrap_or(d),
                };
                errors.push((at, text.clone()));
            }
        }
        // Two default-exported function implementations are also TS2393, unless their
        // local names already clash (reported with the other functions).
        for d in duplicate_impls {
            let ast = self.ast(file);
            let name = match *ast.node(d) {
                Node::FunctionDecl { name, .. } => name,
                _ => None,
            };
            if name.is_some() && self.local_implementations(file, d) > 1 {
                continue;
            }
            self.error(
                Loc::new(file, name.unwrap_or(d)),
                dg::DUPLICATE_FUNCTION_IMPLEMENTATION_2393,
                &[],
            );
        }
        for (at, text) in errors {
            self.error(
                Loc::new(file, at),
                dg::CANNOT_REDECLARE_EXPORTED_VARIABLE_0_2323,
                &[&text],
            );
        }
    }

    /// Function implementations of the local symbol declared by `d`.
    fn local_implementations(&self, file: FileId, d: NodeId) -> usize {
        self.c.sym_of_decl(Loc::new(file, d)).map_or(0, |sym| {
            self.c
                .decls(sym)
                .into_iter()
                .filter(|l| matches!(self.node(*l), Node::FunctionDecl { body: Some(_), .. }))
                .count()
        })
    }
}
