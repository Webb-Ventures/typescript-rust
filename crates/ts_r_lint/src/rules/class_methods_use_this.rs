//! `class-methods-use-this`: instance methods (and, by default, function-valued fields) that
//! use neither `this` nor `super`. Shared with `@typescript-eslint/class-methods-use-this`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "class-methods-use-this",
        "Enforce that class methods utilize `this`",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ClassMethodsUseThis>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IgnoreImplements {
    No,
    All,
    PublicFields,
}

#[derive(Debug)]
pub(crate) struct ClassMethodsUseThis {
    enforce_for_class_fields: bool,
    except_methods: Vec<String>,
    ignore_override: bool,
    ignore_implements: IgnoreImplements,
}

impl FromOptions for ClassMethodsUseThis {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        // `ignoreClassesWithImplements` (core) / `ignoreClassesThatImplementAnInterface` (ts).
        let ignore_implements = match o
            .get("ignoreClassesWithImplements")
            .or_else(|| o.get("ignoreClassesThatImplementAnInterface"))
        {
            Some(Value::String(s)) if s == "public-fields" => IgnoreImplements::PublicFields,
            Some(Value::String(s)) if s == "all" => IgnoreImplements::All,
            Some(Value::Bool(true)) => IgnoreImplements::All,
            _ => IgnoreImplements::No,
        };
        Ok(ClassMethodsUseThis {
            enforce_for_class_fields: bool_field(o, "enforceForClassFields", true),
            except_methods: o
                .get("exceptMethods")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            ignore_override: bool_field(o, "ignoreOverrideMethods", false),
            ignore_implements,
        })
    }
}

/// `this` or `super` used by `f` itself (not by a nested function, field or static block).
fn uses_this(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = Vec::new();
    ast.for_each_child(f, |c| stack.push(c));
    while let Some(n) = stack.pop() {
        match *cx.node(n) {
            Node::This {} | Node::Super {} => return true,
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::StaticBlock { .. } => {
                continue;
            }
            // A nested field's key belongs to the enclosing code, its value doesn't.
            Node::PropertyDef { key, .. } => {
                stack.push(key);
                continue;
            }
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

impl Rule for ClassMethodsUseThis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionExpr, NodeKind::Arrow]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ts = cx.rule_name().starts_with("@typescript-eslint/");
        let ast = cx.ast();
        let arrow = matches!(cx.node(node), Node::Arrow { .. });
        if matches!(cx.node(node), Node::FunctionExpr { body: None, .. }) {
            return;
        }
        let Some(member) = cx.parent(node) else {
            return;
        };
        let (key, modifiers, field) = match *cx.node(member) {
            Node::MethodDef {
                key,
                modifiers,
                kind,
                value,
                ..
            } if value == node && !arrow => {
                if kind == MethodKind::Constructor {
                    return;
                }
                (key, modifiers, false)
            }
            Node::PropertyDef {
                key,
                modifiers,
                value: Some(v),
                ..
            } if v == node => (key, modifiers, true),
            _ => return,
        };
        let Some(class) = cx.parent(member) else {
            return;
        };
        let heritage = match *cx.node(class) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return,
        };
        let accessor = field && modifiers.contains(Modifiers::ACCESSOR);
        if modifiers.contains(Modifiers::STATIC) || (field && !self.enforce_for_class_fields) {
            return;
        }
        if self.ignore_override && modifiers.contains(Modifiers::OVERRIDE) {
            return;
        }
        let implements = ast.list(heritage).iter().any(|&h| {
            matches!(*cx.node(h), Node::HeritageClause { kind: ts_r_ast::HeritageKind::Implements, types } if !ast.list(types).is_empty())
        });
        let private_key = matches!(cx.node(key), Node::PrivateName { .. });
        if implements {
            match self.ignore_implements {
                IgnoreImplements::All => return,
                IgnoreImplements::PublicFields
                    if !modifiers.intersects(Modifiers::PRIVATE | Modifiers::PROTECTED)
                        && (ts || !private_key) =>
                {
                    return;
                }
                _ => {}
            }
        }
        if !matches!(cx.node(key), Node::ComputedKey { .. }) {
            let name = match *cx.node(key) {
                Node::PrivateName { name } => format!("#{}", ast.atom(name)),
                Node::Ident { .. } => ident_name(ast, key).unwrap_or_default().to_string(),
                _ => static_value_string(ast, cx.src(), key)
                    .map(|s| s.into_owned())
                    .unwrap_or_default(),
            };
            if self.except_methods.contains(&name) {
                return;
            }
        }
        if uses_this(cx, node) {
            return;
        }
        let mut head = function_head_span(ast, cx.src(), cx.file.parents, node);
        let mut name = super::func_names::own_name_with_kind(cx, node);
        // ESLint names a field's function after the field ("method 'x'") and reports from the
        // field to the `(`; an `accessor` field is not a field there, so neither applies.
        if field && !(accessor && arrow) {
            let span = ast.span(node);
            let start = if accessor {
                span.start
            } else {
                ast.span(member).start
            };
            let paren = cx
                .src()
                .get(span.start as usize..span.end as usize)
                .and_then(|t| t.find('('))
                .map_or(span.start, |i| span.start + i as u32);
            head = Span::new(start, paren);
        }
        if accessor {
            name = match name.rfind(" '").or_else(|| name.rfind(" #")) {
                Some(i) if !matches!(cx.node(node), Node::FunctionExpr { name: Some(_), .. }) => {
                    name[..i].to_string()
                }
                _ => name,
            }
            .replace("private ", "")
            .replace("method", "function");
        } else if field && arrow {
            name = name.replacen("arrow function", "method", 1);
        }
        cx.report(head, format!("Expected 'this' to be used by class {name}."))
            .id("missingThis");
    }
}
