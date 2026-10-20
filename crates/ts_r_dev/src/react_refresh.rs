//! React Fast Refresh for `ts-r-dev`: a [`Transform`] wrapper that adds React Refresh's
//! registration and hook-signature calls to compiled modules, the `/@ts-r/react-refresh`
//! glue module, and the HTML preamble that installs the runtime before React DOM loads.
//!
//! The runtime is React's own (`react-refresh/runtime`, or the copy Next.js ships), resolved
//! from the project and pre-bundled with the other dependencies, the same runtime Vite's
//! React plugin uses. What is ours: deciding what to register (written from the documented
//! runtime API: `register`, `createSignatureFunctionForTransform`, `performReactRefresh`,
//! `isLikelyComponentType`, `injectIntoGlobalHook`) and the HMR glue on top of
//! `import.meta.hot`.
//!
//! Per module, on the *output* of the TS/JSX transform (parsed again with `ts_r_parser`):
//!
//! - Top-level components (capitalised `function X`, `const X = () => …` /
//!   `function () {…}`, and `memo(…)` / `forwardRef(…)` wrappers, exported or not) are
//!   registered as `"<module id> <name>"`.
//! - Components and custom hooks (`useX`) that call hooks get a signature: the hook calls in
//!   order (callee, the variable they initialise, and `useState`/`useReducer` arguments),
//!   plus the custom hooks they call, so React remounts instead of reusing state when the
//!   hooks change. A custom hook that isn't a top-level binding (a local function, say) can't
//!   be tracked, so calling one forces a remount on every edit, as `// @refresh reset` does.
//! - A module that registers a component accepts its own updates. On an update, if every
//!   export of the new module is a component (and the export names didn't change) React
//!   refreshes in place; otherwise the module invalidates and the update propagates to its
//!   importers, as with Vite's plugin.
//!
//! Every insertion stays on its line: the prelude goes on line 1 and the in-body
//! `__tsr_sN()` calls are inserted mid-line, so the source map is fixed by shifting columns
//! (no line moves); the registration footer is appended after the code.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use ts_r_ast::{Ast, FnFlags, Node, NodeId, SourceKind, Visitor, Walk, walk};
use ts_r_hmr::{Transform, TransformError, TransformInput, TransformOutput};

/// URL of the glue module.
pub const GLUE_URL: &str = "/@ts-r/react-refresh";

/// Where the React Refresh runtime may live, in order.
pub const RUNTIME_CANDIDATES: &[&str] = &[
    "react-refresh/runtime",
    "next/dist/compiled/react-refresh/runtime",
];

/// The `<script>` injected into HTML pages (before the app's scripts, so the runtime hooks
/// into React DOM before React DOM loads).
pub fn preamble() -> String {
    format!("<script type=\"module\">import \"{GLUE_URL}\";</script>")
}

/// The glue module served at [`GLUE_URL`]; `runtime` is the bare specifier of the runtime.
pub fn glue_module(runtime: &str) -> String {
    let spec = ts_r_hmr::render::js_string(runtime);
    format!(
        r#"// ts-r-dev: React Fast Refresh glue over React's refresh runtime.
import * as ns from {spec};
const R = ns.default && ns.default.register ? ns.default : ns;
R.injectIntoGlobalHook(window);
export const register = (type, id) => R.register(type, id);
export const sig = R.createSignatureFunctionForTransform;
const isComponent = (v) => R.isLikelyComponentType(v);
export function registerExports(ns, id) {{
  for (const key in ns) {{
    const v = ns[key];
    if (isComponent(v)) R.register(v, id + " %exports% " + key);
  }}
}}
// null when the update can be applied in place, else why not.
export function validate(prev, next, id) {{
  if (!prev) return "the module's previous exports are unknown";
  const a = Object.keys(prev);
  const b = Object.keys(next);
  if (a.length !== b.length || a.some((k, i) => k !== b[i])) return "its exports changed";
  for (const key of b) {{
    if (!isComponent(next[key])) return "export \"" + key + "\" is not a React component";
  }}
  registerExports(next, id);
  return null;
}}
let queued = false;
// One refresh per update message: every accept callback of a message runs before this.
export function refresh() {{
  if (queued) return;
  queued = true;
  queueMicrotask(() => {{
    queued = false;
    R.performReactRefresh();
  }});
}}
"#
    )
}

