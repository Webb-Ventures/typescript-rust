//! `@typescript-eslint/no-meaningless-void-operator`: `void` only to discard a call's value.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-meaningless-void-operator",
        "Disallow the `void` operator except when used to discard a value",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .fixable()
    .suggestions(),
    create: create::<NoMeaninglessVoidOperator>,
};

#[derive(Debug)]
struct NoMeaninglessVoidOperator {
    check_never: bool,
}

impl FromOptions for NoMeaninglessVoidOperator {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoMeaninglessVoidOperator {
            check_never: bool_field(obj_opt(opts, 0), "checkNever", false),
        })
    }
}

enum Verdict {
    Ok,
    /// A call whose result is already `void`/`undefined` (or `never`).
    VoidCall(String, bool),
    NonCall,
}

impl Rule for NoMeaninglessVoidOperator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Unary {
            op: UnaryOp::Void,
            arg,
        } = *cx.node(node)
        else {
            return;
        };
        let verdict = self.classify(cx, arg, 0);
        let statement = cx
            .parent(node)
            .is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }));
        let src = cx.src();
        let start = cx.span(node).start;
        let remove = Fix::delete(Span::new(
            start,
            skip_trivia_forward(src, start as usize + 4) as u32,
        ));
        match verdict {
            Verdict::Ok => {}
            Verdict::VoidCall(ty, never) => {
                let r = cx
                    .report_node(
                        node,
                        format!(
                            "void operator shouldn't be used on {ty}; it should convey that a return value is being ignored"
                        ),
                    )
                    .id("meaninglessVoidOperator");
                if never {
                    r.suggest("removeVoid", "Remove 'void'", remove);
                } else if statement {
                    r.fix(remove);
                }
            }
            Verdict::NonCall => {
                let r = cx
                    .report_node(
                        node,
                        "void operator is useless here; it should only discard a call's return value",
                    )
                    .id("meaninglessVoidOnNonCall");
                if statement {
                    r.fix(remove);
                }
            }
        }
    }
}

impl NoMeaninglessVoidOperator {
    fn classify(&self, cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> Verdict {
        if depth > 64 {
            return Verdict::Ok;
        }
        let ast = cx.ast();
        let e = skip_parens(ast, e);
        let ti = cx.types();
        match *cx.node(e) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => {
                // A wrapped call keeps its meaning; a wrapped value is still a value.
                return match self.classify(cx, expr, depth + 1) {
                    Verdict::VoidCall(..) => Verdict::Ok,
                    v => v,
                };
            }
            Node::Seq { exprs } => {
                return match ast.list(exprs).last() {
                    Some(&l) => self.classify(cx, l, depth + 1),
                    None => Verdict::Ok,
                };
            }
            Node::Assign { .. } => return Verdict::Ok,
            Node::NumberLit { .. } if cx.span(e).text(cx.src()) == "0" => return Verdict::Ok,
            Node::Call { .. } => {
                let Some(t) = ti.type_at(e) else {
                    return Verdict::Ok;
                };
                if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
                    return Verdict::Ok;
                }
                let ps = parts(ti, t);
                let void_like = ps
                    .iter()
                    .all(|&p| ti.flags(p).intersects(TypeFlags::VOID_LIKE));
                if void_like {
                    return Verdict::VoidCall(ti.type_to_string(t), false);
                }
                if self.check_never && ti.flags(t).contains(TypeFlags::NEVER) {
                    return Verdict::VoidCall("never".into(), true);
                }
                return Verdict::Ok;
            }
            Node::New { .. } | Node::TaggedTemplate { .. } => return Verdict::Ok,
            _ => {}
        }
        let Some(t) = ti.type_at(e) else {
            return Verdict::Ok;
        };
        if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) || is_any(ti, t) {
            return Verdict::Ok;
        }
        if thenable(ti, constrained(ti, t)) != Some(false) {
            return Verdict::Ok;
        }
        Verdict::NonCall
    }
}
