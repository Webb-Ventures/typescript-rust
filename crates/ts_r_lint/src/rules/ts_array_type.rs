//! `@typescript-eslint/array-type`: one spelling for array types: `T[]` (`array`),
//! `Array<T>` (`generic`), or `T[]` only for simple element types (`array-simple`), set apart
//! for readonly arrays with `readonly`.
//!
//! `readonly T[]` pairs with `ReadonlyArray<T>` (and `Readonly<T[]>`). A local declaration
//! named `Array`, `ReadonlyArray` or `Readonly` (anywhere but the global scope) turns the
//! generic check off for that name. Parentheses are added where the new spelling needs them.

use super::prelude::*;
use ts_r_ast::{Ast, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/array-type",
        "Require consistently using either `T[]` or `Array<T>` for arrays",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ArrayType>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Style {
    #[default]
    Array,
    Generic,
    Simple,
}

#[derive(Debug, Default)]
struct ArrayType {
    default: Style,
    readonly: Style,
}

fn style(v: Option<&Value>) -> Result<Option<Style>, String> {
    match v.and_then(Value::as_str) {
        None => Ok(None),
        Some("array") => Ok(Some(Style::Array)),
        Some("generic") => Ok(Some(Style::Generic)),
        Some("array-simple") => Ok(Some(Style::Simple)),
        Some(o) => Err(format!(
            "@typescript-eslint/array-type: unknown option {o:?}"
        )),
    }
}

impl FromOptions for ArrayType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first();
        let default = style(o.and_then(|o| o.get("default")))?.unwrap_or(Style::Array);
        let readonly = style(o.and_then(|o| o.get("readonly")))?.unwrap_or(default);
        Ok(ArrayType { default, readonly })
    }
}

fn unparen(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::ParenType { ty } => id = ty,
            _ => break,
        }
    }
    id
}

/// The parent of a type, looking through parentheses, and whether there were any.
fn type_parent(cx: &Ctx<'_, '_>, mut id: NodeId) -> (Option<NodeId>, bool) {
    let mut parens = false;
    for _ in 0..10_000 {
        match cx.parent(id) {
            Some(p) if matches!(cx.node(p), Node::ParenType { .. }) => {
                parens = true;
                id = p;
            }
            p => return (p, parens),
        }
    }
    (None, parens)
}

fn type_args(ast: &Ast, args: Option<NodeId>) -> Option<&[NodeId]> {
    match *ast.node(args?) {
        Node::TypeArgs { args } => Some(ast.list(args)),
        _ => None,
    }
}

fn is_simple(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 400 {
        return false;
    }
    let id = unparen(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::QualifiedName { .. } | Node::ThisType {} => true,
        Node::ArrayType { .. } => true,
        Node::KeywordType { kind } => {
            !matches!(kind, KeywordTypeKind::Intrinsic | KeywordTypeKind::Const)
        }
        Node::TypeRef {
            name,
            type_args: ta,
        } => {
            if ident_name(ast, name) == Some("Array") {
                match type_args(ast, ta) {
                    None => true,
                    Some([one]) => is_simple(ast, *one, depth + 1),
                    Some(_) => false,
                }
            } else if ta.is_some() {
                false
            } else {
                is_simple(ast, name, depth + 1)
            }
        }
        _ => false,
    }
}

fn needs_parens(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(unparen(ast, id)) {
        Node::TypeRef { name, .. } => ident_name(ast, name) == Some("ReadonlyArray"),
        Node::UnionType { .. }
        | Node::FunctionType { .. }
        | Node::IntersectionType { .. }
        | Node::TypeOperator { .. }
        | Node::InferType { .. }
        | Node::ConstructorType { .. }
        | Node::ConditionalType { .. } => true,
        _ => false,
    }
}

impl ArrayType {
    fn message_type(cx: &Ctx<'_, '_>, id: NodeId) -> String {
        if is_simple(cx.ast(), id, 0) {
            cx.text(unparen(cx.ast(), id)).to_owned()
        } else {
            "T".to_owned()
        }
    }