/// Wraps a transform (the TS/JSX one) and adds React Refresh to its output.
#[derive(Debug)]
pub struct ReactRefresh {
    inner: Arc<dyn Transform>,
    cache_id: String,
}

impl ReactRefresh {
    pub fn new(inner: Arc<dyn Transform>) -> ReactRefresh {
        let cache_id = format!("{}+react-refresh/3", inner.cache_id());
        ReactRefresh { inner, cache_id }
    }
}

impl Transform for ReactRefresh {
    fn cache_id(&self) -> &str {
        &self.cache_id
    }

    fn handles(&self, path: &Path) -> bool {
        self.inner.handles(path)
    }

    fn transform(&self, input: &TransformInput<'_>) -> Result<TransformOutput, TransformError> {
        let out = self.inner.transform(input)?;
        let in_deps = input
            .path
            .components()
            .any(|c| c.as_os_str() == "node_modules");
        if in_deps {
            return Ok(out);
        }
        Ok(add_refresh(out, input.id))
    }

    fn deps(&self, input: &TransformInput<'_>) -> Vec<std::path::PathBuf> {
        self.inner.deps(input)
    }
}

/// Adds registrations, signatures and the self-accept footer; returns `out` unchanged when
/// the module has no components or hooks (or can't be parsed back).
pub fn add_refresh(out: TransformOutput, module_id: &str) -> TransformOutput {
    let code = &out.code;
    // Cheap exits: code using React Refresh's own names is already instrumented, and
    // nothing capitalised or hook-like can't define components or hooks.
    if code.contains("$RefreshReg$")
        || code.contains("$RefreshSig$")
        || !(has_upper_binding(code) || code.contains("use") || code.contains("export default"))
    {
        return out;
    }
    let parsed = ts_r_parser::parse(code, SourceKind::Js);
    if !parsed.diagnostics.is_empty() {
        return out;
    }
    // Our other names share a prefix that nothing in the module contains.
    let p = fresh_prefix(code);
    let plan = analyze(code, &parsed.ast, &p);
    if plan.registrations.is_empty() && plan.signatures.is_empty() {
        return out;
    }
    let id_js = ts_r_hmr::render::js_string(module_id);

    // Line-1 prelude.
    let mut prelude = format!(
        "import * as {p}rr from \"{GLUE_URL}\";const $RefreshReg$ = (t, id) => {p}rr.register(t, {id_js} + \" \" + id), $RefreshSig$ = {p}rr.sig;"
    );
    if !plan.signatures.is_empty() {
        prelude.push_str("var ");
        for (i, _) in plan.signatures.iter().enumerate() {
            if i > 0 {
                prelude.push_str(", ");
            }
            let _ = write!(prelude, "{p}s{i} = $RefreshSig$()");
        }
        prelude.push(';');
    }
    if plan.temps > 0 {
        prelude.push_str("var ");
        for i in 0..plan.temps {
            if i > 0 {
                prelude.push_str(", ");
            }
            let _ = write!(prelude, "{p}c{i}");
        }
        prelude.push(';');
    }

    let mut inserts: Vec<(u32, String)> = Vec::with_capacity(plan.inserts.len() + 1);
    inserts.push((0, prelude));
    inserts.extend(plan.inserts);
    inserts.sort_by_key(|(at, _)| *at);

    let mut new_code = String::with_capacity(code.len() + 512);
    let mut last = 0usize;
    for (at, text) in &inserts {
        new_code.push_str(&code[last..*at as usize]);
        new_code.push_str(text);
        last = *at as usize;
    }
    new_code.push_str(&code[last..]);

    // Footer.
    let force = code.contains("@refresh reset");
    new_code.push_str("\n;");
    for (target, name) in &plan.registrations {
        let _ = writeln!(
            new_code,
            "$RefreshReg$({target}, {});",
            ts_r_hmr::render::js_string(name)
        );
    }
    for (i, s) in plan.signatures.iter().enumerate() {
        let _ = write!(
            new_code,
            "{p}s{i}({}, {}, {}",
            s.target,
            ts_r_hmr::render::js_string(&s.key),
            force || s.force
        );
        if !s.custom_hooks.is_empty() {
            let _ = write!(new_code, ", () => [{}]", s.custom_hooks.join(", "));
        }
        new_code.push_str(");\n");
    }
    if !plan.registrations.is_empty() {
        let _ = write!(
            new_code,
            "if (import.meta.hot) {{\n  let {p}prev;\n  import(import.meta.url).then((m) => {{ {p}prev = m; {p}rr.registerExports(m, {id_js}); }});\n  import.meta.hot.accept((next) => {{\n    if (!next) return;\n    const why = {p}rr.validate({p}prev, next, {id_js});\n    if (why) import.meta.hot.invalidate(\"Fast Refresh: \" + why);\n    else {p}rr.refresh();\n  }});\n}}\n"
        );
    }

    // Imports moved by the insertions before them.
    let shift = |pos: u32| -> u32 {
        pos + inserts
            .iter()
            .take_while(|(at, _)| *at <= pos)
            .map(|(_, t)| t.len() as u32)
            .sum::<u32>()
    };
    let imports = out
        .imports
        .iter()
        .map(|r| ts_r_hmr::ImportRecord {
            start: shift(r.start),
            end: shift(r.end),
            ..r.clone()
        })
        .collect();
    let map = out.map.as_deref().map(|m| shift_map(m, code, &inserts));
    TransformOutput {
        code: new_code,
        map: map.or(out.map),
        imports,
    }
}

