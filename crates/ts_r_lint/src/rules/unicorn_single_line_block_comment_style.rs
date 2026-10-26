//! `unicorn/single-line-block-comment-style`: a block comment alone on its line(s) whose
//! content is one non-blank line is laid out as `'multiline'` (default: delimiters on their
//! own lines, indented like the comment) or `'single-line'` (`/* text */`). `/*!` license
//! comments, ESLint disable/enable directives, tooling directives (source maps, linters,
//! coverage, formatters, `@ts-*`, `@jsx*`, Flow, minifier annotations, cspell, ...), `ignore`
//! patterns (tested on the text without delimiters and doc-comment asterisks) and doc
//! comments whose lines carry `*` prefixes are left alone. The line ending of the fix is the
//! first one in the comment, else the one ending its line, else the one before it, else the
//! file's first, else `\n`.

use super::prelude::*;
use super::unicorn_ast::is_js_space;
use super::unicorn_names::{RegexOpt, regex_list};
use crate::directives::InlineConfig;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/single-line-block-comment-style",
        "Enforce a consistent style for single-line block comments.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<SingleLineBlockCommentStyle>,
};

#[derive(Debug)]
struct SingleLineBlockCommentStyle {
    single_line: bool,
    ignore: Vec<RegexOpt>,
}

impl FromOptions for SingleLineBlockCommentStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let single_line = opts.first().and_then(Value::as_str) == Some("single-line");
        let ignore = regex_list(obj_opt(opts, 1).get("ignore"), "ignore")?;
        Ok(SingleLineBlockCommentStyle {
            single_line,
            ignore,
        })
    }
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

fn split_lines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if is_line_end(c) {
            out.push(&s[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|&(_, n)| n == '\n') {
                iter.next();
                next += 1;
            }
            start = next;
        }
    }
    out.push(&s[start..]);
    out
}

/// The line ending starting at byte `i`.
fn line_ending_at(src: &str, i: usize) -> Option<&str> {
    let rest = src.get(i..)?;
    if rest.starts_with("\r\n") {
        return Some("\r\n");
    }
    let c = rest.chars().next()?;
    is_line_end(c).then(|| &rest[..c.len_utf8()])
}

fn line_end(src: &str, from: usize) -> usize {
    src[from..]
        .char_indices()
        .find(|&(_, c)| is_line_end(c))
        .map_or(src.len(), |(i, _)| from + i)
}

