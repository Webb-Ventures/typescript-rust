//! `@typescript-eslint/no-import-type-side-effects`: `import { type A, type B } from "x"`
//! leaves `import "x"` behind; use `import type { A, B } from "x"`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-import-type-side-effects",
        "Enforce the use of top-level import type qualifier when an import only has specifiers with inline type qualifiers",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoImportTypeSideEffects>,
};

#[derive(Debug, Default)]
struct NoImportTypeSideEffects;

impl Rule for NoImportTypeSideEffects {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ImportDecl {
            type_only: false,
            specifiers,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let specs = ast.list(specifiers);
        if specs.is_empty() {
            return;
        }
        let mut fix: Option<Fix> = None;
        for &s in specs {
            let Node::ImportSpecifier {
                type_only: true,
                imported,
                local,
            } = *ast.node(s)
            else {
                return;
            };
            // From `type` up to the imported name.
            let start = cx.span(s).start;
            let end = cx.span(imported.unwrap_or(local)).start;
            let range = Span::new(start, end);
            fix = Some(match fix {
                None => Fix::delete(range),
                Some(f) => f.and(range, ""),
            });
        }
        let after_import = cx.span(node).start + "import".len() as u32;
        let fix = fix.map(|f| f.and(Span::new(after_import, after_import), " type"));
        cx.report_node(
            node,
            "TypeScript will only remove the inline type specifiers which will leave behind a side effect import at runtime. Convert this to a top-level type qualifier to properly remove the entire import.",
        )
        .id("useTopLevelQualifier")
        .fix_opt(fix);
    }
}
