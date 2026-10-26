//! `import/no-extraneous-dependencies`: an import of an external package (or, with
//! `includeInternal`, an internal one) that the project's `package.json` does not list in
//! `dependencies` (or, where allowed, `devDependencies`, `optionalDependencies`,
//! `peerDependencies`, `bundledDependencies`).
//!
//! - Specifiers: ES modules and `require()` (the module visitor with CommonJS on); the
//!   report is on the importing node. Type-only imports and exports (`import type`,
//!   `export type ... from`, imports whose specifiers are all `type`) are skipped unless
//!   `includeTypes`. Only specifiers that resolve are checked.
//! - The package name is the specifier's first segment (two for a scope); its parent
//!   segments count too (`a/b` passes when `a` is listed). When that name isn't listed,
//!   the name in the `package.json` nearest the resolved file (one without a `name`
//!   passes the search upward) is tried, and is the one messages show.
//! - `package.json`: the nearest one above the file, or the ones `packageDir` names
//!   (resolved from the cwd, merged in order; `bundledDependencies` arrays merge by
//!   index, as `Object.assign` does). With exactly one `packageDir`, a missing or broken
//!   file is reported at line 0 ("could not be found" / "could not be parsed: <V8's
//!   JSON.parse message>"); with several, it is skipped.
//! - `devDependencies` (and the other three) are `true`/`false` or globs matched against
//!   the file's path, or against the cwd joined with the glob; absent means allowed.

use super::import_no_relative_packages::named_package;
use super::import_util::{MODULE_KINDS, ModuleSystems, module_refs, resolve, string_literal};
use super::prelude::*;
use crate::modules::import_type::{ImportType, import_type};
use crate::modules::minimatch;
use rustc_hash::FxHashSet;
use ts_r_ast::Ast;
use ts_r_config::path;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-extraneous-dependencies",
        "Forbid the use of extraneous packages.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoExtraneousDependencies>,
};

/// `true`/`false`, or globs.
#[derive(Debug, Clone)]
enum Allow {
    Unset,
    Bool(bool),
    Globs(Vec<String>),
}

