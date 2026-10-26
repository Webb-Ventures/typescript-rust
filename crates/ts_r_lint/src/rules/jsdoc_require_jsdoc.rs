//! `jsdoc/require-jsdoc`: functions (and, as configured, classes, methods and any selector
//! in `contexts`) need a JSDoc block.
//!
//! What the plugin does, in our words:
//! - `require` turns on built-in checks per node type (`FunctionDeclaration` by default).
//!   Arrow functions and function expressions only count where they are assigned, exported
//!   by default, initialise a variable or are a property's value (all function expressions
//!   with `checkAllFunctionExpressions`); a `MethodDefinition` checks its function. Naming
//!   one of those six types in `contexts` also turns the built-in check on (and replaces a
//!   selector listener of the same name); other `contexts` entries are esquery selectors
//!   checked as they are.
//! - A node is skipped when it is shorter than `minLineCount` (global or its context's), is
//!   an overloaded function's implementation (`exemptOverloadedImplementations`), has a
//!   block (before the overload signatures above it, with
//!   `skipInterveningOverloadedDeclarations`), is a constructor/getter/setter that isn't
//!   checked, is an empty function or constructor (`exemptEmptyFunctions`,
//!   `exemptEmptyConstructors`: no parameters and no return value), or implements an
//!   interface method that has a block. With `publicOnly` only exported nodes are
//!   reported ([`crate::jsdoc_export`]).
//! - The report spans the node's first line (to column 0 of the next); the fix inserts an
//!   empty block (or `/** msg */` with `inlineCommentBlock`) before the node's reduced
//!   form or its first decorator, indented like that line.

use serde_json::Map;

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::esquery::{self, Tree, Val};
use crate::estree::{EsTree, Origin};
use crate::jsdoc::{self, Finder, Settings, Target};
use crate::jsdoc_export::{ExportOpts, is_uncommented_export};

const BUILT_IN: &[&str] = &[
    "ArrowFunctionExpression",
    "ClassDeclaration",
    "ClassExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "MethodDefinition",
];

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-jsdoc",
        "Checks for presence of JSDoc comments, on functions and potentially other contexts (optionally limited to exports).",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireJsdoc>,
};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Accessor {
    Yes,
    No,
    /// `no-setter` / `no-getter`: only when the other accessor is missing.
    Unpaired,
}

#[derive(Debug)]
struct RequireJsdoc {
    opts: Map<String, Value>,
    check_all_fn_exprs: bool,
    check_constructors: bool,
    check_getters: Accessor,
    check_setters: Accessor,
    enable_fixer: bool,
    exempt_empty_constructors: bool,
    exempt_empty_functions: bool,
    exempt_overloaded: bool,
    fixer_message: String,
    min_line_count: Option<i64>,
    public_only: Option<ExportOpts>,
    require: [bool; 6],
    skip_overloads: bool,
}

fn accessor(v: Option<&Value>, unpaired: &str) -> Accessor {
    match v {
        Some(Value::Bool(false)) => Accessor::No,
        Some(Value::String(s)) if s == unpaired => Accessor::Unpaired,
        _ => Accessor::Yes,
    }
}

impl FromOptions for RequireJsdoc {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts
            .first()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let flag = |k: &str, d: bool| bool_field(&o, k, d);
        let public_only = match o.get("publicOnly") {
            None | Some(Value::Bool(false)) | Some(Value::Null) => None,
            Some(v) => {
                let get = |k: &str| {
                    v.as_object()
                        .and_then(|m| m.get(k))
                        .and_then(Value::as_bool)
                };
                Some(ExportOpts {
                    ancestors_only: get("ancestorsOnly").unwrap_or(false),
                    esm: get("esm").unwrap_or(true),
                    cjs: get("cjs").unwrap_or(true),
                    window: get("window").unwrap_or(false),
                })
            }
        };
        let mut require = [false, false, false, true, false, false];
        if let Some(r) = o.get("require") {
            for (i, k) in BUILT_IN.iter().enumerate() {
                match r {
                    Value::Bool(_) => require[i] = false,
                    Value::Object(m) if m.contains_key(*k) => {
                        require[i] = m.get(*k).and_then(Value::as_bool).unwrap_or(false);
                    }
                    _ => {}
                }
            }
        }
        Ok(RequireJsdoc {
            check_all_fn_exprs: flag("checkAllFunctionExpressions", false),
            check_constructors: flag("checkConstructors", true),
            check_getters: accessor(o.get("checkGetters"), "no-setter"),
            check_setters: accessor(o.get("checkSetters"), "no-getter"),
            enable_fixer: flag("enableFixer", true),
            exempt_empty_constructors: flag("exemptEmptyConstructors", true),
            exempt_empty_functions: flag("exemptEmptyFunctions", false),
            exempt_overloaded: flag("exemptOverloadedImplementations", false),
            fixer_message: str_field(&o, "fixerMessage").unwrap_or("").to_owned(),
            min_line_count: o.get("minLineCount").and_then(Value::as_i64),
            public_only,
            require,
            skip_overloads: flag("skipInterveningOverloadedDeclarations", true),
            opts: o,
        })
    }
}

