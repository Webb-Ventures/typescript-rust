//! `@typescript-eslint/consistent-generic-constructors`: put the type arguments of a
//! `new Foo<T>()` initializer on the constructor (`constructor`, the default) or on the
//! type annotation (`type-annotation`).
//!
//! Checked on variable declarators, class fields (including `accessor` fields) and
//! defaulted parameters of function declarations and expressions (not arrows, overloads or
//! parameter properties). The callee must be a plain identifier, and an annotation, when
//! there is one, must be a reference to the same name; a global typed array (`Float32Array`
//! and friends) is left alone. The `constructor` fix moves the annotation's type arguments
//! after the callee, keeps the annotation's other comments there, and adds `()` when the
//! call has none.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-generic-constructors",
        "Enforce specifying generic type arguments on type annotation or constructor name of a constructor call",
        TS_STYLISTIC,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentGenericConstructors>,
};

#[derive(Debug, Default)]
struct ConsistentGenericConstructors {
    type_annotation: bool,
}

impl FromOptions for ConsistentGenericConstructors {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first().and_then(Value::as_str) {
            None | Some("constructor") => Ok(Self::default()),
            Some("type-annotation") => Ok(Self {
                type_annotation: true,
            }),
            Some(o) => Err(format!(
                "@typescript-eslint/consistent-generic-constructors: unknown option {o:?}"
            )),
        }
    }
}

const BUILT_IN_ARRAYS: &[&str] = &[
    "Float32Array",
    "Float64Array",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
];

fn unparen_type(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *cx.node(id) {
            Node::ParenType { ty } => id = ty,
            _ => break,
        }
    }
    id
}

impl Rule for ConsistentGenericConstructors {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDeclarator,
            NodeKind::PropertyDef,
            NodeKind::Param,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        // The report span, the node the annotation goes after, the annotation and the value.
        let (report, after, type_ann, value) = match *ast.node(node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init: Some(init),
                ..
            } => (cx.span(node), cx.span(name).end, type_ann, init),
            Node::PropertyDef {
                key,
                type_ann,
                value: Some(value),
                ..
            } => (cx.span(node), cx.span(key).end, type_ann, value),
            Node::Param {
                modifiers,
                pattern,
                type_ann,
                init: Some(init),
                ..
            } => {
                let param_prop = Modifiers::PUBLIC
                    | Modifiers::PRIVATE
                    | Modifiers::PROTECTED
                    | Modifiers::READONLY
                    | Modifiers::OVERRIDE;
                if modifiers.intersects(param_prop) {
                    return;
                }
                let in_fn = cx.parent(node).is_some_and(|p| {
                    matches!(
                        cx.node(p),
                        Node::FunctionDecl { body: Some(_), .. }
                            | Node::FunctionExpr { body: Some(_), .. }
                    )
                });
                if !in_fn {
                    return;
                }
                let span = Span::new(cx.span(pattern).start, cx.span(init).end);
                (span, cx.span(pattern).end, type_ann, init)
            }
            _ => return,
        };
        let value = skip_parens(ast, value);
        let Node::New {
            callee,
            type_args: new_args,
            has_args,
            ..
        } = *ast.node(value)
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Some(callee_name) = ident_name(ast, callee) else {
            return;
        };
        let mut lhs_args = None;
        if let Some(t) = type_ann {
            let Node::TypeRef { name, type_args } = *cx.node(unparen_type(cx, t)) else {
                return;
            };
            if ident_name(ast, name) != Some(callee_name) {
                return;
            }
            if BUILT_IN_ARRAYS.contains(&callee_name) && !cx.is_shadowed(name, callee_name) {
                return;
            }
            lhs_args = type_args;
        }
        let callee_span = cx.span(callee);
        if self.type_annotation {
            let (None, Some(args)) = (type_ann, new_args) else {
                return;
            };
            let args_span = cx.span(args);
            let text = format!(": {}{}", callee_span.text(src), args_span.text(src));
            let fix = Fix::delete(args_span).and(Span::new(after, after), text);
            cx.report(
                report,
                "The generic type arguments should be specified as part of the type annotation.",
            )
            .id("preferTypeAnnotation")
            .fix(fix);
            return;
        }
        let (Some(t), Some(args), None) = (type_ann, lhs_args, new_args) else {
            return;
        };
        if cx.env().isolated_declarations {
            return;
        }
        let t_span = cx.span(t);
        let colon = skip_trivia_backward(ast, src, t_span.start as usize).saturating_sub(1) as u32;
        if src.as_bytes().get(colon as usize) != Some(&b':') {
            return;
        }
        let ann = Span::new(colon, t_span.end);
        let args_span = cx.span(args);
        let mut text = String::new();
        for c in &ast.comments {
            let s = c.span;
            let in_args = s.start >= args_span.start && s.end <= args_span.end;
            if s.start >= ann.start && s.end <= ann.end && !in_args {
                text.push_str(s.text(src));
            }
        }
        text.push_str(args_span.text(src));
        if !has_args {
            text.push_str("()");
        }
        let at = callee_span.end;
        let fix = Fix::delete(ann).and(Span::new(at, at), text);
        cx.report(
            report,
            "The generic type arguments should be specified as part of the constructor type arguments.",
        )
        .id("preferConstructor")
        .fix(fix);
    }
}
