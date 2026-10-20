//! `no-irregular-whitespace`: disallow irregular whitespace.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-irregular-whitespace",
        "Disallow irregular whitespace",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoIrregularWhitespace>,
};

#[derive(Debug, Default)]
struct NoIrregularWhitespace {
    skip_strings: bool,
    skip_comments: bool,
    skip_regexps: bool,
    skip_templates: bool,
    skip_jsx_text: bool,
}

impl FromOptions for NoIrregularWhitespace {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoIrregularWhitespace {
            skip_strings: bool_field(o, "skipStrings", true),
            skip_comments: bool_field(o, "skipComments", false),
            skip_regexps: bool_field(o, "skipRegExps", false),
            skip_templates: bool_field(o, "skipTemplates", false),
            skip_jsx_text: bool_field(o, "skipJSXText", false),
        })
    }
}

fn is_irregular(c: char) -> bool {
    ('\u{2000}'..='\u{200B}').contains(&c)
        || matches!(
            c,
            '\u{000B}'
                | '\u{000C}'
                | '\u{00A0}'
                | '\u{0085}'
                | '\u{1680}'
                | '\u{180E}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
        )
}

impl Rule for NoIrregularWhitespace {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.src();
        let ast = cx.ast();
        let mut hits: Vec<u32> = Vec::new();
        for (i, c) in src.char_indices() {
            if is_irregular(c) && !(i == 0 && c == '\u{FEFF}') {
                hits.push(i as u32);
            }
        }
        if hits.is_empty() {
            return;
        }
        // Ranges where irregular whitespace is allowed.
        let mut skip: Vec<Span> = Vec::new();
        if self.skip_comments {
            skip.extend(ast.comments.iter().map(|c| c.span));
        }
        for id in 1..ast.nodes.len() as u32 {
            let Some(n) = NodeId::from_raw(id) else {
                continue;
            };
            let allowed = match ast.kind(n) {
                NodeKind::StringLit => self.skip_strings,
                NodeKind::Directive => self.skip_strings,
                NodeKind::RegexLit => self.skip_regexps,
                NodeKind::TemplateElement => self.skip_templates,
                NodeKind::JsxText => self.skip_jsx_text,
                _ => false,
            };
            if allowed {
                skip.push(ast.span(n));
            }
        }
        // Runs of irregular whitespace are reported once.
        let mut runs: Vec<Span> = Vec::new();
        for pos in hits {
            if skip.iter().any(|s| s.start <= pos && pos < s.end) {
                continue;
            }
            let c = src[pos as usize..].chars().next().unwrap_or(' ');
            let len = c.len_utf8() as u32;
            // Line separators are reported on their own.
            let line_sep = matches!(c, '\u{2028}' | '\u{2029}');
            let prev_sep = runs.last().is_some_and(|l| {
                matches!(l.text(src).chars().last(), Some('\u{2028}' | '\u{2029}'))
            });
            match runs.last_mut() {
                Some(last) if last.end == pos && !line_sep && !prev_sep => last.end = pos + len,
                _ => runs.push(Span::new(pos, pos + len)),
            }
        }
        for run in runs {
            cx.report(run, "Irregular whitespace not allowed.")
                .id("noIrregularWhitespace");
        }
    }
}
