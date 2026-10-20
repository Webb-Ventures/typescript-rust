//! `no-undef`: disallow the use of undeclared variables unless mentioned in `/*global */`
//! comments.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-undef",
        "Disallow the use of undeclared variables unless mentioned in `/*global */` comments",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoUndef>,
};

#[derive(Debug, Default)]
struct NoUndef {
    typeof_: bool,
}

impl FromOptions for NoUndef {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUndef {
            typeof_: bool_field(obj_opt(opts, 0), "typeof", false),
        })
    }
}

/// `Object.prototype` members resolve through the global object.
const PROTOTYPE_NAMES: &[&str] = &[
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

impl Rule for NoUndef {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let mut hits = Vec::new();
        for r in sem.references() {
            if r.resolution != Resolution::Unresolved || r.flags.contains(RefFlags::IMPLICIT) {
                continue;
            }
            if !self.typeof_ && r.flags.contains(RefFlags::TYPEOF_EXPR) {
                continue;
            }
            let name = ast.atom(r.name);
            if PROTOTYPE_NAMES.contains(&name) {
                continue;
            }
            hits.push((r.node, name));
        }
        for (node, name) in hits {
            cx.report_node(node, format!("'{name}' is not defined."))
                .id("undef");
        }
    }
}
