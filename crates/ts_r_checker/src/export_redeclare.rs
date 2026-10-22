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
//!   (`export default function () {}`, `export default x`), and at the whole specifier of
//!   `export { a as x }`.
//! - A default-exported class that is not ambient, merged with default-exported functions,
//!   is TS2813 at the class and TS2814 at each function, related to the class.

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
        let mut class_merges = Vec::new();
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
                let class = decls.iter().copied().find(|&d| {
                    matches!(ast.node(d), Node::ClassDecl { .. })
                        && !self.is_ambient_node(Loc::new(file, d))
                });
                if let Some(class) = class
                    && decls
                        .iter()
                        .any(|&d| matches!(ast.node(d), Node::FunctionDecl { .. }))
                {
                    let at = |d| ts_r_binder::decl_name_node(ast, d).unwrap_or(d);
                    for &d in decls.iter() {
                        match ast.node(d) {
                            Node::ClassDecl { .. } | Node::FunctionDecl { .. } => {
                                class_merges.push((at(d), at(class), d == class));
                            }
                            _ => {}
                        }
                    }
                }
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
                    Node::ExportDefaultExpr { .. } | Node::ExportSpecifier { .. } => d,
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
        // A non-ambient default-exported class merged with default-exported functions:
        // TS2813 at the class, TS2814 at each function (tsc: checker.go
        // checkFunctionOrConstructorSymbol).
        for (at, class, is_class) in class_merges {
            let mark = self.diags.len();
            if is_class {
                self.error(
                    Loc::new(file, at),
                    dg::CLASS_DECLARATION_CANNOT_IMPLEMENT_OVERLOAD_LIST_FOR_0_2813,
                    &["default"],
                );
            } else {
                self.error(
                    Loc::new(file, at),
                    dg::FUNCTION_WITH_BODIES_CAN_ONLY_MERGE_WITH_CLASSES_THAT_ARE_AMBIENT_2814,
                    &[],
                );
            }
            self.relate_last(
                mark,
                Loc::new(file, class),
                dg::CONSIDER_ADDING_A_DECLARE_MODIFIER_TO_THIS_CLASS_6506,
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
