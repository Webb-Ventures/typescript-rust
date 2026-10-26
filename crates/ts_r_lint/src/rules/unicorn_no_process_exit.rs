//! `unicorn/no-process-exit`: `process.exit()` outside CLI scripts. Files starting with a
//! hashbang, calls inside the arguments of a `process.on`-style listener registration, and
//! files that load `worker_threads` (`require()` or `import`) are left alone.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-process-exit",
        "Disallow `process.exit()`.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoProcessExit>,
};

#[derive(Debug, Default)]
struct NoProcessExit;

/// `process.<one of methods>(...)` with a plain, non-optional member and call.
fn is_process_call(ast: &Ast, call: NodeId, methods: &[&str], min_args: usize) -> bool {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return false;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return false;
    };
    ident_name(ast, obj) == Some("process")
        && ident_name(ast, prop).is_some_and(|p| methods.contains(&p))
        && ast.list(args).len() >= min_args
}

/// A string literal or expression-free template whose value is `worker_threads`.
fn is_worker_threads(ast: &Ast, id: NodeId) -> bool {
    let value = match *ast.node(id) {
        Node::StringLit { value } => ast.atom(value),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => ast.atom(cooked),
                _ => return false,
            },
            _ => return false,
        },
        _ => return false,
    };
    value == "worker_threads" || value == "node:worker_threads"
}

fn loads_worker_threads(ast: &Ast) -> bool {
    (1..ast.node_count() as u32)
        .filter_map(NodeId::from_raw)
        .filter(|&id| !ast.is_jsdoc(id))
        .any(|id| match *ast.node(id) {
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                ident_name(ast, callee) == Some("require")
                    && matches!(ast.list(args), [a] if is_worker_threads(ast, *a))
            }
            Node::ImportDecl { source, .. } => is_worker_threads(ast, source),
            _ => false,
        })
}

const LISTENERS: &[&str] = &[
    "on",
    "once",
    "addListener",
    "prependListener",
    "prependOnceListener",
];

impl Rule for NoProcessExit {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !is_process_call(ast, node, &["exit"], 0) || cx.src().starts_with("#!") {
            return;
        }
        let in_listener = ancestors_inclusive(cx.file.parents, node)
            .skip(1)
            .any(|a| is_process_call(ast, a, LISTENERS, 1));
        if in_listener || loads_worker_threads(ast) {
            return;
        }
        cx.report_node(
            node,
            "Only use `process.exit()` in CLI apps. Throw an error instead.",
        )
        .id("no-process-exit");
    }
}
