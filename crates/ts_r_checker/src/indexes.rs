//! Index signature constraints on declared members (TS2411, TS2413).
//!
//! Rules observed from tsc 7.0.2:
//! - Each member written in a class body, interface declaration or type literal (and each
//!   constructor parameter property) must have a type assignable to every index signature
//!   of the containing type that applies to its name, including index signatures inherited
//!   from bases or declared in other merged declarations: a `string` index applies to every
//!   name, a `number` index to numeric names (`0`, `"2"`), a template literal index to the
//!   names it matches, a `symbol` index to symbol-keyed names (`[Symbol.iterator]`, which
//!   no other index applies to). Computed keys count by their literal or unique symbol
//!   type. Optional members include `undefined`; enum-typed members are checked too. Static
//!   members are checked against static index signatures. The error is at the member name,
//!   which is printed as written (`'"2"'`, `'[1 << 6]'`).
//! - A member whose computed key has a non-literal type (`[+s]`, `[s + s]`) is checked with
//!   its own declared type against the indexes for its key kind (number-like: `number` and
//!   `string`; string-like: `string`; symbol: `symbol`), implied indexes included.
//! - A `number` index type must be assignable to the `string` index type: TS2413 at each
//!   `number` index signature of the declaration, or at its `string` ones when the `number`
//!   index is inherited.
//! - Classes and interfaces also check what they inherit: an inherited property against
//!   their own index signature (at that signature, the property printed as declared,
//!   `'["get1"]'`); for interfaces also against an index inherited from another base (at the
//!   interface name, unless some base has both the index and the property), and `number` vs
//!   `string` indexes inherited from different bases (TS2413 at the name).

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId, NodeList};
use ts_r_binder::SymbolFlags;
use ts_r_diagnostics::FileId;
use ts_r_types::{IndexInfo, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::expr::is_numeric_name;
use crate::nodes::key_text;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2411/TS2413 for a class, interface declaration or type literal.
    pub(crate) fn check_index_constraints(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                let Some(sym) = self.c.sym_of_decl(l) else {
                    return;
                };
                let d = self.declared_type_of_symbol(sym);
                let this_t = self.store().type_param(sym, true);
                let inst = self.with_this_arg(d, this_t);
                let stat = self.type_of_symbol(sym);
                self.index_constraints_of(l.file, members, inst, Some(stat));
                self.inherited_index_constraints(l, sym, inst);
            }
            Node::Interface { body, .. } => {
                let Some(sym) = self.c.sym_of_decl(l) else {
                    return;
                };
                if self.c.sym_flags(sym).intersects(SymbolFlags::CLASS) {
                    return;
                }
                let d = self.declared_type_of_symbol(sym);
                let this_t = self.store().type_param(sym, true);
                let t = self.with_this_arg(d, this_t);
                self.index_constraints_of(l.file, body, t, None);
                if self.c.first_decl(sym) == Some(l) {
                    self.inherited_index_constraints(l, sym, t);
                }
            }
            Node::TypeLiteral { members } => {
                let t = self.type_from_node(l);
                self.index_constraints_of(l.file, members, t, None);
            }
            _ => {}
        }
    }

    /// Checks `members` (of one declaration) against the index signatures of `inst` (and of
    /// `stat` for static class members).
    fn index_constraints_of(
        &mut self,
        file: FileId,
        members: NodeList,
        inst: TypeId,
        stat: Option<TypeId>,
    ) {
        if !self.reporting(file) || !self.store().flags(inst).contains(TypeFlags::OBJECT) {
            return;
        }
        // Written and implied indexes; `check_member_index` picks the ones that apply.
        let inst_index = self.resolved_members(inst).index.to_vec();
        let stat_index = match stat {
            Some(s) if self.store().flags(s).contains(TypeFlags::OBJECT) => {
                self.resolved_members(s).index.to_vec()
            }
            _ => Vec::new(),
        };
        if inst_index.is_empty() && stat_index.is_empty() {
            return;
        }
        let ast = self.ast(file);
        let list: Vec<NodeId> = ast.list(members).to_vec();
        for m in list {
            match *ast.node(m) {
                Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. } => {
                    self.check_member_index(file, m, key, None, inst, &inst_index);
                }
                Node::PropertyDef { key, modifiers, .. } => {
                    if modifiers.contains(Modifiers::STATIC) {
                        if let Some(s) = stat {
                            self.check_member_index(file, m, key, None, s, &stat_index);
                        }
                    } else {
                        self.check_member_index(file, m, key, None, inst, &inst_index);
                    }
                }
                Node::MethodDef {
                    key,
                    modifiers,
                    kind,
                    value,
                    ..
                } => {
                    if kind == MethodKind::Constructor {
                        let Some(parts) = crate::nodes::fn_parts(ast, value) else {
                            continue;
                        };
                        for &p in ast.list(parts.params) {
                            if self.is_parameter_property(Loc::new(file, p))
                                && let Node::Param { pattern, .. } = *ast.node(p)
                                && matches!(ast.node(pattern), Node::Ident { .. })
                            {
                                self.check_member_index(
                                    file,
                                    p,
                                    pattern,
                                    Some(p),
                                    inst,
                                    &inst_index,
                                );
                            }
                        }
                    } else if modifiers.contains(Modifiers::STATIC) {
                        if let Some(s) = stat {
                            self.check_member_index(file, m, key, None, s, &stat_index);
                        }
                    } else {
                        self.check_member_index(file, m, key, None, inst, &inst_index);
                    }
                }
                _ => {}
            }
        }
        let inst_written = self.written_indexes(inst_index);
        let stat_written = self.written_indexes(stat_index);
        self.number_vs_string_index(file, members, &inst_written, false);
        self.number_vs_string_index(file, members, &stat_written, true);
    }

    /// TS2413: `number` index type vs `string` index type, at each `number` index signature
    /// in `members` (the static ones when `statics`), or at the `string` ones when the
    /// `number` index is inherited.
    fn number_vs_string_index(
        &mut self,
        file: FileId,
        members: NodeList,
        index: &[IndexInfo],
        statics: bool,
    ) {
        let ast = self.ast(file);
        let num = index.iter().find(|i| i.key == TypeId::NUMBER).copied();
        let strg = index.iter().find(|i| i.key == TypeId::STRING).copied();
        if let (Some(n), Some(s)) = (num, strg)
            && !self.is_assignable(n.value, s.value)
        {
            // The declaration (class, interface) holding each index, compared by symbol so
            // merged interface declarations count as one.
            let owner = |ck: &Self, member: Option<Loc>| {
                let p = ck.c.parent(member?)?;
                Some(ck.c.sym_of_decl(p).ok_or(p))
            };
            let here = owner(self, ast.list(members).first().map(|&m| Loc::new(file, m)));
            let at_key = if here.is_some() && owner(self, n.decl.and_then(Loc::unpack)) == here {
                TypeId::NUMBER
            } else {
                TypeId::STRING
            };
            for &m in ast.list(members) {
                let Node::IndexSignature {
                    params, modifiers, ..
                } = *ast.node(m)
                else {
                    continue;
                };
                if modifiers.contains(Modifiers::STATIC) != statics {
                    continue;
                }
                let is_key = ast.list(params).first().is_some_and(|&p| {
                    matches!(*ast.node(p), Node::Param { type_ann: Some(ta), .. }
                        if self.type_from_node(Loc::new(file, ta)) == at_key)
                });
                if !is_key {
                    continue;
                }
                let nv = self.type_to_string(n.value);
                let sv = self.type_to_string(s.value);
                self.error(
                    Loc::new(file, m),
                    dg::_0_INDEX_TYPE_1_IS_NOT_ASSIGNABLE_TO_2_INDEX_TYPE_3_2413,
                    &["number", &nv, "string", &sv],
                );
            }
        }
    }

    /// The index signatures that constrain members: not those implied by computed members
    /// of non-literal key types (`[s]: number`), whose `decl` is the member.
    fn written_indexes(&self, v: Vec<IndexInfo>) -> Vec<IndexInfo> {
        v.into_iter()
            .filter(|i| {
                i.decl
                    .and_then(Loc::unpack)
                    .is_none_or(|l| matches!(self.node(l), Node::IndexSignature { .. }))
            })
            .collect()
    }

    /// TS2411/TS2413 for what an interface inherits: an inherited property against an index
    /// signature of the interface (reported at that signature) or against one inherited from
    /// a base that lacks the property (reported at the interface name); a `number` index
    /// against a `string` index inherited from different bases (at the name).
    fn inherited_index_constraints(&mut self, l: Loc, sym: ts_r_types::SymId, t: TypeId) {
        if !self.reporting(l.file) || !self.store().flags(t).contains(TypeFlags::OBJECT) {
            return;
        }
        let index = self.resolved_members(t).index.to_vec();
        let index = self.written_indexes(index);
        if index.is_empty() {
            return;
        }
        let decls: Vec<Loc> = self
            .c
            .decls(sym)
            .into_iter()
            .filter(|d| {
                matches!(
                    self.ast(d.file).node(d.node),
                    Node::Interface { .. } | Node::ClassDecl { .. } | Node::ClassExpr { .. }
                )
            })
            .collect();
        // Own (instance) index signatures by key type, and the spans of the declarations.
        let mut own_index: Vec<(TypeId, Loc)> = Vec::new();
        for d in &decls {
            let ast = self.ast(d.file);
            let body = match *ast.node(d.node) {
                Node::Interface { body, .. } => body,
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
                _ => continue,
            };
            for &m in ast.list(body) {
                let Node::IndexSignature {
                    params, modifiers, ..
                } = *ast.node(m)
                else {
                    continue;
                };
                if modifiers.contains(Modifiers::STATIC) {
                    continue;
                }
                if let Some(&p) = ast.list(params).first()
                    && let Node::Param {
                        type_ann: Some(ta), ..
                    } = *ast.node(p)
                {
                    let k = self.type_from_node(Loc::new(d.file, ta));
                    if !own_index.iter().any(|x| x.0 == k) {
                        own_index.push((k, Loc::new(d.file, m)));
                    }
                }
            }
        }
        let spans: Vec<(FileId, ts_r_ast::Span)> =
            decls.iter().map(|d| (d.file, self.c.span(*d))).collect();
        let is_own = |ck: &Self, p: ts_r_types::SymId| {
            ck.c.decls(p).iter().any(|pd| {
                let ps = ck.c.span(*pd);
                spans
                    .iter()
                    .any(|(f, s)| *f == pd.file && s.start <= ps.start && ps.end <= s.end)
            })
        };
        let bases = self.base_types(sym).to_vec();
        if bases.is_empty() {
            return;
        }
        let (name_loc, is_class) = match *self.ast(l.file).node(l.node) {
            Node::Interface { name, .. } => (Loc::new(l.file, name), false),
            _ => (l, true),
        };
        let props = self.properties_of(t);
        for (n, p) in props {
            if is_own(self, p) {
                continue;
            }
            let name = self.store().str(n).to_owned();
            if name.starts_with('#') {
                continue;
            }
            let mut pt = self.type_of_symbol(p);
            if self.c.opts.exact_optional_property_types {
                pt = self.remove_missing(pt);
            }
            if self
                .store()
                .flags(pt)
                .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
                || pt == TypeId::UNRESOLVED
                || self.store().is_error(pt)
            {
                continue;
            }
            for info in &index {
                if !self.index_applies(info.key, &name)
                    || info.value == TypeId::UNRESOLVED
                    || self.is_assignable(pt, info.value)
                {
                    continue;
                }
                let at = match own_index.iter().find(|x| x.0 == info.key) {
                    Some(&(_, sig)) => sig,
                    // A class's base reports its own members.
                    None if is_class => continue,
                    None => {
                        // A base with this index and the property reports it itself.
                        let mut same_base = false;
                        for &b in &bases {
                            let has_index = self
                                .index_info_of(b, info.key)
                                .is_some_and(|i| i.key == info.key);
                            if has_index && self.property_of_type(b, &name).is_some() {
                                same_base = true;
                                break;
                            }
                        }
                        if same_base {
                            continue;
                        }
                        name_loc
                    }
                };
                let ps = self.type_to_string(pt);
                let ks = self.type_to_string(info.key);
                let vs = self.type_to_string(info.value);
                let display = self.member_display_name(p, &name);
                self.error(
                    at,
                    dg::PROPERTY_0_OF_TYPE_1_IS_NOT_ASSIGNABLE_TO_2_INDEX_TYPE_3_2411,
                    &[&display, &ps, &ks, &vs],
                );
            }
        }
        if is_class {
            return;
        }
        // TS2413 between `number` and `string` indexes inherited from different bases.
        let num = index.iter().find(|i| i.key == TypeId::NUMBER).copied();
        let strg = index.iter().find(|i| i.key == TypeId::STRING).copied();
        if let (Some(nm), Some(st)) = (num, strg)
            && !own_index
                .iter()
                .any(|x| x.0 == TypeId::NUMBER || x.0 == TypeId::STRING)
            && !self.is_assignable(nm.value, st.value)
        {
            let both = bases.iter().any(|&b| {
                let r = self.resolved_members(b);
                r.index.iter().any(|i| i.key == TypeId::NUMBER)
                    && r.index.iter().any(|i| i.key == TypeId::STRING)
            });
            if !both {
                let nv = self.type_to_string(nm.value);
                let sv = self.type_to_string(st.value);
                self.error(
                    name_loc,
                    dg::_0_INDEX_TYPE_1_IS_NOT_ASSIGNABLE_TO_2_INDEX_TYPE_3_2413,
                    &["number", &nv, "string", &sv],
                );
            }
        }
    }

    /// TS2411 for `member`, named by `key`.
    /// `whole`: report at this node's full span (parameter properties) instead of the key.
    fn check_member_index(
        &mut self,
        file: FileId,
        member: NodeId,
        key: NodeId,
        whole: Option<NodeId>,
        t: TypeId,
        index: &[IndexInfo],
    ) {
        if index.is_empty() {
            return;
        }
        let ast = self.ast(file);
        if matches!(ast.node(key), Node::PrivateName { .. }) {
            return;
        }
        let name = match key_text(ast, key) {
            Some(n) => n,
            None => {
                let Node::ComputedKey { expr } = *ast.node(key) else {
                    return;
                };
                match self.late_bound_name(Loc::new(file, expr)) {
                    Some(n) => n,
                    None => {
                        self.check_dynamic_member_index(file, member, key, expr, index);
                        return;
                    }
                }
            }
        };
        // Implied indexes already cover the named members.
        let index = self.written_indexes(index.to_vec());
        if index.is_empty() {
            return;
        }
        let Some(p) = self.property_of_type(t, &name) else {
            return;
        };
        // Once per property: at its first declaration (overloads, merged declarations).
        let first = self.c.first_decl(p).map(|d| self.error_span_of(d));
        let here = match whole {
            Some(w) => self.error_span_of(Loc::new(file, w)),
            None => self.error_span_of(Loc::new(file, key)),
        };
        if first.is_some_and(|f| f != here) {
            return;
        }
        let mut pt = self.type_of_symbol(p);
        if self.c.opts.exact_optional_property_types {
            pt = self.remove_missing(pt);
        }
        if self.unchecked_member_type(pt) {
            return;
        }
        for info in &index {
            if !self.index_applies(info.key, &name) || self.index_value_holds(pt, info) {
                continue;
            }
            self.report_member_index(file, key, whole, pt, info);
        }
    }

    /// TS2411 for a member whose computed key has a type that is not a literal or unique
    /// symbol (`[+s]`, `[1 << 6]`): a number-like key against the `number` and `string`
    /// indexes, a string-like key against the `string` index, a `symbol` key against the
    /// `symbol` index. The member's own declared type is checked.
    fn check_dynamic_member_index(
        &mut self,
        file: FileId,
        member: NodeId,
        key: NodeId,
        expr: NodeId,
        index: &[IndexInfo],
    ) {
        let kt = self.quietly(|ck| ck.check_expr(Loc::new(file, expr)));
        if kt == TypeId::UNRESOLVED
            || self.store().is_error(kt)
            || self
                .store()
                .flags(kt)
                .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
        {
            return;
        }
        // string, number, symbol
        let mut kinds = [false; 3];
        for k in self.union_members(kt) {
            let f = self.store().flags(k);
            if f.intersects(TypeFlags::STRING_LIKE) {
                kinds[0] = true;
            } else if f.intersects(TypeFlags::NUMBER_LIKE) {
                kinds[0] = true;
                kinds[1] = true;
            } else if f.intersects(TypeFlags::ES_SYMBOL_LIKE) {
                kinds[2] = true;
            }
        }
        let Some(s) = self.c.sym_of_decl(Loc::new(file, member)) else {
            return;
        };
        let pt = self.type_of_symbol(s);
        if self.unchecked_member_type(pt) {
            return;
        }
        for info in index {
            let applies = (info.key == TypeId::STRING && kinds[0])
                || (info.key == TypeId::NUMBER && kinds[1])
                || (info.key == TypeId::SYMBOL && kinds[2]);
            if !applies || self.index_value_holds(pt, info) {
                continue;
            }
            self.report_member_index(file, key, None, pt, info);
        }
    }

    /// Member types not checked against indexes: `any`, `unknown` and error types.
    fn unchecked_member_type(&self, pt: TypeId) -> bool {
        self.store()
            .flags(pt)
            .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
            || pt == TypeId::UNRESOLVED
            || self.store().is_error(pt)
    }

    /// Whether a member of type `pt` satisfies `info` (or can't be judged).
    fn index_value_holds(&mut self, pt: TypeId, info: &IndexInfo) -> bool {
        info.value == TypeId::UNRESOLVED || self.is_assignable(pt, info.value)
    }

    fn report_member_index(
        &mut self,
        file: FileId,
        key: NodeId,
        whole: Option<NodeId>,
        pt: TypeId,
        info: &IndexInfo,
    ) {
        let display = self.c.text(Loc::new(file, key)).to_owned();
        let ps = self.type_to_string(pt);
        let ks = self.type_to_string(info.key);
        let vs = self.type_to_string(info.value);
        let span = match whole {
            Some(w) => self.ast(file).span(w),
            None => self.error_span_of(Loc::new(file, key)),
        };
        self.error_span(
            file,
            span,
            dg::PROPERTY_0_OF_TYPE_1_IS_NOT_ASSIGNABLE_TO_2_INDEX_TYPE_3_2411,
            &[&display, &ps, &ks, &vs],
        );
    }

    /// A property's name as written at its first declaration (`'["get1"]'`), else `name`.
    fn member_display_name(&self, p: ts_r_types::SymId, name: &str) -> String {
        let Some(d) = self.c.first_decl(p) else {
            return name.to_owned();
        };
        let ast = self.ast(d.file);
        match *ast.node(d.node) {
            Node::PropertyDef { key, .. }
            | Node::MethodDef { key, .. }
            | Node::PropertySignature { key, .. }
            | Node::MethodSignature { key, .. } => self.c.text(Loc::new(d.file, key)).to_owned(),
            _ => name.to_owned(),
        }
    }

    /// Whether `t` is or contains (as a union member) an enum type or enum literal.
    pub(crate) fn mentions_enum(&self, t: TypeId) -> bool {
        let st = self.store();
        if st.flags(t).intersects(TypeFlags::ENUM_LIKE) {
            return true;
        }
        st.flags(t).contains(TypeFlags::UNION)
            && st
                .members_of_union(t)
                .iter()
                .any(|&m| st.flags(m).intersects(TypeFlags::ENUM_LIKE))
    }

    /// Whether an index signature of `t` applies to property `name` (every member of a
    /// union, some member of an intersection), template literal and pattern keys included.
    pub(crate) fn has_index_for_name(&mut self, t: TypeId, name: &str) -> bool {
        let t = self.apparent_type(t);
        let f = self.store().flags(t);
        if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
            let members = self.store().members_of_union(t).to_vec();
            let mut each = members
                .into_iter()
                .map(|m| self.has_index_for_name(m, name));
            return if f.contains(TypeFlags::UNION) {
                each.all(|x| x)
            } else {
                each.any(|x| x)
            };
        }
        if !f.contains(TypeFlags::OBJECT) {
            return false;
        }
        let keys: Vec<TypeId> = self
            .resolved_members(t)
            .index
            .iter()
            .map(|i| i.key)
            .collect();
        keys.into_iter().any(|k| self.index_applies(k, name))
    }

    /// Whether an index signature with key type `key` applies to property `name`.
    fn index_applies(&mut self, key: TypeId, name: &str) -> bool {
        // A symbol-keyed name (`__@iterator@12`) only meets `symbol` indexes.
        if name.starts_with("__@") {
            let kt = self.property_key_type(name);
            if self
                .store()
                .flags(kt)
                .intersects(TypeFlags::UNIQUE_ES_SYMBOL)
            {
                return key == TypeId::SYMBOL;
            }
        }
        if key == TypeId::STRING {
            return true;
        }
        if key == TypeId::NUMBER {
            return is_numeric_name(name);
        }
        let f = self.store().flags(key);
        if f.intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING) {
            let lit = self.store().string_literal(name, false);
            return self.is_assignable(lit, key);
        }
        false
    }
}
