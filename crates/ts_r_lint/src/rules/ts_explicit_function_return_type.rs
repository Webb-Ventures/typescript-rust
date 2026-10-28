//! `@typescript-eslint/explicit-function-return-type`: functions and methods declare their
//! return type. The helpers here (typed contexts, higher-order functions) are shared with
//! `explicit-module-boundary-types`.

use super::prelude::*;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/explicit-function-return-type",
        "Require explicit return types on functions and class methods",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ExplicitFunctionReturnType>,
};

/// The options both rules share.
#[derive(Debug)]
pub(crate) struct ReturnOpts {
    pub(crate) allow_direct_const: bool,
    pub(crate) allow_expressions: bool,
    pub(crate) allow_higher_order: bool,
    pub(crate) allow_typed: bool,
}

#[derive(Debug)]
struct ExplicitFunctionReturnType {
    opts: ReturnOpts,
    allow_void: bool,
    allowed_names: Vec<String>,
    allow_without_type_params: bool,
    allow_iifes: bool,
}

impl FromOptions for ExplicitFunctionReturnType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(ExplicitFunctionReturnType {
            opts: ReturnOpts {
                allow_direct_const: bool_field(
                    o,
                    "allowDirectConstAssertionInArrowFunctions",
                    true,
                ),
                allow_expressions: bool_field(o, "allowExpressions", false),
                allow_higher_order: bool_field(o, "allowHigherOrderFunctions", true),
                allow_typed: bool_field(o, "allowTypedFunctionExpressions", true),
            },
            allow_void: bool_field(
                o,
                "allowConciseArrowFunctionExpressionsStartingWithVoid",
                false,
            ),
            allowed_names: str_list(o, "allowedNames"),
            allow_without_type_params: bool_field(o, "allowFunctionsWithoutTypeParameters", false),
            allow_iifes: bool_field(o, "allowIIFEs", false),
        })
    }
}

/// The parent ESTree would show: the first ancestor that is not a `Paren`.
pub(crate) fn es_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

/// `f` is the value of a class member `MethodDef` (ESTree's `MethodDefinition`), not an
/// object literal method (a `Property`).
pub(crate) fn class_method(cx: &Ctx<'_, '_>, f: NodeId) -> Option<NodeId> {
    let p = cx.parent(f)?;
    let Node::MethodDef { value, .. } = *cx.node(p) else {
        return None;
    };
    let class = cx.parent(p)?;
    (value == f
        && matches!(
            cx.node(class),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        ))
    .then_some(p)
}

/// `f`'s ESTree parent is a `Property` (an object literal member, method or not).
fn object_property(cx: &Ctx<'_, '_>, f: NodeId) -> Option<NodeId> {
    let p = es_parent(cx, f)?;
    match *cx.node(p) {
        Node::Property { value, .. } if skip_parens(cx.ast(), value) == f => Some(p),
        Node::MethodDef { value, .. } if value == f && class_method(cx, f).is_none() => Some(p),
        _ => None,
    }
}

/// A context that gives a function expression its type: `x as T`, a typed variable,
/// parameter default or field, a call argument, or a JSX expression.
fn typed_parent(cx: &Ctx<'_, '_>, parent: NodeId, child: NodeId) -> bool {
    match *cx.node(parent) {
        Node::As { .. } | Node::TypeAssertion { .. } => true,
        Node::VarDeclarator { type_ann, .. } | Node::PropertyDef { type_ann, .. } => {
            type_ann.is_some()
        }
        Node::Param { type_ann, init, .. } => {
            type_ann.is_some() && init.is_some_and(|i| skip_parens(cx.ast(), i) == child)
        }
        Node::Call { callee, .. } => skip_parens(cx.ast(), callee) != child,
        Node::JsxExprContainer { .. } | Node::JsxSpreadAttr { .. } => true,
        _ => false,
    }
}

