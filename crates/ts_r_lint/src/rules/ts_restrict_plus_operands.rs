//! `@typescript-eslint/restrict-plus-operands`: both operands of `+` must be numbers, bigints
//! or strings (plus a few types allowed next to a string).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/restrict-plus-operands",
        "Require both operands of addition to be the same type and be `bigint`, `number`, or `string`",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<RestrictPlusOperands>,
};

#[derive(Debug)]
struct RestrictPlusOperands {
    any: bool,
    boolean: bool,
    nullish: bool,
    number_and_string: bool,
    regexp: bool,
    skip_compound: bool,
    /// ", allowing a string + any of: ..." (empty when nothing extra is allowed).
    suffix: String,
}

impl FromOptions for RestrictPlusOperands {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let any = bool_field(o, "allowAny", true);
        let boolean = bool_field(o, "allowBoolean", true);
        let nullish = bool_field(o, "allowNullish", true);
        let regexp = bool_field(o, "allowRegExp", true);
        let mut names = Vec::new();
        if any {
            names.push("`any`");
        }
        if boolean {
            names.push("`boolean`");
        }
        if nullish {
            names.push("`null`");
        }
        if regexp {
            names.push("`RegExp`");
        }
        if nullish {
            names.push("`undefined`");
        }
        let suffix = if names.is_empty() {
            String::new()
        } else {
            format!(", allowing a string + any of: {}", names.join(", "))
        };
        Ok(RestrictPlusOperands {
            any,
            boolean,
            nullish,
            number_and_string: bool_field(o, "allowNumberAndString", true),
            regexp,
            skip_compound: bool_field(o, "skipCompoundAssignments", false),
            suffix,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    Str,
    Num,
    Big,
    Any,
    Bool,
    Nullish,
    RegExp,
    Other,
}

/// The kinds of the union members of `t`; `None` when the type is not known.
fn kinds(ti: &dyn TypeInfo, t: Ty) -> Option<Vec<K>> {
    let t = constrained(ti, t);
    let mut out = Vec::new();
    for p in parts(ti, t) {
        let p = constrained(ti, p);
        if p == Ty::UNRESOLVED {
            return None;
        }
        let k = if is_any(ti, p) {
            K::Any
        } else if has_flags_or_branded(ti, p, TypeFlags::STRING_LIKE) {
            K::Str
        } else if has_flags_or_branded(ti, p, TypeFlags::NUMBER_LIKE) {
            K::Num
        } else if has_flags_or_branded(ti, p, TypeFlags::BIGINT_LIKE) {
            K::Big
        } else if ti.flags(p).intersects(TypeFlags::BOOLEAN_LIKE) {
            K::Bool
        } else if ti
            .flags(p)
            .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID)
        {
            K::Nullish
        } else if is_builtin_symbol_like(ti, p, &["RegExp"]) {
            K::RegExp
        } else {
            K::Other
        };
        out.push(k);
    }
    Some(out)
}

fn all(ks: &[K], k: K) -> bool {
    !ks.is_empty() && ks.iter().all(|&x| x == k)
}

impl RestrictPlusOperands {
    fn valid(&self, ks: &[K], other: &[K]) -> bool {
        // The extra kinds are only allowed next to a string, not a number.
        let other_numeric = other.iter().any(|&k| matches!(k, K::Num | K::Big));
        ks.iter().all(|&k| match k {
            K::Str | K::Num | K::Big => true,
            K::Any => self.any,
            K::Bool => self.boolean && !other_numeric,
            K::Nullish => self.nullish && !other_numeric,
            K::RegExp => self.regexp && !other_numeric,
            K::Other => false,
        })
    }
}

impl Rule for RestrictPlusOperands {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Assign]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (left, right) = match *cx.node(node) {
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            } => (left, right),
            Node::Assign {
                left,
                op: AssignOp::AddAssign,
                right,
            } if !self.skip_compound => (left, right),
            _ => return,
        };
        let ti = cx.types();
        let (Some(lt), Some(rt)) = (ti.type_at(left), ti.type_at(right)) else {
            return;
        };
        let (Some(lk), Some(rk)) = (kinds(ti, lt), kinds(ti, rt)) else {
            return;
        };
        let name = |k: &[K]| {
            if all(k, K::Big) {
                "bigint"
            } else if all(k, K::Num) {
                "number"
            } else {
                "string"
            }
        };
        if (all(&lk, K::Num) && all(&rk, K::Big)) || (all(&lk, K::Big) && all(&rk, K::Num)) {
            cx.report_node(
                node,
                format!(
                    "Numeric '+' operations must either be both bigints or both numbers. Got `{}` + `{}`.",
                    name(&lk),
                    name(&rk)
                ),
            )
            .id("bigintAndNumber");
            return;
        }
        let mut invalid = false;
        for (operand, t, ks, other) in [(left, lt, &lk, &rk), (right, rt, &rk, &lk)] {
            if self.valid(ks, other) {
                continue;
            }
            invalid = true;
            let ct = constrained(ti, t);
            let shown = if ti.flags(ct).contains(TypeFlags::UNIQUE_ES_SYMBOL) {
                "unique symbol".to_string()
            } else {
                ti.type_to_string(ct)
            };
            cx.report_node(
                operand,
                format!(
                    "Invalid operand for a '+' operation. Operands must each be a number or string{}. Got `{shown}`.",
                    self.suffix
                ),
            )
            .id("invalid");
        }
        if invalid || self.number_and_string {
            return;
        }
        let numeric = |k: &[K]| !k.is_empty() && k.iter().all(|&x| matches!(x, K::Num | K::Big));
        if (all(&lk, K::Str) && numeric(&rk)) || (numeric(&lk) && all(&rk, K::Str)) {
            cx.report_node(
                node,
                format!(
                    "Operands of '+' operations must be a number or string{}. Got `{}` + `{}`.",
                    self.suffix,
                    name(&lk),
                    name(&rk)
                ),
            )
            .id("mismatched");
        }
    }
}
