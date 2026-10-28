//! `no-restricted-globals`: report references to configured global names.

use rustc_hash::FxHashMap;

use super::prelude::*;
use crate::semantic::{RefFlags, Resolution, SymbolFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-restricted-globals",
        "Disallow specified global variables",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoRestrictedGlobals>,
};

#[derive(Debug, Default)]
struct NoRestrictedGlobals {
    /// Name -> custom message.
    restricted: FxHashMap<String, Option<String>>,
    /// Global objects whose properties count as the global (`window.foo`), when
    /// `checkGlobalObject` is on.
    global_objects: Vec<String>,
}

impl FromOptions for NoRestrictedGlobals {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut out = Self::default();
        let wrapped = opts
            .first()
            .and_then(Value::as_object)
            .filter(|o| o.contains_key("globals"));
        let list: &[Value] = match wrapped {
            Some(o) => {
                if bool_field(o, "checkGlobalObject", false) {
                    out.global_objects =
                        ["globalThis", "self", "window"].map(String::from).to_vec();
                    out.global_objects.extend(str_list(o, "globalObjects"));
                }
                o.get("globals")
                    .and_then(Value::as_array)
                    .map_or(&[], Vec::as_slice)
            }
            None => opts,
        };
        for item in list {
            match item {
                Value::String(s) => {
                    out.restricted.insert(s.clone(), None);
                }
                Value::Object(o) => {
                    if let Some(name) = str_field(o, "name") {
                        let message = str_field(o, "message").map(str::to_owned);
                        out.restricted.insert(name.to_owned(), message);
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

impl NoRestrictedGlobals {
    fn report(&self, cx: &mut Ctx<'_, '_>, span: Span, name: &str) {
        let Some(message) = self.restricted.get(name) else {
            return;
        };
        match message.as_deref().filter(|m| !m.is_empty()) {
            Some(m) => {
                let msg = format!("Unexpected use of '{name}'. {m}");
                cx.report(span, msg).id("customMessage");
            }
            None => {
                let msg = format!("Unexpected use of '{name}'.");
                cx.report(span, msg).id("defaultMessage");
            }
        }
    }
}

impl Rule for NoRestrictedGlobals {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.restricted.is_empty() {
            return;
        }
        let ast = cx.ast();
        let Some(name) = ident_name(ast, node) else {
            return;
        };
        if self.restricted.contains_key(name) && is_global_value_ref(cx, node) {
            let span = cx.span(node);
            self.report(cx, span, name);
        }
        // `window.foo`, `window.window.foo`.
        if !self.global_objects.iter().any(|g| g == name)
            || !cx.is_global_ref(node, name)
            || !cx.global_available(node, name)
        {
            return;
        }
        let parents = cx.file.parents;
        let src = cx.src();
        let mut cur = node;
        for _ in 0..1_000 {
            let Some(p) = parent_skip_parens(ast, parents, cur) else {
                return;
            };
            let (obj, prop) = match *ast.node(p) {
                Node::Member { obj, prop, .. } => (obj, prop),
                Node::Index { obj, index, .. } => (obj, index),
                _ => return,
            };
            if skip_parens(ast, obj) != cur {
                return;
            }
            if matches!(ast.node(prop), Node::PrivateName { .. }) {
                return;
            }
            let Some(prop_name) = member_name(ast, src, p) else {
                return;
            };
            if prop_name == name {
                cur = p;
                continue;
            }
            let span = ast.span(skip_parens(ast, prop));
            self.report(cx, span, &prop_name);
            return;
        }
    }
}

/// A value reference (not a type) to a name not declared in the file: a configured
/// global, a `/* global */` comment or an undeclared name.
fn is_global_value_ref(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(r) = sem.reference_at(ident).map(|r| sem.reference(r)) else {
        return false;
    };
    if r.flags
        .intersects(RefFlags::TYPE | RefFlags::TYPEOF_TYPE | RefFlags::IMPLICIT)
    {
        return false;
    }
    match r.resolution {
        Resolution::Global | Resolution::Unresolved => true,
        Resolution::Symbol(s) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
    }
}
