//! `unicorn/isolated-functions`: a function that runs somewhere else (a worker, a page, a
//! serialized copy) must not use variables from the code around it, nor `this`/`super`.
//!
//! A function is isolated when the comment right before it (or before the declaration,
//! export or property holding it) is one of the `comments` tags, alone or followed by
//! ` - ` / ` -- ` and an explanation; when it is an argument of a call of one of the
//! `functions`; when it is the first argument of `browser.execute(...)` or
//! `page.evaluate(...)`; when it is the `func` property of the first argument of
//! `chrome.scripting.executeScript(...)` / `browser.scripting.executeScript(...)`; or when
//! one of the `selectors` matches it. The first of these reasons is the one reported.
//!
//! Every reference inside the function that does not resolve inside it is reported (type
//! references aside), except reads of allowed globals and writes of writable ones. A global
//! is allowed when it is configured (or built in) or declared by a `/* global */` comment
//! and not shadowed; `overrideGlobals` replaces that decision per name (`off` forbids).
//! `this` and `super` are reported anywhere in the function except inside nested
//! non-arrow functions and class bodies (a class's `extends` and computed keys still count).

use super::prelude::*;
use super::unicorn_text::static_string_value;
use crate::env::GlobalAccess;
use crate::esquery::{self, Selector, Tree, Val};
use crate::estree::EsTree;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/isolated-functions",
        "Prevent usage of variables from outside the scope of isolated functions.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<IsolatedFunctions>,
};

#[derive(Debug)]
struct IsolatedFunctions {
    functions: Vec<String>,
    selectors: Vec<Selector>,
    /// Lower-cased.
    comments: Vec<String>,
    overrides: Vec<(String, GlobalAccess)>,
}

impl Default for IsolatedFunctions {
    fn default() -> Self {
        IsolatedFunctions {
            functions: vec!["makeSynchronous".into(), "workerize".into()],
            selectors: Vec::new(),
            comments: vec!["@isolated".into()],
            overrides: Vec::new(),
        }
    }
}

impl FromOptions for IsolatedFunctions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut r = IsolatedFunctions::default();
        let o = obj_opt(opts, 0);
        if o.contains_key("functions") {
            r.functions = str_list(o, "functions");
        }
        if o.contains_key("comments") {
            r.comments = str_list(o, "comments")
                .into_iter()
                .map(|c| c.to_lowercase())
                .collect();
        }
        for raw in str_list(o, "selectors") {
            let mut sel =
                esquery::parse(raw.strip_suffix(":exit").unwrap_or(&raw)).map_err(|e| {
                    format!("unicorn/isolated-functions: invalid selector `{raw}`: {e}")
                })?;
            sel.raw = raw;
            r.selectors.push(sel);
        }
        r.selectors.sort_by(|a, b| a.compare(b));
        if let Some(Value::Object(m)) = o.get("overrideGlobals") {
            for (k, v) in m {
                if let Some(a) = GlobalAccess::from_json(v) {
                    r.overrides.push((k.clone(), a));
                }
            }
        }
        Ok(r)
    }
}

