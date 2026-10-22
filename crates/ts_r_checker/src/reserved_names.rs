//! Names the down-level emit reserves inside a scope: TS2818 (`Reflect`) and TS18027
//! (`WeakMap`, `WeakSet`). Both apply only when the target is ES2021 or older and the program
//! emits (tsc skips them under `noEmit`).
//!
//! The rules, in our words (tsc: tsc/internal/checker/checker.go, checkSuperExpression,
//! setNodeLinksForPrivateIdentifierScope, checkCollisionsForDeclarationName,
//! checkReflectCollision, checkWeakMapSetCollision):
//!
//! - A `super.x` or `super[x]` whose super container (arrow functions skipped) is a static
//!   field or a static block of a class with `extends` marks every block scope around it;
//!   the file itself only when it is a module.
//! - A class member with a private name (`#x`) marks every block scope around its class,
//!   the file included.
//! - A runtime declaration named `Reflect` collides when the nearest block scope around it
//!   is marked by a `super` access; a named class expression looks at its members and a
//!   named function expression at itself instead (their names bind inside, not outside).
//!   A declaration named `WeakMap` or `WeakSet` collides when its nearest block scope is
//!   marked by a private name. The error is at the declared name (at the whole specifier
//!   of `import { a as Reflect }`).
//! - Runtime declarations: variables, catch variables, parameters of functions with a body,
//!   binding elements of those, functions, classes, enums (`const` too), namespaces, and
//!   imports that are not type-only. Ambient code and class members never collide.
//!
//! Block scopes are tsc's: the file, a switch, a catch clause, a namespace, `for` loops,
//! functions, methods, arrows, class fields, static blocks, and blocks that are not a
//! function body.

use ts_r_ast::{Ast, HeritageKind, Modifiers, Node, NodeId};
use ts_r_config::ScriptTarget;
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Marked by a `super` property access in a static initializer.
const SUPER_IN_STATIC: u8 = 1;
/// Marked by a class with a private-named member.
const PRIVATE_NAMES: u8 = 2;

