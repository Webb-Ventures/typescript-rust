//! `func-style`: function declarations (`declaration`) or `var` function expressions
//! (`expression`, the default), with overrides for named exports.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "func-style",
        "Enforce the consistent use of either `function` declarations or expressions assigned \
         to variables",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<FuncStyle>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Export {
    #[default]
    Unset,
    Declaration,
    Expression,
    Ignore,
}

#[derive(Debug, Default)]
struct FuncStyle {
    declarations: bool,
    allow_arrows: bool,
    allow_type_annotation: bool,
    exports: Export,
}

impl FromOptions for FuncStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 1);
        let exports = match o
            .get("overrides")
            .and_then(|v| v.get("namedExports"))
            .and_then(Value::as_str)
        {
            Some("declaration") => Export::Declaration,
            Some("expression") => Export::Expression,
            Some("ignore") => Export::Ignore,
            _ => Export::Unset,
        };
        Ok(FuncStyle {
            declarations: opts.first().and_then(Value::as_str) == Some("declaration"),
            allow_arrows: bool_field(o, "allowArrowFunctions", false),
            allow_type_annotation: bool_field(o, "allowTypeAnnotation", false),
            exports,
        })
    }
}

/// A `this` or `super` that belongs to the arrow `f` (not to a nested function).
fn uses_this(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = Vec::new();
    ast.for_each_child(f, |c| stack.push(c));
    while let Some(n) = stack.pop() {
        match ast.node(n) {
            Node::This {} | Node::Super {} => return true,
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => continue,
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

impl FuncStyle {
    /// An overload signature (`function f(): void;`) of the same name next to `f`.
    fn overloaded(&self, cx: &Ctx<'_, '_>, f: NodeId, name: &str) -> bool {
        let ast = cx.ast();
        let Some(p) = cx.parent(f) else {
            return false;
        };
        let mut lists: Vec<&[NodeId]> = Vec::new();
        if let Node::SwitchCase { .. } = ast.node(p) {
            if let Some(sw) = cx.parent(p)
                && let Node::Switch { cases, .. } = *ast.node(sw)
            {
                for &c in ast.list(cases) {
                    if let Node::SwitchCase { body, .. } = *ast.node(c) {
                        lists.push(ast.list(body));
                    }
                }
            }
        } else if let Some(l) = statement_list(ast, p) {
            lists.push(l);
        }
        let exported =
            |m: Modifiers| m.contains(Modifiers::EXPORT) && !m.contains(Modifiers::DEFAULT);
        let own_export =
            matches!(*ast.node(f), Node::FunctionDecl { modifiers, .. } if exported(modifiers));
        lists.into_iter().flatten().any(|&s| match *ast.node(s) {
            Node::FunctionDecl {
                body: None,
                name: Some(n),
                modifiers,
                ..
            } => exported(modifiers) == own_export && ident_name(ast, n) == Some(name),
            _ => false,
        })
    }

    /// `f`, the value of a declarator: report the declarator if it should be a declaration.
    fn check_declarator(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let ast = cx.ast();
        let Some(d) = cx.parent(f) else {
            return;
        };
        let Node::VarDeclarator {
            init: Some(i),
            type_ann,
            ..
        } = *ast.node(d)
        else {
            return;
        };
        if i != f || (self.allow_type_annotation && type_ann.is_some()) {
            return;
        }
        let exported = cx.parent(d).is_some_and(|v| {
            matches!(*ast.node(v), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT))
        });
        let report = if exported {
            match self.exports {
                Export::Unset => self.declarations,
                Export::Declaration => true,
                _ => false,
            }
        } else {
            self.declarations
        };
        if report {
            cx.report_node(d, "Expected a function declaration.")
                .id("declaration");
        }
    }
}

impl Rule for FuncStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::FunctionDecl {
                modifiers,
                name,
                body: Some(_),
                ..
            } => {
                if modifiers.contains(Modifiers::DEFAULT) {
                    return;
                }
                let exported = modifiers.contains(Modifiers::EXPORT);
                let report = if exported {
                    match self.exports {
                        Export::Unset => !self.declarations,
                        Export::Expression => true,
                        _ => false,
                    }
                } else {
                    !self.declarations
                };
                let name = name.and_then(|n| ident_name(ast, n)).unwrap_or("");
                if !report || self.overloaded(cx, node, name) {
                    return;
                }
                let mut span = cx.span(node);
                if exported && cx.text(node).starts_with("export") {
                    span.start = skip_trivia_forward(cx.src(), span.start as usize + 6) as u32;
                }
                cx.report(span, "Expected a function expression.")
                    .id("expression");
            }
            Node::FunctionExpr { .. } => self.check_declarator(cx, node),
            Node::Arrow { .. } if !self.allow_arrows && !uses_this(cx, node) => {
                self.check_declarator(cx, node)
            }
            _ => {}
        }
    }
}
