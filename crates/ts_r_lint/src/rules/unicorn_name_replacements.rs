//! `unicorn/name-replacements`: abbreviated or discouraged names (`e`, `err`, `cb`, `args`,
//! ... or the `replacements` option) get their full forms. A name is looked up whole
//! (first letter lowered, as is, first letter raised) and otherwise split into words (each
//! starts at a character that is not a lowercase letter, and a non-letter is a word of its
//! own); every word is replaced by its sorted replacements, keeping its first letter's case,
//! and up to three combinations are shown (`retVal` -> `returnValue`: a plain word repeated
//! at the end of the previous one is dropped). All-caps names, `allowList` names and names
//! matching `ignore` (plus `i18n`, `l10n`, `a11y`, `e2e`, `jQuery`, `vite-env`) are skipped.
//!
//! Variables (every binding but enum members, scope by scope, with the same import and
//! shorthand options as `consistent-compound-words`) are fixed when there is one
//! replacement, the binding is not an exported declaration or JSX tag, and it is not a
//! parameter property or a parameter of a function documented by an attached `@param`
//! JSDoc; several replacements become suggestions under the same conditions. New names get
//! `_`s until free (see `consistent-compound-words`). With `checkProperties`, property
//! names that define or write a property get suggestions only. The filename (without its
//! extension) is checked too unless `checkFilenames` is `false`.

use super::prelude::*;
use super::unicorn_consistent_compound_words::{
    ImportCheck, can_rename, import_check, import_rules_allow, is_identifier_name,
};
use super::unicorn_filename_case::extname;
use super::unicorn_names::{RegexOpt, regex_list};
use super::unicorn_util::{available_name, estree_ident_span, rename_symbol_fix};
use crate::ast_util::{skip_trivia_backward, skip_trivia_forward};
use crate::diagnostic::Loc;
use crate::semantic::{DeclKind, ScopeId, SymbolFlags, SymbolId};
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/name-replacements",
        "Enforce replacements for variable, property, and filenames.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<NameReplacements>,
};

/// Discouraged word -> its replacements (data from the plugin's defaults).
const DEFAULT_REPLACEMENTS: &[(&str, &[&str])] = &[
    ("acc", &["accumulator"]),
    ("application", &["app"]),
    ("applications", &["apps"]),
    ("arg", &["argument"]),
    ("args", &["arguments"]),
    ("arr", &["array"]),
    ("attr", &["attribute"]),
    ("attrs", &["attributes"]),
    ("btn", &["button"]),
    ("buf", &["buffer"]),
    ("cb", &["callback"]),
    ("cfg", &["config"]),
    ("cmd", &["command"]),
    ("conf", &["config"]),
    ("configuration", &["config"]),
    ("ctx", &["context"]),
    ("cur", &["current"]),
    ("curr", &["current"]),
    ("db", &["database"]),
    ("decl", &["declaration"]),
    ("decls", &["declarations"]),
    ("def", &["defer", "deferred", "define", "definition"]),
    ("dep", &["dependency"]),
    ("deps", &["dependencies"]),
    ("dest", &["destination"]),
    ("dev", &["development"]),
    ("dir", &["direction", "directory"]),
    ("dirs", &["directories"]),
    ("dist", &["distribution"]),
    ("doc", &["document"]),
    ("docs", &["documentation", "documents"]),
    (
        "dst",
        &["daylightSavingTime", "destination", "distribution"],
    ),
    ("e", &["error", "event"]),
    ("el", &["element"]),
    ("elem", &["element"]),
    ("elems", &["elements"]),
    ("env", &["environment"]),
    ("envs", &["environments"]),
    ("err", &["error"]),
    ("ev", &["event"]),
    ("evt", &["event"]),
    ("expr", &["expression"]),
    ("exprs", &["expressions"]),
    ("ext", &["extension"]),
    ("exts", &["extensions"]),
    ("fn", &["function"]),
    ("func", &["function"]),
    ("i", &["index"]),
    ("ident", &["identifier"]),
    ("idents", &["identifiers"]),
    ("idx", &["index"]),
    ("j", &["index"]),
    ("len", &["length"]),
    ("lib", &["library"]),
    ("mod", &["module"]),
    ("msg", &["message"]),
    ("num", &["number"]),
    ("obj", &["object"]),
    ("opts", &["options"]),
    ("param", &["parameter"]),
    ("params", &["parameters"]),
    ("perf", &["performance"]),
    ("pkg", &["package"]),
    ("prev", &["previous"]),
    ("prod", &["production"]),
    ("prop", &["property"]),
    ("props", &["properties"]),
    ("proto", &["prototype"]),
    ("ref", &["reference"]),
    ("refs", &["references"]),
    ("rel", &["related", "relationship", "relative"]),
    ("repo", &["repository"]),
    ("req", &["request"]),
    ("res", &["resource", "response", "result"]),
    ("ret", &["returnValue"]),
    ("retval", &["returnValue"]),
    ("sep", &["separator"]),
    ("src", &["source"]),
    ("stdDev", &["standardDeviation"]),
    ("stmt", &["statement"]),
    ("stmts", &["statements"]),
    ("str", &["string"]),
    ("tbl", &["table"]),
    ("temp", &["temporary"]),
    ("tit", &["title"]),
    ("tmp", &["temporary"]),
    ("util", &["utility"]),
    ("utils", &["utilities"]),
    ("val", &["value"]),
    ("var", &["variable"]),
    ("vars", &["variables"]),
    ("ver", &["version"]),
];
const DEFAULT_ALLOW_LIST: &[&str] = &[
    "defaultProps",
    "devDependencies",
    "EmberENV",
    "getDerivedStateFromProps",
    "getInitialProps",
    "getServerSideProps",
    "getStaticProps",
    "ImportMetaEnv",
    "iOS",
    "ProcessEnv",
    "propTypes",
    "setupFilesAfterEnv",
];
const DEFAULT_IGNORE: &[&str] = &["i18n", "l10n", "a11y", "e2e", "jQuery", "vite-env"];

