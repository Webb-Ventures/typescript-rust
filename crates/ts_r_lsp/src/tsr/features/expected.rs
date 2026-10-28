//! Completions that come from the type a position expects rather than from names in scope:
//! the members a type literal in a type argument may still declare (`Bar<{ | }>`), and the
//! literal values (`"a"`, `2`) an expression or type position expects.
//!
//! Rules learned from tsc 7 (`tsc: tsc/internal/ls/completions.go`: the object type literal
//! in a type argument, the contextual type of the token before the cursor and the literal
//! entries made from it), written here on our AST:
//!
//! - In a type literal that sits in a type argument (directly, as a property's type below
//!   one, or in a union or intersection there), a member name offers the properties of what
//!   the literal is expected to be (see `type_args`), minus those the literal (and the
//!   type literals intersected with it) already declare. Names that are not identifiers are
//!   quoted.
//! - The literal entries are the string, number and bigint literal members (not enum
//!   members) of the expected type: after `:` in such a type literal, the property's
//!   expected type; after `[` or `,` in a tuple there, the expected element type; in an
//!   expression, its contextual type (a call argument's parameter type in every signature,
//!   a declared variable's type after `=`, the left side after `===`, the switched value
//!   after `case`). A type parameter stands for its constraint. Strings are quoted in the
//!   file's quote style (its first import's), double quotes when it has no import.

use super::position::token_before;
use std::collections::HashSet;
use ts_r_ast::{BinaryOp, Node, NodeId};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{SymId, TypeFlags, TypeId, TypeKind};

/// Levels walked up from the cursor at most.
const MAX_UP: usize = 64;

/// Member-name completions in a type literal that a type argument expects to be some type:
/// `None` when the literal is not in such a position.
pub fn type_literal_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    lit: NodeId,
) -> Option<Vec<(String, SymId)>> {
    let c = ck.c;
    let ast = c.ast(file);
    if !matches!(ast.node(lit), Node::TypeLiteral { .. }) {
        return None;
    }
    let l = Loc::new(file, lit);
    let container = match c.parent(l) {
        Some(p) if matches!(ast.node(p.node), Node::IntersectionType { .. }) => p,
        _ => l,
    };
    let t = super::type_args::expected_type(ck, container)?;
    let literals: Vec<NodeId> = match *ast.node(container.node) {
        Node::IntersectionType { types } => ast
            .list(types)
            .iter()
            .copied()
            .filter(|&n| matches!(ast.node(n), Node::TypeLiteral { .. }))
            .collect(),
        _ => vec![lit],
    };
    let mut written: HashSet<String> = HashSet::new();
    for n in literals {
        let Node::TypeLiteral { members } = *ast.node(n) else {
            continue;
        };
        for &m in ast.list(members) {
            if let Some(k) = ts_r_binder::decl_name_node(ast, m)
                && let Some(name) = ts_r_checker::nodes::key_text(ast, k)
            {
                written.insert(name);
            }
        }
    }
    let mut out = Vec::new();
    for (name, s) in super::complete::object_props(ck, t) {
        if name.starts_with("__@") || name.starts_with('#') || written.contains(&name) {
            continue;
        }
        let label = if super::complete::is_identifier_name(&name) {
            name
        } else {
            serde_json::to_string(&name).unwrap_or(name)
        };
        out.push((label, s));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// The literal values the position of the word starting at `word` expects, as labels.
pub fn literal_labels(ck: &mut Ck<'_, '_>, file: FileId, anchor: NodeId, word: u32) -> Vec<String> {
    let Some(t) = expected_at(ck, file, anchor, word) else {
        return Vec::new();
    };
    let quote = super::complete::ImportStyle::of(ck.c, file).quote_char();
    let mut out = Vec::new();
    let members = ck.union_members(t);
    for m in members {
        let m = match ck.type_kind(m) {
            TypeKind::TypeParameter(_) => ck.base_constraint(m),
            _ => m,
        };
        for x in ck.union_members(m) {
            if ck.store().flags(x).intersects(TypeFlags::ENUM_LITERAL) {
                continue;
            }
            let label = match ck.type_kind(x) {
                TypeKind::StringLiteral(s) => quoted(ck.store().str(s), quote),
                TypeKind::NumberLiteral(..) | TypeKind::BigIntLiteral(_) => ck.type_to_string(x),
                _ => continue,
            };
            if !out.contains(&label) {
                out.push(label);
            }
        }
    }
    out
}

/// `s` as a string literal in quote `q`.
fn quoted(s: &str, q: char) -> String {
    let double = serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""));
    if q == '"' {
        return double;
    }
    let inner = &double[1..double.len() - 1];
    let inner = inner.replace("\\\"", "\"").replace('\'', "\\'");
    format!("'{inner}'")
}