const OBJECT_PROTOTYPE: [&str; 7] = [
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

const FUNCTION_TYPES: [&str; 3] = [
    "FunctionDeclaration",
    "FunctionExpression",
    "ArrowFunctionExpression",
];

fn str_attr<'t>(t: &'t EsTree<'_>, n: u32, path: &[&str]) -> Option<std::borrow::Cow<'t, str>> {
    match t.attr(n, path) {
        Val::Str(s) => Some(s),
        _ => None,
    }
}

fn is_true(t: &EsTree<'_>, n: u32, path: &[&str]) -> bool {
    t.attr(n, path) == Val::Bool(true)
}

/// `object.method(...)` with a plain (not computed, not optional) member and call.
fn is_method_call(t: &EsTree<'_>, call: u32, object: Option<&[&str]>, method: &str) -> bool {
    let Some(callee) = t.node_field(call, "callee") else {
        return false;
    };
    t.ty(call) == "CallExpression"
        && !is_true(t, call, &["optional"])
        && t.ty(callee) == "MemberExpression"
        && !is_true(t, callee, &["computed"])
        && !is_true(t, callee, &["optional"])
        && str_attr(t, callee, &["property", "name"]).as_deref() == Some(method)
        && object.is_none_or(|objects| {
            str_attr(t, callee, &["object", "type"]).as_deref() == Some("Identifier")
                && str_attr(t, callee, &["object", "name"])
                    .is_some_and(|n| objects.contains(&n.as_ref()))
        })
}

fn is_first_argument(t: &EsTree<'_>, n: u32) -> bool {
    t.key(n) == "arguments" && t.list_pos(n).is_some_and(|(i, _)| i == 0)
}

impl IsolatedFunctions {
    fn comment_reason(&self, cx: &Ctx<'_, '_>, t: &EsTree<'_>, n: u32) -> Option<String> {
        let ast = cx.ast();
        let src = cx.src();
        let comment_before = |n: u32| {
            let start = t.span(n).start;
            let i = ast.comments.partition_point(|c| c.span.end <= start);
            let c = ast.comments.get(i.checked_sub(1)?)?;
            let gap = src.get(c.span.end as usize..start as usize)?;
            gap.trim().is_empty().then_some(c.span)
        };
        let mut cur = n;
        let mut found = comment_before(cur);
        for _ in 0..16 {
            if found.is_some() {
                break;
            }
            let Some(p) = t.parent(cur) else { break };
            let applies = match t.ty(p) {
                "VariableDeclarator"
                | "VariableDeclaration"
                | "ExportNamedDeclaration"
                | "ExportDefaultDeclaration" => true,
                "Property" | "MethodDefinition" => t.key(cur) == "value",
                _ => false,
            };
            if !applies {
                break;
            }
            cur = p;
            found = comment_before(cur);
        }
        let span = found?;
        let text = src.get(span.start as usize..span.end as usize)?;
        let value = if let Some(b) = text.strip_prefix("/*") {
            b.strip_suffix("*/").unwrap_or(b)
        } else {
            text.strip_prefix("//").unwrap_or(text)
        };
        // Leading `*`s (each with the whitespace after it) are JSDoc decoration.
        let mut v = value;
        while let Some(rest) = v.strip_prefix('*') {
            v = rest.trim_start();
        }
        let v = v.trim().to_lowercase();
        let tag = self.comments.iter().find(|c| {
            v == **c || v.starts_with(&format!("{c} - ")) || v.starts_with(&format!("{c} -- "))
        })?;
        Some(format!("follows comment {}", json_string(tag)))
    }

    fn reason(&self, cx: &Ctx<'_, '_>, t: &EsTree<'_>, n: u32) -> Option<String> {
        if !self.comments.is_empty()
            && let Some(r) = self.comment_reason(cx, t, n)
        {
            return Some(r);
        }
        let parent = t.parent(n)?;
        if !self.functions.is_empty()
            && t.ty(parent) == "CallExpression"
            && t.key(n) == "arguments"
            && str_attr(t, parent, &["callee", "type"]).as_deref() == Some("Identifier")
            && let Some(name) = str_attr(t, parent, &["callee", "name"])
            && self.functions.iter().any(|f| *f == name)
        {
            return Some(format!("callee of function named {}", json_string(&name)));
        }
        if t.ty(parent) == "CallExpression" && is_first_argument(t, n) {
            if is_method_call(t, parent, Some(&["browser"]), "execute") {
                return Some("callee of method named \"browser.execute\"".into());
            }
            if is_method_call(t, parent, Some(&["page"]), "evaluate") {
                return Some("callee of method named \"page.evaluate\"".into());
            }
        }
        // `{func: () => {}}` passed first to `<browser|chrome>.scripting.executeScript`.
        if t.ty(parent) != "Property"
            || str_attr(t, parent, &["kind"]).as_deref() != Some("init")
            || t.key(n) != "value"
        {
            return None;
        }
        let key = t.node_field(parent, "key")?;
        let key_name = if !is_true(t, parent, &["computed"]) && t.ty(key) == "Identifier" {
            str_attr(t, key, &["name"]).map(|s| s.into_owned())
        } else {
            static_string_value(cx.ast(), t.nodes[key as usize].ast)
        };
        if key_name.as_deref() != Some("func") {
            return None;
        }
        let object = t.parent(parent)?;
        let call = t.parent(object)?;
        if t.ty(object) != "ObjectExpression"
            || t.ty(call) != "CallExpression"
            || !is_first_argument(t, object)
            || !is_method_call(t, call, None, "executeScript")
        {
            return None;
        }
        let callee = t.node_field(call, "callee")?;
        let scripting = t.node_field(callee, "object")?;
        let base = t.node_field(scripting, "object")?;
        let base_name = str_attr(t, base, &["name"])?;
        (t.ty(scripting) == "MemberExpression"
            && !is_true(t, scripting, &["computed"])
            && !is_true(t, scripting, &["optional"])
            && str_attr(t, scripting, &["property", "name"]).as_deref() == Some("scripting")
            && t.ty(base) == "Identifier"
            && matches!(base_name.as_ref(), "browser" | "chrome"))
        .then(|| format!("property \"func\" passed to \"{base_name}.scripting.executeScript\""))
    }

    /// `Some(writable)` when the reference names an allowed global.
    fn allowed_global(&self, cx: &Ctx<'_, '_>, name: &str, res: Resolution) -> Option<bool> {
        let sem = cx.semantic();
        let comment = cx
            .file
            .comment_globals
            .iter()
            .rev()
            .find(|g| g.name == name);
        let is_comment_global =
            |s: SymbolId| sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL);
        if let Some((_, a)) = self.overrides.iter().find(|(n, _)| n == name) {
            if *a == GlobalAccess::Off {
                return None;
            }
            if let Resolution::Symbol(s) = res
                && !is_comment_global(s)
            {
                return None;
            }
            return Some(*a == GlobalAccess::Writable);
        }
        if comment.is_some_and(|g| g.off) {
            return None;
        }
        match res {
            // ESLint's ES3 globals include `Object.prototype` members, the `globals` package
            // the rule reads does not.
            Resolution::Global if OBJECT_PROTOTYPE.contains(&name) => None,
            Resolution::Global => cx
                .env()
                .globals
                .get(name)
                .map(|a| a == GlobalAccess::Writable),
            Resolution::Symbol(s) if is_comment_global(s) => comment.map(|g| g.writable),
            _ => None,
        }
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, t: &EsTree<'_>, n: u32, reason: &str) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(scope) = sem.scope_of_node(t.nodes[n as usize].ast) else {
            return;
        };
        let inside = |mut s: ScopeId| {
            for _ in 0..10_000 {
                if s == scope {
                    return true;
                }
                match sem.scope(s).parent {
                    Some(p) => s = p,
                    None => return false,
                }
            }
            false
        };
        let mut problems: Vec<(Span, String)> = Vec::new();
        for r in sem.references() {
            if r.is_type()
                || r.flags
                    .intersects(RefFlags::TYPEOF_TYPE | RefFlags::IMPLICIT)
                || !inside(r.scope)
            {
                continue;
            }
            if let Resolution::Symbol(s) = r.resolution
                && inside(sem.symbol(s).scope)
            {
                continue;
            }
            let name = ast.atom(r.name);
            let mut why = reason.to_owned();
            if let Some(writable) = self.allowed_global(cx, name, r.resolution) {
                if r.is_read() && !r.is_write() || writable {
                    continue;
                }
                why.push_str(" (global variable is not writable)");
            }
            problems.push((
                cx.span(r.node),
                format!(
                    "Variable {name} not defined in scope of isolated function. Function is isolated because: {why}."
                ),
            ));
        }
        for (span, msg) in problems {
            cx.report(span, msg).id("externally-scoped-variable");
        }
        let mut found = Vec::new();
        context_uses(t, n, n, &mut found, 0);
        for m in found {
            let (id, what) = if t.ty(m) == "Super" {
                ("super", "super")
            } else {
                ("this-expression", "this")
            };
            cx.report(
                t.span(m),
                format!(
                    "Unexpected `{what}` in isolated function. Function is isolated because: {reason}."
                ),
            )
            .id(id);
        }
    }
}

