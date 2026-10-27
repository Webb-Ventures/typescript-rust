//! `strict`: where `'use strict'` directives go (`safe`, the default, picks `global` for
//! CommonJS and `function` otherwise; `never`; and none needed in modules, classes or with
//! implied strict mode).

use super::func_names::own_name_with_kind;
use super::prelude::*;
use crate::env::SourceType;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "strict",
        "Require or disallow strict mode directives",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create::<Strict>,
};

#[derive(Debug, Default)]
struct Strict {
    mode: Option<String>,
}

impl FromOptions for Strict {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(Strict {
            mode: opts.first().and_then(Value::as_str).map(str::to_owned),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Never,
    Global,
    Function,
    Module,
    Implied,
}

/// The leading `'use strict'` statements of a statement list.
fn use_strict(ast: &Ast, stmts: &[NodeId]) -> Vec<NodeId> {
    stmts
        .iter()
        .copied()
        .take_while(|&s| match *ast.node(s) {
            Node::Directive { value } => ast.atom(value) == "use strict",
            Node::ExprStmt { expr } => matches!(
                *ast.node(skip_parens(ast, expr)),
                Node::StringLit { value } if ast.atom(value) == "use strict"
            ),
            _ => false,
        })
        .collect()
}

fn simple_params(ast: &Ast, f: NodeId) -> bool {
    let params = match *ast.node(f) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. } => params,
        _ => return true,
    };
    ast.list(params).iter().all(|&p| match *ast.node(p) {
        Node::Param {
            pattern,
            init: None,
            rest: false,
            ..
        } => matches!(ast.node(pattern), Node::Ident { .. }),
        Node::Ident { .. } => true,
        _ => false,
    })
}

fn is_function(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { body: Some(_), .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

const MSG_FUNCTION: &str = "Use the function form of 'use strict'.";

fn message(id: &str) -> &'static str {
    match id {
        "global" => "Use the global form of 'use strict'.",
        "multiple" => "Multiple 'use strict' directives.",
        "never" => "Strict mode is not permitted.",
        "unnecessary" => "Unnecessary 'use strict' directive.",
        "module" => "'use strict' is unnecessary inside of modules.",
        "implied" => "'use strict' is unnecessary when implied strict mode is enabled.",
        "function" => MSG_FUNCTION,
        "unnecessaryInClasses" => "'use strict' is unnecessary inside of classes.",
        _ => {
            "'use strict' directive inside a function with non-simple parameter list throws a \
             syntax error since ES2016."
        }
    }
}

fn report(cx: &mut Ctx<'_, '_>, nodes: &[NodeId], id: &'static str, fix: bool) {
    for &n in nodes {
        let span = cx.span(n);
        cx.report(span, message(id))
            .id(id)
            .fix_opt(fix.then(|| Fix::delete(span)));
    }
}

fn mode_id(mode: Mode) -> &'static str {
    match mode {
        Mode::Never => "never",
        Mode::Global => "global",
        Mode::Function => "function",
        Mode::Module => "module",
        Mode::Implied => "implied",
    }
}

fn fixes(mode: Mode) -> bool {
    matches!(mode, Mode::Module | Mode::Implied)
}

impl Rule for Strict {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let env = cx.env();
        let mut mode = match self.mode.as_deref() {
            _ if env.implied_strict => Mode::Implied,
            Some("never") => Mode::Never,
            Some("global") => Mode::Global,
            Some("function") => Mode::Function,
            _ if env.global_return || env.source_type == SourceType::CommonJs => Mode::Global,
            _ => Mode::Function,
        };
        if env.source_type == SourceType::Module {
            mode = Mode::Module;
        }
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let stmts = ast.list(stmts);
        let top = use_strict(ast, stmts);
        if mode == Mode::Global {
            if !stmts.is_empty() && top.is_empty() {
                let span = Span::new(cx.span(stmts[0]).start, cx.span(stmts[stmts.len() - 1]).end);
                cx.report(span, message("global")).id("global");
            }
            report(cx, top.get(1..).unwrap_or(&[]), "multiple", true);
        } else {
            report(cx, &top, mode_id(mode), fixes(mode));
        }
        // (node, parent strictness: None at the top level, inside a class body).
        let mut stack: Vec<(NodeId, Option<bool>, bool)> = vec![(ast.root, None, false)];
        while let Some((n, parent, in_class)) = stack.pop() {
            let node = ast.node(n);
            let mut child_parent = parent;
            let child_class = in_class;
            if is_function(node) {
                let body = match *node {
                    Node::FunctionDecl { body: Some(b), .. }
                    | Node::FunctionExpr { body: Some(b), .. } => Some(b),
                    Node::Arrow { body, .. } => Some(body),
                    _ => None,
                };
                let dirs = match body.map(|b| ast.node(b)) {
                    Some(&Node::Block { stmts }) => use_strict(ast, ast.list(stmts)),
                    _ => Vec::new(),
                };
                let simple = simple_params(ast, n);
                if mode == Mode::Function {
                    let parent_strict = parent == Some(true);
                    if let Some(&first) = dirs.first() {
                        if !simple {
                            report(cx, &[first], "nonSimpleParameterList", false);
                        } else if parent_strict {
                            report(cx, &[first], "unnecessary", true);
                        } else if in_class {
                            report(cx, &[first], "unnecessaryInClasses", true);
                        }
                        report(cx, &dirs[1..], "multiple", true);
                    } else if parent.is_none() && !in_class {
                        let mut span = cx.span(n);
                        if let Node::FunctionDecl { modifiers, .. } = *node
                            && modifiers.contains(Modifiers::EXPORT)
                            && cx.text(n).starts_with("export")
                        {
                            span.start =
                                skip_trivia_forward(cx.src(), span.start as usize + 6) as u32;
                        }
                        if simple {
                            cx.report(span, MSG_FUNCTION).id("function");
                        } else {
                            let name = own_name_with_kind(cx, n);
                            cx.report(
                                span,
                                format!("Wrap {name} in a function with 'use strict' directive."),
                            )
                            .id("wrap");
                        }
                    }
                    child_parent = Some(parent_strict || !dirs.is_empty());
                } else if !dirs.is_empty() {
                    if simple {
                        report(cx, &dirs, mode_id(mode), fixes(mode));
                    } else {
                        report(cx, &dirs[..1], "nonSimpleParameterList", false);
                        report(cx, &dirs[1..], "multiple", true);
                    }
                }
            }
            // A class's members are in its body; its name, decorators and heritage aren't.
            let outside: Option<(&[NodeId], &[NodeId])> = match *node {
                Node::ClassDecl {
                    heritage,
                    decorators,
                    ..
                }
                | Node::ClassExpr {
                    heritage,
                    decorators,
                    ..
                } => Some((ast.list(heritage), ast.list(decorators))),
                _ => None,
            };
            let name = match *node {
                Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
                _ => None,
            };
            ast.for_each_child(n, |c| {
                let cls = child_class
                    || outside.is_some_and(|(h, d)| {
                        !h.contains(&c) && !d.contains(&c) && name != Some(c)
                    });
                stack.push((c, child_parent, cls));
            });
        }
    }
}
