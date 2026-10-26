//! `jsdoc/text-escaping`: escape `<` and `&` (`escapeHTML`) or paired backtick runs
//! (`escapeMarkdown`) in block and tag descriptions.
//!
//! With `escapeHTML`, a `<`, or an `&` that doesn't start `&amp;`/`&lt;`/`&gt;`/`&quot;`/
//! `&apos;`, followed by a non-space character is unescaped. With `escapeMarkdown`, a run of
//! backticks not preceded by `\`, non-backtick text, then a run of exactly as many backticks
//! is unescaped (its first backtick gets a `\`). The block description is checked first: the
//! first line with a match is rewritten and reported, and nothing else is. Otherwise every
//! tag but `@example` with a match reports, the fix rewriting the tag's first such line on
//! top of the earlier tags' rewrites. Without either option each block reports at 1:2.

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::jsdoc::Block;
use crate::jsdoc_util::{description, each_block, tag_description_lines};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/text-escaping",
        "Auto-escape certain characters that are input within block and tag descriptions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<TextEscaping>,
};

#[derive(Debug, Default)]
struct TextEscaping {
    html: bool,
    markdown: bool,
}

impl FromOptions for TextEscaping {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let flag = |name: &str| {
            opts.first()
                .and_then(|o| o.get(name))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        Ok(TextEscaping {
            html: flag("escapeHTML"),
            markdown: flag("escapeMarkdown"),
        })
    }
}

/// Rewrites a description line, or gives `None` when it has nothing to escape.
type Escape = fn(&str) -> Option<String>;

/// `s` with its unescaped `<` and `&` escaped, or `None` when there are none.
fn html_escaped(s: &str) -> Option<String> {
    const ENTITIES: [&str; 5] = ["amp;", "lt;", "gt;", "quot;", "apos;"];
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    for (i, c) in s.char_indices() {
        let rest = &s[i + c.len_utf8()..];
        let before_text = rest.chars().next().is_some_and(|n| !n.is_whitespace());
        let hit = match c {
            '<' => before_text,
            '&' => before_text && !ENTITIES.iter().any(|e| rest.starts_with(e)),
            _ => false,
        };
        if hit {
            changed = true;
            out.push_str(if c == '<' { "&lt;" } else { "&amp;" });
        } else {
            out.push(c);
        }
    }
    changed.then_some(out)
}

/// The byte ranges of the paired backtick sequences in `s`: a run of backticks not preceded
/// by `\` (the run may start inside a longer one), non-backtick text, and a run of exactly as
/// many backticks.
fn markdown_matches(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let run_len = |at: usize| b[at..].iter().take_while(|&&c| c == b'`').count();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let r = run_len(i);
        let text_start = i + r;
        let text_end = b[text_start..]
            .iter()
            .position(|&c| c == b'`')
            .map_or(b.len(), |p| text_start + p);
        if text_end == text_start || text_end == b.len() {
            i = text_end.max(i + 1);
            continue;
        }
        let close = run_len(text_end);
        // Only the start leaving `close` backticks before the text can pair; inside the run
        // the character before is a backtick, so only the run's first one needs checking.
        let start = (close <= r).then(|| i + (r - close));
        let ok = start.filter(|&st| st > i || st == 0 || b[st - 1] != b'\\');
        match ok {
            Some(st) => {
                out.push((st, text_end + close));
                i = text_end + close;
            }
            None => i = text_end,
        }
    }
    out
}

/// `s` with each paired backtick sequence's opening run prefixed with `\`, or `None`.
fn markdown_escaped(s: &str) -> Option<String> {
    let found = markdown_matches(s);
    if found.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(s.len() + found.len());
    let mut last = 0;
    for (start, end) in found {
        out.push_str(&s[last..start]);
        out.push('\\');
        out.push_str(&s[start..end]);
        last = end;
    }
    out.push_str(&s[last..]);
    Some(out)
}

/// Rewrites the first of `ids` whose description `escape` changes; returns its position in
/// `ids` (0 when none).
fn set_first(b: &mut Block, ids: &[usize], escape: Escape) -> usize {
    for (pos, &id) in ids.iter().enumerate() {
        if let Some(new) = escape(&b.lines[id].tokens.description) {
            b.lines[id].tokens.description = new;
            return pos;
        }
    }
    0
}

impl Rule for TextEscaping {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let (html, markdown) = (self.html, self.markdown);
        each_block(cx, |cx, finder, doc| {
            if !html && !markdown {
                cx.report(
                    Span::at(0),
                    "You must include either `escapeHTML` or `escapeMarkdown`",
                )
                .loc(Loc::at(1, 2));
                return;
            }
            let (escape, kind): (Escape, _) = if html {
                (html_escaped, "HTML characters < or &")
            } else {
                (markdown_escaped, "Markdown backtick sequences")
            };
            let test = |s: &str| escape(s).is_some();
            if description(&doc.block).lines.iter().any(|d| test(d)) {
                // The lines before the first tag or the closing line (the first line always).
                let ids: Vec<usize> = doc
                    .block
                    .source
                    .iter()
                    .enumerate()
                    .take_while(|&(idx, &id)| {
                        let t = &doc.block.lines[id].tokens;
                        idx == 0 || (t.tag.is_empty() && t.end.is_empty())
                    })
                    .map(|(_, &id)| id)
                    .collect();
                let line = set_first(&mut doc.block, &ids, escape);
                let span = doc.line_span(finder, line as i64);
                cx.report(span, format!("You have unescaped {kind}"))
                    .fix(doc.fix());
                return;
            }
            for ti in 0..doc.block.tags.len() {
                if doc.block.tags[ti].tag == "example"
                    || !tag_description_lines(&doc.block, ti)
                        .iter()
                        .any(|d| test(d))
                {
                    continue;
                }
                let ids = doc.block.tags[ti].source.clone();
                let line = set_first(&mut doc.block, &ids, escape) as i64 + doc.block.tag_line(ti);
                let span = doc.line_span(finder, line);
                cx.report(span, format!("You have unescaped {kind} in a tag"))
                    .fix(doc.fix());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(
            html_escaped("<a> and &gt; and &#x; and & x").as_deref(),
            Some("&lt;a> and &gt; and &amp;#x; and & x")
        );
        assert_eq!(html_escaped("< and &"), None);
        assert_eq!(markdown_escaped("a `b` c").as_deref(), Some("a \\`b` c"));
        assert_eq!(markdown_escaped("\\`b`"), None);
        assert_eq!(markdown_escaped("``a`").as_deref(), Some("`\\`a`"));
        assert_eq!(markdown_escaped("`a``"), None);
        assert_eq!(markdown_escaped("`a` `b`").as_deref(), Some("\\`a` \\`b`"));
        assert_eq!(markdown_escaped("`"), None);
    }
}
