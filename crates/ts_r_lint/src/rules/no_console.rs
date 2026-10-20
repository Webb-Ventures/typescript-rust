//! `no-console`: disallow the use of `console`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-console",
        "Disallow the use of `console`",
        NO_PRESET,
        Category::Restriction,
    )
    .suggestions(),
    create: create::<NoConsole>,
};

#[derive(Debug)]
struct NoConsole {
    allow: Vec<String>,
}

impl FromOptions for NoConsole {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoConsole {
            allow: str_list(obj_opt(opts, 0), "allow"),
        })
    }
}

/// Removing `stmt` would let its neighbours join into one statement.
fn removal_hazard(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let ast = cx.ast();
    let Some(prev) = prev_statement(ast, cx.file.parents, stmt) else {
        return false;
    };
    let next = skip_trivia_forward(cx.src(), cx.span(stmt).end as usize);
    match cx.src().as_bytes().get(next) {
        Some(b'[' | b'(' | b'`') => stmt_continuable(ast, cx.src(), prev),
        // `a++` continues into a division or a binary `+`/`-`.
        Some(b'/' | b'+' | b'-') => {
            stmt_continuable(ast, cx.src(), prev)
                || (matches!(cx.node(prev), Node::ExprStmt { .. })
                    && !cx.text(prev).trim_end().ends_with(';'))
        }
        _ => false,
    }
}

impl Rule for NoConsole {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let obj = match *cx.node(node) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            _ => return,
        };
        if ident_name(ast, obj) != Some("console") {
            return;
        }
        let sem = cx.semantic();
        if let Some(r) = sem.reference_at(obj)
            && let Resolution::Symbol(s) = sem.reference(r).resolution
            && !sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL)
        {
            return;
        }
        let name = member_name(ast, cx.src(), node).map(|n| n.into_owned());
        if name.as_ref().is_some_and(|n| self.allow.contains(n)) {
            return;
        }
        // A whole `console.x(...)` statement in a statement list can be removed.
        let stmt = cx.parent(node).and_then(|c| {
            let call = matches!(*cx.node(c), Node::Call { callee, .. } if callee == node);
            let s = cx.parent(c)?;
            let in_list = cx
                .parent(s)
                .is_some_and(|l| statement_list(ast, l).is_some());
            (call && matches!(cx.node(s), Node::ExprStmt { .. }) && in_list).then_some(s)
        });
        let computed = matches!(cx.node(node), Node::Index { .. });
        let sugg = stmt.filter(|&s| !removal_hazard(cx, s)).map(|s| {
            let fix = Fix::delete(cx.span(s));
            match (&name, computed) {
                (Some(n), false) => ("removeConsole", format!("Remove the console.{n}()."), fix),
                _ => (
                    "removeMethodCall",
                    "Remove the console method call.".to_owned(),
                    fix,
                ),
            }
        });
        let r = if self.allow.is_empty() {
            cx.report_node(node, "Unexpected console statement.")
                .id("unexpected")
        } else {
            let list = self.allow.join(", ");
            cx.report_node(
                node,
                format!(
                    "Unexpected console statement. Only these console methods are allowed: {list}."
                ),
            )
            .id("limited")
        };
        if let Some((id, desc, fix)) = sugg {
            let _ = r.suggest(id, desc, fix);
        }
    }
}