/// `this` and `super` that belong to the function `root`.
fn context_uses(t: &EsTree<'_>, n: u32, root: u32, out: &mut Vec<u32>, depth: u32) {
    if depth > 500 {
        return;
    }
    match t.ty(n) {
        "ThisExpression" | "Super" => {
            out.push(n);
            return;
        }
        ty if n != root && FUNCTION_TYPES.contains(&ty) && ty != "ArrowFunctionExpression" => {
            return;
        }
        "ClassDeclaration" | "ClassExpression" if n != root => {
            if let Some(s) = t.node_field(n, "superClass") {
                context_uses(t, s, root, out, depth + 1);
            }
            if let Some(body) = t.node_field(n, "body")
                && let Some(items) = t.list_field(body, "body")
            {
                for el in items.into_iter().flatten() {
                    if is_true(t, el, &["computed"])
                        && let Some(k) = t.node_field(el, "key")
                    {
                        context_uses(t, k, root, out, depth + 1);
                    }
                }
            }
            return;
        }
        _ => {}
    }
    for &c in t.children(n) {
        context_uses(t, c, root, out, depth + 1);
    }
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

impl Rule for IsolatedFunctions {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.src();
        let lower = src.to_lowercase();
        let worth = !self.selectors.is_empty()
            || self.comments.iter().any(|c| lower.contains(c.as_str()))
            || self.functions.iter().any(|f| src.contains(f.as_str()))
            || ["execute", "evaluate"].iter().any(|m| src.contains(m));
        if !worth {
            return;
        }
        let tree = EsTree::build(cx.ast(), cx.src());
        let prepared: Vec<_> = self.selectors.iter().map(|s| s.prepare()).collect();
        for n in 0..tree.len() as u32 {
            if !FUNCTION_TYPES.contains(&tree.ty(n)) {
                continue;
            }
            let reason = self.reason(cx, &tree, n).or_else(|| {
                self.selectors
                    .iter()
                    .zip(&prepared)
                    .find(|(_, p)| p.matches_listener(&tree, n))
                    .map(|(s, _)| format!("matches selector {}", json_string(&s.raw)))
            });
            if let Some(reason) = reason {
                self.check(cx, &tree, n, &reason);
            }
        }
    }
}
