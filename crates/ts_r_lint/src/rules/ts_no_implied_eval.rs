//! `@typescript-eslint/no-implied-eval`: disallow string code passed to `setTimeout` and
//! friends, and the `Function` constructor.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-implied-eval",
        "Disallow the use of `eval()`-like functions",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoImpliedEval>,
};

#[derive(Debug, Default)]
struct NoImpliedEval;

const EVAL_FUNCTIONS: &[&str] = &["setTimeout", "setInterval", "setImmediate", "execScript"];
const GLOBAL_OBJECTS: &[&str] = &["window", "global", "globalThis"];

impl Rule for NoImpliedEval {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (callee, args) = match *cx.node(node) {
            Node::Call { callee, args, .. } | Node::New { callee, args, .. } => (callee, args),
            _ => return,
        };
        let Some(name) = global_callee_name(cx, callee) else {
            return;
        };
        if name == "Function" {
            cx.report_node(
                node,
                "Implied eval. Do not use the Function constructor to create functions.",
            )
            .id("noFunctionConstructor");
            return;
        }
        if !EVAL_FUNCTIONS.contains(&name.as_str()) || matches!(cx.node(node), Node::New { .. }) {
            return;
        }
        let Some(&arg) = cx.ast().list(args).first() else {
            return;
        };
        if matches!(cx.node(arg), Node::Spread { .. }) {
            return;
        }
        // `f.bind(..)` makes a function, whatever type the lib's `bind` declares.
        let ast = cx.ast();
        if let Node::Call { callee, .. } = *cx.node(skip_parens(ast, arg))
            && let Node::Member { prop, .. } = *cx.node(skip_parens(ast, callee))
            && ident_name(ast, prop) == Some("bind")
        {
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(arg) else { return };
        if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
            return;
        }
        let callable = |p: Ty| {
            let p = constrained(ti, p);
            p != Ty::ANY
                && !is_any(ti, p)
                && (!ti.signatures(p, false).is_empty()
                    || is_builtin_symbol_like(ti, p, &["Function"]))
        };
        if parts(ti, t).into_iter().all(callable) {
            return;
        }
        cx.report_node(arg, "Implied eval. Consider passing a function.")
            .id("noImpliedEvalError");
    }
}

/// The name a call goes to when it is a global function: `name(..)` (not shadowed by a
/// declaration of this program) or `window.name(..)` / `globalThis['name'](..)`.
fn global_callee_name(cx: &Ctx<'_, '_>, callee: NodeId) -> Option<String> {
    let ast = cx.ast();
    let callee = skip_parens(ast, callee);
    match *cx.node(callee) {
        Node::Ident { .. } => {
            let name = ident_name(ast, callee)?;
            if !(EVAL_FUNCTIONS.contains(&name) || name == "Function") {
                return None;
            }
            is_global(cx, callee).then(|| name.to_string())
        }
        Node::Member { obj, prop, .. } => {
            let o = ident_name(ast, skip_parens(ast, obj))?;
            if !GLOBAL_OBJECTS.contains(&o) {
                return None;
            }
            ident_name(ast, prop).map(str::to_string)
        }
        Node::Index { obj, index, .. } => {
            let o = ident_name(ast, skip_parens(ast, obj))?;
            if !GLOBAL_OBJECTS.contains(&o) {
                return None;
            }
            static_value_string(ast, cx.src(), skip_parens(ast, index)).map(|s| s.into_owned())
        }
        _ => None,
    }
}

/// `ident` refers to a global: no symbol, or one declared only in lib files.
fn is_global(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ti = cx.types();
    let Some(s) = ti.symbol_at(ident) else {
        return true;
    };
    ti.symbol_decls(s).iter().all(|d| d.in_lib || d.in_external)
}
