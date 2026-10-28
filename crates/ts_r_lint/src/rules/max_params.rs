//! `max-params`: limit the number of parameters of a function (and of a function type).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-params",
        "Enforce a maximum number of parameters in function definitions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxParams>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CountThis {
    Always,
    Never,
    #[default]
    ExceptVoid,
}

#[derive(Debug, Default)]
struct MaxParams {
    max: u64,
    count_this: CountThis,
}

impl FromOptions for MaxParams {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let count_this = match str_field(o, "countThis") {
            Some("always") => CountThis::Always,
            Some("never") => CountThis::Never,
            Some(_) => CountThis::ExceptVoid,
            None if bool_field(o, "countVoidThis", false) => CountThis::Always,
            None => CountThis::ExceptVoid,
        };
        Ok(MaxParams {
            max: max_option(opts, 3),
            count_this,
        })
    }
}

impl Rule for MaxParams {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
            NodeKind::FunctionType,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let params = match *ast.node(node) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. }
            | Node::FunctionType { params, .. } => ast.list(params),
            _ => return,
        };
        let mut count = params.len() as u64;
        if let Some(&first) = params.first()
            && let Node::Param {
                pattern, type_ann, ..
            } = *ast.node(first)
            && ident_name(ast, pattern) == Some("this")
        {
            let is_void = type_ann.is_some_and(|t| {
                matches!(
                    ast.node(t),
                    Node::KeywordType {
                        kind: KeywordTypeKind::Void
                    }
                )
            });
            let counted = match self.count_this {
                CountThis::Always => true,
                CountThis::Never => false,
                CountThis::ExceptVoid => !is_void,
            };
            if !counted {
                count -= 1;
            }
        }
        if count <= self.max {
            return;
        }
        let (span, name) = match *ast.node(node) {
            Node::FunctionType { return_type, .. } => {
                let rt = cx.span(return_type).start as usize;
                let Some(arrow) = cx.src().get(..rt).and_then(|s| s.rfind("=>")) else {
                    return;
                };
                let a = arrow as u32;
                (Span::new(a, a + 2), "Function".to_owned())
            }
            _ => {
                let parents = cx.file.parents;
                let mut name = function_name_with_kind(ast, cx.src(), parents, node);
                // ESLint names a function only by its own name or its key, never by the
                // variable it is assigned to.
                let anonymous = match *ast.node(node) {
                    Node::FunctionExpr { name, .. } => name.is_none(),
                    Node::Arrow { .. } => true,
                    _ => false,
                };
                if anonymous
                    && parents.parent(node).is_some_and(|p| {
                        matches!(
                            ast.node(p),
                            Node::VarDeclarator { .. } | Node::Assign { .. }
                        )
                    })
                    && let Some(i) = name.find(" '")
                {
                    name.truncate(i);
                }
                let mut chars = name.chars();
                let name = match chars.next() {
                    Some(c) => c.to_uppercase().chain(chars).collect(),
                    None => name,
                };
                let mut head = function_head_span(ast, cx.src(), parents, node);
                // A bodiless `declare function` starts at `declare`.
                if let Node::FunctionDecl { body: None, .. } = *ast.node(node) {
                    let src = cx.src();
                    let before = skip_trivia_backward(ast, src, head.start as usize);
                    if src.get(..before).is_some_and(|s| {
                        s.ends_with("declare") && !s[..s.len() - 7].ends_with(is_ident_char)
                    }) {
                        head = Span::new((before - 7) as u32, head.end);
                    }
                }
                (head, name)
            }
        };
        cx.report(
            span,
            format!(
                "{name} has too many parameters ({count}). Maximum allowed is {}.",
                self.max
            ),
        )
        .id("exceed");
    }
}
