//! `@typescript-eslint/prefer-promise-reject-errors`: promises should be rejected with
//! `Error` objects.

use super::prelude::*;
use super::ts_only_throw_error::error_like;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-promise-reject-errors",
        "Require using Error objects as Promise rejection reasons",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<PreferPromiseRejectErrors>,
};

#[derive(Debug)]
struct PreferPromiseRejectErrors {
    allow: Vec<Specifier>,
    allow_empty: bool,
    any: bool,
    unknown: bool,
}

impl FromOptions for PreferPromiseRejectErrors {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferPromiseRejectErrors {
            allow: parse_specifiers(o.get("allow")),
            allow_empty: bool_field(o, "allowEmptyReject", false),
            any: bool_field(o, "allowThrowingAny", false),
            unknown: bool_field(o, "allowThrowingUnknown", false),
        })
    }
}

const MSG: &str = "Expected the Promise rejection reason to be an Error.";

/// `Promise` itself (a `PromiseConstructor`), or a class extending `Promise`.
fn is_promise_constructor(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED {
        return false;
    }
    let t = constrained(ti, t);
    if is_builtin_symbol_like(ti, t, &["PromiseConstructor"]) {
        return true;
    }
    ti.type_symbol(t)
        .and_then(|s| ti.declared_type_of_symbol(s))
        .is_some_and(|inst| inst != t && is_builtin_symbol_like(ti, inst, &["Promise"]))
}

impl PreferPromiseRejectErrors {
    /// Reports `call` when its first argument is not an error.
    fn check_call(&self, cx: &mut Ctx<'_, '_>, call: NodeId, args: ts_r_ast::NodeList) {
        let ast = cx.ast();
        let Some(&arg) = ast.list(args).first() else {
            if !self.allow_empty {
                cx.report_node(call, MSG).id("rejectAnError");
            }
            return;
        };
        if matches!(cx.node(arg), Node::Spread { .. }) {
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(arg) else { return };
        if error_like(ti, t, &self.allow, self.any, self.unknown) == Some(false) {
            cx.report_node(call, MSG).id("rejectAnError");
        }
    }
}

impl Rule for PreferPromiseRejectErrors {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Call { callee, args, .. } => {
                let callee = skip_parens(ast, callee);
                let (obj, is_reject) = match *cx.node(callee) {
                    Node::Member { obj, prop, .. } => {
                        (obj, ident_name(ast, prop) == Some("reject"))
                    }
                    Node::Index { obj, index, .. } => (
                        obj,
                        static_value_string(ast, cx.src(), skip_parens(ast, index))
                            .is_some_and(|s| s == "reject"),
                    ),
                    _ => return,
                };
                if !is_reject {
                    return;
                }
                let ti = cx.types();
                let obj = skip_parens(ast, obj);
                let promise_class = ti
                    .symbol_at(obj)
                    .and_then(|s| ti.declared_type_of_symbol(s))
                    .is_some_and(|inst| {
                        inst != Ty::UNRESOLVED
                            && ti.type_at(obj) != Some(inst)
                            && is_builtin_symbol_like(ti, inst, &["Promise"])
                    });
                if !promise_class
                    && !ti
                        .type_at(obj)
                        .is_some_and(|t| is_promise_constructor(ti, t))
                {
                    return;
                }
                self.check_call(cx, node, args);
            }
            Node::New { callee, args, .. } => {
                let ti = cx.types();
                if !ti
                    .type_at(skip_parens(ast, callee))
                    .is_some_and(|t| is_promise_constructor(ti, t))
                {
                    return;
                }
                let Some(&executor) = ast.list(args).first() else {
                    return;
                };
                let executor = skip_parens(ast, executor);
                let params = match *cx.node(executor) {
                    Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => params,
                    _ => return,
                };
                let Some(&p) = ast.list(params).get(1) else {
                    return;
                };
                let Node::Param {
                    pattern,
                    rest: false,
                    ..
                } = *cx.node(p)
                else {
                    return;
                };
                if !matches!(cx.node(pattern), Node::Ident { .. }) {
                    return;
                }
                let sem = cx.semantic();
                let Some(sym) = sem.symbol_of_decl(pattern) else {
                    return;
                };
                let calls: Vec<(NodeId, ts_r_ast::NodeList)> = sem
                    .symbol(sym)
                    .refs
                    .iter()
                    .filter_map(|&r| {
                        let ident = sem.reference(r).node;
                        let parent = cx.parent(ident)?;
                        match *cx.node(parent) {
                            Node::Call { callee, args, .. } if callee == ident => {
                                Some((parent, args))
                            }
                            _ => None,
                        }
                    })
                    .collect();
                for (call, args) in calls {
                    self.check_call(cx, call, args);
                }
            }
            _ => {}
        }
    }
}
