//! `@typescript-eslint/consistent-type-definitions`: object types are declared one way,
//! as interfaces (`interface`, the default) or as type aliases (`type`).
//!
//! - `interface`: a type alias whose type is an object literal type (through any parens)
//!   reports at its name. The fix swaps `type` for `interface`, replaces everything from
//!   the end of what precedes `=` (comments kept) to the `{` with one space, and drops what
//!   follows the `}` up to the end of the alias (closing parens, `;`).
//! - `type`: every interface reports at its name. The fix swaps `interface` for `type`,
//!   replaces what lies between the name (or type parameters) and the body with ` = `
//!   (dropping `extends`), appends ` & Base` per base after the body, and turns
//!   `export default interface X` into `type X` plus `export default X` on its own line.
//!   No fix inside `declare global`, where a type alias would not merge with the global.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-type-definitions",
        "Enforce type definitions to consistently use either `interface` or `type`",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentTypeDefinitions>,
};

#[derive(Debug, Default)]
struct ConsistentTypeDefinitions {
    prefer_type: bool,
}

impl FromOptions for ConsistentTypeDefinitions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first().and_then(Value::as_str) {
            None | Some("interface") => Ok(Self { prefer_type: false }),
            Some("type") => Ok(Self { prefer_type: true }),
            Some(o) => Err(format!("consistent-type-definitions: unknown style {o:?}")),
        }
    }
}

/// The keyword ending right before `pos` (skipping trivia), when it is `kw`.
fn keyword_before(cx: &Ctx<'_, '_>, pos: u32, kw: &str) -> Option<Span> {
    let end = skip_trivia_backward(cx.ast(), cx.src(), pos as usize);
    let start = end.checked_sub(kw.len())?;
    (cx.src().get(start..end) == Some(kw)).then(|| Span::new(start as u32, end as u32))
}

impl ConsistentTypeDefinitions {
    fn check_alias(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::TypeAlias { name, ty, .. } = *ast.node(node) else {
            return;
        };
        let mut lit = ty;
        while let Node::ParenType { ty } = *ast.node(lit) {
            lit = ty;
        }
        if !matches!(ast.node(lit), Node::TypeLiteral { .. }) {
            return;
        }
        let name_span = cx.span(name);
        let lit_span = cx.span(lit);
        let fix = (|| {
            let kw = keyword_before(cx, name_span.start, "type")?;
            // Back from the `{` over `(`s to the `=`.
            let b = src.as_bytes();
            let mut pos = lit_span.start as usize;
            let eq = loop {
                pos = skip_trivia_backward(ast, src, pos);
                match pos.checked_sub(1).map(|p| b[p]) {
                    Some(b'(') => pos -= 1,
                    Some(b'=') => break pos - 1,
                    _ => return None,
                }
            };
            let before_eq = src[..eq]
                .trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
                .len();
            let end = cx.span(node).end.max(lit_span.end);
            Some(
                Fix::replace(kw, "interface")
                    .and(Span::new(before_eq as u32, lit_span.start), " ")
                    .and(Span::new(lit_span.end, end), ""),
            )
        })();
        cx.report(name_span, "Use an `interface` instead of a `type`.")
            .id("interfaceOverType")
            .fix_opt(fix);
    }

    fn check_interface(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Interface {
            modifiers,
            name,
            type_params,
            extends,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let name_span = cx.span(name);
        let in_declare_global = {
            let mut cur = node;
            let mut found = false;
            while let Some(p) = cx.parent(cur) {
                if let Node::Module {
                    modifiers,
                    kind: ModuleKind::Global,
                    ..
                } = *ast.node(p)
                    && modifiers.contains(Modifiers::DECLARE)
                {
                    found = true;
                    break;
                }
                cur = p;
            }
            found
        };
        let fix = (!in_declare_global)
            .then(|| {
                let kw = keyword_before(cx, name_span.start, "interface")?;
                let head_end = type_params.map_or(name_span.end, |t| cx.span(t).end);
                let after_heads = ast
                    .list(extends)
                    .last()
                    .map_or(head_end, |&e| cx.span(e).end);
                let body_start = find_char(src, after_heads, b'{')?;
                let body_end = cx.span(node).end;
                let mut fix = Fix::replace(kw, "type").and(Span::new(head_end, body_start), " = ");
                let at = Span::new(body_end, body_end);
                for &e in ast.list(extends) {
                    fix = fix.and(at, format!(" & {}", cx.text(e)));
                }
                if modifiers.contains(Modifiers::DEFAULT) {
                    let start = cx.span(node).start;
                    fix = fix
                        .and(Span::new(start, kw.start), "")
                        .and(at, format!("\nexport default {}", cx.text(name)));
                }
                Some(fix)
            })
            .flatten();
        cx.report(name_span, "Use a `type` instead of an `interface`.")
            .id("typeOverInterface")
            .fix_opt(fix);
    }
}

impl Rule for ConsistentTypeDefinitions {
    fn kinds(&self) -> &'static [NodeKind] {
        if self.prefer_type {
            &[NodeKind::Interface]
        } else {
            &[NodeKind::TypeAlias]
        }
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.prefer_type {
            self.check_interface(node, cx);
        } else {
            self.check_alias(node, cx);
        }
    }
}
