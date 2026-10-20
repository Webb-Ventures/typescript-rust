//! `no-useless-constructor`: disallow unnecessary constructors. Also hosts
//! `@typescript-eslint/no-useless-constructor`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-constructor",
        "Disallow unnecessary constructors",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoUselessConstructor>,
};

#[derive(Debug, Default)]
pub(crate) struct NoUselessConstructor;

/// `extends` in a class heritage list.
fn has_extends(ast: &ts_r_ast::Ast, heritage: ts_r_ast::NodeList) -> bool {
    ast.list(heritage).iter().any(|&h| {
        matches!(
            ast.node(h),
            Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                ..
            }
        )
    })
}

/// The constructor only forwards its parameters to `super`.
fn passes_through(ast: &ts_r_ast::Ast, params: &[NodeId], args: &[NodeId]) -> bool {
    let simple: Option<Vec<(bool, &str)>> = params
        .iter()
        .map(|&p| match *ast.node(p) {
            Node::Param {
                rest,
                pattern,
                init: None,
                ..
            } => ident_name(ast, pattern).map(|n| (rest, n)),
            _ => None,
        })
        .collect();
    let Some(simple) = simple else {
        return false;
    };
    // `super(...arguments)`
    if let [a] = args
        && let Node::Spread { arg } = *ast.node(*a)
        && ident_name(ast, arg) == Some("arguments")
    {
        return true;
    }
    simple.len() == args.len()
        && simple.iter().zip(args).all(|(&(rest, name), &a)| {
            let a = if rest {
                match *ast.node(a) {
                    Node::Spread { arg } => arg,
                    _ => return false,
                }
            } else {
                a
            };
            ident_name(ast, a) == Some(name)
        })
}

impl Rule for NoUselessConstructor {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::MethodDef {
            kind: MethodKind::Constructor,
            modifiers,
            key,
            value,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::FunctionExpr {
            params,
            body: Some(body),
            ..
        } = *cx.node(value)
        else {
            return;
        };
        if modifiers.intersects(Modifiers::PRIVATE | Modifiers::PROTECTED) {
            return;
        }
        let params = ast.list(params);
        let decorated_or_prop = params.iter().any(|&p| match *ast.node(p) {
            Node::Param {
                decorators,
                modifiers,
                ..
            } => !ast.list(decorators).is_empty() || !modifiers.is_empty(),
            _ => false,
        });
        if decorated_or_prop {
            return;
        }
        let Some(class) = cx.parent(node) else {
            return;
        };
        let heritage = match *cx.node(class) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return,
        };
        let Node::Block { stmts } = *cx.node(body) else {
            return;
        };
        let stmts = ast.list(stmts);
        let useless = if has_extends(ast, heritage) {
            // A public constructor may widen a protected parent constructor.
            if modifiers.contains(Modifiers::PUBLIC) {
                return;
            }
            match stmts {
                [s] => match *ast.node(*s) {
                    Node::ExprStmt { expr } => match *ast.node(expr) {
                        Node::Call { callee, args, .. }
                            if matches!(ast.node(callee), Node::Super {}) =>
                        {
                            passes_through(ast, params, ast.list(args))
                        }
                        _ => false,
                    },
                    _ => false,
                },
                _ => false,
            }
        } else {
            stmts.is_empty()
        };
        if !useless {
            return;
        }
        let span = Span::new(cx.span(node).start, cx.span(key).end);
        let fix = Fix::replace(cx.span(node), removal_text(cx, node));
        cx.report(span, "Useless constructor.")
            .id("noUselessConstructor")
            .suggest("removeConstructor", "Remove the constructor.", fix);
    }
}

/// `;` when removing `member` would let a field initializer before it run into the next
/// member (`foo = a` followed by `[0]() {}`).
fn removal_text(cx: &Ctx<'_, '_>, member: NodeId) -> &'static str {
    let ast = cx.ast();
    let Some(class) = cx.parent(member) else {
        return "";
    };
    let members = match *cx.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
        _ => return "",
    };
    let Some(i) = members.iter().position(|&m| m == member) else {
        return "";
    };
    let prev_init = i.checked_sub(1).is_some_and(|j| {
        let p = members[j];
        matches!(cx.node(p), Node::PropertyDef { value: Some(_), .. })
            && !cx.text(p).trim_end().ends_with(';')
    });
    if !prev_init {
        return "";
    }
    let src = cx.src();
    let next = skip_trivia_forward(src, cx.span(member).end as usize);
    let rest = &src[next..];
    let word = |w: &str| {
        rest.strip_prefix(w)
            .is_some_and(|r| !r.starts_with(|c: char| is_ident_char(c)))
    };
    if rest.starts_with(['[', '(', '*']) || word("in") || word("instanceof") {
        ";"
    } else {
        ""
    }
}
