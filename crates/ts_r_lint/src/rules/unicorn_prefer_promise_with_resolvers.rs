//! `unicorn/prefer-promise-with-resolvers`: `new Promise(executor)` (the unshadowed global,
//! one argument) whose executor is a plain (not async, not generator) block-bodied function
//! taking one or two distinct identifier parameters, and whose body is nothing but
//! `outer = param;` statements, each copying a different resolver into a declared variable
//! that is not a parameter itself. The `new` expression is reported.
//!
//! The fix rewrites `let a; let b; const p = new Promise(...)` into
//! `const {promise: p, resolve: a, reject: b} = Promise.withResolvers<T>();` when: the promise
//! is the only declarator of a `const` statement in a statement list, binding a plain
//! identifier; the parameters carry no type annotations; the statements right before it are
//! initialiser-free, annotation-free `let`s declaring exactly the extracted variables (each
//! with one declarator or all of them); the extraction is the only write to each variable;
//! and no comment sits in the replaced range.

use super::prelude::*;
use super::unicorn_ast::would_remove_comments;
use super::unicorn_static::is_global_ident;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-promise-with-resolvers",
        "Prefer `Promise.withResolvers()` when extracting resolver functions from `new Promise()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferPromiseWithResolvers>,
};

#[derive(Debug, Default)]
struct PreferPromiseWithResolvers;

const RESOLVERS: [&str; 2] = ["resolve", "reject"];

/// One `outer = param;` statement in the executor.
struct Extraction {
    /// Index into [`RESOLVERS`].
    property: usize,
    /// The assigned identifier.
    target: NodeId,
    symbol: SymbolId,
}

impl Rule for PreferPromiseWithResolvers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New {
            callee,
            has_args: true,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let &[executor] = ast.list(args) else {
            return;
        };
        if !is_global_ident(cx, callee, "Promise") {
            return;
        }
        let (params, body) = match *ast.node(executor) {
            Node::FunctionExpr {
                flags,
                params,
                body: Some(body),
                ..
            }
            | Node::Arrow {
                flags,
                params,
                body,
                ..
            } if !flags
                .intersects(FnFlags::ASYNC | FnFlags::GENERATOR | FnFlags::EXPRESSION_BODY) =>
            {
                (params, body)
            }
            _ => return,
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let Some(extractions) = extractions(cx, ast.list(params), ast.list(stmts)) else {
            return;
        };
        let fix = fix(cx, node, ast.list(params), &extractions);
        cx.report_node(
            node,
            "Prefer `Promise.withResolvers()` over extracting resolver functions from `new Promise()`.",
        )
        .id("prefer-promise-with-resolvers")
        .fix_opt(fix);
    }
}

/// The parameter's identifier: a plain (no default, not rest) identifier parameter.
fn param_ident(ast: &ts_r_ast::Ast, param: NodeId) -> Option<NodeId> {
    match *ast.node(param) {
        Node::Param {
            rest: false,
            pattern,
            init: None,
            ..
        } if matches!(ast.node(pattern), Node::Ident { .. }) => Some(pattern),
        _ => None,
    }
}

fn extractions(cx: &Ctx<'_, '_>, params: &[NodeId], stmts: &[NodeId]) -> Option<Vec<Extraction>> {
    let ast = cx.ast();
    let sem = cx.semantic();
    if params.is_empty() || params.len() > 2 {
        return None;
    }
    let mut names = Vec::with_capacity(2);
    let mut param_symbols = Vec::with_capacity(2);
    for &p in params {
        let id = param_ident(ast, p)?;
        let name = ident_name(ast, id)?;
        if names.contains(&name) {
            return None;
        }
        names.push(name);
        param_symbols.push(sem.symbol_of_decl(id));
    }
    let mut out: Vec<Extraction> = Vec::new();
    for &stmt in stmts {
        let Node::ExprStmt { expr } = *ast.node(stmt) else {
            return None;
        };
        let Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } = *ast.node(expr)
        else {
            return None;
        };
        let (Some(_), Some(right_name)) = (ident_name(ast, left), ident_name(ast, right)) else {
            return None;
        };
        let property = names.iter().position(|&n| n == right_name)?;
        if out.iter().any(|e| e.property == property) {
            return None;
        }
        let symbol = sem
            .reference_at(left)
            .and_then(|r| sem.reference(r).symbol())?;
        if param_symbols.contains(&Some(symbol)) {
            return None;
        }
        out.push(Extraction {
            property,
            target: left,
            symbol,
        });
    }
    (!out.is_empty()).then_some(out)
}

