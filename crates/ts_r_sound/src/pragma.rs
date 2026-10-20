//! Comment directives: the per-file `// @ts-r <level>` pragma (TSR0004, TSR0005) and the
//! `// @ts-r-unsafe[ S-xx ...]: <reason>` escape hatch (TSR0001 to TSR0003). Both are read
//! from the comment list the parser keeps, never from strings or code.

use ts_r_ast::{Ast, CommentKind, Node};
use ts_r_span::{LineIndex, Span};

use crate::codes::{self, SoundMessage};
use crate::config::{Level, parse_rule_id};

/// A sound diagnostic before it becomes a `Diagnostic` (its file is implied).
#[derive(Clone, Debug)]
pub struct Raw {
    pub msg: SoundMessage,
    pub span: Span,
    pub text: String,
}

impl Raw {
    pub fn new(msg: SoundMessage, span: Span, args: &[&str]) -> Raw {
        Raw {
            msg,
            span,
            text: msg.format(args),
        }
    }
}

/// Text after `//` (or inside `/* */`), without the delimiters.
fn comment_body(text: &str) -> &str {
    if let Some(b) = text.strip_prefix("//") {
        b
    } else if let Some(b) = text.strip_prefix("/*") {
        b.strip_suffix("*/").unwrap_or(b)
    } else {
        text
    }
}

/// `@ts-r <word>` (not `@ts-r-unsafe`): the word, or `""` when missing.
fn pragma_word(body: &str) -> Option<&str> {
    let rest = body.trim_start().strip_prefix("@ts-r")?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() => Some(rest.split_whitespace().next().unwrap_or("")),
        _ => None,
    }
}

/// Start of the first statement (comments before it are the file's leading comments).
fn first_statement_start(ast: &Ast) -> u32 {
    match ast.node(ast.root) {
        Node::Program { stmts } => ast
            .list(*stmts)
            .first()
            .map_or(u32::MAX, |&s| ast.span(s).start),
        _ => u32::MAX,
    }
}

/// The level a file's pragma sets (`None`: no valid pragma) and the pragma diagnostics.
pub fn read_pragma(ast: &Ast, text: &str) -> (Option<Level>, Vec<Raw>) {
    let first = first_statement_start(ast);
    let mut level = None;
    let mut diags = Vec::new();
    for c in &ast.comments {
        if c.kind != CommentKind::Line {
            continue;
        }
        let body = comment_body(c.span.text(text));
        let Some(word) = pragma_word(body) else {
            continue;
        };
        if word.is_empty() {
            continue;
        }
        if c.span.start >= first {
            diags.push(Raw::new(codes::LATE_PRAGMA, c.span, &[]));
            continue;
        }
        match Level::parse(word) {
            Some(l) => {
                if level.is_none() {
                    level = Some(l);
                }
            }
            None => diags.push(Raw::new(codes::UNKNOWN_LEVEL, c.span, &[word])),
        }
    }
    (level, diags)
}

/// One `@ts-r-unsafe` directive.
#[derive(Clone, Debug)]
pub struct Hatch {
    pub span: Span,
    /// Zero-based line of the comment; it covers diagnostics starting on `line + 1`.
    pub line: u32,
    /// `None`: every rule.
    pub rules: Option<Vec<u8>>,
    pub reason: String,
    /// Has no error (TSR0001, TSR0003); only valid hatches suppress.
    pub valid: bool,
    pub used: bool,
}

impl Hatch {
    pub fn covers(&self, rule: u8) -> bool {
        // General codes (TSR00xx) are never suppressed.
        rule != 0
            && match &self.rules {
                None => true,
                Some(r) => r.contains(&rule),
            }
    }
}

