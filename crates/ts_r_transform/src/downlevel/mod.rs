//! Lowering for targets below ES2022 (design record 0012, E5).
//!
//! tsc 7 emits ES2015 and later. Syntax newer than the target is rewritten by one pass per
//! feature year, newest first, after the TypeScript transforms ([`crate::transform`]) on the
//! same tree. Until a construct's lowering is implemented, `unlowered` finds it in the output and the
//! program emitter refuses the file (TSR2003) instead of writing unlowered output.
//!
//! ES5 output is ES2015 output (tsc 7 reports TS5108 and lowers like ES2015).

use ts_r_ast::{
    AssignOp, Ast, BinaryOp, FnFlags, Modifiers, Node, NodeId, NodeKind, Span, VarKind,
};
use ts_r_config::ScriptTarget;

use crate::options::TransformOptions;

mod es2017;
mod es2018;
mod es2020;
mod es2022;
pub(crate) mod lower;
mod operators;
mod private;
pub(crate) use private::compound_op;
mod rest;
mod template;
pub(crate) mod using;

/// The lowest target ts-r writes output for (ES5 is lowered like ES2015, as tsc 7 does).
pub const MIN_TARGET: ScriptTarget = ScriptTarget::ES5;

/// True if output for `target` can be written (constructs that still need an unimplemented
/// lowering are refused per file by [`unsupported`]).
pub fn is_supported(target: ScriptTarget) -> bool {
    target >= MIN_TARGET
}

/// The first construct in the transformed tree `ast` (parsed from `src`) that is newer than
/// `opts.target`, with what it is: syntax whose lowering is not implemented. A program
/// emitter must not write such a file (TSR2003). Synthesized syntax counts too (a JSX spread
/// becomes an object spread, static initializers static blocks).
pub(crate) fn unlowered(
    ast: &Ast,
    src: &str,
    opts: &TransformOptions,
) -> Option<(Span, &'static str)> {
    let t = opts.target;
    if t >= ScriptTarget::ES2022 {
        return None;
    }
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        let n = *ast.node(id);
        if crate::is_type_kind(n.kind()) || n.modifiers().contains(Modifiers::DECLARE) {
            continue;
        }
        match n {
            Node::Interface { .. } | Node::TypeAlias { .. } => continue,
            Node::FunctionDecl { body: None, .. } => continue,
            _ => {}
        }
        if let Some(what) = construct(ast, src, id, &n, opts) {
            return Some((ast.span(id), what));
        }
        // `async` on a constructor or accessor is an error tsc leaves alone.
        if let Node::MethodDef {
            kind, key, value, ..
        } = n
            && kind != ts_r_ast::MethodKind::Method
        {
            stack.push(key);
            if let Node::FunctionExpr { params, body, .. } = *ast.node(value) {
                stack.extend(ast.list(params).iter().copied());
                stack.extend(body);
            }
            continue;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    None
}

/// What lowering node `n` needs for `opts.target` that is not implemented yet.
fn construct(
    ast: &Ast,
    src: &str,
    id: NodeId,
    n: &Node,
    opts: &TransformOptions,
) -> Option<&'static str> {
    let t = opts.target;
    let below = |year: ScriptTarget| t < year;
    match *n {
        // ES2022: classes.
        Node::PrivateName { .. } => Some("private name"),
        Node::StaticBlock { .. } => Some("class static block"),
        Node::PropertyDef { .. } => Some("class field"),
        // ES2021.
        Node::Assign {
            op: AssignOp::OrAssign | AssignOp::AndAssign | AssignOp::CoalesceAssign,
            ..
        } if below(ScriptTarget::ES2021) => Some("logical assignment"),
        Node::NumberLit { .. } | Node::BigIntLit { .. }
            if below(ScriptTarget::ES2021) && raw(ast, src, id).contains('_') =>
        {
            Some("numeric separator")
        }
        // ES2020.
        Node::Member { optional: true, .. }
        | Node::Index { optional: true, .. }
        | Node::Call { optional: true, .. }
            if below(ScriptTarget::ES2020) =>
        {
            Some("optional chain")
        }
        Node::Binary {
            op: BinaryOp::Coalesce,
            ..
        } if below(ScriptTarget::ES2020) => Some("nullish coalescing"),
        // ES2019.
        Node::CatchClause { param: None, .. } if below(ScriptTarget::ES2019) => {
            Some("optional catch binding")
        }
        // ES2018.
        Node::ObjectLit { props }
            if below(ScriptTarget::ES2018)
                && ast
                    .list(props)
                    .iter()
                    .any(|&p| ast.kind(p) == NodeKind::Spread) =>
        {
            Some("object spread")
        }
        Node::ObjectPattern { props }
            if below(ScriptTarget::ES2018)
                && ast
                    .list(props)
                    .iter()
                    .any(|&p| ast.kind(p) == NodeKind::Rest) =>
        {
            Some("object rest")
        }
        Node::ForOf { is_await: true, .. } if below(ScriptTarget::ES2018) => Some("for await"),
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. }
            if flags.contains(FnFlags::ASYNC | FnFlags::GENERATOR)
                && below(ScriptTarget::ES2018) =>
        {
            Some("async generator")
        }
        // ES2017.
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. }
            if flags.contains(FnFlags::ASYNC) && below(ScriptTarget::ES2017) =>
        {
            Some("async function")
        }
        // ES2016.
        Node::Binary {
            op: BinaryOp::Exp, ..
        }
        | Node::Assign {
            op: AssignOp::ExpAssign,
            ..
        } if below(ScriptTarget::ES2016) => Some("exponentiation"),
        _ => None,
    }
}

/// The source text of a parsed node (empty for synthesized ones).
fn raw<'s>(ast: &Ast, src: &'s str, id: NodeId) -> &'s str {
    let s = ast.span(id);
    src.get(s.start as usize..s.end as usize).unwrap_or("")
}

/// `using` / `await using` in the original tree that tsc lowers for `target`.
pub(crate) fn using_needing_lowering(
    ast: &Ast,
    target: ScriptTarget,
) -> Option<(Span, &'static str)> {
    // `using` / `await using`: native only for ESNext (observed: ES2022-ES2025 get
    // `__addDisposableResource`). At ESNext tsc 7 keeps every `using` as written, a
    // top-level one followed by exports too (its ESNext chain has no `using` transform:
    // tsc/internal/transformers/estransforms/definitions.go).
    if target >= ScriptTarget::ESNext {
        return None;
    }
    for raw in 1..ast.node_count() as u32 {
        let Some(id) = NodeId::from_raw(raw) else {
            continue;
        };
        if let Node::VarDecl {
            kind: VarKind::Using | VarKind::AwaitUsing,
            ..
        } = *ast.node(id)
        {
            return Some((ast.span(id), "'using' declaration"));
        }
    }
    None
}
