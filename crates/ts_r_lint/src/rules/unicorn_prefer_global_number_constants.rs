//! `unicorn/prefer-global-number-constants`: reads of the global `Number.NaN`,
//! `Number.POSITIVE_INFINITY` and `Number.NEGATIVE_INFINITY` (also through `globalThis`-like
//! objects and aliases, see [`global_path`]) become `NaN`, `Infinity` and `-Infinity`,
//! unless the replacement name is declared in scope. Writes are skipped. The fix (not for
//! `NEGATIVE_INFINITY`, nor when the member holds a comment) replaces the member.

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_util::global_path;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-global-number-constants",
        "Prefer global numeric constants over `Number` static properties.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferGlobalNumberConstants>,
};

#[derive(Debug, Default)]
struct PreferGlobalNumberConstants;

impl Rule for PreferGlobalNumberConstants {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(path) = global_path(cx, node) else {
            return;
        };
        let (property, replacement) = match path.as_str() {
            "Number.NaN" => ("NaN", "NaN"),
            "Number.POSITIVE_INFINITY" => ("POSITIVE_INFINITY", "Infinity"),
            "Number.NEGATIVE_INFINITY" => ("NEGATIVE_INFINITY", "-Infinity"),
            _ => return,
        };
        if is_left_hand_side(cx, node) {
            return;
        }
        let name = if property == "NaN" { "NaN" } else { "Infinity" };
        let sem = cx.semantic();
        if sem
            .lookup(sem.enclosing_scope(node), name)
            .is_some_and(|s| !sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL))
        {
            return;
        }
        let span = cx.span(node);
        let fix = (property != "NEGATIVE_INFINITY" && cx.ast().comments_in(span).is_empty())
            .then(|| Fix::replace(span, replacement));
        let report = cx
            .report(
                span,
                format!("Prefer `{replacement}` over `Number.{property}`."),
            )
            .id("prefer-global-number-constants");
        if let Some(fix) = fix {
            report.fix(fix);
        }
    }
}
