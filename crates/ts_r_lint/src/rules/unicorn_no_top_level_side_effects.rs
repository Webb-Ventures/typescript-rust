//! `unicorn/no-top-level-side-effects`: code that runs when a module with runtime exports
//! is imported must not write global state, and (with `checkUnknownCalls`) must not call
//! anything not known to be pure.
//!
//! Only files with a runtime export are checked (type-only exports, `export {}` and
//! `export =` don't count), and not files starting with a hashbang. Module evaluation is
//! everything outside functions and non-static class field initializers.
//!
//! - A global mutation writes a configured/built-in global or a member chain on one: an
//!   assignment (also through destructuring), `for (x.y of ..)`/`in`, `++`/`--`, `delete`,
//!   or `Object.assign`/`defineProperty`/.. and `Reflect.set`/.. with a global first
//!   argument (`Reflect.set` only with exactly three plain arguments).
//! - An invocation (call, `new`, tagged template, `import()`) is trusted when it carries a
//!   `/* #__PURE__ */` comment, is the callee of a pure-annotated call, sits inside a
//!   global mutation (reported instead), or calls a known pure export (built in, plus
//!   `allow`) through an import or a `const` destructuring of one.
//!
//! Only the innermost problem in a chain of invocations is reported.

use super::prelude::*;
use super::unicorn_conditions::is_ts_wrapper;
use super::unicorn_text::static_string_value;
use crate::semantic::{DeclKind, Resolution};
use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-top-level-side-effects",
        "Disallow top-level side effects in exported modules.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoTopLevelSideEffects>,
};

#[derive(Debug)]
struct NoTopLevelSideEffects {
    check_unknown_calls: bool,
    /// Import source to pure export names (or member paths).
    pure: FxHashMap<String, FxHashSet<String>>,
}

const KNOWN_PURE: &[(&str, &[&str])] = &[
    (
        "react",
        &[
            "memo",
            "forwardRef",
            "lazy",
            "createContext",
            "createRef",
            "createElement",
            "cloneElement",
            "isValidElement",
        ],
    ),
    ("vue", &["defineComponent"]),
    ("eslint/config", &["defineConfig", "globalIgnores"]),
    (
        "@eslint/config-helpers",
        &[
            "defineConfig",
            "globalIgnores",
            "convertIgnorePatternToMinimatch",
        ],
    ),
    ("@stylistic/eslint-plugin", &["configs.customize"]),
    ("stylelint", &["createPlugin"]),
    ("vite", &["defineConfig"]),
    ("vitest/config", &["defineConfig", "defineProject"]),
    ("@playwright/test", &["defineConfig"]),
    ("playwright/test", &["defineConfig"]),
    ("cypress", &["defineConfig"]),
    ("rollup", &["defineConfig"]),
    ("rolldown", &["defineConfig"]),
    ("tsdown", &["defineConfig"]),
    ("astro/config", &["defineConfig"]),
    ("vitepress", &["defineConfig"]),
    ("@rsbuild/core", &["defineConfig"]),
    ("@rslib/core", &["defineConfig"]),
    ("@rspack/cli", &["defineConfig"]),
    ("drizzle-kit", &["defineConfig"]),
    ("unocss", &["defineConfig"]),
    ("@pandacss/dev", &["defineConfig"]),
    ("nuxt/config", &["defineNuxtConfig"]),
    ("memoize", &["default"]),
    ("debounce", &["default"]),
    ("onetime", &["default"]),
    ("p-memoize", &["default"]),
    ("p-debounce", &["default", "promise", "default.promise"]),
    ("throttleit", &["default"]),
    ("p-limit", &["default", "limitFunction"]),
    ("node:util", &["promisify", "callbackify"]),
    ("lodash", &["debounce", "throttle", "memoize", "once"]),
    ("lodash-es", &["debounce", "throttle", "memoize", "once"]),
];

