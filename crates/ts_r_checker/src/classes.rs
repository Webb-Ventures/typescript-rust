//! Class and member rules (track M5 "checker decls"): accessors, abstract members,
//! `override`, heritage compatibility, property initialization, parameter properties,
//! member accessibility and index signatures.

use ts_r_ast::{MethodKind, Modifiers, Node, NodeList};
use ts_r_types::TypeId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::{fn_parts, is_this_param, key_text};

impl<'c, 'a> Ck<'c, 'a> {
    /// The type of an unannotated set-accessor parameter whose accessor has a getter: the
    /// accessor's type (tsc takes it from the getter; no implicit `any`).
    pub(crate) fn setter_param_type_from_getter(&mut self, p: Loc) -> Option<TypeId> {
        let f = self.c.parent(p)?;
        let m = self.c.parent(f)?;
        let Node::MethodDef {
            kind: MethodKind::Set,
            ..
        } = *self.node(m)
        else {
            return None;
        };
        let sym = self.c.sym_of_decl(m)?;
        let has_getter = self.c.decls(sym).iter().any(|&d| {
            matches!(
                self.node(d),
                Node::MethodDef {
                    kind: MethodKind::Get,
                    ..
                }
            )
        });
        if has_getter {
            return Some(self.type_of_symbol(sym));
        }
        // A computed name (`set [Symbol.x](v)`): the sibling getter with the same late-bound
        // name and staticness (the binder keeps computed members apart).
        let Node::MethodDef { key, modifiers, .. } = *self.node(m) else {
            return None;
        };
        let is_static = modifiers.contains(Modifiers::STATIC);
        let Node::ComputedKey { expr } = *self.node(Loc::new(m.file, key)) else {
            return None;
        };
        let name = self.late_bound_name(Loc::new(m.file, expr))?;
        let container = self.c.parent(m)?;
        let ast = self.ast(m.file);
        let members = match *ast.node(container.node) {
            Node::ObjectLit { props } => props,
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return None,
        };
        for &g in ast.list(members) {
            let Node::MethodDef {
                kind: MethodKind::Get,
                key: gk,
                modifiers: gm,
                ..
            } = *ast.node(g)
            else {
                continue;
            };
            if gm.contains(Modifiers::STATIC) != is_static {
                continue;
            }
            let Node::ComputedKey { expr: ge } = *ast.node(gk) else {
                continue;
            };
            if self.late_bound_name(Loc::new(m.file, ge)).as_deref() == Some(name.as_str())
                && let Some(gs) = self.c.sym_of_decl(Loc::new(m.file, g))
            {
                return Some(self.type_of_symbol(gs));
            }
        }
        None
    }

    /// Class-level declaration rules for a class declaration or expression.
    pub(crate) fn check_class_decl(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let members = match *ast.node(l.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return,
        };
        if self.is_ambient_node(l) {
            return;
        }
        self.check_setters_without_getters(l, members);
        self.check_class_heritage(l);
    }

    /// Accessor rules for an object literal.
    pub(crate) fn check_object_accessors(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let Node::ObjectLit { props } = *ast.node(l.node) else {
            return;
        };
        self.check_setters_without_getters(l, props);
    }

    /// TS7032: a set accessor without a getter whose parameter has no annotation.
    fn check_setters_without_getters(&mut self, l: Loc, members: NodeList) {
        if !self.c.opts.no_implicit_any || self.c.file(l.file).is_js() {
            return;
        }
        let file = l.file;
        let ast = self.ast(file);
        let list = ast.list(members);
        for &m in list {
            let Node::MethodDef {
                kind: MethodKind::Set,
                key,
                value,
                ..
            } = *ast.node(m)
            else {
                continue;
            };
            if !matches!(ast.node(key), Node::Ident { .. } | Node::PrivateName { .. }) {
                continue;
            }
            let Some(name) = key_text(ast, key) else {
                continue;
            };
            let Some(sym) = self.c.sym_of_decl(Loc::new(file, m)) else {
                continue;
            };
            let has_getter = self.c.decls(sym).iter().any(|&d| {
                matches!(
                    self.node(d),
                    Node::MethodDef {
                        kind: MethodKind::Get,
                        ..
                    }
                )
            });
            if has_getter {
                continue;
            }
            let Some(parts) = fn_parts(ast, value) else {
                continue;
            };
            let Some(&p) = ast
                .list(parts.params)
                .iter()
                .find(|&&x| !is_this_param(ast, x))
            else {
                continue;
            };
            if let Node::Param { type_ann: None, .. } = *ast.node(p) {
                self.error(
                    Loc::new(file, key),
                    dg::PROPERTY_0_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_ITS_SET_ACCESSOR_LACKS_A_PARAMETER_TY_7032,
                    &[&name],
                );
            }
        }
    }
}
