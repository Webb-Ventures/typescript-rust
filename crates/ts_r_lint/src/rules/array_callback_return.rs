//! `array-callback-return`: callbacks of `map`, `filter`, `reduce`, `Array.from` & co. return
//! a value (and, with `checkForEach`, `forEach` callbacks don't).

use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;
use crate::cfg::Cfg;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "array-callback-return",
        "Enforce `return` statements in callbacks of array methods",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create::<ArrayCallbackReturn>,
};

#[derive(Debug)]
struct ArrayCallbackReturn {
    allow_implicit: bool,
    check_for_each: bool,
    allow_void: bool,
}

impl FromOptions for ArrayCallbackReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(ArrayCallbackReturn {
            allow_implicit: bool_field(o, "allowImplicit", false),
            check_for_each: bool_field(o, "checkForEach", false),
            allow_void: bool_field(o, "allowVoid", false),
        })
    }
}

const TARGET_METHODS: &[&str] = &[
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flatMap",
    "forEach",
    "map",
    "reduce",
    "reduceRight",
    "some",
    "sort",
    "toSorted",
];

/// `X.name` where `X` is an identifier accepted by `object`.
fn member_of(cx: &Ctx<'_, '_>, callee: NodeId, object: impl Fn(&str) -> bool, name: &str) -> bool {
    let ast = cx.ast();
    member_parts(ast, cx.src(), callee).is_some_and(|(obj, n)| {
        let obj = skip_parens(ast, obj);
        n == name
            && matches!(ast.node(obj), Node::Ident { .. })
            && ident_name(ast, obj).is_some_and(&object)
    })
}

struct Returns {
    root: NodeId,
    found: Vec<NodeId>,
}

impl Visitor for Returns {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                if id != self.root =>
            {
                Walk::SkipChildren
            }
            Node::Return { .. } => {
                self.found.push(id);
                Walk::Continue
            }
            _ => Walk::Continue,
        }
    }
}

fn full_name(m: &str) -> String {
    if ["from", "fromAsync", "of", "isArray"].contains(&m) {
        format!("Array.{m}")
    } else {
        format!("Array.prototype.{m}")
    }
}

fn is_void(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::Unary {
            op: UnaryOp::Void,
            ..
        }
    )
}

/// `void ` before `expr` (in parentheses when it binds looser than `void`).
fn prepend_void(cx: &Ctx<'_, '_>, expr: NodeId, after_return: bool) -> Fix {
    let ast = cx.ast();
    let span = cx.span(expr);
    let parens = !matches!(ast.node(expr), Node::Paren { .. }) && precedence(ast, expr) < 16;
    let space = after_return
        && span.start > 0
        && !cx.src().as_bytes()[span.start as usize - 1].is_ascii_whitespace();
    let text = format!(
        "{}void {}",
        if space { " " } else { "" },
        if parens { "(" } else { "" }
    );
    let fix = Fix::insert(span.start, text);
    if parens {
        fix.and(Span::new(span.end, span.end), ")")
    } else {
        fix
    }
}

/// The array method `f` is a callback of: `arr.map(f)`, `Array.from(x, f)`, through
/// `a || f`, `c ? f : g` and functions returning `f` that are called right away.
fn array_method_name(cx: &Ctx<'_, '_>, f: NodeId, is_async: bool) -> Option<String> {
    let ast = cx.ast();
    let src = cx.src();
    let mut cur = f;
    for _ in 0..10_000 {
        let parent = parent_skip_parens(ast, cx.file.parents, cur)?;
        match *ast.node(parent) {
            Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                ..
            }
            | Node::Conditional { .. } => cur = parent,
            Node::Return { .. } => {
                let func = cx.file.parents.ancestors(parent).find(|&a| {
                    matches!(
                        ast.node(a),
                        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                    )
                })?;
                let call = parent_skip_parens(ast, cx.file.parents, func)?;
                let Node::Call { callee, .. } = *ast.node(call) else {
                    return None;
                };
                if skip_parens(ast, callee) != func {
                    return None;
                }
                cur = call;
            }
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                let at = |i: usize| args.get(i).is_some_and(|&a| skip_parens(ast, a) == cur);
                if !is_async {
                    if member_of(cx, callee, |o| o.ends_with("Array"), "from") && at(1) {
                        return Some("from".into());
                    }
                    if let Some((_, n)) = member_parts(ast, src, callee)
                        && TARGET_METHODS.contains(&n.as_ref())
                        && at(0)
                    {
                        return Some(n.into_owned());
                    }
                }
                if member_of(cx, callee, |o| o == "Array", "fromAsync") && at(1) {
                    return Some("fromAsync".into());
                }
                return None;
            }
            _ => return None,
        }
    }
    None
}

