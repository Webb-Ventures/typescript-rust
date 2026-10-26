//! `import/first`: imports (`import` declarations and `import x = ...`) after other
//! statements of the module (directives at the top don't count). With
//! `"absolute-first"`, an absolute import after a relative one is reported too.
//!
//! The fix moves the misplaced imports up, after the last import that is in place (or
//! before the first statement), as the plugin does: every import up to the last one that
//! no earlier code references moves in one edit; the ones before it get an empty fix, the
//! ones after it none.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/first",
        "Ensure all imports appear before other statements.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<First>,
};

/// `import/imports-first`: the deprecated name of `import/first`, same behaviour.
pub static IMPORTS_FIRST: RuleDef = RuleDef {
    meta: meta(
        "import/imports-first",
        "Replaced by `import/first`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<First>,
};

#[derive(Debug)]
struct First {
    absolute_first: bool,
}

impl FromOptions for First {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(First {
            absolute_first: opts.first().and_then(Value::as_str) == Some("absolute-first"),
        })
    }
}

const MESSAGE: &str = "Import in body of module; reorder to top.";

fn is_directive_like(ast: &Ast, stmt: NodeId) -> bool {
    match *ast.node(stmt) {
        Node::Directive { .. } => true,
        Node::ExprStmt { expr } => ast.kind(skip_parens(ast, expr)) == NodeKind::StringLit,
        _ => false,
    }
}

/// The binding identifiers an import declares.
fn import_bindings(ast: &Ast, stmt: NodeId) -> Vec<NodeId> {
    match *ast.node(stmt) {
        Node::ImportDecl { specifiers, .. } => ast
            .list(specifiers)
            .iter()
            .filter_map(|&s| match *ast.node(s) {
                Node::ImportDefaultSpecifier { local }
                | Node::ImportNamespaceSpecifier { local }
                | Node::ImportSpecifier { local, .. } => Some(local),
                _ => None,
            })
            .collect(),
        Node::ImportEquals { name, .. } => vec![name],
        _ => Vec::new(),
    }
}

/// The module specifier of an import (`None` for `import x = A.B`).
fn import_value(ast: &Ast, stmt: NodeId) -> Option<(NodeId, &str)> {
    match *ast.node(stmt) {
        Node::ImportDecl { source, .. } => {
            super::import_util::string_literal(ast, source).map(|v| (source, v))
        }
        Node::ImportEquals { module_ref, .. } => match *ast.node(module_ref) {
            Node::ExternalModuleRef { expr } => {
                super::import_util::string_literal(ast, expr).map(|v| (module_ref, v))
            }
            _ => None,
        },
        _ => None,
    }
}

struct ErrorInfo {
    node: NodeId,
    range: Span,
}

impl Rule for First {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, program: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Program { stmts } = *cx.node(program) else {
            return;
        };
        let body = ast.list(stmts);
        let mut non_imports = 0;
        let mut any_expressions = false;
        let mut any_relative = false;
        let mut last_legal = None;
        let mut errors: Vec<ErrorInfo> = Vec::new();
        let mut should_sort = true;
        let mut last_sort = 0;
        let mut absolute_reports = Vec::new();
        for (index, &node) in body.iter().enumerate() {
            if !any_expressions && is_directive_like(ast, node) {
                continue;
            }
            any_expressions = true;
            if !matches!(
                ast.kind(node),
                NodeKind::ImportDecl | NodeKind::ImportEquals
            ) {
                non_imports += 1;
                continue;
            }
            if self.absolute_first
                && let Some((at, value)) = import_value(ast, node)
            {
                if value.starts_with('.') {
                    any_relative = true;
                } else if any_relative {
                    absolute_reports.push(at);
                }
            }
            if non_imports == 0 {
                last_legal = Some(node);
                continue;
            }
            let end = cx.span(node).end;
            let sem = cx.semantic();
            for b in import_bindings(ast, node) {
                if !should_sort {
                    break;
                }
                let Some(sym) = sem.symbol_of_decl(b) else {
                    continue;
                };
                if sem
                    .symbol(sym)
                    .refs
                    .iter()
                    .any(|&r| cx.span(sem.reference(r).node).start < end)
                {
                    should_sort = false;
                }
            }
            if should_sort {
                last_sort = errors.len();
            }
            errors.push(ErrorInfo {
                node,
                range: Span::new(cx.span(body[index - 1]).end, end),
            });
        }
        for at in absolute_reports {
            cx.report_node(at, "Absolute imports should come before relative imports.");
        }
        if errors.is_empty() {
            return;
        }
        let big_fix = {
            let sorted = &errors[..=last_sort];
            let mut insert: String = sorted
                .iter()
                .map(|e| {
                    let text = e.range.text(src);
                    if text.chars().next().is_some_and(|c| !c.is_whitespace()) {
                        format!("\n{text}")
                    } else {
                        text.to_owned()
                    }
                })
                .collect();
            if last_legal.is_none() {
                let lead_len = insert.len() - insert.trim_start().len();
                let lead = insert[..lead_len].to_owned();
                insert = format!("{}{lead}", insert.trim());
            }
            let insert_at = match last_legal {
                Some(l) => cx.span(l).end,
                None => cx.span(body[0]).start,
            };
            // Pieces in order: the insertion, then each removal.
            let mut edits = vec![(Span::at(insert_at), insert)];
            edits.extend(sorted.iter().map(|e| (e.range, String::new())));
            let mut out = String::new();
            let mut prev = 0u32;
            for (span, text) in &edits {
                if span.start >= prev {
                    out.push_str(&src[prev as usize..span.start as usize]);
                }
                out.push_str(text);
                prev = span.end;
            }
            let end = sorted.last().map_or(0, |e| e.range.end);
            Fix::replace(Span::new(0, end), out)
        };
        let mut big_fix = Some(big_fix);
        for (index, e) in errors.iter().enumerate() {
            let span = cx.span(e.node);
            let fix = match index.cmp(&last_sort) {
                std::cmp::Ordering::Less => Some(Fix::insert(span.end, String::new())),
                std::cmp::Ordering::Equal => big_fix.take(),
                std::cmp::Ordering::Greater => None,
            };
            cx.report(span, MESSAGE).fix_opt(fix);
        }
    }
}
