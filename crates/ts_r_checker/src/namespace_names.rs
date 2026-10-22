//! Namespace parts of entity names (`A.B` in `x: A.B.C` and in `import x = A.B.C`) that
//! resolve to something without a namespace meaning, and the entity of `import x = A.B`.
//!
//! Rules observed from tsc 7.0.2:
//! - A namespace part is looked up with namespace meaning only: a value that shadows an
//!   outer namespace of the same name is skipped.
//! - A lone identifier naming a type (interface, type alias, class) is TS2713 (`Cannot
//!   access 'T.q' because 'T' is a type ...`) when the type has the property accessed on
//!   it, TS2702 (`only refers to a type`) otherwise, in type names and import-equals
//!   entities alike (`C.s` for a static `s` is TS2702). One naming only a value (variable,
//!   function) is TS2503 `Cannot find namespace`.
//! - A qualified namespace part whose last member is a type or class is TS2713 at the name
//!   accessed on it, whatever its properties; an enum member reached from a lone
//!   identifier (`Color.Red.x`, not `N.E.A.x`) is TS2749 for the whole name. One whose
//!   member is only a value is TS2694 on that member, naming its parent.
//! - TS2694 names the namespace by its symbol when it was reached through an alias
//!   (`import i = NI; x: i.Z` says `Namespace 'NI'`), and as written otherwise.
//! - `import x = A.B.C` checks its entity: TS2503 for a missing first name, TS2694 for a
//!   missing member (the last member may have any meaning), `globalThis` alone is fine.
//! - When the entity is a value, its first name looked up as value or namespace must be a
//!   namespace (or enum), else a local value hides it (TS2437 at that name); when it is a
//!   type, the alias may not be named like a predefined type (TS2438). An exported alias
//!   whose entity does not resolve still reads its first name as a value (TS2708 for a
//!   namespace without values). (tsc:
//!   `checkImportEqualsDeclaration` in `tsc/internal/checker/checker.go`.)
//!
//! JavaScript files and symbols declared in them are left alone (expando namespaces).

use ts_r_ast::{Node, NodeId};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_types::SymId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::ident_text;

impl<'c, 'a> Ck<'c, 'a> {
    /// The entity of `import x = A.B.C` at the import-equals declaration `l`.
    pub(crate) fn check_import_equals_entity(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let Node::ImportEquals { module_ref, .. } = *ast.node(l.node) else {
            return;
        };
        if self.c.file(l.file).is_js() {
            return;
        }
        if !matches!(ast.node(module_ref), Node::ExternalModuleRef { .. }) {
            self.check_import_equals_target(l, module_ref);
            self.check_import_equals_type_only_ref(l.file, module_ref);
        }
        let r = Loc::new(l.file, module_ref);
        match *ast.node(module_ref) {
            Node::Ident { name } => {
                let text = ast.atom(name);
                // `import q = null` is a parse error (TS1359).
                if matches!(text, "null" | "true" | "false" | "this" | "super" | "void")
                    || text == "globalThis" && self.c.resolve_ident(r, meaning::ALL).is_none()
                {
                    return;
                }
                self.resolve_namespace_name(r, None);
            }
            Node::QualifiedName { left, right }
            | Node::Member {
                obj: left,
                prop: right,
                ..
            } => {
                let ll = Loc::new(l.file, left);
                if self.global_this_member(ll, right, meaning::ALL).is_some() {
                    return;
                }
                let Some(ls) = self.resolve_namespace_name(ll, Some(right)) else {
                    return;
                };
                let Some(name) = ident_text(ast, right) else {
                    return;
                };
                if self.c.export_of(ls, name).is_none() {
                    let left_text = self.namespace_display(ls, ll);
                    self.report_missing_namespace_member(
                        Loc::new(l.file, right),
                        ls,
                        &left_text,
                        name,
                        meaning::ALL,
                    );
                }
            }
            _ => {}
        }
    }

