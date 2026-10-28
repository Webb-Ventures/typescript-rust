//! `unicorn/template-indent`: multiline template literals that are whitespace-insensitive
//! (tagged with one of `tags`, an argument of one of `functions`, preceded by a block comment
//! from `comments`, a Jest inline snapshot, or matched by `selectors`) are re-indented: their
//! content is dedented and indented one level past the margin of the line the template
//! starts on, with the closing delimiter on its own line at that margin. Templates with text
//! on the opening line are left alone. The level is `indent` when given, else the file's
//! detected indentation (lines inside templates and blank lines ignored), else (only on an
//! unindented line) the template's own, else a tab or two spaces after the margin's first
//! character. The fix rewrites each quasi's text.

use super::prelude::*;
use super::unicorn_ast::{is_jest_inline_snapshot, is_js_space, matches_path};
use crate::esquery::{self, Selector};
use crate::estree::EsTree;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/template-indent",
        "Fix whitespace-insensitive template indentation.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<TemplateIndent>,
};

#[derive(Debug)]
struct TemplateIndent {
    indent: Option<String>,
    tags: Vec<String>,
    functions: Vec<String>,
    selectors: Vec<Selector>,
    comments: Vec<String>,
}

fn strings(v: Option<&Value>, default: &[&str]) -> Vec<String> {
    match v.and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        None => default.iter().map(|s| (*s).to_owned()).collect(),
    }
}

impl FromOptions for TemplateIndent {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let indent = match o.get("indent") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(" ".repeat(n.as_u64().unwrap_or(0).min(1000) as usize)),
            _ => None,
        };
        let mut selectors = Vec::new();
        for s in strings(o.get("selectors"), &[]) {
            selectors.push(esquery::parse(&s).map_err(|e| format!("invalid selector `{s}`: {e}"))?);
        }
        Ok(TemplateIndent {
            indent,
            tags: strings(
                o.get("tags"),
                &["outdent", "dedent", "gql", "sql", "html", "styled"],
            ),
            functions: strings(o.get("functions"), &["dedent", "stripIndent"]),
            selectors,
            comments: strings(o.get("comments"), &["HTML", "indent"])
                .into_iter()
                .map(|c| c.to_lowercase())
                .collect(),
        })
    }
}

fn is_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Byte offsets where a multiline `^` matches.
fn line_starts(s: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, c) in s.char_indices() {
        if is_term(c) {
            v.push(i + c.len_utf8());
        }
    }
    v
}

/// `strip-indent`: removes the smallest indentation (spaces and tabs before a non-space) of
/// any line from every line that has that much.
fn strip_indent(s: &str) -> String {
    let starts = line_starts(s);
    let mut min = usize::MAX;
    for &p in &starts {
        let run = s[p..]
            .bytes()
            .take_while(|&b| b == b' ' || b == b'\t')
            .count();
        if s[p + run..].chars().next().is_some_and(|c| !is_js_space(c)) {
            min = min.min(run);
        }
    }
    if min == 0 || min == usize::MAX {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for &p in &starts {
        let run = s[p..]
            .bytes()
            .take_while(|&b| b == b' ' || b == b'\t')
            .count();
        if run >= min {
            out.push_str(&s[last..p]);
            last = p + min;
        }
    }
    out.push_str(&s[last..]);
    out
}

/// `indent-string`: `indent` before every line that has a non-space character.
fn indent_string(s: &str, indent: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for p in line_starts(s) {
        let blank = s[p..]
            .chars()
            .find(|&c| !is_js_space(c) || is_term(c))
            .is_none_or(is_term);
        if !blank {
            out.push_str(&s[last..p]);
            out.push_str(indent);
            last = p;
        }
    }
    out.push_str(&s[last..]);
    out
}

/// `detect-indent`: the most used indentation step (spaces or tabs) between lines.
fn detect_indent(text: &str) -> String {
    fn map(text: &str, ignore_single: bool) -> Vec<((bool, usize), (u32, u32))> {
        // (is_space, size) -> (uses, weight), in insertion order.
        let mut indents: Vec<((bool, usize), (u32, u32))> = Vec::new();
        let mut prev_size = 0usize;
        let mut prev_type: Option<bool> = None;
        let mut key: Option<(bool, usize)> = None;
        for line in text.split('\n') {
            if line.is_empty() {
                continue;
            }
            let b = line.as_bytes();
            let (is_space, n) = match b[0] {
                b' ' => (true, b.iter().take_while(|&&c| c == b' ').count()),
                b'\t' => (false, b.iter().take_while(|&&c| c == b'\t').count()),
                _ => {
                    prev_size = 0;
                    // Equal to neither type.
                    prev_type = None;
                    continue;
                }
            };
            if ignore_single && is_space && n == 1 {
                continue;
            }
            if prev_type != Some(is_space) {
                prev_size = 0;
            }
            prev_type = Some(is_space);
            let diff = n as isize - prev_size as isize;
            prev_size = n;
            let (uses, weight) = if diff == 0 {
                (0, 1)
            } else {
                let abs = diff.unsigned_abs();
                if ignore_single && is_space && abs == 1 {
                    continue;
                }
                key = Some((is_space, abs));
                (1, 0)
            };
            let Some(k) = key else { continue };
            match indents.iter_mut().find(|(kk, _)| *kk == k) {
                Some((_, e)) => {
                    e.0 += uses;
                    e.1 += weight;
                }
                None => indents.push((k, (1, 0))),
            }
        }
        indents
    }
    let mut indents = map(text, true);
    if indents.is_empty() {
        indents = map(text, false);
    }
    let mut best: Option<(bool, usize)> = None;
    let (mut max_used, mut max_weight) = (0, 0);
    for (k, (used, weight)) in indents {
        if used > max_used || (used == max_used && weight > max_weight) {
            max_used = used;
            max_weight = weight;
            best = Some(k);
        }
    }
    match best {
        Some((true, n)) => " ".repeat(n),
        Some((false, n)) => "\t".repeat(n),
        None => String::new(),
    }
}

/// Lines joined with `\n`, blank ones (and the `ignored` 1-based lines) emptied.
fn detection_text<'s>(lines: impl Iterator<Item = &'s str>, ignored: &[bool]) -> String {
    let mut out = String::new();
    for (i, line) in lines.enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if ignored.get(i + 1).copied().unwrap_or(false) || line.trim_matches(is_js_space).is_empty()
        {
            continue;
        }
        out.push_str(line);
    }
    out
}

