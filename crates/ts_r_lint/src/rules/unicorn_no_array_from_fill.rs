//! `unicorn/no-array-from-fill`: `Array.from({length: n}).fill(x)` (at most one fill argument,
//! no optional chaining or spread, `Array` the global, the object holding just a plain
//! `length` property, shorthand or quoted) should pass a map function to `Array.from`. The
//! `fill` name is reported.

use super::prelude::*;
use super::unicorn_ast::{MethodCall, method_call, no_spread_in};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-from-fill",
        "Disallow `.fill()` after `Array.from({length: …})`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoArrayFromFill>,
};

#[derive(Debug, Default)]
struct NoArrayFromFill;

fn plain_call<'a>(ast: &'a Ast, id: NodeId) -> Option<MethodCall<'a>> {
    method_call(ast, id).filter(|m| m.plain() && no_spread_in(ast, m.args, m.args.len()))
}

fn is_length_object(ast: &Ast, id: NodeId) -> bool {
    let Node::ObjectLit { props } = *ast.node(id) else {
        return false;
    };
    let &[prop] = ast.list(props) else {
        return false;
    };
    let key = match *ast.node(prop) {
        Node::Property { key, .. } => key,
        Node::ShorthandProp {
            name, init: None, ..
        } => name,
        _ => return false,
    };
    match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => ast.atom(name) == "length",
        _ => false,
    }
}

impl Rule for NoArrayFromFill {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(fill) = plain_call(ast, node) else {
            return;
        };
        if fill.name != "fill" || fill.args.len() > 1 {
            return;
        }
        let Some(from) = plain_call(ast, skip_parens(ast, fill.obj)) else {
            return;
        };
        if from.name != "from"
            || !cx.is_global_ref(from.obj, "Array")
            || from.args.len() != 1
            || !is_length_object(ast, skip_parens(ast, from.args[0]))
        {
            return;
        }
        cx.report_node(
            fill.prop,
            "Use the `Array.from(…, mapFunction)` argument instead of chaining `.fill()`.",
        )
        .id("no-array-from-fill");
    }
}
