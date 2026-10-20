//! S-04 TSR0402 where the forced checker run may not report it: `undefined` written into an
//! optional property whose declared type does not include `undefined` by an assignment
//! (`o.p = undefined`). Object literals are left to the forced run, which reports them at the
//! whole value (TS2375/TS2379). A diagnostic at the same place from the forced run wins (same
//! code and span are deduplicated by the driver).

use ts_r_ast::{Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::Ctx;

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(4) || cx.info.program.options.exact_optional_property_types() == Some(true) {
        return;
    }
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        match cx.node(n) {
            Node::Assign {
                left,
                op: ts_r_ast::AssignOp::Assign,
                right,
            } if matches!(cx.node(left), Node::Member { .. }) => {
                let l = cx.loc(left);
                let Some(tp) = cx.ck.quietly(|ck| ck.property_symbol_of_access(l)) else {
                    continue;
                };
                let name = cx.ck.c.sym_name(tp).to_owned();
                check(cx, tp, right, left, &name);
            }
            _ => {}
        }
    }
}

fn has_undefined(cx: &Ctx<'_, '_, '_>, t: TypeId) -> bool {
    let f = cx.flags(t);
    if f.intersects(TypeFlags::UNDEFINED | TypeFlags::VOID) {
        return true;
    }
    f.contains(TypeFlags::UNION)
        && cx.ck.store().members_of_union(t).iter().any(|&m| {
            cx.flags(m)
                .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID)
        })
}

/// The written type of an optional property declaration (without the `undefined` that `?`
/// adds), if every declaration has one.
fn written_type(cx: &mut Ctx<'_, '_, '_>, p: SymId) -> Option<TypeId> {
    let decls = cx.ck.c.decls(p);
    let d = *decls.first()?;
    let ann = match *cx.ck.c.node(d) {
        Node::PropertySignature { type_ann, .. } | Node::PropertyDef { type_ann, .. } => type_ann?,
        _ => return None,
    };
    let l = ts_r_checker::checker::Loc::new(d.file, ann);
    Some(cx.ck.quietly(|ck| ck.type_from_node(l)))
}

fn check(cx: &mut Ctx<'_, '_, '_>, tp: SymId, value: NodeId, at: NodeId, name: &str) {
    if !cx.ck.c.sym_flags(tp).contains(SymbolFlags::OPTIONAL) {
        return;
    }
    let vt = cx.ty(value);
    if vt.is_any_like() || !has_undefined(cx, vt) {
        return;
    }
    let Some(declared) = written_type(cx, tp) else {
        return;
    };
    if declared.is_any_like()
        || cx.flags(declared).intersects(TypeFlags::ANY_OR_UNKNOWN)
        || has_undefined(cx, declared)
    {
        return;
    }
    let vs = cx.type_str(vt);
    let ds = cx.type_str(declared);
    let tsc = format!(
        "Type '{vs}' is not assignable to type '{ds}' with 'exactOptionalPropertyTypes: true'. Consider adding 'undefined' to the type of the target."
    );
    cx.report(codes::FORCED_EXACT_OPTIONAL, cx.span(at), &[&tsc, name]);
}
