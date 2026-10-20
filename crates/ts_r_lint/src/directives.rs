//! Inline configuration comments: `eslint-disable`, `eslint-enable`,
//! `eslint-disable-line`, `eslint-disable-next-line`, `/* eslint rule: ... */`,
//! `/* global ... */` and `/* exported ... */`, plus reporting of unused disable directives.
//!
//! Behaviour is matched against ESLint 10's observed output (see the tests below).

use ts_r_ast::{Ast, CommentKind};
use ts_r_span::{LineIndex, Span};

use crate::diagnostic::{Diagnostic, Fix, Severity};
use crate::semantic::local::CommentGlobal;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DirectiveKind {
    Disable,
    Enable,
    DisableLine,
    DisableNextLine,
}

/// One rule name inside a directive, with its span in the source.
#[derive(Clone, Debug)]
pub struct RuleRef {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Directive {
    pub kind: DirectiveKind,
    /// Empty = all rules.
    pub rules: Vec<RuleRef>,
    /// The whole comment.
    pub comment: Span,
    /// Byte position the directive takes effect from (block directives) or the zero-based
    /// line it applies to (line directives).
    pub pos: u32,
    pub line: u32,
}

/// A `/* eslint rule: value */` entry.
#[derive(Clone, Debug)]
pub struct InlineRuleConfig {
    pub name: String,
    pub value: serde_json::Value,
    pub span: Span,
}

/// Everything found in a file's comments.
#[derive(Clone, Debug, Default)]
pub struct InlineConfig {
    pub directives: Vec<Directive>,
    pub rules: Vec<InlineRuleConfig>,
    pub globals: Vec<CommentGlobal>,
    pub exported: Vec<String>,
    /// Malformed directives (ESLint reports these with `ruleId: null`).
    pub problems: Vec<(Span, String)>,
}

impl InlineConfig {
    pub fn parse(ast: &Ast, src: &str, lines: &LineIndex) -> InlineConfig {
        let mut out = InlineConfig::default();
        for c in &ast.comments {
            let text = c.span.text(src);
            let (value, value_start) = match c.kind {
                CommentKind::Line => (text.get(2..).unwrap_or(""), c.span.start + 2),
                CommentKind::Block => (
                    text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
                    c.span.start + 2,
                ),
                CommentKind::Hashbang => continue,
            };
            out.parse_comment(c.kind, c.span, value, value_start, lines);
        }
        out
    }

    fn parse_comment(
        &mut self,
        kind: CommentKind,
        comment: Span,
        value: &str,
        value_start: u32,
        lines: &LineIndex,
    ) {
        let trimmed = value.trim_start();
        let lead = (value.len() - trimmed.len()) as u32;
        let word_end = trimmed
            .find(|c: char| c.is_whitespace())
            .unwrap_or(trimmed.len());
        let word = &trimmed[..word_end];
        let is_line_comment = kind == CommentKind::Line;
        let dkind = match word {
            "eslint-disable" => Some(DirectiveKind::Disable),
            "eslint-enable" => Some(DirectiveKind::Enable),
            "eslint-disable-line" => Some(DirectiveKind::DisableLine),
            "eslint-disable-next-line" => Some(DirectiveKind::DisableNextLine),
            _ => None,
        };
        if is_line_comment
            && !matches!(
                dkind,
                Some(DirectiveKind::DisableLine | DirectiveKind::DisableNextLine)
            )
        {
            return;
        }
        if !matches!(
            word,
            "eslint-disable"
                | "eslint-enable"
                | "eslint-disable-line"
                | "eslint-disable-next-line"
                | "eslint"
                | "global"
                | "globals"
                | "exported"
        ) {
            return;
        }
        // Strip a `-- description`.
        let rest_full = &trimmed[word_end..];
        let rest = strip_description(rest_full);
        let rest_start = value_start + lead + word_end as u32;
        match word {
            "eslint" => self.parse_rule_config(rest, rest_start),
            "global" | "globals" => {
                for (item, span) in split_list(rest, rest_start, &[',', ' ', '\t', '\n', '\r']) {
                    let (name, val) = match item.split_once(':') {
                        Some((n, v)) => (n.trim(), Some(v.trim())),
                        None => (item.trim(), None),
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let writable = matches!(val, Some("writable" | "writeable" | "true"));
                    let off = item.find(name).unwrap_or(0) as u32;
                    self.globals.push(CommentGlobal {
                        name: name.to_owned(),
                        writable,
                        off: val == Some("off"),
                        span: Span::new(span.start + off, span.start + off + name.len() as u32),
                    });
                }
            }
            "exported" => {
                for (item, _) in split_list(rest, rest_start, &[',', ' ', '\t', '\n', '\r']) {
                    let name = item.split(':').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        self.exported.push(name.to_owned());
                    }
                }
            }
            _ => {
                let Some(dkind) = dkind else { return };
                let start_line = lines.line_of(comment.start);
                let end_line = lines.line_of(comment.end);
                if dkind == DirectiveKind::DisableLine && start_line != end_line {
                    self.problems.push((
                        comment,
                        format!("{word} comment should not span multiple lines."),
                    ));
                    return;
                }
                let rules: Vec<RuleRef> = split_list(rest, rest_start, &[','])
                    .into_iter()
                    .filter_map(|(item, span)| {
                        let name = item.trim();
                        if name.is_empty() {
                            return None;
                        }
                        let off = item.find(name).unwrap_or(0) as u32;
                        Some(RuleRef {
                            name: name.to_owned(),
                            span: Span::new(span.start + off, span.start + off + name.len() as u32),
                        })
                    })
                    .collect();
                let line = match dkind {
                    DirectiveKind::DisableLine => start_line,
                    DirectiveKind::DisableNextLine => end_line + 1,
                    _ => start_line,
                };
                self.directives.push(Directive {
                    kind: dkind,
                    rules,
                    comment,
                    pos: comment.start,
                    line,
                });
            }
        }
    }

