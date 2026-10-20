//! `no-dupe-class-members`: disallow duplicate class members.

use rustc_hash::FxHashMap;

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-dupe-class-members",
        "Disallow duplicate class members",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDupeClassMembers>,
};

#[derive(Debug, Default)]
pub(crate) struct NoDupeClassMembers;

#[derive(Default, Clone, Copy)]
struct Seen {
    init: bool,
    get: bool,
    set: bool,
}

/// Shared with `@typescript-eslint/no-dupe-class-members`, which skips overload signatures.
pub(crate) fn check_class(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let src = cx.src();
    let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *cx.node(node) else {
        return;
    };
    let mut seen: FxHashMap<(String, bool), Seen> = FxHashMap::default();
    for &m in ast.list(members) {
        let (key, kind, is_static) = match *ast.node(m) {
            Node::MethodDef {
                key,
                kind,
                modifiers,
                value,
                ..
            } => {
                if kind == MethodKind::Constructor && !modifiers.contains(Modifiers::STATIC) {
                    continue;
                }
                // TS overload signatures have no body.
                if let Node::FunctionExpr { body: None, .. } = *ast.node(value) {
                    continue;
                }
                (
                    key,
                    match kind {
                        MethodKind::Get => 1,
                        MethodKind::Set => 2,
                        _ => 0,
                    },
                    modifiers.contains(Modifiers::STATIC),
                )
            }
            Node::PropertyDef { key, modifiers, .. } => {
                if modifiers.contains(Modifiers::DECLARE) {
                    continue;
                }
                (key, 0, modifiers.contains(Modifiers::STATIC))
            }
            _ => continue,
        };
        let name = match *ast.node(key) {
            Node::PrivateName { name } => format!("#{}", ast.atom(name)),
            Node::ComputedKey { expr } => {
                let e = skip_parens(ast, expr);
                if !matches!(
                    ast.node(e),
                    Node::StringLit { .. }
                        | Node::NumberLit { .. }
                        | Node::TemplateLit { .. }
                        | Node::BigIntLit { .. }
                        | Node::NullLit {}
                        | Node::BoolLit { .. }
                        | Node::RegexLit { .. }
                ) {
                    continue;
                }
                match static_value_string(ast, src, e) {
                    Some(n) => n.into_owned(),
                    None => continue,
                }
            }
            _ => match key_name(ast, src, key) {
                Some(n) => n.into_owned(),
                None => continue,
            },
        };
        let name = if !matches!(ast.node(key), Node::PrivateName { .. }) && name.starts_with('#') {
            format!("\u{0}{name}")
        } else {
            name
        };
        let e = seen.entry((name.clone(), is_static)).or_default();
        let dup = match kind {
            0 => e.init || e.get || e.set,
            1 => e.init || e.get,
            _ => e.init || e.set,
        };
        match kind {
            0 => e.init = true,
            1 => e.get = true,
            _ => e.set = true,
        }
        if dup {
            let at = match *ast.node(key) {
                Node::ComputedKey { expr } => skip_parens(ast, expr),
                _ => key,
            };
            let display = match *ast.node(key) {
                Node::PrivateName { name } => format!("#{}", ast.atom(name)),
                _ => name.trim_start_matches('\u{0}').to_owned(),
            };
            cx.report_node(at, format!("Duplicate name '{display}'."))
                .id("unexpected");
        }
    }
}

impl Rule for NoDupeClassMembers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        check_class(cx, node);
    }
}
