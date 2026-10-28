//! Which meanings (value, type, namespace) a reference search covers.
//!
//! A name that is declared as several things at once (an interface, a namespace and a
//! function named `Foo`) is searched only in the meanings of the location the search starts
//! from, widened by every declaration sharing one of them (a class is a value and a type, so
//! a search from a value use also finds type uses of the class). Every candidate reference
//! must be used in one of those meanings. Rename searches every meaning. Rule from
//! `tsc: tsc/internal/ls/utilities.go` (`getMeaningFromLocation`,
//! `getMeaningFromDeclaration`, `getIntersectingMeaningFromDeclarations`), v7.0.2.

use ts_r_ast::{HeritageKind, Node};
use ts_r_binder::{SymbolFlags, decl_name_node, meaning};
use ts_r_checker::{Ck, Loc};

/// The meanings a declaration node gives its name.
pub fn decl_meaning(ck: &Ck<'_, '_>, d: Loc) -> u8 {
    let c = ck.c;
    match *c.node(d) {
        Node::VarDeclarator { .. }
        | Node::Param { .. }
        | Node::BindingProperty { .. }
        | Node::Ident { .. }
        | Node::PropertyDef { .. }
        | Node::PropertySignature { .. }
        | Node::Property { .. }
        | Node::ShorthandProp { .. }
        | Node::MethodDef { .. }
        | Node::MethodSignature { .. }
        | Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::CatchClause { .. }
        | Node::JsxAttr { .. } => meaning::VALUE,
        Node::TypeParam { .. }
        | Node::Interface { .. }
        | Node::TypeAlias { .. }
        | Node::TypeLiteral { .. } => meaning::TYPE,
        Node::EnumMember { .. } | Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
            meaning::VALUE | meaning::TYPE
        }
        Node::Module { name, .. } => {
            let ambient = matches!(c.node(Loc::new(d.file, name)), Node::StringLit { .. });
            let inst = c
                .sym_of_decl(d)
                .is_some_and(|s| c.sym_flags(s).intersects(SymbolFlags::VALUE_MODULE));
            if ambient || inst {
                meaning::NAMESPACE | meaning::VALUE
            } else {
                meaning::NAMESPACE
            }
        }
        _ => meaning::ALL,
    }
}

/// The meanings the name at `l` is used in.
pub fn location_meaning(ck: &Ck<'_, '_>, l: Loc) -> u8 {
    let c = ck.c;
    let ast = c.ast(l.file);
    let Some(p) = c.parent(l) else {
        return meaning::VALUE;
    };
    match *ast.node(p.node) {
        Node::ExportAssignment { .. }
        | Node::ExportDefaultExpr { .. }
        | Node::ExportSpecifier { .. }
        | Node::ExternalModuleRef { .. }
        | Node::ImportSpecifier { .. }
        | Node::ImportDefaultSpecifier { .. }
        | Node::ImportNamespaceSpecifier { .. } => return meaning::ALL,
        Node::ImportEquals { name, .. } if name == l.node => return meaning::ALL,
        _ => {}
    }
    // `import a = b.c.d`: the whole reference is everything, its qualifiers namespaces.
    let mut top = l;
    while let Some(q) = c.parent(top)
        && matches!(c.node(q), Node::QualifiedName { .. })
    {
        top = q;
    }
    if let Some(q) = c.parent(top)
        && let Node::ImportEquals { module_ref, .. } = *c.node(q)
        && module_ref == top.node
    {
        let whole = top == l
            || matches!(*c.node(p), Node::QualifiedName { right, .. } if right == l.node && p == top);
        return if whole {
            meaning::ALL
        } else {
            meaning::NAMESPACE
        };
    }
    if decl_name_node(ast, p.node) == Some(l.node) && c.sym_of_decl(p).is_some() {
        return decl_meaning(ck, p);
    }
    if c.sym_of_decl(l).is_some() {
        return meaning::VALUE;
    }
    // Type references: `T`, `A.T`, `implements A.T`.
    let (node, parent) = match *c.node(p) {
        Node::QualifiedName { right, .. } | Node::Member { prop: right, .. } if right == l.node => {
            (p, c.parent(p))
        }
        _ => (l, Some(p)),
    };
    if let Some(pp) = parent {
        match *c.node(pp) {
            Node::TypeRef { name, .. } if name == node.node => return meaning::TYPE,
            Node::ImportType {
                is_typeof: false,
                qualifier: Some(q),
                ..
            } if q == node.node => return meaning::TYPE,
            Node::ExprWithTypeArgs { expr, .. }
                if expr == node.node && in_type_heritage(ck, pp) =>
            {
                return meaning::TYPE;
            }
            _ => {}
        }
    }
    // Namespace qualifiers: every component but the last of `A.B.T` in a type (an import
    // type's qualifier too) or of `A.B.C` in `implements A.B.C`.
    if matches!(*c.node(p), Node::QualifiedName { .. }) {
        let mut root = p;
        while let Some(q) = c.parent(root)
            && matches!(c.node(q), Node::QualifiedName { .. })
        {
            root = q;
        }
        let last = matches!(*c.node(root), Node::QualifiedName { right, .. } if right == l.node);
        if !last
            && c.parent(root).is_some_and(|r| match *c.node(r) {
                Node::TypeRef { .. } => true,
                // `import("m").A.T`: `A` names a namespace too.
                Node::ImportType {
                    is_typeof: false,
                    qualifier: Some(q),
                    ..
                } => q == root.node,
                _ => false,
            })
        {
            return meaning::NAMESPACE;
        }
    }
    if matches!(*c.node(p), Node::Member { .. }) {
        let mut root = p;
        while let Some(q) = c.parent(root)
            && matches!(c.node(q), Node::Member { .. })
        {
            root = q;
        }
        let last = matches!(*c.node(root), Node::Member { prop, .. } if prop == l.node);
        if !last
            && let Some(r) = c.parent(root)
            && matches!(c.node(r), Node::ExprWithTypeArgs { .. })
            && in_type_heritage(ck, r)
        {
            return meaning::NAMESPACE;
        }
    }
    if matches!(c.node(p), Node::TypeParam { .. }) {
        return meaning::TYPE;
    }
    meaning::VALUE
}

/// Whether an `ExprWithTypeArgs` is in a type position: `implements`, or an interface's
/// `extends` (a class's `extends` is a value).
fn in_type_heritage(ck: &Ck<'_, '_>, ewta: Loc) -> bool {
    let c = ck.c;
    match c.parent(ewta).map(|h| (h, *c.node(h))) {
        Some((h, Node::HeritageClause { kind, .. })) => {
            let class = c.parent(h).is_some_and(|k| {
                matches!(c.node(k), Node::ClassDecl { .. } | Node::ClassExpr { .. })
            });
            !class || kind == HeritageKind::Implements
        }
        // Interface `extends` lists hold the `ExprWithTypeArgs` directly.
        Some((_, Node::Interface { .. })) => true,
        _ => false,
    }
}

/// The meanings a search from `l` for a symbol with declarations `decls` covers.
pub fn search_meaning(ck: &Ck<'_, '_>, l: Loc, decls: &[Loc]) -> u8 {
    let mut m = location_meaning(ck, l);
    if decls.is_empty() {
        return m;
    }
    // Widen until stable (at most three bits can be added).
    for _ in 0..4 {
        let before = m;
        for &d in decls {
            let dm = decl_meaning(ck, d);
            if dm & m != 0 {
                m |= dm;
            }
        }
        if m == before {
            break;
        }
    }
    m
}