    fn parse_rule_config(&mut self, text: &str, start: u32) {
        // `name: value, name2: value2` where values may be arrays/objects.
        let mut depth = 0i32;
        let mut in_str: Option<char> = None;
        let mut item_start = 0usize;
        let mut items = Vec::new();
        let bytes: Vec<(usize, char)> = text.char_indices().collect();
        let mut i = 0;
        while i < bytes.len() {
            let (pos, ch) = bytes[i];
            if let Some(q) = in_str {
                if ch == '\\' {
                    i += 2;
                    continue;
                }
                if ch == q {
                    in_str = None;
                }
            } else {
                match ch {
                    '"' | '\'' => in_str = Some(ch),
                    '[' | '{' => depth += 1,
                    ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        items.push((item_start, pos));
                        item_start = pos + 1;
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        items.push((item_start, text.len()));
        for (s, e) in items {
            let item = &text[s..e];
            if item.trim().is_empty() {
                continue;
            }
            let Some((name, value)) = item.split_once(':') else {
                self.problems.push((
                    Span::new(start + s as u32, start + e as u32),
                    format!("Failed to parse JSON from '{}'", item.trim()),
                ));
                continue;
            };
            let name_trim = name.trim().trim_matches('"').trim_matches('\'');
            let v = parse_relaxed_json(value.trim());
            let off = item.find(name.trim()).unwrap_or(0);
            let span = Span::new(
                start + (s + off) as u32,
                start + (s + off + name.trim().len()) as u32,
            );
            match v {
                Some(v) => self.rules.push(InlineRuleConfig {
                    name: name_trim.to_owned(),
                    value: v,
                    span,
                }),
                None => self.problems.push((
                    span,
                    format!("Failed to parse JSON from '{}'", value.trim()),
                )),
            }
        }
    }
}

fn strip_description(s: &str) -> &str {
    // ESLint: a `--` preceded and followed by whitespace starts the description.
    let b = s.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'-' && b[i + 1] == b'-' && (i == 0 || b[i - 1].is_ascii_whitespace()) {
            let mut j = i;
            while j < b.len() && b[j] == b'-' {
                j += 1;
            }
            if j == b.len() || b[j].is_ascii_whitespace() {
                return &s[..i];
            }
        }
        i += 1;
    }
    s
}

/// Splits on any of `seps`, returning non-empty items with their spans.
fn split_list<'s>(text: &'s str, start: u32, seps: &[char]) -> Vec<(&'s str, Span)> {
    let mut out = Vec::new();
    let mut s = 0usize;
    for (i, ch) in text.char_indices() {
        if seps.contains(&ch) {
            if i > s && !text[s..i].trim().is_empty() {
                out.push((&text[s..i], Span::new(start + s as u32, start + i as u32)));
            }
            s = i + ch.len_utf8();
        }
    }
    if s < text.len() && !text[s..].trim().is_empty() {
        out.push((
            &text[s..],
            Span::new(start + s as u32, start + text.len() as u32),
        ));
    }
    // `global a: writable` splits "a:" and "writable" when ' ' is a separator; rejoin.
    let mut merged: Vec<(&'s str, Span)> = Vec::new();
    for (item, span) in out {
        if let Some(last) = merged.last_mut()
            && (last.0.trim_end().ends_with(':') || item.trim_start().starts_with(':'))
        {
            let ns = Span::new(last.1.start, span.end);
            last.0 = &text[(last.1.start - start) as usize..(span.end - start) as usize];
            last.1 = ns;
            continue;
        }
        merged.push((item, span));
    }
    merged
}

/// Parses an inline rule value: JSON, or a bare severity word / number.
pub fn parse_relaxed_json(s: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        return Some(v);
    }
    let t = s.trim();
    if !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(serde_json::Value::String(t.to_owned()));
    }
    // Single-quoted strings: convert to double quotes.
    if t.contains('\'') {
        let converted = t.replace('\'', "\"");
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&converted) {
            return Some(v);
        }
    }
    None
}