/// Upper bound on nodes walked and ancestors followed (the tree is finite; this guards
/// against a malformed parent map).
const GUARD: u32 = 5_000_000;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2818 and TS18027 for `file`.
    pub(crate) fn check_downlevel_reserved_names(&mut self, file: FileId) {
        let f = self.c.file(file);
        if self.c.opts.no_emit
            || self.c.opts.target > ScriptTarget::ES2021
            || !self.reporting(file)
            || f.kind.is_declaration()
        {
            return;
        }
        let text = &f.text;
        let reflect = text.contains("Reflect") && text.contains("super");
        let weak = (text.contains("WeakMap") || text.contains("WeakSet")) && text.contains('#');
        if !reflect && !weak {
            return;
        }
        let is_module = f.bound.is_module;
        let ast = self.ast(file);
        let parents = f.parents();
        let parent = |n: NodeId| parents.parent(n);

        // Nodes in source order, ambient subtrees left out.
        let mut nodes: Vec<NodeId> = Vec::new();
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > GUARD {
                break;
            }
            if ast.node(n).modifiers().contains(Modifiers::DECLARE) {
                continue;
            }
            nodes.push(n);
            let at = stack.len();
            ast.for_each_child(n, |c| stack.push(c));
            stack[at..].reverse();
        }

        let mut marks = vec![0u8; ast.node_count() + 1];
        let mark_scopes_from = |start: Option<NodeId>, flag: u8, marks: &mut Vec<u8>| {
            let mut cur = start;
            let mut guard = 0u32;
            while let Some(n) = cur {
                guard += 1;
                if guard > GUARD {
                    break;
                }
                let p = parent(n);
                if is_block_scope(ast, n, p)
                    && (flag != SUPER_IN_STATIC || is_module || p.is_some())
                {
                    marks[n.index()] |= flag;
                }
                cur = p;
            }
        };
        for &n in &nodes {
            match *ast.node(n) {
                Node::Super {} if reflect => {
                    let Some(access) = parent(n) else { continue };
                    let is_access = matches!(*ast.node(access),
                        Node::Member { obj, .. } | Node::Index { obj, .. } if obj == n);
                    if is_access && in_static_initializer_of_derived_class(ast, n, &parent) {
                        mark_scopes_from(Some(access), SUPER_IN_STATIC, &mut marks);
                    }
                }
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. }
                    if weak && matches!(ast.node(key), Node::PrivateName { .. }) =>
                {
                    if let Some(class) = parent(n)
                        && matches!(
                            ast.node(class),
                            Node::ClassDecl { .. } | Node::ClassExpr { .. }
                        )
                    {
                        mark_scopes_from(parent(class), PRIVATE_NAMES, &mut marks);
                    }
                }
                _ => {}
            }
        }
        if !marks.iter().any(|&m| m != 0) {
            return;
        }

        let nearest_scope = |decl: NodeId| {
            let mut cur = parent(decl);
            let mut guard = 0u32;
            while let Some(n) = cur {
                guard += 1;
                if guard > GUARD {
                    break;
                }
                let p = parent(n);
                if is_block_scope(ast, n, p) {
                    return Some(n);
                }
                cur = p;
            }
            None
        };
        let mut errors: Vec<(NodeId, &str)> = Vec::new();
        for &n in &nodes {
            let Some((decl, name)) = runtime_declaration(ast, n, &parent) else {
                continue;
            };
            let Node::Ident { name: atom } = *ast.node(name) else {
                continue;
            };
            // tsc reports at the whole `a as b` of an import specifier.
            let at = if matches!(ast.node(decl), Node::ImportSpecifier { .. }) {
                decl
            } else {
                name
            };
            match ast.atom(atom) {
                "Reflect" => {
                    let hit = match *ast.node(decl) {
                        Node::ClassExpr { members, .. } => ast
                            .list(members)
                            .iter()
                            .any(|m| marks[m.index()] & SUPER_IN_STATIC != 0),
                        Node::FunctionExpr { .. } => marks[decl.index()] & SUPER_IN_STATIC != 0,
                        _ => nearest_scope(decl)
                            .is_some_and(|s| marks[s.index()] & SUPER_IN_STATIC != 0),
                    };
                    if hit {
                        errors.push((at, "Reflect"));
                    }
                }
                w @ ("WeakMap" | "WeakSet") => {
                    if nearest_scope(decl).is_some_and(|s| marks[s.index()] & PRIVATE_NAMES != 0) {
                        errors.push((at, w));
                    }
                }
                _ => {}
            }
        }
        for (name, reserved) in errors {
            let l = Loc::new(file, name);
            if reserved == "Reflect" {
                self.error(
                    l,
                    dg::DUPLICATE_IDENTIFIER_0_COMPILER_RESERVES_NAME_1_WHEN_EMITTING_SUPER_REFERENCES_I_2818,
                    &["Reflect", "Reflect"],
                );
            } else {
                self.error(
                    l,
                    dg::COMPILER_RESERVES_NAME_0_WHEN_EMITTING_PRIVATE_IDENTIFIER_DOWNLEVEL_18027,
                    &[reserved],
                );
            }
        }
    }
}

/// Is `n` (with parent `p`) a block scope?
fn is_block_scope(ast: &Ast, n: NodeId, p: Option<NodeId>) -> bool {
    match ast.node(n) {
        Node::Program { .. }
        | Node::Switch { .. }
        | Node::CatchClause { .. }
        | Node::Module { .. }
        | Node::For { .. }
        | Node::ForIn { .. }
        | Node::ForOf { .. }
        | Node::FunctionDecl { .. }
        | Node::Arrow { .. }
        | Node::MethodDef { .. }
        | Node::PropertyDef { .. }
        | Node::StaticBlock { .. } => true,
        // A method's function is part of its `MethodDef`.
        Node::FunctionExpr { .. } => {
            !p.is_some_and(|p| matches!(ast.node(p), Node::MethodDef { .. }))
        }
        Node::Block { .. } => !p.is_some_and(|p| {
            matches!(
                ast.node(p),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            )
        }),
        _ => false,
    }
}

