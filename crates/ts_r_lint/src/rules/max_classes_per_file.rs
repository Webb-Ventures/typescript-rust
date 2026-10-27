//! `max-classes-per-file`: at most `max` (1) classes in a file.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-classes-per-file",
        "Enforce a maximum number of classes per file",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxClassesPerFile>,
};

#[derive(Debug, Default)]
struct MaxClassesPerFile {
    max: u64,
    ignore_expressions: bool,
}

impl FromOptions for MaxClassesPerFile {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(match opts.first() {
            Some(Value::Number(n)) => MaxClassesPerFile {
                max: n.as_u64().unwrap_or(1),
                ignore_expressions: false,
            },
            Some(o) => MaxClassesPerFile {
                max: o
                    .get("max")
                    .and_then(Value::as_u64)
                    .filter(|&m| m > 0)
                    .unwrap_or(1),
                ignore_expressions: bool_field(obj_opt(opts, 0), "ignoreExpressions", false),
            },
            None => MaxClassesPerFile {
                max: 1,
                ignore_expressions: false,
            },
        })
    }
}

impl Rule for MaxClassesPerFile {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut count = 0u64;
        let mut stack = vec![ast.root];
        while let Some(n) = stack.pop() {
            match ast.node(n) {
                Node::ClassDecl { .. } => count += 1,
                Node::ClassExpr { .. } if !self.ignore_expressions => count += 1,
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        if count <= self.max {
            return;
        }
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let stmts = ast.list(stmts);
        let (Some(&first), Some(&last)) = (stmts.first(), stmts.last()) else {
            return;
        };
        let span = Span::new(cx.span(first).start, cx.span(last).end);
        cx.report(
            span,
            format!(
                "File has too many classes ({count}). Maximum allowed is {}.",
                self.max
            ),
        )
        .id("maximumExceeded");
    }
}
