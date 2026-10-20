//! Sound mode in the editor (design record 0007, "Reporting", `ts-r-lsp`).
//!
//! For each open file this project checks, the file's level comes from its `// @ts-r` pragma,
//! else the project's `[sound]` configuration (`ts-r.toml`, `tsrconfig.json` or the tsconfig
//! `"ts-r"` key, found by `ts_r_sound::config::discover`). Files at `strict` or above get a
//! sound pass after the regular check (`ts_r_sound::check_sound_files`, that file only); its
//! `TSR` diagnostics are published with the type diagnostics, with `source: "ts-r sound"`, the
//! `TSR0201` code (or the numeric `100201` under `code-prefix = "TS"`) and a link to the rule's
//! page. Quick fixes use the project's `[sound.guard]` options (its refinements).
//!
//! The forced-flag checkers are layered like the session's own checker: each forced program
//! (same files, forced options) is ordered by `ts_r_history::arrange` with the same hot files,
//! so its lib, dependency and stable levels are built once per option set and then found in
//! the registry; a keystroke re-checks only the hot work. The levels are kept here between
//! passes so the registry (weak references) keeps finding them.
//!
//! Quick fixes (`textDocument/codeAction`, from the request's `context.diagnostics`): add
//! `// @ts-r-unsafe S-xx: ` above the line (every rule), remove an unused hatch (TSR0002),
//! `x as T` / `const v: T = source` to `parse<T>(...)` when `T` is validatable (TSR0201,
//! TSR1401), `x!` to `expect(x, "...")` (TSR0301), `await` or `.catch(console.error)` for
//! floating promises (TSR1001, TSR1002), and the missing `@throws` tags (TSR1302).

use super::Project;
use crate::text::Pos;
use crate::tsr::INFERRED;
use crate::tsr::convert::LspLines;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;
use ts_r_ast::{Ast, FnFlags, Node, NodeId, UnaryOp};
use ts_r_checker::{Layer, ProgramFiles};
use ts_r_config::{FileSystem, path};
use ts_r_diagnostics::{Diagnostic, FileId, SOUND_CODE_BASE};
use ts_r_sound::config::{self, Level, SoundConfig};
use ts_r_sound::{SoundRun, check_sound_files, codes, file_level};

/// Where rule pages live: `<base>rules/S-02/` (the site's sound-mode section; the general
/// `TSR00xx` codes link to `<base>`).
pub const DOCS_BASE: &str = "https://webb-ventures.github.io/tsc-rust/sound/";

/// The `source` of sound diagnostics.
pub const SOURCE: &str = "ts-r sound";

/// Sound-mode state of one project.
#[derive(Debug, Default)]
pub struct SoundState {
    /// The project's configuration (`None`: not read yet).
    config: Option<Arc<SoundConfig>>,
    /// Forced-program levels of the last pass (kept so the registry finds them again).
    keep: Vec<Arc<Layer>>,
    /// Sound passes run, and the last one's time.
    pub passes: u64,
    pub last_ms: f64,
}

impl SoundState {
    /// Forgets the configuration (a config file changed).
    pub fn invalidate(&mut self) {
        self.config = None;
    }
}

/// The rule page of a sound code number (`201` -> `.../rules/S-02/`).
pub fn docs_url(n: u32) -> String {
    match codes::rule_of(n) {
        0 => DOCS_BASE.to_owned(),
        r => format!("{DOCS_BASE}rules/S-{r:02}/"),
    }
}

impl Project {
    /// The project's sound configuration (discovered once per config change) and whether
    /// paths compare case-sensitively.
    fn sound_config(&mut self) -> (Arc<SoundConfig>, bool) {
        let fs = super::OverlayFs::new(&self.overlay);
        let case_sensitive = fs.case_sensitive();
        if let Some(c) = &self.sound.config {
            return (c.clone(), case_sensitive);
        }
        let root = self.shared.root.to_string_lossy().replace('\\', "/");
        let tsconfig = if self.name == INFERRED {
            path::combine(&root, "tsconfig.json")
        } else if self.name.starts_with('/') || self.name.contains(':') {
            self.name.clone()
        } else {
            path::combine(&root, &self.name)
        };
        let (cfg, errors) = config::discover(&fs, &tsconfig);
        for e in &errors {
            crate::daemon::log(&format!(
                "ts-r {}: sound-mode configuration {}: {}",
                self.name, e.file, e.message
            ));
        }
        let cfg = Arc::new(cfg.unwrap_or_else(|| SoundConfig {
            base_dir: path::directory_of(&tsconfig).to_owned(),
            ..SoundConfig::default()
        }));
        self.sound.config = Some(cfg.clone());
        (cfg, case_sensitive)
    }

