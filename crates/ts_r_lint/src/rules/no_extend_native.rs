//! `no-extend-native`: no new properties on built-in prototypes.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-extend-native",
        "Disallow extending native types",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoExtendNative>,
};

#[derive(Debug, Default)]
struct NoExtendNative {
    exceptions: Vec<String>,
}

impl FromOptions for NoExtendNative {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(Self {
            exceptions: str_list(obj_opt(opts, 0), "exceptions"),
        })
    }
}

impl Rule for NoExtendNative {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(name) = ident_name(ast, node) else {
            return;
        };
        if !name.starts_with(|c: char| c.is_ascii_uppercase())
            || !crate::globals::is_builtin(name)
            || self.exceptions.iter().any(|e| e == name)
            || !cx.is_global_ref(node, name)
            || !cx.global_available(node, name)
        {
            return;
        }
        let parents = cx.file.parents;
        // `Name.prototype` (or `Name["prototype"]`), with `Name` as the object.
        let Some(proto) = parents.parent(node) else {
            return;
        };
        let Some((obj, prop)) = member_parts(ast, cx.src(), proto) else {
            return;
        };
        if obj != node || prop != "prototype" {
            return;
        }
        let proto = outer_parens(ast, parents, proto);
        let Some(p) = parents.parent(proto) else {
            return;
        };
        let target = match *ast.node(p) {
            // `Name.prototype.x = ...`
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == proto => {
                let m = outer_parens(ast, parents, p);
                match parents.parent(m).map(|a| (a, ast.node(a))) {
                    Some((a, &Node::Assign { left, .. })) if left == m => a,
                    _ => return,
                }
            }
            // `Object.defineProperty(Name.prototype, ...)`
            Node::Call { callee, args, .. } if ast.list(args).first() == Some(&proto) => {
                let Some((o, method)) = member_parts(ast, cx.src(), callee) else {
                    return;
                };
                let o = skip_parens(ast, o);
                if !matches!(&*method, "defineProperty" | "defineProperties")
                    || ident_name(ast, o) != Some("Object")
                {
                    return;
                }
                p
            }
            _ => return,
        };
        let msg = format!("{name} prototype is read only, properties should not be added.");
        let span = cx.span(target);
        cx.report(span, msg).id("unexpected");
    }
}

/// The outermost `Paren` wrapping `id` (or `id` itself).
fn outer_parens(ast: &ts_r_ast::Ast, parents: &ts_r_ast::ParentMap, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match parents.parent(id) {
            Some(p) if matches!(ast.node(p), Node::Paren { .. }) => id = p,
            _ => break,
        }
    }
    id
}