impl ArrayCallbackReturn {
    fn check(&self, f: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (flags, body) = match *cx.node(f) {
            Node::FunctionExpr {
                flags,
                body: Some(body),
                ..
            }
            | Node::Arrow { flags, body, .. } => (flags, body),
            _ => return,
        };
        if flags.contains(FnFlags::GENERATOR) {
            return;
        }
        let Some(method) = array_method_name(cx, f, flags.contains(FnFlags::ASYNC)) else {
            return;
        };
        let src = cx.src();
        let name = function_name_with_kind(ast, src, cx.file.parents, f);
        let full = full_name(&method);
        let for_each = method == "forEach";
        let mut returns = Returns {
            root: f,
            found: Vec::new(),
        };
        walk(ast, body, &mut returns);
        for &r in &returns.found {
            let Node::Return { arg } = *ast.node(r) else {
                continue;
            };
            if for_each {
                let Some(arg) = arg.filter(|_| self.check_for_each) else {
                    continue;
                };
                let msg = format!("{full}() expects no useless return value from {name}.");
                if self.allow_void {
                    if is_void(ast, arg) {
                        continue;
                    }
                    let fix = prepend_void(cx, arg, true);
                    cx.report_node(r, msg).id("expectedNoReturnValue").suggest(
                        "prependVoid",
                        "Prepend `void` to the expression.",
                        fix,
                    );
                } else {
                    cx.report_node(r, msg).id("expectedNoReturnValue");
                }
            } else if !self.allow_implicit && arg.is_none() {
                cx.report_node(r, format!("{full}() expects a return value from {name}."))
                    .id("expectedReturnValue");
            }
        }
        let head = function_head_span(ast, src, cx.file.parents, f);
        if for_each {
            if !self.check_for_each || !flags.contains(FnFlags::EXPRESSION_BODY) {
                return;
            }
            if self.allow_void && is_void(ast, body) {
                return;
            }
            let wrap = Fix::insert(cx.span(body).start, "{")
                .and(Span::new(cx.span(f).end, cx.span(f).end), "}");
            let void = self.allow_void.then(|| prepend_void(cx, body, false));
            let r = cx
                .report(
                    head,
                    format!("{full}() expects no useless return value from {name}."),
                )
                .id("expectedNoReturnValue")
                .suggest("wrapBraces", "Wrap the expression in `{}`.", wrap);
            if let Some(void) = void {
                r.suggest("prependVoid", "Prepend `void` to the expression.", void);
            }
        } else if matches!(ast.node(body), Node::Block { .. }) {
            let cfg = Cfg::function(ast, f);
            if !cfg.is_reachable(cfg.exit) {
                return;
            }
            let (msg, id) = if returns.found.is_empty() {
                (
                    format!("{full}() expects a return value from {name}."),
                    "expectedInside",
                )
            } else {
                (
                    format!("{full}() expects a value to be returned at the end of {name}."),
                    "expectedAtEnd",
                )
            };
            cx.report(head, msg).id(id);
        }
    }
}

impl Rule for ArrayCallbackReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionExpr, NodeKind::Arrow]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        self.check(node, cx);
    }
}