impl FromOptions for NoTopLevelSideEffects {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let obj = obj_opt(opts, 0);
        let mut pure: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        for (source, names) in KNOWN_PURE {
            pure.insert(
                (*source).to_owned(),
                names.iter().map(|n| (*n).to_owned()).collect(),
            );
        }
        for source in ["lodash", "lodash-es"] {
            for method in ["debounce", "throttle", "memoize", "once"] {
                for ext in ["", ".js"] {
                    pure.insert(
                        format!("{source}/{method}{ext}"),
                        std::iter::once("default".to_owned()).collect(),
                    );
                }
            }
        }
        if let Some(allow) = obj.get("allow").and_then(Value::as_object) {
            for (source, names) in allow {
                let set = pure.entry(source.clone()).or_default();
                for n in names.as_array().into_iter().flatten() {
                    if let Some(n) = n.as_str() {
                        set.insert(n.to_owned());
                    }
                }
            }
        }
        Ok(NoTopLevelSideEffects {
            check_unknown_calls: bool_field(obj, "checkUnknownCalls", false),
            pure,
        })
    }
}

/// Through parentheses and TS expression wrappers (an optional chain has no node of its own).
fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..100_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

/// `sourceCode.isGlobalReference`: a reference to a configured, built-in or comment global.
pub(crate) fn is_global_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if !matches!(cx.node(id), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    match sem.reference_at(id).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Global) => true,
        Some(Resolution::Symbol(s)) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
        _ => false,
    }
}

/// The base of a member chain is a global.
fn is_global_target(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = unwrap(ast, id);
    for _ in 0..100_000 {
        match *ast.node(cur) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => cur = unwrap(ast, obj),
            _ => break,
        }
    }
    is_global_reference(cx, cur)
}

/// `getAssignmentTargets`: the identifiers and members an assignment target writes.
fn assignment_targets(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > 200 {
        return;
    }
    let id = unwrap(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::Member { .. } | Node::Index { .. } => out.push(id),
        Node::AssignPattern { left, .. } | Node::Assign { left, .. } => {
            assignment_targets(ast, left, out, depth + 1);
        }
        Node::Rest { arg } | Node::Spread { arg } => assignment_targets(ast, arg, out, depth + 1),
        Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
            for &e in ast.list(elements) {
                assignment_targets(ast, e, out, depth + 1);
            }
        }
        Node::ObjectPattern { props } | Node::ObjectLit { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => {
                        assignment_targets(ast, value.unwrap_or(key), out, depth + 1);
                    }
                    Node::Property { value, .. } => assignment_targets(ast, value, out, depth + 1),
                    Node::ShorthandProp { name, .. } => out.push(name),
                    Node::Rest { arg } | Node::Spread { arg } => {
                        assignment_targets(ast, arg, out, depth + 1);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn member_name(ast: &Ast, id: NodeId) -> Option<(NodeId, String)> {
    match *ast.node(id) {
        Node::Member { obj, prop, .. } => {
            Some((obj, ident_name(ast, prop).unwrap_or_default().to_owned()))
        }
        Node::Index { obj, index, .. } => {
            Some((obj, static_string_value(ast, skip_parens(ast, index))?))
        }
        _ => None,
    }
}

fn is_global_mutation(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut targets = Vec::new();
    match *ast.node(id) {
        Node::Assign { left, .. } | Node::ForIn { left, .. } | Node::ForOf { left, .. } => {
            assignment_targets(ast, left, &mut targets, 0);
            targets.iter().any(|&t| is_global_target(cx, t))
        }
        Node::Update { arg, .. }
        | Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } => is_global_target(cx, arg),
        Node::Call { callee, args, .. } => {
            let args = ast.list(args);
            let Some(&first) = args.first() else {
                return false;
            };
            let callee = unwrap(ast, callee);
            let (receiver, method) = match *ast.node(callee) {
                Node::Member { obj, prop, .. } => {
                    (unwrap(ast, obj), ident_name(ast, prop).map(str::to_owned))
                }
                Node::Index { obj, index, .. } => (
                    unwrap(ast, obj),
                    static_string_value(ast, skip_parens(ast, index)),
                ),
                _ => return false,
            };
            let Some(method) = method else {
                return false;
            };
            let receiver_name = ident_name(ast, receiver);
            if receiver_name == Some("Reflect")
                && method == "set"
                && (args.len() != 3
                    || args
                        .iter()
                        .any(|&a| matches!(ast.node(a), Node::Spread { .. })))
            {
                return false;
            }
            let mutating = match receiver_name {
                Some("Object") => [
                    "assign",
                    "defineProperty",
                    "defineProperties",
                    "freeze",
                    "seal",
                    "preventExtensions",
                    "setPrototypeOf",
                ]
                .contains(&method.as_str()),
                Some("Reflect") => [
                    "defineProperty",
                    "deleteProperty",
                    "set",
                    "preventExtensions",
                    "setPrototypeOf",
                ]
                .contains(&method.as_str()),
                _ => false,
            };
            mutating && is_global_reference(cx, receiver) && is_global_target(cx, first)
        }
        _ => false,
    }
}

fn is_invocation(node: &Node) -> bool {
    matches!(
        node,
        Node::Call { .. }
            | Node::New { .. }
            | Node::TaggedTemplate { .. }
            | Node::ImportCall { .. }
    )
}

/// Evaluated when the module is: not inside a function or a non-static field initializer.
fn is_module_evaluation(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut child = id;
    let mut cur = cx.parent(id);
    for _ in 0..100_000 {
        let Some(p) = cur else {
            return true;
        };
        match *cx.node(p) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                return false;
            }
            Node::PropertyDef {
                modifiers,
                value: Some(v),
                ..
            } if v == child && !modifiers.contains(Modifiers::STATIC) => return false,
            _ => {}
        }
        child = p;
        cur = cx.parent(p);
    }
    true
}

