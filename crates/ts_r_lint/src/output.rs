//! Output formats: `stylish` (default), `json` (ESLint-shaped), `sarif` and `github`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use ts_r_span::{LineIndex, Span};

use crate::diagnostic::{Diagnostic, Severity};
use crate::engine::FileResult;
use crate::registry::Registry;

/// The lint result of one file, ready for printing.
#[derive(Debug)]
pub struct FileReport {
    /// Absolute path.
    pub path: PathBuf,
    /// The text the diagnostics refer to (after fixes).
    pub source: String,
    pub result: FileResult,
}

/// Output format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Stylish,
    Json,
    Sarif,
    Github,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s {
            "stylish" => Format::Stylish,
            "json" => Format::Json,
            "sarif" => Format::Sarif,
            "github" => Format::Github,
            _ => return None,
        })
    }
}

/// ESLint-style counts for one file or a whole run.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Counts {
    pub errors: usize,
    pub fatal: usize,
    pub warnings: usize,
    pub fixable_errors: usize,
    pub fixable_warnings: usize,
}

impl Counts {
    pub fn of(diags: &[Diagnostic]) -> Counts {
        let mut c = Counts::default();
        for d in diags {
            if d.is_error() {
                c.errors += 1;
                if d.fatal {
                    c.fatal += 1;
                }
                if d.fix.is_some() {
                    c.fixable_errors += 1;
                }
            } else if d.severity == Severity::Warn {
                c.warnings += 1;
                if d.fix.is_some() {
                    c.fixable_warnings += 1;
                }
            }
        }
        c
    }
    pub fn add(&mut self, o: Counts) {
        self.errors += o.errors;
        self.fatal += o.fatal;
        self.warnings += o.warnings;
        self.fixable_errors += o.fixable_errors;
        self.fixable_warnings += o.fixable_warnings;
    }
}

/// 1-based line and UTF-16 column for byte offsets, plus UTF-16 string indices (ESLint's
/// `range`).
#[derive(Debug)]
pub struct Positions<'a> {
    src: &'a str,
    lines: LineIndex,
    ascii: bool,
}

