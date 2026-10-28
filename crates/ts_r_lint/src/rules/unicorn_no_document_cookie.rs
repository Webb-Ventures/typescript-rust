//! `unicorn/no-document-cookie`: assigning to `document.cookie` (with `=` or a compound
//! operator) is reported on the member expression.
//!
//! `document` is the global (unshadowed, or reached as `globalThis.document`,
//! `window.document`, `self.document`, `global.document`), or a variable declared with one
//! of those as its initialiser (followed through further aliases). A computed key counts when
//! it is a literal string expression (`["coo" + "kie"]`); identifiers are not resolved, as in
//! ESLint's reference tracker.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-document-cookie",
        "Do not use `document.cookie` directly.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoDocumentCookie>,
};

#[derive(Debug, Default)]
struct NoDocumentCookie;

const GLOBAL_OBJECTS: &[&str] = &["global", "globalThis", "self", "window"];

impl Rule for NoDocumentCookie {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Assign { left, .. } = *cx.node(node) else {
            return;
        };
        let target = skip_parens(ast, left);
        let Some((obj, "cookie")) = property(ast, target) else {
            return;
        };
        if is_document(cx, obj, 0) {
            cx.report_node(target, "Do not use `document.cookie` directly.")
                .id("no-document-cookie");
        }
    }
}

/// `(object, name)` of a non-optional member access with a static name.
fn property(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(NodeId, &str)> {
    match *ast.node(id) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } => Some((obj, ident_name(ast, prop)?)),
        Node::Index {
            obj,
            optional: false,
            index,
        } => {
            let mut buf = String::new();
            literal_string(ast, index, &mut buf, 0)?;
            (buf == "cookie").then_some((obj, "cookie"))
        }
        _ => None,
    }
}

/// Appends the value of a string literal, substitution-free template, or `+` of those.
fn literal_string(ast: &ts_r_ast::Ast, id: NodeId, out: &mut String, depth: u32) -> Option<()> {
    if depth > 32 {
        return None;
    }
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => {
            literal_string(ast, left, out, depth + 1)?;
            literal_string(ast, right, out, depth + 1)
        }
        Node::StringLit { .. } | Node::TemplateLit { .. } => {
            out.push_str(&static_value_string(ast, "", skip_parens(ast, id))?);
            Some(())
        }
        _ => None,
    }
}

fn is_global(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    if ident_name(ast, id) != Some(name) {
        return false;
    }
    let sem = cx.semantic();
    sem.reference_at(id).is_some_and(|r| {
        matches!(
            sem.reference(r).resolution,
            Resolution::Global | Resolution::Unresolved
        )
    }) || cx.is_global_ref(id, name)
}

fn is_document(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 16 {
        return false;
    }
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => {
            if is_global(cx, id, "document") {
                return true;
            }
            alias_inits(cx, id)
                .into_iter()
                .any(|init| is_document(cx, init, depth + 1))
        }
        _ => match property(ast, id) {
            Some((obj, "document")) => {
                let obj = skip_parens(ast, obj);
                GLOBAL_OBJECTS.iter().any(|g| is_global(cx, obj, g))
            }
            _ => false,
        },
    }
}

/// The initialisers of the plain-name declarations the reference `id` resolves to.
fn alias_inits(cx: &Ctx<'_, '_>, id: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Some(Resolution::Symbol(s)) = sem.reference_at(id).map(|r| sem.reference(r).resolution)
    else {
        return Vec::new();
    };
    sem.symbol(s)
        .decls
        .iter()
        .filter_map(|d| match *ast.node(d.node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if name == d.name => Some(init),
            _ => None,
        })
        .collect()
}
