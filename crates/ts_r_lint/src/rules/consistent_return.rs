//! `consistent-return`: every `return` of a function specifies a value, or none does.

use super::func_names::own_name_with_kind;
use super::prelude::*;
use crate::cfg::{Cfg, Options};
use crate::diagnostic::Loc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "consistent-return",
        "Require `return` statements to either always or never specify values",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<ConsistentReturn>,
};

#[derive(Debug, Default)]
pub(crate) struct ConsistentReturn {
    undefined_unspecified: bool,
    /// typescript-eslint's version: bare returns of `void` functions don't count, and
    /// `treatUndefinedAsUnspecified` looks at the value's type.
    pub(crate) ts: bool,
}

impl FromOptions for ConsistentReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ConsistentReturn {
            undefined_unspecified: bool_field(
                obj_opt(opts, 0),
                "treatUndefinedAsUnspecified",
                false,
            ),
            ts: false,
        })
    }
}

fn is_boundary(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::StaticBlock { .. }
    )
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl ConsistentReturn {
    fn has_value(&self, cx: &Ctx<'_, '_>, ret: NodeId) -> bool {
        let ast = cx.ast();
        let Node::Return { arg: Some(a) } = *ast.node(ret) else {
            return false;
        };
        if !self.undefined_unspecified {
            return true;
        }
        if self.ts {
            let ti = cx.types();
            return ti
                .type_at(a)
                .is_none_or(|t| ti.flags(t) != TypeFlags::UNDEFINED);
        }
        let a = skip_parens(ast, a);
        !(ident_name(ast, a) == Some("undefined")
            || matches!(
                ast.node(a),
                Node::Unary {
                    op: ts_r_ast::UnaryOp::Void,
                    ..
                }
            ))
    }

    /// Where ESLint reports a missing final return: the program start, the `=>`, a method's
    /// key, the function's name or its first token.
    fn head(cx: &Ctx<'_, '_>, f: NodeId) -> Span {
        let ast = cx.ast();
        let src = cx.src();
        match *ast.node(f) {
            Node::Arrow { body, .. } => {
                let b = cx.span(body).start as usize;
                match src[cx.span(f).start as usize..b].rfind("=>") {
                    Some(i) => {
                        let at = cx.span(f).start + i as u32;
                        Span::new(at, at + 2)
                    }
                    None => cx.span(f),
                }
            }
            Node::FunctionDecl { name, .. } | Node::FunctionExpr { name, .. } => {
                if let Some(p) = cx.parent(f)
                    && let Node::MethodDef { key, .. } = *ast.node(p)
                {
                    return match *ast.node(key) {
                        Node::ComputedKey { expr } => cx.span(expr),
                        _ => cx.span(key),
                    };
                }
                if let Some(n) = name {
                    return cx.span(n);
                }
                let s = cx.span(f).start;
                let len = src[s as usize..]
                    .find(|c: char| !(c == '$' || ts_r_lexer::is_id_part(c)))
                    .unwrap_or(0);
                Span::new(s, s + len as u32)
            }
            _ => cx.span(f),
        }
    }

    /// The function's type returns `void` (`Promise<void>` when async).
    fn returns_void(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
        let ti = cx.types();
        let is_async = match *cx.node(f) {
            Node::FunctionDecl { flags, .. }
            | Node::FunctionExpr { flags, .. }
            | Node::Arrow { flags, .. } => flags.contains(FnFlags::ASYNC),
            _ => return false,
        };
        let Some(t) = ti.type_at(f) else {
            return false;
        };
        // `void` itself or in a union (typescript-eslint's `isTypeFlagSet`).
        let has_void = |t: Ty| {
            ti.flags(t).contains(TypeFlags::VOID)
                || (ti.flags(t).contains(TypeFlags::UNION)
                    && ti
                        .union_members(t)
                        .into_iter()
                        .any(|m| ti.flags(m).contains(TypeFlags::VOID)))
        };
        ti.signatures(t, false).into_iter().any(|sig| {
            let mut r = ti.return_type(sig);
            if !is_async {
                return has_void(r);
            }
            for _ in 0..32 {
                if thenable(ti, r) != Some(true) {
                    return false;
                }
                let Some(&a) = ti.type_args(r).first() else {
                    return false;
                };
                if has_void(a) {
                    return true;
                }
                r = a;
            }
            false
        })
    }

    pub(crate) fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let mut rets = Vec::new();
        let mut stack = Vec::new();
        ast.for_each_child(node, |c| stack.push(c));
        while let Some(n) = stack.pop() {
            match *ast.node(n) {
                ref x if is_boundary(x) => continue,
                Node::Return { .. } => rets.push(n),
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        if self.ts && !rets.is_empty() && Self::returns_void(cx, node) {
            rets.retain(|&r| matches!(ast.node(r), Node::Return { arg: Some(_) }));
        }
        rets.sort_by_key(|&r| cx.span(r).start);
        let Some(&first) = rets.first() else {
            return;
        };
        let program = matches!(ast.node(node), Node::Program { .. });
        let first_value = self.has_value(cx, first);
        let name = if program {
            "Program".to_string()
        } else {
            upper_first(&own_name_with_kind(cx, node))
        };
        for &r in &rets[1..] {
            if self.has_value(cx, r) != first_value {
                let msg = if first_value {
                    format!("{name} expected a return value.")
                } else {
                    format!("{name} expected no return value.")
                };
                let id = if first_value {
                    "missingReturnValue"
                } else {
                    "unexpectedReturnValue"
                };
                cx.report_node(r, msg).id(id);
            }
        }
        if !first_value {
            return;
        }
        // Constructors: `class { constructor() {} }` and ES5 `function Foo() {}`.
        let parent = cx.parent(node);
        if parent.is_some_and(|p| {
            matches!(
                cx.node(p),
                Node::MethodDef {
                    kind: MethodKind::Constructor,
                    ..
                }
            )
        }) {
            return;
        }
        if let Node::FunctionDecl { name: Some(n), .. } | Node::FunctionExpr { name: Some(n), .. } =
            *ast.node(node)
            && ident_name(ast, n)
                .and_then(|s| s.chars().next())
                .is_some_and(|c| c.to_lowercase().ne(std::iter::once(c)))
        {
            return;
        }
        let opts = Options {
            try_end_to_catch: true,
            ..Options::default()
        };
        let cfg = match *ast.node(node) {
            Node::Program { stmts } => Cfg::statements_with(ast, ast.list(stmts), &opts),
            _ => Cfg::function_with(ast, node, &opts),
        };
        if !cfg.is_reachable(cfg.exit) {
            return;
        }
        if program {
            cx.report(
                Span::at(0),
                "Expected to return a value at the end of program.",
            )
            .id("missingReturn")
            .loc(Loc::at(1, 1));
            return;
        }
        let kind = own_name_with_kind(cx, node);
        let head = Self::head(cx, node);
        cx.report(
            head,
            format!("Expected to return a value at the end of {kind}."),
        )
        .id("missingReturn");
    }
}

impl Rule for ConsistentReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        self.check(cx, node);
    }
}
