//! `override` modifiers and `noImplicitOverride` (TS4112-TS4117, TS4127).
//!
//! Rules observed from tsc 7.0.2 (errors at the member name; parameter properties at the
//! parameter):
//! - `override` in a class without `extends`: TS4112 (`implements` does not count).
//! - `override` on a member the base class (instance side, or constructor side for
//!   `static` members) does not have: TS4113, or TS4117 with a spelling suggestion.
//! - `override` on a computed name that is not a literal or unique symbol: TS4127.
//! - With `noImplicitOverride`, a member without `override` that redeclares a base member:
//!   TS4114 (TS4115 for parameter properties). Implementing an abstract member does not
//!   need `override`. Ambient classes and `declare` fields are exempt.
//! - The base class prints as its instance type (`'A'`, `'{ mx(): void; }'`).
//! - In JavaScript the modifier is a JSDoc `@override` tag, with the JSDoc messages
//!   (TS4119-TS4123, TS4128) in place of TS4112-TS4117 and TS4127.

use ts_r_ast::{HeritageKind, MethodKind, Modifiers, Node, NodeId};
use ts_r_diagnostics::FileId;
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::{fn_parts, key_text};

/// One member subject to override rules.
struct Member {
    /// Where errors go: the key, or the parameter for parameter properties.
    at: NodeId,
    key: NodeId,
    modifiers: Modifiers,
    is_param: bool,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Override rules for the class declaration or expression `l`.
    pub(crate) fn check_class_overrides(&mut self, l: Loc) {
        if !self.reporting(l.file) {
            return;
        }
        let saved = self.print_scope.replace(l);
        self.check_class_overrides_in(l);
        self.print_scope = saved;
    }

