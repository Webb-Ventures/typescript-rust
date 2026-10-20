//! `use-isnan`: require `isNaN()` when checking for `NaN`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "use-isnan",
        "Require calls to `isNaN()` when checking for `NaN`",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create::<UseIsNan>,
};

#[derive(Debug, Default)]
struct UseIsNan {
    switch_case: bool,
    index_of: bool,
}

impl FromOptions for UseIsNan {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(UseIsNan {
            switch_case: bool_field(o, "enforceForSwitchCase", true),
            index_of: bool_field(o, "enforceForIndexOf", false),
        })
    }
}

/// `NaN`, `Number.NaN`, `Number?.NaN`, `Number['NaN']`, or a sequence ending in one.
fn is_nan(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => cx.is_global_ref(id, "NaN"),
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            cx.is_global_ref(obj, "Number") && member_name(ast, src, id).as_deref() == Some("NaN")
        }
        Node::Seq { exprs } => ast.list(exprs).last().is_some_and(|&l| is_nan(cx, l)),
        _ => false,
    }
}

impl Rule for UseIsNan {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Switch, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        match *cx.node(node) {
            Node::Binary { left, op, right } => {
                use BinaryOp::*;
                if !matches!(
                    op,
                    Eq | NotEq | StrictEq | StrictNotEq | Lt | LtEq | Gt | GtEq
                ) {
                    return;
                }
                let (ln, rn) = (is_nan(cx, left), is_nan(cx, right));
                if !ln && !rn {
                    return;
                }
                let msg = "Use the isNaN function to compare with NaN.";
                let equality = matches!(op, Eq | NotEq | StrictEq | StrictNotEq);
                let other = if ln { right } else { left };
                let other_is_nan_seq = matches!(
                    ast.node(skip_parens(ast, if ln { left } else { right })),
                    Node::Seq { .. }
                );
                if !equality || (ln && rn) || other_is_nan_seq {
                    cx.report_node(node, msg).id("comparisonWithNaN");
                    return;
                }
                let neg = if matches!(op, NotEq | StrictNotEq) {
                    "!"
                } else {
                    ""
                };
                let o = skip_parens(ast, other);
                let mut ot = ast.span(o).text(src).to_owned();
                if matches!(ast.node(o), Node::Seq { .. }) {
                    ot = format!("({ot})");
                }
                let whole = ast.span(node);
                let mut rep = cx.report(whole, msg).id("comparisonWithNaN").suggest(
                    "replaceWithIsNaN",
                    "Replace with Number.isNaN.",
                    Fix::replace(whole, format!("{neg}Number.isNaN({ot})")),
                );
                if matches!(op, Eq | NotEq) {
                    rep = rep.suggest(
                        "replaceWithCastingAndIsNaN",
                        "Replace with Number.isNaN and cast to a Number.",
                        Fix::replace(whole, format!("{neg}Number.isNaN(Number({ot}))")),
                    );
                }
                let _ = rep;
            }
            Node::Switch { disc, cases } => {
                if !self.switch_case {
                    return;
                }
                if is_nan(cx, disc) {
                    cx.report_node(
                        node,
                        "'switch(NaN)' can never match a case clause. Use Number.isNaN instead of the switch.",
                    )
                    .id("switchNaN");
                }
                for &c in ast.list(cases) {
                    if let Node::SwitchCase { test: Some(t), .. } = *ast.node(c)
                        && is_nan(cx, t)
                    {
                        cx.report_node(
                            c,
                            "'case NaN' can never match. Use Number.isNaN before the switch.",
                        )
                        .id("caseNaN");
                    }
                }
            }
            Node::Call { callee, args, .. } => {
                if !self.index_of {
                    return;
                }
                let callee = skip_parens(ast, callee);
                let Some(name) = member_name(ast, src, callee) else {
                    return;
                };
                if name != "indexOf" && name != "lastIndexOf" {
                    return;
                }
                let a = ast.list(args);
                if a.is_empty() || a.len() > 2 || !is_nan(cx, a[0]) {
                    return;
                }
                let msg = format!("Array prototype method '{name}' cannot find NaN.");
                let first = skip_parens(ast, a[0]);
                let simple = a.len() == 1 && !matches!(ast.node(first), Node::Seq { .. });
                let new_name = if name == "indexOf" {
                    "findIndex"
                } else {
                    "findLastIndex"
                };
                let prop_span = match *ast.node(callee) {
                    Node::Member { prop, .. } => Some((ast.span(prop), new_name.to_owned())),
                    Node::Index { index, .. } => Some((ast.span(index), format!("\"{new_name}\""))),
                    _ => None,
                };
                match (simple, prop_span) {
                    (true, Some((ps, text))) => {
                        let fix = Fix::replace(ps, text).and(ast.span(a[0]), "Number.isNaN");
                        cx.report_node(node, msg).id("indexOfNaN").suggest(
                            "replaceWithFindIndex",
                            format!("Replace with Array.prototype.{new_name}."),
                            fix,
                        );
                    }
                    _ => {
                        cx.report_node(node, msg).id("indexOfNaN");
                    }
                }
            }
            _ => {}
        }
    }
}
