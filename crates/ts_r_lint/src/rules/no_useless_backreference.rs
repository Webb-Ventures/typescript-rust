//! `no-useless-backreference`: disallow useless backreferences in regular expressions.

use super::prelude::*;
use crate::regex::{self, Flags, RId, RKind, Regex};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-backreference",
        "Disallow useless backreferences in regular expressions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUselessBackreference>,
};

#[derive(Debug, Default)]
struct NoUselessBackreference;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Problem {
    Nested,
    Forward,
    Backward,
    Disjunctive,
    IntoNegativeLookaround,
}

fn problem(r: &Regex<'_>, bref: RId, group: RId) -> Option<Problem> {
    let banc: Vec<RId> = std::iter::once(bref).chain(r.ancestors(bref)).collect();
    if banc.contains(&group) {
        return Some(Problem::Nested);
    }
    // Lowest common ancestor and the child on the group's side.
    let mut g_path = vec![group];
    let mut lca = group;
    for a in r.ancestors(group) {
        if banc.contains(&a) {
            lca = a;
            break;
        }
        g_path.push(a);
    }
    let g_child = *g_path.last()?;
    let b_child = banc[banc.iter().position(|&a| a == lca)?.checked_sub(1)?];
    if !matches!(r.kind(lca), RKind::Alternative) && g_child != b_child {
        return Some(Problem::Disjunctive);
    }
    let behind = std::iter::once(lca)
        .chain(r.ancestors(lca))
        .find_map(|a| match r.kind(a) {
            RKind::Lookaround { behind, .. } => Some(*behind),
            _ => None,
        })
        .unwrap_or(false);
    let (g, b) = (r.node(group), r.node(bref));
    if behind {
        if g.end <= b.start {
            return Some(Problem::Backward);
        }
    } else if g.start >= b.end {
        return Some(Problem::Forward);
    }
    if g_path
        .iter()
        .any(|&a| matches!(r.kind(a), RKind::Lookaround { negate: true, .. }))
    {
        return Some(Problem::IntoNegativeLookaround);
    }
    None
}

fn check(cx: &mut Ctx<'_, '_>, node: NodeId, pattern: &str, flags: &str) {
    let Ok(r) = regex::parse(pattern, Flags::new(flags)) else {
        return;
    };
    let mut brefs: Vec<RId> = (0..r.nodes.len() as RId)
        .filter(|&i| matches!(r.kind(i), RKind::Backref { .. }))
        .collect();
    brefs.sort_by_key(|&b| r.node(b).start);
    for b in brefs {
        let targets = r.backref_targets(b);
        if targets.is_empty() {
            continue;
        }
        let problems: Vec<Option<Problem>> = targets.iter().map(|&g| problem(&r, b, g)).collect();
        if problems.iter().any(Option::is_none) {
            continue;
        }
        let Some(kind) = problems.iter().flatten().min().copied() else {
            continue;
        };
        let groups: Vec<RId> = targets
            .iter()
            .zip(&problems)
            .filter(|(_, p)| **p == Some(kind))
            .map(|(&g, _)| g)
            .collect();
        let other = match groups.len() {
            1 => String::new(),
            2 => " and another group".to_owned(),
            n => format!(" and other {} groups", n - 1),
        };
        let (id, tail) = match kind {
            Problem::Nested => ("nested", "from within that group"),
            Problem::Forward => ("forward", "which appears later in the pattern"),
            Problem::Backward => ("backward", "which appears before in the same lookbehind"),
            Problem::Disjunctive => ("disjunctive", "which is in another alternative"),
            Problem::IntoNegativeLookaround => (
                "intoNegativeLookaround",
                "which is in a negative lookaround",
            ),
        };
        let msg = format!(
            "Backreference '{}' will be ignored. It references group '{}'{other} {tail}.",
            r.text(b),
            r.text(groups[0])
        );
        cx.report_node(node, msg).id(id);
    }
}

impl Rule for NoUselessBackreference {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit, NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Some((pattern, flags)) = regex_parts(ast, node) {
            check(cx, node, pattern, flags);
            return;
        }
        let Some(args) = regexp_call_args(cx, node, true, true) else {
            return;
        };
        let Some(pattern) = args.first().and_then(|&a| static_string_eval(cx, a)) else {
            return;
        };
        let flags = match args.get(1) {
            None => String::new(),
            Some(&f) => static_string_eval(cx, f).unwrap_or_default(),
        };
        check(cx, node, &pattern, &flags);
    }
}
