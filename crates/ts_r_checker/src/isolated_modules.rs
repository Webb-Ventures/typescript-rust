//! Imports and re-exports that a single-file transpiler cannot handle: the alias checks of
//! `isolatedModules` and `verbatimModuleSyntax` (tsc: checker.go checkAliasSymbol).
//!
//! Rules (in our own words; see `AGENTS.md`):
//! - Only non-type-only, non-ambient import and export specifiers, default and namespace
//!   imports and `import x = ...` of TypeScript files are checked, when `isolatedModules`
//!   (or `verbatimModuleSyntax`, which implies it) is on.
//! - An alias whose target has no value meaning, or whose chain passes a type-only
//!   import or export, is "type-like". Under `verbatimModuleSyntax` a type-like named or
//!   default import, or `import x = ...`, is TS1484 (a type), TS1485 (through a type-only
//!   declaration) or TS1288 (`import x = N.T`, an entity name). `export import x = ...` of a
//!   type is TS1269. A type-like export specifier is TS1205 (a type) or TS1448 (through a
//!   type-only declaration), except that a type-only import of the same file makes the
//!   re-export elidable unless `verbatimModuleSyntax` is on.
//! - Under `verbatimModuleSyntax`, every such alias except `import x = ...` in a file
//!   emitted as CommonJS is TS1295 (TS1286 in `.cts`/`.cjs` files), and one of an ambient
//!   `const` enum is TS2748.

use ts_r_ast::{Node, NodeId};
use ts_r_binder::program::ModuleTarget;
use ts_r_binder::{AliasTarget, ImportName, SymbolFacts, SymbolFlags};
use ts_r_diagnostics::FileId;