/// One `contexts` entry.
struct ContextEntry {
    context: Option<String>,
    inline: bool,
    min_line_count: Option<i64>,
}

/// What a listener tells `checkJsDoc`.
#[derive(Clone, Copy)]
struct Info<'s> {
    selector: Option<&'s str>,
    function_context: bool,
}

struct Run<'a, 'r> {
    rule: &'r RequireJsdoc,
    t: &'a EsTree<'a>,
    f: &'a Finder<'a>,
    settings: &'a Settings,
    contexts: Vec<ContextEntry>,
}

fn str_attr(t: &EsTree<'_>, n: u32, path: &[&str]) -> Option<String> {
    match t.attr(n, path) {
        Val::Str(s) => Some(s.into_owned()),
        _ => None,
    }
}

impl Run<'_, '_> {
    fn ty(&self, n: u32) -> &str {
        self.t.ty(n)
    }

    fn target(&self, n: u32) -> Option<Target> {
        let e = &self.t.nodes[n as usize];
        match e.origin {
            Origin::Real => Some(Target::Node(e.ast)),
            Origin::Export => Some(Target::Export(e.ast)),
            Origin::Synthetic => None,
        }
    }

    /// `findJSDocComment` on the node itself.
    fn has_own_block(&self, n: u32) -> bool {
        match self.target(n) {
            Some(Target::Node(id) | Target::Export(id)) => self.f.find(id).is_some(),
            None => false,
        }
    }

    /// The ESTree node `getReducedASTNode` gives for `n`.
    fn reduced(&self, n: u32) -> Option<u32> {
        match self.target(n)? {
            Target::Export(_) => Some(n),
            Target::Node(id) => {
                let r = self.f.reduce(id);
                if self.f.is_exported_decl(r)
                    && let Some(e) = self.t.of_ast(r, true)
                {
                    return Some(e);
                }
                self.t.of_ast(r, false)
            }
        }
    }

    /// `getJSDocComment` with `checkOverloads`.
    fn has_block(&self, n: u32, depth: u32) -> bool {
        let Some(t) = self.target(n) else {
            return false;
        };
        if self.f.jsdoc_of_target(t).is_some() {
            return true;
        }
        if !self.rule.skip_overloads || depth > 200 {
            return false;
        }
        let Some(r) = self.reduced(n) else {
            return false;
        };
        let Some(name) = self.current_overload_name(r) else {
            return false;
        };
        let Some(prev) = self.previous_overload_sibling(r) else {
            return false;
        };
        if self.previous_overload_name(prev).as_deref() != Some(name.as_str()) {
            return false;
        }
        let methods = |ty: &str| matches!(ty, "MethodDefinition" | "TSAbstractMethodDefinition");
        if methods(self.ty(r)) && !self.matching_method_overload(r, prev) {
            return false;
        }
        self.has_block(prev, depth + 1)
    }

    fn id_name(&self, n: u32) -> Option<String> {
        let id = self.t.node_field(n, "id")?;
        str_attr(self.t, id, &["name"])
    }

    fn method_key_name(&self, n: u32) -> Option<String> {
        if matches!(self.t.attr(n, &["computed"]), Val::Bool(true)) {
            return None;
        }
        let k = self.t.node_field(n, "key")?;
        str_attr(self.t, k, &["name"])
    }

    fn current_overload_name(&self, n: u32) -> Option<String> {
        match self.ty(n) {
            "TSDeclareFunction" | "FunctionDeclaration" => self.id_name(n),
            "ExportNamedDeclaration" => {
                let d = self.t.node_field(n, "declaration")?;
                matches!(self.ty(d), "FunctionDeclaration" | "TSDeclareFunction")
                    .then(|| self.id_name(d))
                    .flatten()
            }
            "MethodDefinition" | "TSAbstractMethodDefinition" => self.method_key_name(n),
            _ => None,
        }
    }