    /// What `import x = A.B` stands for: a value whose first name `A` is shadowed by a
    /// local value (TS2437), a type under a predefined type's name (TS2438).
    /// TS1380/TS1379: the entity of `import x = a.B`, or one of its left parts, is itself
    /// a type-only import or export (each part looked up without resolving its final
    /// alias, from the whole name leftwards; reported once, at the whole entity).
    fn check_import_equals_type_only_ref(
        &mut self,
        file: ts_r_diagnostics::FileId,
        module_ref: NodeId,
    ) {
        let mut name = module_ref;
        for _ in 0..4096 {
            let ast = self.ast(file);
            if let Some(s) = self.resolve_entity(Loc::new(file, name), meaning::ALL)
                && let Some((_, exported)) = self.type_only_alias(s)
            {
                let info = if exported {
                    dg::AN_IMPORT_ALIAS_CANNOT_REFERENCE_A_DECLARATION_THAT_WAS_EXPORTED_USING_EXPORT_TY_1379
                } else {
                    dg::AN_IMPORT_ALIAS_CANNOT_REFERENCE_A_DECLARATION_THAT_WAS_IMPORTED_USING_IMPORT_TY_1380
                };
                self.error(Loc::new(file, module_ref), info, &[]);
                return;
            }
            match *ast.node(name) {
                Node::QualifiedName { left, .. } | Node::Member { obj: left, .. } => name = left,
                _ => return,
            }
        }
    }

    fn check_import_equals_target(&mut self, l: Loc, module_ref: NodeId) {
        let ast = self.ast(l.file);
        let Node::ImportEquals {
            name, modifiers, ..
        } = *ast.node(l.node)
        else {
            return;
        };
        let mut first = module_ref;
        for _ in 0..4096 {
            match *ast.node(first) {
                Node::QualifiedName { left, .. } | Node::Member { obj: left, .. } => first = left,
                _ => break,
            }
        }
        let fl = Loc::new(l.file, first);
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        // TS1392 (grammar): `import type X = A.B`, when the entity resolves (tsc:
        // checker.go checkImportEqualsDeclaration).
        if let Node::ImportEquals {
            type_only: true, ..
        } = *ast.node(l.node)
            && self.c.resolve_alias(sym).is_some()
            && !self.has_syntax_errors(l.file)
        {
            self.error(l, dg::AN_IMPORT_ALIAS_CANNOT_USE_IMPORT_TYPE_1392, &[]);
        }
        let Some(target) = self.c.resolve_alias(sym) else {
            // An exported alias of nothing is still emitted as a value read of its first
            // name: a namespace without values there is TS2708.
            if modifiers.contains(ts_r_ast::Modifiers::EXPORT)
                && !self.is_ambient_node(l)
                && let Some(text) = ident_text(ast, first)
                && self.c.resolve_ident(fl, meaning::VALUE).is_none()
                && let Some(t) = self.c.resolve_ident(fl, meaning::NAMESPACE)
                && let Some(t) = self.c.resolve_alias(t)
            {
                let text = text.to_owned();
                self.report_non_value(fl, t, &text);
            }
            return;
        };
        // Only a namespace or enum target counts: an alias that resolved to a plain value
        // already got TS2503 or TS2702 from the namespace lookup.
        // The binder resolved `A` as a namespace; look it up again with both meanings.
        if self.c.has_meaning(target, meaning::VALUE)
            && self.c.sym_flags(target).intersects(SymbolFlags::NAMESPACE)
            && let Some(text) = ident_text(ast, first)
            && let Some(s) =
                self.c
                    .lookup_in_scope_chain(fl, text, meaning::VALUE | meaning::NAMESPACE)
            && let Some(s) = self.c.resolve_alias(s)
            && !self.c.sym_flags(s).intersects(SymbolFlags::NAMESPACE)
        {
            self.error(
                fl,
                dg::MODULE_0_IS_HIDDEN_BY_A_LOCAL_DECLARATION_WITH_THE_SAME_NAME_2437,
                &[text],
            );
        }
        if self.c.has_meaning(target, meaning::TYPE)
            && let Some(text) = ident_text(ast, name)
            && crate::decl_names::RESERVED.contains(&text)
        {
            self.error(
                Loc::new(l.file, name),
                dg::IMPORT_NAME_CANNOT_BE_0_2438,
                &[text],
            );
        }
    }

