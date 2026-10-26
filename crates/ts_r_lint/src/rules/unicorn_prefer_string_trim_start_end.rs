//! `unicorn/prefer-string-trim-start-end`: `x.trimLeft()` / `x.trimRight()` (no arguments,
//! not an optional call, a non-computed member) are reported on the method name and fixed
//! to `trimStart` / `trimEnd`. Receivers known not to be strings (an annotation or JSDoc
//! type says so) are left alone.

use super::prelude::*;
use super::unicorn_types::is_known_non_string;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-trim-start-end",
        "Prefer `String#trimStart()` / `String#trimEnd()` over `String#trimLeft()` / `String#trimRight()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferStringTrimStartEnd>,
};

#[derive(Debug, Default)]
struct PreferStringTrimStartEnd;

impl Rule for PreferStringTrimStartEnd {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !ast.list(args).is_empty() {
            return;
        }
        let Node::Member { obj, prop, .. } = *ast.node(callee) else {
            return;
        };
        let replacement = match ident_name(ast, prop) {
            Some("trimLeft") => "trimStart",
            Some("trimRight") => "trimEnd",
            _ => return,
        };
        if is_known_non_string(cx, obj) {
            return;
        }
        let method = cx.text(prop);
        let msg = format!("Prefer `String#{replacement}()` over `String#{method}()`.");
        let fix = Fix::replace(cx.span(prop), replacement);
        cx.report_node(prop, msg)
            .id("prefer-string-trim-start-end")
            .fix(fix);
    }
}