    fn previous_overload_name(&self, n: u32) -> Option<String> {
        match self.ty(n) {
            "TSDeclareFunction" => self.id_name(n),
            "ExportNamedDeclaration" => {
                let d = self.t.node_field(n, "declaration")?;
                (self.ty(d) == "TSDeclareFunction")
                    .then(|| self.id_name(d))
                    .flatten()
            }
            "MethodDefinition" | "TSAbstractMethodDefinition" => self.method_key_name(n),
            _ => None,
        }
    }

    fn previous_overload_sibling(&self, n: u32) -> Option<u32> {
        let parent = self.t.parent(n)?;
        let statement_list = |p: u32| {
            matches!(
                self.ty(p),
                "BlockStatement" | "Program" | "StaticBlock" | "TSModuleBlock"
            )
            .then(|| self.t.list_field(p, "body"))
            .flatten()
        };
        let (child, siblings) = if matches!(
            self.ty(n),
            "MethodDefinition" | "TSAbstractMethodDefinition"
        ) && self.ty(parent) == "ClassBody"
        {
            (n, self.t.list_field(parent, "body")?)
        } else if self.ty(parent) == "ExportNamedDeclaration" {
            (parent, statement_list(self.t.parent(parent)?)?)
        } else {
            (n, statement_list(parent)?)
        };
        let idx = siblings.iter().position(|&s| s == Some(child))?;
        if idx == 0 {
            return None;
        }
        siblings[idx - 1]
    }

    fn matching_method_overload(&self, cur: u32, prev: u32) -> bool {
        let info = |n: u32| -> Option<(bool, String, String, bool)> {
            if matches!(self.t.attr(n, &["computed"]), Val::Bool(true)) {
                return None;
            }
            let kind = str_attr(self.t, n, &["kind"])?;
            if kind != "method" && kind != "constructor" {
                return None;
            }
            let name = self.method_key_name(n)?;
            let bodyless = self.ty(n) == "TSAbstractMethodDefinition"
                || self
                    .t
                    .node_field(n, "value")
                    .is_some_and(|v| self.ty(v) == "TSEmptyBodyFunctionExpression");
            let stat = matches!(self.t.attr(n, &["static"]), Val::Bool(true));
            Some((bodyless, kind, name, stat))
        };
        match (info(cur), info(prev)) {
            (Some(c), Some(p)) => p.0 && p.2 == c.2 && p.1 == c.1 && p.3 == c.3,
            _ => false,
        }
    }

    fn parent_kind(&self, n: u32) -> Option<String> {
        let p = self.t.parent(n)?;
        str_attr(self.t, p, &["kind"])
    }

    fn is_constructor(&self, n: u32) -> bool {
        (self.ty(n) == "MethodDefinition"
            && str_attr(self.t, n, &["kind"]).as_deref() == Some("constructor"))
            || self.parent_kind(n).as_deref() == Some("constructor")
    }

    /// `hasAccessorPair` for the accessor `m` (a method definition or property).
    fn has_accessor_pair(&self, m: u32) -> bool {
        let name = self
            .t
            .node_field(m, "key")
            .and_then(|k| str_attr(self.t, k, &["name"]));
        let opposite = match str_attr(self.t, m, &["kind"]).as_deref() {
            Some("get") => "set",
            _ => "get",
        };
        let Some(holder) = self.t.parent(m) else {
            return false;
        };
        let field = if self.ty(m) == "MethodDefinition" {
            "body"
        } else {
            "properties"
        };
        self.t
            .list_field(holder, field)
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .any(|c| {
                let n = self
                    .t
                    .node_field(c, "key")
                    .and_then(|k| str_attr(self.t, k, &["name"]));
                str_attr(self.t, c, &["kind"]).as_deref() == Some(opposite) && n == name
            })
    }

    /// `exemptSpeciaMethods` with an empty block and this rule's options.
    fn exempt_special(&self, n: u32) -> bool {
        let r = self.rule;
        if !r.check_constructors && self.is_constructor(n) {
            return true;
        }
        let kind = self.parent_kind(n);
        let exempt = |a: Accessor| match a {
            Accessor::No => true,
            Accessor::Yes => false,
            Accessor::Unpaired => self.t.parent(n).is_some_and(|p| self.has_accessor_pair(p)),
        };
        (kind.as_deref() == Some("get") && exempt(r.check_getters))
            || (kind.as_deref() == Some("set") && exempt(r.check_setters))
    }

