//! `unicorn/id-match`: ESLint's `id-match` (identifiers must match a `u`-flag regex) plus
//! `checkNamedSpecifiers` (default `true`): when `false`, names in named import specifiers
//! and in `export {…} from` specifiers are left alone, since another module chose them.
//!
//! Which identifiers count follows the core rule, judged on the ESTree view:
//! - references to configured globals, import attribute keys and meta properties never do;
//! - with `properties`, a member property counts when the object has the same name or the
//!   member is assigned to, and object literal keys count;
//! - object pattern bindings count (renamed ones always; same-named ones unless
//!   `ignoreDestructuring`);
//! - imports count by their local name, class fields (and `#private` fields) only with
//!   `classFields`;
//! - anything else counts unless it is a callee or argument, and with `onlyDeclarations`
//!   only directly under a function declaration or a declarator.
//!
//! Each source range is reported once (a shorthand property is two ESTree nodes).

use super::prelude::*;
use super::unicorn_names::RegexOpt;
use crate::esquery::{Tree, Val};
use crate::estree::EsTree;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/id-match",
        "Require identifiers to match a specified regular expression.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<IdMatch>,
};

#[derive(Debug)]
struct IdMatch {
    pattern: String,
    regex: RegexOpt,
    class_fields: bool,
    ignore_destructuring: bool,
    only_declarations: bool,
    properties: bool,
    named_specifiers: bool,
}

impl FromOptions for IdMatch {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let pattern = opts
            .first()
            .and_then(Value::as_str)
            .unwrap_or("^.+$")
            .to_owned();
        let o = obj_opt(opts, 1);
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        Ok(IdMatch {
            regex: RegexOpt {
                source: pattern.clone(),
                flags: "u".to_owned(),
            },
            pattern,
            class_fields: flag("classFields", false),
            ignore_destructuring: flag("ignoreDestructuring", false),
            only_declarations: flag("onlyDeclarations", false),
            properties: flag("properties", false),
            named_specifiers: flag("checkNamedSpecifiers", true),
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

fn inside_object_pattern(t: &EsTree<'_>, n: u32) -> bool {
    let mut cur = t.parent(n);
    for _ in 0..10_000 {
        match cur {
            Some(p) if t.ty(p) == "ObjectPattern" => return true,
            Some(p) => cur = t.parent(p),
            None => return false,
        }
    }
    false
}

/// A reference to a configured (or `/* global */`) variable not declared in the file.
fn is_global_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let sem = cx.semantic();
    match sem.reference_at(id).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Global) => true,
        Some(Resolution::Symbol(s)) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
        _ => false,
    }
}

impl IdMatch {
    fn invalid(&self, name: &str) -> bool {
        !self.regex.test(name)
    }

    fn should_report(&self, t: &EsTree<'_>, effective: Option<u32>, name: &str) -> bool {
        let ety = ty(t, effective);
        (!self.only_declarations || matches!(ety, "FunctionDeclaration" | "VariableDeclarator"))
            && !matches!(ety, "CallExpression" | "NewExpression")
            && self.invalid(name)
    }

