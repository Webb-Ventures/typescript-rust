//! `export { x }` without a module specifier (TS2661, TS2304), and imports and exports in
//! module augmentations (TS2666, TS2667, TS2670).
//!
//! Rules observed from tsc 7.0.2, for the specifiers of a local export in a module file
//! (including its `declare global` blocks) or an ambient module declaration:
//! - The local name is resolved with every meaning. A name that resolves nowhere is
//!   TS2304/TS2552, except the intrinsic type names `any`, `unknown`, `string`,
//!   `number`, `boolean`, `never` and `undefined`, which are TS2661.
//! - A name that resolves to a declaration outside the exporting module (a global, lib
//!   or `globalThis`, or a top-level declaration of the file around `declare module "m"`)
//!   is TS2661 at the local name.

use ts_r_ast::{ModuleKind, Node, NodeId};
use ts_r_binder::meaning;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Names tsc treats as declared but not local (`globalThis` is not a binder symbol here).
const INTRINSIC: [&str; 8] = [
    "any",
    "unknown",
    "string",
    "number",
    "boolean",
    "never",
    "undefined",
    "globalThis",
];

/// Parent steps from an export to its container.
const MAX_STEPS: u32 = 4096;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the specifiers of `export { ... }` statement `l` (no `from` clause).
    pub(crate) fn check_local_export_specifiers(&mut self, l: Loc) {
        if !self.reporting(l.file) {
            return;
        }
        let ast = self.ast(l.file);
        let Node::ExportNamed {
            specifiers,
            source: None,
            ..
        } = *ast.node(l.node)
        else {
            return;
        };
        let Some((container, own_global)) = self.export_container(l) else {
            return;
        };
        for &s in ast.list(specifiers) {
            let Node::ExportSpecifier { local, .. } = *ast.node(s) else {
                continue;
            };
            let Node::Ident { name } = *ast.node(local) else {
                continue;
            };
            let text = ast.atom(name);
            let nl = Loc::new(l.file, local);
            let local_decl = match self.c.resolve_ident(nl, meaning::ALL) {
                // JSDoc `@typedef`s are not binder symbols (left to the JS checks).
                None if self.c.file(l.file).is_js() => continue,
                None if !INTRINSIC.contains(&text) => {
                    self.report_cannot_find_name(nl, text);
                    continue;
                }
                None => false,
                Some(sym) => self.c.decls(sym).iter().any(|d| {
                    d.file == l.file && self.within(d.node, container, own_global, l.file)
                }),
            };
            if !local_decl {
                self.error(
                    nl,
                    dg::CANNOT_EXPORT_0_ONLY_LOCAL_DECLARATIONS_CAN_BE_EXPORTED_FROM_A_MODULE_2661,
                    &[text],
                );
            }
        }
    }

    /// TS2667 for an import and TS2666 for an export directly inside `declare global` or a
    /// module augmentation (`declare module "x"` in a module file, when `"x"` resolves), at
    /// the statement. `import a = N.b` is fine.
    pub(crate) fn check_augmentation_statement(&mut self, l: Loc) {
        if !self.reporting(l.file) || !self.c.file(l.file).bound.is_module {
            return;
        }
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let Some(block) = parents.parent(l.node) else {
            return;
        };
        let Some(Node::Module { kind, name, .. }) = parents.parent(block).map(|m| ast.node(m))
        else {
            return;
        };
        if *kind != ModuleKind::Global {
            // An augmentation of a module that cannot be found (TS2664) is not checked.
            let name = *name;
            if !matches!(ast.node(name), Node::StringLit { .. }) {
                return;
            }
            let resolved = self
                .c
                .bound(l.file)
                .module_requests
                .iter()
                .position(|r| r.node == name)
                .is_some_and(|i| {
                    self.c.link.module_target(l.file, i as u32)
                        != ts_r_binder::program::ModuleTarget::Unresolved
                });
            if !resolved {
                return;
            }
        }
        let info = match *ast.node(l.node) {
            Node::ImportDecl { .. } => {
                dg::IMPORTS_ARE_NOT_PERMITTED_IN_MODULE_AUGMENTATIONS_CONSIDER_MOVING_THEM_TO_THE_EN_2667
            }
            Node::ImportEquals { module_ref, .. }
                if matches!(ast.node(module_ref), Node::ExternalModuleRef { .. }) =>
            {
                dg::IMPORTS_ARE_NOT_PERMITTED_IN_MODULE_AUGMENTATIONS_CONSIDER_MOVING_THEM_TO_THE_EN_2667
            }
            Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. } => {
                dg::EXPORTS_AND_EXPORT_ASSIGNMENTS_ARE_NOT_PERMITTED_IN_MODULE_AUGMENTATIONS_2666
            }
            _ => return,
        };
        self.error(l, info, &[]);
    }

    /// TS2670 at the name of a `global { }` block that is neither `declare` nor in an
    /// ambient context (after the binder's placement TS2669).
    pub(crate) fn check_global_augmentation_declare(&mut self, l: Loc) {
        let Node::Module {
            kind: ModuleKind::Global,
            name,
            ..
        } = *self.ast(l.file).node(l.node)
        else {
            return;
        };
        if self.reporting(l.file) && !self.is_ambient_node(l) {
            self.error(
                Loc::new(l.file, name),
                dg::AUGMENTATIONS_FOR_THE_GLOBAL_SCOPE_SHOULD_HAVE_DECLARE_MODIFIER_UNLESS_THEY_APPE_2670,
                &[],
            );
        }
    }

    /// The node an export statement exports from: the innermost ambient module around
    /// it, else the root of a module file (also from `declare global`). `None` inside
    /// namespaces and in scripts.
    /// Also the `declare global` block the export is in, whose declarations count as local.
    fn export_container(&self, l: Loc) -> Option<(NodeId, Option<NodeId>)> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut own_global = None;
        let mut cur = parents.parent(l.node);
        for _ in 0..MAX_STEPS {
            let n = cur?;
            match *ast.node(n) {
                // `declare global` exports the declarations of the module around it.
                Node::Module {
                    kind: ModuleKind::Global,
                    ..
                } => own_global = own_global.or(Some(n)),
                Node::Module { name, .. } => {
                    return matches!(ast.node(name), Node::StringLit { .. })
                        .then_some((n, own_global));
                }
                Node::Program { .. } => {
                    return self
                        .c
                        .file(l.file)
                        .bound
                        .is_module
                        .then_some((n, own_global));
                }
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    /// Whether `node` is `container` or inside it, without crossing a `declare global`
    /// (whose declarations are global) other than `own_global`.
    fn within(
        &self,
        node: NodeId,
        container: NodeId,
        own_global: Option<NodeId>,
        file: ts_r_diagnostics::FileId,
    ) -> bool {
        let ast = self.ast(file);
        let parents = self.c.file(file).parents();
        let mut cur = Some(node);
        for _ in 0..MAX_STEPS {
            let Some(n) = cur else {
                return false;
            };
            if n == container {
                return true;
            }
            if let Node::Module {
                kind: ModuleKind::Global,
                ..
            } = *ast.node(n)
                && own_global != Some(n)
            {
                return false;
            }
            cur = parents.parent(n);
        }
        false
    }
}
