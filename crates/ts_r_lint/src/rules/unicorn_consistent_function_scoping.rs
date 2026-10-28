//! `unicorn/consistent-function-scoping`: a function declared inside another function (as
//! a statement, a `const` initializer or a returned arrow, also through a chain of returned
//! arrows) that uses nothing from the scope it sits in could live in the outer scope.
//!
//! "The scope it sits in" is the function (or catch clause, static block...) owning the
//! block, plus, for a block that is a loop body (or nested bare blocks in one), the loop's
//! and those blocks' scopes. The function uses that scope when one of the variables it
//! references is declared there (sibling function declarations count, the function's own
//! name does not), is referenced from there, or is a binding of the owning function itself.
//! Skipped: top-level functions, React hook callbacks' inner functions, IIFE bodies, arrows
//! reading `this`/`super`/`new.target`, and factories passed to `jest.mock`/`vi.mock`
//! (second argument) or `vi.hoisted`. `checkArrowFunctions: false` skips arrows.

use super::prelude::*;
use super::unicorn_ast::matches_path;
use crate::semantic::{DeclKind, Resolution, ScopeKind};
use ts_r_ast::{Ast, MetaPropKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-function-scoping",
        "Move function definitions to the highest possible scope.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ConsistentFunctionScoping>,
};

#[derive(Debug)]
struct ConsistentFunctionScoping {
    check_arrows: bool,
}

impl FromOptions for ConsistentFunctionScoping {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut check_arrows = true;
        if let Some(o) = opts.first().and_then(Value::as_object)
            && let Some(v) = o.get("checkArrowFunctions")
        {
            check_arrows = v
                .as_bool()
                .ok_or("`checkArrowFunctions` must be a boolean")?;
        }
        Ok(ConsistentFunctionScoping { check_arrows })
    }
}

const REACT_HOOKS: &[&str] = &[
    "useState",
    "useEffect",
    "useContext",
    "useReducer",
    "useCallback",
    "useMemo",
    "useRef",
    "useImperativeHandle",
    "useLayoutEffect",
    "useDebugValue",
];

