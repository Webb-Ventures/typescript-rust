//! `getter-return`: enforce `return` statements in getters.

use super::prelude::*;
use crate::cfg::Cfg;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "getter-return",
        "Enforce `return` statements in getters",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<GetterReturn>,
};

#[derive(Debug, Default)]
struct GetterReturn {
    allow_implicit: bool,
}

impl FromOptions for GetterReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(GetterReturn {
            allow_implicit: bool_field(obj_opt(opts, 0), "allowImplicit", false),
        })
    }
}

impl GetterReturn {
    fn check(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let ast = cx.ast();
        if !matches!(
            ast.node(f),
            Node::FunctionExpr { body: Some(_), .. } | Node::Arrow { .. }
        ) {
            return;
        }
        if let Node::Arrow { body, .. } = *ast.node(f)
            && !matches!(ast.node(body), Node::Block { .. })
        {
            return;
        }
        let cfg = Cfg::function(ast, f);
        let name = function_name_with_kind(ast, cx.src(), cx.file.parents, f);
        let mut any_return = false;
        let mut bare = Vec::new();
        for (i, b) in cfg.blocks.iter().enumerate() {
            for &e in &b.events {
                if let Node::Return { arg } = *ast.node(e) {
                    any_return = true;
                    if arg.is_none() && cfg.is_reachable(i as u32) && !bare.contains(&e) {
                        bare.push(e);
                    }
                }
            }
        }
        if cfg.is_reachable(cfg.exit) {
            let span = function_head_span(ast, cx.src(), cx.file.parents, f);
            if any_return {
                cx.report(span, format!("Expected {name} to always return a value."))
                    .id("expectedAlways");
            } else {
                cx.report(span, format!("Expected to return a value in {name}."))
                    .id("expected");
            }
        }
        if !self.allow_implicit {
            for r in bare {
                cx.report_node(r, format!("Expected to return a value in {name}."))
                    .id("expected");
            }
        }
    }

    /// Getter functions in property descriptors passed to `Object.defineProperty` & co.
    fn descriptor_getters(cx: &Ctx<'_, '_>, call: NodeId) -> Vec<NodeId> {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(call) else {
            return Vec::new();
        };
        let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
            return Vec::new();
        };
        let Some(method) = ident_name(ast, prop) else {
            return Vec::new();
        };
        let args = ast.list(args);
        let descriptors: Vec<NodeId> = if (cx.is_global_ref(obj, "Object")
            || cx.is_global_ref(obj, "Reflect"))
            && method == "defineProperty"
        {
            args.get(2).copied().into_iter().collect()
        } else if cx.is_global_ref(obj, "Object") && matches!(method, "defineProperties" | "create")
        {
            match args.get(1).map(|&a| ast.node(skip_parens(ast, a))) {
                Some(Node::ObjectLit { props }) => ast
                    .list(*props)
                    .iter()
                    .filter_map(|&p| match *ast.node(p) {
                        Node::Property { value, .. } => Some(value),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let mut out = Vec::new();
        for d in descriptors {
            let Node::ObjectLit { props } = *ast.node(skip_parens(ast, d)) else {
                continue;
            };
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::Property { key, value, .. }
                        if static_key_name(ast, cx.src(), key).as_deref() == Some("get") =>
                    {
                        out.push(skip_parens(ast, value));
                    }
                    Node::MethodDef {
                        key,
                        value,
                        kind: MethodKind::Method,
                        ..
                    } if static_key_name(ast, cx.src(), key).as_deref() == Some("get") => {
                        out.push(value);
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

impl Rule for GetterReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::MethodDef {
                kind: MethodKind::Get,
                value,
                ..
            } => self.check(cx, value),
            Node::Call { .. } => {
                for f in Self::descriptor_getters(cx, node) {
                    self.check(cx, f);
                }
            }
            _ => {}
        }
    }
}
