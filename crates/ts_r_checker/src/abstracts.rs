//! Abstract classes and members (TS1245, TS2515, TS2653, TS2654, TS2656).
//!
//! Rules observed from tsc 7.0.2:
//! - A non-abstract class (declared or ambient) that extends a class must implement every
//!   abstract member it inherits. One missing member: TS2515 at the class name ("member foo
//!   from class 'A'", the name unquoted); several: TS2654 listing them quoted in the base's
//!   declaration order (more than five: the first four "and N more", TS2655). Class
//!   expressions report TS2653/TS2656/TS2650 at `class`. Class and base
//!   names print as types (`'C<T>'`, `'A<number>'`). Members redeclared in the class
//!   (including `declare` fields) or inherited through merged interfaces count as
//!   implemented.
//! - An abstract method with a body: TS1245 at its name.
//! - `abstract` members of a non-abstract class (TS1244, TS1253) and `abstract` with
//!   `private`, `static` or `async` (TS1243) are modifier errors (modifiers.rs).

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId};
use ts_r_diagnostics::FileId;
use ts_r_span::Span;
use ts_r_types::{TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::fn_parts;

impl<'c, 'a> Ck<'c, 'a> {
    /// Abstract-member rules for the class declaration or expression `l`.
    pub(crate) fn check_class_abstracts(&mut self, l: Loc) {
        if !self.reporting(l.file) || self.c.file(l.file).is_js() {
            return;
        }
        let ast = self.ast(l.file);
        let (modifiers, members, name, is_expr) = match *ast.node(l.node) {
            Node::ClassDecl {
                modifiers,
                members,
                name,
                ..
            } => (modifiers, members, name, false),
            Node::ClassExpr {
                modifiers,
                members,
                name,
                ..
            } => (modifiers, members, name, true),
            _ => return,
        };
        let class_abstract = modifiers.contains(Modifiers::ABSTRACT);
        for &m in ast.list(members) {
            self.check_abstract_member(l.file, m);
        }
        if !class_abstract {
            self.check_inherited_abstracts(l, name, is_expr);
        }
    }

    /// TS1245: an abstract method with a body. (`abstract` in a class that is not
    /// abstract and with other modifiers is a modifier error, see modifiers.rs.)
    fn check_abstract_member(&mut self, file: FileId, m: NodeId) {
        let ast = self.ast(file);
        let Node::MethodDef {
            modifiers,
            key,
            kind: MethodKind::Method,
            value,
            ..
        } = *ast.node(m)
        else {
            return;
        };
        if !modifiers.contains(Modifiers::ABSTRACT) {
            return;
        }
        if fn_parts(ast, value).is_some_and(|p| p.body.is_some()) {
            let n = self.c.text(Loc::new(file, key)).to_owned();
            self.error(
                Loc::new(file, key),
                dg::METHOD_0_CANNOT_HAVE_AN_IMPLEMENTATION_BECAUSE_IT_IS_MARKED_ABSTRACT_1245,
                &[&n],
            );
        }
    }

    /// TS2515/2653/2654/2656: inherited abstract members a non-abstract class lacks.
    fn check_inherited_abstracts(&mut self, l: Loc, name: Option<NodeId>, is_expr: bool) {
        let Some(sym) = self.c.sym_of_decl(l) else {
            return;
        };
        let bases = self.base_types(sym);
        let Some(&base) = bases.first() else {
            return;
        };
        if base == TypeId::UNRESOLVED
            || self.store().is_error(base)
            || !self
                .store()
                .flags(base)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            return;
        }
        let d = self.declared_type_of_symbol(sym);
        let this_t = self.store().type_param(sym, true);
        let inst = self.with_this_arg(d, this_t);
        let mut missing: Vec<String> = Vec::new();
        for (n, p) in self.properties_of(base) {
            if !self.is_abstract_symbol(p) {
                continue;
            }
            let pname = self.store().str(n).to_owned();
            if pname.starts_with("__@") || pname.starts_with('#') {
                continue;
            }
            let mut implemented = match self.property_of_type(inst, &pname) {
                Some(dp) => !self.is_abstract_symbol(dp),
                None => false,
            };
            // Bases of merged interface declarations can supply the member.
            for &other in bases.iter().skip(1) {
                if implemented {
                    break;
                }
                if let Some(op) = self.property_of_type(other, &pname) {
                    implemented = !self.is_abstract_symbol(op);
                }
            }
            if !implemented {
                missing.push(pname);
            }
        }
        if missing.is_empty() {
            return;
        }
        let base_s = self.type_to_string(base);
        if is_expr {
            let start = self.ast(l.file).span(l.node).start;
            let span = Span::new(start, start + 5);
            if missing.len() == 1 {
                self.error_span(
                    l.file,
                    span,
                    dg::NON_ABSTRACT_CLASS_EXPRESSION_DOES_NOT_IMPLEMENT_INHERITED_ABSTRACT_MEMBER_0_FRO_2653,
                    &[&missing[0], &base_s],
                );
            } else if missing.len() > 5 {
                let list = quoted_list(&missing[..4]);
                let more = (missing.len() - 4).to_string();
                self.error_span(
                    l.file,
                    span,
                    dg::NON_ABSTRACT_CLASS_EXPRESSION_IS_MISSING_IMPLEMENTATIONS_FOR_THE_FOLLOWING_MEMBE_2650,
                    &[&base_s, &list, &more],
                );
            } else {
                let list = quoted_list(&missing);
                self.error_span(
                    l.file,
                    span,
                    dg::NON_ABSTRACT_CLASS_EXPRESSION_IS_MISSING_IMPLEMENTATIONS_FOR_THE_FOLLOWING_MEMBE_2656,
                    &[&base_s, &list],
                );
            }
            return;
        }
        let Some(name) = name else {
            return;
        };
        let class_s = self.type_to_string(d);
        let at = Loc::new(l.file, name);
        if missing.len() == 1 {
            self.error(
                at,
                dg::NON_ABSTRACT_CLASS_0_DOES_NOT_IMPLEMENT_INHERITED_ABSTRACT_MEMBER_1_FROM_CLASS_2_2515,
                &[&class_s, &missing[0], &base_s],
            );
        } else if missing.len() > 5 {
            let list = quoted_list(&missing[..4]);
            let more = (missing.len() - 4).to_string();
            self.error(
                at,
                dg::NON_ABSTRACT_CLASS_0_IS_MISSING_IMPLEMENTATIONS_FOR_THE_FOLLOWING_MEMBERS_OF_1_2_2655,
                &[&class_s, &base_s, &list, &more],
            );
        } else {
            let list = quoted_list(&missing);
            self.error(
                at,
                dg::NON_ABSTRACT_CLASS_0_IS_MISSING_IMPLEMENTATIONS_FOR_THE_FOLLOWING_MEMBERS_OF_1_2_2654,
                &[&class_s, &base_s, &list],
            );
        }
    }

    /// Whether a member symbol is declared `abstract`.
    fn is_abstract_symbol(&self, p: ts_r_types::SymId) -> bool {
        let decls = self.c.decls(p);
        !decls.is_empty()
            && decls
                .iter()
                .all(|d| self.node(*d).modifiers().contains(Modifiers::ABSTRACT))
    }
}

fn quoted_list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ")
}
