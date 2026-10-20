//! `@typescript-eslint/no-unsafe-assignment`: disallow assigning `any` to variables,
//! properties and destructuring targets.

use super::prelude::*;
use super::ts_no_misused_promises::jsx_attr_type;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-assignment",
        "Disallow assigning a value with type `any` to variables and properties",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoUnsafeAssignment>,
};

#[derive(Debug, Default)]
struct NoUnsafeAssignment;

const THIS_HINT: &str = "\nYou can try to fix this by turning on the `noImplicitThis` compiler option, or adding a `this` parameter to the function.";

impl Rule for NoUnsafeAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDeclarator,
            NodeKind::Param,
            NodeKind::PropertyDef,
            NodeKind::Assign,
            NodeKind::ArrayLit,
            NodeKind::ObjectLit,
            NodeKind::JsxAttr,
            NodeKind::BindingProperty,
            NodeKind::AssignPattern,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ti = cx.types();
        match *cx.node(node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init: Some(init),
                ..
            } => {
                let receiver = type_ann.and_then(|t| ti.type_of_type_node(t));
                let span = Span::new(cx.span(name).start, cx.span(init).end);
                check(cx, span, name, init, receiver, true);
            }
            Node::Param {
                pattern,
                type_ann,
                init: Some(init),
                ..
            } => {
                let receiver = type_ann.and_then(|t| ti.type_of_type_node(t));
                let span = Span::new(cx.span(pattern).start, cx.span(init).end);
                check(cx, span, pattern, init, receiver, true);
            }
            // A default inside a binding pattern (`const { p = x } = o`, `[y = x]`): an `any`
            // default is unsafe whatever the binding's type, even `unknown`; another default
            // is checked against the binding's type (observed).
            Node::BindingProperty {
                key,
                value,
                init: Some(init),
            } => {
                let target = value.unwrap_or(key);
                let span = Span::new(cx.span(target).start, cx.span(init).end);
                let receiver = default_receiver(cx, target, init);
                check(cx, span, target, init, receiver, true);
            }
            Node::AssignPattern { left, right } => {
                let span = Span::new(cx.span(left).start, cx.span(right).end);
                let receiver = default_receiver(cx, left, right);
                check(cx, span, left, right, receiver, true);
            }
            Node::PropertyDef {
                key,
                type_ann,
                value: Some(value),
                ..
            } => {
                let receiver = type_ann.and_then(|t| ti.type_of_type_node(t));
                check(cx, cx.span(node), key, value, receiver, false);
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => {
                let l = skip_parens(cx.ast(), left);
                let pattern = matches!(cx.node(l), Node::ArrayLit { .. } | Node::ObjectLit { .. });
                // A pattern target's own type is compared first; the pattern is checked
                // element by element only when that is safe (observed: `[[[[x]]]] =
                // [1 as any]` is `[any]` to `[[[[any]]]]`).
                if pattern
                    && let Some(st) = constrained_type_at(ti, right)
                    && st != Ty::UNRESOLVED
                    && any_kind(ti, st).is_none()
                    && let Some(rt) = ti.type_at(l).filter(|&r| r != Ty::UNRESOLVED)
                    && is_unsafe_assignment(ti, st, rt)
                {
                    let s = ti.type_to_string(st);
                    let r = ti.type_to_string(rt);
                    cx.report(
                        cx.span(node),
                        format!("Unsafe assignment of type `{s}` to a variable of type `{r}`."),
                    )
                    .id("unsafeAssignment");
                    return;
                }
                let receiver = if pattern { None } else { ti.type_at(l) };
                check(cx, cx.span(node), l, right, receiver, true);
            }
            Node::ArrayLit { elements } => {
                if is_assignment_target(cx.ast(), cx.file.parents, node) {
                    return;
                }
                for &e in cx.ast().list(elements) {
                    let Node::Spread { arg } = *cx.node(e) else {
                        continue;
                    };
                    if let Some(t) = ti.type_at(arg)
                        && (is_any(ti, t) || is_any_array(ti, t))
                    {
                        cx.report_node(e, "Unsafe spread of an `any` value in an array.")
                            .id("unsafeArraySpread");
                    }
                }
            }
            Node::ObjectLit { props } => {
                if is_assignment_target(cx.ast(), cx.file.parents, node) {
                    return;
                }
                let ctx = ti.contextual_type(node).map(|c| ti.non_nullable(c));
                let ast = cx.ast();
                let src = cx.src();
                for &p in ast.list(props) {
                    let (key, value) = match *cx.node(p) {
                        Node::Property { key, value, .. } => (key, value),
                        Node::ShorthandProp {
                            name, init: None, ..
                        } => (name, name),
                        _ => continue,
                    };
                    if matches!(
                        cx.node(skip_parens(ast, value)),
                        Node::FunctionExpr { .. } | Node::Arrow { .. }
                    ) {
                        continue;
                    }
                    let receiver = ctx.and_then(|c| {
                        let name = static_key_name(ast, src, key)?;
                        contextual_property(ti, c, &name)
                    });
                    // An error-typed property of an intersection receives anything
                    // (observed).
                    if receiver.is_some_and(|r| ti.is_error(r))
                        && ctx.is_some_and(|c| ti.flags(c).contains(TypeFlags::INTERSECTION))
                    {
                        continue;
                    }
                    check(cx, cx.span(p), value, value, receiver, false);
                }
            }
            Node::JsxAttr {
                name,
                value: Some(v),
            } => {
                let Node::JsxExprContainer { expr: Some(e) } = *cx.node(v) else {
                    return;
                };
                let receiver = ti
                    .contextual_type(e)
                    .or_else(|| jsx_attr_type(cx, node, name));
                check(cx, cx.span(e), e, e, receiver, false);
            }
            _ => {}
        }
    }
}