    /// `getFunctionParameterNames(node).length`.
    fn param_count(&self, n: u32) -> usize {
        let t = self.t;
        let len = |m: u32| t.list_field(m, "params").map(|l| l.len());
        if let Some(ann) = t.node_field(n, "typeAnnotation")
            && let Some(ty) = t.node_field(ann, "typeAnnotation")
            && let Some(l) = len(ty)
        {
            return l;
        }
        if let Some(l) = len(n) {
            return l;
        }
        t.node_field(n, "value").and_then(len).unwrap_or(0)
    }

    fn has_return_value(&self, n: u32) -> bool {
        match self.target(n) {
            Some(Target::Node(id)) => jsdoc::has_plain_return_value(self.f.ast, id),
            _ => false,
        }
    }

    /// `isFunctionWithOverload`.
    fn is_overloaded_implementation(&self, n: u32) -> bool {
        if self.ty(n) != "FunctionDeclaration" {
            return false;
        }
        let Some(p) = self.t.parent(n) else {
            return false;
        };
        let (holder, child) = match self.ty(p) {
            "Program" => (p, n),
            "ExportNamedDeclaration" => match self.t.parent(p) {
                Some(g) if self.ty(g) == "Program" => (g, p),
                _ => return false,
            },
            _ => return false,
        };
        let name = self.id_name(n);
        let body = self.t.list_field(holder, "body").unwrap_or_default();
        let Some(idx) = body.iter().position(|&s| s == Some(child)) else {
            return false;
        };
        let Some(Some(prev)) = idx.checked_sub(1).map(|i| body[i]) else {
            return false;
        };
        match self.ty(prev) {
            "TSDeclareFunction" => self.id_name(prev) == name,
            "ExportNamedDeclaration" => self
                .t
                .node_field(prev, "declaration")
                .is_some_and(|d| self.ty(d) == "TSDeclareFunction" && self.id_name(d) == name),
            _ => false,
        }
    }

    /// `isExemptedImplementer`: a method of a class that implements an interface whose
    /// method of the same name has a block.
    fn exempted_implementer(&self, n: u32) -> bool {
        let t = self.t;
        if self.ty(n) != "FunctionExpression" {
            return false;
        }
        let Some(method) = t.parent(n).filter(|&m| self.ty(m) == "MethodDefinition") else {
            return false;
        };
        let Some(body) = t.parent(method).filter(|&b| self.ty(b) == "ClassBody") else {
            return false;
        };
        let Some(class) = t.parent(body).filter(|&c| self.ty(c) == "ClassDeclaration") else {
            return false;
        };
        let Some(impls) = t.list_field(class, "implements") else {
            return false;
        };
        let method_name = t
            .node_field(method, "key")
            .and_then(|k| str_attr(t, k, &["name"]));
        for imp in impls.into_iter().flatten() {
            let Some(iname) = t
                .node_field(imp, "expression")
                .and_then(|e| str_attr(t, e, &["name"]))
            else {
                continue;
            };
            if let Some(sig) = self.interface_method(class, &iname, method_name.as_deref())
                && self.has_block(sig, 0)
            {
                return true;
            }
        }
        false
    }

