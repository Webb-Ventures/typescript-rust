//! `max-lines-per-function`: at most `max` (50) lines in a function (a method counts from
//! its key), optionally not counting blank and comment-only lines or IIFEs.

use super::func_names::own_name_with_kind;
use super::max_lines::{comment_only_lines, line_starts};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-lines-per-function",
        "Enforce a maximum number of lines of code in a function",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxLinesPerFunction>,
};

#[derive(Debug, Default)]
struct MaxLinesPerFunction {
    max: u64,
    skip_comments: bool,
    skip_blank: bool,
    iifes: bool,
}

impl FromOptions for MaxLinesPerFunction {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let max = match opts.first() {
            Some(Value::Number(n)) => n.as_u64(),
            _ => o.get("max").and_then(Value::as_u64),
        }
        .unwrap_or(50);
        Ok(MaxLinesPerFunction {
            max,
            skip_comments: bool_field(o, "skipComments", false),
            skip_blank: bool_field(o, "skipBlankLines", false),
            iifes: bool_field(o, "IIFEs", false),
        })
    }
}

impl Rule for MaxLinesPerFunction {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let mut funcs = Vec::new();
        let mut stack = vec![ast.root];
        while let Some(n) = stack.pop() {
            if matches!(
                ast.node(n),
                Node::FunctionDecl { body: Some(_), .. }
                    | Node::FunctionExpr { .. }
                    | Node::Arrow { .. }
            ) {
                funcs.push(n);
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        if funcs.is_empty() {
            return;
        }
        funcs.sort_by_key(|&f| cx.span(f).start);
        let starts = line_starts(src);
        let comment_lines = if self.skip_comments {
            comment_only_lines(cx, &starts)
        } else {
            Vec::new()
        };
        let line_of = |pos: usize| starts.partition_point(|&s| s <= pos) - 1;
        let text = |i: usize| {
            let end = starts.get(i + 1).copied().unwrap_or(src.len());
            &src[starts[i]..end]
        };
        for f in funcs {
            let parent = cx.parent(f);
            let node = match parent.map(|p| (p, ast.node(p))) {
                Some((p, Node::MethodDef { value, .. })) if *value == f => p,
                _ => f,
            };
            if !self.iifes
                && !matches!(ast.node(f), Node::FunctionDecl { .. })
                && let Some(p) = parent_skip_parens(ast, cx.file.parents, f)
                && let Node::Call { callee, .. } = *ast.node(p)
                && skip_parens(ast, callee) == f
            {
                continue;
            }
            let span = cx.span(node);
            let first = line_of(span.start as usize);
            let last = line_of(
                (span.end as usize)
                    .saturating_sub(1)
                    .max(span.start as usize),
            );
            let count = (first..=last)
                .filter(|&l| !(self.skip_comments && comment_lines[l]))
                .filter(|&l| !(self.skip_blank && text(l).trim().is_empty()))
                .count() as u64;
            if count <= self.max {
                continue;
            }
            let name = own_name_with_kind(cx, f);
            let mut c = name.chars();
            let name: String = match c.next() {
                Some(h) => h.to_uppercase().chain(c).collect(),
                None => name,
            };
            let head = function_head_span(ast, src, cx.file.parents, f);
            cx.report(
                head,
                format!(
                    "{name} has too many lines ({count}). Maximum allowed is {}.",
                    self.max
                ),
            )
            .id("exceed");
        }
    }
}
