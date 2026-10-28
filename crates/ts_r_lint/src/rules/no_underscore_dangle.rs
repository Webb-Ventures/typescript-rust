//! `no-underscore-dangle`: identifiers that start or end with `_`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-underscore-dangle",
        "Disallow dangling underscores in identifiers",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoUnderscoreDangle>,
};

#[derive(Debug, Default)]
struct NoUnderscoreDangle {
    allow: Vec<String>,
    after_super: bool,
    after_this: bool,
    after_this_constructor: bool,
    function_params: bool,
    array_destructuring: bool,
    object_destructuring: bool,
    class_fields: bool,
    method_names: bool,
}

impl FromOptions for NoUnderscoreDangle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnderscoreDangle {
            allow: o
                .get("allow")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            after_super: bool_field(o, "allowAfterSuper", false),
            after_this: bool_field(o, "allowAfterThis", false),
            after_this_constructor: bool_field(o, "allowAfterThisConstructor", false),
            function_params: bool_field(o, "allowFunctionParams", true),
            array_destructuring: bool_field(o, "allowInArrayDestructuring", true),
            object_destructuring: bool_field(o, "allowInObjectDestructuring", true),
            class_fields: bool_field(o, "enforceInClassFields", false),
            method_names: bool_field(o, "enforceInMethodNames", false),
        })
    }
}

fn dangling(name: &str) -> bool {
    name != "_" && (name.starts_with('_') || name.ends_with('_'))
}

/// The name of an identifier or private name (without `#`).
fn plain_name<'a>(cx: &Ctx<'a, '_>, id: NodeId) -> Option<&'a str> {
    match *cx.node(id) {
        Node::Ident { .. } => ident_name(cx.ast(), id),
        Node::PrivateName { name } => Some(cx.ast().atom(name)),
        _ => None,
    }
}

impl NoUnderscoreDangle {
    fn flagged(&self, name: &str) -> bool {
        dangling(name) && !self.allow.iter().any(|a| a == name)
    }

    fn report(cx: &mut Ctx<'_, '_>, span: Span, name: &str) {
        cx.report(span, format!("Unexpected dangling '_' in '{name}'."))
            .id("unexpectedUnderscore");
    }

    fn check_params(&self, cx: &mut Ctx<'_, '_>, params: ts_r_ast::NodeList) {
        if self.function_params {
            return;
        }
        let ast = cx.ast();
        for &p in ast.list(params) {
            let Node::Param {
                modifiers, pattern, ..
            } = *cx.node(p)
            else {
                continue;
            };
            if modifiers.intersects(
                Modifiers::PUBLIC
                    | Modifiers::PRIVATE
                    | Modifiers::PROTECTED
                    | Modifiers::READONLY
                    | Modifiers::OVERRIDE,
            ) || !matches!(cx.node(pattern), Node::Ident { .. })
            {
                continue;
            }
            if let Some(name) = ident_name(ast, pattern)
                && self.flagged(name)
            {
                Self::report(cx, cx.span(p), name);
            }
        }
    }

    /// The names a declarator declares, with the pattern each sits in directly.
    fn check_declarator(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: NodeId) {
        let ast = cx.ast();
        // (identifier, inside an array pattern, inside an object pattern)
        let mut names: Vec<(NodeId, bool, bool)> = Vec::new();
        let mut stack = vec![(name, false, false)];
        while let Some((n, in_arr, in_obj)) = stack.pop() {
            match *cx.node(n) {
                Node::Ident { .. } => names.push((n, in_arr, in_obj)),
                Node::ArrayPattern { elements } => {
                    for &e in ast.list(elements).iter().rev() {
                        stack.push((e, true, false));
                    }
                }
                Node::ObjectPattern { props } => {
                    for &e in ast.list(props).iter().rev() {
                        stack.push((e, false, true));
                    }
                }
                Node::BindingProperty { key, value, .. } => {
                    stack.push((value.unwrap_or(key), in_arr, in_obj));
                }
                Node::AssignPattern { left, .. } => stack.push((left, in_arr, in_obj)),
                Node::Rest { arg } => stack.push((arg, in_arr, in_obj)),
                _ => {}
            }
        }
        let span = cx.span(node);
        for (id, in_arr, in_obj) in names {
            let Some(n) = ident_name(ast, id) else {
                continue;
            };
            if self.flagged(n)
                && !(self.array_destructuring && in_arr)
                && !(self.object_destructuring && in_obj)
            {
                Self::report(cx, span, n);
            }
        }
    }
}

impl Rule for NoUnderscoreDangle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
            NodeKind::VarDeclarator,
            NodeKind::Member,
            NodeKind::Index,
            NodeKind::MethodDef,
            NodeKind::PropertyDef,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::FunctionDecl {
                name,
                params,
                body: Some(_),
                ..
            } => {
                if let Some(n) = name.and_then(|n| ident_name(ast, n))
                    && self.flagged(n)
                {
                    Self::report(cx, cx.span(node), n);
                }
                self.check_params(cx, params);
            }
            Node::FunctionExpr {
                params,
                body: Some(_),
                ..
            }
            | Node::Arrow { params, .. } => self.check_params(cx, params),
            Node::VarDeclarator { name, .. } => self.check_declarator(cx, node, name),
            Node::Member { obj, prop, .. }
            | Node::Index {
                obj, index: prop, ..
            } => {
                let Some(n) = plain_name(cx, prop) else {
                    return;
                };
                let obj = skip_parens(ast, obj);
                let of_this = matches!(cx.node(obj), Node::This {});
                let of_super = matches!(cx.node(obj), Node::Super {});
                let of_this_ctor = matches!(*cx.node(obj), Node::Member { obj: o, prop: p, .. }
                    if matches!(cx.node(o), Node::This {}) && ident_name(ast, p) == Some("constructor"));
                if self.flagged(n)
                    && !(of_this && self.after_this)
                    && !(of_super && self.after_super)
                    && !(of_this_ctor && self.after_this_constructor)
                    && n != "__proto__"
                {
                    Self::report(cx, cx.span(node), n);
                }
            }
            Node::MethodDef { key, kind, .. } => {
                // Object literal getters and setters are not methods.
                let in_object = cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::ObjectLit { .. }));
                if !self.method_names
                    || (in_object && matches!(kind, MethodKind::Get | MethodKind::Set))
                {
                    return;
                }
                if let Some(n) = plain_name(cx, key)
                    && self.flagged(n)
                {
                    let shown = if matches!(cx.node(key), Node::PrivateName { .. }) {
                        format!("#{n}")
                    } else {
                        n.to_string()
                    };
                    Self::report(cx, cx.span(node), &shown);
                }
            }
            Node::PropertyDef { key, modifiers, .. } => {
                if !self.class_fields || modifiers.contains(Modifiers::ACCESSOR) {
                    return;
                }
                if let Some(n) = plain_name(cx, key)
                    && self.flagged(n)
                {
                    let shown = if matches!(cx.node(key), Node::PrivateName { .. }) {
                        format!("#{n}")
                    } else {
                        n.to_string()
                    };
                    Self::report(cx, cx.span(node), &shown);
                }
            }
            _ => {}
        }
    }
}