/// Reads every `// @ts-r-unsafe` directive that sits on its own line.
pub fn read_hatches(ast: &Ast, text: &str, lines: &LineIndex) -> (Vec<Hatch>, Vec<Raw>) {
    let mut hatches = Vec::new();
    let mut diags = Vec::new();
    for c in &ast.comments {
        if c.kind != CommentKind::Line {
            continue;
        }
        let body = comment_body(c.span.text(text)).trim_start();
        let Some(rest) = body.strip_prefix("@ts-r-unsafe") else {
            continue;
        };
        if !matches!(
            rest.chars().next(),
            None | Some(':') | Some(' ') | Some('\t')
        ) {
            continue;
        }
        let line = lines.line_of(c.span.start);
        let line_start = lines.line_start(line).unwrap_or(0) as usize;
        let own_line = text
            .get(line_start..c.span.start as usize)
            .is_some_and(|s| s.trim().is_empty());
        if !own_line {
            continue;
        }
        let (head, reason) = match rest.find(':') {
            Some(i) => (&rest[..i], rest[i + 1..].trim()),
            None => (rest, ""),
        };
        let mut valid = true;
        let mut rules = Vec::new();
        for tok in head.split_whitespace() {
            match parse_rule_id(tok) {
                Some(r) => rules.push(r),
                None => {
                    diags.push(Raw::new(codes::UNKNOWN_RULE, c.span, &[tok]));
                    valid = false;
                }
            }
        }
        if reason.is_empty() {
            if valid {
                diags.push(Raw::new(codes::UNSAFE_NEEDS_REASON, c.span, &[]));
            }
            valid = false;
        }
        hatches.push(Hatch {
            span: c.span,
            line,
            rules: (!rules.is_empty()).then_some(rules),
            reason: reason.to_owned(),
            valid,
            used: false,
        });
    }
    (hatches, diags)
}

/// `@ts-ignore`, `@ts-expect-error` and `@ts-nocheck` directive comments (S-12): tsc's form,
/// a comment whose text starts with the directive after `//` or `/*` and spaces.
pub fn suppression_directives(ast: &Ast, text: &str) -> Vec<(Span, &'static str)> {
    let mut out = Vec::new();
    for c in &ast.comments {
        if c.kind == CommentKind::Hashbang {
            continue;
        }
        let t = c.span.text(text);
        let body = if let Some(b) = t.strip_prefix("///") {
            b
        } else {
            comment_body(t)
        };
        let body = body.trim_start_matches(|ch: char| ch == '*' || ch.is_whitespace());
        for d in ["ts-ignore", "ts-expect-error", "ts-nocheck"] {
            if let Some(rest) = body.strip_prefix('@').and_then(|b| b.strip_prefix(d))
                && rest
                    .chars()
                    .next()
                    .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
            {
                out.push((c.span, d));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn parse(src: &str) -> Ast {
        ts_r_parser_free_parse(src)
    }

    // The parser is not a dependency of this crate; the checker re-exports parsed files, so
    // tests build a tiny AST through the checker's `SourceFile`.
    fn ts_r_parser_free_parse(src: &str) -> Ast {
        let f = ts_r_checker::SourceFile::new(
            "/t.ts".into(),
            src.into(),
            false,
            &ts_r_binder::BindOptions::default(),
        );
        assert_eq!(SourceKind::from_path("/t.ts"), f.kind);
        f.ast
    }

    #[test]
    fn pragmas() {
        let src = "// @ts-r strict\nexport const a = 1;\n";
        let ast = parse(src);
        let (l, d) = read_pragma(&ast, src);
        assert_eq!(l, Some(Level::Strict));
        assert!(d.is_empty());
        let src = "// @ts-r loud\n// @ts-r-unsafe: x\nexport const a = 1;\n// @ts-r off\n";
        let ast = parse(src);
        let (l, d) = read_pragma(&ast, src);
        assert_eq!(l, None);
        let codes: Vec<u32> = d.iter().map(|r| r.msg.n).collect();
        assert_eq!(codes, vec![4, 5]);
    }

    #[test]
    fn hatches() {
        let src = "// @ts-r-unsafe S-02 S-03: ok\nx;\n// @ts-r-unsafe\ny;\n// @ts-r-unsafe S-99: r\nz; // @ts-r-unsafe: trailing\n";
        let ast = parse(src);
        let lines = LineIndex::new(src);
        let (h, d) = read_hatches(&ast, src, &lines);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].rules, Some(vec![2, 3]));
        assert!(h[0].valid && h[0].covers(3) && !h[0].covers(1));
        assert!(!h[1].valid);
        assert!(!h[2].valid);
        let codes: Vec<u32> = d.iter().map(|r| r.msg.n).collect();
        assert_eq!(codes, vec![1, 3]);
    }

    #[test]
    fn suppression_comments() {
        let src = "// @ts-ignore\nx;\n/* @ts-expect-error */\ny;\n// see @ts-ignore here\nconst s = \"// @ts-ignore\";\n// @ts-nocheck\n";
        let ast = parse(src);
        let found: Vec<&str> = suppression_directives(&ast, src)
            .into_iter()
            .map(|(_, d)| d)
            .collect();
        assert_eq!(found, vec!["ts-ignore", "ts-expect-error", "ts-nocheck"]);
    }
}