/// The type an object literal's contextual type gives property `name`: the property, else
/// an index signature; a union contributes the members that have one (an async function's
/// `V | PromiseLike<V>` return context types its object by `V`).
fn contextual_property(ti: &dyn TypeInfo, c: Ty, name: &str) -> Option<Ty> {
    let one = |t: Ty| match ti.property_of_type(t, name) {
        Some(s) => Some(ti.type_of_symbol(s)),
        // An index signature receives the property.
        None => ti.index_type(t, false),
    };
    if let Some(r) = one(c) {
        return Some(r);
    }
    if !ti.flags(c).contains(TypeFlags::UNION) {
        return None;
    }
    let found: Vec<Ty> = ti.union_members(c).into_iter().filter_map(one).collect();
    match found.len() {
        0 => None,
        1 => Some(found[0]),
        _ => Some(ti.union_of(&found)),
    }
}

/// The receiver of a binding-pattern default: the bound identifier's type, unless the
/// default is `any`-like (then nothing exempts it).
fn default_receiver(cx: &Ctx<'_, '_>, target: NodeId, default: NodeId) -> Option<Ty> {
    let ti = cx.types();
    if !matches!(cx.node(target), Node::Ident { .. }) {
        return None;
    }
    let st = constrained_type_at(ti, default)?;
    if any_kind(ti, st).is_some() {
        return None;
    }
    ti.type_at(target)
        .or_else(|| ti.symbol_at(target).map(|s| ti.type_of_symbol(s)))
}

/// Checks `sender` assigned to `target` (an identifier, member or pattern), reporting `span`.
fn check(
    cx: &mut Ctx<'_, '_>,
    span: Span,
    target: NodeId,
    sender: NodeId,
    receiver: Option<Ty>,
    destructure: bool,
) {
    let ti = cx.types();
    let ast = cx.ast();
    let Some(st) = constrained_type_at(ti, sender) else {
        return;
    };
    if st == Ty::UNRESOLVED {
        return;
    }
    let receiver = receiver.filter(|&r| r != Ty::UNRESOLVED);
    if let Some(kind) = any_kind(ti, st) {
        if receiver.is_some_and(|r| is_unknown(ti, r)) {
            return;
        }
        if kind == AnyKind::Any && matches!(cx.node(skip_parens(ast, sender)), Node::This {}) {
            cx.report(
                span,
                format!(
                    "Unsafe assignment of an `any` value. `this` is typed as `any`.{THIS_HINT}"
                ),
            )
            .id("anyAssignmentThis");
            return;
        }
        let what = match kind {
            AnyKind::Any => "an `any` value",
            AnyKind::Error => "an error typed value",
        };
        cx.report(span, format!("Unsafe assignment of {what}."))
            .id("anyAssignment");
        return;
    }
    if destructure {
        let is_pattern = matches!(
            cx.node(target),
            Node::ArrayPattern { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayLit { .. }
                | Node::ObjectLit { .. }
        );
        if is_pattern {
            destructuring(cx, target, Some(sender), st, 0);
            return;
        }
    }
    let Some(rt) = receiver else { return };
    if let Node::New {
        args,
        type_args: None,
        ..
    } = *cx.node(skip_parens(ast, sender))
        && ast.list(args).is_empty()
    {
        return;
    }
    if is_unsafe_assignment(ti, st, rt) {
        let s = ti.type_to_string(st);
        let r = ti.type_to_string(rt);
        cx.report(
            span,
            format!("Unsafe assignment of type `{s}` to a variable of type `{r}`."),
        )
        .id("unsafeAssignment");
    }
}

/// The binding/assignment target inside a pattern element with a default.
fn element_target(cx: &Ctx<'_, '_>, e: NodeId) -> NodeId {
    match *cx.node(e) {
        Node::AssignPattern { left, .. } => left,
        Node::Assign {
            left,
            op: AssignOp::Assign,
            ..
        } => left,
        _ => e,
    }
}

fn what(kind: AnyKind) -> &'static str {
    match kind {
        AnyKind::Any => "an `any` value",
        AnyKind::Error => "an error typed value",
    }
}

