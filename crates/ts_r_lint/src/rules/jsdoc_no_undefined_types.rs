//! `jsdoc/no-undefined-types`: every name a JSDoc type, namepath or `{@link}` mentions is
//! declared somewhere the plugin looks: scopes, globals, typedefs, `@import`, `@template`,
//! class and namespace members, or the `definedTypes` option.

use super::jsdoc_valid_types::closure_template_names;
use super::prelude::*;
use crate::diagnostic::Loc;
use crate::jsdoc::{Block, Target, structured_tags_error};
use crate::jsdoc_tags::{SMode, Structure};
use crate::jsdoc_tree::{block_inline_tags, tag_inline_tags};
use crate::jsdoc_type::{self as jt, Id, N, TypeTree};
use crate::jsdoc_util::each_block;
use rustc_hash::FxHashSet;
use ts_r_ast::{Ast, CommentKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-undefined-types",
        "Besides some expected built-in types, prohibits any types not specified as globals or within `@typedef`.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoUndefinedTypes>,
};

#[derive(Debug, Default)]
struct NoUndefinedTypes {
    check_used_typedefs: bool,
    defined_types: Vec<String>,
    disable_reporting: bool,
}

impl FromOptions for NoUndefinedTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first();
        let flag = |k: &str| o.and_then(|o| o.get(k)).and_then(Value::as_bool) == Some(true);
        Ok(NoUndefinedTypes {
            check_used_typedefs: flag("checkUsedTypedefs"),
            defined_types: o
                .and_then(|o| o.get("definedTypes"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            disable_reporting: flag("disableReporting"),
        })
    }
}

/// Names that are always defined.
const EXTRA_TYPES: &[&str] = &[
    "null",
    "undefined",
    "void",
    "string",
    "boolean",
    "object",
    "function",
    "symbol",
    "number",
    "bigint",
    "NaN",
    "Infinity",
    "any",
    "*",
    "never",
    "unknown",
    "const",
    "this",
    "true",
    "false",
    "Array",
    "Object",
    "RegExp",
    "Date",
    "Function",
    "Intl",
];

/// Global objects whose properties are not checked.
const GLOBAL_OBJECTS: &[&str] = &["globalThis", "global", "window", "self"];

/// TypeScript's iteration and utility types (`typescript` mode).
const TS_GLOBALS: &[&str] = &[
    "Iterable",
    "Iterator",
    "IteratorObject",
    "Awaited",
    "Partial",
    "Required",
    "Readonly",
    "Record",
    "Pick",
    "Omit",
    "Exclude",
    "Extract",
    "NonNullable",
    "Parameters",
    "ConstructorParameters",
    "ReturnType",
    "InstanceType",
    "ThisParameterType",
    "OmitThisParameter",
    "ThisType",
    "Uppercase",
    "Lowercase",
    "Capitalize",
    "Uncapitalize",
];

const SETTINGS_ERROR: &str =
    "Invalid `settings.jsdoc.preferredTypes`. Values must be falsy, a string, or an object.";

/// Drops one trailing `.`, `<>`, `.<>` or `[]`.
fn strip_pseudo(s: &str) -> &str {
    [".<>", "<>", ".", "[]"]
        .iter()
        .find_map(|p| s.strip_suffix(p))
        .unwrap_or(s)
}

/// The text between `/*` and `*/` (ESLint's `comment.value`).
fn comment_value(text: &str) -> &str {
    let t = text.strip_prefix("/*").unwrap_or(text);
    t.strip_suffix("*/").unwrap_or(t)
}

/// `/**` but not `/***`, as the plugin picks document-wide blocks.
fn is_doc_comment(text: &str) -> bool {
    let v = comment_value(text);
    v.starts_with('*') && !v[1..].starts_with('*')
}

