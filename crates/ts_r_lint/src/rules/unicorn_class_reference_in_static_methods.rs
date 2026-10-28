//! `unicorn/class-reference-in-static-methods`: inside a static method (arrow functions
//! included, other functions and field initializers or static blocks excluded), refer to
//! the class as `this` and to a plain-name superclass as `super` (the defaults,
//! `preferThis` and `preferSuper`), or the other way round when an option is `false`.
//!
//! With `preferThis`, a reference to the class's own binding (its name, or the `const`
//! a nameless class expression initializes) is reported; with `preferSuper`, a plain
//! member access (not optional, not parenthesized) on the superclass's binding. With an
//! option off, `this` (or `super`) is reported instead, naming the class (superclass) when
//! that name resolves to the same binding at the use, else without a suggestion. Not
//! reported: assignment targets (through member chains, TypeScript wrappers and
//! destructuring), callees (`A()`, `new A()`, tags), `A.#x` and `#x in A`, and names used
//! as types.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/class-reference-in-static-methods",
        "Enforce consistent class references in static methods.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<ClassReferenceInStaticMethods>,
};

#[derive(Debug)]
struct ClassReferenceInStaticMethods {
    prefer_this: bool,
    prefer_super: bool,
}

impl FromOptions for ClassReferenceInStaticMethods {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(Self {
            prefer_this: bool_field(o, "preferThis", true),
            prefer_super: bool_field(o, "preferSuper", true),
        })
    }
}

fn is_ts_wrapper(ast: &Ast, parent: NodeId, child: NodeId) -> bool {
    match *ast.node(parent) {
        Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::TypeAssertion { expr, .. }
        | Node::NonNull { expr } => expr == child,
        _ => false,
    }
}

/// `node` with the parentheses and TypeScript wrappers around it, and its parent.
fn outermost(cx: &Ctx<'_, '_>, node: NodeId) -> (NodeId, Option<NodeId>) {
    let ast = cx.ast();
    let mut cur = node;
    loop {
        let Some(p) = cx.parent(cur) else {
            return (cur, None);
        };
        if matches!(ast.node(p), Node::Paren { .. }) || is_ts_wrapper(ast, p, cur) {
            cur = p;
        } else {
            return (cur, Some(p));
        }
    }
}

/// The static method whose code `node` runs in.
fn static_method(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut child = node;
    while let Some(p) = cx.parent(child) {
        match *ast.node(p) {
            Node::Arrow { .. } => {}
            Node::FunctionDecl { .. } => return None,
            Node::FunctionExpr { .. } => {
                let method = cx.parent(p).filter(
                    |&m| matches!(ast.node(m), Node::MethodDef { value, .. } if *value == p),
                );
                let in_class = method.and_then(|m| cx.parent(m)).is_some_and(|c| {
                    matches!(ast.kind(c), NodeKind::ClassDecl | NodeKind::ClassExpr)
                });
                if !in_class {
                    return None;
                }
            }
            Node::MethodDef {
                modifiers, value, ..
            } => {
                return (modifiers.contains(Modifiers::STATIC) && value == child).then_some(p);
            }
            Node::PropertyDef { .. }
            | Node::StaticBlock { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => return None,
            _ => {}
        }
        child = p;
    }
    None
}

/// The binding naming the class: its own name, or the `const`/`let` it initializes.
fn class_reference(cx: &Ctx<'_, '_>, class: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. }) = *ast.node(class) else {
        return None;
    };
    if name.is_some() {
        return name;
    }
    if ast.kind(class) != NodeKind::ClassExpr {
        return None;
    }
    let (_, parent) = outermost(cx, class);
    match *ast.node(parent?) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if skip_parens(ast, init) == class && matches!(ast.node(name), Node::Ident { .. }) => {
            Some(name)
        }
        _ => None,
    }
}

