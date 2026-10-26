//! `@typescript-eslint/class-literal-property-style`: expose class literals one way,
//! as `readonly` fields (`fields`, the default) or as getters (`getters`).
//!
//! - `fields`: a getter (not `override`) whose first statement returns a literal (a
//!   primitive literal, a template without substitutions, or such a tagged template) reports,
//!   unless the class has a setter of the same static name. The suggestion (not for
//!   decorated getters) rewrites it as a `readonly` field, keeping the return type.
//! - `getters`: a `readonly` field (not `declare` or `override`) initialised with such a
//!   literal reports, unless its constructor assigns it (`this.x = ...` directly in the
//!   class's constructor, not in a nested function). The suggestion makes it a getter.
//! - Static names: an identifier or private key, a literal computed key, or a computed key
//!   naming a variable that is never reassigned and holds a literal.

use std::borrow::Cow;

use super::prelude::*;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/class-literal-property-style",
        "Enforce that literals on classes are exposed in a consistent style",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<ClassLiteralPropertyStyle>,
};

#[derive(Debug, Default)]
struct ClassLiteralPropertyStyle {
    getters: bool,
}

impl FromOptions for ClassLiteralPropertyStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first().and_then(Value::as_str) {
            None | Some("fields") => Ok(Self { getters: false }),
            Some("getters") => Ok(Self { getters: true }),
            Some(o) => Err(format!("class-literal-property-style: unknown style {o:?}")),
        }
    }
}

fn supported_literal(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::StringLit { .. }
        | Node::RegexLit { .. } => true,
        Node::TemplateLit { parts } => ast
            .list(parts)
            .iter()
            .all(|&p| matches!(ast.node(p), Node::TemplateElement { .. })),
        Node::TaggedTemplate { quasi, .. } => supported_literal(ast, quasi),
        _ => false,
    }
}

/// `getStaticMemberAccessValue` of a class member key.
fn static_name<'a>(cx: &Ctx<'_, 'a>, key: NodeId) -> Option<Cow<'a, str>> {
    let ast = cx.ast();
    let src = cx.src();
    match *ast.node(key) {
        Node::ComputedKey { expr } => {
            let expr = skip_parens(ast, expr);
            if let Some(v) = static_value_string(ast, src, expr) {
                return Some(v);
            }
            let init = unchanging_init(cx, expr)?;
            static_value_string(ast, src, init)
        }
        _ => key_name(ast, src, key),
    }
}

/// The initializer of the variable `ident` refers to, when it is never reassigned.
fn unchanging_init(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    if !matches!(ast.node(ident), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let r = sem.reference_at(ident)?;
    let Resolution::Symbol(s) = sem.reference(r).resolution else {
        return None;
    };
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    if !matches!(decl.kind, DeclKind::Const | DeclKind::Let | DeclKind::Var) {
        return None;
    }
    let written = sym.refs.iter().any(|&r| {
        let f = sem.reference(r).flags;
        f.contains(RefFlags::WRITE) && !f.contains(RefFlags::INIT)
    });
    if written {
        return None;
    }
    match *ast.node(decl.node) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => Some(init),
        _ => None,
    }
}

/// The key as ESLint reports it (a computed key's expression) and as the fix writes it.
fn key_parts(cx: &Ctx<'_, '_>, key: NodeId) -> (Span, String) {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::ComputedKey { expr } => {
            let e = skip_parens(ast, expr);
            (cx.span(e), format!("[{}]", cx.text(e)))
        }
        _ => (cx.span(key), cx.text(key).to_owned()),
    }
}

fn modifiers_text(modifiers: Modifiers, last: &str) -> String {
    let access = if modifiers.contains(Modifiers::PUBLIC) {
        "public"
    } else if modifiers.contains(Modifiers::PRIVATE) {
        "private"
    } else if modifiers.contains(Modifiers::PROTECTED) {
        "protected"
    } else {
        ""
    };
    let stat = if modifiers.contains(Modifiers::STATIC) {
        " static"
    } else {
        ""
    };
    format!("{access}{stat} {last} ").trim_start().to_owned()
}

