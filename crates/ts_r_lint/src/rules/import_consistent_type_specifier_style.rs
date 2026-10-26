//! `import/consistent-type-specifier-style`: `"prefer-inline"` (the default) reports
//! `import type { A }` and fixes it to `import { type A }`; `"prefer-top-level"` reports
//! inline `type` specifiers and moves them to a new `import type` declaration after the
//! original (replacing it when every specifier is a type). A lone default or namespace
//! binding is both styles and is never reported.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/consistent-type-specifier-style",
        "Enforce or ban the use of inline type-only markers for named imports.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentTypeSpecifierStyle>,
};

#[derive(Debug)]
struct ConsistentTypeSpecifierStyle {
    prefer_inline: bool,
}

impl FromOptions for ConsistentTypeSpecifierStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ConsistentTypeSpecifierStyle {
            prefer_inline: opts.first().and_then(Value::as_str) != Some("prefer-top-level"),
        })
    }
}

fn is_lone_binding(ast: &Ast, specs: &[NodeId]) -> bool {
    matches!(specs, &[s] if matches!(ast.kind(s), NodeKind::ImportDefaultSpecifier | NodeKind::ImportNamespaceSpecifier))
}

/// The span of the `,` right after `pos`, if that is the next token.
fn comma_after(ast: &Ast, src: &str, pos: u32) -> Option<Span> {
    let _ = ast;
    let p = skip_trivia_forward(src, pos as usize);
    (src.as_bytes().get(p) == Some(&b',')).then(|| Span::new(p as u32, p as u32 + 1))
}

impl Rule for ConsistentTypeSpecifierStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ImportDecl {
            type_only,
            specifiers,
            source,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let specs = ast.list(specifiers);
        if specs.is_empty() || is_lone_binding(ast, specs) {
            return;
        }
        let span = cx.span(node);
        if self.prefer_inline {
            if !type_only {
                return;
            }
            // Drop the `type` after `import`, and mark each specifier.
            let kw = skip_trivia_forward(src, span.start as usize + "import".len()) as u32;
            let mut fix = Fix::replace(Span::new(kw, kw + "type".len() as u32), "");
            for &s in specs {
                fix = fix.and(Span::at(cx.span(s).start), "type ");
            }
            cx.report(
                span,
                "Prefer using inline type specifiers instead of a top-level type-only import.",
            )
            .fix(fix);
            return;
        }
        if type_only {
            return;
        }
        let mut types = Vec::new();
        let mut values = Vec::new();
        let mut default = None;
        for &s in specs {
            match *ast.node(s) {
                Node::ImportDefaultSpecifier { .. } => default = Some(s),
                Node::ImportSpecifier {
                    type_only: true, ..
                } => types.push(s),
                _ => values.push(s),
            }
        }
        if types.is_empty() {
            return;
        }
        let names: Vec<String> = types
            .iter()
            .map(|&s| {
                let Node::ImportSpecifier {
                    imported, local, ..
                } = *ast.node(s)
                else {
                    return String::new();
                };
                let local = ast.name_of(local).unwrap_or("");
                match imported.and_then(|i| ast.name_of(i)) {
                    Some(i) if i != local => format!("{i} as {local}"),
                    Some(i) => i.to_owned(),
                    None if imported.is_some() => format!("undefined as {local}"),
                    None => local.to_owned(),
                }
            })
            .collect();
        let new_import = format!(
            "import type {{{}}} from {};",
            names.join(", "),
            cx.text(source)
        );
        let message =
            "Prefer using a top-level type-only import instead of inline type specifiers.";
        if types.len() == specs.len() {
            cx.report(span, message).fix(Fix::replace(span, new_import));
            return;
        }
        let mut edits: Vec<(Span, String)> = Vec::new();
        if let Some(&last_value) = values.last() {
            for &t in &types {
                let ts = cx.span(t);
                if let Some(c) = comma_after(ast, src, ts.end) {
                    edits.push((c, String::new()));
                }
                edits.push((ts, String::new()));
            }
            if let Some(c) = comma_after(ast, src, cx.span(last_value).end) {
                edits.push((c, String::new()));
            }
        } else if let Some(d) = default {
            let comma = comma_after(ast, src, cx.span(d).end);
            let last = cx.span(*specs.last().unwrap_or(&d)).end as usize;
            let close = src[last..span.end as usize].find('}').map(|i| last + i + 1);
            if let (Some(comma), Some(close)) = (comma, close) {
                edits.push((Span::new(comma.start, close as u32), String::new()));
            }
        }
        edits.push((Span::at(span.end), format!("\n{new_import}")));
        edits.sort_by_key(|(s, _)| (s.start, s.end));
        edits.dedup_by_key(|(s, _)| *s);
        let mut fix: Option<Fix> = None;
        for (s, t) in edits {
            fix = Some(match fix {
                None => Fix::replace(s, t),
                Some(f) => f.and(s, t),
            });
        }
        for &t in &types {
            let ts = cx.span(t);
            cx.report(ts, message).fix_opt(fix.clone());
        }
    }
}