fn fix(
    cx: &Ctx<'_, '_>,
    new: NodeId,
    params: &[NodeId],
    extractions: &[Extraction],
) -> Option<Fix> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let declarator = cx.parent(new)?;
    let Node::VarDeclarator {
        name,
        type_ann: None,
        init: Some(init),
        ..
    } = *ast.node(declarator)
    else {
        return None;
    };
    if init != new {
        return None;
    }
    let promise_name = ident_name(ast, name)?;
    let declaration = cx.parent(declarator)?;
    let Node::VarDecl {
        modifiers,
        kind: VarKind::Const,
        decls,
    } = *ast.node(declaration)
    else {
        return None;
    };
    // `export const` and `declare const` are not plain statements of the list.
    if !modifiers.is_empty() || ast.list(decls).len() != 1 {
        return None;
    }
    if params.iter().any(|&p| {
        matches!(
            *ast.node(p),
            Node::Param {
                type_ann: Some(_),
                ..
            }
        )
    }) {
        return None;
    }
    let container = cx.parent(declaration)?;
    let siblings = match *ast.node(container) {
        Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => {
            ast.list(stmts)
        }
        Node::StaticBlock { body } => ast.list(body),
        _ => return None,
    };
    let at = siblings.iter().position(|&s| s == declaration)?;

    // The `let` statements right before the promise, nearest first.
    let target_names: Vec<&str> = extractions
        .iter()
        .filter_map(|e| ident_name(ast, e.target))
        .collect();
    let mut first = None;
    let mut declared: Vec<SymbolId> = Vec::new();
    let mut decl_stmts: Vec<NodeId> = Vec::new();
    for &stmt in siblings[..at].iter().rev() {
        let Node::VarDecl {
            modifiers,
            kind: VarKind::Let,
            decls,
        } = *ast.node(stmt)
        else {
            break;
        };
        if !modifiers.is_empty() {
            break;
        }
        let list = ast.list(decls);
        let plain = !list.is_empty()
            && list.iter().all(|&d| match *ast.node(d) {
                Node::VarDeclarator {
                    name,
                    type_ann: None,
                    init: None,
                    ..
                } => ident_name(ast, name).is_some_and(|n| target_names.contains(&n)),
                _ => false,
            });
        if !plain {
            break;
        }
        decl_stmts.push(stmt);
        for &d in list {
            if let Node::VarDeclarator { name, .. } = *ast.node(d)
                && let Some(s) = sem.symbol_of_decl(name)
                && !declared.contains(&s)
            {
                declared.push(s);
            }
        }
        if declared.len() == extractions.len() {
            let shapes_ok = decl_stmts.iter().all(|&s| match *ast.node(s) {
                Node::VarDecl { decls, .. } => {
                    let n = ast.list(decls).len();
                    n == 1 || n == extractions.len()
                }
                _ => false,
            });
            if !shapes_ok || !extractions.iter().all(|e| declared.contains(&e.symbol)) {
                return None;
            }
            first = Some(stmt);
            break;
        }
    }
    let first = first?;

    // The extraction is the only write to each variable.
    for e in extractions {
        let only = sem.symbol(e.symbol).refs.iter().all(|&r| {
            let r = sem.reference(r);
            !r.is_write() || r.node == e.target
        });
        if !only {
            return None;
        }
    }

    let range = Span::new(cx.span(first).start, cx.span(declaration).end);
    if would_remove_comments(ast, range, &[]) {
        return None;
    }

    let binding = |property: &str, local: &str| {
        if property == local {
            property.to_owned()
        } else {
            format!("{property}: {local}")
        }
    };
    let mut bindings = vec![binding("promise", promise_name)];
    for (i, property) in RESOLVERS.iter().enumerate() {
        for e in extractions.iter().filter(|e| e.property == i) {
            bindings.push(binding(property, ident_name(ast, e.target)?));
        }
    }
    let type_args = match *ast.node(new) {
        Node::New {
            type_args: Some(t), ..
        } => cx.text(t).to_owned(),
        _ => String::new(),
    };
    Some(Fix::replace(
        range,
        format!(
            "const {{{}}} = Promise.withResolvers{type_args}();",
            bindings.join(", ")
        ),
    ))
}