/// The type expected at the word starting at `word` (see the module docs).
fn expected_at(ck: &mut Ck<'_, '_>, file: FileId, anchor: NodeId, word: u32) -> Option<TypeId> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (tok_at, tok) = token_before(ast, text, word)?;
    let parents = c.file(file).parents();
    // Type positions in a type argument: `{ x: | }` and `[ | ]`.
    let mut cur = Some(anchor);
    for _ in 0..MAX_UP {
        let Some(n) = cur else { break };
        match *ast.node(n) {
            Node::PropertySignature { key, .. } if tok == ":" && ast.span(key).end <= tok_at => {
                return super::type_args::expected_type(ck, Loc::new(file, n));
            }
            Node::TupleType { .. } if matches!(tok, "[" | ",") => {
                let t = super::type_args::expected_type(ck, Loc::new(file, n))?;
                return ck.element_type_of_array_like(t);
            }
            // `{ x: | }`: the property whose `:` is before the cursor.
            Node::TypeLiteral { members } if tok == ":" => {
                let m = ast.list(members).iter().copied().find(|&m| {
                    matches!(*ast.node(m), Node::PropertySignature { key, .. }
                        if ast.span(key).end <= tok_at && tok_at < ast.span(m).end)
                });
                match m {
                    Some(m) => return super::type_args::expected_type(ck, Loc::new(file, m)),
                    None => break,
                }
            }
            Node::TypeLiteral { .. } | Node::TypeArgs { .. } => break,
            _ => {}
        }
        cur = parents.parent(n);
    }
    // Expressions: the word being typed (or the parser's placeholder) has a contextual type.
    let word_node = [Some(anchor), super::position::node_covering(ast, word)]
        .into_iter()
        .flatten()
        .find(|&n| {
            matches!(ast.node(n), Node::Ident { .. } | Node::Missing {})
                && ast.span(n).start <= word
                && word <= ast.span(n).end
        });
    if let Some(n) = word_node
        && super::locate::is_expression_name(ck, Loc::new(file, n))
        && let Some(t) = ck.contextual_type(Loc::new(file, n))
    {
        return Some(t);
    }
    // No node for the expression yet: read the token before it.
    let holder = super::position::node_covering(ast, tok_at).unwrap_or(anchor);
    let mut cur = Some(holder);
    for _ in 0..MAX_UP {
        let Some(n) = cur else { break };
        match *ast.node(n) {
            Node::Call { callee, args, .. } | Node::New { callee, args, .. }
                if matches!(tok, "(" | ",") && ast.span(callee).end <= tok_at =>
            {
                let kind = if matches!(ast.node(n), Node::New { .. }) {
                    SigKind::Construct
                } else {
                    SigKind::Call
                };
                let index = ast
                    .list(args)
                    .iter()
                    .filter(|&&a| ast.span(a).end <= tok_at)
                    .count()
                    .min(if tok == "(" { 0 } else { usize::MAX });
                return argument_type(ck, Loc::new(file, callee), kind, index);
            }
            // `{ a: | }`: the property's contextual type.
            Node::Property { key, .. } if tok == ":" && ast.span(key).end <= tok_at => {
                let obj = parents.parent(n)?;
                let ct = ck.contextual_type(Loc::new(file, obj))?;
                let name = ts_r_checker::nodes::key_text(ast, key)?;
                return ck.contextual_property_type(ct, &name);
            }
            Node::VarDeclarator {
                type_ann: Some(ta), ..
            } if tok == "=" => {
                return Some(ck.type_from_node(Loc::new(file, ta)));
            }
            Node::Binary {
                left,
                op: BinaryOp::StrictEq | BinaryOp::Eq | BinaryOp::StrictNotEq | BinaryOp::NotEq,
                ..
            } if matches!(tok, "=") => {
                return Some(ck.quietly(|ck| ck.check_expr(Loc::new(file, left))));
            }
            Node::SwitchCase { .. } if tok == "case" => {
                let sw = parents.parent(n)?;
                let Node::Switch { disc, .. } = *ast.node(sw) else {
                    return None;
                };
                return Some(ck.quietly(|ck| ck.check_expr(Loc::new(file, disc))));
            }
            Node::Block { .. } | Node::Program { .. } => break,
            _ => {}
        }
        cur = parents.parent(n);
    }
    None
}

/// The type of the parameter at `index` across the callee's signatures (a rest parameter's
/// element type past its start).
fn argument_type(ck: &mut Ck<'_, '_>, callee: Loc, kind: SigKind, index: usize) -> Option<TypeId> {
    let t = ck.quietly(|ck| ck.check_expr(callee));
    let a = ck.apparent_type(t);
    let sigs = ck.signatures_of(a, kind);
    let mut found = Vec::new();
    for s in sigs {
        let d = ck.store().sig(s);
        let rest = d.has_rest();
        let params: Vec<TypeId> = d.params.iter().map(|p| p.ty).collect();
        let ty = if index < params.len() && !(rest && index + 1 >= params.len()) {
            params[index]
        } else if rest && !params.is_empty() {
            match ck.element_type_of_array_like(params[params.len() - 1]) {
                Some(e) => e,
                None => continue,
            }
        } else {
            continue;
        };
        if !found.contains(&ty) {
            found.push(ty);
        }
    }
    match found.len() {
        0 => None,
        1 => Some(found[0]),
        _ => Some(ck.union(&found)),
    }
}
