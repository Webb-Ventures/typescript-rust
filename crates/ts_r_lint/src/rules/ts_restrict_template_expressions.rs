//! `@typescript-eslint/restrict-template-expressions`: template literal expressions must be
//! strings (or other types the options allow).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/restrict-template-expressions",
        "Enforce template literal expressions to be of `string` type",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<RestrictTemplateExpressions>,
};

#[derive(Debug)]
pub(crate) struct Allowed {
    pub any: bool,
    pub array: bool,
    pub boolean: bool,
    pub never: bool,
    pub nullish: bool,
    pub number: bool,
    pub regexp: bool,
    pub allow: Vec<Specifier>,
}

impl Allowed {
    pub(crate) fn from_object(o: &serde_json::Map<String, Value>) -> Allowed {
        let allow = match o.get("allow") {
            Some(v) => parse_specifiers(Some(v)),
            None => vec![Specifier {
                names: vec!["Error".into(), "URL".into(), "URLSearchParams".into()],
                from: SpecFrom::Lib,
            }],
        };
        Allowed {
            any: bool_field(o, "allowAny", true),
            array: bool_field(o, "allowArray", false),
            boolean: bool_field(o, "allowBoolean", true),
            never: bool_field(o, "allowNever", false),
            nullish: bool_field(o, "allowNullish", true),
            number: bool_field(o, "allowNumber", true),
            regexp: bool_field(o, "allowRegExp", true),
            allow,
        }
    }

    /// Every union member of `t` is allowed (`None` when the type is not known).
    pub(crate) fn ok(&self, ti: &dyn TypeInfo, t: Ty, depth: u32) -> Option<bool> {
        if t == Ty::UNRESOLVED || depth > 16 {
            return None;
        }
        if self.ok_part(ti, t, depth)? {
            return Some(true);
        }
        if !ti.flags(t).contains(TypeFlags::UNION) {
            return Some(false);
        }
        for p in parts(ti, t) {
            if !self.ok_part(ti, constrained(ti, p), depth)? {
                return Some(false);
            }
        }
        Some(true)
    }

    fn ok_part(&self, ti: &dyn TypeInfo, t: Ty, depth: u32) -> Option<bool> {
        if t == Ty::UNRESOLVED {
            return None;
        }
        if has_flags_or_branded(ti, t, TypeFlags::STRING_LIKE) {
            return Some(true);
        }
        if self.number
            && has_flags_or_branded(ti, t, TypeFlags::NUMBER_LIKE | TypeFlags::BIGINT_LIKE)
        {
            return Some(true);
        }
        if self.boolean && has_flags_or_branded(ti, t, TypeFlags::BOOLEAN_LIKE) {
            return Some(true);
        }
        if self.nullish
            && ti
                .flags(t)
                .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID)
        {
            return Some(true);
        }
        if self.any && is_any(ti, t) {
            return Some(true);
        }
        if self.never && ti.flags(t).contains(TypeFlags::NEVER) {
            return Some(true);
        }
        if self.regexp && is_builtin_symbol_like(ti, t, &["RegExp"]) {
            return Some(true);
        }
        if type_or_base_matches(ti, t, &self.allow) {
            return Some(true);
        }
        if self.array && (ti.is_array(t) || ti.is_tuple(t)) {
            let elems = if ti.is_tuple(t) {
                ti.type_args(t)
            } else {
                ti.array_element(t).into_iter().collect()
            };
            for e in elems {
                if !self.ok(ti, constrained(ti, e), depth + 1)? {
                    return Some(false);
                }
            }
            return Some(true);
        }
        Some(false)
    }
}

#[derive(Debug)]
struct RestrictTemplateExpressions {
    allowed: Allowed,
}

impl FromOptions for RestrictTemplateExpressions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(RestrictTemplateExpressions {
            allowed: Allowed::from_object(obj_opt(opts, 0)),
        })
    }
}

impl Rule for RestrictTemplateExpressions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TemplateLit]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if cx
            .parent(node)
            .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
        {
            return;
        }
        let Node::TemplateLit { parts: list } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        for &e in ast.list(list) {
            if matches!(cx.node(e), Node::TemplateElement { .. }) {
                continue;
            }
            let ti = cx.types();
            let Some(t) = constrained_type_at(ti, e) else {
                continue;
            };
            if self.allowed.ok(ti, t, 0) == Some(false) {
                let name = ti.type_to_string(t);
                cx.report_node(
                    e,
                    format!("Invalid type \"{name}\" of template literal expression."),
                )
                .id("invalidType");
            }
        }
    }
}