#[derive(Debug)]
struct NameReplacements {
    check_properties: bool,
    check_variables: bool,
    default_imports: ImportCheck,
    shorthand_imports: ImportCheck,
    shorthand_properties: bool,
    check_filenames: bool,
    /// Word -> its replacements with their enabled flag, in option order.
    replacements: Vec<(String, Vec<(String, bool)>)>,
    allow: Vec<(String, bool)>,
    ignore: Vec<RegexOpt>,
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

impl FromOptions for NameReplacements {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let user = o.get("replacements").and_then(Value::as_object);
        let entries_of = |v: Option<&Value>| -> Vec<(String, bool)> {
            v.and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), truthy(v))).collect())
                .unwrap_or_default()
        };
        let mut replacements: Vec<(String, Vec<(String, bool)>)> = Vec::new();
        if flag("extendDefaultReplacements", true) {
            for (k, v) in DEFAULT_REPLACEMENTS {
                replacements.push((
                    (*k).to_owned(),
                    v.iter().map(|r| ((*r).to_owned(), true)).collect(),
                ));
            }
            for (k, v) in user.into_iter().flatten() {
                let pos = match replacements.iter().position(|(n, _)| n == k) {
                    Some(p) => p,
                    None => {
                        replacements.push((k.clone(), Vec::new()));
                        replacements.len() - 1
                    }
                };
                if *v == Value::Bool(false) {
                    replacements[pos].1.clear();
                    continue;
                }
                for (name, on) in entries_of(Some(v)) {
                    let list = &mut replacements[pos].1;
                    match list.iter_mut().find(|(n, _)| *n == name) {
                        Some(e) => e.1 = on,
                        None => list.push((name, on)),
                    }
                }
            }
        } else {
            for (k, v) in user.into_iter().flatten() {
                replacements.push((k.clone(), entries_of(Some(v))));
            }
        }
        let mut allow: Vec<(String, bool)> = Vec::new();
        if flag("extendDefaultAllowList", true) {
            allow.extend(DEFAULT_ALLOW_LIST.iter().map(|n| ((*n).to_owned(), true)));
        }
        for (k, v) in o
            .get("allowList")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match allow.iter_mut().find(|(n, _)| n == k) {
                Some(e) => e.1 = truthy(v),
                None => allow.push((k.clone(), truthy(v))),
            }
        }
        let mut ignore: Vec<RegexOpt> = DEFAULT_IGNORE
            .iter()
            .map(|p| RegexOpt {
                source: (*p).to_owned(),
                flags: "u".to_owned(),
            })
            .collect();
        ignore.extend(regex_list(o.get("ignore"), "ignore")?);
        Ok(NameReplacements {
            check_properties: flag("checkProperties", false),
            check_variables: flag("checkVariables", true),
            default_imports: import_check(o.get("checkDefaultAndNamespaceImports")),
            shorthand_imports: import_check(o.get("checkShorthandImports")),
            shorthand_properties: flag("checkShorthandProperties", false),
            check_filenames: flag("checkFilenames", true),
            replacements,
            allow,
            ignore,
        })
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

