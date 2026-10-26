//! `unicorn/prefer-structured-clone`: `JSON.parse(JSON.stringify(x))` and `_.cloneDeep(x)` /
//! `lodash.cloneDeep(x)` (plus the `functions` option's names and paths) deep-clone the long
//! way. Each call must have exactly one non-spread argument and no optional chaining.
//!
//! The JSON form is reported from the start of `JSON.parse(...)` to the end of the inner
//! callee; its suggestion renames `JSON.parse` and deletes `JSON.stringify` with its
//! parentheses and the inner call's `(`, `)` and trailing comma. A clone function is
//! reported on its callee, whose text the suggestion replaces. No suggestion when a comment
//! sits inside a replaced callee.

use super::prelude::*;
use super::unicorn_ast::{call_tokens, matches_path, method_call, no_spread_in};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-structured-clone",
        "Prefer using `structuredClone` to create a deep clone.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<PreferStructuredClone>,
};

#[derive(Debug, Default)]
struct PreferStructuredClone {
    functions: Vec<String>,
}

impl FromOptions for PreferStructuredClone {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut functions = str_list(obj_opt(opts, 0), "functions");
        functions.extend(["_.cloneDeep".to_owned(), "lodash.cloneDeep".to_owned()]);
        Ok(PreferStructuredClone { functions })
    }
}

const SUGGEST_ID: &str = "prefer-structured-clone/suggestion";
const SUGGEST: &str = "Switch to `structuredClone(…)`.";

fn json_call(ast: &Ast, id: NodeId, method: &str) -> bool {
    method_call(ast, id).is_some_and(|c| {
        c.plain()
            && c.name == method
            && ident_name(ast, skip_parens(ast, c.obj)) == Some("JSON")
            && c.args.len() == 1
            && no_spread_in(ast, c.args, 1)
    })
}

impl Rule for PreferStructuredClone {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let args = ast.list(args);
        if json_call(ast, node, "parse") {
            let inner = skip_parens(ast, args[0]);
            if json_call(ast, inner, "stringify") {
                self.report_json(cx, node, callee, inner);
            }
        }
        if optional || args.len() != 1 || !no_spread_in(ast, args, 1) {
            return;
        }
        let Some(name) = self.functions.iter().find(|f| matches_path(ast, callee, f)) else {
            return;
        };
        let target = skip_parens(ast, callee);
        let span = cx.span(target);
        let msg = format!(
            "Prefer `structuredClone(…)` over `{}(…)` to create a deep clone.",
            name.trim()
        );
        let quiet = !ast.comments_in(span).is_empty();
        let r = cx.report(span, msg).id("prefer-structured-clone/error");
        if !quiet {
            r.suggest(SUGGEST_ID, SUGGEST, Fix::replace(span, "structuredClone"));
        }
    }
}

impl PreferStructuredClone {
    fn report_json(
        &self,
        cx: &mut Ctx<'_, '_>,
        parse: NodeId,
        parse_callee: NodeId,
        inner: NodeId,
    ) {
        let ast = cx.ast();
        let Node::Call {
            callee: str_callee, ..
        } = *ast.node(inner)
        else {
            return;
        };
        let parse_target = cx.span(skip_parens(ast, parse_callee));
        let str_target = cx.span(skip_parens(ast, str_callee));
        let span = Span::new(cx.span(parse).start, str_target.end);
        let quiet =
            !ast.comments_in(parse_target).is_empty() || !ast.comments_in(str_target).is_empty();
        let fix = if quiet {
            None
        } else {
            call_tokens(cx, inner).map(|(open, close, comma)| {
                let mut fix = Fix::replace(parse_target, "structuredClone");
                // The callee and the parentheses around it.
                let mut c = skip_parens(ast, str_callee);
                fix = fix.and(cx.span(c), "");
                while let Some(p) = cx.parent(c)
                    && matches!(ast.node(p), Node::Paren { .. })
                {
                    let ps = cx.span(p);
                    fix = fix.and(Span::new(ps.start, ps.start + 1), "");
                    fix = fix.and(Span::new(ps.end - 1, ps.end), "");
                    c = p;
                }
                fix = fix.and(Span::new(open, open + 1), "");
                fix = fix.and(Span::new(close, close + 1), "");
                if let Some(comma) = comma {
                    fix = fix.and(Span::new(comma, comma + 1), "");
                }
                fix
            })
        };
        let r = cx
            .report(
                span,
                "Prefer `structuredClone(…)` over `JSON.parse(JSON.stringify(…))` to create a deep clone.",
            )
            .id("prefer-structured-clone/error");
        if let Some(fix) = fix {
            r.suggest(SUGGEST_ID, SUGGEST, fix);
        }
    }
}
