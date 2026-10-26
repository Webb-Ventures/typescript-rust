//! `unicorn/require-module-attributes`: no empty attribute list. An import or re-export
//! with a source followed by `with {}` is reported from `{` to `}`, fixed (unless a comment
//! sits between `with` and `}`) by deleting the three tokens. In `import(x, options)` with
//! an object literal as options, the options object (when empty, or holding only an empty
//! `with: {}`) or the `with: {}` property is the target; the reported node is the empty
//! `with` value or the options object. Without comments inside the target, the fix deletes
//! the options with the comma before and its parentheses, or the property's tokens with
//! the comma after it (or, for the last of several properties, the comma before).

use super::prelude::*;
use super::unicorn_util::parenthesized_span;
use super::unicorn_util::{delete_all, token_runs};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-module-attributes",
        "Require non-empty module attributes for imports and exports",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<RequireModuleAttributes>,
};

#[derive(Debug, Default)]
struct RequireModuleAttributes;

fn is_ident_part(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphanumeric()
}

impl RequireModuleAttributes {
    fn declaration(
        &self,
        cx: &mut Ctx<'_, '_>,
        source: NodeId,
        attributes: Option<NodeId>,
        what: &str,
    ) {
        let ast = cx.ast();
        if let Some(a) = attributes
            && let Node::ImportAttributes { entries, .. } = *ast.node(a)
            && !ast.list(entries).is_empty()
        {
            return;
        }
        let src = cx.src();
        let with = skip_trivia_forward(src, cx.span(source).end as usize);
        if !src[with..].starts_with("with")
            || src[with + 4..].chars().next().is_some_and(is_ident_part)
        {
            return;
        }
        let open = skip_trivia_forward(src, with + 4);
        if !src[open..].starts_with('{') {
            return;
        }
        let close = skip_trivia_forward(src, open + 1);
        if !src[close..].starts_with('}') {
            return;
        }
        let (with, open, close) = (with as u32, open as u32, close as u32);
        let fix = ast
            .comments_in(Span::new(with, close))
            .is_empty()
            .then(|| {
                delete_all([
                    Span::new(with, with + 4),
                    Span::new(open, open + 1),
                    Span::new(close, close + 1),
                ])
            })
            .flatten();
        cx.report(
            Span::new(open, close + 1),
            format!("{what} statement with empty attribute list is not allowed."),
        )
        .id("require-module-attributes")
        .fix_opt(fix);
    }

    fn import_call(&self, cx: &mut Ctx<'_, '_>, options: NodeId) {
        let ast = cx.ast();
        let options = skip_parens(ast, options);
        let Node::ObjectLit { props } = *ast.node(options) else {
            return;
        };
        let props = ast.list(props);
        let empty_with = props.iter().copied().find_map(|p| {
            let Node::Property { key, value, .. } = *ast.node(p) else {
                return None;
            };
            let named_with = match *ast.node(key) {
                Node::Ident { name } | Node::StringLit { value: name } => ast.atom(name) == "with",
                _ => false,
            };
            let value = skip_parens(ast, value);
            let empty =
                matches!(*ast.node(value), Node::ObjectLit { props } if ast.list(props).is_empty());
            (named_with && empty).then_some((p, value))
        });
        let (target, is_prop) = if props.is_empty() || (empty_with.is_some() && props.len() == 1) {
            (options, false)
        } else if let Some((p, _)) = empty_with {
            (p, true)
        } else {
            return;
        };
        let reported = empty_with.map_or(target, |(_, v)| v);
        let src = cx.src();
        let tspan = cx.span(target);
        let fix = if !ast.comments_in(tspan).is_empty() {
            None
        } else if is_prop {
            let mut spans = token_runs(ast, src, tspan);
            let after = skip_trivia_forward(src, tspan.end as usize);
            if src[after..].starts_with(',') {
                spans.push(Span::new(after as u32, after as u32 + 1));
            } else if props.len() > 1 && props.last() == Some(&target) {
                let before = skip_trivia_backward(ast, src, tspan.start as usize);
                if src[..before].ends_with(',') {
                    spans.push(Span::new(before as u32 - 1, before as u32));
                }
            }
            delete_all(spans)
        } else {
            let outer = parenthesized_span(cx, target);
            let mut spans = Vec::new();
            // The comma before the outermost parenthesis.
            let before = skip_trivia_backward(ast, src, outer.start as usize);
            if src[..before].ends_with(',') {
                spans.push(Span::new(before as u32 - 1, before as u32));
            }
            spans.extend(token_runs(ast, src, tspan));
            for region in [
                Span::new(outer.start, tspan.start),
                Span::new(tspan.end, outer.end),
            ] {
                spans.extend(
                    token_runs(ast, src, region)
                        .into_iter()
                        .flat_map(|r| (r.start..r.end).map(|p| Span::new(p, p + 1)))
                        .filter(|s| matches!(&src[s.start as usize..s.end as usize], "(" | ")")),
                );
            }
            spans.sort_by_key(|s| s.start);
            delete_all(spans)
        };
        cx.report_node(
            reported,
            "import expression with empty attribute list is not allowed.",
        )
        .id("require-module-attributes")
        .fix_opt(fix);
    }
}

impl Rule for RequireModuleAttributes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportDecl,
            NodeKind::ExportNamed,
            NodeKind::ExportAll,
            NodeKind::ImportCall,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::ImportDecl {
                source, attributes, ..
            } => self.declaration(cx, source, attributes, "import"),
            Node::ExportNamed {
                source: Some(source),
                attributes,
                ..
            }
            | Node::ExportAll {
                source, attributes, ..
            } => self.declaration(cx, source, attributes, "export"),
            Node::ImportCall {
                options: Some(options),
                ..
            } => self.import_call(cx, options),
            _ => {}
        }
    }
}