/// `isAssignee`: the member is written (assigned, deleted, updated, an array destructuring
/// element or a rest target), possibly through a TS wrapper.
fn is_assignee(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let parents = cx.file.parents;
    let Some(p) = parent_skip_parens(ast, parents, id) else {
        return false;
    };
    match *ast.node(p) {
        Node::Assign { left, .. } => skip_parens(ast, left) == id,
        Node::Unary {
            op: UnaryOp::Delete,
            arg,
        }
        | Node::Update { arg, .. } => skip_parens(ast, arg) == id,
        Node::ArrayLit { .. } => is_assignment_target(ast, parents, p),
        Node::Spread { .. } => parents.parent(p).is_some_and(|gp| {
            matches!(ast.node(gp), Node::ArrayLit { .. } | Node::ObjectLit { .. })
                && is_assignment_target(ast, parents, gp)
        }),
        Node::NonNull { .. }
        | Node::As { .. }
        | Node::TypeAssertion { .. }
        | Node::Satisfies { .. } => is_assignee(cx, p),
        _ => false,
    }
}

/// The class whose body holds `id` (a member, or something inside one), skipping classes
/// `id` is only in the heritage, name or decorators of.
fn enclosing_class_body(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if let Node::ClassDecl {
            decorators,
            name,
            type_params,
            heritage,
            ..
        }
        | Node::ClassExpr {
            decorators,
            name,
            type_params,
            heritage,
            ..
        } = *ast.node(p)
            && name != Some(cur)
            && type_params != Some(cur)
            && !ast.list(decorators).contains(&cur)
            && !ast.list(heritage).contains(&cur)
        {
            return Some(p);
        }
        cur = p;
    }
    None
}

/// Whether the function nearest `id` is a class constructor.
fn in_constructor(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if is_function(ast.kind(p)) {
            return cx.parent(p).is_some_and(|m| {
                matches!(
                    ast.node(m),
                    Node::MethodDef {
                        kind: MethodKind::Constructor,
                        ..
                    }
                )
            });
        }
        cur = p;
    }
    false
}

/// `getStaticMemberAccessValue` of `this.x` / `this[k]`.
fn member_static_name<'a>(cx: &Ctx<'_, 'a>, member: NodeId) -> Option<Cow<'a, str>> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } | Node::PrivateName { name } => {
                Some(Cow::Borrowed(ast.atom(name)))
            }
            _ => None,
        },
        Node::Index { index, .. } => {
            let e = skip_parens(ast, index);
            if let Some(v) = static_value_string(ast, cx.src(), e) {
                return Some(v);
            }
            let init = unchanging_init(cx, e)?;
            static_value_string(ast, cx.src(), init)
        }
        _ => None,
    }
}

#[derive(Default)]
struct ThisNodes(Vec<NodeId>);

impl Visitor for ThisNodes {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
        if matches!(ast.node(id), Node::This {}) {
            self.0.push(id);
        }
        Walk::Continue
    }
}

