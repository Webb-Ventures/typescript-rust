//! `no-setter-return`: disallow returning values from setters.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-setter-return",
        "Disallow returning values from setters",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoSetterReturn>,
};

#[derive(Debug, Default)]
struct NoSetterReturn;

/// True when function `f` is a setter: a `set` accessor, or the `set` property of a property
/// descriptor passed to `Object.defineProperty` / `Reflect.defineProperty` /
/// `Object.defineProperties` / `Object.create`.
fn is_setter(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let Some(p) = cx.parent(f) else { return false };
    match *ast.node(p) {
        Node::MethodDef {
            kind: MethodKind::Set,
            ..
        } => true,
        Node::MethodDef {
            kind: MethodKind::Method,
            key,
            ..
        }
        | Node::Property { key, .. } => {
            if matches!(ast.node(p), Node::Property { value, .. } if *value != f) {
                return false;
            }
            if key_name(ast, src, key).as_deref() != Some("set") {
                return false;
            }
            // The object literal must be a descriptor.
            let Some(desc) = cx.parent(p) else {
                return false;
            };
            if !matches!(ast.node(desc), Node::ObjectLit { .. }) {
                return false;
            }
            is_descriptor(cx, desc)
        }
        _ => false,
    }
}

pub(crate) fn is_descriptor(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let Some(parent) = parent_skip_parens(ast, cx.file.parents, obj) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Call { callee, args, .. } => {
            let a = ast.list(args);
            let Some((o, m)) = member_parts(ast, src, callee) else {
                return false;
            };
            let object = cx.is_global_ref(o, "Object");
            let reflect = cx.is_global_ref(o, "Reflect");
            a.get(2) == Some(&obj) && m == "defineProperty" && (object || reflect)
        }
        Node::Property { value, .. } if value == obj => {
            // `Object.defineProperties(o, { p: descriptor })` / `Object.create(proto, { ... })`
            let Some(outer) = cx.parent(parent) else {
                return false;
            };
            let Some(call) = parent_skip_parens(ast, cx.file.parents, outer) else {
                return false;
            };
            let Node::Call { callee, args, .. } = *ast.node(call) else {
                return false;
            };
            let a = ast.list(args);
            let Some((o, m)) = member_parts(ast, src, callee) else {
                return false;
            };
            a.get(1).map(|&x| skip_parens(ast, x)) == Some(outer)
                && cx.is_global_ref(o, "Object")
                && (m == "defineProperties" || m == "create")
        }
        _ => false,
    }
}

impl Rule for NoSetterReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Return, NodeKind::Arrow]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Node::Arrow { flags, body, .. } = *cx.node(node) {
            if flags.contains(FnFlags::EXPRESSION_BODY) && is_setter(cx, node) {
                cx.report_node(skip_parens(ast, body), "Setter cannot return a value.")
                    .id("returnsValue");
            }
            return;
        }
        let Node::Return { arg: Some(_) } = *cx.node(node) else {
            return;
        };
        // The enclosing function.
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            match ast.kind(p) {
                NodeKind::FunctionDecl => return,
                NodeKind::FunctionExpr | NodeKind::Arrow => {
                    if is_setter(cx, p) {
                        cx.report_node(node, "Setter cannot return a value.")
                            .id("returnsValue");
                    }
                    return;
                }
                NodeKind::ClassDecl | NodeKind::ClassExpr | NodeKind::StaticBlock => return,
                _ => {}
            }
            cur = p;
        }
    }
}
