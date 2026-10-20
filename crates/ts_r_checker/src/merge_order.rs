//! Declaration merges that depend on placement (TS2433, TS2434, TS2432).
//!
//! Rules observed from tsc 7.0.2:
//! - An instantiated, non-ambient namespace merged with a class or function that has an
//!   implementation (not `declare`, a body for functions): TS2433 at the namespace name when
//!   that class or function is in another file, TS2434 when it comes later in the file.
//!   Namespaces holding only types are fine.
//! - Enum declarations of one symbol: each one after the first whose first member has no
//!   initializer is reported at that member (TS2432).

use ts_r_ast::{Modifiers, Node, NodeId};
use ts_r_binder::SymbolFlags;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Node budget of the syntactic "is this namespace instantiated" walk.
const MAX_DEPTH: u32 = 64;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks namespace declaration `d` against a class or function it merges with.
    pub(crate) fn check_namespace_merge_order(&mut self, d: Loc) {
        if !self.reporting(d.file) || self.c.file(d.file).is_js() {
            return;
        }
        let ast = self.ast(d.file);
        let Node::Module {
            name,
            body: Some(body),
            ..
        } = *ast.node(d.node)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) || self.is_ambient_node(d) {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(d) else {
            return;
        };
        let flags = self.c.sym_flags(sym);
        if !flags.intersects(SymbolFlags::VALUE_MODULE)
            || !flags.intersects(SymbolFlags::CLASS | SymbolFlags::FUNCTION)
            || !namespace_instantiated(ast, body, 0)
        {
            return;
        }
        let target = self.c.decls(sym).into_iter().find(|l| {
            let lt = self.ast(l.file);
            match *lt.node(l.node) {
                Node::ClassDecl { modifiers, .. } => !modifiers.contains(Modifiers::DECLARE),
                Node::FunctionDecl { body: Some(_), .. } => true,
                _ => false,
            }
        });
        let Some(target) = target else {
            return;
        };
        if self.is_ambient_node(target) {
            return;
        }
        let info = if target.file != d.file {
            dg::A_NAMESPACE_DECLARATION_CANNOT_BE_IN_A_DIFFERENT_FILE_FROM_A_CLASS_OR_FUNCTION_W_2433
        } else if self.c.span(d).start < self.c.span(target).start {
            dg::A_NAMESPACE_DECLARATION_CANNOT_BE_LOCATED_PRIOR_TO_A_CLASS_OR_FUNCTION_WITH_WHIC_2434
        } else {
            return;
        };
        self.error(Loc::new(d.file, name), info, &[]);
    }

    /// Checks enum declaration `d`: only one declaration of the enum may start with a member
    /// without an initializer.
    pub(crate) fn check_enum_merge_initializers(&mut self, d: Loc) {
        if !self.reporting(d.file) {
            return;
        }
        let Some(first) = self.first_uninitialized_enum_member(d) else {
            return;
        };
        let Some(sym) = self.c.sym_of_decl(d) else {
            return;
        };
        for other in self.c.decls(sym) {
            if other == d {
                return;
            }
            if self.first_uninitialized_enum_member(other).is_some() {
                self.error(
                    Loc::new(d.file, first),
                    dg::IN_AN_ENUM_WITH_MULTIPLE_DECLARATIONS_ONLY_ONE_DECLARATION_CAN_OMIT_AN_INITIALIZ_2432,
                    &[],
                );
                return;
            }
        }
    }

    /// The name of the first member of enum declaration `d` when it has no initializer.
    fn first_uninitialized_enum_member(&self, d: Loc) -> Option<NodeId> {
        let ast = self.ast(d.file);
        let Node::Enum { members, .. } = *ast.node(d.node) else {
            return None;
        };
        let &m = ast.list(members).first()?;
        match *ast.node(m) {
            Node::EnumMember { name, init: None } => Some(name),
            _ => None,
        }
    }
}

/// Whether a namespace body declares values (syntactically: anything but interfaces, type
/// aliases, imports of types, const enums and namespaces holding only those).
pub(crate) fn namespace_instantiated(ast: &ts_r_ast::Ast, body: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let stmts = match *ast.node(body) {
        Node::ModuleBlock { stmts } => stmts,
        Node::Module { body: Some(b), .. } => return namespace_instantiated(ast, b, depth + 1),
        _ => return false,
    };
    ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::Interface { .. } | Node::TypeAlias { .. } | Node::ImportDecl { .. } => false,
        Node::ImportEquals { type_only, .. } => !type_only,
        Node::Enum { modifiers, .. } => !modifiers.contains(Modifiers::CONST),
        Node::Module { body: Some(b), .. } => namespace_instantiated(ast, b, depth + 1),
        Node::Module { body: None, .. } => false,
        _ => true,
    })
}