/// The local names an `import` statement written in a `@import` tag binds: the default,
/// the named imports (by local name) and the namespace. `None` when it isn't an import
/// with a `from "..."` clause.
fn import_names(stmt: &str) -> Option<Vec<String>> {
    let rest = stmt.strip_prefix("import")?;
    if !rest.starts_with(char::is_whitespace) && !rest.starts_with(['{', '*']) {
        return None;
    }
    let (clause, source) = rest.rsplit_once("from")?;
    let source = source.trim().trim_end_matches(';').trim();
    let quoted = source.len() >= 2
        && ((source.starts_with('"') && source.ends_with('"'))
            || (source.starts_with('\'') && source.ends_with('\'')));
    if !quoted {
        return None;
    }
    let mut clause = clause.trim();
    if let Some(c) = clause.strip_prefix("type ") {
        clause = c.trim_start();
    }
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            && !s.starts_with(|c: char| c.is_ascii_digit())
    };
    let mut names = Vec::new();
    let mut cur = clause;
    if !cur.starts_with(['{', '*']) {
        let end = cur.find(',').unwrap_or(cur.len());
        let default = cur[..end].trim();
        if !ident(default) {
            return None;
        }
        names.push(default.to_owned());
        cur = cur[end..].trim_start_matches(',').trim();
    }
    if let Some(inner) = cur.strip_prefix('{') {
        let inner = inner.strip_suffix('}')?;
        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let part = part.strip_prefix("type ").map_or(part, str::trim);
            let local = match part.rsplit_once(" as ") {
                Some((_, l)) => l.trim(),
                None => part,
            };
            if !ident(local) {
                return None;
            }
            names.push(local.to_owned());
        }
    } else if let Some(ns) = cur.strip_prefix('*') {
        let ns = ns.trim().strip_prefix("as")?.trim();
        if !ident(ns) {
            return None;
        }
        names.push(ns.to_owned());
    } else if !cur.is_empty() {
        return None;
    }
    Some(names)
}

/// The `@template` names a block declares.
fn template_names(block: &Block) -> Vec<String> {
    block
        .tags
        .iter()
        .filter(|t| t.tag == "template")
        .flat_map(|t| closure_template_names(&t.name))
        .collect()
}

/// Names of class members with a plain or private name (`Class.member` candidates).
fn class_member_names(ast: &Ast, members: &[NodeId], private_too: bool) -> Vec<String> {
    let mut out = Vec::new();
    for &m in members {
        let key = match *ast.node(m) {
            Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => key,
            _ => continue,
        };
        match *ast.node(key) {
            Node::Ident { name } => out.push(ast.atom(name).to_owned()),
            Node::PrivateName { name } if private_too => out.push(ast.atom(name).to_owned()),
            _ => {}
        }
    }
    out
}