    /// Whether the identifier `n` should be reported.
    fn check_identifier(&self, cx: &Ctx<'_, '_>, t: &EsTree<'_>, n: u32) -> bool {
        let Some(own) = name(t, Some(n)) else {
            return false;
        };
        let Some(parent) = t.parent(n) else {
            return self.should_report(t, None, &own);
        };
        let pty = t.ty(parent);
        let effective = if pty == "MemberExpression" {
            t.parent(parent)
        } else {
            Some(parent)
        };
        if is_global_reference(cx, t.nodes[n as usize].ast)
            || (pty == "ImportAttribute" && t.node_field(parent, "key") == Some(n))
            || pty == "MetaProperty"
        {
            return false;
        }
        match pty {
            "MemberExpression" => {
                if !self.properties {
                    return false;
                }
                let object = t.node_field(parent, "object");
                if ty(t, object) == "Identifier" && name(t, object).as_deref() == Some(&own) {
                    return self.invalid(&own);
                }
                let Some(e) = effective.filter(|&e| t.ty(e) == "AssignmentExpression") else {
                    return false;
                };
                let left = t.node_field(e, "left");
                if ty(t, left) == "MemberExpression"
                    && name(t, left.and_then(|l| t.node_field(l, "property"))).as_deref()
                        == Some(&own)
                {
                    return self.invalid(&own);
                }
                ty(t, t.node_field(e, "right")) != "MemberExpression" && self.invalid(&own)
            }
            "Property"
                if ty(t, t.parent(parent)) == "ObjectExpression"
                    && t.node_field(parent, "key") == Some(n)
                    && !is_true(t, parent, "computed") =>
            {
                self.properties && self.invalid(&own)
            }
            "Property" | "AssignmentPattern" => {
                let mut report = false;
                if ty(t, t.parent(parent)) == "ObjectPattern" {
                    let value = t.node_field(parent, "value");
                    if !self.ignore_destructuring
                        && is_true(t, parent, "shorthand")
                        && value.is_some_and(|v| t.node_field(v, "left").is_some())
                        && self.invalid(&own)
                    {
                        report = true;
                    }
                    let key = t.node_field(parent, "key");
                    let key_name = name(t, key);
                    let value_name = name(t, value);
                    let same = key_name == value_name;
                    if !same && key == Some(n) {
                        return report;
                    }
                    if value_name.is_some()
                        && self.invalid(&own)
                        && !(same && self.ignore_destructuring)
                    {
                        report = true;
                    }
                }
                if (!self.properties && !is_true(t, parent, "computed"))
                    || (self.ignore_destructuring && inside_object_pattern(t, n))
                {
                    return report;
                }
                report
                    || (t.node_field(parent, "right") != Some(n)
                        && self.should_report(t, effective, &own))
            }
            "ImportSpecifier" | "ImportNamespaceSpecifier" | "ImportDefaultSpecifier" => {
                name(t, t.node_field(parent, "local")).as_deref() == Some(&own)
                    && self.invalid(&own)
            }
            "PropertyDefinition" => self.class_fields && self.invalid(&own),
            _ => self.should_report(t, effective, &own),
        }
    }

    /// `checkNamedSpecifiers: false` drops reports inside named import specifiers and
    /// `export {…} from` specifiers.
    fn ignored_specifier(&self, t: &EsTree<'_>, n: u32) -> bool {
        if self.named_specifiers {
            return false;
        }
        let parent = t.parent(n);
        match ty(t, parent) {
            "ImportSpecifier" => true,
            "ExportSpecifier" => parent
                .and_then(|p| t.parent(p))
                .is_some_and(|d| t.node_field(d, "source").is_some()),
            _ => false,
        }
    }
}

impl Rule for IdMatch {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let tree = EsTree::build(cx.ast(), cx.src());
        let mut reported: Vec<Span> = Vec::new();
        let mut out: Vec<(Span, String, bool)> = Vec::new();
        for n in 0..tree.len() as u32 {
            let report = match tree.ty(n) {
                "Identifier" => self.check_identifier(cx, &tree, n),
                "PrivateIdentifier" => {
                    let field = ty(&tree, tree.parent(n)) == "PropertyDefinition";
                    (!field || self.class_fields)
                        && name(&tree, Some(n)).is_some_and(|s| self.invalid(&s))
                }
                _ => false,
            };
            if !report || self.ignored_specifier(&tree, n) {
                continue;
            }
            let span = tree.span(n);
            if reported.contains(&span) {
                continue;
            }
            reported.push(span);
            let own = name(&tree, Some(n)).unwrap_or_default().into_owned();
            out.push((span, own, tree.ty(n) == "PrivateIdentifier"));
        }
        for (span, own, private) in out {
            let (id, hash) = if private {
                ("notMatchPrivate", "#")
            } else {
                ("notMatch", "")
            };
            cx.report(
                span,
                format!(
                    "Identifier '{hash}{own}' does not match the pattern '{}'.",
                    self.pattern
                ),
            )
            .id(id);
        }
    }
}