fn line_start(src: &str, at: usize) -> usize {
    src[..at]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_line_end(c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// The line ending before the line holding `at`.
fn line_ending_before(src: &str, at: usize) -> Option<&str> {
    let ls = line_start(src, at);
    if ls == 0 {
        return None;
    }
    let prev = src[..ls].chars().next_back()?;
    let mut idx = ls - prev.len_utf8();
    if prev == '\n' && src[..idx].ends_with('\r') {
        idx -= 1;
    }
    line_ending_at(src, idx)
}

/// `rest` starts with one of `words` followed by whitespace, the end, or (with `colon`) `:`.
fn word_then_end(rest: &str, words: &[&str], colon: bool) -> bool {
    words.iter().any(|w| {
        rest.strip_prefix(w).is_some_and(|after| {
            after
                .chars()
                .next()
                .is_none_or(|c| is_js_space(c) || (colon && c == ':'))
        })
    })
}

fn strip_spaces(s: &str) -> &str {
    s.trim_start_matches(is_js_space)
}

/// Whitespace, at least one character.
fn strip_spaces1(s: &str) -> Option<&str> {
    let t = strip_spaces(s);
    (t.len() < s.len()).then_some(t)
}

/// Tooling directives that keep their layout.
fn is_directive_text(text: &str) -> bool {
    let t = strip_spaces(text);
    if let Some(r) = t.strip_prefix(['#', '@'])
        && let r = strip_spaces(r)
        && (r.starts_with("sourceURL=") || r.starts_with("sourceMappingURL="))
    {
        return true;
    }
    if word_then_end(
        t,
        &[
            "eslint-env",
            "eslint",
            "jshint",
            "jslint",
            "tslint",
            "jscs",
            "globals",
            "global",
            "exported",
            "no default",
            "noinspection",
        ],
        true,
    ) {
        return true;
    }
    let flow = t.strip_prefix('*').map_or(t, strip_spaces);
    if word_then_end(
        flow,
        &["flowlint-next-line", "flowlint-line", "flowlint"],
        true,
    ) || word_then_end(
        t,
        &["flowlint-next-line", "flowlint-line", "flowlint"],
        true,
    ) {
        return true;
    }
    if t.starts_with(':') || word_then_end(t, &["flow-include"], false) {
        return true;
    }
    for tool in ["c8", "istanbul", "nyc", "v8"] {
        if let Some(r) = t.strip_prefix(tool).and_then(strip_spaces1)
            && word_then_end(r, &["ignore"], false)
        {
            return true;
        }
    }
    if let Some(r) = t.strip_prefix("node:coverage").and_then(strip_spaces1) {
        if word_then_end(r, &["disable", "enable"], false) {
            return true;
        }
        if let Some(r) = r.strip_prefix("ignore").and_then(strip_spaces1)
            && word_then_end(r, &["next"], false)
        {
            return true;
        }
    }
    for tool in ["biome", "deno", "dprint", "oxlint", "prettier"] {
        let Some(r) = t.strip_prefix(tool).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        for action in ["ignore", "lint-ignore", "disable", "enable"] {
            let Some(r) = r.strip_prefix(action) else {
                continue;
            };
            if word_then_end(r, &[""], false) {
                return true;
            }
            if let Some(r) = r.strip_prefix('-')
                && word_then_end(
                    r,
                    &["line", "next-line", "start", "end", "file", "all"],
                    false,
                )
            {
                return true;
            }
        }
    }
    if t.starts_with("cspell:") || t.starts_with("spell-checker:") {
        return true;
    }
    if let Some(r) = t.strip_prefix('@')
        && word_then_end(
            r,
            &[
                "ts-check",
                "ts-nocheck",
                "ts-ignore",
                "ts-expect-error",
                "jsx",
                "jsxFrag",
                "jsxImportSource",
                "jsxRuntime",
                "jsxFactory",
                "flow",
                "noflow",
                "jest-environment",
                "vitest-environment",
                "noformat",
                "noprettier",
            ],
            true,
        )
    {
        return true;
    }
    if let Some(r) = t
        .strip_prefix("$FlowFixMe[")
        .or_else(|| t.strip_prefix("$FlowExpectedError["))
        && let Some(close) = r.find(']')
        && close > 0
        && word_then_end(&r[close + 1..], &[""], true)
    {
        return true;
    }
    if let Some(r) = t.strip_prefix(['#', '@'])
        && let Some(r) = r.strip_prefix("__")
    {
        for name in [
            "INLINE",
            "NOINLINE",
            "PURE",
            "KEY",
            "MANGLE_PROP",
            "NO_SIDE_EFFECTS",
        ] {
            if let Some(after) = r.strip_prefix(name).and_then(|a| a.strip_prefix("__"))
                && strip_spaces(after).is_empty()
            {
                return true;
            }
        }
    }
    false
}

impl Rule for SingleLineBlockCommentStyle {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.src();
        let ast = cx.ast();
        let directives: Vec<Span> = InlineConfig::parse(ast, src, cx.file.lines)
            .directives
            .iter()
            .map(|d| d.comment)
            .collect();
        for c in &ast.comments {
            if c.kind != CommentKind::Block {
                continue;
            }
            let (start, end) = (c.span.start as usize, c.span.end as usize);
            let prefix = &src[line_start(src, start)..start];
            if !js_trim(prefix).is_empty() || !js_trim(&src[end..line_end(src, end)]).is_empty() {
                continue;
            }
            let text = &src[start..end];
            let doc = text.starts_with("/**") && text.as_bytes().get(3) != Some(&b'*');
            let opening = if doc { "/**" } else { "/*" };
            if text.starts_with("/*!") || directives.contains(&c.span) {
                continue;
            }
            let inner = text.get(2..text.len().saturating_sub(2)).unwrap_or("");
            let comment_text = split_lines(inner)
                .iter()
                .map(|l| {
                    let l = strip_spaces(l);
                    if doc {
                        strip_spaces(l.strip_prefix('*').unwrap_or(l))
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if self.ignore.iter().any(|p| p.test(&comment_text)) || is_directive_text(&comment_text)
            {
                continue;
            }
            let content = text
                .get(opening.len()..text.len().saturating_sub(2))
                .unwrap_or("");
            let lines = split_lines(content);
            if doc && lines.iter().any(|l| strip_spaces(l).starts_with('*')) {
                continue;
            }
            let filled: Vec<&str> = lines
                .iter()
                .filter(|l| !js_trim(l).is_empty())
                .copied()
                .collect();
            let [single] = filled[..] else {
                continue;
            };
            let single = js_trim(single);
            let (style, fixed) = if self.single_line {
                if lines.len() == 1 {
                    continue;
                }
                ("single-line", format!("{opening} {single} */"))
            } else {
                if lines.len() == 3 && js_trim(lines[0]).is_empty() && js_trim(lines[2]).is_empty()
                {
                    continue;
                }
                let eol = content
                    .char_indices()
                    .find(|&(_, ch)| is_line_end(ch))
                    .and_then(|(i, _)| line_ending_at(content, i))
                    .or_else(|| line_ending_at(src, line_end(src, end)))
                    .or_else(|| line_ending_before(src, start))
                    .or_else(|| line_ending_at(src, line_end(src, 0)))
                    .unwrap_or("\n");
                (
                    "multiline",
                    format!("{opening}{eol}{prefix}{single}{eol}{prefix}*/"),
                )
            };
            cx.report(c.span, format!("Use a {style} block comment."))
                .id("single-line-block-comment-style")
                .fix(Fix::replace(c.span, fixed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives() {
        assert!(is_directive_text("eslint-env node"));
        assert!(is_directive_text("@ts-ignore"));
        assert!(is_directive_text("prettier-ignore"));
        assert!(is_directive_text("deno-lint-ignore-file"));
        assert!(is_directive_text("#__PURE__"));
        assert!(is_directive_text("istanbul ignore next"));
        assert!(!is_directive_text("eslintrc"));
        assert!(!is_directive_text("@jsxFoo"));
    }
}