/// `f` is a member of an object literal (or of nested object literals) in a typed context.
fn property_of_typed_object(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let mut cur = f;
    for _ in 0..1000 {
        let Some(prop) = object_property(cx, cur) else {
            return false;
        };
        let Some(obj) = cx.parent(prop) else {
            return false;
        };
        if !matches!(cx.node(obj), Node::ObjectLit { .. }) {
            return false;
        }
        let Some(p) = es_parent(cx, obj) else {
            return false;
        };
        // Any call counts here, the callee position too.
        if matches!(cx.node(p), Node::Call { .. }) || typed_parent(cx, p, obj) {
            return true;
        }
        cur = obj;
    }
    false
}

pub(crate) fn typed_function_expression(cx: &Ctx<'_, '_>, f: NodeId, o: &ReturnOpts) -> bool {
    if !o.allow_typed {
        return false;
    }
    let Some(p) = es_parent(cx, f) else {
        return false;
    };
    typed_parent(cx, p, f)
        || property_of_typed_object(cx, f)
        || matches!(cx.node(p), Node::New { .. })
}

pub(crate) fn valid_function_expression_return_type(
    cx: &Ctx<'_, '_>,
    f: NodeId,
    o: &ReturnOpts,
) -> bool {
    if typed_function_expression(cx, f, o) {
        return true;
    }
    let p = es_parent(cx, f);
    if o.allow_expressions
        && !p.is_some_and(|p| {
            matches!(
                cx.node(p),
                Node::VarDeclarator { .. }
                    | Node::ExportDefaultExpr { .. }
                    | Node::PropertyDef { .. }
            )
        })
        && class_method(cx, f).is_none()
    {
        return true;
    }
    if !o.allow_direct_const {
        return false;
    }
    let Node::Arrow { flags, body, .. } = *cx.node(f) else {
        return false;
    };
    if !flags.contains(FnFlags::EXPRESSION_BODY) {
        return false;
    }
    let ast = cx.ast();
    let mut body = skip_parens(ast, body);
    for _ in 0..1000 {
        match *ast.node(body) {
            Node::Satisfies { expr, .. } => body = skip_parens(ast, expr),
            _ => break,
        }
    }
    matches!(
        *ast.node(body),
        Node::As { ty, .. } | Node::TypeAssertion { ty, .. }
            if matches!(ast.node(ty), Node::KeywordType { kind: KeywordTypeKind::Const })
    )
}

struct Returns {
    root: NodeId,
    all_functions: bool,
    any: bool,
}

impl Visitor for Returns {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
                if id != self.root =>
            {
                Walk::SkipChildren
            }
            Node::Return { arg } => {
                self.any = true;
                if !arg.is_some_and(|a| is_fn(ast, skip_parens(ast, a))) {
                    self.all_functions = false;
                    return Walk::Stop;
                }
                Walk::Continue
            }
            _ => Walk::Continue,
        }
    }
}

