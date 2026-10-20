//! Declarations named like a predefined type (TS2414, TS2427, TS2457, TS2431, TS2368).
//!
//! Rules observed from tsc 7.0.2: a class (declaration or named expression), interface,
//! type alias, enum or type parameter named `any`, `unknown`, `never`, `number`, `bigint`,
//! `boolean`, `string`, `symbol`, `object` or `undefined` is reported at its name
//! (`intrinsic`, `void` and `null` are not: the last two do not parse as names).

use ts_r_ast::{Node, NodeId};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

const RESERVED: [&str; 10] = [
    "any",
    "unknown",
    "never",
    "number",
    "bigint",
    "boolean",
    "string",
    "symbol",
    "object",
    "undefined",
];

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the name of declaration `n` of `file` against the predefined type names.
    pub(crate) fn check_declaration_name(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let (name, info) = match *ast.node(n) {
            Node::ClassDecl {
                name: Some(name), ..
            }
            | Node::ClassExpr {
                name: Some(name), ..
            } => (name, dg::CLASS_NAME_CANNOT_BE_0_2414),
            Node::Interface { name, .. } => (name, dg::INTERFACE_NAME_CANNOT_BE_0_2427),
            Node::TypeAlias { name, .. } => (name, dg::TYPE_ALIAS_NAME_CANNOT_BE_0_2457),
            Node::Enum { name, .. } => (name, dg::ENUM_NAME_CANNOT_BE_0_2431),
            Node::TypeParam { name, .. } => (name, dg::TYPE_PARAMETER_NAME_CANNOT_BE_0_2368),
            _ => return,
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let text = self.c.text(Loc::new(file, name));
        if RESERVED.contains(&text) {
            let text = text.to_owned();
            self.error(Loc::new(file, name), info, &[&text]);
        }
    }
}