    /// The first member named `method` of the nearest interface named `iface` declared in a
    /// statement list around `from`.
    fn interface_method(&self, from: u32, iface: &str, method: Option<&str>) -> Option<u32> {
        let t = self.t;
        let mut cur = t.parent(from);
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 10_000 {
                return None;
            }
            if matches!(
                self.ty(c),
                "Program" | "BlockStatement" | "TSModuleBlock" | "StaticBlock"
            ) {
                for s in t
                    .list_field(c, "body")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    let decl = match self.ty(s) {
                        "ExportNamedDeclaration" | "ExportDefaultDeclaration" => {
                            t.node_field(s, "declaration")
                        }
                        _ => Some(s),
                    };
                    let Some(d) = decl.filter(|&d| self.ty(d) == "TSInterfaceDeclaration") else {
                        continue;
                    };
                    if self.id_name(d).as_deref() != Some(iface) {
                        continue;
                    }
                    let Some(ib) = t.node_field(d, "body") else {
                        continue;
                    };
                    for m in t
                        .list_field(ib, "body")
                        .unwrap_or_default()
                        .into_iter()
                        .flatten()
                    {
                        let k = t
                            .node_field(m, "key")
                            .and_then(|k| str_attr(t, k, &["name"]));
                        if k.as_deref() == method {
                            return Some(m);
                        }
                    }
                }
            }
            cur = t.parent(c);
        }
        None
    }

    fn line_count(&self, n: u32) -> i64 {
        let s = self.t.span(n).text(self.f.src);
        s.matches('\n').count() as i64 + 1
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, info: Info<'_>, n: u32) {
        let r = self.rule;
        if r.min_line_count.is_some() || self.contexts.iter().any(|c| c.min_line_count.is_some()) {
            let lines = self.line_count(n);
            let under = |c: Option<i64>| c.is_some_and(|c| c > lines);
            if under(r.min_line_count) {
                return;
            }
            let want = info.selector.unwrap_or(self.ty(n));
            let ctx_min = self
                .contexts
                .iter()
                .find(|c| c.context.as_deref() == Some(want))
                .and_then(|c| c.min_line_count);
            if under(ctx_min) {
                return;
            }
        }
        if r.exempt_overloaded && self.is_overloaded_implementation(n) {
            return;
        }
        if self.has_block(n, 0) || self.exempt_special(n) {
            return;
        }
        if ((r.exempt_empty_functions && info.function_context)
            || (r.exempt_empty_constructors && self.is_constructor(n)))
            && self.param_count(n) == 0
            && !self.has_return_value(n)
        {
            return;
        }
        if self.exempted_implementer(n) {
            return;
        }
        if let Some(opts) = r.public_only
            && !is_uncommented_export(self.t, n, opts, |e| self.has_own_block(e))
        {
            return;
        }
        let span = self.t.span(n);
        let lines = self.f.lines;
        let line = lines.line_of(span.start);
        let line_start = lines.line_start(line).unwrap_or(span.start);
        let col = self.f.src[line_start as usize..span.start as usize]
            .encode_utf16()
            .count() as u32;
        let loc = Loc {
            line: line + 1,
            column: col + 1,
            end_line: line + 2,
            end_column: 1,
        };
        let fix = r.enable_fixer.then(|| self.fix(n)).flatten();
        let rep = cx
            .report(span, "Missing JSDoc comment.")
            .id("missingJsDoc")
            .loc(loc);
        if let Some(fix) = fix {
            rep.fix(fix);
        }
    }

    fn fix(&self, n: u32) -> Option<Fix> {
        let s = self.settings;
        let lines = if s.min_lines == 0 && s.max_lines >= 1 {
            1
        } else {
            s.min_lines.max(0) as usize
        };
        let id = match self.target(n)? {
            Target::Node(id) => self.f.reduce(id),
            Target::Export(id) => id,
        };
        let pos = self.f.start_of(id);
        let line = self.f.lines.line_of(pos);
        let line_start = self.f.lines.line_start(line).unwrap_or(pos);
        let before = &self.f.src[line_start as usize..pos as usize];
        let ws: String = before
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .collect();
        let indent = format!("{ws} ");
        let inline = self
            .contexts
            .iter()
            .find(|c| c.context.as_deref() == Some(self.ty(n)))
            .is_some_and(|c| c.inline);
        let msg = &self.rule.fixer_message;
        let head = if inline {
            format!("/** {msg}")
        } else {
            format!("/**\n{indent}*{msg}\n{indent}")
        };
        let text = format!("{head}*/{}{ws}", "\n".repeat(lines));
        Some(Fix::insert(pos, text))
    }

    fn has_option(&self, ty: &str) -> bool {
        BUILT_IN
            .iter()
            .position(|b| *b == ty)
            .is_some_and(|i| self.rule.require[i])
            || self
                .contexts
                .iter()
                .any(|c| c.context.as_deref() == Some(ty))
    }

    /// The built-in listener for `n`, if one applies: the node to check and its info.
    fn built_in(&self, n: u32) -> Option<(u32, Info<'static>)> {
        let ty = self.ty(n);
        if !BUILT_IN.contains(&ty) || !self.has_option(ty) {
            return None;
        }
        let func = Info {
            selector: None,
            function_context: true,
        };
        let placed = || {
            let Some(p) = self.t.parent(n) else {
                return false;
            };
            match self.ty(p) {
                "AssignmentExpression" | "ExportDefaultDeclaration" | "VariableDeclarator" => true,
                "ClassProperty" | "ObjectProperty" | "Property" | "PropertyDefinition" => {
                    self.t.node_field(p, "value") == Some(n)
                }
                _ => false,
            }
        };
        match ty {
            "ArrowFunctionExpression" => placed().then_some((n, func)),
            "FunctionExpression" => (self.rule.check_all_fn_exprs || placed()).then_some((n, func)),
            "FunctionDeclaration" => Some((n, func)),
            "ClassDeclaration" | "ClassExpression" => Some((
                n,
                Info {
                    selector: None,
                    function_context: false,
                },
            )),
            "MethodDefinition" => self.t.node_field(n, "value").map(|v| {
                (
                    v,
                    Info {
                        selector: Some("MethodDefinition"),
                        function_context: true,
                    },
                )
            }),
            _ => None,
        }
    }
}

