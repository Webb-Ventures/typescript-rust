//! `default-case`: require a `default` case in `switch` statements, unless the last
//! comment in the switch says it is left out on purpose.

use super::prelude::*;
use crate::regex;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "default-case",
        "Require `default` cases in `switch` statements",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<DefaultCase>,
};

#[derive(Debug, Default)]
struct DefaultCase {
    comment_pattern: Option<String>,
}

impl FromOptions for DefaultCase {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(DefaultCase {
            comment_pattern: str_field(obj_opt(opts, 0), "commentPattern").map(str::to_owned),
        })
    }
}

impl Rule for DefaultCase {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Switch]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Switch { cases, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let cases = ast.list(cases);
        let Some(&last) = cases.last() else {
            return;
        };
        if cases
            .iter()
            .any(|&c| matches!(ast.node(c), Node::SwitchCase { test: None, .. }))
        {
            return;
        }
        let (from, to) = (cx.span(last).end, cx.span(node).end);
        let comment = ast
            .comments
            .iter()
            .rfind(|c| c.span.start >= from && c.span.end <= to);
        if let Some(c) = comment {
            let text = c.span.text(cx.src());
            let value = match c.kind {
                CommentKind::Line => text.get(2..).unwrap_or(""),
                _ => text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
            }
            .trim();
            let ok = match &self.comment_pattern {
                Some(p) => regex::compile(p).is_some_and(|r| r.is_match(value, false)),
                None => value.eq_ignore_ascii_case("no default"),
            };
            if ok {
                return;
            }
        }
        cx.report(cx.span(node), "Expected a default case.")
            .id("missingDefaultCase");
    }
}
