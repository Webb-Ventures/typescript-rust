//! `@typescript-eslint/no-unsafe-argument`: disallow passing `any` as a function argument.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-argument",
        "Disallow calling a function with a value with type `any`",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoUnsafeArgument>,
};

#[derive(Debug, Default)]
struct NoUnsafeArgument;

/// The parameter types of a signature, in argument order.
struct Params {
    fixed: Vec<Ty>,
    /// Elements of a rest parameter's tuple type (before its own rest element).
    tuple: Vec<Ty>,
    /// The type every later argument gets (a rest array's element type).
    rest: Option<Ty>,
    next: usize,
}

impl Params {
    fn new(ti: &dyn TypeInfo, sig: Sig) -> Option<Params> {
        let (count, has_rest) = ti.param_count(sig);
        let fixed: Vec<Ty> = (0..count)
            .map(|i| ti.param_type(sig, i).unwrap_or(Ty::UNRESOLVED))
            .collect();
        let mut tuple = Vec::new();
        let mut rest = None;
        if has_rest {
            let r = ti.rest_param_type(sig)?;
            if r == Ty::UNRESOLVED || ti.flags(r).intersects(TypeFlags::INSTANTIABLE) {
                return None;
            }
            if is_any(ti, r) {
                rest = Some(Ty::ANY);
            } else if ti.is_tuple(r) {
                let args = ti.type_args(r);
                match ti.tuple_rest_index(r) {
                    Some(i) => {
                        tuple = args[..i.min(args.len())].to_vec();
                        rest = args.get(i).copied();
                    }
                    None => tuple = args,
                }
            } else if ti.is_array(r) {
                rest = ti.array_element(r);
            } else {
                return None;
            }
        }
        Some(Params {
            fixed,
            tuple,
            rest,
            next: 0,
        })
    }

    fn next(&mut self) -> Option<Ty> {
        let i = self.next;
        self.next += 1;
        if let Some(&t) = self.fixed.get(i) {
            return Some(t);
        }
        let j = i - self.fixed.len();
        if let Some(&t) = self.tuple.get(j) {
            return Some(t);
        }
        self.rest
    }
}

impl Rule for NoUnsafeArgument {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New, NodeKind::TaggedTemplate]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (args, skip_first): (Vec<NodeId>, bool) = match *cx.node(node) {
            Node::Call { args, .. } | Node::New { args, .. } => (ast.list(args).to_vec(), false),
            Node::TaggedTemplate { quasi, .. } => {
                let Node::TemplateLit { parts } = *cx.node(quasi) else {
                    return;
                };
                let exprs: Vec<NodeId> = ast
                    .list(parts)
                    .iter()
                    .copied()
                    .filter(|&p| !matches!(cx.node(p), Node::TemplateElement { .. }))
                    .collect();
                (exprs, true)
            }
            _ => return,
        };
        if args.is_empty() {
            return;
        }
        let ti = cx.types();
        let sig = ti.resolved_signature(node).or_else(|| {
            // Tagged templates: the tag's only signature.
            let Node::TaggedTemplate { tag, .. } = *cx.node(node) else {
                return None;
            };
            match ti.signatures(ti.type_at(tag)?, false)[..] {
                [s] => Some(s),
                _ => None,
            }
        });
        let Some(sig) = sig else {
            return;
        };
        let Some(mut params) = Params::new(ti, sig) else {
            return;
        };
        if skip_first {
            params.next();
        }
        for arg in args {
            if let Node::Spread { arg: inner } = *cx.node(arg) {
                let Some(t) = ti.type_at(inner) else { return };
                if t == Ty::UNRESOLVED {
                    return;
                }
                if is_any(ti, t) {
                    cx.report_node(arg, "Unsafe spread of an `any` type.")
                        .id("unsafeSpread");
                    return;
                }
                if is_any_array(ti, t) {
                    let msg = match ti.array_element(t).and_then(|e| any_kind(ti, e)) {
                        Some(AnyKind::Error) => "Unsafe spread of an error array type.",
                        _ => "Unsafe spread of an `any[]` array type.",
                    };
                    cx.report_node(arg, msg).id("unsafeArraySpread");
                    return;
                }
                if !ti.is_tuple(t) {
                    // An array of unknown length: later arguments can't be matched up.
                    return;
                }
                let elems = ti.type_args(t);
                let fixed = ti
                    .tuple_rest_index(t)
                    .map_or(elems.len(), |i| i.min(elems.len()));
                let mut reported = false;
                for &e in &elems[..fixed] {
                    let Some(p) = params.next() else { return };
                    if reported {
                        continue;
                    }
                    if let Some(desc) = unsafe_value(ti, e, p, None, cx) {
                        let pt = ti.type_to_string(p);
                        let what = match desc {
                            Desc::Error => "is error typed".to_string(),
                            Desc::Type(s) => format!("is of type `{s}`"),
                        };
                        cx.report_node(
                            arg,
                            format!(
                                "Unsafe spread of a tuple type. The argument {what} and is assigned to a parameter of type `{pt}`."
                            ),
                        )
                        .id("unsafeTupleSpread");
                        reported = true;
                    }
                }
                continue;
            }
            let Some(p) = params.next() else { return };
            let Some(t) = constrained_type_at(ti, arg) else {
                continue;
            };
            if let Some(desc) = unsafe_value(ti, t, p, Some(arg), cx) {
                let pt = ti.type_to_string(p);
                let what = match desc {
                    Desc::Error => "error typed".to_string(),
                    Desc::Type(s) => format!("`{s}`"),
                };
                cx.report_node(
                    arg,
                    format!(
                        "Unsafe argument of type {what} assigned to a parameter of type `{pt}`."
                    ),
                )
                .id("unsafeArgument");
            }
        }
    }
}

enum Desc {
    Error,
    Type(String),
}

/// Whether passing a value of type `t` (from `node`, when known) to parameter type `p` is
/// unsafe, and how to describe the value.
fn unsafe_value(
    ti: &dyn TypeInfo,
    t: Ty,
    p: Ty,
    node: Option<NodeId>,
    cx: &Ctx<'_, '_>,
) -> Option<Desc> {
    if t == Ty::UNRESOLVED || p == Ty::UNRESOLVED {
        return None;
    }
    if let Some(k) = any_kind(ti, t) {
        if is_any(ti, p) || is_unknown(ti, p) {
            return None;
        }
        return Some(match k {
            AnyKind::Error => Desc::Error,
            AnyKind::Any => Desc::Type("any".into()),
        });
    }
    if let Some(n) = node
        && let Node::New {
            args,
            type_args: None,
            ..
        } = *cx.node(skip_parens(cx.ast(), n))
        && cx.ast().list(args).is_empty()
    {
        return None;
    }
    is_unsafe_assignment(ti, t, p).then(|| Desc::Type(ti.type_to_string(t)))
}