    fn check_class_overrides_in(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let (members, heritage, name) = match *ast.node(l.node) {
            Node::ClassDecl {
                members,
                heritage,
                name,
                ..
            }
            | Node::ClassExpr {
                members,
                heritage,
                name,
                ..
            } => (members, heritage, name),
            _ => return,
        };
        let mut list: Vec<Member> = Vec::new();
        for &m in ast.list(members) {
            match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. } => list.push(Member {
                    at: key,
                    key,
                    modifiers,
                    is_param: false,
                }),
                Node::MethodDef {
                    key,
                    modifiers,
                    kind,
                    value,
                    ..
                } => {
                    if kind != MethodKind::Constructor {
                        list.push(Member {
                            at: key,
                            key,
                            modifiers,
                            is_param: false,
                        });
                        continue;
                    }
                    let Some(parts) = fn_parts(ast, value) else {
                        continue;
                    };
                    for &p in ast.list(parts.params) {
                        if let Node::Param {
                            modifiers, pattern, ..
                        } = *ast.node(p)
                            && self.is_parameter_property(Loc::new(l.file, p))
                            && matches!(ast.node(pattern), Node::Ident { .. })
                        {
                            list.push(Member {
                                at: p,
                                key: pattern,
                                modifiers,
                                is_param: true,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        if list.is_empty() {
            return;
        }
        let has_extends = ast.list(heritage).iter().any(|&h| {
            matches!(
                *ast.node(h),
                Node::HeritageClause {
                    kind: HeritageKind::Extends,
                    ..
                }
            )
        });
        if !has_extends {
            let class_name = self.class_display_name(l, name);
            for m in &list {
                if m.modifiers.contains(Modifiers::OVERRIDE) {
                    let info = self.override_message(
                        l.file,
                        dg::THIS_MEMBER_CANNOT_HAVE_AN_OVERRIDE_MODIFIER_BECAUSE_ITS_CONTAINING_CLASS_0_DOES_4112,
                    );
                    self.error(Loc::new(l.file, m.at), info, &[&class_name]);
                }
            }
            return;
        }
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let bases = self.base_types(sym);
        let Some(&base) = bases.first() else {
            return;
        };
        if !self.usable_base(base) {
            return;
        }
        let base_static = self.base_constructor_type(sym);
        let base_s = self.type_to_string(base);
        let ambient = self.is_ambient_node(l);
        for m in &list {
            self.check_member_override(l.file, m, base, base_static, &base_s, ambient);
        }
        let class_name = self.class_display_name(l, name);
        self.check_accessor_property_overrides(l.file, members, base, &base_s, &class_name);
    }

    /// The class's name as tsc prints it in messages: `const X = class { .. }` takes the
    /// variable's name; other unnamed classes are `(Anonymous class)`.
    pub(crate) fn class_display_name(&self, l: Loc, name: Option<NodeId>) -> String {
        let ast = self.ast(l.file);
        let name = name.or_else(|| match self.c.parent(l).map(|p| self.node(p)) {
            Some(&Node::VarDeclarator { name, init, .. })
                if init == Some(l.node) && matches!(ast.node(name), Node::Ident { .. }) =>
            {
                Some(name)
            }
            _ => None,
        });
        name.map_or_else(
            || "(Anonymous class)".to_owned(),
            |n| self.c.text(Loc::new(l.file, n)).to_owned(),
        )
    }

    /// TS2610 (a property overriding a base accessor) and TS2611 (an accessor overriding a
    /// base property), for instance members whose base member is declared, non-abstract,
    /// in a class. Accessors report at their first declaration.
    fn check_accessor_property_overrides(
        &mut self,
        file: FileId,
        members: ts_r_ast::NodeList,
        base: TypeId,
        base_s: &str,
        class_name: &str,
    ) {
        let ast = self.ast(file);
        // (error node, name, derived member is an accessor)
        let mut derived: Vec<(NodeId, String, bool)> = Vec::new();
        let mut push = |at: NodeId, key: NodeId, accessor: bool| {
            if matches!(
                ast.node(key),
                Node::ComputedKey { .. } | Node::PrivateName { .. }
            ) {
                return;
            }
            if let Some(name) = key_text(ast, key)
                && !derived.iter().any(|(_, n, _)| *n == name)
            {
                derived.push((at, name, accessor));
            }
        };
        for &m in ast.list(members) {
            match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. }
                    if !modifiers.contains(Modifiers::STATIC) =>
                {
                    push(key, key, modifiers.contains(Modifiers::ACCESSOR));
                }
                Node::MethodDef {
                    key,
                    modifiers,
                    kind: MethodKind::Get | MethodKind::Set,
                    ..
                } if !modifiers.contains(Modifiers::STATIC) => push(key, key, true),
                Node::MethodDef {
                    kind: MethodKind::Constructor,
                    value,
                    ..
                } => {
                    let Some(parts) = fn_parts(ast, value) else {
                        continue;
                    };
                    for &p in ast.list(parts.params) {
                        if let Node::Param { pattern, .. } = *ast.node(p)
                            && matches!(ast.node(pattern), Node::Ident { .. })
                            && self.is_parameter_property(Loc::new(file, p))
                        {
                            push(pattern, pattern, false);
                        }
                    }
                }
                _ => {}
            }
        }
        for (at, name, accessor) in derived {
            let Some(bp) = self.property_of_type(base, &name) else {
                continue;
            };
            let Some(base_accessor) = self.class_member_kind(bp) else {
                continue;
            };
            if base_accessor == accessor {
                continue;
            }
            let info = if accessor {
                dg::_0_IS_DEFINED_AS_A_PROPERTY_IN_CLASS_1_BUT_IS_OVERRIDDEN_HERE_IN_2_AS_AN_ACCESSOR_2611
            } else {
                dg::_0_IS_DEFINED_AS_AN_ACCESSOR_IN_CLASS_1_BUT_IS_OVERRIDDEN_HERE_IN_2_AS_AN_INSTANC_2610
            };
            self.error(Loc::new(file, at), info, &[&name, base_s, class_name]);
        }
    }

    /// For a member declared only in classes and not abstract: `Some(true)` for an accessor
    /// (get/set or `accessor` field), `Some(false)` for a property (field or parameter
    /// property). `None` for anything else (interface members, methods, mapped members).
    fn class_member_kind(&self, p: SymId) -> Option<bool> {
        let p = match self.c.synth_sym(p) {
            Some(x) if x.origins.len() == 1 => x.origins[0],
            Some(_) => return None,
            None => p,
        };
        let decls = self.c.decls(p);
        let mut kind = None;
        for &d in decls.iter() {
            let ast = self.ast(d.file);
            let this = match *ast.node(d.node) {
                Node::PropertyDef { modifiers, .. } => {
                    if modifiers.contains(Modifiers::ABSTRACT) {
                        return None;
                    }
                    modifiers.contains(Modifiers::ACCESSOR)
                }
                Node::MethodDef {
                    modifiers,
                    kind: MethodKind::Get | MethodKind::Set,
                    ..
                } => {
                    if modifiers.contains(Modifiers::ABSTRACT) {
                        return None;
                    }
                    true
                }
                Node::Param { .. } if self.is_parameter_property(d) => false,
                _ => return None,
            };
            if kind.is_some_and(|k| k != this) {
                return None;
            }
            kind = Some(this);
        }
        kind
    }

    fn usable_base(&self, t: TypeId) -> bool {
        t != TypeId::UNRESOLVED
            && !self.store().is_error(t)
            && self
                .store()
                .flags(t)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
    }

    fn check_member_override(
        &mut self,
        file: FileId,
        m: &Member,
        base: TypeId,
        base_static: Option<TypeId>,
        base_s: &str,
        ambient: bool,
    ) {
        let ast = self.ast(file);
        if matches!(ast.node(m.key), Node::PrivateName { .. }) {
            return;
        }
        let has_override = m.modifiers.contains(Modifiers::OVERRIDE);
        let name = match *ast.node(m.key) {
            Node::ComputedKey { expr } => {
                key_text(ast, m.key).or_else(|| self.late_bound_name(Loc::new(file, expr)))
            }
            _ => key_text(ast, m.key),
        };
        let at = Loc::new(file, m.at);
        let Some(name) = name else {
            // Entity names (`[foo]`) may be unique symbols this layer does not track.
            let symbol_entity = match *ast.node(m.key) {
                Node::ComputedKey { expr } => {
                    let e = crate::nodes::skip_parens(ast, expr);
                    matches!(ast.node(e), Node::Ident { .. } | Node::Member { .. }) && {
                        let t = self.quietly(|ck| ck.check_expr(Loc::new(file, e)));
                        self.store().flags(t).intersects(TypeFlags::ES_SYMBOL_LIKE)
                            || t == TypeId::UNRESOLVED
                    }
                }
                _ => false,
            };
            let entity = symbol_entity;
            if has_override && !entity {
                let info = self.override_message(
                    file,
                    dg::THIS_MEMBER_CANNOT_HAVE_AN_OVERRIDE_MODIFIER_BECAUSE_ITS_NAME_IS_DYNAMIC_4127,
                );
                self.error(at, info, &[]);
            }
            return;
        };
        let target = if m.modifiers.contains(Modifiers::STATIC) {
            match base_static {
                Some(t) if self.usable_base(t) => t,
                _ => return,
            }
        } else {
            base
        };
        let found = self.property_of_type(target, &name);
        if has_override {
            if found.is_some() {
                return;
            }
            match self.spelling_suggestion_for_property(&name, target) {
                Some(s) => {
                    let info = self.override_message(
                        file,
                        dg::THIS_MEMBER_CANNOT_HAVE_AN_OVERRIDE_MODIFIER_BECAUSE_IT_IS_NOT_DECLARED_IN_THE_B_4117,
                    );
                    self.error(at, info, &[base_s, &s]);
                }
                None => {
                    let info = self.override_message(
                        file,
                        dg::THIS_MEMBER_CANNOT_HAVE_AN_OVERRIDE_MODIFIER_BECAUSE_IT_IS_NOT_DECLARED_IN_THE_B_4113,
                    );
                    self.error(at, info, &[base_s]);
                }
            }
            return;
        }
        if !self.c.opts.no_implicit_override || ambient || m.modifiers.contains(Modifiers::DECLARE)
        {
            return;
        }
        let Some(bp) = found else {
            return;
        };
        // Implementing an abstract member needs no `override`.
        let decls = self.c.decls(bp);
        if decls.is_empty()
            || decls
                .iter()
                .any(|d| self.node(*d).modifiers().contains(Modifiers::ABSTRACT))
        {
            return;
        }
        let info = if m.is_param {
            dg::THIS_PARAMETER_PROPERTY_MUST_HAVE_AN_OVERRIDE_MODIFIER_BECAUSE_IT_OVERRIDES_A_ME_4115
        } else {
            dg::THIS_MEMBER_MUST_HAVE_AN_OVERRIDE_MODIFIER_BECAUSE_IT_OVERRIDES_A_MEMBER_IN_THE_B_4114
        };
        let info = self.override_message(file, info);
        self.error(at, info, &[base_s]);
    }

    /// In a JavaScript file, the JSDoc `@override` form of an `override` message.
    fn override_message(
        &self,
        file: FileId,
        info: crate::diagnostics_generated::DiagnosticInfo,
    ) -> crate::diagnostics_generated::DiagnosticInfo {
        if !self.c.file(file).is_js() {
            return info;
        }
        match info.code {
            4112 => dg::THIS_MEMBER_CANNOT_HAVE_A_JSDOC_COMMENT_WITH_AN_OVERRIDE_TAG_BECAUSE_ITS_CONTAIN_4121,
            4113 => dg::THIS_MEMBER_CANNOT_HAVE_A_JSDOC_COMMENT_WITH_AN_OVERRIDE_TAG_BECAUSE_IT_IS_NOT_D_4122,
            4114 => dg::THIS_MEMBER_MUST_HAVE_A_JSDOC_COMMENT_WITH_AN_OVERRIDE_TAG_BECAUSE_IT_OVERRIDES_A_4119,
            4115 => dg::THIS_PARAMETER_PROPERTY_MUST_HAVE_A_JSDOC_COMMENT_WITH_AN_OVERRIDE_TAG_BECAUSE_I_4120,
            4117 => dg::THIS_MEMBER_CANNOT_HAVE_A_JSDOC_COMMENT_WITH_AN_OVERRIDE_TAG_BECAUSE_IT_IS_NOT_D_4123,
            4127 => dg::THIS_MEMBER_CANNOT_HAVE_A_JSDOC_COMMENT_WITH_AN_OVERRIDE_TAG_BECAUSE_ITS_NAME_IS_4128,
            _ => info,
        }
    }
}
