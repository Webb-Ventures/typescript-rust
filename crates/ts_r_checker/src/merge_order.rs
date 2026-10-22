//! Declaration merges that depend on placement or on class-owned names (TS2433, TS2434, TS2432, TS2300).
//!
//! Rules observed from tsc 7.0.2:
//! - An instantiated, non-ambient namespace merged with a class or function that has an
//!   implementation (not `declare`, a body for functions): TS2433 at the namespace name when
//!   that class or function is in another file, TS2434 when it comes later in the file.
//!   Namespaces holding only types are fine. A local namespace counts an exported class
//!   or function of its name in the same block (they share that block's local symbol).
//! - A class owns a static `prototype` property, so a value exported as `prototype` from a
//!   namespace merged with a class (exported explicitly, or implicitly in an ambient
//!   namespace: variables, functions, classes, enums, instantiated namespaces) is TS2300
//!   at its name, in either order and across files (tsc: bindClassLikeDeclaration in
//!   binder.go and mergeSymbol in checker.go).
//! - Enum declarations of one symbol: each one after the first whose first member has no
//!   initializer is reported at that member (TS2432).

use ts_r_ast::{Modifiers, Node, NodeId};
use ts_r_binder::SymbolFlags;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Node budget of the syntactic "is this namespace instantiated" walk.
const MAX_DEPTH: u32 = 64;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2300 at each value named `prototype` that the namespace `d` exports into a class.
    pub(crate) fn check_namespace_prototype_export(&mut self, d: Loc) {
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
        let Node::ModuleBlock { stmts } = *ast.node(body) else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(d) else {
            return;
        };
        if !self.c.sym_flags(sym).intersects(SymbolFlags::CLASS) {
            return;
        }
        let ambient = self.is_ambient_node(d);
        let mut names = Vec::new();
        for &st in ast.list(stmts) {
            let node = ast.node(st);
            if !ambient && !node.modifiers().contains(Modifiers::EXPORT) {
                continue;
            }
            match *node {
                Node::VarDecl { decls, .. } => {
                    for &v in ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(v)
                            && matches!(ast.node(name), Node::Ident { .. })
                        {
                            names.push(name);
                        }
                    }
                }
                Node::FunctionDecl { name: Some(n), .. }
                | Node::ClassDecl { name: Some(n), .. }
                | Node::Enum { name: n, .. } => names.push(n),
                Node::Module {
                    name: n,
                    body: Some(b),
                    ..
                } if matches!(ast.node(n), Node::Ident { .. })
                    && namespace_instantiated(ast, b, 0) =>
                {
                    names.push(n)
                }
                _ => {}
            }
        }
        for n in names {
            let at = Loc::new(d.file, n);
            if self.c.text(at) == "prototype" {
                self.error(at, dg::DUPLICATE_IDENTIFIER_0_2300, &["prototype"]);
            }
        }
    }

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
        if self.is_ambient_node(d) || !namespace_instantiated(ast, body, 0) {
            return;
        }
        // `namespace A.B.C {}` declares `A` and `B` at their names and `C` at the
        // declaration; each part is checked against what it merges with.
        let mut parts = Vec::new();
        let mut cur = name;
        while let Node::QualifiedName { left, right } = *ast.node(cur) {
            parts.push((right, None));
            cur = left;
        }
        if !matches!(ast.node(cur), Node::Ident { .. }) {
            return;
        }
        parts.push((cur, None));
        parts.reverse();
        if let Some(last) = parts.last_mut() {
            last.1 = Some(d);
        }
        for (part, decl) in parts {
            let decl = decl.unwrap_or(Loc::new(d.file, part));
            self.check_namespace_part_merge_order(d, part, decl);
        }
    }

    /// Checks one name `part` of namespace declaration `d` (declared by `decl`).
    fn check_namespace_part_merge_order(&mut self, d: Loc, part: NodeId, decl: Loc) {
        let Some(sym) = self.c.sym_of_decl(decl) else {
            return;
        };
        let flags = self.c.sym_flags(sym);
        if !flags.intersects(SymbolFlags::VALUE_MODULE) {
            return;
        }
        let implemented = |ck: &Self, l: Loc| match *ck.ast(l.file).node(l.node) {
            Node::ClassDecl { modifiers, .. } => !modifiers.contains(Modifiers::DECLARE),
            Node::FunctionDecl { body: Some(_), .. } => true,
            _ => false,
        };
        let target = if flags.intersects(SymbolFlags::CLASS | SymbolFlags::FUNCTION) {
            self.c
                .decls(sym)
                .into_iter()
                .find(|&l| implemented(self, l))
        } else if decl == d
            && !self
                .ast(d.file)
                .node(d.node)
                .modifiers()
                .contains(Modifiers::EXPORT)
        {
            // A local namespace and an exported class or function of its name in the same
            // block share the block's local symbol (the binder keeps them apart).
            self.sibling_exported_class_or_function(d, part)
                .filter(|&l| implemented(self, l))
        } else {
            None
        };
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
        self.error(Loc::new(d.file, part), info, &[]);
    }

    /// An exported class or function named like `part` among the statements of the block
    /// holding declaration `d`.
    fn sibling_exported_class_or_function(&self, d: Loc, part: NodeId) -> Option<Loc> {
        let ast = self.ast(d.file);
        let text = self.c.text(Loc::new(d.file, part));
        let parent = self.c.file(d.file).parents().parent(d.node)?;
        let stmts = match *ast.node(parent) {
            Node::ModuleBlock { stmts } | Node::Program { stmts } => stmts,
            _ => return None,
        };
        ast.list(stmts).iter().copied().find_map(|st| {
            let n = ast.node(st);
            let name = match *n {
                Node::ClassDecl { name: Some(nm), .. }
                | Node::FunctionDecl { name: Some(nm), .. } => nm,
                _ => return None,
            };
            (n.modifiers().contains(Modifiers::EXPORT)
                && self.c.text(Loc::new(d.file, name)) == text)
                .then_some(Loc::new(d.file, st))
        })
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