    fn array(&self, cx: &mut Ctx<'_, '_>, node: NodeId, elem: NodeId) {
        let ast = cx.ast();
        let (parent, _) = type_parent(cx, node);
        let readonly_op = parent.filter(|&p| {
            matches!(
                ast.node(p),
                Node::TypeOperator {
                    op: TypeOperatorKind::Readonly,
                    ..
                }
            )
        });
        let current = if readonly_op.is_some() {
            self.readonly
        } else {
            self.default
        };
        if current == Style::Array || (current == Style::Simple && is_simple(ast, elem, 0)) {
            return;
        }
        let (id, msg) = if current == Style::Generic {
            ("errorStringGeneric", "is forbidden")
        } else {
            (
                "errorStringGenericSimple",
                "is forbidden for non-simple types",
            )
        };
        let error = readonly_op.unwrap_or(node);
        let (class, prefix) = if readonly_op.is_some() {
            ("ReadonlyArray", "readonly ")
        } else {
            ("Array", "")
        };
        let ty = Self::message_type(cx, elem);
        let elem_span = cx.span(unparen(ast, elem));
        let error_span = cx.span(error);
        let fix = Fix::replace(
            Span::new(error_span.start, elem_span.start),
            format!("{class}<"),
        )
        .and(Span::new(elem_span.end, error_span.end), ">");
        cx.report(
            error_span,
            format!("Array type using '{prefix}{ty}[]' {msg}. Use '{class}<{ty}>' instead."),
        )
        .id(id)
        .fix(fix);
    }

    fn reference(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: NodeId, ta: Option<NodeId>) {
        let ast = cx.ast();
        let Some(class) = ident_name(ast, name) else {
            return;
        };
        let args = type_args(ast, ta);
        let first_is_array = args
            .and_then(|a| a.first())
            .is_some_and(|&a| matches!(ast.node(unparen(ast, a)), Node::ArrayType { .. }));
        match class {
            "Array" | "ReadonlyArray" => {}
            "Readonly" if first_is_array => {}
            _ => return,
        }
        // A local declaration of the name (outside the global scope) shadows the global.
        let sem = cx.semantic();
        let scope = sem.enclosing_scope(node);
        if sem
            .lookup(scope, class)
            .is_some_and(|s| sem.symbol(s).scope != sem.global_scope())
        {
            return;
        }
        let readonly_generic = class == "Readonly";
        let readonly = class == "ReadonlyArray" || readonly_generic;
        let current = if readonly {
            self.readonly
        } else {
            self.default
        };
        if current == Style::Generic {
            return;
        }
        let prefix = if readonly { "readonly " } else { "" };
        let (id, simple) = match current {
            Style::Array if readonly_generic => ("errorStringArrayReadonly", false),
            Style::Array => ("errorStringArray", false),
            _ if readonly && class != "ReadonlyArray" => ("errorStringArraySimpleReadonly", true),
            _ => ("errorStringArraySimple", true),
        };
        let Some(&[ty]) = args else {
            return;
        };
        if current == Style::Simple && !is_simple(ast, ty, 0) {
            return;
        }
        let ty_parens = needs_parens(ast, ty);
        let parent_parens = readonly
            && matches!(type_parent(cx, node), (Some(p), false) if matches!(ast.node(p), Node::ArrayType { .. }));
        let start = format!(
            "{}{prefix}{}",
            if parent_parens { "(" } else { "" },
            if ty_parens { "(" } else { "" }
        );
        let end = format!(
            "{}{}{}",
            if ty_parens { ")" } else { "" },
            if readonly_generic { "" } else { "[]" },
            if parent_parens { ")" } else { "" }
        );
        let shown = Self::message_type(cx, ty);
        let class_shown = if readonly { class } else { "Array" };
        let simple_text = if simple { " for simple types" } else { "" };
        let suggested = if readonly_generic {
            format!("{prefix}{shown}")
        } else {
            format!("{prefix}{shown}[]")
        };
        let node_span = cx.span(node);
        let ty_span = cx.span(unparen(ast, ty));
        let fix = Fix::replace(Span::new(node_span.start, ty_span.start), start)
            .and(Span::new(ty_span.end, node_span.end), end);
        cx.report(
            node_span,
            format!(
                "Array type using '{class_shown}<{shown}>' is forbidden{simple_text}. Use '{suggested}' instead."
            ),
        )
        .id(id)
        .fix(fix);
    }
}

impl Rule for ArrayType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayType, NodeKind::TypeRef]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::ArrayType { elem } => self.array(cx, node, elem),
            Node::TypeRef { name, type_args } => self.reference(cx, node, name, type_args),
            _ => {}
        }
    }
}
