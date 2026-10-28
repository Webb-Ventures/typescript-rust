//! `unicorn/prefer-identifier-import-export-specifiers`: a string literal that is a valid
//! identifier name, used as an imported name, an exported name, the local name of a
//! renaming re-export, the `export * as` name or an import attribute key, is written as
//! the identifier. The fix adds a space where the name would touch a word.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-identifier-import-export-specifiers",
        "Prefer identifiers over string literals in import and export specifiers.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferIdentifierImportExportSpecifiers>,
};

#[derive(Debug, Default)]
struct PreferIdentifierImportExportSpecifiers;

fn is_identifier_name(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c == '$' || c == '_' || ts_r_lexer::is_id_start(c))
        && it.all(|c| c == '$' || c == '_' || ts_r_lexer::is_id_part(c))
}

fn is_word_char(c: char) -> bool {
    c == '$' || c == '_' || c == '\\' || ts_r_lexer::is_id_part(c)
}

fn check(cx: &mut Ctx<'_, '_>, id: Option<NodeId>) {
    let Some(id) = id else {
        return;
    };
    let ast = cx.ast();
    let Node::StringLit { value } = *ast.node(id) else {
        return;
    };
    let name = ast.atom(value);
    if !is_identifier_name(name) {
        return;
    }
    let src = cx.src();
    let span = cx.span(id);
    let mut text = name.to_owned();
    if src[..span.start as usize]
        .chars()
        .next_back()
        .is_some_and(is_word_char)
    {
        text.insert(0, ' ');
    }
    if src[span.end as usize..]
        .chars()
        .next()
        .is_some_and(is_word_char)
    {
        text.push(' ');
    }
    let literal = span.text(src);
    cx.report(
        span,
        format!("Prefer identifier `{name}` over string literal `{literal}`."),
    )
    .id("prefer-identifier-import-export-specifiers")
    .fix(Fix::replace(span, text));
}

impl Rule for PreferIdentifierImportExportSpecifiers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportSpecifier,
            NodeKind::ExportSpecifier,
            NodeKind::ExportAll,
            NodeKind::ImportAttribute,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::ImportSpecifier { imported, .. } => check(cx, imported),
            Node::ExportSpecifier {
                local, exported, ..
            } => {
                let reexport = cx.parent(node).is_some_and(|p| {
                    matches!(
                        ast.node(p),
                        Node::ExportNamed {
                            source: Some(_),
                            ..
                        }
                    )
                });
                match exported {
                    Some(e) => {
                        check(cx, Some(e));
                        if reexport {
                            check(cx, Some(local));
                        }
                    }
                    None => check(cx, Some(local)),
                }
            }
            Node::ExportAll { exported, .. } => check(cx, exported),
            Node::ImportAttribute { key, .. } => check(cx, Some(key)),
            _ => {}
        }
    }
}
