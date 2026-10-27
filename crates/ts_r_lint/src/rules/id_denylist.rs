//! `id-denylist`: identifiers with the configured names, judged on the ESTree view the way
//! ESLint picks which identifiers count.

use super::prelude::*;
use super::unicorn_id_match::{is_global_reference, is_import_attribute_key};
use crate::esquery::{Tree, Val};
use crate::estree::EsTree;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "id-denylist",
        "Disallow specified identifiers",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<IdDenylist>,
};

#[derive(Debug, Default)]
struct IdDenylist {
    names: Vec<String>,
}

impl FromOptions for IdDenylist {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(IdDenylist {
            names: opts
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
        })
    }
}

fn is_true(t: &EsTree<'_>, n: u32, key: &str) -> bool {
    t.attr(n, &[key]) == Val::Bool(true)
}

fn ty<'t>(t: &'t EsTree<'_>, n: Option<u32>) -> &'t str {
    n.map_or("", |n| t.ty(n))
}

/// ESLint's `isAssignmentTarget` for the member expression `m`.
fn assignment_target(t: &EsTree<'_>, m: u32) -> bool {
    let Some(p) = t.parent(m) else {
        return false;
    };
    let f = |k: &str| t.node_field(p, k);
    match t.ty(p) {
        "AssignmentExpression" | "AssignmentPattern" => f("left") == Some(m),
        "ArrayPattern" | "RestElement" => true,
        "Property" => f("value") == Some(m) && ty(t, t.parent(p)) == "ObjectPattern",
        _ => false,
    }
}

fn should_check(cx: &Ctx<'_, '_>, t: &EsTree<'_>, n: u32) -> bool {
    if is_import_attribute_key(t, n) {
        return false;
    }
    let Some(p) = t.parent(n) else {
        return true;
    };
    let f = |k: &str| t.node_field(p, k);
    match t.ty(p) {
        "MetaProperty" => return false,
        "MemberExpression" if f("property") == Some(n) && !is_true(t, p, "computed") => {
            return assignment_target(t, p);
        }
        "CallExpression" | "NewExpression" => return false,
        _ => {}
    }
    let span = |x: Option<u32>| x.map(|x| t.span(x));
    let renamed = match t.ty(p) {
        "ImportSpecifier" => f("imported") == Some(n) && span(f("imported")) != span(f("local")),
        "ExportSpecifier" => {
            t.parent(p)
                .and_then(|d| t.node_field(d, "source"))
                .is_some()
                && f("local") == Some(n)
                && span(f("local")) != span(f("exported"))
        }
        _ => false,
    };
    let destructured_key = t.ty(p) == "Property"
        && !is_true(t, p, "computed")
        && ty(t, t.parent(p)) == "ObjectPattern"
        && f("key") == Some(n);
    // A property key is never a reference (a shorthand's key shares the value's node).
    let key = t.ty(p) == "Property" && f("key") == Some(n) && !is_true(t, p, "computed");
    !renamed
        && !destructured_key
        && !(t.ty(n) == "Identifier" && !key && is_global_reference(cx, t.nodes[n as usize].ast))
}

impl Rule for IdDenylist {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if self.names.is_empty() {
            return;
        }
        let tree = EsTree::build(cx.ast(), cx.src());
        let mut seen = rustc_hash::FxHashSet::default();
        let mut out = Vec::new();
        for n in 0..tree.len() as u32 {
            let private = match tree.ty(n) {
                "Identifier" => false,
                "PrivateIdentifier" => true,
                _ => continue,
            };
            let Val::Str(own) = tree.attr(n, &["name"]) else {
                continue;
            };
            if !self.names.iter().any(|d| *d == own) || !should_check(cx, &tree, n) {
                continue;
            }
            let span = tree.span(n);
            if seen.insert((span.start, span.end)) {
                out.push((span, own.into_owned(), private));
            }
        }
        for (span, own, private) in out {
            let (id, hash) = if private {
                ("restrictedPrivate", "#")
            } else {
                ("restricted", "")
            };
            cx.report(span, format!("Identifier '{hash}{own}' is restricted."))
                .id(id);
        }
    }
}