impl Allow {
    fn parse(v: Option<&Value>) -> Allow {
        match v {
            Some(Value::Bool(b)) => Allow::Bool(*b),
            Some(Value::Array(a)) => Allow::Globs(
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => Allow::Unset,
        }
    }

    fn allows(&self, file: &str, cwd: &str) -> bool {
        match self {
            Allow::Unset => true,
            Allow::Bool(b) => *b,
            Allow::Globs(globs) => globs.iter().any(|g| {
                minimatch::matches(g, file)
                    || minimatch::matches(&path::normalize(&path::combine(cwd, g)), file)
            }),
        }
    }
}

#[derive(Debug)]
struct NoExtraneousDependencies {
    dev: Allow,
    optional: Allow,
    peer: Allow,
    bundled: Allow,
    package_dir: Vec<String>,
    include_internal: bool,
    include_types: bool,
}

impl FromOptions for NoExtraneousDependencies {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let get = |k: &str| o.get(k);
        let package_dir = match get("packageDir") {
            Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        Ok(NoExtraneousDependencies {
            dev: Allow::parse(get("devDependencies")),
            optional: Allow::parse(get("optionalDependencies")),
            peer: Allow::parse(get("peerDependencies")),
            bundled: Allow::parse(get("bundledDependencies")),
            package_dir,
            include_internal: bool_field(o, "includeInternal", false),
            include_types: bool_field(o, "includeTypes", false),
        })
    }
}

/// The dependency fields of the project's `package.json` files.
#[derive(Debug, Default)]
struct Deps {
    deps: FxHashSet<String>,
    dev: FxHashSet<String>,
    optional: FxHashSet<String>,
    peer: FxHashSet<String>,
    bundled: Vec<String>,
}

impl Deps {
    fn add(&mut self, pkg: &Value) {
        let keys = |v: &Value, set: &mut FxHashSet<String>| {
            if let Value::Object(o) = v {
                set.extend(o.keys().cloned());
            }
        };
        keys(&pkg["dependencies"], &mut self.deps);
        keys(&pkg["devDependencies"], &mut self.dev);
        keys(&pkg["optionalDependencies"], &mut self.optional);
        keys(&pkg["peerDependencies"], &mut self.peer);
        let bundled = match pkg.get("bundleDependencies").filter(|v| truthy(v)) {
            Some(v) => v,
            None => &pkg["bundledDependencies"],
        };
        let list: Vec<String> = match bundled {
            Value::Array(a) => a
                .iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                .collect(),
            Value::Object(o) => o.keys().cloned().collect(),
            _ => Vec::new(),
        };
        // `Object.assign` of arrays: element `i` replaces element `i`.
        for (i, name) in list.into_iter().enumerate() {
            match self.bundled.get_mut(i) {
                Some(slot) => *slot = name,
                None => self.bundled.push(name),
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.deps.is_empty()
            && self.dev.is_empty()
            && self.optional.is_empty()
            && self.peer.is_empty()
            && self.bundled.is_empty()
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Status {
    deps: bool,
    dev: bool,
    optional: bool,
    peer: bool,
    bundled: bool,
}

/// Whether `name` or one of its parent names (`a` for `a/b`; scopes alone don't count)
/// is listed, added to `st`.
fn declaration(deps: &Deps, name: &str, mut st: Status) -> Status {
    let parts: Vec<&str> = name.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if part.starts_with('@') {
            continue;
        }
        let ancestor = parts[..=i].join("/");
        st.deps |= deps.deps.contains(&ancestor);
        st.dev |= deps.dev.contains(&ancestor);
        st.optional |= deps.optional.contains(&ancestor);
        st.peer |= deps.peer.contains(&ancestor);
        st.bundled |= deps.bundled.contains(&ancestor);
    }
    st
}

/// `@scope/name` or the first segment.
fn original_name(name: &str) -> String {
    let mut parts = name.split('/');
    let first = parts.next().unwrap_or("");
    if first.starts_with('@') {
        format!("{first}/{}", parts.next().unwrap_or("undefined"))
    } else {
        first.to_owned()
    }
}

struct Allowed {
    dev: bool,
    optional: bool,
    peer: bool,
    bundled: bool,
}

impl Allowed {
    fn ok(&self, st: Status) -> bool {
        st.deps
            || self.dev && st.dev
            || self.peer && st.peer
            || self.optional && st.optional
            || self.bundled && st.bundled
    }
}

/// A type-only import or export (`importKind`/`exportKind` `type`, or every specifier an
/// `import { type x }`).
fn is_type_only(ast: &Ast, node: NodeId) -> bool {
    match *ast.node(node) {
        Node::ImportDecl {
            type_only: true, ..
        }
        | Node::ExportNamed {
            type_only: true, ..
        }
        | Node::ExportAll {
            type_only: true, ..
        } => true,
        Node::ImportDecl { specifiers, .. } => {
            let specs = ast.list(specifiers);
            !specs.is_empty()
                && specs.iter().all(|&s| {
                    matches!(
                        ast.node(s),
                        Node::ImportSpecifier {
                            type_only: true,
                            ..
                        }
                    )
                })
        }
        _ => false,
    }
}

const PROGRAM_LOC: crate::diagnostic::Loc = crate::diagnostic::Loc {
    line: 0,
    column: 1,
    end_line: 0,
    end_column: 1,
};

impl NoExtraneousDependencies {
    /// The dependencies to check against, or `Err` with a file-level problem to report.
    fn dependencies(&self, cx: &Ctx<'_, '_>) -> Result<Deps, String> {
        let m = cx.file.modules();
        let mut deps = Deps::default();
        if self.package_dir.is_empty() {
            if let Some(dir) = m.host().package_dir(m.dir())
                && let Some(pkg) = m.host().package_json(&dir)
            {
                deps.add(&pkg);
            }
        } else {
            let single = self.package_dir.len() == 1;
            for d in &self.package_dir {
                let file = path::combine(&path::resolve(&m.cwd, d), "package.json");
                let text = if m.fs().file_exists(&file) {
                    m.fs().read_file(&file)
                } else {
                    None
                };
                let Some(text) = text else {
                    if single {
                        return Err("The package.json file could not be found.".to_owned());
                    }
                    continue;
                };
                match serde_json::from_str::<Value>(&text) {
                    Ok(pkg) => deps.add(&pkg),
                    Err(_) if single => {
                        return Err(format!(
                            "The package.json file could not be parsed: {}",
                            v8_json_error(&text)
                        ));
                    }
                    Err(_) => {}
                }
            }
        }
        Ok(if deps.is_empty() {
            Deps::default()
        } else {
            deps
        })
    }
}

impl Rule for NoExtraneousDependencies {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let deps = match self.dependencies(cx) {
            Ok(d) => d,
            Err(message) => {
                cx.report(Span::at(0), message).loc(PROGRAM_LOC);
                Deps::default()
            }
        };
        let (file, cwd) = {
            let m = cx.file.modules();
            (m.file.clone(), m.cwd.clone())
        };
        let allowed = Allowed {
            dev: self.dev.allows(&file, &cwd),
            optional: self.optional.allows(&file, &cwd),
            peer: self.peer.allows(&file, &cwd),
            bundled: self.bundled.allows(&file, &cwd),
        };
        let ast = cx.ast();
        let systems = ModuleSystems {
            esmodule: true,
            commonjs: true,
            amd: false,
        };
        let mut refs = Vec::new();
        super::import_util::walk(ast, |id| {
            if MODULE_KINDS.contains(&ast.node(id).kind()) {
                refs.extend(module_refs(ast, id, systems));
            }
        });
        for r in refs {
            if !self.include_types && is_type_only(ast, r.importer) {
                continue;
            }
            let Some(name) = string_literal(ast, r.source) else {
                continue;
            };
            let Some(resolved) = resolve(cx, name) else {
                continue;
            };
            let m = cx.file.modules();
            let kind = import_type(m, name, resolved.path());
            if kind != ImportType::External
                && (kind != ImportType::Internal || !self.include_internal)
            {
                continue;
            }
            let Some(target) = resolved.path() else {
                continue;
            };
            let original = original_name(name);
            let mut st = declaration(&deps, &original, Status::default());
            if allowed.ok(st) {
                continue;
            }
            let real = named_package(m, path::directory_of(target)).map(|(_, n)| n);
            if let Some(real) = real.as_deref().filter(|r| *r != original) {
                st = declaration(&deps, real, st);
                if allowed.ok(st) {
                    continue;
                }
            }
            let shown = real.unwrap_or(original);
            let message = if st.dev && !allowed.dev {
                format!(
                    "'{shown}' should be listed in the project's dependencies, not devDependencies."
                )
            } else if st.optional && !allowed.optional {
                format!(
                    "'{shown}' should be listed in the project's dependencies, not optionalDependencies."
                )
            } else {
                format!(
                    "'{shown}' should be listed in the project's dependencies. Run 'npm i -S {shown}' to add it"
                )
            };
            cx.report_node(r.importer, message);
        }
    }
}

/// The message V8's `JSON.parse` throws for `text` (which `serde_json` rejected), for
/// the "could not be parsed" report. Positions are UTF-16 offsets, with a 1-based line
/// and column.
fn v8_json_error(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut p = JsonScan {
        chars: &chars,
        i: 0,
        text,
    };
    match p.document() {
        Err(e) => e,
        Ok(()) => "Unexpected end of JSON input".to_owned(),
    }
}

struct JsonScan<'a> {
    chars: &'a [char],
    i: usize,
    text: &'a str,
}

/// The deepest nesting checked (deeper input is reported where it stops).
const JSON_MAX_DEPTH: usize = 512;

impl JsonScan<'_> {
    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }
    /// " in JSON at position N (line L column C)" for the current character.
    fn at(&self, what: &str) -> String {
        let mut pos = 0usize;
        let mut line = 1usize;
        let mut col = 1usize;
        for &c in &self.chars[..self.i.min(self.chars.len())] {
            pos += c.len_utf16();
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += c.len_utf16();
            }
        }
        format!("{what} in JSON at position {pos} (line {line} column {col})")
    }
    fn unexpected(&self) -> String {
        match self.peek() {
            None => "Unexpected end of JSON input".to_owned(),
            Some(c) if c.is_ascii_digit() || c == '-' => self.at("Unexpected number"),
            Some('"') => self.at("Unexpected string"),
            Some(c) => {
                let total = self.text.chars().count();
                if total <= 40 {
                    format!(
                        "Unexpected token '{c}', \"{}\" is not valid JSON",
                        self.text
                    )
                } else {
                    let from = self.i.saturating_sub(10);
                    let to = (self.i + 10).min(self.chars.len());
                    let snippet: String = self.chars[from..to].iter().collect();
                    let pre = if from > 0 { "..." } else { "" };
                    let post = if to < self.chars.len() { "..." } else { "" };
                    format!("Unexpected token '{c}', {pre}\"{snippet}\"{post} is not valid JSON")
                }
            }
        }
    }
    fn document(&mut self) -> Result<(), String> {
        self.ws();
        self.value(0)?;
        self.ws();
        if self.peek().is_some() {
            return Err(self.at("Unexpected non-whitespace character after JSON"));
        }
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Result<(), String> {
        if depth > JSON_MAX_DEPTH {
            return Err(self.unexpected());
        }
        match self.peek() {
            Some('{') => self.object(depth),
            Some('[') => self.array(depth),
            Some('"') => self.string(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) => {
                let word = match c {
                    't' => "true",
                    'f' => "false",
                    'n' => "null",
                    _ => return Err(self.unexpected()),
                };
                // V8 reports the first character that breaks the literal.
                for w in word.chars() {
                    if self.peek() != Some(w) {
                        return Err(self.unexpected());
                    }
                    self.i += 1;
                }
                Ok(())
            }
            None => Err(self.unexpected()),
        }
    }
    fn object(&mut self, depth: usize) -> Result<(), String> {
        self.i += 1;
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(());
        }
        let mut first = true;
        loop {
            if self.peek() != Some('"') {
                return Err(match self.peek() {
                    None => self.unexpected(),
                    Some(_) if first => self.at("Expected property name or '}'"),
                    Some(_) => self.at("Expected double-quoted property name"),
                });
            }
            first = false;
            self.string()?;
            self.ws();
            if self.peek() != Some(':') {
                return Err(match self.peek() {
                    None => self.unexpected(),
                    Some(_) => self.at("Expected ':' after property name"),
                });
            }
            self.i += 1;
            self.ws();
            self.value(depth + 1)?;
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                    self.ws();
                }
                Some('}') => {
                    self.i += 1;
                    return Ok(());
                }
                None => return Err(self.unexpected()),
                Some(_) => return Err(self.at("Expected ',' or '}' after property value")),
            }
        }
    }
    fn array(&mut self, depth: usize) -> Result<(), String> {
        self.i += 1;
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.value(depth + 1)?;
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                    self.ws();
                }
                Some(']') => {
                    self.i += 1;
                    return Ok(());
                }
                None => return Err(self.unexpected()),
                Some(_) => return Err(self.at("Expected ',' or ']' after array element")),
            }
        }
    }
    fn string(&mut self) -> Result<(), String> {
        self.i += 1;
        loop {
            match self.peek() {
                None => return Err(self.at("Unterminated string")),
                Some('"') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => self.i += 1,
                        Some('u') => {
                            self.i += 1;
                            for _ in 0..4 {
                                if !self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                                    return Err(self.at("Bad Unicode escape"));
                                }
                                self.i += 1;
                            }
                        }
                        None => return Err(self.at("Unterminated string")),
                        Some(_) => return Err(self.at("Bad escaped character")),
                    }
                }
                Some(c) if (c as u32) < 0x20 => {
                    return Err(self.at("Bad control character in string literal"));
                }
                Some(_) => self.i += 1,
            }
        }
    }
    fn number(&mut self) -> Result<(), String> {
        if self.peek() == Some('-') {
            self.i += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.at("No number after minus sign"));
            }
        }
        if self.peek() == Some('0') {
            self.i += 1;
        } else {
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if self.peek() == Some('.') {
            self.i += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.at("Unterminated fractional number"));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.at("Exponent part is missing a number"));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::v8_json_error;

    #[test]
    fn v8_messages() {
        assert_eq!(
            v8_json_error("{ {\"a\": 1}"),
            "Expected property name or '}' in JSON at position 2 (line 1 column 3)"
        );
        assert_eq!(
            v8_json_error("{\"a\":1,}"),
            "Expected double-quoted property name in JSON at position 7 (line 1 column 8)"
        );
        assert_eq!(v8_json_error("{\"a\":"), "Unexpected end of JSON input");
        assert_eq!(
            v8_json_error("{\"a\": tru}"),
            "Unexpected token '}', \"{\"a\": tru}\" is not valid JSON"
        );
        assert_eq!(
            v8_json_error("[1 2]"),
            "Expected ',' or ']' after array element in JSON at position 3 (line 1 column 4)"
        );
        // Deep nesting stops at the depth limit instead of recursing further.
        let deep = "[".repeat(100_000);
        assert!(!v8_json_error(&deep).is_empty());
    }
}