fn is_upper_case(s: &str) -> bool {
    s.to_uppercase() == s
}

/// An approximation of `localeCompare` for words: case-insensitive first, then lowercase
/// before uppercase.
fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| s.to_lowercase();
    key(a).cmp(&key(b)).then_with(|| {
        for (x, y) in a.chars().zip(b.chars()) {
            if x != y {
                return y.is_lowercase().cmp(&x.is_lowercase());
            }
        }
        a.len().cmp(&b.len())
    })
}

/// Splits a name where a character that is not a lowercase letter starts, and after every
/// character that is not a letter.
fn split_words(name: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_letter = true;
    for (i, c) in name.char_indices() {
        if i > 0 && (!c.is_lowercase() || !prev_letter) {
            out.push(&name[start..i]);
            start = i;
        }
        prev_letter = c.is_alphabetic();
    }
    if start < name.len() {
        out.push(&name[start..]);
    }
    out
}

/// (total, up to three samples).
struct Found {
    total: u64,
    samples: Vec<String>,
}

impl NameReplacements {
    fn word_replacements(&self, word: &str) -> Vec<String> {
        let lookup = |k: &str| self.replacements.iter().find(|(n, _)| n == k);
        let Some((_, list)) = lookup(&lower_first(word))
            .or_else(|| lookup(word))
            .or_else(|| lookup(&upper_first(word)))
        else {
            return Vec::new();
        };
        if is_upper_case(word) || self.allow.iter().any(|(n, on)| *on && n == word) {
            return Vec::new();
        }
        let upper = word
            .chars()
            .next()
            .is_some_and(|c| c.to_uppercase().eq([c]));
        let mut out: Vec<String> = list
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| {
                if upper {
                    upper_first(n)
                } else {
                    lower_first(n)
                }
            })
            .collect();
        out.sort_by(|a, b| locale_cmp(a, b));
        out
    }

    fn name_replacements(&self, name: &str) -> Found {
        let none = Found {
            total: 0,
            samples: Vec::new(),
        };
        if is_upper_case(name)
            || self.allow.iter().any(|(n, on)| *on && n == name)
            || self.ignore.iter().any(|p| p.test(name))
        {
            return none;
        }
        let exact = self.word_replacements(name);
        if !exact.is_empty() {
            return Found {
                total: exact.len() as u64,
                samples: exact.into_iter().take(3).collect(),
            };
        }
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase()) {
            return none;
        }
        let mut any = false;
        let combos: Vec<Vec<String>> = split_words(name)
            .into_iter()
            .map(|w| {
                let r = self.word_replacements(w);
                if r.is_empty() {
                    vec![w.to_owned()]
                } else {
                    any = true;
                    r
                }
            })
            .collect();
        if !any {
            return none;
        }
        let total = combos
            .iter()
            .fold(1u64, |t, c| t.saturating_mul(c.len() as u64));
        let mut samples = Vec::new();
        for n in 0..total.min(3) {
            let mut rest = n;
            let mut parts: Vec<String> = vec![String::new(); combos.len()];
            for (i, items) in combos.iter().enumerate().rev() {
                let len = items.len() as u64;
                parts[i] = items[(rest % len) as usize].clone();
                rest /= len;
            }
            let mut i = parts.len().saturating_sub(1);
            while i > 0 {
                if parts[i].bytes().all(|b| b.is_ascii_alphabetic())
                    && !parts[i].is_empty()
                    && parts[i - 1].ends_with(parts[i].as_str())
                {
                    parts.remove(i);
                }
                i -= 1;
            }
            samples.push(parts.concat());
        }
        Found { total, samples }
    }
}