    /// Requests sound mode answers on the project thread: `textDocument/codeAction` (the
    /// sound quick fixes) and `ts-r/soundLevel` (`{textDocument}` -> `{level, config,
    /// passes, lastMs}`: the file's level for the status bar). `None` for other methods.
    pub(super) fn sound_request(
        &mut self,
        program: &ProgramFiles,
        file: FileId,
        method: &str,
        params: &Value,
    ) -> Option<Value> {
        match method {
            "textDocument/codeAction" => Some(self.sound_code_actions(program, file, params)),
            "ts-r/soundLevel" => {
                let (cfg, case_sensitive) = self.sound_config();
                let level = file_level(program, file.0 as usize, &cfg, case_sensitive);
                Some(json!({
                    "level": level.as_str(),
                    "config": (!cfg.source.is_empty()).then(|| cfg.source.clone()),
                    "passes": self.sound.passes,
                    "lastMs": self.sound.last_ms,
                }))
            }
            _ => None,
        }
    }

    /// Sound diagnostics of `file` (only pragma problems when the file is `off`). `semantic`
    /// are its diagnostics from the regular check.
    pub(super) fn sound_check(
        &mut self,
        program: &ProgramFiles,
        file: FileId,
        semantic: &[Diagnostic],
    ) -> Vec<Diagnostic> {
        let (cfg, case_sensitive) = self.sound_config();
        if file_level(program, file.0 as usize, &cfg, case_sensitive) == Level::Off {
            // No pass, but a misspelt opt-in (`// @ts-r soud`) or a late pragma is reported.
            return ts_r_sound::pragma_diagnostics(program, file.0 as usize);
        }
        let t = Instant::now();
        let hot: Vec<usize> = self
            .hot
            .iter()
            .filter_map(|p| self.index.get(p).copied())
            .collect();
        let kept = std::cell::RefCell::new(Vec::new());
        let reordered = std::cell::Cell::new(false);
        let prepare = |p: &mut ProgramFiles| -> Option<Arc<Layer>> {
            let a = ts_r_history::arrange(p, &hot);
            if !a.same_order() {
                // File ids are shared with the regular check: put the files back.
                let moved = p.files.clone();
                for (old, &new) in a.perm.iter().enumerate() {
                    if let Some(f) = moved.get(new as usize) {
                        p.files[old] = f.clone();
                    }
                }
                reordered.set(true);
                return None;
            }
            if let Some(l) = &a.parent {
                kept.borrow_mut().push(l.clone());
            }
            a.parent
        };
        let run = SoundRun {
            program,
            config: &cfg,
            base: semantic,
            threads: 1,
            case_sensitive,
        };
        let result = check_sound_files(&run, &[file.0 as usize], Some(&prepare));
        self.sound.keep = kept.into_inner();
        self.sound.passes += 1;
        self.sound.last_ms = t.elapsed().as_secs_f64() * 1000.0;
        if reordered.get() {
            crate::daemon::log(&format!(
                "ts-r {}: a forced program ordered differently; sound pass without levels",
                self.name
            ));
        }
        if self.sound.last_ms > 50.0 {
            crate::daemon::log(&format!(
                "ts-r {}: sound pass on {} in {:.1} ms",
                self.name, program.files[file.0 as usize].path, self.sound.last_ms
            ));
        }
        result
            .diagnostics
            .into_iter()
            .filter(|d| d.file == file)
            .collect()
    }

    /// Gives published sound diagnostics their `source`, code label and rule link.
    pub(super) fn sound_decorate(&self, diags: &mut [Value]) {
        let numeric = self
            .sound
            .config
            .as_ref()
            .is_some_and(|c| c.policy.numeric_codes);
        for d in diags {
            let Some(code) = d["code"].as_u64() else {
                continue;
            };
            let Some(n) = u32::try_from(code)
                .ok()
                .and_then(|c| c.checked_sub(SOUND_CODE_BASE))
            else {
                continue;
            };
            d["source"] = json!(SOURCE);
            if !numeric {
                d["code"] = json!(format!("TSR{n:04}"));
            }
            d["codeDescription"] = json!({ "href": docs_url(n) });
        }
    }