fn is_function_node(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn matches_any(ast: &Ast, callee: NodeId, paths: &[&str]) -> bool {
    paths.iter().any(|p| matches_path(ast, callee, p))
}

fn is_react_hook_callee(ast: &Ast, callee: NodeId) -> bool {
    let callee = skip_parens(ast, callee);
    REACT_HOOKS
        .iter()
        .any(|h| matches_path(ast, callee, h) || matches_path(ast, callee, &format!("React.{h}")))
}

/// `this`, `super` or `new.target` that belongs to the scope around `id` (not rebound by a
/// nested non-arrow function or class body).
fn contains_lexical_this(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 512 {
        return false;
    }
    match *ast.node(id) {
        Node::This {}
        | Node::Super {}
        | Node::MetaProperty {
            kind: MetaPropKind::NewTarget,
        } => true,
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => false,
        Node::ClassDecl {
            heritage, members, ..
        }
        | Node::ClassExpr {
            heritage, members, ..
        } => {
            ast.list(heritage)
                .iter()
                .any(|&h| contains_lexical_this(ast, h, depth + 1))
                || ast.list(members).iter().any(|&m| {
                    let key = match *ast.node(m) {
                        Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                        _ => return false,
                    };
                    matches!(ast.node(key), Node::ComputedKey { .. })
                        && contains_lexical_this(ast, key, depth + 1)
                })
        }
        _ => {
            let mut found = false;
            ast.for_each_child(id, |c| {
                if !found && contains_lexical_this(ast, c, depth + 1) {
                    found = true;
                }
            });
            found
        }
    }
}

/// Inside the factory passed to `jest.mock(path, factory)`, `vi.mock(path, factory)` or
/// `vi.hoisted(factory)`.
fn in_hoisted_factory(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    for cur in ancestors_inclusive(cx.file.parents, node) {
        if matches!(ast.node(cur), Node::Paren { .. }) {
            continue;
        }
        let Some(p) = parent_skip_parens(ast, cx.file.parents, cur) else {
            break;
        };
        let Node::Call { callee, args, .. } = *ast.node(p) else {
            continue;
        };
        let inner = super::unicorn_util::skip_wrappers(ast, cur);
        if !is_function_node(ast, inner) {
            continue;
        }
        let args = ast.list(args);
        let arg = |i: usize| args.get(i).map(|&a| skip_parens(ast, a));
        let callee = skip_parens(ast, callee);
        if (arg(1) == Some(cur) && matches_any(ast, callee, &["jest.mock", "vi.mock"]))
            || (arg(0) == Some(cur) && matches_path(ast, callee, "vi.hoisted"))
        {
            return true;
        }
    }
    false
}

fn is_loop(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::DoWhile { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::For { .. }
            | Node::While { .. }
    )
}

fn loop_body(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::DoWhile { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::For { body, .. }
        | Node::While { body, .. } => Some(body),
        _ => None,
    }
}

impl ConsistentFunctionScoping {
    /// The scope nodes the function `f` sits in, or `None` when it is skipped.
    fn parent_scopes(cx: &Ctx<'_, '_>, f: NodeId) -> Option<Vec<NodeId>> {
        let ast = cx.ast();
        let sem = cx.semantic();
        let is_arrow = matches!(ast.node(f), Node::Arrow { .. });
        let mut parent = parent_skip_parens(ast, cx.file.parents, f)?;
        if matches!(ast.node(parent), Node::VarDeclarator { .. }) {
            parent = cx.parent(parent)?;
        }
        if matches!(ast.node(parent), Node::VarDecl { .. }) {
            parent = cx.parent(parent)?;
        }
        if is_arrow && matches!(ast.node(parent), Node::Return { .. }) {
            parent = cx.parent(parent)?;
        }
        if is_arrow && matches!(ast.node(parent), Node::Arrow { .. }) {
            let mut a = parent;
            while matches!(ast.node(a), Node::Arrow { .. }) {
                a = parent_skip_parens(ast, cx.file.parents, a)?;
            }
            if matches!(ast.node(a), Node::Return { .. }) {
                parent = cx.parent(a)?;
            }
        }
        let mut block = None;
        if matches!(ast.node(parent), Node::Block { .. }) {
            block = Some(parent);
            parent = cx.parent(parent)?;
        }
        if matches!(ast.node(parent), Node::Program { .. }) {
            return None;
        }
        let own_scope = |n: NodeId| sem.scope_of_node(n).filter(|&s| sem.scope(s).node == n);
        let parent_scope = own_scope(parent);
        let mut scopes = Vec::new();
        if let Some(s) = parent_scope {
            scopes.push(s);
        }
        // A loop body block (through nested bare blocks).
        let mut chain = Vec::new();
        let mut cur = block;
        while let Some(b) = cur {
            if !matches!(ast.node(b), Node::Block { .. }) {
                chain.clear();
                break;
            }
            chain.push(b);
            let p = cx.parent(b);
            if p.is_some_and(|p| is_loop(ast, p) && loop_body(ast, p) == Some(b)) {
                if let Some(s) = p.and_then(own_scope) {
                    scopes.push(s);
                }
                break;
            }
            cur = p;
            if cur.is_none() {
                chain.clear();
            }
        }
        if !chain
            .last()
            .and_then(|&b| cx.parent(b))
            .is_some_and(|p| is_loop(ast, p))
        {
            chain.clear();
        }
        for &b in &chain {
            if let Some(s) = own_scope(b) {
                scopes.push(s);
            }
        }
        if scopes.is_empty()
            || scopes
                .iter()
                .any(|&s| matches!(sem.scope(s).kind, ScopeKind::Global | ScopeKind::Module))
        {
            return None;
        }
        if parent_scope.is_some() {
            // React hook callbacks and IIFEs.
            if let Some(Node::Call { callee, .. }) =
                parent_skip_parens(ast, cx.file.parents, parent).map(|c| ast.node(c))
            {
                if is_react_hook_callee(ast, *callee) {
                    return None;
                }
                if is_function_node(ast, parent)
                    && !matches!(ast.node(parent), Node::FunctionDecl { .. })
                    && skip_parens(ast, *callee) == parent
                {
                    return None;
                }
            }
        }
        Some(scopes.iter().map(|&s| sem.scope(s).node).collect())
    }

    fn uses_parent_scope(cx: &Ctx<'_, '_>, f: NodeId, parents: &[NodeId]) -> bool {
        let ast = cx.ast();
        let sem = cx.semantic();
        let span = cx.span(f);
        let in_parents = |n: NodeId| parents.contains(&n);
        let scope_node = |s| sem.scope(s).node;
        // The scope `s` is `f`'s own scope or inside it.
        let within = |mut s: crate::semantic::ScopeId| {
            for _ in 0..10_000 {
                let sc = sem.scope(s);
                if sc.node == f {
                    return true;
                }
                match sc.parent {
                    Some(p) => s = p,
                    None => return false,
                }
            }
            false
        };
        let mut seen = Vec::new();
        for r in sem.references() {
            let rs = cx.span(r.node);
            if rs.start < span.start || rs.end > span.end || !within(r.scope) {
                continue;
            }
            let Resolution::Symbol(sym) = r.resolution else {
                continue;
            };
            if seen.contains(&sym) {
                continue;
            }
            seen.push(sym);
            let s = sem.symbol(sym);
            if s.refs
                .iter()
                .any(|&x| in_parents(scope_node(sem.reference(x).scope)))
            {
                return true;
            }
            let function_name = s
                .decls
                .first()
                .is_some_and(|d| matches!(d.kind, DeclKind::Function | DeclKind::FunctionExprName));
            if !function_name && in_parents(scope_node(s.scope)) {
                return true;
            }
            for d in &s.decls {
                let def_node = match d.kind {
                    DeclKind::Param => ancestors_inclusive(cx.file.parents, d.node)
                        .find(|&a| is_function_node(ast, a))
                        .unwrap_or(d.node),
                    _ => d.node,
                };
                if in_parents(def_node) {
                    return true;
                }
                if d.kind == DeclKind::Function
                    && matches!(ast.node(d.node), Node::FunctionDecl { .. })
                    && d.node != f
                    && let Some(own) = sem.scope_of_node(d.node)
                    && let Some(upper) = sem.scope(own).parent
                    && in_parents(scope_node(upper))
                {
                    return true;
                }
            }
        }
        false
    }
}

impl Rule for ConsistentFunctionScoping {
    fn exit_kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn exit(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let is_arrow = matches!(ast.node(node), Node::Arrow { .. });
        if is_arrow && !self.check_arrows {
            return;
        }
        if cx.semantic().scope_of_node(node).is_none() || in_hoisted_factory(cx, node) {
            return;
        }
        let Some(parents) = Self::parent_scopes(cx, node) else {
            return;
        };
        if (is_arrow && contains_lexical_this(ast, node, 0))
            || Self::uses_parent_scope(cx, node, &parents)
        {
            return;
        }
        let head = function_head_span(ast, cx.src(), cx.file.parents, node);
        let name = function_name_with_kind(ast, cx.src(), cx.file.parents, node);
        cx.report(head, format!("Move {name} to the outer scope."))
            .id("consistent-function-scoping");
    }
}
