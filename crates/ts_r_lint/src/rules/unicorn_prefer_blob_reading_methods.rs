//! `unicorn/prefer-blob-reading-methods`: `reader.readAsText(blob)` and
//! `reader.readAsArrayBuffer(blob)` (exactly one non-spread argument, no optional chaining)
//! are reported on the method name; `Blob#text()` / `Blob#arrayBuffer()` read a blob directly.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-blob-reading-methods",
        "Prefer `Blob#arrayBuffer()` over `FileReader#readAsArrayBuffer(…)` and `Blob#text()` over `FileReader#readAsText(…)`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferBlobReadingMethods>,
};

#[derive(Debug, Default)]
struct PreferBlobReadingMethods;

impl Rule for PreferBlobReadingMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        let replacement = match call.name {
            "readAsText" => "text",
            "readAsArrayBuffer" => "arrayBuffer",
            _ => return,
        };
        if !call.plain() || call.args.len() != 1 || !no_spread_in(ast, call.args, 1) {
            return;
        }
        let msg = format!(
            "Prefer `Blob#{replacement}()` over `FileReader#{}(blob)`.",
            call.name
        );
        cx.report_node(call.prop, msg).id("error");
    }
}
