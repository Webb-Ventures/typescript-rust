//! `prefer-arrow-callback`: arrow functions instead of function expressions as callbacks.

use super::prelude::*;
use crate::semantic::{Resolution, SymbolFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-arrow-callback",
        "Require using arrow functions for callbacks",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferArrowCallback>,
};

#[derive(Debug, Default)]
struct PreferArrowCallback {
    allow_named: bool,
    allow_unbound_this: bool,
}

impl FromOptions for PreferArrowCallback {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferArrowCallback {
            allow_named: bool_field(o, "allowNamedFunctions", false),
            allow_unbound_this: bool_field(o, "allowUnboundThis", true),
        })
    }
}

#[derive(Default)]
struct Uses {
    this: bool,
    sup: bool,
    meta: bool,
    arguments: bool,
}

/// What the function's own body uses (nested non-arrow functions not included).
fn uses(cx: &Ctx<'_, '_>, f: NodeId) -> Uses {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut u = Uses::default();
    let mut stack = Vec::new();
    ast.for_each_child(f, |c| stack.push(c));
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => continue,
            Node::This {} => u.this = true,
            Node::Super {} => u.sup = true,
            Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::NewTarget,
            } => u.meta = true,
            Node::Ident { .. } if ident_name(ast, n) == Some("arguments") => {
                if let Some(r) = sem.reference_at(n)
                    && let Resolution::Symbol(s) = sem.reference(r).resolution
                    && sem.symbol(s).flags.contains(SymbolFlags::ARGUMENTS)
                {
                    u.arguments = true;
                }
            }
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    u
}

/// ESLint's `getCallbackInfo`: (is a callback, bound with `.bind(this)`, the bind call).
fn callback_info(cx: &Ctx<'_, '_>, f: NodeId) -> (bool, bool, Option<NodeId>) {
    let ast = cx.ast();
    let mut cur = f;
    let mut bound = false;
    let mut lexical_this = false;
    let mut bind_call = None;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return (false, lexical_this, bind_call);
        };
        match *ast.node(p) {
            Node::Paren { .. } => {}
            Node::Binary {
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                ..
            } => {}
            Node::Conditional { test, .. } => {
                if test == cur {
                    return (false, lexical_this, bind_call);
                }
            }
            Node::Member { obj, prop, .. } => {
                if obj != cur || ident_name(ast, prop) != Some("bind") {
                    return (false, lexical_this, bind_call);
                }
                let call = parent_skip_parens(ast, cx.file.parents, p);
                let Some(call) = call.filter(|&c| {
                    matches!(*ast.node(c), Node::Call { callee, .. } if skip_parens(ast, callee) == p)
                }) else {
                    return (false, lexical_this, bind_call);
                };
                if !bound {
                    bound = true;
                    if let Node::Call { args, .. } = *ast.node(call) {
                        let args = ast.list(args);
                        lexical_this =
                            args.len() == 1 && matches!(ast.node(args[0]), Node::This {});
                    }
                    bind_call = Some(call);
                }
                cur = call;
                continue;
            }
            Node::Call { callee, .. } | Node::New { callee, .. } => {
                return (callee != cur, lexical_this, bind_call);
            }
            _ => return (false, lexical_this, bind_call),
        }
        cur = p;
    }
    (false, lexical_this, bind_call)
}

fn is_parenthesised(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    cx.parent(id)
        .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }))
}

