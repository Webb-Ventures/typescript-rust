//! `unicorn/no-null`: use `undefined` instead of `null`.
//!
//! Not reported: `null` compared with `===`/`!==` (unless `checkStrictEquality`), a direct
//! call or `new` argument when `checkArguments` is off, the first argument of
//! `Object.create(null[, x])`, `useRef(null)` / `React.useRef(null)`, and the second argument
//! of `x.insertBefore(y, null)`. Under `delete` (through TS wrappers) it is reported without a
//! fix. Next to `==`/`!=` it is fixed to `undefined`; elsewhere `undefined` is a suggestion,
//! and after `return` or as the initialiser of a plain `let`/`var` name a "remove" suggestion
//! (deleting from the keyword or name to the end of the parenthesized `null`) comes first
//! when no comment is in that range.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-null",
        "Disallow the use of the `null` literal.",
        NO_PRESET,
        Category::Restriction,
    )
    .fixable()
    .suggestions(),
    create: create::<NoNull>,
};

#[derive(Debug, Default)]
struct NoNull {
    check_arguments: bool,
    check_strict_equality: bool,
}

impl FromOptions for NoNull {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoNull {
            check_arguments: bool_field(o, "checkArguments", true),
            check_strict_equality: bool_field(o, "checkStrictEquality", false),
        })
    }
}

impl Rule for NoNull {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NullLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parents = cx.file.parents;
        let parent = parent_skip_parens(ast, parents, node);
        let parent_node = parent.map(|p| ast.node(p));
        match parent_node {
            Some(Node::Binary { op, .. })
                if !self.check_strict_equality
                    && matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq) =>
            {
                return;
            }
            Some(&Node::Call {
                callee,
                optional,
                args,
                ..
            }) => {
                let args = ast.list(args);
                let index = args.iter().position(|&a| skip_parens(ast, a) == node);
                if index.is_some() && !self.check_arguments {
                    return;
                }
                if allowed_call(ast, callee, optional, args, index) {
                    return;
                }
            }
            Some(&Node::New { args, .. }) => {
                if !self.check_arguments
                    && ast.list(args).iter().any(|&a| skip_parens(ast, a) == node)
                {
                    return;
                }
            }
            _ => {}
        }
        let span = cx.span(node);
        let replace = Fix::replace(span, "undefined");
        let message = "Use `undefined` instead of `null`.";
        if under_delete(cx, node) {
            cx.report(span, message).id("error");
            return;
        }
        if let Some(Node::Binary {
            op: BinaryOp::Eq | BinaryOp::NotEq,
            ..
        }) = parent_node
        {
            cx.report(span, message).id("error").fix(replace);
            return;
        }
        let remove_from = match (parent, parent_node) {
            (Some(p), Some(Node::Return { .. })) => Some(cx.span(p).start + "return".len() as u32),
            (
                Some(p),
                Some(&Node::VarDeclarator {
                    name,
                    type_ann,
                    init: Some(_),
                    ..
                }),
            ) if matches!(ast.node(name), Node::Ident { .. })
                && cx.parent(p).is_some_and(|d| {
                    matches!(
                        ast.node(d),
                        Node::VarDecl {
                            kind: VarKind::Let | VarKind::Var,
                            ..
                        }
                    )
                }) =>
            {
                Some(type_ann.map_or(cx.span(name).end, |t| cx.span(t).end))
            }
            _ => None,
        };
        let remove = remove_from.and_then(|start| {
            let end = parenthesized_span(cx, node).end;
            let range = Span::new(start, end);
            ast.comments_in(range).is_empty().then_some(range)
        });
        let mut report = cx.report(span, message).id("error");
        if let Some(range) = remove {
            report = report.suggest("remove", "Remove `null`.", Fix::delete(range));
        }
        report.suggest("replace", "Replace `null` with `undefined`.", replace);
    }
}

/// `Object.create(null[, x])`, `useRef(null)`, `React.useRef(null)`, `x.insertBefore(y, null)`.
fn allowed_call(
    ast: &ts_r_ast::Ast,
    callee: NodeId,
    optional: bool,
    args: &[NodeId],
    index: Option<usize>,
) -> bool {
    if optional {
        return false;
    }
    let spread = |n: usize| {
        args.iter()
            .take(n)
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    };
    let callee = skip_parens(ast, callee);
    let method = match *ast.node(callee) {
        Node::Member {
            obj,
            optional,
            prop,
        } => Some((
            ident_name(ast, skip_parens(ast, obj)),
            optional,
            ident_name(ast, prop),
        )),
        _ => None,
    };
    match (index, method) {
        (Some(0), Some((Some("Object"), false, Some("create")))) => {
            (1..=2).contains(&args.len()) && !spread(2)
        }
        (Some(0), Some((Some("React"), false, Some("useRef")))) => args.len() == 1 && !spread(1),
        (Some(0), None) => {
            ident_name(ast, callee) == Some("useRef") && args.len() == 1 && !spread(1)
        }
        (Some(1), Some((_, _, Some("insertBefore")))) => args.len() == 2 && !spread(2),
        _ => false,
    }
}

/// `delete null`, through parentheses and TS wrappers.
fn under_delete(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        match ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            _ => return false,
        }
    }
    false
}

/// The span of `node` with the parentheses around it.
fn parenthesized_span(cx: &Ctx<'_, '_>, node: NodeId) -> Span {
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        cur = p;
    }
    cx.span(cur)
}