    /// Quick fixes for the sound diagnostics in a `textDocument/codeAction` request.
    pub(super) fn sound_code_actions(
        &self,
        program: &ProgramFiles,
        file: FileId,
        params: &Value,
    ) -> Value {
        let Some(f) = program.files.get(file.0 as usize) else {
            return json!([]);
        };
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let lines = LspLines::new(&f.text);
        let fx = Fixer {
            program,
            file: file.0 as usize,
            ast: &f.ast,
            text: &f.text,
            lines: &lines,
            // The project's `[sound.guard]` (refinements decide which branded types a
            // `parse<T>` fix can offer); defaults before the configuration was read.
            guard: self
                .sound
                .config
                .as_ref()
                .map(|c| c.guard.options())
                .unwrap_or_default(),
        };
        let mut out = Vec::new();
        for d in params["context"]["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if d["source"] != SOURCE {
                continue;
            }
            let Some(n) = sound_number(&d["code"]) else {
                continue;
            };
            let (Some(s), Some(e)) = (
                Pos::from_lsp(&d["range"]["start"]),
                Pos::from_lsp(&d["range"]["end"]),
            ) else {
                continue;
            };
            let span = (lines.offset(&f.text, s), lines.offset(&f.text, e));
            let message = d["message"].as_str().unwrap_or_default();
            for (title, edits, preferred) in fx.fixes(n, span, message) {
                out.push(json!({
                    "title": title,
                    "kind": "quickfix",
                    "diagnostics": [d],
                    "isPreferred": preferred,
                    "edit": {"changes": {uri: edits}},
                }));
            }
        }
        Value::Array(out)
    }
}

/// `TSR0201`, `"TS100201"` or `100201` to `201`.
fn sound_number(code: &Value) -> Option<u32> {
    if let Some(c) = code.as_u64() {
        return u32::try_from(c).ok()?.checked_sub(SOUND_CODE_BASE);
    }
    let s = code.as_str()?;
    if let Some(n) = s.strip_prefix("TSR") {
        return n.parse().ok();
    }
    s.strip_prefix("TS")?
        .parse::<u32>()
        .ok()?
        .checked_sub(SOUND_CODE_BASE)
}

/// One fix: title, LSP text edits, preferred.
type Fix = (String, Vec<Value>, bool);

struct Fixer<'a> {
    program: &'a ProgramFiles,
    file: usize,
    ast: &'a Ast,
    text: &'a str,
    lines: &'a LspLines,
    guard: ts_r_guard::GuardOptions,
}

