//! `no-import-assign`: disallow assigning to imported bindings.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-import-assign",
        "Disallow assigning to imported bindings",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoImportAssign>,
};

#[derive(Debug, Default)]
struct NoImportAssign;

const OBJECT_MUTATORS: &[&str] = &[
    "assign",
    "defineProperty",
    "defineProperties",
    "freeze",
    "setPrototypeOf",
];
const REFLECT_MUTATORS: &[&str] = &["defineProperty", "deleteProperty", "set", "setPrototypeOf"];

/// `Object.assign(id, ...)` and friends: returns the call.
fn mutator_call(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (cur, p) = {
        let mut cur = id;
        loop {
            let p = cx.parent(cur)?;
            if matches!(ast.node(p), Node::Paren { .. }) {
                cur = p;
            } else {
                break (cur, p);
            }
        }
    };
    let Node::Call { callee, args, .. } = *ast.node(p) else {
        return None;
    };
    if ast.list(args).first() != Some(&cur) {
        return None;
    }
    let callee = skip_parens(ast, callee);
    if !matches!(ast.node(callee), Node::Member { .. }) {
        return None;
    }
    let (obj, name) = member_parts(ast, cx.src(), callee)?;
    let obj = skip_parens(ast, obj);
    let list = if cx.is_global_ref(obj, "Object") {
        OBJECT_MUTATORS
    } else if cx.is_global_ref(obj, "Reflect") {
        REFLECT_MUTATORS
    } else {
        return None;
    };
    list.contains(&name.as_ref()).then_some(p)
}

impl Rule for NoImportAssign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let parents = cx.file.parents;
        let mut hits: Vec<(NodeId, String, bool)> = Vec::new();
        for sym in sem.symbols() {
            let Some(kind) = sym.decls.first().map(|d| d.kind) else {
                continue;
            };
            if !matches!(
                kind,
                DeclKind::ImportDefault | DeclKind::ImportNamed | DeclKind::ImportNamespace
            ) {
                continue;
            }
            let name = ast.atom(sym.name);
            for &r in &sym.refs {
                let r = sem.reference(r);
                if r.is_type() {
                    continue;
                }
                if r.is_write() {
                    let at = write_root(ast, parents, r.node).unwrap_or(r.node);
                    hits.push((at, name.to_owned(), false));
                    continue;
                }
                if kind != DeclKind::ImportNamespace {
                    continue;
                }
                // `mod.x = 1`, `delete mod.x`, `Object.assign(mod, ...)`.
                if let Some(call) = mutator_call(cx, r.node) {
                    hits.push((call, name.to_owned(), true));
                    continue;
                }
                let Some(p) = parent_skip_parens(ast, parents, r.node) else {
                    continue;
                };
                let is_obj = match *ast.node(p) {
                    Node::Member { obj, .. } | Node::Index { obj, .. } => {
                        skip_parens(ast, obj) == r.node
                    }
                    _ => false,
                };
                if !is_obj {
                    continue;
                }
                if let Some(at) = write_root(ast, parents, p) {
                    hits.push((at, name.to_owned(), true));
                }
            }
        }
        hits.sort_by_key(|(n, _, _)| cx.span(*n).start);
        for (node, name, member) in hits {
            if member {
                cx.report_node(node, format!("The members of '{name}' are read-only."))
                    .id("readonlyMember");
            } else {
                cx.report_node(node, format!("'{name}' is read-only."))
                    .id("readonly");
            }
        }
    }
}
