//! Imports that share a name with a local declaration (TS2440).
//!
//! Rules observed from tsc 7.0.2:
//! - An import (`import x = ...`, `import {x}`, default and namespace imports) whose target
//!   has a meaning (value, type, namespace) that a local declaration of the same name in
//!   the same block also has: TS2440 at the import (the `import ... =` statement, the
//!   specifier, or the name of a default or namespace import), in either order. A value import next to an interface, or a type import
//!   next to a variable, is fine.
//! - Exported and local declarations count together when they are in the same block
//!   (`export var r; import r = m.v;`); an exported declaration of another block of the
//!   same namespace does not conflict with a local import, but two exported ones do (they
//!   are one symbol).

use ts_r_ast::Node;
use ts_r_binder::{SymbolFacts, SymbolFlags, SymbolId, meaning};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Meaning bits (`meaning::VALUE | TYPE | NAMESPACE`) of declarations with `flags`.
fn meanings(flags: SymbolFlags) -> u8 {
    let mut m = 0;
    if flags.intersects(SymbolFlags::VALUE) {
        m |= meaning::VALUE;
    }
    if flags.intersects(SymbolFlags::TYPE) {
        m |= meaning::TYPE;
    }
    if flags.intersects(SymbolFlags::NAMESPACE) {
        m |= meaning::NAMESPACE;
    }
    m
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the imports of `file` against local declarations of the same name.
    pub(crate) fn check_import_conflicts(&mut self, file: FileId) {
        if !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let b = self.c.bound(file);
        let ast = self.ast(file);
        let mut found: Vec<(ts_r_ast::NodeId, SymbolId, u8)> = Vec::new();
        for a in &b.aliases {
            let sym = b.symbol(a.symbol);
            let own = sym.flags;
            if !own.intersects(SymbolFlags::ALIAS) || sym.facts.intersects(SymbolFacts::DETACHED) {
                continue;
            }
            if !matches!(
                ast.node(a.decl),
                Node::ImportEquals { .. }
                    | Node::ImportSpecifier { .. }
                    | Node::ImportDefaultSpecifier { .. }
                    | Node::ImportNamespaceSpecifier { .. }
            ) {
                continue;
            }
            let mut local = own.without(SymbolFlags::ALIAS);
            // The block the import is in, and its declarations in the other table.
            let block = self.declaring_block(Loc::new(file, a.decl));
            let partner = block.and_then(|blk| {
                let sc = b.scope_of(blk).or_else(|| {
                    matches!(ast.node(blk), Node::Program { .. }).then_some(ts_r_binder::ScopeId(0))
                })?;
                let scope = &b.scopes[sc.0 as usize];
                let table = if sym.facts.intersects(SymbolFacts::EXPORTED) {
                    scope.locals
                } else {
                    b.symbol(scope.symbol?).exports
                };
                let p = b.lookup(table, sym.name)?;
                (p != a.symbol).then_some((p, blk))
            });
            if let Some((p, blk)) = partner {
                let ps = b.symbol(p);
                let same_block = b
                    .decls(p)
                    .iter()
                    .any(|&d| self.declaring_block(Loc::new(file, d)) == Some(blk));
                if same_block && !ps.flags.intersects(SymbolFlags::ALIAS) {
                    local |= ps.flags;
                }
            }
            let local = meanings(local);
            if local == 0 {
                continue;
            }
            found.push((a.decl, a.symbol, local));
        }
        for (decl, s, local) in found {
            let sym = self.c.sym(file, s);
            let Some(target) = self.c.resolve_alias(sym) else {
                continue;
            };
            if meanings(self.c.sym_flags(target)) & local == 0 {
                continue;
            }
            let name = self.c.sym_name(sym).to_owned();
            // Default and namespace imports report at the name.
            let at = match *self.ast(file).node(decl) {
                Node::ImportDefaultSpecifier { local }
                | Node::ImportNamespaceSpecifier { local } => local,
                _ => decl,
            };
            self.error(
                Loc::new(file, at),
                dg::IMPORT_DECLARATION_CONFLICTS_WITH_LOCAL_DECLARATION_OF_0_2440,
                &[&name],
            );
        }
    }

    /// The block (`Program` or namespace body) declaring `d`.
    fn declaring_block(&self, d: Loc) -> Option<ts_r_ast::NodeId> {
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let mut cur = parents.parent(d.node);
        while let Some(n) = cur {
            if matches!(ast.node(n), Node::Program { .. } | Node::ModuleBlock { .. }) {
                return Some(n);
            }
            cur = parents.parent(n);
        }
        None
    }
}
