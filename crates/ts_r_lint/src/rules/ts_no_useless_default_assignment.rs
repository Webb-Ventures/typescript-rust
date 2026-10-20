//! `@typescript-eslint/no-useless-default-assignment`: no default for a parameter or
//! destructured property that can never be `undefined`, and no `= undefined` defaults.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-useless-default-assignment",
        "Disallow default values that will never be used",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create::<NoUselessDefaultAssignment>,
};

#[derive(Debug)]
struct NoUselessDefaultAssignment {
    allow_without_strict_null_checks: bool,
}

impl FromOptions for NoUselessDefaultAssignment {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUselessDefaultAssignment {
            allow_without_strict_null_checks: bool_field(
                obj_opt(opts, 0),
                "allowRuleToRunWithoutStrictNullChecksIKnowWhatIAmDoing",
                false,
            ),
        })
    }
}

const PREFER_OPTIONAL: &str =
    "Using `= undefined` to make a parameter optional adds unnecessary runtime logic.";
const NO_STRICT_NULL_CHECKS: &str = "This rule requires the `strictNullChecks` compiler option to be turned on to function correctly.";
const REMOVE: &str = "Remove the default value.";

impl Rule for NoUselessDefaultAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Param,
            NodeKind::BindingProperty,
            NodeKind::AssignPattern,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ti = cx.types();
        match *cx.node(node) {
            Node::Program { .. } => {
                if !ti.strict_null_checks() && !self.allow_without_strict_null_checks {
                    cx.report(Span::new(0, 0), NO_STRICT_NULL_CHECKS)
                        .id("noStrictNullCheck");
                }
            }
            Node::Param {
                pattern,
                type_ann,
                init: Some(def),
                rest: false,
                ..
            } => {
                let target_end = type_ann.map_or(cx.span(pattern).end, |t| cx.span(t).end);
                if is_undefined(cx, def) {
                    if let Some(ta) = type_ann
                        && matches!(cx.node(pattern), Node::Ident { .. })
                    {
                        let fix = Fix::insert(cx.span(pattern).end, "?")
                            .and(Span::new(cx.span(ta).end, cx.span(def).end), "");
                        cx.report_node(def, PREFER_OPTIONAL)
                            .id("preferOptionalSyntax")
                            .suggest(
                                "useOptionalSyntax",
                                "Use the `?` optional syntax instead.",
                                fix,
                            );
                    } else {
                        report_undefined(cx, def, target_end, "parameter");
                    }
                    return;
                }
                if type_ann.is_some() || !matches!(cx.node(pattern), Node::Ident { .. }) {
                    return;
                }
                if contextual_param_type(cx, node).is_some_and(|t| is_defined(ti, t)) {
                    report_useless(cx, def, target_end, "parameter");
                }
            }
            Node::BindingProperty {
                key,
                value,
                init: Some(def),
            } => {
                let target_end = cx.span(value.unwrap_or(key)).end;
                if is_undefined(cx, def) {
                    report_undefined(cx, def, target_end, "property");
                    return;
                }
                let Some(op) = cx.parent(node) else { return };
                let Some(name) = static_key_name(cx, key) else {
                    return;
                };
                let Some(st) = source_type(cx, op, 0) else {
                    return;
                };
                if property_defined(ti, st, &name) {
                    report_useless(cx, def, target_end, "property");
                }
            }
            Node::AssignPattern { left, right } => {
                let Some(ap) = cx.parent(node) else { return };
                let Node::ArrayPattern { elements } = *cx.node(ap) else {
                    return;
                };
                let target_end = cx.span(left).end;
                if is_undefined(cx, right) {
                    report_undefined(cx, right, target_end, "property");
                    return;
                }
                let Some(i) = cx.ast().list(elements).iter().position(|&e| e == node) else {
                    return;
                };
                let Some(st) = source_type(cx, ap, 0) else {
                    return;
                };
                if element_defined(ti, st, i) {
                    report_useless(cx, right, target_end, "property");
                }
            }
            _ => {}
        }
    }
}