use ts_r_types::SymId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the aliases of `file` under `isolatedModules` / `verbatimModuleSyntax`.
    pub(crate) fn check_isolated_module_aliases(&mut self, file: FileId) {
        if !self.c.opts.isolated_modules || !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let b = self.c.bound(file);
        let ast = self.ast(file);
        let mut found: Vec<(NodeId, ts_r_binder::SymbolId)> = Vec::new();
        for a in &b.aliases {
            let sym = b.symbol(a.symbol);
            if !sym.flags.intersects(SymbolFlags::ALIAS)
                || sym
                    .facts
                    .intersects(SymbolFacts::DETACHED | SymbolFacts::TYPE_ONLY)
            {
                continue;
            }
            if matches!(
                ast.node(a.decl),
                Node::ImportEquals { .. }
                    | Node::ImportSpecifier { .. }
                    | Node::ImportDefaultSpecifier { .. }
                    | Node::ImportNamespaceSpecifier { .. }
                    | Node::ExportSpecifier { .. }
            ) {
                found.push((a.decl, a.symbol));
            }
        }
        for (decl, s) in found {
            let d = Loc::new(file, decl);
            if self.is_ambient_node(d) {
                continue;
            }
            let sym = self.c.sym(file, s);
            // A named import of a property of an `export =` value (which may be an
            // expression we do not resolve, `export = globalThis.console`) is a value.
            let target = match self.c.resolve_alias(sym) {
                Some(t) => Some(t),
                None if self.imports_from_export_equals(sym) => None,
                None => continue,
            };
            self.check_isolated_module_alias(d, sym, target);
        }
    }

    /// Whether `s` is a named import of a module with `export =`.
    fn imports_from_export_equals(&self, s: SymId) -> bool {
        let Some(r) = self.c.parts(s).next() else {
            return false;
        };
        let f = self.c.file(r.file);
        let Some(info) = f.bound.alias(r.symbol) else {
            return false;
        };
        let AliasTarget::Module {
            request,
            import: ImportName::Named(_),
        } = info.target
        else {
            return false;
        };
        let t = self.c.link.module_target(r.file, request);
        self.c.link.resolve_export(t, "export=").is_some()
    }

    /// `target` is `None` for a value we cannot resolve (see `imports_from_export_equals`).
    fn check_isolated_module_alias(&mut self, d: Loc, sym: SymId, target: Option<SymId>) {
        let file = d.file;
        let verbatim = self.c.opts.verbatim_module_syntax;
        let flag = self.isolated_modules_flag_name();
        let target_flags = target.map_or(SymbolFlags::VARIABLE, |t| self.c.sym_flags(t));
        let is_type = !target_flags.intersects(SymbolFlags::VALUE);
        let type_only_file = self.type_only_alias_file(sym);
        let ast = self.ast(file);
        // Default and namespace imports report at their name.
        let at = match *ast.node(d.node) {
            Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local } => {
                local
            }
            _ => d.node,
        };
        let at = Loc::new(file, at);
        let is_import_equals = matches!(ast.node(d.node), Node::ImportEquals { .. });
        if is_type || type_only_file.is_some() {
            match *ast.node(d.node) {
                Node::ImportSpecifier { .. }
                | Node::ImportDefaultSpecifier { .. }
                | Node::ImportEquals { .. } => {
                    let name = self.alias_source_name(d);
                    if verbatim {
                        let internal = matches!(
                            *ast.node(d.node),
                            Node::ImportEquals { module_ref, .. }
                                if !matches!(ast.node(module_ref), Node::ExternalModuleRef { .. })
                        );
                        let info = if internal {
                            dg::AN_IMPORT_ALIAS_CANNOT_RESOLVE_TO_A_TYPE_OR_TYPE_ONLY_DECLARATION_WHEN_VERBATIMM_1288
                        } else if is_type {
                            dg::_0_IS_A_TYPE_AND_MUST_BE_IMPORTED_USING_A_TYPE_ONLY_IMPORT_WHEN_VERBATIMMODULESYN_1484
                        } else {
                            dg::_0_RESOLVES_TO_A_TYPE_ONLY_DECLARATION_AND_MUST_BE_IMPORTED_USING_A_TYPE_ONLY_IMP_1485
                        };
                        self.error(at, info, &[&name]);
                    }
                    if is_type
                        && is_import_equals
                        && ast
                            .node(d.node)
                            .modifiers()
                            .contains(ts_r_ast::Modifiers::EXPORT)
                    {
                        self.error(
                            at,
                            dg::CANNOT_USE_EXPORT_IMPORT_ON_A_TYPE_OR_TYPE_ONLY_NAMESPACE_WHEN_0_IS_ENABLED_1269,
                            &[flag],
                        );
                    }
                }
                // `import type { A } from "./a"; export { A }` is elided by a single-file
                // transpiler, so only a type-only declaration elsewhere is an error.
                Node::ExportSpecifier { .. } if verbatim || type_only_file != Some(file) => {
                    let name = self.alias_source_name(d);
                    if is_type {
                        self.error(
                            at,
                            dg::RE_EXPORTING_A_TYPE_WHEN_0_IS_ENABLED_REQUIRES_USING_EXPORT_TYPE_1205,
                            &[flag],
                        );
                    } else {
                        self.error(
                            at,
                            dg::_0_RESOLVES_TO_A_TYPE_ONLY_DECLARATION_AND_MUST_BE_RE_EXPORTED_USING_A_TYPE_ONLY_R_1448,
                            &[&name, flag],
                        );
                    }
                }
                _ => {}
            }
        }
        if verbatim && !is_import_equals && self.emits_commonjs(file) {
            let info = self.verbatim_commonjs_message(file);
            self.error(at, info, &[]);
        }
        if verbatim
            && target_flags.intersects(SymbolFlags::CONST_ENUM)
            && self
                .c
                .value_decl(target.unwrap_or(sym))
                .is_some_and(|v| self.is_ambient_node(v))
        {
            self.error(
                at,
                dg::CANNOT_ACCESS_AMBIENT_CONST_ENUMS_WHEN_0_IS_ENABLED_2748,
                &[flag],
            );
        }
    }

    /// `export default X` / `export = X` under `isolatedModules` / `verbatimModuleSyntax`
    /// (tsc: checker.go checkExportAssignment): `export default` in a file emitted as
    /// CommonJS is TS1295/TS1286 under `verbatimModuleSyntax`, and nothing else is checked.
    /// Otherwise, for an identifier: under `verbatimModuleSyntax` it must name a value
    /// (TS1282/TS1284) that is not reached through a type-only declaration
    /// (TS1283/TS1285); under `isolatedModules`, an import of a type or of a type-only
    /// declaration elsewhere must be marked type-only first (TS1291/TS1292, TS1289/TS1290).
    pub(crate) fn check_isolated_export_assignment(&mut self, l: Loc, expr: NodeId) {
        let file = l.file;
        if !self.c.opts.isolated_modules
            || !self.reporting(file)
            || self.c.file(file).is_js()
            || self.is_ambient_node(l)
        {
            return;
        }
        let ast = self.ast(file);
        let export_equals = matches!(ast.node(l.node), Node::ExportAssignment { .. });
        let verbatim = self.c.opts.verbatim_module_syntax;
        if !export_equals && verbatim && self.emits_commonjs(file) {
            let info = self.verbatim_commonjs_message(file);
            self.error(l, info, &[]);
            return;
        }
        if !matches!(ast.node(expr), Node::Ident { .. }) {
            return;
        }
        let id = Loc::new(file, expr);
        let Some(sym) = self.c.resolve_ident(
            id,
            ts_r_binder::meaning::VALUE
                | ts_r_binder::meaning::TYPE
                | ts_r_binder::meaning::NAMESPACE,
        ) else {
            return;
        };
        let own = self.c.sym_flags(sym);
        let is_alias = own.intersects(SymbolFlags::ALIAS);
        let target_flags = if is_alias {
            self.c
                .resolve_alias(sym)
                .map_or(SymbolFlags::NONE, |t| self.c.sym_flags(t))
        } else {
            SymbolFlags::NONE
        };
        let type_only = if is_alias {
            self.type_only_alias_file(sym)
        } else {
            None
        };
        let name = crate::nodes::key_text(ast, expr).unwrap_or_default();
        let flag = self.isolated_modules_flag_name();
        if (own.without(SymbolFlags::ALIAS) | target_flags).intersects(SymbolFlags::VALUE) {
            // A local value merged with the alias is what gets exported.
            if verbatim
                && type_only.is_some()
                && !own
                    .without(SymbolFlags::ALIAS)
                    .intersects(SymbolFlags::VALUE)
            {
                let info = if export_equals {
                    dg::AN_EXPORT_DECLARATION_MUST_REFERENCE_A_REAL_VALUE_WHEN_VERBATIMMODULESYNTAX_IS_E_1283
                } else {
                    dg::AN_EXPORT_DEFAULT_MUST_REFERENCE_A_REAL_VALUE_WHEN_VERBATIMMODULESYNTAX_IS_ENABL_1285
                };
                self.error(id, info, &[&name]);
            }
        } else if verbatim {
            let info = if export_equals {
                dg::AN_EXPORT_DECLARATION_MUST_REFERENCE_A_VALUE_WHEN_VERBATIMMODULESYNTAX_IS_ENABLE_1282
            } else {
                dg::AN_EXPORT_DEFAULT_MUST_REFERENCE_A_VALUE_WHEN_VERBATIMMODULESYNTAX_IS_ENABLED_BU_1284
            };
            self.error(id, info, &[&name]);
        }
        if own.intersects(SymbolFlags::VALUE) {
            return;
        }
        if is_alias
            && target_flags.intersects(SymbolFlags::TYPE)
            && !target_flags.intersects(SymbolFlags::VALUE)
            && type_only != Some(file)
        {
            let info = if export_equals {
                dg::_0_RESOLVES_TO_A_TYPE_AND_MUST_BE_MARKED_TYPE_ONLY_IN_THIS_FILE_BEFORE_RE_EXPORTI_1291
            } else {
                dg::_0_RESOLVES_TO_A_TYPE_AND_MUST_BE_MARKED_TYPE_ONLY_IN_THIS_FILE_BEFORE_RE_EXPORTI_1292
            };
            self.error(id, info, &[&name, flag]);
        } else if type_only.is_some_and(|f| f != file) {
            let info = if export_equals {
                dg::_0_RESOLVES_TO_A_TYPE_ONLY_DECLARATION_AND_MUST_BE_MARKED_TYPE_ONLY_IN_THIS_FILE_B_1289
            } else {
                dg::_0_RESOLVES_TO_A_TYPE_ONLY_DECLARATION_AND_MUST_BE_MARKED_TYPE_ONLY_IN_THIS_FILE_B_1290
            };
            self.error(id, info, &[&name, flag]);
        }
    }

    /// Under `verbatimModuleSyntax`, a top-level `export namespace` with values in a file
    /// emitted as CommonJS is TS1287 at `export` (tsc: checker.go checkModuleDeclaration).
    /// Under `isolatedModules`, an instantiated non-ambient namespace in a global script is
    /// TS1280 at its name (each part of a dotted name): namespaces of scripts merge across
    /// files, and a transpiler cannot see the other files' members (tsc: checker.go
    /// checkModuleDeclaration). `isolatedModules` preserves const enums, so one of those
    /// makes the namespace instantiated too.
    pub(crate) fn check_isolated_script_namespaces(&mut self, file: FileId) {
        if !self.c.opts.isolated_modules
            || !self.reporting(file)
            || self.c.bound(file).is_module
            || self.c.file(file).is_js()
        {
            return;
        }
        let ast = self.ast(file);
        let mut at = Vec::new();
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 1_000_000 {
                break;
            }
            match *ast.node(n) {
                Node::Module {
                    kind: ts_r_ast::ModuleKind::Namespace | ts_r_ast::ModuleKind::Module,
                    name,
                    body: Some(b),
                    ..
                } => {
                    if !matches!(ast.node(name), Node::StringLit { .. })
                        && instantiated_with_const_enums(ast, b, 0)
                        && !self.is_ambient_node(Loc::new(file, n))
                    {
                        let mut cur = name;
                        let mut guard = 0;
                        while let Node::QualifiedName { left, right } = *ast.node(cur) {
                            at.push(right);
                            cur = left;
                            guard += 1;
                            if guard > 10_000 {
                                break;
                            }
                        }
                        if matches!(ast.node(cur), Node::Ident { .. }) {
                            at.push(cur);
                        }
                    }
                    stack.push(b);
                }
                Node::Program { stmts } | Node::ModuleBlock { stmts } => {
                    stack.extend(ast.list(stmts).iter().copied());
                }
                _ => {}
            }
        }
        at.sort_unstable();
        let flag = self.isolated_modules_flag_name();
        for n in at {
            self.error(
                Loc::new(file, n),
                dg::NAMESPACES_ARE_NOT_ALLOWED_IN_GLOBAL_SCRIPT_FILES_WHEN_0_IS_ENABLED_IF_THIS_FILE_1280,
                &[flag],
            );
        }
    }

    /// Under `isolatedModules`, a bare reference in a non-ambient enum to a member declared
    /// by the same enum in another file is TS1281: a transpiler of this file cannot know the
    /// name is a member (tsc: binder/nameresolver.go, enum scopes).
    pub(crate) fn check_cross_file_enum_member(&mut self, e: Loc, s: SymId, name: &str) {
        if !self.c.opts.isolated_modules
            || !self.c.sym_flags(s).intersects(SymbolFlags::ENUM_MEMBER)
            || self.c.value_decl(s).is_none_or(|d| d.file == e.file)
        {
            return;
        }
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 100_000 {
                return;
            }
            if let Node::Enum { .. } = ast.node(n) {
                break;
            }
            cur = parents.parent(n);
        }
        let Some(en) = cur else { return };
        let el = Loc::new(e.file, en);
        if self.is_ambient_node(el) {
            return;
        }
        let Some(es) = self.c.sym_of_decl(el) else {
            return;
        };
        let qualified = format!("{}.{name}", self.c.sym_name(es));
        let flag = self.isolated_modules_flag_name();
        self.error(
            e,
            dg::CANNOT_ACCESS_0_FROM_ANOTHER_FILE_WITHOUT_QUALIFICATION_WHEN_1_IS_ENABLED_USE_2_I_1281,
            &[name, flag, &qualified],
        );
    }

    pub(crate) fn check_verbatim_commonjs_namespaces(&mut self, file: FileId) {
        if !self.c.opts.verbatim_module_syntax
            || !self.reporting(file)
            || !self.emits_commonjs(file)
        {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let mut at = Vec::new();
        for &st in ast.list(stmts) {
            if let Node::Module {
                modifiers,
                kind: ts_r_ast::ModuleKind::Namespace | ts_r_ast::ModuleKind::Module,
                name,
                body: Some(b),
            } = *ast.node(st)
                && modifiers.contains(ts_r_ast::Modifiers::EXPORT)
                && !matches!(ast.node(name), Node::StringLit { .. })
                && crate::merge_order::namespace_instantiated(ast, b, 0)
                && !self.is_ambient_node(Loc::new(file, st))
                && let Some(t) = ast
                    .modifier_tokens_of(st)
                    .iter()
                    .find(|t| t.flag == ts_r_ast::Modifiers::EXPORT)
            {
                at.push(t.span);
            }
        }
        for span in at {
            self.error_span(
                file,
                span,
                dg::A_TOP_LEVEL_EXPORT_MODIFIER_CANNOT_BE_USED_ON_VALUE_DECLARATIONS_IN_A_COMMONJS_M_1287,
                &[],
            );
        }
    }

    /// Under explicit `isolatedModules`, a value use in a module of a global (`new Date()`)
    /// that the file also imports as a type (`import { Date } from "./types"` of an
    /// interface) is TS2866 at the import: a single-file transpiler would keep the import
    /// and shadow the global (tsc: checker.go resolveNameHelper).
    pub(crate) fn check_global_shadowed_by_type_import(&mut self, e: Loc, s: SymId, name: &str) {
        let file = e.file;
        if !self.c.opts.isolated_modules_explicit
            || !self.c.bound(file).is_module
            || !self.reporting(file)
            || self.c.global_symbol(name, ts_r_binder::meaning::VALUE) != Some(s)
        {
            return;
        }
        let b = self.c.bound(file);
        let ast = self.ast(file);
        let Some(local) = b.lookup_str(ast, b.scopes[0].locals, name) else {
            return;
        };
        let l = self.c.sym(file, local);
        if !self.c.alias_lacks_meaning(l, ts_r_binder::meaning::VALUE)
            || b.symbol(local).facts.intersects(SymbolFacts::TYPE_ONLY)
        {
            return;
        }
        let Some(&d) = b.decls(local).iter().find(|&&d| {
            matches!(
                ast.node(d),
                Node::ImportSpecifier { .. }
                    | Node::ImportDefaultSpecifier { .. }
                    | Node::ImportNamespaceSpecifier { .. }
                    | Node::ImportEquals { .. }
            )
        }) else {
            return;
        };
        let at = match *ast.node(d) {
            Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local } => {
                local
            }
            _ => d,
        };
        // Each use reports it again; the driver drops the duplicates.
        self.error(
            Loc::new(file, at),
            dg::IMPORT_0_CONFLICTS_WITH_GLOBAL_VALUE_USED_IN_THIS_FILE_SO_MUST_BE_DECLARED_WITH_A_2866,
            &[name],
        );
    }

    /// TS1286 in `.cts`/`.cjs` files, else TS1295.
    pub(crate) fn verbatim_commonjs_message(&self, file: FileId) -> dg::DiagnosticInfo {
        let path = self.c.file(file).path.to_ascii_lowercase();
        if path.ends_with(".cts") || path.ends_with(".cjs") {
            dg::ECMASCRIPT_IMPORTS_AND_EXPORTS_CANNOT_BE_WRITTEN_IN_A_COMMONJS_FILE_UNDER_VERBAT_1286
        } else {
            dg::ECMASCRIPT_IMPORTS_AND_EXPORTS_CANNOT_BE_WRITTEN_IN_A_COMMONJS_FILE_UNDER_VERBAT_1295
        }
    }

    /// The name an import or export specifier takes from its module or entity (`a` in
    /// `{ a as b }`); the local name of default imports and `import x = ...`.
    fn alias_source_name(&self, d: Loc) -> String {
        let ast = self.ast(d.file);
        let n = match *ast.node(d.node) {
            Node::ImportSpecifier {
                imported, local, ..
            } => imported.unwrap_or(local),
            Node::ExportSpecifier { local, .. } => local,
            Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local } => {
                local
            }
            Node::ImportEquals { name, .. } => name,
            _ => return String::new(),
        };
        crate::nodes::key_text(ast, n).unwrap_or_default()
    }

    /// The file of the first type-only import or export on the alias chain of `s`
    /// (including a name re-exported through `export type *`).
    pub(crate) fn type_only_alias_file(&self, s: SymId) -> Option<FileId> {
        self.type_only_alias(s).map(|(f, _)| f)
    }

    /// Like `type_only_alias_file`, with whether that link is an export (`export type`,
    /// `export type *`) rather than an import.
    pub(crate) fn type_only_alias(&self, s: SymId) -> Option<(FileId, bool)> {
        let r = self.c.parts(s).next()?;
        for a in self.c.link.alias_chain(r) {
            let f = self.c.file(a.file);
            if f.bound
                .symbol(a.symbol)
                .facts
                .intersects(SymbolFacts::TYPE_ONLY)
            {
                let exported = f.bound.decls(a.symbol).first().is_some_and(|&d| {
                    matches!(
                        f.ast.node(d),
                        Node::ExportSpecifier { .. }
                            | Node::ExportNamed { .. }
                            | Node::ExportAll { .. }
                    )
                });
                return Some((a.file, exported));
            }
            if let Some(info) = f.bound.alias(a.symbol)
                && let AliasTarget::Module {
                    request,
                    import: ImportName::Named(n),
                } = info.target
            {
                let text = f.bound.name_text(&f.ast, n);
                let t = self.c.link.module_target(a.file, request);
                if self.c.link.star_export_is_type_only(t, text) {
                    let m = match t {
                        ModuleTarget::File(m) => m,
                        _ => a.file,
                    };
                    return Some((m, true));
                }
            }
        }
        None
    }
}

/// Whether a namespace body declares values, counting const enums (preserved under
/// `isolatedModules`).
fn instantiated_with_const_enums(ast: &ts_r_ast::Ast, body: NodeId, depth: u32) -> bool {
    if depth > 256 {
        return true;
    }
    let stmts = match *ast.node(body) {
        Node::ModuleBlock { stmts } => stmts,
        Node::Module { body: Some(b), .. } => {
            return instantiated_with_const_enums(ast, b, depth + 1);
        }
        _ => return false,
    };
    ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::Interface { .. } | Node::TypeAlias { .. } | Node::ImportDecl { .. } => false,
        Node::ImportEquals { type_only, .. } => !type_only,
        Node::Module { body: Some(b), .. } => instantiated_with_const_enums(ast, b, depth + 1),
        Node::Module { body: None, .. } => false,
        _ => true,
    })
}
