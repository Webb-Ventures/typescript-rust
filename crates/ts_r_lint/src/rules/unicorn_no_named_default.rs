//! `unicorn/no-named-default`: `import {default as x} from "m"` and `export {x as default}`
//! (value imports and local exports only) are reported on the specifier.
//!
//! The import fix removes the specifier (with the `{...}` when it is the only named
//! import, or the following comma) and either adds `x` after `import` (`import x, {...}`)
//! or, when there is already a default import, inserts a separate `import x from "m"`
//! (with the original attributes) before the declaration. The export fix removes the
//! specifier (the whole declaration when it is alone) and inserts `export default x;`
//! before it. Neither fix runs when a comment would be removed. New lines use the file's
//! first line break.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-named-default",
        "Disallow named usage of default import and export.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoNamedDefault>,
};

#[derive(Debug, Default)]
struct NoNamedDefault;

fn linebreak(src: &str) -> &'static str {
    match src.find(['\n', '\r', '\u{2028}', '\u{2029}']) {
        Some(i) if src[i..].starts_with("\r\n") => "\r\n",
        Some(i) if src[i..].starts_with('\r') => "\r",
        Some(i) if src[i..].starts_with('\u{2028}') => "\u{2028}",
        Some(i) if src[i..].starts_with('\u{2029}') => "\u{2029}",
        _ => "\n",
    }
}

fn has_comment_between(ast: &ts_r_ast::Ast, a: u32, b: u32) -> bool {
    a < b && !ast.comments_in(Span::new(a, b)).is_empty()
}

impl Rule for NoNamedDefault {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportSpecifier, NodeKind::ExportSpecifier]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(decl) = cx.parent(node) else {
            return;
        };
        match *cx.node(node) {
            Node::ImportSpecifier {
                type_only: false,
                imported: Some(imported),
                local,
            } if ident_name(cx.ast(), imported) == Some("default") => {
                self.import(cx, node, decl, local);
            }
            Node::ExportSpecifier {
                type_only: false,
                local,
                exported: Some(exported),
            } if ident_name(cx.ast(), exported) == Some("default") => {
                self.export(cx, node, decl, local);
            }
            _ => {}
        }
    }
}

impl NoNamedDefault {
    fn import(&self, cx: &mut Ctx<'_, '_>, spec: NodeId, decl: NodeId, local: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ImportDecl {
            type_only: false,
            specifiers,
            source,
            ..
        } = *ast.node(decl)
        else {
            return;
        };
        let specs = ast.list(specifiers);
        let default = specs
            .iter()
            .copied()
            .find(|&s| matches!(ast.node(s), Node::ImportDefaultSpecifier { .. }));
        let only_named = specs
            .iter()
            .all(|&s| s == spec || !matches!(ast.node(s), Node::ImportSpecifier { .. }));
        let spec_span = cx.span(spec);
        let source_start = cx.span(source).start;
        let from_end = skip_trivia_backward(ast, src, source_start as usize) as u32;
        let from_start = from_end.saturating_sub(4);
        // The `,` after the default import, or the `{`.
        let start = match default {
            Some(d) => skip_trivia_forward(src, cx.span(d).end as usize) as u32,
            None => {
                (skip_trivia_backward(ast, src, spec_span.start as usize) as u32).saturating_sub(1)
            }
        };
        let commented = if only_named {
            has_comment_between(ast, start + 1, from_start)
        } else {
            !ast.comments_in(spec_span).is_empty()
        };
        let msg = "Prefer using the default import over named import.";
        let fix = (!commented).then(|| {
            let local_text = cx.span(local).text(src);
            let mut fix = if only_named {
                let glued = skip_trivia_backward(ast, src, start as usize) as u32 == start;
                Fix::replace(Span::new(start, from_start), if glued { " " } else { "" })
            } else {
                remove_with_comma(cx, spec_span)
            };
            let decl_span = cx.span(decl);
            if default.is_some() {
                let rest = Span::new(from_start, decl_span.end).text(src);
                fix = fix.and(
                    Span::at(decl_span.start),
                    format!("import {local_text} {rest}{}", linebreak(src)),
                );
            } else {
                let comma = specs
                    .iter()
                    .any(|&s| s != spec && matches!(ast.node(s), Node::ImportSpecifier { .. }));
                fix = fix.and(
                    Span::at(decl_span.start + "import".len() as u32),
                    format!(" {local_text}{}", if comma { "," } else { "" }),
                );
            }
            fix
        });
        cx.report(spec_span, msg)
            .id("no-named-default")
            .fix_opt(fix);
    }

    fn export(&self, cx: &mut Ctx<'_, '_>, spec: NodeId, decl: NodeId, local: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ExportNamed {
            type_only: false,
            specifiers,
            source: None,
            ..
        } = *ast.node(decl)
        else {
            return;
        };
        let alone = ast.list(specifiers).len() == 1;
        let decl_span = cx.span(decl);
        let spec_span = cx.span(spec);
        let removed = if alone { decl_span } else { spec_span };
        let commented = !ast.comments_in(removed).is_empty();
        let fix = (!commented).then(|| {
            let mut fix = if alone {
                let semi = cx.parent(decl).is_some_and(|_| {
                    prev_statement(ast, cx.file.parents, decl)
                        .is_some_and(|p| super::unicorn_ast::ends_without_semicolon(cx, p))
                });
                Fix::replace(decl_span, if semi { ";" } else { "" })
            } else {
                remove_with_comma(cx, spec_span)
            };
            fix = fix.and(
                Span::at(decl_span.start),
                format!(
                    "export default {};{}",
                    cx.span(local).text(src),
                    linebreak(src)
                ),
            );
            fix
        });
        cx.report(
            spec_span,
            "Prefer using the default export over named export.",
        )
        .id("no-named-default")
        .fix_opt(fix);
    }
}

/// Removes `span` and a `,` right after it (across whitespace and comments).
fn remove_with_comma(cx: &Ctx<'_, '_>, span: Span) -> Fix {
    let after = skip_trivia_forward(cx.src(), span.end as usize);
    let fix = Fix::delete(span);
    if cx.src().as_bytes().get(after) == Some(&b',') {
        fix.and(Span::new(after as u32, after as u32 + 1), "")
    } else {
        fix
    }
}