/// Is the super keyword `sup` in a static field or static block (arrow functions looked
/// through) of a class with `extends`?
fn in_static_initializer_of_derived_class(
    ast: &Ast,
    sup: NodeId,
    parent: &impl Fn(NodeId) -> Option<NodeId>,
) -> bool {
    let mut cur = parent(sup);
    let mut guard = 0u32;
    let container = loop {
        let Some(n) = cur else { return false };
        guard += 1;
        if guard > GUARD {
            return false;
        }
        match ast.node(n) {
            // A computed member name belongs to the scope outside its member.
            Node::ComputedKey { .. } => cur = parent(n).and_then(&parent),
            Node::Arrow { .. } => cur = parent(n),
            Node::PropertyDef { .. } | Node::StaticBlock { .. } => break n,
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::MethodDef { .. }
            | Node::PropertySignature { .. }
            | Node::MethodSignature { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Program { .. } => return false,
            _ => cur = parent(n),
        }
    };
    if let Node::PropertyDef { modifiers, .. } = *ast.node(container)
        && !modifiers.contains(Modifiers::STATIC)
    {
        return false;
    }
    let Some(class) = parent(container) else {
        return false;
    };
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return false;
    };
    ast.list(heritage).iter().any(|&h| {
        matches!(
            ast.node(h),
            Node::HeritageClause {
                kind: HeritageKind::Extends,
                ..
            }
        )
    })
}

/// If `n` declares a runtime name: the declaration and its name node.
fn runtime_declaration(
    ast: &Ast,
    n: NodeId,
    parent: &impl Fn(NodeId) -> Option<NodeId>,
) -> Option<(NodeId, NodeId)> {
    match *ast.node(n) {
        Node::VarDeclarator { name, .. } => Some((n, name)),
        Node::CatchClause {
            param: Some(name), ..
        } => Some((n, name)),
        Node::Param { pattern, .. } => param_has_body(ast, n, parent).then_some((n, pattern)),
        Node::FunctionDecl {
            name: Some(name), ..
        }
        | Node::FunctionExpr {
            name: Some(name), ..
        }
        | Node::ClassDecl {
            name: Some(name), ..
        }
        | Node::ClassExpr {
            name: Some(name), ..
        } => Some((n, name)),
        Node::Enum { name, .. } | Node::Module { name, .. } => Some((n, name)),
        Node::ImportEquals {
            type_only: false,
            name,
            ..
        } => Some((n, name)),
        Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local } => {
            (!import_decl_type_only(ast, n, parent)).then_some((n, local))
        }
        Node::ImportSpecifier {
            type_only, local, ..
        } => (!type_only && !import_decl_type_only(ast, n, parent)).then_some((n, local)),
        Node::Ident { .. } => binding_element(ast, n, parent).then_some((n, n)),
        _ => None,
    }
}

/// Does the function of parameter `p` have a body (it is not an overload or a signature)?
fn param_has_body(ast: &Ast, p: NodeId, parent: &impl Fn(NodeId) -> Option<NodeId>) -> bool {
    parent(p).is_some_and(|f| match ast.node(f) {
        Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body.is_some(),
        Node::Arrow { .. } => true,
        _ => false,
    })
}

/// Is the import specifier `n` in an `import type` declaration?
fn import_decl_type_only(ast: &Ast, n: NodeId, parent: &impl Fn(NodeId) -> Option<NodeId>) -> bool {
    let mut cur = parent(n);
    for _ in 0..4 {
        let Some(p) = cur else { return false };
        if let Node::ImportDecl { type_only, .. } = *ast.node(p) {
            return type_only;
        }
        cur = parent(p);
    }
    false
}

/// Is the identifier `id` the name of a binding element in a variable, catch variable or
/// parameter (of a function with a body) destructuring pattern?
fn binding_element(ast: &Ast, id: NodeId, parent: &impl Fn(NodeId) -> Option<NodeId>) -> bool {
    let Some(p) = parent(id) else { return false };
    let names = match *ast.node(p) {
        Node::BindingProperty { key, value, .. } => {
            if value.is_some() {
                value == Some(id)
            } else {
                key == id
            }
        }
        Node::ArrayPattern { .. } | Node::Rest { .. } => true,
        Node::AssignPattern { left, .. } => left == id,
        _ => false,
    };
    if !names {
        return false;
    }
    // Up through the patterns to what declares them.
    let mut cur = Some(p);
    let mut guard = 0u32;
    while let Some(n) = cur {
        guard += 1;
        if guard > GUARD {
            return false;
        }
        match ast.node(n) {
            Node::BindingProperty { .. }
            | Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. } => cur = parent(n),
            Node::VarDeclarator { .. } | Node::CatchClause { .. } => return true,
            Node::Param { .. } => return param_has_body(ast, n, parent),
            _ => return false,
        }
    }
    false
}
