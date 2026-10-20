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
            let member_name = ident_text(ast, m)?;
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
    /// when `left` is an alias, the text as written otherwise.
    pub(crate) fn namespace_display(&mut self, ls: SymId, left: Loc) -> String {
        let ast = self.ast(left.file);
        let through_alias = matches!(ast.node(left.node), Node::Ident { .. })
            && self
                .c
                .resolve_ident(left, meaning::NAMESPACE)
                .is_some_and(|s| self.c.sym_flags(s).intersects(SymbolFlags::ALIAS));
        if !through_alias || self.c.sym_flags(ls).intersects(SymbolFlags::MODULE_FILE) {
            return crate::nodes::entity_text(ast, left.node);
        }
        let mut out = self.c.sym_name(ls).to_owned();
        let mut cur = self.c.sym_parent(ls);
        for _ in 0..32 {
            let Some(p) = cur else { break };
            let f = self.c.sym_flags(p);
            if f.intersects(SymbolFlags::MODULE_FILE)
                || !f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
                || self.c.sym_name(p) == "__global"
            {
                break;
            }
            out = format!("{}.{}", self.c.sym_name(p), out);
            cur = self.c.sym_parent(p);
        }
        out
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

    /// A spelling suggestion for `name` among the names of meaning `m` in scope at `e`:
    /// locals of the enclosing scopes, then globals.
    fn meaning_name_suggestion(&mut self, e: Loc, name: &str, m: u8) -> Option<String> {
        let b = self.c.bound(e.file);
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = Some(e.node);
        let mut scope = None;
        while let Some(n) = cur {
            if let Some(sc) = b.scope_of(n) {
                scope = Some(sc);
                break;
            }
            cur = parents.parent(n);
        }
        let at = self.c.span(e);
        // Not the alias being declared (`import v = V`).
        let declaring = |s: SymId| {
            self.c.decls(s).iter().any(|d| {
                let ds = self.c.span(*d);
                d.file == e.file && ds.start <= at.start && at.end <= ds.end
            })
        };
        let mut names: Vec<String> = Vec::new();
        for _ in 0..1000 {
            let Some(sc) = scope else { break };
            let Some(data) = b.scopes.get(sc.0 as usize) else {
                break;
            };
            for (n, sym) in b.table(data.locals) {
                let s = self.c.sym(e.file, *sym);
                if self.c.has_meaning(s, m) && !declaring(s) {
                    names.push(b.names.text(ast, *n).to_owned());
                }
            }
            scope = data.parent;
        }
        for (g, _) in self.c.link.globals.iter() {
            if self.c.global_symbol(g, m).is_some_and(|s| !declaring(s)) {
                names.push(g.to_string());
            }
        }
        crate::relate::spelling_suggestion(name, names.iter().map(String::as_str))
    }
}