fn is_fn(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

/// The body of `f` (an arrow's expression or block).
fn fn_body(cx: &Ctx<'_, '_>, f: NodeId) -> Option<NodeId> {
    match *cx.node(f) {
        Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body,
        Node::Arrow { body, .. } => Some(body),
        _ => None,
    }
}

/// An arrow whose body is a function, or a function whose every `return` returns one.
pub(crate) fn immediately_returns_function(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    if let Node::Arrow { flags, body, .. } = *cx.node(f)
        && flags.contains(FnFlags::EXPRESSION_BODY)
    {
        return is_fn(ast, skip_parens(ast, body));
    }
    let Some(body) = fn_body(cx, f) else {
        return false;
    };
    let mut v = Returns {
        root: f,
        all_functions: true,
        any: false,
    };
    walk(ast, body, &mut v);
    v.any && v.all_functions
}

fn return_type(cx: &Ctx<'_, '_>, f: NodeId) -> Option<NodeId> {
    match *cx.node(f) {
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type,
        _ => None,
    }
}

/// A return type, a constructor or setter, or (with the option) a higher-order function.
pub(crate) fn valid_function_return_type(cx: &Ctx<'_, '_>, f: NodeId, o: &ReturnOpts) -> bool {
    if o.allow_higher_order && immediately_returns_function(cx, f) {
        return true;
    }
    if return_type(cx, f).is_some() {
        return true;
    }
    let Some(p) = cx.parent(f) else { return false };
    match *cx.node(p) {
        Node::MethodDef { kind, value, .. } if value == f => {
            kind == MethodKind::Set
                || (kind == MethodKind::Constructor && class_method(cx, f).is_some())
        }
        _ => false,
    }
}

/// `f` is returned (or is an arrow's body) inside something with a declared type.
pub(crate) fn ancestor_has_return_type(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let Some(mut anc) = es_parent(cx, f) else {
        return false;
    };
    if object_property(cx, f).is_some() {
        anc = f;
    }
    let ok = match *cx.node(anc) {
        Node::Return { .. } => true,
        Node::Arrow { flags, .. } => flags.contains(FnFlags::EXPRESSION_BODY),
        _ => false,
    };
    if !ok {
        return false;
    }
    let mut cur = Some(anc);
    while let Some(a) = cur {
        match *cx.node(a) {
            Node::FunctionDecl { return_type, .. }
            | Node::FunctionExpr { return_type, .. }
            | Node::Arrow { return_type, .. } => {
                if return_type.is_some() {
                    return true;
                }
            }
            Node::VarDeclarator { type_ann, .. } | Node::PropertyDef { type_ann, .. } => {
                return type_ann.is_some();
            }
            Node::ExprStmt { .. } => return false,
            _ => {}
        }
        cur = cx.parent(a);
    }
    false
}

impl ExplicitFunctionReturnType {
    fn allowed(&self, cx: &Ctx<'_, '_>, f: NodeId) -> bool {
        let (type_params, name) = match *cx.node(f) {
            Node::FunctionDecl {
                type_params, name, ..
            }
            | Node::FunctionExpr {
                type_params, name, ..
            } => (type_params, name),
            Node::Arrow { type_params, .. } => (type_params, None),
            _ => return false,
        };
        if self.allow_without_type_params && type_params.is_none() {
            return true;
        }
        if self.allow_iifes
            && es_parent(cx, f).is_some_and(|p| matches!(cx.node(p), Node::Call { .. }))
        {
            return true;
        }
        if self.allowed_names.is_empty() {
            return false;
        }
        let ast = cx.ast();
        let mut fname = name.and_then(|n| ident_name(ast, n));
        if fname.is_none() && !matches!(cx.node(f), Node::FunctionDecl { .. }) {
            fname = es_parent(cx, f).and_then(|p| match *cx.node(p) {
                Node::VarDeclarator { name, .. } => {
                    matches!(cx.node(name), Node::Ident { .. }).then(|| ident_name(ast, name))?
                }
                Node::MethodDef { key, .. }
                | Node::PropertyDef { key, .. }
                | Node::Property { key, .. } => {
                    matches!(cx.node(key), Node::Ident { .. }).then(|| ident_name(ast, key))?
                }
                _ => None,
            });
        }
        fname.is_some_and(|n| self.allowed_names.iter().any(|a| a == n))
    }
}

impl Rule for ExplicitFunctionReturnType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        // Bodyless functions are ESTree's `TSDeclareFunction` / `TSEmptyBodyFunctionExpression`.
        if fn_body(cx, node).is_none() {
            return;
        }
        let o = &self.opts;
        if let Node::FunctionDecl { return_type, .. } = *cx.node(node) {
            if self.allowed(cx, node) || (o.allow_typed && return_type.is_some()) {
                return;
            }
        } else {
            if self.allow_void
                && let Node::Arrow { flags, body, .. } = *cx.node(node)
                && flags.contains(FnFlags::EXPRESSION_BODY)
                && matches!(
                    cx.node(body),
                    Node::Unary {
                        op: UnaryOp::Void,
                        ..
                    }
                )
            {
                return;
            }
            if self.allowed(cx, node) {
                return;
            }
            if o.allow_typed
                && (valid_function_expression_return_type(cx, node, o)
                    || ancestor_has_return_type(cx, node))
            {
                return;
            }
        }
        if valid_function_return_type(cx, node, o) {
            return;
        }
        let head = member_function_head_span(cx.ast(), cx.src(), cx.file.parents, node);
        cx.report(head, "Missing return type on function.")
            .id("missingReturnType");
    }
}
