//! `no-dupe-keys`: disallow duplicate keys in object literals.

use rustc_hash::FxHashMap;

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-dupe-keys",
        "Disallow duplicate keys in object literals",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDupeKeys>,
};

#[derive(Debug, Default)]
struct NoDupeKeys;

#[derive(Default, Clone, Copy)]
struct Seen {
    init: bool,
    get: bool,
    set: bool,
}

impl Rule for NoDupeKeys {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ObjectLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ObjectLit { props } = *cx.node(node) else {
            return;
        };
        let mut seen: FxHashMap<String, Seen> = FxHashMap::default();
        for &p in ast.list(props) {
            let (key, kind) = match *ast.node(p) {
                Node::Property { key, .. } => {
                    // `__proto__: x` sets the prototype; duplicates are a syntax error anyway.
                    if !matches!(ast.node(key), Node::ComputedKey { .. })
                        && key_name(ast, src, key).as_deref() == Some("__proto__")
                    {
                        continue;
                    }
                    (key, 0)
                }
                Node::ShorthandProp { name, .. } => (name, 0),
                Node::MethodDef { key, kind, .. } => (
                    key,
                    match kind {
                        MethodKind::Get => 1,
                        MethodKind::Set => 2,
                        _ => 0,
                    },
                ),
                _ => continue,
            };
            let Some(name) = key_name(ast, src, key) else {
                continue;
            };
            let name = name.into_owned();
            let e = seen.entry(name.clone()).or_default();
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
                cx.report_node(at, format!("Duplicate key '{name}'."))
                    .id("unexpected");
            }
        }
    }
}
