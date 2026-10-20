//! `@typescript-eslint/triple-slash-reference`: disallow certain triple-slash directives
//! in favor of ES6-style imports.

use super::prelude::*;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/triple-slash-reference",
        "Disallow certain triple slash directives in favor of ES6-style import declarations",
        TS_REC,
        Category::Style,
    ),
    create: create::<TripleSlashReference>,
};

#[derive(Debug)]
struct TripleSlashReference {
    lib_never: bool,
    path_never: bool,
    /// "always", "never" or "prefer-import".
    types: String,
}

impl FromOptions for TripleSlashReference {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(TripleSlashReference {
            lib_never: str_field(o, "lib") == Some("never"),
            path_never: str_field(o, "path").unwrap_or("never") == "never",
            types: str_field(o, "types").unwrap_or("prefer-import").to_owned(),
        })
    }
}

/// `/ <reference kind="value" ...` (the text after `//`).
fn parse_reference(text: &str) -> Option<(&str, &str)> {
    let t = text.strip_prefix('/')?.trim_start();
    let t = t.strip_prefix("<reference")?.trim_start();
    let kind = ["types", "path", "lib"]
        .into_iter()
        .find(|k| t.starts_with(k))?;
    let t = t[kind.len()..].trim_start().strip_prefix('=')?.trim_start();
    t.chars().next().filter(|c| matches!(c, '"' | '\'' | '|'))?;
    let rest = &t[1..];
    // Greedy up to the last quote, like `(.*)["|']`.
    let end = rest.rfind(['"', '\'', '|'])?;
    Some((kind, &rest[..end]))
}

impl Rule for TripleSlashReference {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut imports: Vec<&str> = Vec::new();
        for i in 0..ast.nodes.len() {
            let Some(id) = NodeId::from_raw(i as u32) else {
                continue;
            };
            match *ast.node(id) {
                Node::ImportDecl { source, .. } => {
                    if let Node::StringLit { value } = *ast.node(source) {
                        imports.push(ast.atom(value));
                    }
                }
                Node::ExternalModuleRef { expr } => {
                    if let Node::StringLit { value } = *ast.node(expr) {
                        imports.push(ast.atom(value));
                    }
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        for c in &ast.comments {
            if c.kind != CommentKind::Line {
                continue;
            }
            let text = &cx.src()[c.span.start as usize + 2..c.span.end as usize];
            let Some((kind, module)) = parse_reference(text) else {
                continue;
            };
            let bad = match kind {
                "lib" => self.lib_never,
                "path" => self.path_never,
                _ => match self.types.as_str() {
                    "never" => true,
                    "prefer-import" => imports.contains(&module),
                    _ => false,
                },
            };
            if bad {
                found.push((c.span, module.to_owned()));
            }
        }
        for (span, module) in found {
            cx.report(
                span,
                format!(
                    "Do not use a triple slash reference for {module}, use `import` style instead."
                ),
            )
            .id("tripleSlashReference");
        }
    }
}