/// Interface members with an identifier key (property and method signatures).
fn interface_member_names(ast: &Ast, body: &[NodeId]) -> Vec<String> {
    body.iter()
        .filter_map(|&m| match *ast.node(m) {
            Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. } => {
                match *ast.node(key) {
                    Node::Ident { name } => Some(ast.atom(name).to_owned()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn ident_name(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        _ => None,
    }
}

/// The qualified names a namespace body declares: type aliases, classes, interfaces (and
/// their members), nested namespaces (recursively).
fn namespace_names(ast: &Ast, prefix: &str, module: NodeId, depth: u32, out: &mut Vec<String>) {
    if depth > 64 {
        return;
    }
    let Node::Module {
        body: Some(body), ..
    } = *ast.node(module)
    else {
        return;
    };
    let Node::ModuleBlock { stmts } = *ast.node(body) else {
        return;
    };
    for &s in ast.list(stmts) {
        match *ast.node(s) {
            Node::TypeAlias { name, .. } => {
                if let Some(n) = ident_name(ast, name) {
                    out.push(format!("{prefix}.{n}"));
                }
            }
            Node::ClassDecl {
                name: Some(name), ..
            } => {
                if let Some(n) = ident_name(ast, name) {
                    out.push(format!("{prefix}.{n}"));
                }
            }
            Node::Interface { name, body, .. } => {
                if let Some(n) = ident_name(ast, name) {
                    out.push(format!("{prefix}.{n}"));
                    for m in interface_member_names(ast, ast.list(body)) {
                        out.push(format!("{prefix}.{n}.{m}"));
                    }
                }
            }
            Node::Module { name, .. } => {
                if let Some(n) = ident_name(ast, name) {
                    let p = format!("{prefix}.{n}");
                    out.push(p.clone());
                    namespace_names(ast, &p, s, depth + 1, out);
                }
            }
            _ => {}
        }
    }
}

/// What the file defines, whatever block is checked.
#[derive(Default)]
struct FileNames {
    defined: FxHashSet<String>,
    closed: FxHashSet<String>,
    imports: Vec<String>,
    globals: Vec<String>,
    import_tags: Vec<String>,
    typedefs: Vec<String>,
    all_templates: Vec<String>,
}

impl NoUndefinedTypes {
    fn file_names(&self, cx: &Ctx<'_, '_>, structure: &Structure, mode: &str) -> FileNames {
        let ast = cx.ast();
        let src = cx.src();
        let sem = cx.semantic();
        let mut f = FileNames::default();
        let blocks: Vec<Block> = ast
            .comments
            .iter()
            .filter(|c| c.kind == CommentKind::Block && is_doc_comment(c.span.text(src)))
            .map(|c| Block::parse(c.span.text(src)))
            .collect();
        for c in &ast.comments {
            let v = match c.kind {
                CommentKind::Block => comment_value(c.span.text(src)),
                CommentKind::Line => &c.span.text(src)[2..],
                _ => continue,
            };
            if let Some(rest) = v.trim_start().strip_prefix("globals") {
                let rest = rest.trim();
                let mut parts = Vec::new();
                let mut cur = rest;
                while let Some(at) = cur.find(',') {
                    parts.push(&cur[..at]);
                    cur = cur[at + 1..].trim_start();
                }
                parts.push(cur);
                f.globals.extend(parts.into_iter().map(str::to_owned));
            }
        }
        for b in &blocks {
            for t in &b.tags {
                if structure.is_name_or_namepath_defining(&t.tag)
                    && !matches!(
                        t.tag.as_str(),
                        "arg" | "argument" | "param" | "prop" | "property"
                    )
                {
                    f.typedefs.push(t.name.clone());
                }
            }
            f.all_templates.extend(template_names(b));
        }
        if mode == "typescript" {
            for b in &blocks {
                for t in b.tags.iter().filter(|t| t.tag == "import") {
                    let type_part = if t.typ.is_empty() {
                        String::new()
                    } else {
                        format!("{{{}}} ", t.typ)
                    };
                    let stmt = if t.description.is_empty() {
                        format!("import {type_part}{}", t.name)
                    } else {
                        format!("import {type_part}{} {}", t.name, t.description)
                    };
                    if let Some(names) = import_names(stmt.trim()) {
                        f.import_tags
                            .extend(names.into_iter().filter(|n| !n.is_empty()));
                    }
                }
            }
        }

        // Global and module scope.
        let global = sem.global_scope();
        let scopes = sem.scopes();
        for &s in &sem.scope(global).symbols {
            f.defined.insert(sem.name(s).to_owned());
        }
        let module_scoped = scopes
            .iter()
            .any(|s| s.parent == Some(global) && s.node == ast.root);
        if module_scoped {
            for scope in scopes.iter().filter(|s| s.parent == Some(global)) {
                for &sym in &scope.symbols {
                    let name = sem.name(sym).to_owned();
                    let Some(decl) = sem.symbol(sym).decls.first().copied() else {
                        f.defined.insert(name);
                        continue;
                    };
                    match decl.kind {
                        DeclKind::Class => {
                            f.closed.insert(name.clone());
                            if let Node::ClassDecl { members, .. } = *ast.node(decl.node) {
                                for m in class_member_names(ast, ast.list(members), true) {
                                    f.defined.insert(format!("{name}.{m}"));
                                }
                            }
                        }
                        DeclKind::ImportDefault
                        | DeclKind::ImportNamespace
                        | DeclKind::ImportNamed => f.imports.push(name.clone()),
                        DeclKind::Interface => {
                            if let Node::Interface { body, .. } = *ast.node(decl.node) {
                                for m in interface_member_names(ast, ast.list(body)) {
                                    f.defined.insert(format!("{name}.{m}"));
                                }
                            }
                        }
                        DeclKind::Namespace => {
                            f.closed.insert(name.clone());
                            let mut out = Vec::new();
                            namespace_names(ast, &name, decl.node, 0, &mut out);
                            f.defined.extend(out);
                        }
                        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                            let required = cx.parent(decl.name) == Some(decl.node)
                                && matches!(*ast.node(decl.node), Node::VarDeclarator { init: Some(init), .. }
                                    if matches!(*ast.node(init), Node::Call { callee, .. }
                                        if ident_name(ast, callee).as_deref() == Some("require")));
                            if required {
                                f.imports.push(name.clone());
                            }
                        }
                        _ => {}
                    }
                    f.defined.insert(name);
                }
            }
        }
        f.defined
            .extend(EXTRA_TYPES.iter().map(|s| (*s).to_owned()));
        f.defined.extend(f.typedefs.iter().cloned());
        f.defined.extend(f.import_tags.iter().cloned());
        f.defined.extend(self.defined_types.iter().cloned());
        for scope in scopes.iter().filter(|s| s.kind == ScopeKind::TsModule) {
            for &sym in &scope.symbols {
                f.defined.insert(sem.name(sym).to_owned());
            }
        }
        // `Class.prop = ...` at the top level.
        if let Node::Program { stmts } = *ast.node(ast.root) {
            for &s in ast.list(stmts) {
                let Node::ExprStmt { expr } = *ast.node(s) else {
                    continue;
                };
                let Node::Assign { left, .. } = *ast.node(expr) else {
                    continue;
                };
                let Node::Member { obj, prop, .. } = *ast.node(left) else {
                    continue;
                };
                if let (Some(o), Some(p)) = (ident_name(ast, obj), ident_name(ast, prop))
                    && f.closed.contains(&o)
                {
                    f.defined.insert(format!("{o}.{p}"));
                }
            }
        }
        f
    }
}

/// The names `getScope(node)` sees for a block's node: its scope, every scope above it,
/// and the scopes one, three, five, ... levels below it.
fn runtime_names(cx: &Ctx<'_, '_>, target: Target) -> Vec<String> {
    let sem = cx.semantic();
    let scopes = sem.scopes();
    let mut node = match target {
        Target::Node(id) => Some(id),
        Target::Export(id) => cx.parent(id),
    };
    let mut found = None;
    let mut guard = 0;
    while let Some(n) = node {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        if let Some(s) = sem.scope_of_node(n) {
            found = Some(s);
            break;
        }
        node = cx.parent(n);
    }
    let Some(mut start) = found else {
        return Vec::new();
    };
    let children = |s: ScopeId| -> Vec<ScopeId> {
        let mut out = Vec::new();
        for (i, c) in scopes.iter().enumerate() {
            if c.parent == Some(s) {
                if c.kind == ScopeKind::ClassExprName {
                    out.extend(
                        scopes
                            .iter()
                            .enumerate()
                            .filter(|(_, g)| g.parent == Some(ScopeId(i as u32)))
                            .map(|(j, _)| ScopeId(j as u32)),
                    );
                } else {
                    out.push(ScopeId(i as u32));
                }
            }
        }
        out
    };
    if matches!(
        sem.scope(start).kind,
        ScopeKind::FunctionExprName | ScopeKind::ClassExprName
    ) && let Some(&c) = children(start).first()
    {
        start = c;
    }
    let mut out = Vec::new();
    let add = |s: ScopeId, out: &mut Vec<String>| {
        for &sym in &sem.scope(s).symbols {
            out.push(sem.name(sym).to_owned());
        }
    };
    // Odd levels below `start`.
    let mut level = vec![start];
    let mut depth = 0u32;
    while !level.is_empty() && depth < 10_000 {
        depth += 1;
        let next: Vec<ScopeId> = level.iter().flat_map(|&s| children(s)).collect();
        if depth % 2 == 1 {
            for &s in &next {
                add(s, &mut out);
            }
        }
        level = next;
    }
    let mut cur = Some(start);
    while let Some(s) = cur {
        add(s, &mut out);
        cur = sem.scope(s).parent;
    }
    out
}

impl Rule for NoUndefinedTypes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings_value = cx
            .env()
            .settings
            .get("jsdoc")
            .cloned()
            .unwrap_or(Value::Null);
        let settings_ok = structured_tags_error(cx).is_none();
        let mode = settings_value
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("typescript")
            .to_owned();
        let structure = Structure::new(SMode::from_setting(&mode), &settings_value);
        let file = if settings_ok {
            self.file_names(cx, &structure, &mode)
        } else {
            FileNames::default()
        };
        // Preference replacements count as defined.
        let mut preferred: Vec<String> = Vec::new();
        let mut bad_preference = false;
        if let Some(p) = settings_value
            .get("preferredTypes")
            .and_then(Value::as_object)
        {
            for v in p.values() {
                let s = match v {
                    Value::String(s) => Some(s.as_str()),
                    Value::Null | Value::Bool(false) | Value::Array(_) => None,
                    Value::Object(o) => o.get("replacement").and_then(Value::as_str),
                    Value::Number(n) if n.as_f64() == Some(0.0) => None,
                    _ => {
                        bad_preference = true;
                        None
                    }
                };
                if let Some(s) = s.map(strip_pseudo).filter(|s| !s.is_empty()) {
                    preferred.push(s.to_owned());
                }
            }
        }
        let structured: Vec<(String, Vec<String>)> = settings_value
            .get("structuredTags")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| {
                        let types = v.get("type")?.as_array()?;
                        Some((
                            k.clone(),
                            types
                                .iter()
                                .filter_map(|t| t.as_str().map(str::to_owned))
                                .collect(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let env_globals = cx.env().globals.clone();
        let lib = cx.env().lib.clone();
        let mut found_typedefs: Vec<String> = Vec::new();
        each_block(cx, |cx, finder, doc| {
            if bad_preference {
                cx.report(Span::at(0), SETTINGS_ERROR).loc(Loc::at(1, 2));
            }
            let ast = finder.ast;
            // Per-block definitions.
            let mut local: FxHashSet<String> = preferred.iter().cloned().collect();
            if let Some(Target::Node(id)) = doc.target
                && let Node::MethodDef { .. } = ast.node(id)
                && let Some(class) = finder.parent(id)
                && let Node::ClassDecl { name, members, .. } | Node::ClassExpr { name, members, .. } =
                    *ast.node(class)
            {
                let class_name = name
                    .and_then(|n| ident_name(ast, n))
                    .unwrap_or_else(|| "undefined".to_owned());
                for m in ast.list(members).iter().filter(|&&m| match *ast.node(m) {
                    Node::MethodDef { modifiers, .. } | Node::PropertyDef { modifiers, .. } => {
                        !modifiers.contains(Modifiers::ABSTRACT)
                    }
                    _ => false,
                }) {
                    for n in class_member_names(ast, std::slice::from_ref(m), false) {
                        local.insert(format!("{class_name}.{n}"));
                        local.insert(n);
                    }
                }
            }
            if let Some(t) = doc.target {
                local.extend(runtime_names(cx, t));
            }
            if mode != "jsdoc" {
                if mode == "typescript" {
                    local.extend(TS_GLOBALS.iter().map(|s| (*s).to_owned()));
                }
                match doc.target {
                    Some(t) => {
                        let mut n = Some(t.id());
                        let mut guard = 0;
                        while let Some(id) = n
                            && id != ast.root
                            && guard < 100_000
                        {
                            guard += 1;
                            let mut comments = vec![finder.jsdoc_of(id)];
                            if finder.is_exported_decl(id) {
                                comments.push(finder.jsdoc_of_target(Target::Export(id)));
                            }
                            for c in comments.into_iter().flatten() {
                                let text = ast.comments[c].span.text(finder.src);
                                local.extend(template_names(&Block::parse(text)));
                            }
                            n = finder.parent(id);
                        }
                    }
                    None => local.extend(file.all_templates.iter().cloned()),
                }
            }
            let is_defined = |name: &str, local: &FxHashSet<String>| {
                file.defined.contains(name)
                    || local.contains(name)
                    || env_globals.contains(name)
                    || lib.as_ref().is_some_and(|l| l.is_global(name))
            };
            let defined_here: FxHashSet<&str> = doc
                .block
                .tags
                .iter()
                .filter(|t| structure.is_name_or_namepath_defining(&t.tag))
                .map(|t| t.name.as_str())
                .collect();
            let parse = |text: &str| match jt::Mode::from_setting(&mode) {
                Some(m) => jt::parse(text, m, false),
                None => jt::try_parse(text, false),
            };
            // (tree, tag name, comment-relative line)
            let mut checked: Vec<(TypeTree, String, i64)> = Vec::new();
            for (i, t) in doc.block.tags.iter().enumerate() {
                if t.tag != "import"
                    && structure.might_type(&t.tag)
                    && (t.tag != "suppress" || mode != "closure")
                    && let Some(tree) = parse(&t.typ)
                {
                    checked.push((tree, t.tag.clone(), doc.block.tag_line(i)));
                }
            }
            for (i, t) in doc.block.tags.iter().enumerate() {
                if structure.is_namepath_referencing(&t.tag)
                    && let Some(tree) = parse(&t.name)
                {
                    checked.push((tree, t.tag.clone(), doc.block.tag_line(i)));
                }
            }
            let mut inline = block_inline_tags(&doc.block);
            for i in 0..doc.block.tags.len() {
                inline.extend(tag_inline_tags(&doc.block, i));
            }
            for t in inline {
                if structure.is_namepath_or_url_referencing(&t.tag)
                    && let Some(tree) = parse(&t.namepath_or_url)
                {
                    checked.push((tree, t.tag, t.line));
                }
            }
            let not_appended = |v: &str| {
                file.imports.iter().any(|x| x == v)
                    || file.globals.iter().any(|x| x == v)
                    || env_globals.contains(v)
                    || file.import_tags.iter().any(|x| x == v)
                    || EXTRA_TYPES.contains(&v)
                    || file.typedefs.iter().any(|x| x == v)
                    || GLOBAL_OBJECTS.contains(&v)
            };
            let mut found: Vec<String> = Vec::new();
            let mut reports: Vec<(i64, String)> = Vec::new();
            for (tree, tag, line) in &checked {
                let walk = tree.walk();
                let mut parents: Vec<Option<Id>> = vec![None; tree.nodes.len()];
                for &(id, p, _) in &walk {
                    parents[id] = p;
                }
                for &(id, parent, _) in &walk {
                    let N::Name(value) = &tree.nodes[id] else {
                        continue;
                    };
                    let mut val = value.clone();
                    let mut cur = Some(id);
                    loop {
                        cur = cur.and_then(|c| parents[c]);
                        let Some(c) = cur else { break };
                        if !not_appended(&val)
                            && let N::NamePath { right, .. } = &tree.nodes[c]
                            && let N::Property { value: pv, .. } = &tree.nodes[*right]
                        {
                            val = format!("{val}.{pv}");
                        }
                        if !matches!(tree.nodes[c], N::NamePath { .. }) {
                            break;
                        }
                    }
                    let structured_ok = structured
                        .iter()
                        .any(|(t, types)| t == tag && types.contains(&val));
                    let root = val.split('.').next().unwrap_or("");
                    let namespace_ok = (self.defined_types.iter().any(|d| d == root)
                        || is_defined(root, &local))
                        && !file.closed.contains(root);
                    if !is_defined(&val, &local)
                        && !defined_here.contains(val.as_str())
                        && !structured_ok
                        && !namespace_ok
                    {
                        match parent.map(|p| &tree.nodes[p]) {
                            Some(N::TypeParameter { .. }) => continue,
                            Some(N::Function {
                                type_params: Some(tps),
                                ..
                            }) if tps.iter().any(|&tp| {
                                matches!(&tree.nodes[tp], N::TypeParameter { name, .. }
                                    if matches!(&tree.nodes[*name], N::Name(n) if n == value))
                            }) =>
                            {
                                continue;
                            }
                            Some(N::Infer(el)) if matches!(&tree.nodes[*el], N::Name(n) if n == value) =>
                            {
                                local.insert(value.clone());
                                continue;
                            }
                            _ => {}
                        }
                        if !self.disable_reporting {
                            reports.push((*line, format!("The type '{val}' is undefined.")));
                        }
                    }
                    if self.check_used_typedefs && file.typedefs.contains(&val) {
                        found.push(val);
                    }
                }
            }
            found_typedefs = found;
            for (line, msg) in reports {
                let span = doc.line_span(finder, line);
                cx.report(span, msg);
            }
        });
        if !self.check_used_typedefs || !settings_ok {
            return;
        }
        let ast = cx.ast();
        let src = cx.src();
        let mut unused = Vec::new();
        for c in &ast.comments {
            if c.kind != CommentKind::Block || !is_doc_comment(c.span.text(src)) {
                continue;
            }
            let b = Block::parse(c.span.text(src));
            if let Some(first) = b
                .tags
                .iter()
                .find(|t| structure.is_name_or_namepath_defining(&t.tag))
                && !found_typedefs.contains(&first.name)
            {
                unused.push(c.span);
            }
        }
        for span in unused {
            cx.report(span, "This typedef was not used within the file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_statements() {
        let n = |s: &str| import_names(s);
        assert_eq!(n("import X from \"m\""), Some(vec!["X".into()]));
        assert_eq!(
            n("import X, { A as B, C } from 'm'"),
            Some(vec!["X".into(), "B".into(), "C".into()])
        );
        assert_eq!(
            n("import X, * as NS from \"m\""),
            Some(vec!["X".into(), "NS".into()])
        );
        assert_eq!(
            n("import {Linter} from \"eslint\""),
            Some(vec!["Linter".into()])
        );
        assert_eq!(n("import Bad"), None);
        assert_eq!(n("import"), None);
    }

    #[test]
    fn pseudo_suffixes() {
        assert_eq!(strip_pseudo("A.<>"), "A");
        assert_eq!(strip_pseudo("A<>"), "A");
        assert_eq!(strip_pseudo("A[]"), "A");
        assert_eq!(strip_pseudo("<>"), "");
    }
}
