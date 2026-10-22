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
        self.check_setters_without_getters(l, members);
        self.check_class_heritage(l);
        if self.is_ambient_node(l) {
            return;
        }
        self.check_class_merged_with_functions(l);
    }

    /// A class declaration (not ambient) merged with function declarations: TS2813 at the
    /// class name, with "Consider adding a 'declare' modifier" related to the class. The
    /// functions' TS2814 is reported when their own file is checked
    /// (`check_function_merged_with_class`), since they may live in another file.
    fn check_class_merged_with_functions(&mut self, l: Loc) {
        let Node::ClassDecl {
            name: Some(name), ..
        } = *self.node(l)
        else {
            return;
        };
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let has_function = self
            .c
            .decls(sym)
            .iter()
            .any(|&d| matches!(self.node(d), Node::FunctionDecl { name: Some(_), .. }));
        if !has_function || !self.reporting(l.file) {
            return;
        }
        let nl = Loc::new(l.file, name);
        let text = self.c.text(nl).to_owned();
        let mark = self.diags.len();
        self.error(
            nl,
            dg::CLASS_DECLARATION_CANNOT_IMPLEMENT_OVERLOAD_LIST_FOR_0_2813,
            &[&text],
        );
        self.relate_last(
            mark,
            nl,
            dg::CONSIDER_ADDING_A_DECLARE_MODIFIER_TO_THIS_CLASS_6506,
            &[],
        );
    }

    /// TS2814 at a function declaration whose symbol also has a non-ambient class
    /// declaration (in this file or another), related to that class. tsc reports it from
    /// the overload checks of the function symbol (tsc: checker.go,
    /// checkFunctionOrConstructorSymbol).
    pub(crate) fn check_function_merged_with_class(&mut self, l: Loc) {
        let Node::FunctionDecl {
            name: Some(fname), ..
        } = *self.node(l)
        else {
            return;
        };
        if !self.reporting(l.file) {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let class = self.c.decls(sym).iter().copied().find(|&d| {
            matches!(self.node(d), Node::ClassDecl { name: Some(_), .. })
                && !self.is_ambient_node(d)
        });
        let Some(class) = class else {
            return;
        };
        let Node::ClassDecl {
            name: Some(cname), ..
        } = *self.node(class)
        else {
            return;
        };
        let mark = self.diags.len();
        self.error(
            Loc::new(l.file, fname),
            dg::FUNCTION_WITH_BODIES_CAN_ONLY_MERGE_WITH_CLASSES_THAT_ARE_AMBIENT_2814,
            &[],
        );
        self.relate_last(
            mark,
            Loc::new(class.file, cname),
            dg::CONSIDER_ADDING_A_DECLARE_MODIFIER_TO_THIS_CLASS_6506,
            &[],
        );
    }

    /// Accessor rules for an object literal.
    pub(crate) fn check_object_accessors(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let Node::ObjectLit { props } = *ast.node(l.node) else {
            return;
        };
        self.check_setters_without_getters(l, props);
    }

    /// TS7032: a set accessor whose parameter has no annotation, without a getter that
    /// types the property (one with a body or a return type annotation); TS7033: a lone
    /// getter with neither (`abstract get x();`).
    fn check_setters_without_getters(&mut self, l: Loc, members: NodeList) {
        if !self.c.opts.no_implicit_any || self.c.file(l.file).is_js() {
            return;
        }
        let file = l.file;
        let ast = self.ast(file);
        let list = ast.list(members);
        let typing_getter = |ck: &Self, d: Loc| match *ck.node(d) {
            Node::MethodDef {
                kind: MethodKind::Get,
                value,
                ..
            } => fn_parts(ck.ast(d.file), value)
                .is_some_and(|p| p.body.is_some() || p.return_type.is_some()),
            _ => false,
        };
        for &m in list {
            let Node::MethodDef {
                kind: kind @ (MethodKind::Set | MethodKind::Get),
                key,
                value,
                modifiers,
                ..
            } = *ast.node(m)
            else {
                continue;
            };
            // Private members of ambient classes are not typed at all.
            if modifiers.contains(ts_r_ast::Modifiers::PRIVATE) && self.is_ambient_node(l) {
                continue;
            }
            if let Node::ComputedKey { expr } = *ast.node(key) {
                // A computed name that is no literal or unique symbol makes an accessor
                // of its own (no getter types its setter), named by its source text.
                if kind == MethodKind::Set
                    && self.late_bound_name(Loc::new(file, expr)).is_none()
                    && let Some(parts) = fn_parts(ast, value)
                    && let Some(&p) = ast
                        .list(parts.params)
                        .iter()
                        .find(|&&x| !is_this_param(ast, x))
                    && let Node::Param { type_ann: None, .. } = *ast.node(p)
                {
                    let sp = ast.span(key);
                    let text = &self.c.file(file).text;
                    if let Some(name) = text.get(sp.start as usize..sp.end as usize) {
                        let name = name.to_owned();
                        self.error(
                            Loc::new(file, key),
                            dg::PROPERTY_0_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_ITS_SET_ACCESSOR_LACKS_A_PARAMETER_TY_7032,
                            &[&name],
                        );
                    }
                }
                continue;
            }
            if !matches!(ast.node(key), Node::Ident { .. } | Node::PrivateName { .. }) {
                continue;
            }
            let Some(name) = key_text(ast, key) else {
                continue;
            };
            let Some(sym) = self.c.sym_of_decl(Loc::new(file, m)) else {
                continue;
            };
            if kind == MethodKind::Get {
                let has_setter = self.c.decls(sym).iter().any(|&d| {
                    matches!(
                        self.node(d),
                        Node::MethodDef {
                            kind: MethodKind::Set,
                            ..
                        }
                    )
                });
                if !has_setter && !typing_getter(self, Loc::new(file, m)) {
                    self.error(
                        Loc::new(file, key),
                        dg::PROPERTY_0_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_ITS_GET_ACCESSOR_LACKS_A_RETURN_TYPE_A_7033,
                        &[&name],
                    );
                }
                continue;
            }
            let decls = self.c.decls(sym).to_vec();
            if decls.iter().any(|&d| typing_getter(self, d)) {
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
