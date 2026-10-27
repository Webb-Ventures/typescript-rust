//! `init-declarations`: require (`"always"`, the default) or disallow (`"never"`) an
//! initializer on variable declarations. Shared with `@typescript-eslint/init-declarations`.

use super::prelude::*;
use ts_r_ast::VarKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "init-declarations",
        "Require or disallow initialization in variable declarations",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<InitDeclarations>,
};

#[derive(Debug)]
pub(crate) struct InitDeclarations {
    never: bool,
    ignore_for_loop_init: bool,
}

impl FromOptions for InitDeclarations {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(InitDeclarations {
            never: opts.first().and_then(Value::as_str) == Some("never"),
            ignore_for_loop_init: bool_field(obj_opt(opts, 1), "ignoreForLoopInit", false),
        })
    }
}

impl Rule for InitDeclarations {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = *cx.node(node)
        else {
            return;
        };
        let ast = cx.ast();
        // Ambient declarations: `declare let x;` and anything inside `declare namespace`.
        if modifiers.contains(Modifiers::DECLARE)
            || ancestors_inclusive(cx.file.parents, node).any(|a| {
                matches!(*cx.node(a), Node::Module { modifiers, .. } if modifiers.contains(Modifiers::DECLARE))
            })
        {
            return;
        }
        // A `for (x in ...)` / `for (x of ...)` / `for (x; ...)` head counts as initialized.
        let for_head = cx.parent(node).filter(|&p| match *cx.node(p) {
            Node::For { init, .. } => init == Some(node),
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => left == node,
            _ => false,
        });
        let constant = matches!(kind, VarKind::Const | VarKind::Using | VarKind::AwaitUsing);
        for &d in ast.list(decls) {
            let Node::VarDeclarator { name, init, .. } = *cx.node(d) else {
                continue;
            };
            let initialized = for_head.is_some() || init.is_some();
            let (id, msg) = if !self.never && !initialized {
                ("initialized", "should be initialized")
            } else if self.never
                && !constant
                && initialized
                && !(self.ignore_for_loop_init && for_head.is_some())
            {
                ("notInitialized", "should not be initialized")
            } else {
                continue;
            };
            let Some(n) = (matches!(cx.node(name), Node::Ident { .. }))
                .then(|| ident_name(ast, name))
                .flatten()
            else {
                continue;
            };
            let mut span = ast.span(d);
            // typescript-eslint reports an uninitialized declarator at its name only.
            if init.is_none() && cx.rule_name().starts_with("@typescript-eslint/") {
                span = Span::new(span.start, span.start + n.len() as u32);
            }
            let text = format!("Variable '{n}' {msg} on declaration.");
            cx.report(span, text).id(id);
        }
    }
}