impl Rule for RequireJsdoc {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if !jsdoc::settings_load(cx) {
            return;
        }
        let settings = Settings::from_cx(cx);
        let raw: Vec<Value> = match self.opts.get("contexts").and_then(Value::as_array) {
            Some(a) => a.clone(),
            None => settings.contexts.clone().unwrap_or_default(),
        };
        let contexts: Vec<ContextEntry> = raw
            .iter()
            .map(|v| match v {
                Value::String(s) => ContextEntry {
                    context: Some(s.clone()),
                    inline: false,
                    min_line_count: None,
                },
                _ => ContextEntry {
                    context: v.get("context").and_then(Value::as_str).map(str::to_owned),
                    inline: v
                        .get("inlineCommentBlock")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    min_line_count: v.get("minLineCount").and_then(Value::as_i64),
                },
            })
            .collect();
        let file = cx.file;
        let finder = Finder {
            ast: file.ast,
            src: file.src,
            parents: file.parents,
            lines: file.lines,
            settings: &settings,
        };
        let tree = EsTree::build(file.ast, file.src);
        // Selector listeners for every entry not replaced by a built-in listener.
        let mut selectors = Vec::new();
        for c in &contexts {
            let Some(raw) = c.context.as_deref() else {
                continue;
            };
            if BUILT_IN.contains(&raw) {
                continue;
            }
            if let Ok(sel) = esquery::parse(raw.strip_suffix(":exit").unwrap_or(raw)) {
                selectors.push((raw.to_owned(), sel));
            }
        }
        let prepared: Vec<_> = selectors.iter().map(|(_, s)| s.prepare()).collect();
        let run = Run {
            rule: self,
            t: &tree,
            f: &finder,
            settings: &settings,
            contexts,
        };
        for n in 0..tree.len() as u32 {
            if tree.parent(n).is_none() {
                continue;
            }
            for (i, p) in prepared.iter().enumerate() {
                if p.matches_listener(&tree, n) {
                    let info = Info {
                        selector: Some(selectors[i].0.as_str()),
                        function_context: false,
                    };
                    run.check(cx, info, n);
                }
            }
            if let Some((node, info)) = run.built_in(n) {
                run.check(cx, info, node);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::engine::lint_source;
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};

    /// The 1-based lines of the rule's reports with `options`.
    fn lines(code: &str, options: serde_json::Value) -> Vec<usize> {
        let cfg = serde_json::json!([{
            "files": ["**/*.ts"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
            "rules": { "jsdoc/require-jsdoc": ["error", options] },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        let mut out: Vec<usize> = lint_source("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("jsdoc/require-jsdoc"))
            .map(|d| code[..d.span.start as usize].matches('\n').count() + 1)
            .collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn public_only_follows_module_exports() {
        let code = "function a() {}\nfunction b() {}\nmodule.exports = { a };\n";
        assert_eq!(lines(code, serde_json::json!({ "publicOnly": true })), [1]);
    }

    #[test]
    fn cyclic_symbols_terminate() {
        let code = "const a = {};\na.b = a;\na.b.c = a;\nfunction f() {}\nexport { a };\n";
        assert_eq!(
            lines(code, serde_json::json!({ "publicOnly": true })),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let depth = 3000;
        let code = format!(
            "export const x = {}function f() {{}}{};\n",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let _ = lines(&code, serde_json::json!({ "publicOnly": true }));
    }
}