/// The symbol `ident` refers to.
fn resolved_symbol(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<crate::semantic::SymbolId> {
    let sem = cx.semantic();
    sem.reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
}

fn is_type_import(cx: &Ctx<'_, '_>, node: NodeId, parent: NodeId) -> bool {
    matches!(
        cx.node(node),
        Node::ImportSpecifier {
            type_only: true,
            ..
        }
    ) || matches!(
        cx.node(parent),
        Node::ImportDecl {
            type_only: true,
            ..
        }
    )
}

fn is_type_only_decl(cx: &Ctx<'_, '_>, d: &crate::semantic::Decl) -> bool {
    match d.kind {
        DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam => true,
        DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => {
            is_type_import(cx, d.node, d.parent)
        }
        _ => false,
    }
}

/// `isTypeOnlyReference`: an identifier naming a type-only binding.
fn is_type_only_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if !matches!(cx.node(id), Node::Ident { .. }) {
        return false;
    }
    let Some(sym) = resolved_symbol(cx, id) else {
        return false;
    };
    let s = cx.semantic().symbol(sym);
    !s.is_value() || (!s.decls.is_empty() && s.decls.iter().all(|d| is_type_only_decl(cx, d)))
}

/// A program statement that exports something at runtime.
fn is_runtime_export(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(stmt) {
        Node::ExportNamed {
            type_only,
            specifiers,
            source,
            ..
        } => {
            let specs = ast.list(specifiers);
            let all_type_only = !specs.is_empty()
                && specs.iter().all(|&sp| match *ast.node(sp) {
                    Node::ExportSpecifier {
                        type_only, local, ..
                    } => type_only || (source.is_none() && is_type_only_reference(cx, local)),
                    _ => false,
                });
            !type_only && (source.is_some() || !specs.is_empty()) && !all_type_only
        }
        Node::ExportAll { type_only, .. } => !type_only,
        Node::ExportDefaultExpr { expr } => !is_type_only_reference(cx, skip_parens(ast, expr)),
        Node::VarDecl { modifiers, .. }
        | Node::FunctionDecl { modifiers, .. }
        | Node::ClassDecl { modifiers, .. }
        | Node::Enum { modifiers, .. }
        | Node::Module { modifiers, .. }
        | Node::ImportEquals { modifiers, .. } => {
            modifiers.contains(Modifiers::EXPORT) && !modifiers.contains(Modifiers::DECLARE)
        }
        _ => false,
    }
}

