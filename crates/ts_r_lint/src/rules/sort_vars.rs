//! `sort-vars`: the names in one declaration in alphabetical order.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "sort-vars",
        "Require variables within the same declaration block to be sorted",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<SortVars>,
};

#[derive(Debug, Default)]
struct SortVars {
    ignore_case: bool,
}

impl FromOptions for SortVars {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(SortVars {
            ignore_case: bool_field(obj_opt(opts, 0), "ignoreCase", false),
        })
    }
}

impl Rule for SortVars {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDecl { decls, .. } = *cx.node(node) else {
            return;
        };
        // (declarator, sort key as UTF-16, as JavaScript compares strings).
        let ids: Vec<(NodeId, Vec<u16>)> = ast
            .list(decls)
            .iter()
            .filter_map(|&d| {
                let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                    return None;
                };
                let n = ident_name(ast, name)?;
                let key = if self.ignore_case {
                    n.to_lowercase().encode_utf16().collect()
                } else {
                    n.encode_utf16().collect()
                };
                Some((d, key))
            })
            .collect();
        if ids.len() < 2 {
            return;
        }
        let unfixable = ids.iter().any(|&(d, _)| {
            matches!(*ast.node(d), Node::VarDeclarator { init: Some(i), .. } if !matches!(
                ast.node(skip_parens(ast, i)),
                Node::StringLit { .. }
                    | Node::NumberLit { .. }
                    | Node::BigIntLit { .. }
                    | Node::BoolLit { .. }
                    | Node::NullLit {}
                    | Node::RegexLit { .. }
            ))
        });
        let src = cx.src();
        let mut fixed = false;
        let mut memo = 0;
        for i in 1..ids.len() {
            if ids[i].1 >= ids[memo].1 {
                memo = i;
                continue;
            }
            let fix = (!unfixable && !fixed).then(|| {
                let mut sorted: Vec<usize> = (0..ids.len()).collect();
                sorted.sort_by(|&a, &b| ids[a].1.cmp(&ids[b].1));
                let mut out = String::new();
                for (slot, &k) in sorted.iter().enumerate() {
                    out.push_str(cx.text(ids[k].0));
                    if let Some(next) = ids.get(slot + 1) {
                        let end = cx.span(ids[slot].0).end as usize;
                        out.push_str(&src[end..cx.span(next.0).start as usize]);
                    }
                }
                let span = Span::new(cx.span(ids[0].0).start, cx.span(ids[ids.len() - 1].0).end);
                Fix::replace(span, out)
            });
            fixed = true;
            cx.report_node(
                ids[i].0,
                "Variables within the same declaration block should be sorted alphabetically.",
            )
            .id("sortVars")
            .fix_opt(fix);
        }
    }
}