fn report_undefined(cx: &mut Ctx<'_, '_>, def: NodeId, target_end: u32, what: &str) {
    let fix = Fix::delete(Span::new(target_end, cx.span(def).end));
    // typescript-eslint pluralizes with a plain `s` ("propertys").
    cx.report_node(
        def,
        format!(
            "Default value is useless because it is undefined. Optional {what}s are already undefined by default."
        ),
    )
        .id("uselessUndefined")
        .suggest("removeDefaultAssignment", REMOVE, fix);
}

fn report_useless(cx: &mut Ctx<'_, '_>, def: NodeId, target_end: u32, what: &str) {
    let fix = Fix::delete(Span::new(target_end, cx.span(def).end));
    cx.report_node(
        def,
        format!("Default value is useless because the {what} is not optional."),
    )
    .id("uselessDefaultAssignment")
    .suggest("removeDefaultAssignment", REMOVE, fix);
}

/// The global `undefined`.
fn is_undefined(cx: &Ctx<'_, '_>, e: NodeId) -> bool {
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    if ident_name(ast, e) != Some("undefined") {
        return false;
    }
    let ti = cx.types();
    match ti.symbol_at(e) {
        None => true,
        Some(s) => ti.symbol_decls(s).iter().all(|d| d.in_lib),
    }
}

/// A static property key: identifier, string or number literal, or a computed literal.
fn static_key_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *cx.node(key) {
        Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
        Node::ComputedKey { expr } => {
            static_value_string(ast, cx.src(), expr).map(|s| s.into_owned())
        }
        _ => static_value_string(ast, cx.src(), key).map(|s| s.into_owned()),
    }
}

/// No part of the type is `undefined`, `void`, `any` or `unknown`.
fn is_defined(ti: &dyn TypeInfo, t: Ty) -> bool {
    if has_unresolved_part(ti, t) {
        return false;
    }
    let t = constrained(ti, t);
    let ps = parts(ti, t);
    !ps.is_empty()
        && ps.iter().all(|&p| {
            let p = constrained(ti, p);
            p != Ty::UNRESOLVED
                && !ti.flags(p).intersects(
                    TypeFlags::UNDEFINED | TypeFlags::VOID | TypeFlags::ANY | TypeFlags::UNKNOWN,
                )
        })
}

/// Every union member declares `name` as a required property that is never `undefined`.
fn property_defined(ti: &dyn TypeInfo, st: Ty, name: &str) -> bool {
    let ps = parts(ti, constrained(ti, st));
    !ps.is_empty()
        && ps.iter().all(|&p| {
            if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
                return false;
            }
            let Some(s) = ti.property_of_type(p, name) else {
                return false;
            };
            !ti.symbol_facts(s).optional && is_defined(ti, ti.type_of_symbol(s))
        })
}

/// Element `i` of a tuple is always present and never `undefined`.
fn element_defined(ti: &dyn TypeInfo, st: Ty, i: usize) -> bool {
    let st = constrained(ti, st);
    if st == Ty::UNRESOLVED || !ti.is_tuple(st) {
        return false;
    }
    let elems = ti.type_args(st);
    match ti.tuple_rest_index(st) {
        None => elems.get(i).is_some_and(|&e| is_defined(ti, e)),
        Some(r) if i < r => is_defined(ti, elems[i]),
        Some(r) => {
            let min_len = elems.len() - 1;
            i < min_len
                && elems[r..].iter().all(|&e| {
                    let e = if ti.is_array(e) {
                        ti.array_element(e).unwrap_or(e)
                    } else {
                        e
                    };
                    is_defined(ti, e)
                })
        }
    }
}

/// The type of element `i` of an array or tuple, for a pattern nested there.
fn element_type(ti: &dyn TypeInfo, st: Ty, i: usize) -> Option<Ty> {
    let st = constrained(ti, st);
    if st == Ty::UNRESOLVED {
        return None;
    }
    if ti.is_tuple(st) {
        let elems = ti.type_args(st);
        return match ti.tuple_rest_index(st) {
            Some(r) if i >= r => None,
            _ => elems.get(i).copied(),
        };
    }
    if ti.is_array(st) {
        return ti.array_element(st);
    }
    None
}

