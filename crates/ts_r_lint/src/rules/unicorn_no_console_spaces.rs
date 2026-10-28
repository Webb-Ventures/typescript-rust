//! `unicorn/no-console-spaces`: no leading or trailing space in a string argument of
//! `console.log`, `.debug`, `.info`, `.warn` or `.error` (console adds one between
//! arguments).
//!
//! A string or template argument (raw text between its delimiters) is reported when it
//! starts with exactly one space and isn't the first argument, or ends with exactly one
//! unescaped space and isn't the last. The report and the fix (delete it) are on the space.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-console-spaces",
        "Do not use leading/trailing space between `console.log` parameters.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoConsoleSpaces>,
};

#[derive(Debug, Default)]
struct NoConsoleSpaces;

impl Rule for NoConsoleSpaces {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        let Some(method @ ("log" | "debug" | "info" | "warn" | "error")) = ident_name(ast, prop)
        else {
            return;
        };
        if ident_name(ast, skip_parens(ast, obj)) != Some("console") {
            return;
        }
        let args = ast.list(args);
        let src = cx.src();
        for (i, &arg) in args.iter().enumerate() {
            let arg = skip_parens(ast, arg);
            if !matches!(
                ast.node(arg),
                Node::StringLit { .. } | Node::TemplateLit { .. }
            ) {
                continue;
            }
            let span = cx.span(arg);
            let Some(raw) = src.get(span.start as usize + 1..(span.end as usize).saturating_sub(1))
            else {
                continue;
            };
            let b = raw.as_bytes();
            if i != 0 && b.len() > 1 && b[0] == b' ' && b[1] != b' ' {
                report(cx, method, "leading", span.start + 1);
            }
            let n = b.len();
            if i != args.len() - 1
                && n > 1
                && b[n - 1] == b' '
                && b[n - 2] != b' '
                && b[..n - 1].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 0
            {
                report(cx, method, "trailing", span.end - 2);
            }
        }
    }
}

fn report(cx: &mut Ctx<'_, '_>, method: &str, position: &str, at: u32) {
    let space = Span::new(at, at + 1);
    cx.report(
        space,
        format!("Do not use {position} space between `console.{method}` parameters."),
    )
    .id("no-console-spaces")
    .fix(Fix::delete(space));
}