/// `__tsr_`, or `__tsrN_` for the first `N` whose prefix appears nowhere in `code`, so the
/// generated bindings can't collide with (or be shadowed by) the module's own names.
fn fresh_prefix(code: &str) -> String {
    let mut p = "__tsr_".to_owned();
    let mut n = 0u32;
    while code.contains(&p) {
        n += 1;
        p = format!("__tsr{n}_");
    }
    p
}

fn has_upper_binding(code: &str) -> bool {
    ["function ", "const ", "let ", "var "].iter().any(|kw| {
        code.match_indices(kw).any(|(i, _)| {
            code[i + kw.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
        })
    })
}

// ---------------------------------------------------------------------------------------
// Analysis

#[derive(Debug, Default)]
struct Plan {
    /// `(expression, registration name)`.
    registrations: Vec<(String, String)>,
    signatures: Vec<Sig>,
    /// `(byte offset, text)` inside the code.
    inserts: Vec<(u32, String)>,
    /// Number of `<prefix>cN` temporaries (for wrapped components).
    temps: usize,
    /// Prefix of the generated names ([`fresh_prefix`]).
    prefix: String,
}

#[derive(Debug)]
struct Sig {
    target: String,
    key: String,
    custom_hooks: Vec<String>,
    /// Calls a custom hook whose signature can't be tracked: remount on every edit.
    force: bool,
}

fn is_component_name(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn is_hook_name(n: &str) -> bool {
    n.strip_prefix("use")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_uppercase()))
}

const BUILTIN_HOOKS: &[&str] = &[
    "use",
    "useState",
    "useReducer",
    "useEffect",
    "useLayoutEffect",
    "useInsertionEffect",
    "useMemo",
    "useCallback",
    "useRef",
    "useContext",
    "useImperativeHandle",
    "useDebugValue",
    "useId",
    "useDeferredValue",
    "useTransition",
    "useSyncExternalStore",
    "useOptimistic",
    "useActionState",
    "useFormStatus",
    "useEffectEvent",
];

fn unparen(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren { expr } = *ast.node(id) {
        id = expr;
    }
    id
}

fn ident_name(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Arrow { .. } | Node::FunctionExpr { .. })
}

/// Nesting limit for wrapped components (`memo(forwardRef(hoc(...)))`).
const MAX_HOC_DEPTH: usize = 16;

/// Whether `call` looks like a component wrapper (`memo(…)`, `forwardRef(…)`,
/// `withRouter(Foo)`, `connect(map)(Foo)`): its first argument is an inline function, a
/// capitalised identifier (a component declared elsewhere), or another such call. Calls
/// whose first argument is anything else (`createContext(null)`, `require("x")`) are not.
fn is_hoc_call(ast: &Ast, call: NodeId, depth: usize) -> bool {
    if depth > MAX_HOC_DEPTH {
        return false;
    }
    let Node::Call { args, .. } = *ast.node(call) else {
        return false;
    };
    let Some(&first) = ast.list(args).first() else {
        return false;
    };
    let first = unparen(ast, first);
    match *ast.node(first) {
        Node::Arrow { .. } | Node::FunctionExpr { .. } => true,
        Node::Ident { name } => is_component_name(ast.atom(name)),
        Node::Call { .. } => is_hoc_call(ast, first, depth + 1),
        _ => false,
    }
}

