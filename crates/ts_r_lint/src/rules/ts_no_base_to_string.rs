//! `@typescript-eslint/no-base-to-string`: values stringified with Object's default
//! `toString` (`[object Object]`).

use super::prelude::*;
use rustc_hash::FxHashSet;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-base-to-string",
        "Require `.toString()` and `.toLocaleString()` to only be called on objects which provide useful information when stringified",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<NoBaseToString>,
};

#[derive(Debug)]
struct NoBaseToString {
    check_unknown: bool,
    ignored: Vec<String>,
}

impl FromOptions for NoBaseToString {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let ignored = match o.get("ignoredTypeNames") {
            Some(_) => str_list(o, "ignoredTypeNames"),
            None => ["Error", "RegExp", "URL", "URLSearchParams"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        Ok(NoBaseToString {
            check_unknown: bool_field(o, "checkUnknown", false),
            ignored,
        })
    }
}

/// Whether stringifying a value uses Object's default `toString`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Certainty {
    Always,
    Sometimes,
    Never,
}

use Certainty::{Always, Never, Sometimes};

/// Union semantics: all the same, else "sometimes".
fn any_of(cs: impl IntoIterator<Item = Certainty>) -> Certainty {
    let mut seen_always = false;
    let mut seen_never = false;
    for c in cs {
        match c {
            Always => seen_always = true,
            Never => seen_never = true,
            Sometimes => return Sometimes,
        }
    }
    match (seen_always, seen_never) {
        (true, true) => Sometimes,
        (true, false) => Always,
        _ => Never,
    }
}

struct Checker<'a> {
    ti: &'a dyn TypeInfo,
    rule: &'a NoBaseToString,
    object_to_string: Option<Sym>,
    visiting: FxHashSet<u32>,
    /// A type our checker could not compute was involved: stay silent.
    unknown: bool,
}

impl Checker<'_> {
    fn certainty(&mut self, t: Ty, depth: u32) -> Certainty {
        let ti = self.ti;
        if depth > 32 {
            return Never;
        }
        let t = constrained(ti, t);
        if t == Ty::UNRESOLVED {
            self.unknown = true;
            return Never;
        }
        if is_any(ti, t) {
            return Never;
        }
        if is_unknown(ti, t) {
            return if self.rule.check_unknown {
                Sometimes
            } else {
                Never
            };
        }
        let f = ti.flags(t);
        if f.intersects(
            TypeFlags::STRING_LIKE
                | TypeFlags::NUMBER_LIKE
                | TypeFlags::BIGINT_LIKE
                | TypeFlags::BOOLEAN_LIKE
                | TypeFlags::ENUM_LIKE
                | TypeFlags::ES_SYMBOL_LIKE
                | TypeFlags::NULL
                | TypeFlags::UNDEFINED
                | TypeFlags::VOID
                | TypeFlags::NEVER,
        ) {
            return Never;
        }
        if self.ignored(t, 0) {
            return Never;
        }
        if !self.visiting.insert(t.0) {
            return Never;
        }
        let r = self.certainty_inner(t, f, depth);
        self.visiting.remove(&t.0);
        r
    }

    fn certainty_inner(&mut self, t: Ty, f: TypeFlags, depth: u32) -> Certainty {
        let ti = self.ti;
        if f.contains(TypeFlags::UNION) {
            let ps = ti.union_members(t);
            return any_of(ps.into_iter().map(|p| self.certainty(p, depth + 1)));
        }
        if f.contains(TypeFlags::INTERSECTION) {
            let cs: Vec<Certainty> = ti
                .intersection_members(t)
                .into_iter()
                .map(|m| self.certainty(m, depth + 1))
                .collect();
            if cs.contains(&Never) {
                return Never;
            }
            return any_of(cs);
        }
        if ti.is_tuple(t) {
            let cs: Vec<Certainty> = ti
                .type_args(t)
                .into_iter()
                .map(|e| self.certainty(e, depth + 1))
                .collect();
            if cs.contains(&Always) {
                return Always;
            }
            if cs.contains(&Sometimes) {
                return Sometimes;
            }
            return Never;
        }
        if ti.is_array(t) {
            return match ti.array_element(t) {
                Some(e) => self.certainty(e, depth + 1),
                None => Never,
            };
        }
        self.object_certainty(t)
    }

    /// An object type: does its `toString` (or `valueOf` / `[Symbol.toPrimitive]`) come
    /// from somewhere other than `Object`?
    fn object_certainty(&mut self, t: Ty) -> Certainty {
        let ti = self.ti;
        let at = ti.apparent_type(t);
        if at == Ty::UNRESOLVED {
            self.unknown = true;
            return Never;
        }
        let Some(to_string) = ti.property_of_type(at, "toString") else {
            self.unknown = true;
            return Never;
        };
        let Some(object_to_string) = self.object_to_string else {
            self.unknown = true;
            return Never;
        };
        // A `toString` without declarations (a non-homomorphic mapped type's member) counts
        // as Object's (observed: `{ [K in 'toString']: () => string }` is reported).
        if to_string != object_to_string && !ti.symbol_decls(to_string).is_empty() {
            return Never;
        }
        let object = ti.global_type("Object");
        for name in ["valueOf"] {
            if let Some(s) = ti.property_of_type(at, name)
                && object
                    .and_then(|o| ti.property_of_type(o, name))
                    .is_none_or(|os| os != s)
            {
                return Never;
            }
        }
        if let Some(prim) = ti.well_known_symbol("toPrimitive")
            && ti.property_of_type(at, &prim).is_some()
        {
            return Never;
        }
        Always
    }

    /// The type (or an alias, or a base type) is named in `ignoredTypeNames`.
    fn ignored(&self, t: Ty, depth: u32) -> bool {
        let ti = self.ti;
        if depth > 16 || self.rule.ignored.is_empty() {
            return false;
        }
        for s in [ti.alias_symbol(t), ti.type_symbol(t)]
            .into_iter()
            .flatten()
        {
            let name = ti.symbol_name(s);
            if self.rule.ignored.contains(&name) {
                return true;
            }
        }
        ti.base_types(t)
            .into_iter()
            .any(|b| self.ignored(b, depth + 1))
    }
}

