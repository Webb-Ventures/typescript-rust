//! `@typescript-eslint/only-throw-error`: only `Error` objects may be thrown.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/only-throw-error",
        "Disallow throwing non-`Error` values as exceptions",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<OnlyThrowError>,
};

#[derive(Debug)]
struct OnlyThrowError {
    allow: Vec<Specifier>,
    any: bool,
    unknown: bool,
    rethrowing: bool,
}

impl FromOptions for OnlyThrowError {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(OnlyThrowError {
            allow: parse_specifiers(o.get("allow")),
            any: bool_field(o, "allowThrowingAny", true),
            unknown: bool_field(o, "allowThrowingUnknown", true),
            rethrowing: bool_field(o, "allowRethrowing", true),
        })
    }
}

/// Whether a value of type `t` is an error object (or allowed): `None` when not known.
pub(crate) fn error_like(
    ti: &dyn TypeInfo,
    t: Ty,
    allow: &[Specifier],
    any: bool,
    unknown: bool,
) -> Option<bool> {
    if t == Ty::UNRESOLVED {
        return None;
    }
    if is_any(ti, t) {
        return Some(any);
    }
    if is_unknown(ti, t) {
        return Some(unknown);
    }
    if type_or_base_matches(ti, t, allow) {
        return Some(true);
    }
    let f = ti.flags(t);
    if f.contains(TypeFlags::TYPE_PARAMETER) {
        // Only a constraint can make a type parameter an error.
        let c = ti.base_constraint(t);
        if c == Ty::UNRESOLVED {
            return None;
        }
        if is_unknown(ti, c) || is_any(ti, c) {
            return Some(false);
        }
        return error_like(ti, c, allow, any, unknown);
    }
    if f.contains(TypeFlags::UNION) {
        let mut all = true;
        for p in ti.union_members(t) {
            match error_like(ti, p, allow, any, unknown) {
                None => return None,
                Some(false) => all = false,
                Some(true) => {}
            }
        }
        return Some(all);
    }
    if f.intersects(TypeFlags::INSTANTIABLE) {
        let c = constrained(ti, t);
        if c == t {
            return None;
        }
        return error_like(ti, c, allow, any, unknown);
    }
    if f.contains(TypeFlags::INTERSECTION) {
        let mut known = true;
        for m in ti.intersection_members(t) {
            match error_like(ti, m, allow, any, unknown) {
                Some(true) => return Some(true),
                None => known = false,
                Some(false) => {}
            }
        }
        return known.then_some(false);
    }
    // `Readonly<E>` of an error is an error.
    if let Some(a) = ti.alias_symbol(t)
        && ti.symbol_name(a) == "Readonly"
        && is_lib_symbol(ti, a)
        && let [arg] = ti.alias_type_args(t)[..]
        && arg != t
    {
        return error_like(ti, arg, allow, any, unknown);
    }
    Some(is_builtin_symbol_like(ti, t, &["Error"]))
}

impl Rule for OnlyThrowError {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Throw]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Throw { arg } = *cx.node(node) else {
            return;
        };
        let arg = skip_parens(cx.ast(), arg);
        if self.rethrowing && is_rethrow(cx, arg) {
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(arg) else { return };
        if t != Ty::UNRESOLVED && ti.flags(t) == TypeFlags::UNDEFINED {
            let allowed = self.allow.iter().any(|s| {
                matches!(s.from, SpecFrom::Lib | SpecFrom::Any)
                    && s.names.iter().any(|n| n == "undefined")
            });
            if !allowed {
                cx.report_node(arg, "Do not throw undefined.").id("undef");
            }
            return;
        }
        if error_like(ti, t, &self.allow, self.any, self.unknown) == Some(false) {
            cx.report_node(arg, "Expected an error object to be thrown.")
                .id("object");
        }
    }
}

/// `arg` names a caught error: a `catch (e)` binding, or the first parameter of a
/// `.catch(e => ..)` / `.then(_, e => ..)` rejection callback.
fn is_rethrow(cx: &Ctx<'_, '_>, arg: NodeId) -> bool {
    if !matches!(cx.node(arg), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_ident(arg) else {
        return false;
    };
    let Some(decl) = sem.symbol(sym).decls.first() else {
        return false;
    };
    match decl.kind {
        DeclKind::CatchParam => {
            matches!(*cx.node(decl.node), Node::CatchClause { param: Some(p), .. } if p == decl.name)
        }
        DeclKind::Param => {
            let Node::Param {
                rest: false,
                pattern,
                ..
            } = *cx.node(decl.node)
            else {
                return false;
            };
            if pattern != decl.name {
                return false;
            }
            let Some(func) = cx.parent(decl.node) else {
                return false;
            };
            let params = match *cx.node(func) {
                Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => params,
                _ => return false,
            };
            if cx.ast().list(params).first() != Some(&decl.node) {
                return false;
            }
            is_rejection_callback(cx, func)
        }
        _ => false,
    }
}

/// `func` is the rejection handler of a `.catch()` / `.then()` call on a thenable.
pub(crate) fn is_rejection_callback(cx: &Ctx<'_, '_>, func: NodeId) -> bool {
    let ast = cx.ast();
    let Some(call) = cx.parent(func) else {
        return false;
    };
    let Node::Call { callee, args, .. } = *cx.node(call) else {
        return false;
    };
    let Node::Member { obj, prop, .. } = *cx.node(skip_parens(ast, callee)) else {
        return false;
    };
    let index = match ident_name(ast, prop) {
        Some("catch") => 0,
        Some("then") => 1,
        _ => return false,
    };
    let args = ast.list(args);
    if args
        .iter()
        .take(index + 1)
        .any(|&a| matches!(cx.node(a), Node::Spread { .. }))
    {
        return false;
    }
    if args.get(index) != Some(&func) {
        return false;
    }
    let ti = cx.types();
    ti.type_at(obj).is_some_and(|t| is_thenable(ti, t))
}
