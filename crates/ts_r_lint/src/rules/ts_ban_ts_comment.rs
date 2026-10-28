//! `@typescript-eslint/ban-ts-comment`: disallow `@ts-<directive>` comments or require
//! descriptions after them.

use super::prelude::*;
use crate::regex;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/ban-ts-comment",
        "Disallow `@ts-<directive>` comments or require descriptions after directives",
        TS_REC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<BanTsComment>,
};

#[derive(Debug, Clone, PartialEq)]
enum Setting {
    Off,
    Ban,
    Describe(Option<String>),
}

#[derive(Debug)]
struct BanTsComment {
    expect_error: Setting,
    ignore: Setting,
    nocheck: Setting,
    check: Setting,
    min_len: usize,
}

fn setting(o: &serde_json::Map<String, Value>, k: &str, default: Setting) -> Setting {
    match o.get(k) {
        Some(Value::Bool(true)) => Setting::Ban,
        Some(Value::Bool(false)) => Setting::Off,
        Some(Value::String(s)) if s == "allow-with-description" => Setting::Describe(None),
        Some(Value::Object(f)) => {
            Setting::Describe(str_field(f, "descriptionFormat").map(str::to_owned))
        }
        _ => default,
    }
}

impl FromOptions for BanTsComment {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(BanTsComment {
            expect_error: setting(o, "ts-expect-error", Setting::Describe(None)),
            ignore: setting(o, "ts-ignore", Setting::Ban),
            nocheck: setting(o, "ts-nocheck", Setting::Ban),
            check: setting(o, "ts-check", Setting::Off),
            min_len: o
                .get("minimumDescriptionLength")
                .and_then(Value::as_u64)
                .unwrap_or(3) as usize,
        })
    }
}

const DIRECTIVES: [&str; 4] = ["expect-error", "ignore", "nocheck", "check"];

/// `@ts-<directive><description>` at the start of `text`.
fn directive(text: &str) -> Option<(&'static str, &str)> {
    let rest = text.strip_prefix("@ts-")?;
    let d = DIRECTIVES.into_iter().find(|d| rest.starts_with(d))?;
    Some((d, &rest[d.len()..]))
}

/// Approximate grapheme count: joiners, variation selectors, skin tones and combining
/// marks extend the previous character.
pub(crate) fn graphemes(s: &str) -> usize {
    let mut n = 0;
    let mut join_next = false;
    for c in s.chars() {
        let u = c as u32;
        if c == '\u{200d}' {
            join_next = true;
            continue;
        }
        let extends = matches!(u, 0x300..=0x36f | 0xfe00..=0xfe0f | 0x1f3fb..=0x1f3ff | 0xe0020..=0xe007f | 0xe0100..=0xe01ef);
        if !extends && !join_next {
            n += 1;
        }
        join_next = false;
    }
    n
}

impl BanTsComment {
    fn setting(&self, d: &str) -> &Setting {
        match d {
            "expect-error" => &self.expect_error,
            "ignore" => &self.ignore,
            "nocheck" => &self.nocheck,
            _ => &self.check,
        }
    }
}

impl Rule for BanTsComment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let first_token = match *ast.node(ast.root) {
            Node::Program { stmts } => ast
                .list(stmts)
                .first()
                .map_or(u32::MAX, |&s| cx.span(s).start),
            _ => u32::MAX,
        };
        let mut found = Vec::new();
        for c in &ast.comments {
            let (s, e) = (c.span.start as usize, c.span.end as usize);
            let (d, desc) = match c.kind {
                CommentKind::Line => {
                    let value = &src[s + 2..e];
                    let slashes = value.len() - value.trim_start_matches('/').len();
                    let Some((d, desc)) = directive(value.trim_start_matches('/').trim_start())
                    else {
                        continue;
                    };
                    // Pragmas allow two or three leading slashes only.
                    if matches!(d, "check" | "nocheck") && slashes > 1 {
                        continue;
                    }
                    (d, desc)
                }
                CommentKind::Block => {
                    let value = &src[s + 2..e.saturating_sub(2).max(s + 2)];
                    let last = value
                        .rsplit(['\n', '\r', '\u{2028}', '\u{2029}'])
                        .next()
                        .unwrap_or("");
                    let t = last
                        .trim_start()
                        .trim_start_matches(['/', '*'])
                        .trim_start();
                    let Some((d, desc)) = directive(t) else {
                        continue;
                    };
                    if !matches!(d, "expect-error" | "ignore") {
                        continue;
                    }
                    (d, desc)
                }
                CommentKind::Hashbang => continue,
            };
            if d == "nocheck" && c.span.end > first_token {
                continue;
            }
            found.push((c.span, d, desc));
        }
        for (span, d, desc) in found {
            match self.setting(d) {
                Setting::Off => {}
                Setting::Ban if d == "ignore" => {
                    let text = span.text(src);
                    let Some(at) = text.find("@ts-ignore") else {
                        continue;
                    };
                    let p = span.start + at as u32;
                    cx.report(
                        span,
                        "Use \"@ts-expect-error\" instead of \"@ts-ignore\", as \"@ts-ignore\" will do nothing if the following line is error-free.",
                    )
                    .id("tsIgnoreInsteadOfExpectError")
                    .suggest(
                        "replaceTsIgnoreWithTsExpectError",
                        "Replace \"@ts-ignore\" with \"@ts-expect-error\".",
                        Fix::replace(Span::new(p, p + 10), "@ts-expect-error"),
                    );
                }
                Setting::Ban => {
                    cx.report(
                        span,
                        format!("Do not use \"@ts-{d}\" because it alters compilation errors."),
                    )
                    .id("tsDirectiveComment");
                }
                Setting::Describe(format) => {
                    if graphemes(desc.trim()) < self.min_len {
                        let min = self.min_len;
                        cx.report(
                            span,
                            format!(
                                "Include a description after the \"@ts-{d}\" directive to explain why the @ts-{d} is necessary. The description must be {min} characters or longer."
                            ),
                        )
                        .id("tsDirectiveCommentRequiresDescription");
                    } else if let Some(f) = format
                        && !regex::compile(f).is_some_and(|r| r.is_match(desc, false))
                    {
                        let f = f.clone();
                        cx.report(
                            span,
                            format!(
                                "The description for the \"@ts-{d}\" directive must match the {f} format."
                            ),
                        )
                        .id("tsDirectiveCommentDescriptionNotMatchPattern");
                    }
                }
            }
        }
    }
}
