//! `unicorn/no-keyword-prefix`: identifiers named like `newFoo` or `classBar` (a keyword
//! followed by an uppercase letter, or by anything with `onlyCamelCase: false`).
//!
//! Runs on the ESTree view, since which identifiers count depends on ESTree parents:
//! - member properties count only when the object has the same name (`newFoo.newFoo`) or
//!   the member is the target of an assignment (`a.newFoo = 1`, but not `x = a.newFoo`);
//! - in object patterns the local binding counts (and a computed key), not the key;
//! - imports count by their local name;
//! - callees and arguments (`foo(newBar)`, `newFoo()`) do not count, nor do default
//!   values.
//!
//! `checkProperties: false` skips member properties and object literal keys. A shorthand
//! property is reported once.

use super::prelude::*;
use crate::esquery::{Tree, Val};
use crate::estree::EsTree;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-keyword-prefix",
        "Disallow identifiers starting with `new` or `class`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoKeywordPrefix>,
};

#[derive(Debug)]
struct NoKeywordPrefix {
    prefixes: Vec<String>,
    check_properties: bool,
    only_camel_case: bool,
}

impl FromOptions for NoKeywordPrefix {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let prefixes = match o.get("disallowedPrefixes").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            None => vec!["new".to_owned(), "class".to_owned()],
        };
        let flag = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(true);
        Ok(NoKeywordPrefix {
            prefixes,
            check_properties: flag("checkProperties"),
            only_camel_case: flag("onlyCamelCase"),
        })
    }
}

impl NoKeywordPrefix {
    fn keyword(&self, name: &str) -> Option<&str> {
        self.prefixes.iter().map(String::as_str).find(|k| {
            name.strip_prefix(k)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| !self.only_camel_case || c.is_ascii_uppercase())
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

struct Reporter<'k> {
    reported: Vec<u32>,
    out: Vec<(Span, &'k str)>,
}

impl<'k> Reporter<'k> {
    fn report(&mut self, t: &EsTree<'_>, n: u32, keyword: &'k str) {
        let span = t.span(n);
        if !self.reported.contains(&span.start) {
            self.reported.push(span.start);
            self.out.push((span, keyword));
        }
    }
}

impl NoKeywordPrefix {
    fn check<'k>(&'k self, t: &EsTree<'_>, n: u32, r: &mut Reporter<'k>) {
        let Some(own) = name(t, Some(n)) else { return };
        let keyword = self.keyword(&own);
        let Some(parent) = t.parent(n) else {
            if let Some(k) = keyword {
                r.report(t, n, k);
            }
            return;
        };
        let parent_ty = t.ty(parent);
        let effective = if parent_ty == "MemberExpression" {
            t.parent(parent)
        } else {
            Some(parent)
        };
        let call_like = matches!(ty(t, effective), "CallExpression" | "NewExpression");
        match parent_ty {
            "MemberExpression" => {
                let Some(k) = keyword.filter(|_| self.check_properties) else {
                    return;
                };
                let object = t.node_field(parent, "object");
                if ty(t, object) == "Identifier" && name(t, object).as_deref() == Some(&own) {
                    r.report(t, n, k);
                    return;
                }
                if let Some(assign) = effective
                    && t.ty(assign) == "AssignmentExpression"
                {
                    let left = t.node_field(assign, "left");
                    let right = t.node_field(assign, "right");
                    if (ty(t, right) != "MemberExpression" || ty(t, left) == "MemberExpression")
                        && name(t, left.and_then(|l| t.node_field(l, "property"))).as_deref()
                            == Some(&own)
                    {
                        r.report(t, n, k);
                    }
                }
            }
            "Property" | "AssignmentPattern" => {
                let property = if parent_ty == "AssignmentPattern"
                    && t.node_field(parent, "left") == Some(n)
                {
                    t.parent(parent)
                } else {
                    Some(parent)
                };
                if let Some(prop) = property
                    && t.ty(prop) == "Property"
                    && ty(t, t.parent(prop)) == "ObjectPattern"
                {
                    let computed = t.attr(prop, &["computed"]) == Val::Bool(true);
                    if let Some(k) = keyword
                        && computed
                    {
                        r.report(t, n, k);
                    }
                    let key = t.node_field(prop, "key");
                    if key == Some(n) {
                        return;
                    }
                    if name(t, key).as_deref() != Some(&own) {
                        if let Some(k) = keyword {
                            r.report(t, n, k);
                        }
                        return;
                    }
                }
                if !self.check_properties {
                    return;
                }
                if let Some(k) = keyword
                    && !call_like
                    && t.node_field(parent, "right") != Some(n)
                {
                    r.report(t, n, k);
                }
            }
            "ImportSpecifier" | "ImportNamespaceSpecifier" | "ImportDefaultSpecifier" => {
                if let Some(k) = keyword
                    && name(t, t.node_field(parent, "local")).as_deref() == Some(&own)
                {
                    r.report(t, n, k);
                }
            }
            _ => {
                if let Some(k) = keyword
                    && !call_like
                {
                    r.report(t, n, k);
                }
            }
        }
    }
}

impl Rule for NoKeywordPrefix {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if self.prefixes.is_empty() || !self.prefixes.iter().any(|p| cx.src().contains(p.as_str()))
        {
            return;
        }
        let tree = EsTree::build(cx.ast(), cx.src());
        let mut r = Reporter {
            reported: Vec::new(),
            out: Vec::new(),
        };
        for n in 0..tree.len() as u32 {
            if tree.ty(n) == "Identifier" {
                self.check(&tree, n, &mut r);
            }
        }
        for (span, keyword) in r.out {
            cx.report(
                span,
                format!("Do not prefix identifiers with keyword `{keyword}`."),
            )
            .id("noKeywordPrefix");
        }
    }
}
