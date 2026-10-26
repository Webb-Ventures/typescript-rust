//! `no-empty-function`: disallow functions whose body is an empty block without comments
//! (shared with `@typescript-eslint/no-empty-function`).
//!
//! The `allow` option names kinds: `functions`, `arrowFunctions`, `methods`, `getters`,
//! `setters`, `constructors`, with `generator`/`async` prefixes (`asyncMethods`), plus
//! `privateConstructors` / `protectedConstructors` (typescript-eslint also spells them
//! `private-constructors` / `protected-constructors`), `decoratedFunctions` and
//! `overrideMethods` for class methods. A constructor with parameter properties is never
//! reported. The report covers the body and suggests a comment inside it.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-empty-function",
        "Disallow empty functions",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<NoEmptyFunction>,
};

#[derive(Debug, Default)]
pub(crate) struct NoEmptyFunction {
    allow: Vec<String>,
}

impl FromOptions for NoEmptyFunction {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoEmptyFunction {
            allow: str_list(obj_opt(opts, 0), "allow"),
        })
    }
}

impl NoEmptyFunction {
    fn allows(&self, k: &str) -> bool {
        self.allow.iter().any(|a| a == k)
    }

    /// The `allow` kind of function `f` (ESLint's `getKind`).
    fn kind(cx: &Ctx<'_, '_>, f: NodeId) -> String {
        let ast = cx.ast();
        let flags = match *ast.node(f) {
            Node::Arrow { .. } => return "arrowFunctions".into(),
            Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. } => flags,
            _ => FnFlags::default(),
        };
        let base = match cx.parent(f).map(|p| *cx.node(p)) {
            Some(Node::MethodDef { kind, value, .. }) if value == f => match kind {
                MethodKind::Get => return "getters".into(),
                MethodKind::Set => return "setters".into(),
                MethodKind::Constructor => return "constructors".into(),
                MethodKind::Method => "methods",
            },
            _ => "functions",
        };
        let prefix = if flags.contains(FnFlags::GENERATOR) {
            "generator"
        } else if flags.contains(FnFlags::ASYNC) {
            "async"
        } else {
            return base.into();
        };
        let mut cs = base.chars();
        let first = cs.next().map(|c| c.to_ascii_uppercase()).unwrap_or('M');
        format!("{prefix}{first}{}", cs.as_str())
    }

    fn allowed(&self, cx: &Ctx<'_, '_>, f: NodeId) -> bool {
        let ast = cx.ast();
        let kind = Self::kind(cx, f);
        if self.allows(&kind) {
            return true;
        }
        let Some(Node::MethodDef {
            decorators,
            modifiers,
            value,
            ..
        }) = cx.parent(f).map(|p| *cx.node(p))
        else {
            return false;
        };
        if value != f {
            return false;
        }
        if kind == "constructors" {
            let private = modifiers.contains(Modifiers::PRIVATE);
            let protected = modifiers.contains(Modifiers::PROTECTED);
            let param_props = match *ast.node(f) {
                Node::FunctionExpr { params, .. } => ast.list(params).iter().any(|&p| {
                    matches!(*ast.node(p), Node::Param { modifiers, .. } if modifiers.intersects(
                        Modifiers::PUBLIC
                            .union(Modifiers::PRIVATE)
                            .union(Modifiers::PROTECTED)
                            .union(Modifiers::READONLY)
                            .union(Modifiers::OVERRIDE)
                    ))
                }),
                _ => false,
            };
            return (private
                && (self.allows("privateConstructors") || self.allows("private-constructors")))
                || (protected
                    && (self.allows("protectedConstructors")
                        || self.allows("protected-constructors")))
                || param_props;
        }
        (!ast.list(decorators).is_empty() && self.allows("decoratedFunctions"))
            || (modifiers.contains(Modifiers::OVERRIDE) && self.allows("overrideMethods"))
    }
}

impl Rule for NoEmptyFunction {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Arrow,
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let body = match *ast.node(node) {
            Node::Arrow { flags, body, .. } if !flags.contains(FnFlags::EXPRESSION_BODY) => body,
            Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => {
                b
            }
            _ => return,
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let span = cx.span(body);
        if !ast.list(stmts).is_empty() || !ast.comments_in(span).is_empty() {
            return;
        }
        if self.allowed(cx, node) {
            return;
        }
        let mut name = function_name_with_kind(ast, cx.src(), cx.file.parents, node);
        // ESLint names a function by its own name or key, never by the variable it is
        // assigned to.
        let anonymous = match *ast.node(node) {
            Node::FunctionExpr { name, .. } => name.is_none(),
            Node::Arrow { .. } => true,
            _ => false,
        };
        if anonymous
            && cx.parent(node).is_some_and(|p| {
                matches!(cx.node(p), Node::VarDeclarator { .. } | Node::Assign { .. })
            })
            && let Some(i) = name.find(" '")
        {
            name.truncate(i);
        }
        let inner = Span::new(
            span.start + 1,
            span.end.saturating_sub(1).max(span.start + 1),
        );
        cx.report(span, format!("Unexpected empty {name}."))
            .id("unexpected")
            .suggest(
                "suggestComment",
                format!("Add comment inside empty {name}."),
                Fix::replace(inner, " /* empty */ "),
            );
    }
}