fn super_class(ast: &Ast, class: NodeId) -> Option<NodeId> {
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return None;
    };
    for &h in ast.list(heritage) {
        if let Node::HeritageClause {
            kind: ts_r_ast::HeritageKind::Extends,
            types,
        } = *ast.node(h)
            && let Some(&t) = ast.list(types).first()
        {
            let e = match *ast.node(t) {
                Node::ExprWithTypeArgs { expr, .. } => expr,
                _ => t,
            };
            let e = skip_parens(ast, e);
            return matches!(ast.node(e), Node::Ident { .. }).then_some(e);
        }
    }
    None
}

/// The symbol of a binding or reference identifier.
fn binding_of(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    cx.semantic().symbol_of_ident(ident)
}

/// `reference`'s name, looked up at `node`, is the same binding.
fn name_available(cx: &Ctx<'_, '_>, node: NodeId, reference: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(name) = ident_name(cx.ast(), reference) else {
        return false;
    };
    // Two unresolved globals of one name are the same binding.
    sem.lookup(sem.enclosing_scope(node), name) == binding_of(cx, reference)
}

fn is_assignment_root(ast: &Ast, parent: NodeId, node: NodeId) -> bool {
    match *ast.node(parent) {
        Node::Assign { left, .. } => left == node,
        Node::Update { arg, .. } => arg == node,
        Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } => arg == node,
        Node::ForIn { left, .. } | Node::ForOf { left, .. } => left == node,
        _ => false,
    }
}

fn is_assignment_target(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..4096 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if is_assignment_root(ast, p, cur) {
            return true;
        }
        let up = match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj == cur,
            Node::Paren { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::Spread { .. }
            | Node::Rest { .. }
            | Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. } => true,
            Node::Property { value, .. } => value == cur,
            Node::ShorthandProp { name, .. } => name == cur,
            Node::BindingProperty { value, .. } => value == Some(cur),
            Node::AssignPattern { left, .. } => left == cur,
            // A default inside a destructuring target (`[a.b = 1] = x`).
            Node::Assign { left, .. } => {
                left == cur
                    && cx.parent(p).is_some_and(|g| {
                        matches!(ast.kind(g), NodeKind::ArrayLit | NodeKind::Property)
                    })
            }
            _ => is_ts_wrapper(ast, p, cur),
        };
        if !up {
            return false;
        }
        cur = p;
    }
    false
}

fn is_direct_callee(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let (cur, parent) = outermost(cx, node);
    parent.is_some_and(|p| match *cx.ast().node(p) {
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == cur,
        Node::TaggedTemplate { tag, .. } => tag == cur,
        _ => false,
    })
}

fn is_private_use(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let (cur, parent) = outermost(cx, node);
    parent.is_some_and(|p| match *ast.node(p) {
        Node::Member { obj, prop, .. } => {
            obj == cur && matches!(ast.node(prop), Node::PrivateName { .. })
        }
        Node::Binary {
            op: BinaryOp::In,
            left,
            right,
        } => right == cur && matches!(ast.node(left), Node::PrivateName { .. }),
        _ => false,
    })
}

/// A non-optional member access on `node` (not parenthesized) that is not written to.
fn is_simple_member_access(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let Some(p) = cx.parent(node) else {
        return false;
    };
    matches!(*cx.ast().node(p),
        Node::Member { obj, optional: false, .. } | Node::Index { obj, optional: false, .. }
            if obj == node)
        && !is_assignment_target(cx, p)
}

/// The fix replacing the identifier, `this` or `super` at `node` with `text`.
fn replacement(cx: &Ctx<'_, '_>, node: NodeId, text: &str) -> Fix {
    let ast = cx.ast();
    if let Some(p) = cx.parent(node)
        && matches!(ast.node(p), Node::ShorthandProp { name, init: None, .. } if *name == node)
    {
        return Fix::replace(cx.span(node), format!("{}: {text}", cx.text(node)));
    }
    Fix::replace(cx.span(node), text.to_owned())
}

