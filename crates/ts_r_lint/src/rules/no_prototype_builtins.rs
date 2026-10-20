//! `no-prototype-builtins`: disallow calling some `Object.prototype` methods directly on
//! objects.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-prototype-builtins",
        "Disallow calling some `Object.prototype` methods directly on objects",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoPrototypeBuiltins>,
};

#[derive(Debug, Default)]
struct NoPrototypeBuiltins;

const METHODS: &[&str] = &["hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable"];

impl Rule for NoPrototypeBuiltins {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Call {
            callee,
            args,
            optional: call_optional,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Some(name) = member_name(ast, src, callee) else {
            return;
        };
        let Some(method) = METHODS.iter().find(|m| **m == name) else {
            return;
        };
        let (obj, prop_span, member_optional) = match *ast.node(callee) {
            Node::Member {
                obj,
                prop,
                optional,
            } => (obj, ast.span(prop), optional),
            Node::Index {
                obj,
                index,
                optional,
            } => (obj, ast.span(skip_parens(ast, index)), optional),
            _ => return,
        };
        let msg = format!("Do not access Object.prototype method '{method}' from target object.");
        let can_fix = !call_optional
            && !member_optional
            && !chain_has_optional(ast, obj)
            && cx.global_available(node, "Object");
        if !can_fix {
            cx.report(prop_span, msg).id("prototypeBuildIn");
            return;
        }
        let o = skip_parens(ast, obj);
        let mut obj_text = ast.span(o).text(src).to_owned();
        if matches!(ast.node(o), Node::Seq { .. }) {
            obj_text = format!("({obj_text})");
        }
        let a = ast.list(args);
        let call_span = ast.span(node);
        let mut text = format!("Object.prototype.{method}.call({obj_text}");
        if let (Some(&first), Some(&last)) = (a.first(), a.last()) {
            text.push_str(", ");
            text.push_str(&src[ast.span(first).start as usize..ast.span(last).end as usize]);
        }
        text.push(')');
        cx.report(prop_span, msg).id("prototypeBuildIn").suggest(
            "callObjectPrototype",
            format!("Call Object.prototype.{method} explicitly."),
            Fix::replace(call_span, text),
        );
    }
}