    /// The identifier `l` (text `text`, `member` accessed on it) resolved to `s`, standing
    /// for `r`: `r` itself when it is a namespace, an outer namespace it shadows, or
    /// TS2713/TS2702/TS2503.
    pub(crate) fn namespace_ident_without_namespace(
        &mut self,
        l: Loc,
        s: SymId,
        r: SymId,
        text: &str,
        member: Option<NodeId>,
    ) -> Option<SymId> {
        if self.namespace_like(l, r) {
            return Some(r);
        }
        if let Some(n) = self.c.resolve_ident(l, meaning::NAMESPACE)
            && n != s
        {
            return self.c.resolve_alias(n);
        }
        if self.c.has_meaning(r, meaning::TYPE) {
            self.type_used_as_namespace(l, r, text, member);
        } else {
            self.report_cannot_find_namespace(l, text);
        }
        None
    }

    /// The identifier `l` names the type `t`, with `member` accessed on it: TS2713 when
    /// the type has that property, TS2702 otherwise.
    pub(crate) fn type_used_as_namespace(
        &mut self,
        l: Loc,
        t: SymId,
        text: &str,
        member: Option<NodeId>,
    ) {
        if self.c.file(l.file).is_js() {
            return;
        }
        let ast = self.ast(l.file);
        // A missing member name is a parse error (`Array.<number>`).
        let member_name = match member.map(|m| ident_text(ast, m)) {
            None => None,
            Some(Some(n)) if !n.is_empty() => Some(n),
            Some(_) => return,
        };
        let has_property = match member_name {
            Some(m) => {
                let ty = self.declared_type_of_symbol(t);
                self.property_of_type(ty, m).is_some()
            }
            None => false,
        };
        match member_name {
            Some(m) if has_property => self.error(
                l,
                dg::CANNOT_ACCESS_0_1_BECAUSE_0_IS_A_TYPE_BUT_NOT_A_NAMESPACE_DID_YOU_MEAN_TO_RETRIE_2713,
                &[text, m],
            ),
            _ => self.error(
                l,
                dg::_0_ONLY_REFERS_TO_A_TYPE_BUT_IS_BEING_USED_AS_A_NAMESPACE_HERE_2702,
                &[text],
            ),
        }
    }

    /// The qualified name `l` (`left.right`, `left` standing for `ls`) resolved to `t`:
    /// `t` when it is a namespace, else TS2713 at `member` (a type) or TS2694 at `right`
    /// (a value).
    pub(crate) fn namespace_member_without_namespace(
        &mut self,
        l: Loc,
        ls: SymId,
        t: SymId,
        member: Option<NodeId>,
    ) -> Option<SymId> {
        if self.namespace_like(l, t) {
            return Some(t);
        }
        let ast = self.ast(l.file);
        let (Node::QualifiedName { left, right }
        | Node::Member {
            obj: left,
            prop: right,
            ..
        }) = *ast.node(l.node)
        else {
            return Some(t);
        };
        let name = ident_text(ast, right)?;
        if self.c.sym_flags(t).intersects(SymbolFlags::ENUM_MEMBER)
            && matches!(ast.node(left), Node::Ident { .. })
        {
            let member_name = ident_text(ast, member?)?;
            let text = format!("{}.{member_name}", crate::nodes::entity_text(ast, l.node));
            self.error(
                l,
                dg::_0_REFERS_TO_A_VALUE_BUT_IS_BEING_USED_AS_A_TYPE_HERE_DID_YOU_MEAN_TYPEOF_0_2749,
                &[&text],
            );
        } else if self.c.has_meaning(t, meaning::TYPE) {
            let m = member?;
            // A missing member name (a parse error) still reports, with an empty name.
            let member_name = match ast.node(m) {
                Node::Missing {} => "",
                _ => ident_text(ast, m)?,
            };
            self.error(
                Loc::new(l.file, m),
                dg::CANNOT_ACCESS_0_1_BECAUSE_0_IS_A_TYPE_BUT_NOT_A_NAMESPACE_DID_YOU_MEAN_TO_RETRIE_2713,
                &[name, member_name],
            );
        } else {
            let left_text = self.namespace_display(ls, Loc::new(l.file, left));
            self.error(
                Loc::new(l.file, right),
                dg::NAMESPACE_0_HAS_NO_EXPORTED_MEMBER_1_2694,
                &[&left_text, name],
            );
        }
        None
    }

    /// `true` when `r` can stand as a namespace at `l`: it has a namespace meaning, is a
    /// module, or JavaScript is involved.
    fn namespace_like(&self, l: Loc, r: SymId) -> bool {
        self.c.has_meaning(r, meaning::NAMESPACE)
            || self
                .c
                .sym_flags(r)
                .intersects(SymbolFlags::MODULE_FILE | SymbolFlags::ALIAS)
            || self.c.file(l.file).is_js()
            || self.c.decls(r).iter().any(|d| self.c.file(d.file).is_js())
    }