impl NoTopLevelSideEffects {
    /// `isKnownPureImport`: `reference` is a runtime import whose export (with `path`
    /// below it) is pure.
    fn is_pure_import(&self, cx: &Ctx<'_, '_>, reference: NodeId, path: &str) -> bool {
        let ast = cx.ast();
        if !matches!(ast.node(reference), Node::Ident { .. }) {
            return false;
        }
        let Some(sym) = resolved_symbol(cx, reference) else {
            return false;
        };
        cx.semantic().symbol(sym).decls.iter().any(|d| {
            let Node::ImportDecl { source, .. } = *ast.node(d.parent) else {
                return false;
            };
            if is_type_import(cx, d.node, d.parent) {
                return false;
            }
            let Some(names) = static_string_value(ast, source).and_then(|s| self.pure.get(&s))
            else {
                return false;
            };
            if let Node::ImportSpecifier {
                imported, local, ..
            } = *ast.node(d.node)
            {
                let name_node = imported.unwrap_or(local);
                let export = ident_name(ast, name_node)
                    .map(str::to_owned)
                    .or_else(|| static_string_value(ast, name_node))
                    .unwrap_or_default();
                if export != "default" {
                    return if path.is_empty() {
                        names.contains(&export)
                    } else {
                        names.contains(&format!("{export}.{path}"))
                    };
                }
            }
            let namespace = matches!(ast.node(d.node), Node::ImportNamespaceSpecifier { .. });
            (!namespace || !path.is_empty())
                && names.contains(if path.is_empty() { "default" } else { path })
        })
    }

    fn is_known_pure_call(&self, cx: &Ctx<'_, '_>, call: NodeId) -> bool {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(call) else {
            return false;
        };
        let mut reference = unwrap(ast, callee);
        let mut members: Vec<String> = Vec::new();
        for _ in 0..10_000 {
            let Some((obj, name)) = member_name(ast, reference) else {
                break;
            };
            members.insert(0, name);
            reference = unwrap(ast, obj);
        }
        // A computed member without a static name stops the walk on the `Index` itself.
        if !matches!(ast.node(reference), Node::Ident { .. }) {
            return false;
        }
        if self.is_pure_import(cx, reference, &members.join(".")) {
            return true;
        }
        if !members.is_empty() {
            return false;
        }
        let Some(sym) = resolved_symbol(cx, reference) else {
            return false;
        };
        let name = ident_name(ast, reference).unwrap_or_default();
        cx.semantic().symbol(sym).decls.iter().any(|d| {
            let Node::VarDeclarator {
                name: pattern,
                init: Some(init),
                ..
            } = *ast.node(d.node)
            else {
                return false;
            };
            if !matches!(
                ast.node(d.parent),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            ) {
                return false;
            }
            let Node::ObjectPattern { props } = *ast.node(pattern) else {
                return false;
            };
            ast.list(props).iter().any(|&p| {
                let Node::BindingProperty {
                    key,
                    value,
                    init: None,
                } = *ast.node(p)
                else {
                    return false;
                };
                let binding = value.unwrap_or(key);
                if ident_name(ast, binding) != Some(name) {
                    return false;
                }
                let key_name = ident_name(ast, key)
                    .map(str::to_owned)
                    .or_else(|| static_string_value(ast, key));
                key_name.is_some_and(|k| self.is_pure_import(cx, unwrap(ast, init), &k))
            })
        })
    }
}

/// A `/* #__PURE__ */` (or `@__PURE__`) block comment before the expression, through
/// other comments and its own parentheses.
fn has_pure_comment_before(cx: &Ctx<'_, '_>, expr: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let mut parens = 0usize;
    let mut cur = expr;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        parens += 1;
        cur = p;
    }
    let mut pos = cx.span(expr).start as usize;
    for _ in 0..100_000 {
        let trimmed = src[..pos].trim_end();
        pos = trimmed.len();
        if pos == 0 {
            return false;
        }
        if let Some(c) = ast
            .comments_in(Span::new(0, pos as u32))
            .last()
            .filter(|c| c.span.end as usize == pos)
        {
            let text = c.span.text(src);
            if text.starts_with("/*") && (text.contains("#__PURE__") || text.contains("@__PURE__"))
            {
                return true;
            }
            pos = c.span.start as usize;
            continue;
        }
        if parens > 0 && trimmed.ends_with('(') {
            parens -= 1;
            pos -= 1;
            continue;
        }
        return false;
    }
    false
}