impl NoBaseToString {
    fn checker<'a>(&'a self, ti: &'a dyn TypeInfo) -> Checker<'a> {
        let object_to_string = ti
            .global_type("Object")
            .and_then(|o| ti.property_of_type(o, "toString"));
        Checker {
            ti,
            rule: self,
            object_to_string,
            visiting: FxHashSet::default(),
            unknown: false,
        }
    }

    /// Reports `node` when stringifying it may use the default format.
    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, join: bool) {
        let ti = cx.types();
        let Some(t) = ti.type_at(node) else { return };
        let mut ck = self.checker(ti);
        let c = if join {
            // `.join()` stringifies the elements of an array or tuple; other union members
            // don't take part.
            let t = constrained(ti, t);
            let mut any_array = false;
            let mut cs = Vec::new();
            for p in parts(ti, t) {
                let p = constrained(ti, p);
                let members =
                    if p != Ty::UNRESOLVED && ti.flags(p).contains(TypeFlags::INTERSECTION) {
                        ti.intersection_members(p)
                    } else {
                        vec![p]
                    };
                if members
                    .iter()
                    .any(|&m| m != Ty::UNRESOLVED && (ti.is_array(m) || ti.is_tuple(m)))
                {
                    any_array = true;
                    cs.push(ck.certainty(p, 0));
                } else {
                    cs.push(Never);
                }
            }
            if !any_array {
                return;
            }
            any_of(cs)
        } else {
            ck.certainty(t, 0)
        };
        if ck.unknown || c == Never {
            return;
        }
        let certainty = if c == Always { "will" } else { "may" };
        let text = cx.span(node).text(cx.src()).to_string();
        if join {
            cx.report_node(
                node,
                format!(
                    "Using `join()` for {text} {certainty} use Object's default stringification format ('[object Object]') when stringified."
                ),
            )
            .id("baseArrayJoin");
        } else {
            cx.report_node(
                node,
                format!(
                    "'{text}' {certainty} use Object's default stringification format ('[object Object]') when stringified."
                ),
            )
            .id("baseToString");
        }
    }
}

fn is_string_like(ti: &dyn TypeInfo, node: NodeId) -> bool {
    ti.type_at(node)
        .is_some_and(|t| has_flags_or_branded(ti, constrained(ti, t), TypeFlags::STRING_LIKE))
}

impl Rule for NoBaseToString {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::TemplateLit,
            NodeKind::Call,
            NodeKind::Binary,
            NodeKind::Assign,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::TemplateLit { parts: list } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                for &e in ast.list(list) {
                    if !matches!(cx.node(e), Node::TemplateElement { .. }) {
                        self.check(cx, e, false);
                    }
                }
            }
            Node::Call { callee, args, .. } => {
                let callee = skip_parens(ast, callee);
                match *cx.node(callee) {
                    Node::Member { obj, prop, .. } => match ident_name(ast, prop) {
                        Some("toString" | "toLocaleString") => {
                            self.check(cx, skip_parens(ast, obj), false)
                        }
                        Some("join") => self.check(cx, skip_parens(ast, obj), true),
                        _ => {}
                    },
                    Node::Ident { .. } if ident_name(ast, callee) == Some("String") => {
                        let ti = cx.types();
                        let global = ti.symbol_at(callee).is_some_and(|s| is_lib_symbol(ti, s));
                        if !global {
                            return;
                        }
                        if let Some(&a) = ast.list(args).first()
                            && !matches!(cx.node(a), Node::Spread { .. })
                        {
                            self.check(cx, skip_parens(ast, a), false);
                        }
                    }
                    _ => {}
                }
            }
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            }
            | Node::Assign {
                left,
                op: AssignOp::AddAssign,
                right,
            } => {
                let ti = cx.types();
                let (ls, rs) = (is_string_like(ti, left), is_string_like(ti, right));
                if ls {
                    self.check(cx, skip_parens(ast, right), false);
                }
                if rs {
                    self.check(cx, skip_parens(ast, left), false);
                }
            }
            _ => {}
        }
    }
}
