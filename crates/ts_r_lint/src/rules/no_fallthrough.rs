//! `no-fallthrough`: disallow fallthrough of `case` statements.

use super::prelude::*;
use crate::cfg::Cfg;
use crate::regex::{self, Regex};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-fallthrough",
        "Disallow fallthrough of `case` statements",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoFallthrough>,
};

#[derive(Debug, Default)]
struct NoFallthrough {
    pattern: Option<String>,
    allow_empty_case: bool,
    report_unused: bool,
}

impl FromOptions for NoFallthrough {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoFallthrough {
            pattern: str_field(o, "commentPattern").map(str::to_owned),
            allow_empty_case: bool_field(o, "allowEmptyCase", false),
            report_unused: bool_field(o, "reportUnusedFallthroughComment", false),
        })
    }
}

/// The nearest function, static block or program around `node`, with its graph.
pub(crate) fn enclosing_cfg(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Cfg> {
    let ast = cx.ast();
    let mut cur = cx.parent(node);
    while let Some(p) = cur {
        match *ast.node(p) {
            Node::Program { stmts } => return Some(Cfg::statements(ast, ast.list(stmts))),
            Node::StaticBlock { body } => return Some(Cfg::statements(ast, ast.list(body))),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                return Some(Cfg::function(ast, p));
            }
            _ => {}
        }
        cur = cx.parent(p);
    }
    None
}

fn default_pattern(text: &str) -> bool {
    // /falls?\s?through/i
    let lower = text.to_lowercase();
    let b = lower.as_bytes();
    let mut i = 0;
    while let Some(off) = lower[i..].find("fall") {
        let mut j = i + off + 4;
        if b.get(j) == Some(&b's') {
            j += 1;
        }
        let rest = &lower[j..];
        let ws = rest
            .chars()
            .next()
            .filter(|c| c.is_whitespace())
            .map_or(0, char::len_utf8);
        if rest[ws..].starts_with("through") || rest.starts_with("through") {
            return true;
        }
        i += off + 4;
    }
    false
}

impl NoFallthrough {
    fn is_fallthrough_comment(&self, compiled: Option<&Regex<'_>>, text: &str) -> bool {
        // Directive comments (`eslint-disable-next-line no-fallthrough`) never count.
        if text.starts_with("eslint-") || text.starts_with("eslint ") {
            return false;
        }
        match (&self.pattern, compiled) {
            (Some(_), Some(r)) => r.is_match(text, false),
            (Some(_), None) => false,
            (None, _) => default_pattern(text),
        }
    }

    /// The fallthrough comment between case `case` and the next case, if any.
    fn comment(&self, cx: &Ctx<'_, '_>, case: NodeId, next: NodeId) -> Option<Span> {
        let ast = cx.ast();
        let src = cx.src();
        let compiled = self.pattern.as_deref().and_then(regex::compile);
        let comment_text = |c: &ts_r_ast::Comment| {
            let t = c.span.text(src);
            let t = t
                .strip_prefix("//")
                .or_else(|| t.strip_prefix("/*"))
                .unwrap_or(t);
            t.strip_suffix("*/").unwrap_or(t).trim().to_owned()
        };
        // The last comment right before the next case.
        let next_start = cx.span(next).start;
        let before = ast.comments.iter().rfind(|c| c.span.end <= next_start);
        if let Some(c) = before
            && src[c.span.end as usize..next_start as usize]
                .trim()
                .is_empty()
            && c.span.start >= cx.span(case).start
            && self.is_fallthrough_comment(compiled.as_ref(), &comment_text(c))
        {
            return Some(c.span);
        }
        // The last comment inside a case that is a single block.
        if let Node::SwitchCase { body, .. } = *ast.node(case)
            && let [only] = ast.list(body)
            && matches!(ast.node(*only), Node::Block { .. })
        {
            let close = cx.span(*only).end - 1;
            if let Some(c) = ast.comments.iter().rfind(|c| c.span.end <= close)
                && c.span.start > cx.span(*only).start
                && src[c.span.end as usize..close as usize].trim().is_empty()
                && self.is_fallthrough_comment(compiled.as_ref(), &comment_text(c))
            {
                return Some(c.span);
            }
        }
        None
    }
}

fn line_of(src: &str, pos: u32) -> usize {
    src[..pos as usize].matches('\n').count()
}

impl Rule for NoFallthrough {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Switch]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Switch { cases, .. } = *ast.node(node) else {
            return;
        };
        let cases = ast.list(cases);
        if cases.len() < 2 {
            return;
        }
        let Some(cfg) = enclosing_cfg(cx, node) else {
            return;
        };
        let src = cx.src();
        for w in cases.windows(2) {
            let (case, next) = (w[0], w[1]);
            let Node::SwitchCase { body, .. } = *ast.node(case) else {
                continue;
            };
            let falls = cfg
                .case_ends
                .iter()
                .filter(|(c, _)| *c == case)
                .any(|&(_, b)| cfg.is_reachable(b));
            let comment = self.comment(cx, case, next);
            if !falls {
                if self.report_unused
                    && let Some(c) = comment
                {
                    cx.report(c, "Found a comment that would permit fallthrough, but case cannot fall through.")
                        .id("unusedFallthroughComment");
                }
                continue;
            }
            if ast.list(body).is_empty()
                && (self.allow_empty_case
                    || line_of(src, cx.span(next).start) <= line_of(src, cx.span(case).end) + 1)
            {
                continue;
            }
            if comment.is_some() {
                continue;
            }
            let what = if matches!(ast.node(next), Node::SwitchCase { test: None, .. }) {
                "default"
            } else {
                "case"
            };
            cx.report_node(
                next,
                format!("Expected a 'break' statement before '{what}'."),
            )
            .id(if what == "default" { "default" } else { "case" });
        }
    }
}