fn is_pure_annotated(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if !matches!(cx.node(id), Node::Call { .. } | Node::New { .. }) {
        return false;
    }
    let mut outer = id;
    while let Some(p) = parent_skip_parens(cx.ast(), cx.file.parents, outer) {
        if !is_ts_wrapper(cx.node(p)) {
            break;
        }
        outer = p;
    }
    has_pure_comment_before(cx, id) || (outer != id && has_pure_comment_before(cx, outer))
}

struct Finder<'r, 'c, 'a, 'b> {
    rule: &'r NoTopLevelSideEffects,
    cx: &'c Ctx<'a, 'b>,
    reported: FxHashSet<NodeId>,
    out: Vec<(NodeId, bool)>,
}

impl Finder<'_, '_, '_, '_> {
    fn mark(&mut self, node: NodeId, mutation: bool) {
        self.reported.insert(node);
        let mut cur = self.cx.parent(node);
        while let Some(p) = cur {
            if is_invocation(self.cx.node(p)) {
                self.reported.insert(p);
            }
            cur = self.cx.parent(p);
        }
        self.out.push((node, mutation));
    }

    fn is_trusted(&self, node: NodeId) -> bool {
        let cx = self.cx;
        if is_pure_annotated(cx, node) || self.rule.is_known_pure_call(cx, node) {
            return true;
        }
        let mut child = node;
        let mut cur = cx.parent(node);
        while let Some(p) = cur {
            cur = cx.parent(p);
            match *cx.node(p) {
                Node::Paren { .. } => continue,
                Node::ForIn { .. } | Node::ForOf { .. } => {}
                Node::Call { callee, .. } if skip_parens(cx.ast(), callee) == child => {
                    if is_pure_annotated(cx, p) || is_global_mutation(cx, p) {
                        return true;
                    }
                }
                _ => {
                    if is_global_mutation(cx, p) {
                        return true;
                    }
                }
            }
            child = p;
        }
        false
    }
}

impl Visitor for Finder<'_, '_, '_, '_> {
    fn enter(&mut self, _ast: &Ast, id: NodeId) -> Walk {
        let cx = self.cx;
        let candidate = match cx.node(id) {
            Node::Assign { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::Update { .. }
            | Node::Call { .. } => true,
            Node::Unary { op, .. } => *op == UnaryOp::Delete,
            _ => false,
        };
        if candidate && is_module_evaluation(cx, id) && is_global_mutation(cx, id) {
            let target = match *cx.node(id) {
                Node::ForIn { left, .. } | Node::ForOf { left, .. } => left,
                _ => id,
            };
            self.mark(target, true);
        }
        Walk::Continue
    }
    fn leave(&mut self, _ast: &Ast, id: NodeId) {
        if self.rule.check_unknown_calls
            && is_invocation(self.cx.node(id))
            && !self.reported.contains(&id)
            && is_module_evaluation(self.cx, id)
            && !self.is_trusted(id)
        {
            self.mark(id, false);
        }
    }
}

impl Rule for NoTopLevelSideEffects {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if cx.src().starts_with("#!") {
            return;
        }
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        if !ast.list(stmts).iter().any(|&s| is_runtime_export(cx, s)) {
            return;
        }
        let mut finder = Finder {
            rule: self,
            cx,
            reported: FxHashSet::default(),
            out: Vec::new(),
        };
        walk(ast, ast.root, &mut finder);
        let out = finder.out;
        for (node, mutation) in out {
            let span = cx.span(node);
            if mutation {
                cx.report(
                    span,
                    "Do not modify global state when the module is imported.",
                )
                .id("global-mutation");
            } else {
                cx.report(
                    span,
                    "This invocation may cause side effects when the module is imported.",
                )
                .id("no-top-level-side-effects");
            }
        }
    }
}