/// The message for `found` naming `name` as a `kind` (`variable`, `property`, `filename`).
fn message(name: &str, found: &Found, kind: &str) -> (String, &'static str) {
    if found.total == 1 {
        return (
            format!(
                "The {kind} `{name}` should be named `{}`. A more descriptive name will do too.",
                found.samples[0]
            ),
            "replace",
        );
    }
    let mut text = found
        .samples
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = found.total - found.samples.len() as u64;
    if omitted > 0 {
        let n = if omitted > 99 {
            "99+".to_owned()
        } else {
            omitted.to_string()
        };
        text.push_str(&format!(", ... ({n} more omitted)"));
    }
    (
        format!(
            "Please rename the {kind} `{name}`. Suggested names are: {text}. A more descriptive name will do too."
        ),
        "suggestion",
    )
}

/// The comment right before `node` (or before the enclosing declaration, assignment,
/// property or statement it starts), ending at most one line above it.
fn attached_comment(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Span> {
    let ast = cx.ast();
    let src = cx.src();
    let comment_before = |pos: u32| -> Option<Span> {
        let p = src[..pos as usize].trim_end().len() as u32;
        ast.comments
            .iter()
            .find(|c| c.span.end == p && c.kind != CommentKind::Hashbang)
            .map(|c| c.span)
    };
    let mut cur = node;
    let mut found = comment_before(cx.span(cur).start);
    for _ in 0..1000 {
        if found.is_some() {
            break;
        }
        let Some(p) = cx.parent(cur) else {
            break;
        };
        if !matches!(
            ast.node(p),
            Node::Assign { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportNamed { .. }
                | Node::ExprStmt { .. }
                | Node::MethodDef { .. }
                | Node::Property { .. }
                | Node::PropertyDef { .. }
                | Node::PropertySignature { .. }
                | Node::TypeAlias { .. }
                | Node::VarDecl { .. }
                | Node::VarDeclarator { .. }
                | Node::Paren { .. }
        ) {
            break;
        }
        cur = p;
        found = comment_before(cx.span(cur).start);
    }
    let c = found?;
    let lines = cx.file.lines;
    (lines.line_of(c.end) + 1 >= lines.line_of(cx.span(cur).start)).then_some(c)
}

/// A parameter binding that a fix may rename: not a parameter property, and its function
/// has no attached JSDoc comment with `@param`.
fn parameter_fixable(cx: &Ctx<'_, '_>, sym: SymbolId) -> bool {
    let sem = cx.semantic();
    let s = sem.symbol(sym);
    let Some(def) = s.decls.first() else {
        return true;
    };
    if def.kind != DeclKind::Param {
        return true;
    }
    if s.flags.contains(SymbolFlags::PARAM_PROPERTY) {
        return false;
    }
    let ast = cx.ast();
    let mut cur = def.name;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            return true;
        };
        if matches!(
            ast.node(p),
            Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::CallSignature { .. }
                | Node::ConstructSignature { .. }
                | Node::ConstructorType { .. }
                | Node::FunctionType { .. }
                | Node::MethodSignature { .. }
        ) {
            let src = cx.src();
            return !attached_comment(cx, p).is_some_and(|c| {
                let text = c.text(src);
                let inner = text.get(2..).unwrap_or("");
                text.starts_with("/*")
                    && inner.trim_start().starts_with('*')
                    && has_param_tag(inner)
            });
        }
        cur = p;
    }
    true
}

