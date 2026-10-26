//! Renames through import and export specifiers that keep their name (`import { x }`,
//! `export { x }`, `export { x } from "m"`), as tsc 7 does with prefix and suffix text
//! (observed on tsc 7.0.2's answers to `findAllRefsReExportLocal`, `renameImportOfReExport`,
//! `renameImportAndExport*`, `transitiveExportImports*`, `renameExportSpecifier2` and
//! `findAllRefsReExportsUseInImportType`; `tsc: tsc/internal/ls/rename.go` and
//! `findallreferences.go` explain them).
//!
//! A rename changes one name, so such a specifier is where it stops: the specifier is kept
//! working by spelling out both sides.
//!
//! - Renaming at a specifier (or a use of an import) renames only that alias: the
//!   specifier becomes `x as new`, and the alias's uses (for an import) or the modules that
//!   import the export (for an export) follow.
//! - Renaming a symbol leaves the names it is exported under: an export specifier of it
//!   becomes `new as x`. An import specifier that imports the symbol itself (a declaration
//!   exported by its own name) is renamed as it is, with its uses and, as above, its
//!   re-exports.
//!
//! The rename symbol `R` is the symbol at the cursor before aliases are followed. A name
//! whose symbol's alias chain is `s0, s1, ...` is renamed when the chain reaches `R` as
//! `[R, ..]`, `[A, R, ..]`, `[E, R, ..]` or `[E, A, R, ..]`, where `A` is a name-keeping
//! import specifier and `E` a name-keeping export specifier written at that name (the
//! suffix case). Every other name the search found through aliases is left alone.

use ts_r_ast::Node;
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_types::SymId;

use super::locate::{meaning_at, node_at};

/// How one occurrence is renamed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Decor {
    /// The new name.
    Plain,
    /// `old as new`: the specifier of the renamed alias keeps what it imports or exports.
    Prefix,
    /// `new as old`: an export specifier keeps the name it exports.
    Suffix,
}

/// The name-keeping specifier kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spec {
    Import,
    Export,
}

/// The kind of `s` when it is an alias declared by a specifier that keeps its name.
/// (`import { x as x }` counts as keeping).
fn keeping_spec(c: &Checker<'_>, s: SymId) -> Option<(Spec, Loc)> {
    if !super::aliases::is_alias(c, s) {
        return None;
    }
    let same = |a: ts_r_ast::NodeId, b: ts_r_ast::NodeId, f| {
        super::locate::name_text(c.ast(f), a)
            .is_some_and(|t| super::locate::name_text(c.ast(f), b) == Some(t))
    };
    c.decls(s).into_iter().find_map(|d| match *c.node(d) {
        Node::ImportSpecifier { imported: None, .. } => Some((Spec::Import, d)),
        Node::ImportSpecifier {
            imported: Some(i),
            local,
            ..
        } if same(i, local, d.file) => Some((Spec::Import, d)),
        Node::ExportSpecifier { exported: None, .. } => Some((Spec::Export, d)),
        _ => None,
    })
}

/// The symbol a rename at name `l` starts from: the alias itself when `l` names one (the
/// variable of a shorthand property when `variable` is set).
pub(super) fn rename_symbol(ck: &mut Ck<'_, '_>, l: Loc, variable: bool) -> Option<SymId> {
    let c = ck.c;
    if let Some(s) = super::aliases::imported_side(c, l) {
        return Some(s);
    }
    let m = meaning_at(ck, l);
    if variable {
        m.shorthand.or(m.primary)
    } else {
        m.primary.or(m.shorthand)
    }
}

/// Whether name `l` is the name written in specifier `spec`.
fn is_spec_name(c: &Checker<'_>, spec: Loc, l: Loc) -> bool {
    spec.file == l.file && c.parent(l) == Some(spec)
}

/// How the name at `[start, end)` of `file` is renamed when renaming `r`; `None` when it
/// is left alone. Names that are not identifiers, or whose symbol is not on an alias chain
/// with `r` (inherited members, contextual properties), are renamed as found.
pub(super) fn decor(
    ck: &mut Ck<'_, '_>,
    r: SymId,
    file: ts_r_diagnostics::FileId,
    (start, end): (u32, u32),
    variable: bool,
) -> Option<Decor> {
    let c = ck.c;
    let ast = c.ast(file);
    let Some(n) = node_at(ast, start).filter(|&n| {
        let s = ast.span(n);
        s.start == start && s.end == end && matches!(ast.node(n), Node::Ident { .. })
    }) else {
        return Some(Decor::Plain);
    };
    let l = Loc::new(file, n);
    let Some(s) = rename_symbol(ck, l, variable) else {
        return Some(Decor::Plain);
    };
    let chain = super::aliases::chain(c, s);
    if !chain.contains(&r) {
        // What `r` stands for (the declaration behind a renamed import) is left alone;
        // anything else was found another way (an inherited or contextual member).
        return (!super::aliases::chain(c, r).contains(&s)).then_some(Decor::Plain);
    }
    let mut i = 0;
    let mut out = Decor::Plain;
    if chain[0] == r {
        if let Some((_, spec)) = keeping_spec(c, r)
            && is_spec_name(c, spec, l)
            && matches!(
                *c.node(spec),
                Node::ImportSpecifier { imported: None, .. }
                    | Node::ExportSpecifier { exported: None, .. }
            )
        {
            out = Decor::Prefix;
        }
        return Some(out);
    }
    if let Some((Spec::Export, spec)) = keeping_spec(c, chain[0]) {
        if !is_spec_name(c, spec, l) {
            return None;
        }
        out = Decor::Suffix;
        i = 1;
    }
    // Aliases that give their own name (`import { a as x }`, `import x from`, `export
    // default x`) are crossed: renaming their local name keeps them working. A name-keeping
    // import is crossed only into `r` itself (it imports the name being renamed); a
    // name-keeping export never is.
    for _ in 0..64 {
        let &x = chain.get(i)?;
        if x == r {
            return Some(out);
        }
        match keeping_spec(c, x) {
            None if super::aliases::is_alias(c, x) => {}
            Some((Spec::Import, _)) if chain.get(i + 1) == Some(&r) => {}
            _ => return None,
        }
        i += 1;
    }
    None
}
