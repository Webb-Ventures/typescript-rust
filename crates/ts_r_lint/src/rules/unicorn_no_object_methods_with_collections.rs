//! `unicorn/no-object-methods-with-collections`: `Object.entries/keys/values(x)` (the
//! global `Object`, one argument) on a value known to be a `Map` or a `Set` (see
//! `unicorn_types::builtin_collection_type`) reads no entries. Reported on the method
//! name, with the suggestion `Array.from(x.entries())` unless comments would be lost.

use super::prelude::*;
use super::unicorn_text::{member_object_text, paren_span, would_remove_comments};
use super::unicorn_types::builtin_collection_type;
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-object-methods-with-collections",
        "Disallow `Object` methods with `Map` or `Set`.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoObjectMethodsWithCollections>,
};

#[derive(Debug, Default)]
struct NoObjectMethodsWithCollections;

impl Rule for NoObjectMethodsWithCollections {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            args,
            optional: false,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        let &[arg] = ast.list(args) else {
            return;
        };
        let method = match ident_name(ast, prop) {
            Some(m @ ("entries" | "keys" | "values")) => m,
            _ => return,
        };
        let obj = skip_parens(ast, obj);
        if !matches!(ast.node(obj), Node::Ident { .. })
            || !cx.is_global_ref(obj, "Object")
            || matches!(ast.node(arg), Node::Spread { .. })
        {
            return;
        }
        let Some(kind) = builtin_collection_type(cx, arg) else {
            return;
        };
        let arg = skip_parens(ast, arg);
        let replacement = format!("{}.{method}()", member_object_text(cx, arg, arg));
        let fix = (!would_remove_comments(cx, cx.span(node), &[paren_span(cx, arg)])).then(|| {
            with_edits(
                Fix::replace(cx.span(node), format!("Array.from({replacement})")),
                space_around_keyword(cx, node),
            )
        });
        let report = cx
            .report_node(
                prop,
                format!("`Object.{method}()` does not return {kind} contents."),
            )
            .id("no-object-methods-with-collections/error");
        if let Some(fix) = fix {
            report.suggest(
                "no-object-methods-with-collections/suggestion",
                format!("Use `Array.from({replacement})`."),
                fix,
            );
        }
    }
}