/// Checks a destructuring `pattern` against a value of type `t`. When the value is an array
/// literal (`node`), its elements are matched one by one, as TypeScript types such a literal
/// as a tuple.
fn destructuring(cx: &mut Ctx<'_, '_>, pattern: NodeId, node: Option<NodeId>, t: Ty, depth: u32) {
    if depth > 64 || t == Ty::UNRESOLVED {
        return;
    }
    let ti = cx.types();
    let ast = cx.ast();
    let literal = node
        .map(|n| skip_parens(ast, n))
        .and_then(|n| match *cx.node(n) {
            Node::ArrayLit { elements }
                if !ast
                    .list(elements)
                    .iter()
                    .any(|&e| matches!(cx.node(e), Node::Spread { .. })) =>
            {
                Some(elements)
            }
            _ => None,
        });
    match *cx.node(pattern) {
        Node::ArrayPattern { elements } | Node::ArrayLit { elements } if literal.is_some() => {
            let values = ast.list(literal.unwrap_or_default());
            for (i, &e) in ast.list(elements).iter().enumerate() {
                if matches!(cx.node(e), Node::Rest { .. } | Node::Spread { .. }) {
                    break;
                }
                if matches!(cx.node(e), Node::Hole {}) {
                    continue;
                }
                let Some(&v) = values.get(i) else { break };
                let Some(vt) = constrained_type_at(ti, v) else {
                    continue;
                };
                let target = element_target(cx, e);
                if let Some(k) = any_kind(ti, vt) {
                    cx.report_node(
                        target,
                        format!(
                            "Unsafe array destructuring of a tuple element with {}.",
                            what(k)
                        ),
                    )
                    .id("unsafeArrayPatternFromTuple");
                } else {
                    destructuring(cx, target, Some(v), vt, depth + 1);
                }
            }
        }
        Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
            if is_any_array(ti, t) {
                cx.report_node(
                    pattern,
                    "Unsafe array destructuring of an `any` array value.",
                )
                .id("unsafeArrayPattern");
                return;
            }
            if !ti.is_tuple(t) {
                return;
            }
            let elems = ti.type_args(t);
            for (i, &e) in ast.list(elements).iter().enumerate() {
                if matches!(cx.node(e), Node::Rest { .. } | Node::Spread { .. }) {
                    break;
                }
                if matches!(cx.node(e), Node::Hole {}) {
                    continue;
                }
                let Some(&et) = elems.get(i) else { break };
                let target = element_target(cx, e);
                if let Some(k) = any_kind(ti, et) {
                    cx.report_node(
                        target,
                        format!(
                            "Unsafe array destructuring of a tuple element with {}.",
                            what(k)
                        ),
                    )
                    .id("unsafeArrayPatternFromTuple");
                } else {
                    destructuring(cx, target, None, et, depth + 1);
                }
            }
        }
        Node::ObjectPattern { props } | Node::ObjectLit { props } => {
            let src = cx.src();
            for &p in ast.list(props) {
                let (key, target) = match *cx.node(p) {
                    Node::BindingProperty { key, value, .. } => (key, value.unwrap_or(key)),
                    Node::Property { key, value, .. } => (key, element_target(cx, value)),
                    Node::ShorthandProp { name, .. } => (name, name),
                    _ => continue,
                };
                let Some(name) = static_key_name(ast, src, key) else {
                    continue;
                };
                let Some(s) = ti.property_of_type(t, &name) else {
                    continue;
                };
                let pt = ti.type_of_symbol(s);
                if let Some(k) = any_kind(ti, pt) {
                    cx.report_node(
                        target,
                        format!(
                            "Unsafe object destructuring of a property with {}.",
                            what(k)
                        ),
                    )
                    .id("unsafeObjectPattern");
                } else {
                    destructuring(cx, target, None, pt, depth + 1);
                }
            }
        }
        _ => {}
    }
}
