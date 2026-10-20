//! `no-empty-character-class`: disallow empty character classes in regular expressions.

use super::prelude::*;
use crate::regex::{self, Flags, RKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-empty-character-class",
        "Disallow empty character classes in regular expressions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoEmptyCharacterClass>,
};

#[derive(Debug, Default)]
struct NoEmptyCharacterClass;

impl Rule for NoEmptyCharacterClass {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some((pattern, flags)) = regex_parts(cx.ast(), node) else {
            return;
        };
        let Ok(r) = regex::parse(pattern, Flags::new(flags)) else {
            return;
        };
        let empty = r
            .nodes
            .iter()
            .any(|n| matches!(n.kind, RKind::Class { negate: false }) && n.children.is_empty());
        if empty {
            cx.report_node(node, "Empty class.").id("unexpected");
        }
    }
}