/// A wrapped component bound to `binding` (or, for `export default`, to a new temporary)
/// and registered as `name`; inline functions and calls inside it are registered as
/// `name$<callee>` (`A$memo$forwardRef`), with hook signatures, so React keeps their state
/// across edits the same way as a top-level declaration's.
#[allow(clippy::too_many_arguments)]
fn on_hoc_call(
    code: &str,
    ast: &Ast,
    bindings: &[String],
    plan: &mut Plan,
    name: &str,
    binding: Option<&str>,
    call: NodeId,
    depth: usize,
) {
    let Node::Call { callee, args, .. } = *ast.node(call) else {
        return;
    };
    let target = match binding {
        Some(b) => b.to_owned(),
        None => {
            let temp = format!("{}c{}", plan.prefix, plan.temps);
            plan.temps += 1;
            plan.inserts
                .push((ast.span(call).start, format!("{temp} = ")));
            temp
        }
    };
    plan.registrations.push((target, name.to_owned()));
    let Some(&first) = ast.list(args).first() else {
        return;
    };
    let span = ast.span(unparen(ast, callee));
    let callee_text = code
        .get(span.start as usize..span.end as usize)
        .unwrap_or("");
    let inner_name = format!("{name}${callee_text}");
    let inner = unparen(ast, first);
    if is_function(ast, inner) {
        let temp = format!("{}c{}", plan.prefix, plan.temps);
        plan.temps += 1;
        plan.inserts
            .push((ast.span(inner).start, format!("{temp} = ")));
        on_component_function(code, ast, bindings, plan, &temp, &inner_name, inner);
    } else if matches!(ast.node(inner), Node::Call { .. }) && depth < MAX_HOC_DEPTH {
        on_hoc_call(
            code,
            ast,
            bindings,
            plan,
            &inner_name,
            None,
            inner,
            depth + 1,
        );
    }
}

fn analyze(code: &str, ast: &Ast, prefix: &str) -> Plan {
    let mut plan = Plan {
        prefix: prefix.to_owned(),
        ..Plan::default()
    };
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return plan;
    };
    let stmts = ast.list(stmts);
    let bindings = top_level_bindings(ast, stmts);
    for &stmt in stmts {
        match *ast.node(stmt) {
            Node::FunctionDecl {
                name: Some(name),
                body: Some(_),
                ..
            } => {
                if let Some(n) = ident_name(ast, name) {
                    let n = n.to_owned();
                    on_function(code, ast, &bindings, &mut plan, &n, &n, stmt);
                }
            }
            Node::VarDecl { decls, .. } => {
                for &d in ast.list(decls) {
                    let Node::VarDeclarator {
                        name,
                        init: Some(init),
                        ..
                    } = *ast.node(d)
                    else {
                        continue;
                    };
                    let Some(n) = ident_name(ast, name) else {
                        continue;
                    };
                    let n = n.to_owned();
                    let init = unparen(ast, init);
                    if is_function(ast, init) {
                        on_function(code, ast, &bindings, &mut plan, &n, &n, init);
                    } else if is_component_name(&n) && is_hoc_call(ast, init, 0) {
                        on_hoc_call(code, ast, &bindings, &mut plan, &n, Some(&n), init, 0);
                    }
                }
            }
            // `export default memo(…)`, `export default withRouter(Page)`. Anonymous
            // `export default () => …` / `export default function () {}` stay unregistered:
            // React's documented rule is that they don't keep state (the update goes to the
            // importer, which re-renders them as a new type).
            Node::ExportDefaultExpr { expr } => {
                let expr = unparen(ast, expr);
                if is_hoc_call(ast, expr, 0) {
                    on_hoc_call(code, ast, &bindings, &mut plan, "%default%", None, expr, 0);
                }
            }
            _ => {}
        }
    }
    plan
}

/// A function that is a component by position (inside a wrapper), whatever its name.
fn on_component_function(
    code: &str,
    ast: &Ast,
    bindings: &[String],
    plan: &mut Plan,
    target: &str,
    name: &str,
    func: NodeId,
) {
    plan.registrations
        .push((target.to_owned(), name.to_owned()));
    add_signature(code, ast, bindings, plan, target, func);
}

/// A top-level function named `name`, referenced as `target`: registers it if it's a
/// component and adds a signature if it calls hooks.
fn on_function(
    code: &str,
    ast: &Ast,
    bindings: &[String],
    plan: &mut Plan,
    target: &str,
    name: &str,
    func: NodeId,
) {
    let component = is_component_name(name);
    if !component && !is_hook_name(name) {
        return;
    }
    if component {
        plan.registrations
            .push((target.to_owned(), name.to_owned()));
    }
    add_signature(code, ast, bindings, plan, target, func);
}

