//! References to a string literal that names no symbol (`"option 1"` of a union of
//! literal types, `f.f = 'foo'`): the literals of the same text in the program whose type
//! is the same. Rule read in `tsc: tsc/internal/ls/findallreferences.go`
//! (`getReferencesForStringLiteral`) and `tsc: tsc/internal/ls/utilities.go`, restated:
//!
//! - a literal's type is its contextual type (for a `case` test, the switched expression's
//!   type; for an operand of `==`/`===`/`!=`/`!==`, the other operand's type), else, in a
//!   type, the type of the outermost type node around it (a type node's parent chain up
//!   through type nodes and type members: `"a" | "b"` of `type T = "a" | "b"`);
//! - with a type (other than `string`), a literal matches when its type is that same type,
//!   or when it is the quoted name of a property signature (`"Prop 1": string`);
//! - without one, every literal of the text matches, except a template that spans lines.
//!
//! Spans exclude the quotes. Lib and `node_modules` files are searched too.

use ts_r_ast::{Ast, BinaryOp, Node, NodeId};
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{TypeId, TypeKind};

/// Enclosing type nodes walked at most.
const MAX_TYPE_DEPTH: u32 = 256;

/// The text of `n` when it is a string literal or a template without substitutions.
pub(super) fn literal_text(ast: &Ast, n: NodeId) -> Option<&str> {
    match *ast.node(n) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[only] => match *ast.node(only) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// A node that is a type built of other types.
fn is_type_container(n: &Node) -> bool {
    matches!(
        n,
        Node::TypeRef { .. }
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::TypeLiteral { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::MappedType { .. }
            | Node::ParenType { .. }
            | Node::TypePredicate { .. }
            | Node::TypeArgs { .. }
    )
}

/// A member of a type literal or interface.
fn is_type_member(n: &Node) -> bool {
    matches!(
        n,
        Node::PropertySignature { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::IndexSignature { .. }
    )
}

/// Whether `child` is written as a type of `parent`.
fn holds_type(ast: &Ast, parent: NodeId, child: NodeId) -> bool {
    let some = Some(child);
    match *ast.node(parent) {
        Node::ImportType { arg, .. } => arg != child,
        Node::TypeRef { name, .. } => name != child,
        ref n if is_type_container(n) => true,
        Node::VarDeclarator { type_ann, .. }
        | Node::Param { type_ann, .. }
        | Node::PropertyDef { type_ann, .. }
        | Node::PropertySignature { type_ann, .. }
        | Node::IndexSignature { type_ann, .. } => type_ann == some,
        Node::TypeAlias { ty, .. }
        | Node::As { ty, .. }
        | Node::Satisfies { ty, .. }
        | Node::TypeAssertion { ty, .. } => ty == child,
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. }
        | Node::MethodSignature { return_type, .. }
        | Node::CallSignature { return_type, .. }
        | Node::ConstructSignature { return_type, .. } => return_type == some,
        _ => false,
    }
}

/// The outermost type node around the literal type `lit`, or `None` when `lit` is no type.
fn ancestor_type(c: &Checker<'_>, lit: Loc) -> Option<Loc> {
    let ast = c.ast(lit.file);
    let p = c.parent(lit)?;
    if !holds_type(ast, p.node, lit.node) {
        return None;
    }
    let mut last = lit;
    let mut n = lit;
    for _ in 0..MAX_TYPE_DEPTH {
        let Some(p) = c.parent(n) else { break };
        let node = ast.node(p.node);
        if is_type_container(node) {
            last = p;
        } else if !is_type_member(node) {
            break;
        }
        n = p;
    }
    Some(last)
}

/// The type a string literal is compared with (see the module docs).
fn literal_type(ck: &mut Ck<'_, '_>, lit: Loc) -> Option<TypeId> {
    let c = ck.c;
    if let Some(p) = c.parent(lit) {
        match *c.node(p) {
            Node::SwitchCase { test: Some(t), .. } if t == lit.node => {
                let sw = c.parent(p)?;
                if let Node::Switch { disc, .. } = *c.node(sw) {
                    return Some(ck.check_expr(Loc::new(lit.file, disc)));
                }
            }
            Node::Binary {
                left,
                op: BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq,
                right,
            } => {
                let other = if left == lit.node { right } else { left };
                return Some(ck.check_expr(Loc::new(lit.file, other)));
            }
            _ => {}
        }
    }
    if let Some(t) = ancestor_type(c, lit) {
        // The type of an alias is the alias's type (the one its uses get).
        if let Some(p) = c.parent(t)
            && matches!(*c.node(p), Node::TypeAlias { ty, .. } if ty == t.node)
            && let Some(s) = c.sym_of_decl(p)
        {
            return Some(ck.declared_type_of_symbol(s));
        }
        return Some(ck.type_from_node(t));
    }
    ck.contextual_type(lit)
}

/// Every string literal and substitution-free template in `ast`, in source order.
fn literals_in(ast: &Ast) -> Vec<NodeId> {
    struct V(Vec<NodeId>);
    impl ts_r_ast::visit::Visitor for V {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            if literal_text(ast, id).is_some() {
                self.0.push(id);
            }
            ts_r_ast::visit::Walk::Continue
        }
    }
    let mut v = V(Vec::new());
    ts_r_ast::visit::walk(ast, ast.root, &mut v);
    v.0
}

/// The span of a literal's text, without its quotes.
pub(super) fn inner_span(ast: &Ast, n: NodeId) -> (u32, u32) {
    let s = ast.span(n);
    (s.start + 1, s.end.saturating_sub(1).max(s.start + 1))
}

/// The literals in `files` that match the literal `lit` (see the module docs), as spans
/// in file order then source order.
pub(super) fn references(
    ck: &mut Ck<'_, '_>,
    lit: Loc,
    files: &[FileId],
) -> Vec<(FileId, u32, u32)> {
    let c = ck.c;
    let Some(text) = literal_text(c.ast(lit.file), lit.node).map(str::to_string) else {
        return Vec::new();
    };
    let t = literal_type(ck, lit);
    if t == Some(TypeId::STRING) {
        return Vec::new();
    }
    // Text that needs no escape appears in the source as written wherever a literal holds
    // it; other text may be written with escapes, so every file's literals are read.
    let plain = text
        .bytes()
        .all(|b| (b' '..=b'~').contains(&b) && !matches!(b, b'\\' | b'"' | b'\'' | b'`' | b'$'));
    let mut out = Vec::new();
    for &f in files {
        let src = c.file(f).text.as_str();
        if plain && !src.contains(text.as_str()) {
            continue;
        }
        let ast = c.ast(f);
        for n in literals_in(ast) {
            if literal_text(ast, n) != Some(text.as_str()) {
                continue;
            }
            let l = Loc::new(f, n);
            let ok = match t {
                Some(t) => l == lit || is_property_name(c, l) || literal_type(ck, l) == Some(t),
                None => {
                    let span = ast.span(n);
                    !matches!(ast.node(n), Node::TemplateLit { .. })
                        || !src
                            .get(span.start as usize..span.end as usize)
                            .is_some_and(|s| s.contains('\n'))
                }
            };
            if ok {
                let (s, e) = inner_span(ast, n);
                out.push((f, s, e));
            }
        }
    }
    out
}

/// Whether the literal is the quoted name of a property signature.
fn is_property_name(c: &Checker<'_>, l: Loc) -> bool {
    c.parent(l)
        .is_some_and(|p| matches!(*c.node(p), Node::PropertySignature { key, .. } if key == l.node))
}

/// Whether the literal `lit` may be renamed: its type is a string literal type or a union
/// of them (`tsc: tsc/internal/ls/rename.go`), or it is a quoted export name; a module
/// specifier or a plain string is not.
pub(super) fn renamable(ck: &mut Ck<'_, '_>, lit: Loc) -> bool {
    // A quoted export name (`export { a as "b" }`, `import { "b" as c }`) is a name.
    if ck.c.parent(lit).is_some_and(|p| {
        matches!(
            ck.c.node(p),
            Node::ImportSpecifier { .. } | Node::ExportSpecifier { .. }
        )
    }) {
        return true;
    }
    let Some(t) = literal_type(ck, lit) else {
        return false;
    };
    match ck.type_kind(t) {
        TypeKind::StringLiteral(_) => true,
        TypeKind::Union { list, .. } => {
            let items = ck.store().list_items(list).to_vec();
            items
                .into_iter()
                .all(|m| matches!(ck.type_kind(m), TypeKind::StringLiteral(_)))
        }
        _ => false,
    }
}