fn report_with_suggestion(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    id: &'static str,
    msg: String,
    source: &str,
    text: &str,
) {
    let fix = replacement(cx, node, text);
    cx.report_node(node, msg).id(id).suggest(
        "suggestion",
        format!("Replace `{source}` with `{text}`."),
        fix,
    );
}

/// The identifier names a type (`A`, `A.B` in a type reference, `typeof A`).
fn is_type_name(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        match ast.kind(p) {
            NodeKind::QualifiedName => cur = p,
            NodeKind::TypeRef | NodeKind::TypeQuery => return true,
            _ => return false,
        }
    }
    false
}

impl ClassReferenceInStaticMethods {
    fn check_this(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.prefer_this
            || is_assignment_target(cx, node)
            || is_direct_callee(cx, node)
            || is_private_use(cx, node)
        {
            return;
        }
        let Some(method) = static_method(cx, node) else {
            return;
        };
        let Some(class) = cx.parent(method) else {
            return;
        };
        match class_reference(cx, class) {
            Some(r) if name_available(cx, node, r) => {
                let name = cx.text(r).to_owned();
                report_with_suggestion(
                    cx,
                    node,
                    "class",
                    format!("Use `{name}` instead of `this` in static methods."),
                    "this",
                    &name,
                );
            }
            _ => {
                cx.report_node(
                    node,
                    "Use the class name instead of `this` in static methods.",
                )
                .id("class-unavailable");
            }
        }
    }

    fn check_super(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.prefer_super || is_assignment_target(cx, node) {
            return;
        }
        let Some(method) = static_method(cx, node) else {
            return;
        };
        let Some(class) = cx.parent(method) else {
            return;
        };
        match super_class(cx.ast(), class) {
            Some(r) if name_available(cx, node, r) => {
                let name = cx.text(r).to_owned();
                report_with_suggestion(
                    cx,
                    node,
                    "super-class",
                    format!("Use `{name}` instead of `super` in static methods."),
                    "super",
                    &name,
                );
            }
            _ => {
                cx.report_node(
                    node,
                    "Use the superclass name instead of `super` in static methods.",
                )
                .id("super-class-unavailable");
            }
        }
    }

    fn check_ident(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !self.prefer_this && !self.prefer_super {
            return;
        }
        let sem = cx.semantic();
        let Some(r) = sem.reference_at(node) else {
            return;
        };
        if sem.reference(r).is_type()
            || is_type_name(cx, node)
            || is_assignment_target(cx, node)
            || is_direct_callee(cx, node)
            || is_private_use(cx, node)
        {
            return;
        }
        let Some(method) = static_method(cx, node) else {
            return;
        };
        let Some(class) = cx.parent(method) else {
            return;
        };
        let sym = binding_of(cx, node);
        let name = cx.text(node).to_owned();
        if self.prefer_this
            && let Some(r) = class_reference(cx, class)
            && cx.text(r) == name
            && binding_of(cx, r) == sym
        {
            report_with_suggestion(
                cx,
                node,
                "this",
                format!("Use `this` instead of `{name}` in static methods."),
                &name,
                "this",
            );
            return;
        }
        if !self.prefer_super || !is_simple_member_access(cx, node) {
            return;
        }
        if let Some(s) = super_class(cx.ast(), class)
            && cx.text(s) == name
            && binding_of(cx, s) == sym
        {
            report_with_suggestion(
                cx,
                node,
                "super",
                format!("Use `super` instead of `{name}` in static methods."),
                &name,
                "super",
            );
        }
    }
}

impl Rule for ClassReferenceInStaticMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::This, NodeKind::Super, NodeKind::Ident]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.ast().kind(node) {
            NodeKind::This => self.check_this(node, cx),
            NodeKind::Super => self.check_super(node, cx),
            NodeKind::Ident => self.check_ident(node, cx),
            _ => {}
        }
    }
}
