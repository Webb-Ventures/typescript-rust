//! Rules decided mostly by syntax: written `any` and calls of `Function` (S-01 TSR0101,
//! TSR0103), non-null assertions (S-03), definite assignment assertions (S-08 TSR0801),
//! ambient declarations and module augmentation (S-11), suppression directives and global
//! augmentation (S-12). Overload implementations (S-12) are checked in `overloads.rs`.

use ts_r_ast::{KeywordTypeKind, Modifiers, ModuleKind, Node, NodeId};
use ts_r_checker::checker::GlobalType;
use ts_r_span::Span;

use crate::codes;
use crate::ctx::Ctx;
use crate::pragma::suppression_directives;

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    let nodes = std::mem::take(&mut cx.nodes);
    for &n in &nodes {
        match cx.node(n) {
            Node::KeywordType {
                kind: KeywordTypeKind::Any,
            } => cx.report(codes::NO_ANY, cx.span(n), &[]),
            Node::Call { callee, .. } | Node::New { callee, .. } if cx.has_rule(1) => {
                function_call(cx, n, callee);
            }
            Node::NonNull { expr } => {
                let operand = cx.short(expr);
                cx.report(codes::NON_NULL, cx.span(n), &[&operand]);
            }
            Node::PropertyDef {
                key,
                definite: true,
                ..
            } => definite(cx, key),
            Node::VarDeclarator {
                name,
                definite: true,
                ..
            } => definite(cx, name),
            _ => {}
        }
    }
    cx.nodes = nodes;
    if cx.has_rule(11) || cx.has_rule(12) {
        let root = cx.ast.root;
        if let Node::Program { stmts } = cx.node(root) {
            let list = cx.ast.list(stmts).to_vec();
            ambient_statements(cx, &list, true);
        }
    }
    if cx.has_rule(12) {
        for (span, d) in suppression_directives(cx.ast, cx.text) {
            if d == "ts-nocheck" {
                cx.report(codes::NOCHECK_DIRECTIVE, span, &[]);
            } else {
                cx.report(codes::SUPPRESSION_DIRECTIVE, span, &[d]);
            }
        }
    }
}

/// TSR0103: calling or constructing a value of the global `Function` type.
fn function_call(cx: &mut Ctx<'_, '_, '_>, call: NodeId, callee: NodeId) {
    let Some(ft) = cx.ck.global_type(GlobalType::Function) else {
        return;
    };
    let t = cx.ty(callee);
    if t == ft {
        let name = cx.short(callee);
        cx.report(codes::FUNCTION_CALL, cx.span(call), &[&name]);
    }
}

/// TSR0801 at the `!` after `name`.
fn definite(cx: &mut Ctx<'_, '_, '_>, name: NodeId) {
    let end = cx.span(name).end as usize;
    let bang = cx.text[end..]
        .find('!')
        .map(|i| (end + i) as u32)
        .unwrap_or(end as u32);
    let text = cx.src(name).to_owned();
    cx.report(
        codes::DEFINITE_ASSIGNMENT,
        Span::new(bang, bang + 1),
        &[&text],
    );
}

fn has_declare(m: Modifiers) -> bool {
    m.contains(Modifiers::DECLARE)
}

/// S-11 and S-12 over a statement list (the file, or a non-ambient namespace body).
fn ambient_statements(cx: &mut Ctx<'_, '_, '_>, stmts: &[NodeId], top: bool) {
    let script = top && !cx.ck.c.bound(cx.file).is_module;
    for &s in stmts {
        match cx.node(s) {
            Node::Module {
                modifiers,
                kind,
                name,
                body,
            } => match kind {
                ModuleKind::Global => cx.report(codes::GLOBAL_AUGMENTATION, cx.span(name), &[]),
                _ if matches!(cx.node(name), Node::StringLit { .. }) => {
                    let text = cx.src(name).trim_matches(['"', '\'']).to_owned();
                    if body.is_none() {
                        cx.report(codes::SHORTHAND_MODULE, cx.span(name), &[&text]);
                    } else {
                        cx.report(codes::MODULE_AUGMENTATION, cx.span(name), &[&text]);
                    }
                }
                _ if has_declare(modifiers) => {
                    let text = cx.src(name).to_owned();
                    cx.report(codes::AMBIENT_DECLARATION, cx.span(name), &[&text]);
                }
                _ => {
                    if script && merges_with_other_file(cx, s) {
                        cx.report(codes::GLOBAL_AUGMENTATION, cx.span(name), &[]);
                    }
                    if let Some(b) = body
                        && let Node::ModuleBlock { stmts } = cx.node(b)
                    {
                        let inner = cx.ast.list(stmts).to_vec();
                        ambient_statements(cx, &inner, false);
                    }
                }
            },
            Node::VarDecl {
                modifiers, decls, ..
            } if has_declare(modifiers) => {
                if let Some(&d) = cx.ast.list(decls).first()
                    && let Node::VarDeclarator { name, .. } = cx.node(d)
                {
                    let text = cx.src(name).to_owned();
                    cx.report(codes::AMBIENT_DECLARATION, cx.span(name), &[&text]);
                }
            }
            Node::FunctionDecl {
                modifiers,
                name: Some(name),
                ..
            }
            | Node::ClassDecl {
                modifiers,
                name: Some(name),
                ..
            } if has_declare(modifiers) => {
                let text = cx.src(name).to_owned();
                cx.report(codes::AMBIENT_DECLARATION, cx.span(name), &[&text]);
            }
            Node::Enum {
                modifiers, name, ..
            } if has_declare(modifiers) => {
                let text = cx.src(name).to_owned();
                cx.report(codes::AMBIENT_DECLARATION, cx.span(name), &[&text]);
            }
            Node::Interface { name, .. }
            | Node::ClassDecl {
                name: Some(name), ..
            }
            | Node::Enum { name, .. }
            | Node::FunctionDecl {
                name: Some(name), ..
            } if script && merges_with_other_file(cx, s) => {
                cx.report(codes::GLOBAL_AUGMENTATION, cx.span(name), &[]);
            }
            _ => {}
        }
    }
}

/// A top-level declaration of a script file whose symbol also has declarations in another
/// file (it merges into a global declared elsewhere, e.g. `interface String` with lib).
fn merges_with_other_file(cx: &mut Ctx<'_, '_, '_>, decl: NodeId) -> bool {
    let l = cx.loc(decl);
    let Some(sym) = cx.ck.c.sym_of_decl(l) else {
        return false;
    };
    cx.ck.c.decls(sym).iter().any(|d| d.file != cx.file)
}