/// A problem passed through the disable directives.
#[derive(Clone, Copy, Debug)]
pub struct ProblemPos {
    pub rule: Option<&'static str>,
    pub pos: u32,
    pub line: u32,
}

/// Result of applying directives to a file's problems.
#[derive(Debug, Default)]
pub struct DirectiveOutcome {
    /// Per problem (same order as the input): suppressed or not.
    pub suppressed: Vec<bool>,
    /// Unused-directive diagnostics (empty when reporting is off).
    pub unused: Vec<Diagnostic>,
}

/// Options for unused-directive reporting.
#[derive(Clone, Copy, Debug)]
pub struct UnusedDirectiveOptions {
    pub severity: Severity,
}

/// Applies `directives` to `problems`. `known_rule(name)` says whether the rule is one ts-r-lint
/// actually ran for this file: unused reports for other rules (unsupported plugins,
/// type-aware rules) are skipped because we cannot know whether they would have fired.
/// `all_rules_known` is false when some configured rule was skipped, which makes blanket
/// `eslint-disable` directives unverifiable.
pub fn apply_directives(
    directives: &[Directive],
    problems: &[ProblemPos],
    src: &str,
    unused: Option<UnusedDirectiveOptions>,
    known_rule: &dyn Fn(&str) -> bool,
    all_rules_known: bool,
) -> DirectiveOutcome {
    // used[d][r]: rule index r of directive d suppressed something (index rules.len() = "all").
    let mut used: Vec<Vec<bool>> = directives
        .iter()
        .map(|d| vec![false; d.rules.len().max(1)])
        .collect();
    let mut suppressed = vec![false; problems.len()];

    // Block directives, in position order.
    let mut block: Vec<usize> = (0..directives.len())
        .filter(|&i| {
            matches!(
                directives[i].kind,
                DirectiveKind::Disable | DirectiveKind::Enable
            )
        })
        .collect();
    block.sort_by_key(|&i| directives[i].pos);
    let mut order: Vec<usize> = (0..problems.len()).collect();
    order.sort_by_key(|&i| problems[i].pos);

    let mut all_disabled: Option<usize> = None;
    let mut rule_disabled: Vec<(String, usize, usize)> = Vec::new(); // (rule, dir, idx)
    let mut exceptions: Vec<String> = Vec::new();
    let mut enable_used: Vec<bool> = vec![false; directives.len()];
    let mut bi = 0;
    for &pi in &order {
        let p = problems[pi];
        while bi < block.len() && directives[block[bi]].pos <= p.pos {
            let di = block[bi];
            let d = &directives[di];
            match d.kind {
                DirectiveKind::Disable => {
                    if d.rules.is_empty() {
                        all_disabled = Some(di);
                        exceptions.clear();
                    } else {
                        for (ri, r) in d.rules.iter().enumerate() {
                            rule_disabled.retain(|(n, _, _)| n != &r.name);
                            rule_disabled.push((r.name.clone(), di, ri));
                            exceptions.retain(|n| n != &r.name);
                        }
                    }
                }
                DirectiveKind::Enable => {
                    if d.rules.is_empty() {
                        if all_disabled.is_some() || !rule_disabled.is_empty() {
                            enable_used[di] = true;
                        }
                        all_disabled = None;
                        rule_disabled.clear();
                        exceptions.clear();
                    } else {
                        for r in &d.rules {
                            let before = rule_disabled.len();
                            rule_disabled.retain(|(n, _, _)| n != &r.name);
                            if rule_disabled.len() != before || all_disabled.is_some() {
                                enable_used[di] = true;
                            }
                            if all_disabled.is_some() {
                                exceptions.push(r.name.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
            bi += 1;
        }
        let Some(rule) = p.rule else { continue };
        // Line directives first (ESLint attributes the problem to the line directive).
        let mut hit = false;
        for (di, d) in directives.iter().enumerate() {
            if !matches!(
                d.kind,
                DirectiveKind::DisableLine | DirectiveKind::DisableNextLine
            ) || d.line != p.line
            {
                continue;
            }
            if d.rules.is_empty() {
                used[di][0] = true;
                hit = true;
            } else if let Some(ri) = d.rules.iter().position(|r| r.name == rule) {
                used[di][ri] = true;
                hit = true;
            }
        }
        if !hit {
            if let Some((_, di, ri)) = rule_disabled.iter().find(|(n, _, _)| n == rule) {
                used[*di][*ri] = true;
                hit = true;
            } else if let Some(di) = all_disabled
                && !exceptions.iter().any(|n| n == rule)
            {
                used[di][0] = true;
                hit = true;
            }
        }
        suppressed[pi] = hit;
    }
    // Process remaining enable directives for "unused enable" bookkeeping.
    while bi < block.len() {
        let di = block[bi];
        let d = &directives[di];
        match d.kind {
            DirectiveKind::Disable => {
                if d.rules.is_empty() {
                    all_disabled = Some(di);
                } else {
                    for (ri, r) in d.rules.iter().enumerate() {
                        rule_disabled.retain(|(n, _, _)| n != &r.name);
                        rule_disabled.push((r.name.clone(), di, ri));
                    }
                }
            }
            DirectiveKind::Enable => {
                if d.rules.is_empty() {
                    if all_disabled.is_some() || !rule_disabled.is_empty() {
                        enable_used[di] = true;
                    }
                    all_disabled = None;
                    rule_disabled.clear();
                } else {
                    for r in &d.rules {
                        let before = rule_disabled.len();
                        rule_disabled.retain(|(n, _, _)| n != &r.name);
                        if rule_disabled.len() != before || all_disabled.is_some() {
                            enable_used[di] = true;
                        }
                    }
                }
            }
            _ => {}
        }
        bi += 1;
    }

    let mut out = Vec::new();
    if let Some(opts) = unused
        && opts.severity != Severity::Off
    {
        for (di, d) in directives.iter().enumerate() {
            if d.kind == DirectiveKind::Enable {
                if enable_used[di] {
                    continue;
                }
                let unknown = d.rules.iter().any(|r| !known_rule(&r.name));
                if unknown {
                    continue;
                }
                let msg = if d.rules.is_empty() {
                    "Unused eslint-enable directive (no matching eslint-disable directives were found).".to_owned()
                } else {
                    format!(
                        "Unused eslint-enable directive (no matching eslint-disable directives were found for {}).",
                        list_names(d.rules.iter().map(|r| r.name.as_str()))
                    )
                };
                out.push(unused_diag(
                    d.comment,
                    msg,
                    opts.severity,
                    Fix::replace(d.comment, " "),
                ));
                continue;
            }
            if d.rules.is_empty() {
                if !used[di][0] && all_rules_known {
                    out.push(unused_diag(
                        d.comment,
                        "Unused eslint-disable directive (no problems were reported).".to_owned(),
                        opts.severity,
                        Fix::replace(d.comment, " "),
                    ));
                }
                continue;
            }
            let unused_rules: Vec<usize> = (0..d.rules.len())
                .filter(|&ri| !used[di][ri] && known_rule(&d.rules[ri].name))
                .collect();
            if unused_rules.is_empty() {
                continue;
            }
            let names = list_names(unused_rules.iter().map(|&ri| d.rules[ri].name.as_str()));
            let msg = format!(
                "Unused eslint-disable directive (no problems were reported from {names})."
            );
            let fix = if unused_rules.len() == d.rules.len() {
                Fix::replace(d.comment, " ")
            } else {
                remove_rules_fix(src, d, &unused_rules)
            };
            out.push(unused_diag(d.comment, msg, opts.severity, fix));
        }
    }
    DirectiveOutcome {
        suppressed,
        unused: out,
    }
}

fn unused_diag(comment: Span, message: String, severity: Severity, fix: Fix) -> Diagnostic {
    Diagnostic {
        rule: None,
        severity,
        span: Span::at(comment.start),
        message,
        message_id: None,
        fix: Some(fix),
        suggestions: Vec::new(),
        fatal: false,
    }
}

fn list_names<'n>(names: impl Iterator<Item = &'n str>) -> String {
    let v: Vec<String> = names.map(|n| format!("'{n}'")).collect();
    match v.len() {
        0 => String::new(),
        1 => v[0].clone(),
        2 => format!("{} or {}", v[0], v[1]),
        _ => format!("{}, or {}", v[..v.len() - 1].join(", "), v[v.len() - 1]),
    }
}

/// Removes some rule names (and their commas) from a directive's list.
fn remove_rules_fix(src: &str, d: &Directive, remove: &[usize]) -> Fix {
    let mut fix = Fix::default();
    for &ri in remove {
        let r = &d.rules[ri];
        let mut span = r.span;
        if ri + 1 < d.rules.len() {
            // `a, b` -> remove `a, `
            span.end = d.rules[ri + 1].span.start;
        } else if ri > 0 {
            // remove `, b` from the previous rule's end
            let prev_end = d.rules[ri - 1].span.end;
            if !remove.contains(&(ri - 1)) {
                span.start = prev_end;
            }
        }
        let _ = src;
        fix.edits.push(crate::diagnostic::Edit {
            span,
            text: String::new(),
        });
    }
    fix.edits.sort_by_key(|e| e.span.start);
    fix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> InlineConfig {
        let r = ts_r_parser::parse(src, ts_r_ast::SourceKind::Js);
        let lines = LineIndex::new(src);
        InlineConfig::parse(&r.ast, src, &lines)
    }

    #[test]
    fn parses_directives() {
        let src = "/* eslint-disable no-console, no-debugger -- because */\nfoo(); // eslint-disable-line\n// eslint-disable-next-line no-alert\n/* eslint no-debugger: \"warn\", no-alert: [\"error\", {\"a\": 1}] */\n/* global zz: writable, yy */\n// eslint no-x: off\n";
        let c = parse(src);
        assert_eq!(c.directives.len(), 3);
        assert_eq!(c.directives[0].kind, DirectiveKind::Disable);
        assert_eq!(
            c.directives[0]
                .rules
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["no-console", "no-debugger"]
        );
        assert_eq!(c.directives[1].kind, DirectiveKind::DisableLine);
        assert_eq!(c.directives[1].line, 1);
        assert_eq!(c.directives[2].line, 3);
        assert_eq!(c.rules.len(), 2);
        assert_eq!(c.rules[1].value, serde_json::json!(["error", {"a": 1}]));
        assert_eq!(c.globals.len(), 2);
        assert!(c.globals[0].writable);
        assert_eq!(c.globals[1].name, "yy");
        assert_eq!(c.globals[0].span.text(src), "zz");
    }

    #[test]
    fn unused_messages() {
        assert_eq!(list_names(["a", "b"].into_iter()), "'a' or 'b'");
        assert_eq!(list_names(["a", "b", "c"].into_iter()), "'a', 'b', or 'c'");
        assert_eq!(strip_description(" no-x -- why"), " no-x ");
        assert_eq!(strip_description(" no-x--y"), " no-x--y");
    }

    #[test]
    fn suppression() {
        let src =
            "/* eslint-disable a */\nx;\n/* eslint-enable a */\ny; // eslint-disable-line b\nz;\n";
        let c = parse(src);
        let lines = LineIndex::new(src);
        let at = |needle: &str, rule: &'static str| {
            let pos = src.find(needle).unwrap() as u32;
            ProblemPos {
                rule: Some(rule),
                pos,
                line: lines.line_of(pos),
            }
        };
        let problems = [at("x;", "a"), at("y;", "b"), at("y;", "a"), at("z;", "b")];
        let out = apply_directives(
            &c.directives,
            &problems,
            src,
            Some(UnusedDirectiveOptions {
                severity: Severity::Warn,
            }),
            &|_| true,
            true,
        );
        assert_eq!(out.suppressed, [true, true, false, false]);
        assert!(out.unused.is_empty());
    }
}