    /// How TS2694 names the namespace `ls` written as `left`: its symbol's qualified name
    /// when `left` is an alias, the text as written otherwise; either way a namespace
    /// exported from a module file is led by that file's quoted path (`"a".N`).
    pub(crate) fn namespace_display(&mut self, ls: SymId, left: Loc) -> String {
        let ast = self.ast(left.file);
        let through_alias = matches!(ast.node(left.node), Node::Ident { .. })
            && self
                .c
                .resolve_ident(left, meaning::NAMESPACE)
                .is_some_and(|s| self.c.sym_flags(s).intersects(SymbolFlags::ALIAS));
        if self.c.sym_flags(ls).intersects(SymbolFlags::MODULE_FILE) {
            return crate::nodes::entity_text(ast, left.node);
        }
        let mut out = self.c.sym_name(ls).to_owned();
        let mut file_prefix = None;
        let mut cur = self.c.sym_parent(ls);
        for _ in 0..32 {
            let Some(p) = cur else { break };
            let f = self.c.sym_flags(p);
            if f.intersects(SymbolFlags::MODULE_FILE) {
                file_prefix = self.module_file_display(p);
                break;
            }
            if !f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
                || self.c.sym_name(p) == "__global"
            {
                break;
            }
            out = format!("{}.{}", self.c.sym_name(p), out);
            cur = self.c.sym_parent(p);
        }
        match file_prefix {
            Some(m) => format!("{m}.{out}"),
            None if through_alias => out,
            None => crate::nodes::entity_text(ast, left.node),
        }
    }

    /// A source file's module symbol as tsc names it: its path without the extension,
    /// quoted. `None` for anything else.
    fn module_file_display(&self, m: SymId) -> Option<String> {
        if self.c.sym_name(m) != "__file" {
            return None;
        }
        let d = self.c.first_decl(m)?;
        let path = crate::print::strip_ts_ext(&self.c.file(d.file).path);
        Some(format!("\"{path}\""))
    }

    /// TS2503 for a namespace name that resolves to none, TS2833 with a suggestion among
    /// the namespaces in scope.
    pub(crate) fn report_cannot_find_namespace(&mut self, l: Loc, text: &str) {
        match self.meaning_name_suggestion(l, text, meaning::NAMESPACE) {
            Some(s) => self.error(
                l,
                dg::CANNOT_FIND_NAMESPACE_0_DID_YOU_MEAN_1_2833,
                &[text, &s],
            ),
            None => self.error(l, dg::CANNOT_FIND_NAMESPACE_0_2503, &[text]),
        }
    }

    /// TS2694 for a member `name` missing from the namespace `ls` (shown as `left_text`),
    /// TS2724 with a suggestion among its exports of meaning `m`.
    pub(crate) fn report_missing_namespace_member(
        &mut self,
        at: Loc,
        ls: SymId,
        left_text: &str,
        name: &str,
        m: u8,
    ) {
        let names: Vec<String> = self
            .c
            .exports_of(ls)
            .into_iter()
            .filter(|(n, s)| {
                n != "default"
                    && n != "export="
                    && self
                        .c
                        .resolve_alias(*s)
                        .is_some_and(|t| self.c.has_meaning(t, m))
            })
            .map(|(n, _)| n)
            .collect();
        match crate::relate::spelling_suggestion(name, names.iter().map(String::as_str)) {
            Some(s) => self.error(
                at,
                dg::_0_HAS_NO_EXPORTED_MEMBER_NAMED_1_DID_YOU_MEAN_2_2724,
                &[left_text, name, &s],
            ),
            None => self.error(
                at,
                dg::NAMESPACE_0_HAS_NO_EXPORTED_MEMBER_1_2694,
                &[left_text, name],
            ),
        }
    }

    /// A spelling suggestion for `name` among the names of meaning `m` in scope at `e`
    /// (`Ck::scope_name_candidates`), not the alias being declared (`import v = V`).
    fn meaning_name_suggestion(&mut self, e: Loc, name: &str, m: u8) -> Option<String> {
        let names = self.scope_name_candidates(e, m, true);
        crate::relate::spelling_suggestion(name, names.iter().map(String::as_str))
    }
}