impl Fixer<'_> {
    fn edit(&self, start: u32, end: u32, new_text: &str) -> Value {
        json!({"range": self.lines.range(self.text, start, end), "newText": new_text})
    }

    fn src(&self, n: NodeId) -> &str {
        let s = self.ast.span(n);
        self.text
            .get(s.start as usize..s.end as usize)
            .unwrap_or("")
    }

    fn line_start(&self, at: u32) -> u32 {
        self.text[..(at as usize).min(self.text.len())]
            .rfind('\n')
            .map_or(0, |i| i as u32 + 1)
    }

    fn indent(&self, line_start: u32) -> &str {
        let rest = &self.text[line_start as usize..];
        let n = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        &rest[..n]
    }

    /// The node whose span is exactly `span`, else the deepest one containing it.
    fn node_at(&self, (start, end): (u32, u32)) -> Option<NodeId> {
        let mut n = crate::tsr::features::locate::node_at(self.ast, start)?;
        let parents = self.program.files[self.file].parents();
        let mut best = n;
        for _ in 0..64 {
            let s = self.ast.span(n);
            if s.start == start && s.end == end {
                return Some(n);
            }
            if s.start <= start && s.end >= end {
                best = n;
                break;
            }
            n = parents.parent(n)?;
        }
        // Climb further while the span is still the same node (wrappers).
        let mut cur = best;
        for _ in 0..8 {
            let Some(p) = parents.parent(cur) else { break };
            let s = self.ast.span(p);
            if s.start == start && s.end == end {
                return Some(p);
            }
            if s.start != self.ast.span(cur).start || s.end > end {
                break;
            }
            cur = p;
        }
        Some(best)
    }

    fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.program.files[self.file].parents().parent(n)
    }

    fn fixes(&self, n: u32, span: (u32, u32), message: &str) -> Vec<Fix> {
        let mut out: Vec<Fix> = Vec::new();
        match n {
            201 | 1401 => out.extend(self.parse_fix(span)),
            301 => out.extend(self.expect_fix(span)),
            1001 | 1002 => out.extend(self.promise_fix(n, span)),
            1302 => out.extend(self.throws_fix(span, message)),
            2 => {
                let ls = self.line_start(span.0);
                let le = self.text[span.1 as usize..]
                    .find('\n')
                    .map_or(self.text.len() as u32, |i| span.1 + i as u32 + 1);
                out.push((
                    "Remove the unused '@ts-r-unsafe' directive".into(),
                    vec![self.edit(ls, le, "")],
                    true,
                ));
            }
            _ => {}
        }
        let rule = codes::rule_of(n);
        if (1..=14).contains(&rule) {
            let ls = self.line_start(span.0);
            let indent = self.indent(ls).to_owned();
            out.push((
                format!("Add '@ts-r-unsafe S-{rule:02}' with a reason"),
                vec![self.edit(ls, ls, &format!("{indent}// @ts-r-unsafe S-{rule:02}: \n"))],
                false,
            ));
        }
        out
    }

    /// The local name of `name` imported from `@ts-r.dev/guard`, or the edit that imports it.
    fn guard_import(&self, name: &str) -> (String, Option<Value>) {
        let Node::Program { stmts } = *self.ast.node(self.ast.root) else {
            return (name.to_owned(), None);
        };
        let stmts = self.ast.list(stmts);
        for &s in stmts {
            let Node::ImportDecl {
                type_only: false,
                specifiers,
                source,
                ..
            } = *self.ast.node(s)
            else {
                continue;
            };
            let Node::StringLit { value } = *self.ast.node(source) else {
                continue;
            };
            if self.ast.atom(value) != "@ts-r.dev/guard" {
                continue;
            }
            let specs = self.ast.list(specifiers);
            let mut last_named = None;
            for &sp in specs {
                match *self.ast.node(sp) {
                    Node::ImportSpecifier {
                        type_only: false,
                        imported,
                        local,
                    } => {
                        let imported_name = self.src(imported.unwrap_or(local));
                        if imported_name == name {
                            return (self.src(local).to_owned(), None);
                        }
                        last_named = Some(sp);
                    }
                    Node::ImportNamespaceSpecifier { local } => {
                        return (format!("{}.{name}", self.src(local)), None);
                    }
                    _ => {}
                }
            }
            if let Some(sp) = last_named {
                let end = self.ast.span(sp).end;
                return (
                    name.to_owned(),
                    Some(self.edit(end, end, &format!(", {name}"))),
                );
            }
        }
        // A new import before the first statement (after leading comments, so a `@ts-r`
        // pragma stays first).
        let at = stmts
            .first()
            .map_or(self.text.len() as u32, |&s| self.ast.span(s).start);
        let at = self.line_start(at);
        (
            name.to_owned(),
            Some(self.edit(
                at,
                at,
                &format!("import {{ {name} }} from \"@ts-r.dev/guard\";\n"),
            )),
        )
    }

    fn validatable(&self, ty: NodeId) -> bool {
        let mut g = ts_r_guard::Generator::new(self.program, self.guard.clone());
        g.add(self.file, ty).is_ok()
    }

    /// `x as T` / `<T>x` / `const v: T = source` to `parse<T>(...)`.
    fn parse_fix(&self, span: (u32, u32)) -> Option<Fix> {
        let mut n = self.node_at(span)?;
        for _ in 0..6 {
            match *self.ast.node(n) {
                Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                    if !self.validatable(ty) {
                        return None;
                    }
                    let (parse, import) = self.guard_import("parse");
                    let t = self.src(ty).to_owned();
                    let s = self.ast.span(n);
                    let mut edits = vec![self.edit(
                        s.start,
                        s.end,
                        &format!("{parse}<{t}>({})", self.src(expr)),
                    )];
                    edits.extend(import);
                    return Some((format!("Validate with 'parse<{t}>'"), edits, true));
                }
                Node::VarDeclarator {
                    name,
                    type_ann: Some(ty),
                    init: Some(init),
                    ..
                } => {
                    if !self.validatable(ty) {
                        return None;
                    }
                    let (parse, import) = self.guard_import("parse");
                    let t = self.src(ty).to_owned();
                    let i = self.ast.span(init);
                    let mut edits = vec![
                        self.edit(self.ast.span(name).end, self.ast.span(ty).end, ""),
                        self.edit(i.start, i.end, &format!("{parse}<{t}>({})", self.src(init))),
                    ];
                    edits.extend(import);
                    return Some((format!("Validate with 'parse<{t}>'"), edits, true));
                }
                Node::ExprStmt { .. }
                | Node::Block { .. }
                | Node::Program { .. }
                | Node::FunctionDecl { .. }
                | Node::Arrow { .. }
                | Node::FunctionExpr { .. } => return None,
                _ => {}
            }
            n = self.parent(n)?;
        }
        None
    }

    /// `x!` to `expect(x, "x is set")`.
    fn expect_fix(&self, span: (u32, u32)) -> Option<Fix> {
        let n = self.node_at(span)?;
        let Node::NonNull { expr } = *self.ast.node(n) else {
            return None;
        };
        let x = self.src(expr).to_owned();
        let shown: String = x
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let (expect, import) = self.guard_import("expect");
        let s = self.ast.span(n);
        let mut edits = vec![self.edit(
            s.start,
            s.end,
            &format!("{expect}({x}, \"{shown} is set\")"),
        )];
        edits.extend(import);
        Some((format!("Replace '!' with '{expect}(...)'"), edits, true))
    }

    /// Whether the function enclosing `n` is `async`.
    fn in_async_function(&self, n: NodeId) -> bool {
        let mut cur = n;
        for _ in 0..256 {
            let Some(p) = self.parent(cur) else {
                return false;
            };
            match *self.ast.node(p) {
                Node::FunctionDecl { flags, .. }
                | Node::FunctionExpr { flags, .. }
                | Node::Arrow { flags, .. } => return flags.contains(FnFlags::ASYNC),
                _ => cur = p,
            }
        }
        false
    }

    fn with_catch(&self, e: NodeId) -> String {
        let x = self.src(e);
        match *self.ast.node(e) {
            Node::Call { .. } | Node::Member { .. } | Node::Ident { .. } | Node::Paren { .. } => {
                format!("{x}.catch(console.error)")
            }
            _ => format!("({x}).catch(console.error)"),
        }
    }

    /// TSR1001: `await` in async functions, else `void x.catch(console.error)`; TSR1002:
    /// attach `.catch(console.error)` under the `void`.
    fn promise_fix(&self, n: u32, span: (u32, u32)) -> Vec<Fix> {
        let Some(node) = self.node_at(span) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if n == 1002 {
            if let Node::Unary {
                op: UnaryOp::Void,
                arg,
            } = *self.ast.node(node)
            {
                let a = self.ast.span(arg);
                out.push((
                    "Attach '.catch(console.error)'".into(),
                    vec![self.edit(a.start, a.end, &self.with_catch(arg))],
                    true,
                ));
            }
            return out;
        }
        let expr = match *self.ast.node(node) {
            Node::ExprStmt { expr } => expr,
            _ => node,
        };
        let e = self.ast.span(expr);
        if self.in_async_function(expr) {
            out.push((
                "Add 'await'".into(),
                vec![self.edit(e.start, e.start, "await ")],
                true,
            ));
        }
        out.push((
            "Discard with 'void' after '.catch(console.error)'".into(),
            vec![self.edit(e.start, e.end, &format!("void {}", self.with_catch(expr)))],
            out.is_empty(),
        ));
        out
    }

    /// TSR1302: the `@throws` tags the message lists (`can throw 'A | B'`), in the
    /// declaration's JSDoc (a new one when it has none).
    fn throws_fix(&self, span: (u32, u32), message: &str) -> Option<Fix> {
        let rest = &message[message.find("can throw '")? + "can throw '".len()..];
        let types: Vec<&str> = rest[..rest.find('\'')?]
            .split(" | ")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        if types.is_empty() {
            return None;
        }
        let mut decl = self.node_at(span)?;
        for _ in 0..8 {
            match *self.ast.node(decl) {
                Node::FunctionDecl { .. } | Node::VarDecl { .. } | Node::MethodDef { .. } => break,
                _ => decl = self.parent(decl)?,
            }
        }
        // `export` / `export default` come before the declaration on its line.
        let ls = self.line_start(self.ast.span(decl).start);
        let indent = self.indent(ls).to_owned();
        let tags: Vec<String> = types.iter().map(|t| format!("@throws {{{t}}}")).collect();
        let title = format!("Add {}", tags.join(", "));
        let before = self.text[..ls as usize].trim_end();
        let jsdoc = before.ends_with("*/")
            && before
                .rfind("/**")
                .is_some_and(|open| !before[open..].contains("\n\n"));
        let edit = if jsdoc {
            let close = before.len() as u32 - 2;
            let close_ls = self.line_start(close);
            if self.text[close_ls as usize..close as usize]
                .trim()
                .is_empty()
            {
                // `*/` on its own line: the tags go on lines before it.
                let body: String = tags.iter().map(|t| format!("{indent} * {t}\n")).collect();
                self.edit(close_ls, close_ls, &body)
            } else {
                let body: String = tags.iter().map(|t| format!("\n{indent} * {t}")).collect();
                let at = self.text[..close as usize].trim_end().len() as u32;
                self.edit(at, close, &format!("{body}\n{indent} "))
            }
        } else {
            let body: String = tags.iter().map(|t| format!("{indent} * {t}\n")).collect();
            self.edit(ls, ls, &format!("{indent}/**\n{body}{indent} */\n"))
        };
        Some((title, vec![edit], true))
    }
}