impl<'a> Positions<'a> {
    pub fn new(src: &'a str) -> Positions<'a> {
        Positions {
            src,
            lines: LineIndex::new(src),
            ascii: src.is_ascii(),
        }
    }
    /// A byte offset inside a character: the character's start, and 1 when the offset is
    /// inside a 4-byte (astral) character. Rules use `start + 2` of an astral character for
    /// the position between its two UTF-16 surrogates, as ESLint reports regex code units.
    fn split(&self, pos: u32) -> (u32, u32) {
        let mut p = (pos as usize).min(self.src.len());
        if self.ascii || self.src.is_char_boundary(p) {
            return (p as u32, 0);
        }
        while !self.src.is_char_boundary(p) {
            p -= 1;
        }
        let astral = self.src[p..]
            .chars()
            .next()
            .is_some_and(|c| c.len_utf16() == 2);
        (p as u32, u32::from(astral))
    }
    /// `(line, column)`, both 1-based, column in UTF-16 units.
    pub fn line_col(&self, pos: u32) -> (u32, u32) {
        let (pos, extra) = self.split(pos);
        let lc = self.lines.line_col_utf16(self.src, pos);
        (lc.line + 1, lc.col + 1 + extra)
    }
    /// Where `d` starts, as ESLint prints it: its explicit [`Loc`](crate::diagnostic::Loc)
    /// if it has one, else its span's start.
    pub fn start_of(&self, d: &Diagnostic) -> (u32, u32) {
        match d.loc {
            Some(l) => (l.line, l.column),
            None => self.line_col(d.span.start),
        }
    }
    /// Where `d` ends (its explicit location's end, else its span's end).
    pub fn end_of(&self, d: &Diagnostic) -> (u32, u32) {
        match d.loc {
            Some(l) => (l.end_line, l.end_column),
            None => self.line_col(d.span.end.max(d.span.start)),
        }
    }
    /// The UTF-16 index of a byte offset.
    pub fn utf16(&self, pos: u32) -> u32 {
        if self.ascii {
            return pos;
        }
        let (pos, extra) = self.split(pos);
        self.utf16_at_boundary(pos) + extra
    }
    fn utf16_at_boundary(&self, pos: u32) -> u32 {
        let end = (pos as usize).min(self.src.len());
        let mut n = 0u32;
        for (i, c) in self.src.char_indices() {
            if i >= end {
                break;
            }
            n += c.len_utf16() as u32;
        }
        n
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Renders reports in `format`. `cwd` makes SARIF/github paths relative.
pub fn render(format: Format, reports: &[FileReport], cwd: &Path, registry: &Registry) -> String {
    match format {
        Format::Stylish => stylish(reports),
        Format::Json => {
            let v: Vec<Value> = reports.iter().map(json_result).collect();
            serde_json::to_string(&v).unwrap_or_default()
        }
        Format::Sarif => sarif(reports, cwd, registry),
        Format::Github => github(reports, cwd),
    }
}

/// ESLint's `stylish` format (without colours).
pub fn stylish(reports: &[FileReport]) -> String {
    let mut out = String::new();
    let mut total = Counts::default();
    for r in reports {
        let msgs = &r.result.diagnostics;
        if msgs.is_empty() {
            continue;
        }
        total.add(Counts::of(msgs));
        let pos = Positions::new(&r.source);
        out.push('\n');
        out.push_str(&r.path.to_string_lossy());
        out.push('\n');
        let rows: Vec<[String; 5]> = msgs
            .iter()
            .map(|d| {
                let (l, c) = if d.rule.is_none() && r.source.is_empty() {
                    (0, 0)
                } else {
                    pos.start_of(d)
                };
                [
                    l.to_string(),
                    c.to_string(),
                    if d.is_error() { "error" } else { "warning" }.to_owned(),
                    strip_period(&d.message).to_owned(),
                    d.rule.unwrap_or("").to_owned(),
                ]
            })
            .collect();
        let mut w = [0usize; 5];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                w[i] = w[i].max(cell.chars().count());
            }
        }
        for row in &rows {
            // text-table: "" | line (right) | column (left) | severity | message | rule, two
            // spaces between columns; then "line  col" collapses to "line:col".
            let mut line = String::from("  ");
            let _ = write!(line, "{:>width$}", row[0], width = w[0]);
            line.push(':');
            line.push_str(&row[1]);
            let pad = w[1] - row[1].chars().count();
            line.push_str(&" ".repeat(pad + 2));
            for i in 2..5 {
                line.push_str(&row[i]);
                if i < 4 {
                    let pad = w[i] - row[i].chars().count();
                    line.push_str(&" ".repeat(pad + 2));
                }
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    let n = total.errors + total.warnings;
    if n > 0 {
        let _ = writeln!(
            out,
            "\n\u{2716} {} ({}, {})",
            plural(n, "problem"),
            plural(total.errors, "error"),
            plural(total.warnings, "warning")
        );
        if total.fixable_errors + total.fixable_warnings > 0 {
            let _ = writeln!(
                out,
                "  {} and {} potentially fixable with the `--fix` option.",
                plural(total.fixable_errors, "error"),
                plural(total.fixable_warnings, "warning")
            );
        }
        out.push('\n');
    }
    out
}

fn strip_period(m: &str) -> &str {
    match m.strip_suffix('.') {
        Some(s) if !s.ends_with(' ') && !s.is_empty() => s,
        _ => m,
    }
}

fn severity_num(d: &Diagnostic) -> u8 {
    match d.severity {
        Severity::Error => 2,
        Severity::Warn => 1,
        Severity::Off => 0,
    }
}

fn fix_json(pos: &Positions<'_>, src: &str, f: &crate::diagnostic::Fix) -> Value {
    let e = f.merged(src);
    json!({ "range": [pos.utf16(e.span.start), pos.utf16(e.span.end)], "text": e.text })
}

/// One ESLint message object.
pub fn message_json(pos: &Positions<'_>, src: &str, d: &Diagnostic) -> Value {
    let (line, column) = pos.start_of(d);
    let mut m = serde_json::Map::new();
    m.insert("ruleId".into(), d.rule.map_or(Value::Null, |r| r.into()));
    if d.fatal {
        m.insert("fatal".into(), true.into());
    }
    m.insert("severity".into(), severity_num(d).into());
    m.insert("message".into(), d.message.clone().into());
    // File-level messages (ignored files) have no position.
    if d.rule.is_some() || !src.is_empty() {
        m.insert("line".into(), line.into());
        m.insert("column".into(), column.into());
    }
    if let Some(id) = d.message_id {
        m.insert("messageId".into(), id.into());
    }
    if !d.fatal && d.rule.is_some() {
        let (el, ec) = pos.end_of(d);
        m.insert("endLine".into(), el.into());
        m.insert("endColumn".into(), ec.into());
    }
    if let Some(f) = &d.fix {
        m.insert("fix".into(), fix_json(pos, src, f));
    }
    if !d.suggestions.is_empty() {
        let s: Vec<Value> = d
            .suggestions
            .iter()
            .map(|s| {
                let mut o = serde_json::Map::new();
                if let Some(id) = s.message_id {
                    o.insert("messageId".into(), id.into());
                }
                o.insert("fix".into(), fix_json(pos, src, &s.fix));
                o.insert("desc".into(), s.desc.clone().into());
                Value::Object(o)
            })
            .collect();
        m.insert("suggestions".into(), s.into());
    }
    Value::Object(m)
}

/// ESLint's JSON result object for a file.
pub fn json_result(r: &FileReport) -> Value {
    let pos = Positions::new(&r.source);
    let msgs: Vec<Value> = r
        .result
        .diagnostics
        .iter()
        .map(|d| message_json(&pos, &r.source, d))
        .collect();
    let suppressed: Vec<Value> = r
        .result
        .suppressed
        .iter()
        .map(|d| {
            let mut v = message_json(&pos, &r.source, d);
            if let Value::Object(o) = &mut v {
                o.insert(
                    "suppressions".into(),
                    json!([{ "kind": "directive", "justification": "" }]),
                );
            }
            v
        })
        .collect();
    let c = Counts::of(&r.result.diagnostics);
    let mut o = serde_json::Map::new();
    o.insert(
        "filePath".into(),
        r.path.to_string_lossy().into_owned().into(),
    );
    o.insert("messages".into(), msgs.into());
    o.insert("suppressedMessages".into(), suppressed.into());
    o.insert("errorCount".into(), c.errors.into());
    o.insert("fatalErrorCount".into(), c.fatal.into());
    o.insert("warningCount".into(), c.warnings.into());
    o.insert("fixableErrorCount".into(), c.fixable_errors.into());
    o.insert("fixableWarningCount".into(), c.fixable_warnings.into());
    if let Some(out) = &r.result.output {
        o.insert("output".into(), out.clone().into());
    } else if !r.result.diagnostics.is_empty() {
        o.insert("source".into(), r.source.clone().into());
    }
    o.insert("usedDeprecatedRules".into(), json!([]));
    Value::Object(o)
}

fn rel_uri(path: &Path, cwd: &Path) -> String {
    let p = path.strip_prefix(cwd).unwrap_or(path);
    p.to_string_lossy().replace('\\', "/")
}

/// SARIF 2.1.0.
pub fn sarif(reports: &[FileReport], cwd: &Path, registry: &Registry) -> String {
    let mut rule_ids: Vec<&'static str> = Vec::new();
    let mut artifacts = Vec::new();
    let mut results = Vec::new();
    for r in reports {
        if r.result.diagnostics.is_empty() {
            continue;
        }
        let uri = rel_uri(&r.path, cwd);
        let index = artifacts.len();
        artifacts.push(json!({ "location": { "uri": uri } }));
        let pos = Positions::new(&r.source);
        for d in &r.result.diagnostics {
            let (sl, sc) = pos.start_of(d);
            let (el, ec) = pos.end_of(d);
            let mut res = json!({
                "level": if d.is_error() { "error" } else { "warning" },
                "message": { "text": d.message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": uri, "index": index },
                        "region": { "startLine": sl, "startColumn": sc, "endLine": el, "endColumn": ec }
                    }
                }]
            });
            if let Some(rule) = d.rule {
                let ri = match rule_ids.iter().position(|x| *x == rule) {
                    Some(i) => i,
                    None => {
                        rule_ids.push(rule);
                        rule_ids.len() - 1
                    }
                };
                res["ruleId"] = rule.into();
                res["ruleIndex"] = ri.into();
            }
            results.push(res);
        }
    }
    let rules: Vec<Value> = rule_ids
        .iter()
        .map(|id| match registry.find(id) {
            Some(d) => json!({
                "id": id,
                "shortDescription": { "text": d.meta.docs },
                "helpUri": d.meta.docs_url(),
            }),
            None => json!({ "id": id }),
        })
        .collect();
    let v = json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": [{
            "tool": { "driver": {
                "name": "ts-r-lint",
                "informationUri": "https://github.com/Webb-Ventures/typescript-rust",
                "rules": rules,
            }},
            "artifacts": artifacts,
            "results": results,
        }]
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

fn gh_escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn gh_escape_prop(s: &str) -> String {
    gh_escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// GitHub Actions workflow commands.
pub fn github(reports: &[FileReport], cwd: &Path) -> String {
    let mut out = String::new();
    for r in reports {
        let pos = Positions::new(&r.source);
        let file = rel_uri(&r.path, cwd);
        for d in &r.result.diagnostics {
            let (l, c) = pos.start_of(d);
            let (el, ec) = pos.end_of(d);
            let kind = if d.is_error() { "error" } else { "warning" };
            let title = d.rule.unwrap_or("ts-r-lint");
            let _ = writeln!(
                out,
                "::{kind} file={},line={l},endLine={el},col={c},endColumn={ec},title={}::{}",
                gh_escape_prop(&file),
                gh_escape_prop(title),
                gh_escape_data(&d.message)
            );
        }
    }
    out
}

/// A diagnostic that is not tied to a rule (ignored-file warnings and similar).
pub fn file_warning(message: &str) -> Diagnostic {
    Diagnostic {
        rule: None,
        severity: Severity::Warn,
        span: Span::at(0),
        message: message.to_owned(),
        message_id: None,
        fix: None,
        suggestions: Vec::new(),
        fatal: false,
        loc: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(src: &str, diags: Vec<Diagnostic>) -> FileReport {
        FileReport {
            path: PathBuf::from("/p/a.js"),
            source: src.to_owned(),
            result: FileResult {
                diagnostics: diags,
                ..FileResult::default()
            },
        }
    }

    fn diag(rule: &'static str, sev: Severity, span: Span, msg: &str) -> Diagnostic {
        Diagnostic {
            rule: Some(rule),
            severity: sev,
            span,
            message: msg.to_owned(),
            message_id: None,
            fix: None,
            suggestions: Vec::new(),
            fatal: false,
            loc: None,
        }
    }

    #[test]
    fn stylish_layout() {
        let src = "debugger;\nfoo;\n".repeat(6);
        let r = report(
            &src,
            vec![
                diag(
                    "no-debugger",
                    Severity::Error,
                    Span::new(0, 9),
                    "Unexpected 'debugger' statement.",
                ),
                diag(
                    "no-undef",
                    Severity::Warn,
                    Span::new(85, 88),
                    "'foo' is not defined.",
                ),
            ],
        );
        let s = stylish(&[r]);
        assert_eq!(
            s,
            "\n/p/a.js\n   1:1  error    Unexpected 'debugger' statement  no-debugger\n  12:1  warning  'foo' is not defined             no-undef\n\n\u{2716} 2 problems (1 error, 1 warning)\n\n"
        );
    }

    #[test]
    fn utf16_positions() {
        let p = Positions::new("a\u{1F600}b\nc");
        assert_eq!(p.line_col(5), (1, 4));
        assert_eq!(p.utf16(6), 4);
        assert_eq!(p.line_col(7), (2, 1));
        // Start + 2 of an astral character is between its surrogates.
        assert_eq!(p.line_col(3), (1, 3));
        assert_eq!(p.utf16(3), 2);
        assert_eq!(p.line_col(1), (1, 2));
    }

    #[test]
    fn github_escapes() {
        let r = report(
            "x",
            vec![diag("r", Severity::Warn, Span::new(0, 1), "a\nb%")],
        );
        let s = github(&[r], Path::new("/p"));
        assert_eq!(
            s,
            "::warning file=a.js,line=1,endLine=1,col=1,endColumn=2,title=r::a%0Ab%25\n"
        );
    }

    /// An explicit location wins over the span everywhere a position is printed, even past
    /// the end of its line (ESLint's 1:2 on an empty first line).
    #[test]
    fn explicit_location() {
        let mut d = diag("r", Severity::Error, Span::at(0), "m");
        d.loc = Some(crate::diagnostic::Loc::at(1, 2));
        let src = "\nx";
        let pos = Positions::new(src);
        let m = message_json(&pos, src, &d);
        assert_eq!((&m["line"], &m["column"]), (&json!(1), &json!(2)));
        assert_eq!((&m["endLine"], &m["endColumn"]), (&json!(1), &json!(2)));
        let r = report(src, vec![d]);
        assert!(stylish(std::slice::from_ref(&r)).contains("1:2"));
        assert!(github(&[r], Path::new("/p")).contains("line=1,endLine=1,col=2,endColumn=2"));
    }
}
