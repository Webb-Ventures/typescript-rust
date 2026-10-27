//! `id-length`: identifiers at least `min` (2) and at most `max` graphemes long, judged on
//! the ESTree view the way ESLint picks which identifiers count.

use super::prelude::*;
use super::ts_ban_ts_comment::graphemes;
use super::unicorn_id_match::is_import_attribute_key;
use crate::esquery::{Tree, Val};
use crate::estree::EsTree;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "id-length",
        "Enforce minimum and maximum identifier lengths",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<IdLength>,
};

#[derive(Debug, Default)]
struct IdLength {
    min: u64,
    max: Option<u64>,
    properties: bool,
    exceptions: Vec<String>,
    patterns: Vec<String>,
}

impl FromOptions for IdLength {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let strs = |k: &str| -> Vec<String> {
            o.get(k)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        Ok(IdLength {
            min: o.get("min").and_then(Value::as_u64).unwrap_or(2),
            max: o.get("max").and_then(Value::as_u64),
            properties: str_field(o, "properties") != Some("never"),
            exceptions: strs("exceptions"),
            patterns: strs("exceptionPatterns"),
        })
    }
}

fn name<'t>(t: &'t EsTree<'_>, n: Option<u32>) -> Option<std::borrow::Cow<'t, str>> {
    match t.attr(n?, &["name"]) {
        Val::Str(s) => Some(s),
        _ => None,
    }
}

fn ty<'t>(t: &'t EsTree<'_>, n: Option<u32>) -> &'t str {
    n.map_or("", |n| t.ty(n))
}

fn is_true(t: &EsTree<'_>, n: u32, key: &str) -> bool {
    t.attr(n, &[key]) == Val::Bool(true)
}

/// An import/export name: an identifier's name or a string's value.
fn export_name(t: &EsTree<'_>, n: Option<u32>) -> Option<String> {
    let n = n?;
    match t.ty(n) {
        "Identifier" => name(t, Some(n)).map(|s| s.into_owned()),
        _ => match t.attr(n, &["value"]) {
            Val::Str(s) => Some(s.into_owned()),
            _ => None,
        },
    }
}

impl IdLength {
    /// ESLint's `SUPPORTED_EXPRESSIONS`: whether `node` under `parent` counts.
    fn counts(&self, t: &EsTree<'_>, parent: u32, node: u32) -> bool {
        let field = |n: u32, k: &str| t.node_field(n, k);
        match t.ty(parent) {
            "MemberExpression" => {
                if !self.properties || is_true(t, parent, "computed") {
                    return false;
                }
                let Some(pp) = t.parent(parent) else {
                    return false;
                };
                (t.ty(pp) == "AssignmentExpression" && field(pp, "left") == Some(parent))
                    || (t.ty(pp) == "Property"
                        && field(pp, "value") == Some(parent)
                        && t.parent(pp).is_some_and(|op| {
                            t.ty(op) == "ObjectPattern"
                                && t.parent(op).and_then(|g| field(g, "left")) == Some(op)
                        }))
            }
            "AssignmentPattern" => field(parent, "left") == Some(node),
            "VariableDeclarator" => field(parent, "id") == Some(node),
            "Property" => {
                if ty(t, t.parent(parent)) == "ObjectPattern" {
                    let value = field(parent, "value");
                    let key = field(parent, "key");
                    let same = name(t, value).is_some() && name(t, value) == name(t, key);
                    return (!same && value == Some(node))
                        || (same && key == Some(node) && self.properties);
                }
                self.properties
                    && !is_import_attribute_key(t, node)
                    && !is_true(t, parent, "computed")
                    && name(t, field(parent, "key")) == name(t, Some(node))
            }
            "ImportSpecifier" => {
                field(parent, "local") == Some(node)
                    && export_name(t, field(parent, "imported"))
                        != export_name(t, field(parent, "local"))
            }
            "ImportDefaultSpecifier"
            | "ImportNamespaceSpecifier"
            | "RestElement"
            | "FunctionExpression"
            | "ArrowFunctionExpression"
            | "ClassDeclaration"
            | "FunctionDeclaration"
            | "MethodDefinition"
            | "PropertyDefinition"
            | "CatchClause"
            | "ArrayPattern" => true,
            _ => false,
        }
    }
}

impl Rule for IdLength {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let tree = EsTree::build(cx.ast(), cx.src());
        let mut seen = rustc_hash::FxHashSet::default();
        let mut out = Vec::new();
        for n in 0..tree.len() as u32 {
            let private = match tree.ty(n) {
                "Identifier" => false,
                "PrivateIdentifier" => true,
                _ => continue,
            };
            let Some(own) = name(&tree, Some(n)) else {
                continue;
            };
            let len = graphemes(&own) as u64;
            let short = len < self.min;
            let long = self.max.is_some_and(|m| len > m);
            if !(short || long)
                || self.exceptions.iter().any(|e| *e == own)
                || self
                    .patterns
                    .iter()
                    .any(|p| crate::regex_exec::test(p, "u", &own))
            {
                continue;
            }
            let Some(parent) = tree.parent(n) else {
                continue;
            };
            if !self.counts(&tree, parent, n) {
                continue;
            }
            let span = tree.span(n);
            if !seen.insert((span.start, span.end)) {
                continue;
            }
            out.push((span, own.into_owned(), short, private));
        }
        for (span, own, short, private) in out {
            let hash = if private { "#" } else { "" };
            let (id, msg) = if short {
                (
                    if private {
                        "tooShortPrivate"
                    } else {
                        "tooShort"
                    },
                    format!(
                        "Identifier name '{hash}{own}' is too short (< {}).",
                        self.min
                    ),
                )
            } else {
                (
                    if private { "tooLongPrivate" } else { "tooLong" },
                    format!(
                        "Identifier name '{hash}{own}' is too long (> {}).",
                        self.max.unwrap_or(0)
                    ),
                )
            };
            cx.report(span, msg).id(id);
        }
    }
}
