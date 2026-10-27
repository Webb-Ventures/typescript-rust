//! `new-cap`: `new` with a lowercase constructor name, or a call to a capitalized function.

use super::prelude::*;
use crate::regex;
use std::borrow::Cow;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "new-cap",
        "Require constructor names to begin with a capital letter",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NewCap>,
};

/// Built-ins that may be called without `new`.
const CAPS_ALLOWED: &[&str] = &[
    "Array", "Boolean", "Date", "Error", "Function", "Number", "Object", "RegExp", "String",
    "Symbol", "BigInt",
];

#[derive(Debug)]
pub(crate) struct NewCap {
    new_is_cap: bool,
    cap_is_new: bool,
    properties: bool,
    new_is_cap_exceptions: Vec<String>,
    new_is_cap_pattern: Option<String>,
    cap_is_new_exceptions: Vec<String>,
    cap_is_new_pattern: Option<String>,
}

impl FromOptions for NewCap {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let list = |k| {
            o.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let pattern = |k| o.get(k).and_then(Value::as_str).map(str::to_owned);
        Ok(NewCap {
            new_is_cap: bool_field(o, "newIsCap", true),
            cap_is_new: bool_field(o, "capIsNew", true),
            properties: bool_field(o, "properties", true),
            new_is_cap_exceptions: list("newIsCapExceptions"),
            new_is_cap_pattern: pattern("newIsCapExceptionPattern").filter(|p| !p.is_empty()),
            cap_is_new_exceptions: list("capIsNewExceptions"),
            cap_is_new_pattern: pattern("capIsNewExceptionPattern").filter(|p| !p.is_empty()),
        })
    }
}

/// ESLint's static name of a callee: an identifier, or a member's non-private static name.
fn callee_name<'a>(ast: &'a Ast, src: &'a str, callee: NodeId) -> Option<Cow<'a, str>> {
    match *ast.node(callee) {
        Node::Ident { name } => Some(Cow::Borrowed(ast.atom(name))),
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(Cow::Borrowed(ast.atom(name))),
            _ => None,
        },
        Node::Index { index, .. } => static_value_string(ast, src, index),
        _ => None,
    }
}

/// `window` / `global` / `globalThis` / `self` as the global object.
fn is_global_builtin(cx: &Ctx<'_, '_>, node: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let node = skip_parens(ast, node);
    match *cx.node(node) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            callee_name(ast, cx.src(), node).as_deref() == Some(name)
                && ["global", "globalThis", "self", "window"]
                    .iter()
                    .any(|g| cx.is_global_ref(skip_parens(ast, obj), g))
        }
        _ => cx.is_global_ref(node, name),
    }
}

impl NewCap {
    fn is_cap_allowed(
        &self,
        cx: &Ctx<'_, '_>,
        callee: NodeId,
        name: &str,
        allowed: &[String],
        pattern: Option<&str>,
    ) -> bool {
        let text = cx.ast().span(callee).text(cx.src());
        if allowed.iter().any(|a| a == name || a == text) {
            return true;
        }
        if pattern
            .and_then(regex::compile)
            .is_some_and(|re| re.is_match_flags(text, "u"))
        {
            return true;
        }
        if let Node::Member { obj, .. } | Node::Index { obj, .. } = *cx.node(callee) {
            return !self.properties || (name == "UTC" && is_global_builtin(cx, obj, "Date"));
        }
        false
    }
}

impl Rule for NewCap {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, is_new) = match *cx.node(node) {
            Node::New { callee, .. } if self.new_is_cap => (callee, true),
            Node::Call { callee, .. } if self.cap_is_new => (callee, false),
            _ => return,
        };
        let callee = skip_parens(ast, callee);
        let Some(name) = callee_name(ast, cx.src(), callee).filter(|n| !n.is_empty()) else {
            return;
        };
        let Some(first) = name.chars().next() else {
            return;
        };
        let lower: String = first.to_lowercase().collect();
        let upper: String = first.to_uppercase().collect();
        let cap = if lower == upper {
            None
        } else {
            Some(lower == first.to_string())
        };
        let allowed = if is_new {
            cap != Some(true)
                || self.is_cap_allowed(
                    cx,
                    callee,
                    &name,
                    &self.new_is_cap_exceptions,
                    self.new_is_cap_pattern.as_deref(),
                )
        } else {
            cap != Some(false)
                || self.is_cap_allowed(
                    cx,
                    callee,
                    &name,
                    &self.cap_is_new_exceptions,
                    self.cap_is_new_pattern.as_deref(),
                )
                || (CAPS_ALLOWED.contains(&&*name) && is_global_builtin(cx, callee, &name))
        };
        if allowed {
            return;
        }
        let at = match *cx.node(callee) {
            Node::Member { prop, .. } => prop,
            Node::Index { index, .. } => skip_parens(ast, index),
            _ => callee,
        };
        let span = ast.span(at);
        if is_new {
            cx.report(
                span,
                "A constructor name should not start with a lowercase letter.",
            )
            .id("lower");
        } else {
            cx.report(
                span,
                "A function with a name starting with an uppercase letter should only be used as a constructor.",
            )
            .id("upper");
        }
    }
}