/// The type of the value `pattern` destructures (defaults applied).
pub(super) fn source_type(cx: &Ctx<'_, '_>, pattern: NodeId, depth: u32) -> Option<Ty> {
    if depth > 32 {
        return None;
    }
    let ti = cx.types();
    let parent = cx.parent(pattern)?;
    let t = match *cx.node(parent) {
        Node::Param {
            pattern: p,
            type_ann,
            init,
            rest: false,
            ..
        } if p == pattern => {
            let t = match type_ann {
                Some(ta) => ti.type_of_type_node(ta)?,
                None => contextual_param_type(cx, parent)?,
            };
            if init.is_some() {
                ti.non_nullable(t)
            } else {
                t
            }
        }
        Node::VarDeclarator {
            name,
            type_ann,
            init: Some(init),
            ..
        } if name == pattern => match type_ann {
            Some(ta) => ti.type_of_type_node(ta)?,
            None if has_any_operand(cx, init, 0) => return None,
            None => ti.type_at(init)?,
        },
        Node::BindingProperty {
            key,
            value: Some(v),
            init,
        } if v == pattern => {
            let op = cx.parent(parent)?;
            let st = source_type(cx, op, depth + 1)?;
            let name = static_key_name(cx, key)?;
            let mut types = Vec::new();
            for p in parts(ti, constrained(ti, st)) {
                if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
                    return None;
                }
                let s = ti.property_of_type(p, &name)?;
                types.push(ti.type_of_symbol(s));
            }
            let t = ti.union_of(&types);
            if init.is_some() {
                ti.non_nullable(t)
            } else {
                t
            }
        }
        Node::AssignPattern { left, .. } if left == pattern => {
            let ap = cx.parent(parent)?;
            let i = array_index(cx, ap, parent)?;
            let st = source_type(cx, ap, depth + 1)?;
            ti.non_nullable(element_type(ti, st, i)?)
        }
        Node::ArrayPattern { .. } => {
            let i = array_index(cx, parent, pattern)?;
            let st = source_type(cx, parent, depth + 1)?;
            element_type(ti, st, i)?
        }
        _ => return None,
    };
    (!has_unresolved_part(ti, t)).then_some(t)
}

/// A branch of `?:` or an operand of `&&` / `||` / `??` is `any` (the checker's type for the
/// whole may have dropped it).
fn has_any_operand(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> bool {
    if depth > 32 {
        return true;
    }
    let ti = cx.types();
    let e = skip_parens(cx.ast(), e);
    match *cx.node(e) {
        Node::Conditional { cons, alt, .. } => {
            has_any_operand(cx, cons, depth + 1) || has_any_operand(cx, alt, depth + 1)
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } => has_any_operand(cx, left, depth + 1) || has_any_operand(cx, right, depth + 1),
        _ => ti
            .type_at(e)
            .is_none_or(|t| t == Ty::UNRESOLVED || is_any(ti, t)),
    }
}

fn array_index(cx: &Ctx<'_, '_>, ap: NodeId, element: NodeId) -> Option<usize> {
    let Node::ArrayPattern { elements } = *cx.node(ap) else {
        return None;
    };
    cx.ast().list(elements).iter().position(|&e| e == element)
}

/// The type a function's contextual signature gives an unannotated parameter.
fn contextual_param_type(cx: &Ctx<'_, '_>, param: NodeId) -> Option<Ty> {
    let ti = cx.types();
    let f = cx.parent(param)?;
    let params = match *cx.node(f) {
        Node::FunctionExpr { params, .. } | Node::Arrow { params, .. } => params,
        _ => return None,
    };
    let i = cx.ast().list(params).iter().position(|&p| p == param)?;
    let ct = ti.contextual_type(f)?;
    if ct == Ty::UNRESOLVED || ti.flags(ct).contains(TypeFlags::UNION) {
        return None;
    }
    let sigs = ti.signatures(ct, false);
    let [sig] = sigs.as_slice() else {
        return None;
    };
    let (count, rest) = ti.param_count(*sig);
    if i < count {
        return ti.param_type(*sig, i);
    }
    if rest {
        let r = ti.rest_param_type(*sig)?;
        return if ti.is_array(r) {
            ti.array_element(r)
        } else {
            None
        };
    }
    None
}