impl ClassLiteralPropertyStyle {
    fn check_getter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::MethodDef {
            decorators,
            modifiers,
            kind: MethodKind::Get,
            key,
            value,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        if modifiers.contains(Modifiers::OVERRIDE) {
            return;
        }
        let Some(class) = cx.parent(node) else {
            return;
        };
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *ast.node(class)
        else {
            return;
        };
        let Node::FunctionExpr {
            return_type,
            body: Some(body),
            ..
        } = *ast.node(value)
        else {
            return;
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let Some(&first) = ast.list(stmts).first() else {
            return;
        };
        let Node::Return { arg: Some(arg) } = *ast.node(first) else {
            return;
        };
        if !supported_literal(ast, arg) {
            return;
        }
        if let Some(name) = static_name(cx, key).filter(|n| !n.is_empty()) {
            let has_setter = ast.list(members).iter().any(|&m| {
                matches!(
                    *ast.node(m),
                    Node::MethodDef { kind: MethodKind::Set, modifiers, key, .. }
                        if !modifiers.contains(Modifiers::ABSTRACT)
                            && static_name(cx, key).as_deref() == Some(&*name)
                )
            });
            if has_setter {
                return;
            }
        }
        let (span, key_text) = key_parts(cx, key);
        let fix = if ast.list(decorators).is_empty() {
            let src = cx.src();
            let after = match return_type {
                Some(rt) => cx.span(rt).start as usize,
                None => cx.span(body).start as usize,
            };
            let mut pos = skip_trivia_backward(ast, src, after);
            if src.as_bytes().get(pos.wrapping_sub(1)) == Some(&b':') {
                pos = skip_trivia_backward(ast, src, pos - 1);
            }
            let between = src.get(pos..cx.span(body).start as usize).unwrap_or("");
            let text = format!(
                "{}{key_text}{between}= {};",
                modifiers_text(modifiers, "readonly"),
                cx.text(skip_parens(ast, arg))
            );
            Some(Fix::replace(cx.span(node), text))
        } else {
            None
        };
        let r = cx
            .report(span, "Literals should be exposed using readonly fields.")
            .id("preferFieldStyle");
        if let Some(fix) = fix {
            r.suggest(
                "preferFieldStyleSuggestion",
                "Replace the literals with readonly fields.",
                fix,
            );
        }
    }

    fn check_class(&self, class: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *ast.node(class)
        else {
            return;
        };
        let props: Vec<NodeId> = ast
            .list(members)
            .iter()
            .copied()
            .filter(|&m| {
                matches!(
                    *ast.node(m),
                    Node::PropertyDef { modifiers, value: Some(v), .. }
                        if modifiers.contains(Modifiers::READONLY)
                            && !modifiers.intersects(
                                Modifiers::DECLARE
                                    | Modifiers::OVERRIDE
                                    | Modifiers::ABSTRACT
                                    | Modifiers::ACCESSOR
                            )
                            && supported_literal(ast, v)
                )
            })
            .collect();
        if props.is_empty() {
            return;
        }
        let mut thises = ThisNodes::default();
        for &m in ast.list(members) {
            walk(ast, m, &mut thises);
        }
        let mut exclude: Vec<Cow<'_, str>> = Vec::new();
        for &t in &thises.0 {
            let Some(member) = parent_skip_parens(ast, cx.file.parents, t) else {
                continue;
            };
            if !matches!(ast.node(member), Node::Member { .. } | Node::Index { .. })
                || !is_assignee(cx, member)
                || !in_constructor(cx, t)
                || enclosing_class_body(cx, t) != Some(class)
            {
                continue;
            }
            if let Some(name) = member_static_name(cx, member).filter(|n| !n.is_empty()) {
                exclude.push(name);
            }
        }
        for p in props {
            let Node::PropertyDef {
                modifiers,
                key,
                value: Some(value),
                ..
            } = *ast.node(p)
            else {
                continue;
            };
            if let Some(name) = static_name(cx, key)
                && !name.is_empty()
                && exclude.contains(&name)
            {
                continue;
            }
            let (span, key_text) = key_parts(cx, key);
            let text = format!(
                "{}{key_text}() {{ return {}; }}",
                modifiers_text(modifiers, "get"),
                cx.text(skip_parens(ast, value))
            );
            let fix = Fix::replace(cx.span(p), text);
            cx.report(span, "Literals should be exposed using getters.")
                .id("preferGetterStyle")
                .suggest(
                    "preferGetterStyleSuggestion",
                    "Replace the literals with getters.",
                    fix,
                );
        }
    }
}

impl Rule for ClassLiteralPropertyStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        if self.getters {
            &[NodeKind::ClassDecl, NodeKind::ClassExpr]
        } else {
            &[NodeKind::MethodDef]
        }
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.getters {
            self.check_class(node, cx);
        } else {
            self.check_getter(node, cx);
        }
    }
}