fn has_param_tag(text: &str) -> bool {
    text.match_indices("@param").any(|(i, _)| {
        !text[i + 6..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

impl NameReplacements {
    fn check_filename(&self, cx: &mut Ctx<'_, '_>) {
        let path = cx.file.path;
        if !self.check_filenames || path == "<input>" || path == "<text>" {
            return;
        }
        let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let ext = extname(base);
        let stem = &base[..base.len() - ext.len()];
        let mut found = self.name_replacements(stem);
        if found.total == 0 {
            return;
        }
        for s in &mut found.samples {
            s.push_str(ext);
        }
        let (msg, id) = message(base, &found, "filename");
        let src = cx.src();
        let start = skip_trivia_forward(src, 0) as u32;
        let end = (skip_trivia_backward(cx.ast(), src, src.len()) as u32).max(start);
        let lines = cx.file.lines;
        let (s, e) = (
            lines.line_col_utf16(src, start),
            lines.line_col_utf16(src, end),
        );
        cx.report(Span::new(start, end), msg).id(id).loc(Loc {
            line: s.line + 1,
            column: s.col + 1,
            end_line: e.line + 1,
            end_column: e.col + 1,
        });
    }

    fn check_variables(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let mut order: Vec<(ScopeId, SymbolId)> = (0..sem.symbols().len())
            .map(|i| {
                let id = SymbolId(i as u32);
                (sem.symbol(id).scope, id)
            })
            .collect();
        order.sort_by_key(|&(s, id)| (s.0, id.0));
        let mut generated: Vec<(ScopeId, String)> = Vec::new();
        for (scope, sym) in order {
            let s = sem.symbol(sym);
            let Some(def) = s.decls.first() else {
                continue;
            };
            if def.kind == DeclKind::EnumMember
                || s.flags
                    .intersects(SymbolFlags::ARGUMENTS | SymbolFlags::COMMENT_GLOBAL)
            {
                continue;
            }
            if !import_rules_allow(
                cx,
                sym,
                self.default_imports,
                self.shorthand_imports,
                self.shorthand_properties,
            ) {
                continue;
            }
            let name = sem.name(sym).to_owned();
            let mut found = self.name_replacements(&name);
            if found.total == 0 {
                continue;
            }
            let mut scopes: Vec<ScopeId> = s.refs.iter().map(|&r| sem.reference(r).scope).collect();
            scopes.push(scope);
            found.samples = found
                .samples
                .iter()
                .filter_map(|n| {
                    available_name(cx, n, &scopes, &|c| {
                        !generated
                            .iter()
                            .any(|(gs, gn)| gn == c && scopes.contains(gs))
                    })
                })
                .collect();
            if found.samples.is_empty() {
                continue;
            }
            let ident = def.name;
            let (msg, id) = message(&name, &found, "variable");
            let renamable = can_rename(cx, sym) && parameter_fixable(cx, sym);
            let fix = (found.total == 1 && renamable).then(|| {
                let n = found.samples[0].clone();
                generated.extend(scopes.iter().map(|&sc| (sc, n.clone())));
                rename_symbol_fix(cx, sym, &n)
            });
            let suggestions: Vec<(String, Fix)> = if fix.is_none() && renamable && found.total > 1 {
                found
                    .samples
                    .iter()
                    .filter(|n| is_identifier_name(n))
                    .map(|n| (n.clone(), rename_symbol_fix(cx, sym, n)))
                    .collect()
            } else {
                Vec::new()
            };
            let span = estree_ident_span(cx, ident);
            let mut r = cx.report(span, msg).id(id).fix_opt(fix);
            for (n, f) in suggestions {
                r = r.suggest("rename", format!("Rename to `{n}`."), f);
            }
        }
    }

    /// A property-name identifier that defines or writes the property (`export { a as b }`
    /// counts for `b`).
    fn is_property_name(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        let ast = cx.ast();
        let Some(p) = cx.parent(id) else {
            return false;
        };
        match *ast.node(p) {
            Node::Member { prop, .. } if prop == id => {
                let Some(target) = parent_skip_parens(ast, cx.file.parents, p) else {
                    return false;
                };
                matches!(*ast.node(target), Node::Assign { left, .. } if skip_parens(ast, left) == p)
                    || matches!(*ast.node(target), Node::Update { arg, .. } if skip_parens(ast, arg) == p)
            }
            Node::ExportSpecifier {
                exported: Some(e), ..
            } => e == id,
            Node::Property { key, .. }
            | Node::MethodDef { key, .. }
            | Node::PropertyDef { key, .. }
            | Node::PropertySignature { key, .. }
            | Node::MethodSignature { key, .. } => key == id,
            _ => false,
        }
    }
}

impl Rule for NameReplacements {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.check_filename(cx);
        if self.check_variables {
            self.check_variables(cx);
        }
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !self.check_properties {
            return;
        }
        let ast = cx.ast();
        let Node::Ident { name } = *ast.node(node) else {
            return;
        };
        let name = ast.atom(name).to_owned();
        if name == "__proto__" || !Self::is_property_name(cx, node) {
            return;
        }
        let found = self.name_replacements(&name);
        if found.total == 0 {
            return;
        }
        let (msg, id) = message(&name, &found, "property");
        let export = cx
            .parent(node)
            .is_some_and(|p| matches!(ast.node(p), Node::ExportSpecifier { .. }));
        let span = cx.span(node);
        let mut r = cx.report(span, msg).id(id);
        if found.total > 1 && !export {
            for n in found.samples.iter().filter(|n| is_identifier_name(n)) {
                r = r.suggest(
                    "rename",
                    format!("Rename to `{n}`."),
                    Fix::replace(span, n.clone()),
                );
            }
        }
    }
}