/// Adds a hook signature for `func` (referenced as `target`) if it calls hooks.
fn add_signature(
    code: &str,
    ast: &Ast,
    bindings: &[String],
    plan: &mut Plan,
    target: &str,
    func: NodeId,
) {
    let (body, expr_body) = match *ast.node(func) {
        Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => {
            (b, false)
        }
        Node::Arrow { body, flags, .. } => (body, flags.contains(FnFlags::EXPRESSION_BODY)),
        _ => return,
    };
    let mut hooks = HookCalls {
        code,
        root: body,
        lhs: Vec::new(),
        key: String::new(),
        custom: Vec::new(),
        force: false,
        bindings,
    };
    walk(ast, body, &mut hooks);
    if hooks.key.is_empty() {
        return;
    }
    let i = plan.signatures.len();
    let span = ast.span(body);
    if expr_body {
        plan.inserts
            .push((span.start, format!("({}s{i}(), ", plan.prefix)));
        plan.inserts.push((span.end, ")".to_owned()));
    } else if code.as_bytes().get(span.start as usize) == Some(&b'{') {
        plan.inserts
            .push((span.start + 1, format!("{}s{i}();", plan.prefix)));
    } else {
        return;
    }
    plan.signatures.push(Sig {
        target: target.to_owned(),
        key: hooks.key,
        custom_hooks: hooks.custom,
        force: hooks.force,
    });
}

