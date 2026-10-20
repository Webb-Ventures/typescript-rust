//! Implicitly `any` members of interfaces and type literals (TS7008, TS7032, TS7033).
//!
//! Rules observed from tsc 7.0.2, under `noImplicitAny` in TypeScript files:
//! - A property signature without a type annotation is TS7008 at its name, printed as
//!   written (quotes included). Computed names are not reported, nor are files with syntax
//!   errors (our parser recovers members tsc does not).
//! - Accessor signatures: when neither the getter's return nor the setter's parameter is
//!   annotated, the setter is TS7032 (or a lone getter TS7033) at its name. The setter's
//!   parameter is TS7006 only when there is no getter: a getter (annotated or not) types it.
//!   An accessor written with a body (TS1183) is typed by its body.

use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS7008 for the property signature `n`.
    pub(crate) fn check_property_signature_implicit_any(&mut self, file: FileId, n: NodeId) {
        if !self.c.opts.no_implicit_any || !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        let Node::PropertySignature {
            key,
            type_ann: None,
            ..
        } = *ast.node(n)
        else {
            return;
        };
        if !matches!(
            ast.node(key),
            Node::Ident { .. } | Node::NumberLit { .. } | Node::StringLit { .. }
        ) || self.has_syntax_errors(file)
        {
            return;
        }
        let name = self.c.text(Loc::new(file, key)).to_owned();
        self.error(
            Loc::new(file, key),
            dg::MEMBER_0_IMPLICITLY_HAS_AN_1_TYPE_7008,
            &[&name, "any"],
        );
    }

    /// TS7032/TS7033 for the accessor signature `n`.
    pub(crate) fn check_accessor_signature_implicit_any(&mut self, file: FileId, n: NodeId) {
        if !self.c.opts.no_implicit_any || !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        let Node::MethodSignature { kind, key, .. } = *ast.node(n) else {
            return;
        };
        if !matches!(kind, MethodKind::Get | MethodKind::Set) {
            return;
        }
        let Some(name) = crate::nodes::ident_text(ast, key) else {
            return;
        };
        // An accessor written with a body (TS1183) takes its type from the body.
        let pair = self.accessor_signature_pair(file, n);
        if pair.iter().any(|&(f, d)| self.followed_by_body(f, d)) {
            return;
        }
        if pair
            .iter()
            .any(|&(f, d)| self.accessor_signature_annotated(f, d))
        {
            return;
        }
        let has_setter = pair.iter().any(|&(f, d)| {
            matches!(
                self.ast(f).node(d),
                Node::MethodSignature {
                    kind: MethodKind::Set,
                    ..
                }
            )
        });
        let info = match kind {
            MethodKind::Set => {
                dg::PROPERTY_0_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_ITS_SET_ACCESSOR_LACKS_A_PARAMETER_TY_7032
            }
            _ if !has_setter => {
                dg::PROPERTY_0_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_ITS_GET_ACCESSOR_LACKS_A_RETURN_TYPE_A_7033
            }
            _ => return,
        };
        self.error(Loc::new(file, key), info, &[name]);
    }

    /// The parameter `p` of a set accessor signature that has a getter, which types it.
    pub(crate) fn is_setter_param_typed_by_getter(&self, file: FileId, p: NodeId) -> bool {
        let Some(sig) = self.c.file(file).parents().parent(p) else {
            return false;
        };
        if !matches!(
            self.ast(file).node(sig),
            Node::MethodSignature {
                kind: MethodKind::Set,
                ..
            }
        ) {
            return false;
        }
        self.accessor_signature_pair(file, sig)
            .iter()
            .any(|&(f, d)| {
                matches!(
                    self.ast(f).node(d),
                    Node::MethodSignature {
                        kind: MethodKind::Get,
                        ..
                    }
                )
            })
    }

    /// The accessor signatures declaring the same member as `n` (`n` included).
    fn accessor_signature_pair(&self, file: FileId, n: NodeId) -> Vec<(FileId, NodeId)> {
        let Some(sym) = self.c.sym_of_decl(Loc::new(file, n)) else {
            return vec![(file, n)];
        };
        self.c
            .decls(sym)
            .into_iter()
            .filter(|d| {
                matches!(
                    self.ast(d.file).node(d.node),
                    Node::MethodSignature {
                        kind: MethodKind::Get | MethodKind::Set,
                        ..
                    }
                )
            })
            .map(|d| (d.file, d.node))
            .collect()
    }

    /// The unannotated accessor signature `n` was written with a body (`get x() { .. }`,
    /// TS1183): its text has a `{` after the parameter list.
    fn followed_by_body(&self, file: FileId, n: NodeId) -> bool {
        let sp = self.ast(file).span(n);
        let Some(text) = self
            .c
            .file(file)
            .text
            .get(sp.start as usize..sp.end as usize)
        else {
            return false;
        };
        text.find(')')
            .is_some_and(|close| text[close..].contains('{'))
    }

    /// A getter signature with a return type or a setter signature with a typed parameter.
    fn accessor_signature_annotated(&self, file: FileId, n: NodeId) -> bool {
        let ast = self.ast(file);
        let Node::MethodSignature {
            kind,
            params,
            return_type,
            ..
        } = *ast.node(n)
        else {
            return false;
        };
        match kind {
            MethodKind::Get => return_type.is_some(),
            _ => ast.list(params).iter().any(|&p| {
                matches!(
                    ast.node(p),
                    Node::Param {
                        type_ann: Some(_),
                        ..
                    }
                )
            }),
        }
    }
}