/// ESLint's `sourceCode.lines`.
fn source_lines(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = src.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if is_term(c) {
            out.push(&src[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && it.peek().is_some_and(|&(_, n)| n == '\n') {
                it.next();
                end += 1;
            }
            start = end;
        }
    }
    out.push(&src[start..]);
    out
}

fn split_crlf(s: &str) -> impl Iterator<Item = &str> {
    s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// The placeholder standing in for each `${...}` while the quasis are re-indented.
const HOLE: char = '\u{f8ff}';

impl TemplateIndent {
    fn should_indent(&self, cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let ast = cx.ast();
        let src = cx.src();
        let start = cx.span(node).start as usize;
        if !self.comments.is_empty() {
            let before = src[..start].trim_end_matches(is_js_space);
            if before.ends_with("*/")
                && let Some(c) = ast
                    .comments
                    .iter()
                    .find(|c| c.span.end as usize == before.len() && c.kind == CommentKind::Block)
            {
                let text = &src[c.span.start as usize + 2..c.span.end as usize - 2];
                let text = text.trim_matches(is_js_space).to_lowercase();
                if self.comments.contains(&text) {
                    return true;
                }
            }
        }
        if is_jest_inline_snapshot(cx, node) {
            return true;
        }
        let parent = parent_skip_parens(ast, cx.file.parents, node);
        if !self.tags.is_empty()
            && let Some(p) = cx.parent(node)
            && let Node::TaggedTemplate { tag, quasi, .. } = *cx.node(p)
            && quasi == node
            && self.tags.iter().any(|t| matches_path(ast, tag, t))
        {
            return true;
        }
        if !self.functions.is_empty()
            && let Some(p) = parent
            && let Node::Call { callee, args, .. } = *cx.node(p)
            && ast.list(args).iter().any(|&a| skip_parens(ast, a) == node)
            && self.functions.iter().any(|f| matches_path(ast, callee, f))
        {
            return true;
        }
        false
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, file_indent: &mut Option<String>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::TemplateLit { parts } = *cx.node(node) else {
            return;
        };
        let quasis: Vec<Span> = ast
            .list(parts)
            .iter()
            .filter(|&&p| matches!(ast.node(p), Node::TemplateElement { .. }))
            .map(|&p| ast.span(p))
            .collect();
        let mut joined = String::new();
        for (i, q) in quasis.iter().enumerate() {
            if i > 0 {
                joined.push(HOLE);
            }
            joined.push_str(q.text(src));
        }
        if src.contains(HOLE) {
            return;
        }
        let Some(nl) = joined.find('\n') else {
            return;
        };
        let (eol_at, eol) = if nl > 0 && joined.as_bytes()[nl - 1] == b'\r' {
            (nl - 1, "\r\n")
        } else {
            (nl, "\n")
        };
        if !joined[..eol_at].trim_matches(is_js_space).is_empty() {
            return;
        }
        let lines = cx.file.lines;
        let start = cx.span(node).start;
        let line_start = lines.line_start(lines.line_of(start)).unwrap_or(0) as usize;
        let margin_len = src[line_start..]
            .char_indices()
            .find(|&(_, c)| !is_js_space(c) || is_term(c))
            .map_or(src.len() - line_start, |(i, _)| i);
        let margin = &src[line_start..line_start + margin_len];
        let indent = match &self.indent {
            Some(i) => i.clone(),
            None => {
                let fi = file_indent.get_or_insert_with(|| file_indent_of(cx));
                if !fi.is_empty() {
                    fi.clone()
                } else {
                    let own = if margin.is_empty() {
                        let text = detection_text(split_crlf(&joined), &[]);
                        let d = detect_indent(&strip_indent(&text));
                        if d.is_empty() {
                            detect_indent(&text)
                        } else {
                            d
                        }
                    } else {
                        String::new()
                    };
                    if !own.is_empty() {
                        own
                    } else if margin.starts_with('\t') {
                        "\t".to_owned()
                    } else {
                        "  ".to_owned()
                    }
                }
            }
        };
        let dedented = strip_indent(&joined);
        let mut trimmed: &str = dedented.strip_prefix(eol).unwrap_or(&dedented);
        let t = trimmed.trim_end_matches([' ', '\t']);
        if let Some(t) = t.strip_suffix(eol) {
            trimmed = t;
        }
        let fixed = format!(
            "{eol}{}{eol}{margin}",
            indent_string(trimmed, &format!("{margin}{indent}"))
        );
        if fixed == joined {
            return;
        }
        let pieces: Vec<&str> = fixed.split(HOLE).collect();
        if pieces.len() != quasis.len() {
            return;
        }
        let mut fix: Option<Fix> = None;
        for (q, text) in quasis.iter().zip(pieces) {
            fix = Some(match fix {
                None => Fix::replace(*q, text),
                Some(f) => f.and(*q, text),
            });
        }
        cx.report_node(node, "Templates should be properly indented.")
            .id("template-indent")
            .fix_opt(fix);
    }
}

fn file_indent_of(cx: &Ctx<'_, '_>) -> String {
    let ast = cx.ast();
    let lines = cx.file.lines;
    let src_lines = source_lines(cx.src());
    let mut ignored = vec![false; src_lines.len() + 2];
    for id in (1..=ast.node_count() as u32).filter_map(NodeId::from_raw) {
        if matches!(ast.node(id), Node::TemplateLit { .. }) && !ast.is_jsdoc(id) {
            let sp = ast.span(id);
            let (a, b) = (lines.line_of(sp.start) + 1, lines.line_of(sp.end) + 1);
            for l in a + 1..=b {
                if let Some(x) = ignored.get_mut(l as usize) {
                    *x = true;
                }
            }
        }
    }
    detect_indent(&detection_text(src_lines.into_iter(), &ignored))
}

impl Rule for TemplateIndent {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut targets: Vec<NodeId> = Vec::new();
        for id in (1..=ast.node_count() as u32).filter_map(NodeId::from_raw) {
            if matches!(ast.node(id), Node::TemplateLit { .. })
                && !ast.is_jsdoc(id)
                && self.should_indent(cx, id)
            {
                targets.push(id);
            }
        }
        if !self.selectors.is_empty() {
            let tree = EsTree::build(ast, cx.src());
            let prepared: Vec<_> = self.selectors.iter().map(Selector::prepare).collect();
            for n in 0..tree.len() as u32 {
                let e = &tree.nodes[n as usize];
                if e.ty == "TemplateLiteral"
                    && prepared.iter().any(|p| p.matches_listener(&tree, n))
                {
                    targets.push(e.ast);
                }
            }
        }
        targets.sort_by_key(|&id| (cx.span(id).start, id.index()));
        targets.dedup();
        let mut file_indent = None;
        for t in targets {
            self.check(cx, t, &mut file_indent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(strip_indent("\n  a\n    b\n"), "\na\n  b\n");
        assert_eq!(indent_string("a\n\nb", "\t"), "\ta\n\n\tb");
        assert_eq!(detect_indent("a\n\tb\n\t\tc\n"), "\t");
        assert_eq!(detect_indent("a\n  b\n    c\n  d\n"), "  ");
        assert_eq!(source_lines("a\r\nb\nc"), ["a", "b", "c"]);
    }
}