/// Names bound at the top level (imports and declarations).
fn top_level_bindings(ast: &Ast, stmts: &[NodeId]) -> Vec<String> {
    let mut out = Vec::new();
    for &s in stmts {
        match *ast.node(s) {
            Node::ImportDecl { specifiers, .. } => {
                for &sp in ast.list(specifiers) {
                    let local = match *ast.node(sp) {
                        Node::ImportDefaultSpecifier { local }
                        | Node::ImportNamespaceSpecifier { local }
                        | Node::ImportSpecifier { local, .. } => local,
                        _ => continue,
                    };
                    if let Some(n) = ident_name(ast, local) {
                        out.push(n.to_owned());
                    }
                }
            }
            Node::FunctionDecl { name: Some(n), .. } | Node::ClassDecl { name: Some(n), .. } => {
                if let Some(n) = ident_name(ast, n) {
                    out.push(n.to_owned());
                }
            }
            Node::VarDecl { decls, .. } => {
                for &d in ast.list(decls) {
                    if let Node::VarDeclarator { name, .. } = *ast.node(d)
                        && let Some(n) = ident_name(ast, name)
                    {
                        out.push(n.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Collects hook calls in one function body (not in nested functions or classes).
struct HookCalls<'a> {
    code: &'a str,
    root: NodeId,
    /// `(call id, declarator name text)` for `const x = useX()`.
    lhs: Vec<(NodeId, &'a str)>,
    key: String,
    custom: Vec<String>,
    /// A custom hook that isn't a top-level binding (a local function, a parameter, a
    /// global): its own hooks can't be part of the signature.
    force: bool,
    bindings: &'a [String],
}

impl<'a> Visitor for HookCalls<'a> {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
        let text = |id: NodeId| -> &'a str {
            let s = ast.span(id);
            self.code
                .get(s.start as usize..s.end as usize)
                .unwrap_or("")
        };
        match *ast.node(id) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
                if id != self.root =>
            {
                Walk::SkipChildren
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                self.lhs.push((unparen(ast, init), text(name)));
                Walk::Continue
            }
            Node::Call { callee, args, .. } => {
                let callee = unparen(ast, callee);
                let (hook, root_name) = match *ast.node(callee) {
                    Node::Ident { name } => (ast.atom(name), Some(ast.atom(name))),
                    Node::Member { obj, prop, .. } => (
                        ident_name(ast, prop).unwrap_or(""),
                        ident_name(ast, unparen(ast, obj)),
                    ),
                    _ => return Walk::Continue,
                };
                if !is_hook_name(hook) {
                    return Walk::Continue;
                }
                let lhs = self
                    .lhs
                    .iter()
                    .rev()
                    .find(|(c, _)| *c == id)
                    .map_or("", |(_, t)| *t);
                if !self.key.is_empty() {
                    self.key.push('\n');
                }
                let _ = write!(self.key, "{}{{{lhs}", text(callee));
                if matches!(hook, "useState" | "useReducer")
                    && let Some(&a) = ast.list(args).first()
                {
                    let _ = write!(self.key, "({})", text(a));
                }
                self.key.push('}');
                if !BUILTIN_HOOKS.contains(&hook) {
                    if root_name.is_some_and(|r| self.bindings.iter().any(|b| b == r)) {
                        let t = text(callee).to_owned();
                        if !self.custom.contains(&t) {
                            self.custom.push(t);
                        }
                    } else {
                        self.force = true;
                    }
                }
                Walk::Continue
            }
            _ => Walk::Continue,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Source maps: insertions never add lines, so shifting generated columns is enough.

fn shift_map(map_json: &str, code: &str, inserts: &[(u32, String)]) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(map_json) else {
        return map_json.to_owned();
    };
    let Some(mappings) = v.get("mappings").and_then(|m| m.as_str()) else {
        return map_json.to_owned();
    };
    // (line, utf-16 column, utf-16 length) per insertion, in order.
    let mut points: Vec<(u32, u32, u32)> = Vec::with_capacity(inserts.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    let mut scanned = 0usize;
    for (at, text) in inserts {
        let at = *at as usize;
        for (i, b) in code.as_bytes()[scanned..at].iter().enumerate() {
            if *b == b'\n' {
                line += 1;
                line_start = scanned + i + 1;
            }
        }
        scanned = at;
        let col = code[line_start..at].encode_utf16().count() as u32;
        points.push((line, col, text.encode_utf16().count() as u32));
    }
    let mut segs = ts_r_codegen::sourcemap::decode_mappings(mappings);
    for s in &mut segs {
        let delta: u32 = points
            .iter()
            .filter(|(l, c, _)| *l == s.gen_line && *c <= s.gen_col)
            .map(|(_, _, n)| n)
            .sum();
        s.gen_col += delta;
    }
    v["mappings"] = serde_json::Value::String(encode_mappings(&segs));
    v.to_string()
}

fn encode_mappings(segs: &[ts_r_codegen::Mapping]) -> String {
    let mut out = String::new();
    let (mut src, mut line, mut col, mut name) = (0i64, 0i64, 0i64, 0i64);
    let mut cur_line = 0u32;
    let mut gen_col = 0i64;
    let mut first = true;
    for s in segs {
        while cur_line < s.gen_line {
            out.push(';');
            cur_line += 1;
            gen_col = 0;
            first = true;
        }
        if !first {
            out.push(',');
        }
        first = false;
        vlq(&mut out, s.gen_col as i64 - gen_col);
        gen_col = s.gen_col as i64;
        if let Some((si, l, c, n)) = s.source {
            vlq(&mut out, si as i64 - src);
            vlq(&mut out, l as i64 - line);
            vlq(&mut out, c as i64 - col);
            src = si as i64;
            line = l as i64;
            col = c as i64;
            if let Some(n) = n {
                vlq(&mut out, n as i64 - name);
                name = n as i64;
            }
        }
    }
    out
}

fn vlq(out: &mut String, v: i64) {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut n: u64 = if v < 0 {
        ((-v as u64) << 1) | 1
    } else {
        (v as u64) << 1
    };
    loop {
        let mut digit = (n & 31) as usize;
        n >>= 5;
        if n > 0 {
            digit |= 32;
        }
        out.push(B64[digit] as char);
        if n == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_hmr::{ImportKind, ImportRecord};

    fn run(code: &str) -> TransformOutput {
        let imports = ts_r_hmr::scan::scan(code).imports;
        add_refresh(
            TransformOutput {
                code: code.to_owned(),
                map: None,
                imports,
            },
            "src/App.tsx",
        )
    }

    #[test]
    fn registers_components_and_hook_signatures() {
        let src = "import { useState } from \"react\";\nimport { useMe } from \"./me\";\nexport function App() {\n  const [n, setN] = useState(0);\n  const me = useMe();\n  return n;\n}\nconst Row = () => useState(1)[0];\nexport const Memo = memo(function Inner() { return 1; });\nfunction helper() { return useState(2); }\nexport function useThing() { return useMe(); }\n";
        let out = run(src);
        let c = &out.code;
        assert!(
            c.starts_with("import * as __tsr_rr from \"/@ts-r/react-refresh\";"),
            "{c}"
        );
        assert!(c.contains("$RefreshReg$(App, \"App\");"), "{c}");
        assert!(c.contains("$RefreshReg$(Row, \"Row\");"), "{c}");
        assert!(c.contains("$RefreshReg$(Memo, \"Memo\");"), "{c}");
        assert!(c.contains("$RefreshReg$(__tsr_c0, \"Memo$memo\");"), "{c}");
        assert!(!c.contains("\"helper\""), "{c}");
        assert!(c.contains("export function App() {__tsr_s0();"), "{c}");
        assert!(
            c.contains("const Row = () => (__tsr_s1(), useState(1)[0]);"),
            "{c}"
        );
        assert!(c.contains("memo(__tsr_c0 = function Inner()"), "{c}");
        assert!(
            c.contains(
                "__tsr_s0(App, \"useState{[n, setN](0)}\\nuseMe{me}\", false, () => [useMe]);"
            ),
            "{c}"
        );
        assert!(
            c.contains("__tsr_s2(useThing, \"useMe{}\", false, () => [useMe]);"),
            "{c}"
        );
        assert!(c.contains("import.meta.hot.accept((next)"), "{c}");
        // Import spans still point at the specifiers.
        for r in &out.imports {
            assert_eq!(&c[r.start as usize..r.end as usize], r.specifier);
        }
        assert!(ts_r_hmr::scan::scan_hot(c).self_accepting);
    }

    #[test]
    fn wrapped_and_default_exported_components() {
        let src = "import { memo, forwardRef, useState } from \"react\";\n\
            function Inner() { const [n] = useState(0); return n; }\n\
            export const Hoc = withBorder(Inner);\n\
            export const Conn = connect(mapState)(Inner);\n\
            export const Ref = forwardRef((props, ref) => { const [v] = useState(1); return v; });\n\
            const Ctx = createContext(null);\n\
            export default memo(forwardRef(function (props, ref) { return 1; }));\n";
        let out = run(src);
        let c = &out.code;
        assert!(c.contains("$RefreshReg$(Inner, \"Inner\");"), "{c}");
        assert!(c.contains("$RefreshReg$(Hoc, \"Hoc\");"), "{c}");
        assert!(c.contains("$RefreshReg$(Conn, \"Conn\");"), "{c}");
        assert!(c.contains("$RefreshReg$(Ref, \"Ref\");"), "{c}");
        // Inline functions inside wrappers: registered as <name>$<callee>, with signatures.
        assert!(
            c.contains("forwardRef(__tsr_c0 = (props, ref) => {__tsr_s1();"),
            "{c}"
        );
        assert!(
            c.contains("$RefreshReg$(__tsr_c0, \"Ref$forwardRef\");"),
            "{c}"
        );
        assert!(c.contains("__tsr_s1(__tsr_c0, \"useState{[v](1)}\""), "{c}");
        // export default: temporaries for each level.
        assert!(
            c.contains("export default __tsr_c1 = memo(__tsr_c2 = forwardRef(__tsr_c3 = function (props, ref)"),
            "{c}"
        );
        assert!(c.contains("$RefreshReg$(__tsr_c1, \"%default%\");"), "{c}");
        assert!(
            c.contains("$RefreshReg$(__tsr_c2, \"%default%$memo\");"),
            "{c}"
        );
        assert!(
            c.contains("$RefreshReg$(__tsr_c3, \"%default%$memo$forwardRef\");"),
            "{c}"
        );
        assert!(
            c.contains("var __tsr_c0, __tsr_c1, __tsr_c2, __tsr_c3;"),
            "{c}"
        );
        // Not wrappers: no registration.
        assert!(!c.contains("\"Ctx\""), "{c}");
        for r in &out.imports {
            assert_eq!(&c[r.start as usize..r.end as usize], r.specifier);
        }
        // Lines never move.
        assert_eq!(
            c.split("\n;").next().unwrap().lines().count(),
            src.lines().count()
        );
    }

    #[test]
    fn anonymous_default_exports_stay_unregistered() {
        for src in [
            "export default function () { return 1; }\n",
            "export default () => 1;\n",
            "export default class extends Component {}\n",
            "export default memo;\n",
        ] {
            let out = run(src);
            assert!(!out.code.contains("$RefreshReg$("), "{}", out.code);
            assert!(!out.code.contains("hot.accept"), "{}", out.code);
        }
        // Even without capitalised bindings or hooks, a wrapped default export is found.
        let out = run("import { memo } from 'react';\nexport default memo(() => 1);\n");
        assert!(
            out.code
                .contains("export default __tsr_c0 = memo(__tsr_c1 = () => 1);"),
            "{}",
            out.code
        );
        assert!(out.code.contains("hot.accept"), "{}", out.code);
    }

    #[test]
    fn untrackable_custom_hooks_force_a_remount() {
        let src = "import { useMe } from './me';\nexport function App() {\n  function useLocal() { return useState(0); }\n  const [n] = useLocal();\n  return n;\n}\nexport function Other() { return useMe(); }\n";
        let c = run(src).code;
        assert!(c.contains("__tsr_s0(App, \"useLocal{[n]}\", true);"), "{c}");
        assert!(
            c.contains("__tsr_s1(Other, \"useMe{}\", false, () => [useMe]);"),
            "{c}"
        );
    }

    #[test]
    fn generated_names_avoid_the_module_s_own() {
        let src = "const __tsr_rr = 1;\nlet __tsr1_x;\nexport function App() {\n  const [n] = useState(0);\n  return n + __tsr_rr;\n}\nexport const M = memo(() => 1);\n";
        let c = run(src).code;
        assert!(c.starts_with("import * as __tsr2_rr from"), "{c}");
        assert!(c.contains("export function App() {__tsr2_s0();"), "{c}");
        assert!(c.contains("memo(__tsr2_c0 = () => 1)"), "{c}");
        assert!(c.contains("let __tsr2_prev;"), "{c}");
        assert!(c.contains("return n + __tsr_rr;"), "{c}");
        assert!(!c.contains("__tsr_s") && !c.contains("__tsr_c"), "{c}");
        let parsed = ts_r_parser::parse(&c, SourceKind::Js);
        assert!(parsed.diagnostics.is_empty(), "{c}");
        // Wrapped default exports' temporaries too.
        let src = "let __tsr_x;\nexport default memo(() => 1);\n";
        let c = run(src).code;
        assert!(
            c.contains("export default __tsr1_c0 = memo(__tsr1_c1 = () => 1);"),
            "{c}"
        );
        assert!(!c.contains("__tsr_c"), "{c}");
        // React Refresh's own names mean the module is already instrumented.
        let src = "const $RefreshSig$ = 1;\nexport function App() { return useState(0); }\n";
        assert_eq!(run(src).code, src);
    }

    #[test]
    fn modules_without_components_are_untouched() {
        let src = "export const x = 1;\nexport function helper() { return 2; }\n";
        assert_eq!(run(src).code, src);
        // Hooks only: signatures, no self-accept.
        let out = run(
            "import { useState } from 'react';\nexport function useFlag() { return useState(false); }\n",
        );
        assert!(out.code.contains("__tsr_s0(useFlag,"), "{}", out.code);
        assert!(!out.code.contains("hot.accept"), "{}", out.code);
    }

    #[test]
    fn source_map_columns_shift_with_insertions() {
        // One segment at line 0 col 0 and one at line 2 col 2 ("  const" in App's body).
        let code =
            "import a from \"a\";\nfunction App() {\n  const [n] = useState(0);\n  return n;\n}\n";
        let map = r#"{"version":3,"sources":["a.tsx"],"names":[],"mappings":"AAAA;AACA;EACE"}"#;
        let out = add_refresh(
            TransformOutput {
                code: code.to_owned(),
                map: Some(map.to_owned()),
                imports: vec![ImportRecord {
                    specifier: "a".into(),
                    start: 15,
                    end: 16,
                    kind: ImportKind::Static,
                }],
            },
            "a.tsx",
        );
        let v: serde_json::Value = serde_json::from_str(out.map.as_deref().unwrap()).unwrap();
        let segs = ts_r_codegen::sourcemap::decode_mappings(v["mappings"].as_str().unwrap());
        let prelude_len = out.code.find("import a").unwrap() as u32;
        assert_eq!(segs[0].gen_col, prelude_len);
        // Line 1 ("function App() {"): the body insertion is after the segment's column.
        assert_eq!(segs[1].gen_col, 0);
        assert_eq!(segs[2].gen_col, 2);
        // The original line 2 is unchanged (insertion was on line 1, after `{`).
        let line1 = out.code.lines().nth(1).unwrap();
        assert_eq!(line1, "function App() {__tsr_s0();");
        assert_eq!(encode_mappings(&segs), v["mappings"].as_str().unwrap());
    }
}
