//! `unicorn/no-repeated-call-wrappers`: a local function whose every call (two or more, all
//! outside the function itself) is the only argument of the same wrapper, `new X(f(..))` or
//! `String`/`Number`/`Boolean`/`BigInt(f(..))`, should return the wrapped value itself.
//!
//! The functions are those whose callers are all known (see
//! `unicorn_no_unnecessary_parameters::local_call_targets`); async functions and generators
//! are skipped. Each call must be a plain call (no `?.`) whose parent, through parentheses
//! only, is the wrapper; every wrapper must be the same kind, name the same binding as the
//! name means inside the function, and carry the same type-argument text. The report goes on
//! the function's name, else the name or key it is assigned to, else the function.

use super::prelude::*;
use super::unicorn_no_unnecessary_parameters::local_call_targets;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-repeated-call-wrappers",
        "Disallow repeating the same wrapper at every call to a function.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoRepeatedCallWrappers>,
};

#[derive(Debug, Default)]
struct NoRepeatedCallWrappers;

const PRIMITIVE_CONVERSIONS: &[&str] = &["String", "Number", "Boolean", "BigInt"];

/// The wrapper around `call`: `(wrapper, is_new, name, type_args)`.
fn wrapper_of<'a>(
    cx: &Ctx<'_, 'a>,
    call: NodeId,
) -> Option<(NodeId, bool, &'a str, Option<NodeId>)> {
    let ast: &'a Ast = cx.ast();
    let Node::Call {
        callee, optional, ..
    } = *ast.node(call)
    else {
        return None;
    };
    if optional {
        return None;
    }
    if let Node::Member { optional: true, .. } | Node::Index { optional: true, .. } =
        *ast.node(skip_wrappers(ast, callee))
    {
        return None;
    }
    let wrapper = parent_skip_parens(ast, cx.file.parents, call)?;
    let (is_new, wcallee, type_args, args) = match *ast.node(wrapper) {
        Node::New {
            callee,
            type_args,
            args,
            ..
        } => (true, callee, type_args, args),
        Node::Call {
            callee,
            optional: false,
            type_args,
            args,
        } => (false, callee, type_args, args),
        _ => return None,
    };
    let Node::Ident { .. } = *ast.node(wcallee) else {
        return None;
    };
    let name = ident_name(ast, wcallee)?;
    if !is_new && !PRIMITIVE_CONVERSIONS.contains(&name) {
        return None;
    }
    let args = ast.list(args);
    if args.len() != 1 || skip_parens(ast, args[0]) != call {
        return None;
    }
    Some((wrapper, is_new, name, type_args))
}

/// The node the report goes on (see the module docs).
fn report_node(cx: &Ctx<'_, '_>, func: NodeId) -> NodeId {
    let ast = cx.ast();
    if let Node::FunctionDecl { name: Some(n), .. } | Node::FunctionExpr { name: Some(n), .. } =
        *ast.node(func)
    {
        return n;
    }
    let mut cur = func;
    for _ in 0..1000 {
        let Some(p) = cx.parent(cur) else {
            return func;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            Node::VarDeclarator { name, .. } => return name,
            Node::MethodDef { key, .. }
            | Node::PropertyDef { key, .. }
            | Node::Property { key, .. } => return key,
            _ => return func,
        }
    }
    func
}

impl Rule for NoRepeatedCallWrappers {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let Some(targets) = local_call_targets(cx) else {
            return;
        };
        let ast = cx.ast();
        let sem = cx.semantic();
        let mut reports: Vec<(NodeId, String)> = Vec::new();
        for t in &targets {
            let flags = match *ast.node(t.func) {
                Node::FunctionDecl { flags, .. }
                | Node::FunctionExpr { flags, .. }
                | Node::Arrow { flags, .. } => flags,
                _ => continue,
            };
            if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) || t.calls.len() < 2 {
                continue;
            }
            let fspan = cx.span(t.func);
            let mut wrappers = Vec::with_capacity(t.calls.len());
            let mut ok = true;
            for &c in &t.calls {
                let cs = cx.span(c);
                if cs.start >= fspan.start && cs.end <= fspan.end {
                    ok = false;
                    break;
                }
                match wrapper_of(cx, c) {
                    Some(w) => wrappers.push(w),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let (first, first_new, name, first_targs) = wrappers[0];
            let binding = sem.lookup(sem.enclosing_scope(first), name);
            let fscope = sem
                .scope_of_node(t.func)
                .unwrap_or_else(|| sem.enclosing_scope(t.func));
            let targs_text = |t: Option<NodeId>| t.map_or("", |t| cx.text(t));
            let first_text = targs_text(first_targs);
            if sem.lookup(fscope, name) != binding
                || wrappers.iter().any(|&(w, is_new, n, targs)| {
                    is_new != first_new
                        || n != name
                        || sem.lookup(sem.enclosing_scope(w), name) != binding
                        || targs_text(targs) != first_text
                })
            {
                continue;
            }
            let wrapper = if first_new {
                format!("new {name}()")
            } else {
                format!("{name}()")
            };
            reports.push((
                report_node(cx, t.func),
                format!(
                    "Every call wraps the result of this function in `{wrapper}`. Consider returning the wrapped value directly."
                ),
            ));
        }
        for (node, msg) in reports {
            cx.report_node(node, msg).id("no-repeated-call-wrappers");
        }
    }
}