impl PreferArrowCallback {
    fn fix(
        &self,
        cx: &Ctx<'_, '_>,
        f: NodeId,
        u: &Uses,
        lexical_this: bool,
        bind_call: Option<NodeId>,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let Node::FunctionExpr {
            flags,
            name,
            params,
            return_type,
            body: Some(body),
            ..
        } = *ast.node(f)
        else {
            return None;
        };
        let params = ast.list(params);
        if !lexical_this && u.this {
            return None;
        }
        let idents: Vec<&str> = params
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::Param {
                    pattern,
                    rest: false,
                    init: None,
                    ..
                } if matches!(ast.node(pattern), Node::Ident { .. }) => ident_name(ast, pattern),
                _ => None,
            })
            .collect();
        if idents.len() == params.len() {
            let mut sorted = idents.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.len() != idents.len() {
                return None;
            }
        }
        if let Some(&p) = params.first()
            && let Node::Param { pattern, .. } = *ast.node(p)
            && ident_name(ast, pattern) == Some("this")
        {
            return None;
        }
        let fspan = cx.span(f);
        let mut func = fspan.start as usize;
        if flags.contains(FnFlags::ASYNC) {
            func = skip_trivia_forward(src, func + "async".len());
        }
        if !src[func..].starts_with("function") {
            return None;
        }
        let func_end = func + "function".len();
        // The first `(` token after `function`.
        let body_start = cx.span(body).start as usize;
        let mut lparen = skip_trivia_forward(src, func_end);
        while lparen < body_start && !src[lparen..].starts_with('(') {
            lparen += src[lparen..].chars().next().map_or(1, char::len_utf8);
            lparen = skip_trivia_forward(src, lparen);
        }
        if lparen >= body_start {
            return None;
        }
        if flags.contains(FnFlags::ASYNC) && has_newline(src, func as u32, lparen as u32) {
            return None;
        }
        let mut fix: Option<Fix> = None;
        let mut add = |span: Span, text: &str| {
            fix = Some(match fix.take() {
                None => Fix::replace(span, text),
                Some(f) => f.and(span, text),
            });
        };
        if lexical_this {
            let member = parent_skip_parens(ast, cx.file.parents, f)?;
            let call = bind_call?;
            let Node::Member { obj, .. } = *ast.node(member) else {
                return None;
            };
            if is_parenthesised(cx, member) {
                return None;
            }
            let mut first = cx.span(obj).end as usize;
            loop {
                first = skip_trivia_forward(src, first);
                if src[first..].starts_with(')') {
                    first += 1;
                } else {
                    break;
                }
            }
            let last = cx.span(call).end;
            if !ast.comments_in(Span::new(first as u32, last)).is_empty() {
                return None;
            }
            add(Span::new(first as u32, last), "");
        }
        if !ast
            .comments_in(Span::new(func as u32, lparen as u32))
            .is_empty()
        {
            add(Span::new(func as u32, func_end as u32), "");
            if let Some(n) = name {
                add(cx.span(n), "");
            }
        } else {
            add(Span::new(func as u32, lparen as u32), "");
        }
        let before_body = match return_type {
            Some(rt) => cx.span(rt).end,
            None => skip_trivia_backward(ast, src, cx.span(body).start as usize) as u32,
        };
        add(Span::new(before_body, before_body), " =>");
        let replaced = if lexical_this { bind_call? } else { f };
        let parent = cx.parent(replaced);
        let wrap = !parent.is_some_and(|p| {
            matches!(
                cx.node(p),
                Node::Call { .. } | Node::Conditional { .. } | Node::Paren { .. }
            )
        }) && !is_parenthesised(cx, f);
        if wrap {
            let s = cx.span(replaced);
            add(Span::new(s.start, s.start), "(");
            add(Span::new(s.end, s.end), ")");
        }
        fix
    }
}

impl Rule for PreferArrowCallback {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::FunctionExpr { flags, name, .. } = *cx.node(node) else {
            return;
        };
        // Methods' values are function expressions too.
        if cx
            .parent(node)
            .is_some_and(|p| matches!(cx.node(p), Node::MethodDef { .. }))
        {
            return;
        }
        if self.allow_named && name.is_some() {
            return;
        }
        if flags.contains(FnFlags::GENERATOR) {
            return;
        }
        if let Some(n) = name {
            let sem = cx.semantic();
            if sem
                .symbol_of_decl(n)
                .is_some_and(|s| !sem.symbol(s).refs.is_empty())
            {
                return;
            }
        }
        let u = uses(cx, node);
        if u.arguments {
            return;
        }
        let (callback, lexical_this, bind_call) = callback_info(cx, node);
        if !callback || !(!self.allow_unbound_this || !u.this || lexical_this) || u.sup || u.meta {
            return;
        }
        let fix = self.fix(cx, node, &u, lexical_this, bind_call);
        cx.report_node(node, "Unexpected function expression.")
            .id("preferArrowCallback")
            .fix_opt(fix);
    }
}
