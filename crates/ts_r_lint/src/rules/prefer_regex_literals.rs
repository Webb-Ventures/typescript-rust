//! `prefer-regex-literals`: `RegExp("abc")` / `new RegExp("abc", "g")` with static strings,
//! and (with `disallowRedundantWrapping`) `new RegExp(/abc/)`.

use super::prelude::*;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-regex-literals",
        "Disallow use of the `RegExp` constructor in favor of regular expression literals",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferRegexLiterals>,
};

#[derive(Debug)]
struct PreferRegexLiterals {
    disallow_redundant_wrapping: bool,
}

impl FromOptions for PreferRegexLiterals {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(PreferRegexLiterals {
            disallow_redundant_wrapping: bool_field(
                obj_opt(opts, 0),
                "disallowRedundantWrapping",
                false,
            ),
        })
    }
}

/// Tokens after which a regex literal can stand.
const PRECEDING_PUNCT: &[&str] = &[
    "(", ";", "[", ",", "=", "+", "*", "-", "?", "~", "%", "**", "!", "&&", "||", "??", "...", "<",
    ">", "<=", ">=", "==", "===", "!=", "!==", "<<", ">>", ">>>", "&", "|", "^", ":", "{", "=>",
    "*=", "<<=", ">>=", ">>>=", "^=", "|=", "&=", "??=", "||=", "&&=", "**=", "+=", "-=", "/=",
    "%=", "/",
];
const PRECEDING_WORDS: &[&str] = &[
    "typeof",
    "instanceof",
    "return",
    "delete",
    "void",
    "in",
    "do",
    "break",
    "continue",
    "debugger",
    "case",
    "throw",
];

/// The value of a static string argument: a string, a template without substitutions, or
/// `String.raw` of one.
fn string_value(cx: &Ctx<'_, '_>, n: NodeId) -> Option<String> {
    let ast = cx.ast();
    let n = skip_parens(ast, n);
    match *cx.node(n) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match *ast.list(parts) {
            [e] => match *cx.node(e) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        Node::TaggedTemplate { tag, quasi, .. } => {
            let (obj, name) = member_parts(ast, cx.src(), skip_parens(ast, tag))?;
            if name != "raw"
                || !cx.is_global_ref(obj, "String")
                || !matches!(*cx.node(quasi), Node::TemplateLit { parts } if ast.list(parts).len() == 1)
            {
                return None;
            }
            let text = cx.text(quasi);
            text.get(1..text.len().saturating_sub(1)).map(str::to_owned)
        }
        _ => None,
    }
}

/// Flags valid for the ecmaVersion, each at most once, not both `u` and `v`.
fn valid_flags(flags: &str, ecma: u32) -> bool {
    let mut seen = String::new();
    for c in flags.chars() {
        let since = match c {
            'g' | 'i' | 'm' => 5,
            'u' | 'y' => 2015,
            's' => 2018,
            'd' => 2022,
            'v' => 2024,
            _ => return false,
        };
        if since > ecma || seen.contains(c) {
            return false;
        }
        seen.push(c);
    }
    !(seen.contains('u') && seen.contains('v'))
}

/// ESLint's `resolveEscapes`: characters a literal must escape.
fn escaped(raw: &str) -> Option<&'static str> {
    Some(match raw {
        "\n" | "\\\n" => "\\n",
        "\r" | "\\\r" => "\\r",
        "\t" | "\\\t" => "\\t",
        "\u{b}" | "\\\u{b}" => "\\v",
        "\u{c}" | "\\\u{c}" => "\\f",
        "/" => "\\/",
        _ => return None,
    })
}

fn can_fix_to(cx: &Ctx<'_, '_>, node: NodeId, pattern: &str, flags: &str) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let span = ast.span(node);
    if ast
        .comments
        .iter()
        .any(|c| c.span.start >= span.start && c.span.end <= span.end)
    {
        return false;
    }
    let before = skip_trivia_backward(ast, src, span.start as usize);
    let head = src.get(..before).unwrap_or("");
    let ok_before = head.is_empty()
        || if head.ends_with(is_ident_char) {
            let word_start = head
                .char_indices()
                .rev()
                .take_while(|&(_, c)| is_ident_char(c))
                .last()
                .map_or(before, |(i, _)| i);
            PRECEDING_WORDS.contains(&&head[word_start..])
        } else {
            !head.ends_with("++")
                && !head.ends_with("--")
                && PRECEDING_PUNCT.iter().any(|p| head.ends_with(p))
        };
    let ecma = crate::globals::normalize_ecma_version(cx.env().ecma_version).max(5);
    ok_before
        && valid_flags(flags, ecma)
        && regex::parse(pattern, regex::Flags::new(flags)).is_ok_and(|re| {
            // Modifier groups `(?i:...)` are ES2025.
            ecma >= 2025
                || !re.nodes.iter().enumerate().any(|(i, n)| {
                    matches!(n.kind, regex::RKind::Group)
                        && re.text(i as u32).starts_with("(?")
                        && !re.text(i as u32).starts_with("(?:")
                })
        })
}

/// The replacement text, spaced from tokens it would merge with.
fn safe_output(cx: &Ctx<'_, '_>, node: NodeId, lit: &str) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let span = ast.span(node);
    let before = skip_trivia_backward(ast, src, span.start as usize);
    let space_before = before == span.start as usize && src[..before].ends_with('/');
    let next = src.get(span.end as usize..).and_then(|r| r.chars().next());
    let space_after = next.is_some_and(is_ident_char);
    format!(
        "{}{lit}{}",
        if space_before { " " } else { "" },
        if space_after { " " } else { "" }
    )
}

impl Rule for PreferRegexLiterals {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args) = match *cx.node(node) {
            Node::New { callee, args, .. } | Node::Call { callee, args, .. } => (callee, args),
            _ => return,
        };
        let callee = skip_parens(ast, callee);
        let is_regexp = cx.is_global_ref(callee, "RegExp")
            || member_parts(ast, cx.src(), callee).is_some_and(|(obj, name)| {
                name == "RegExp"
                    && ["globalThis", "self", "window", "global"]
                        .iter()
                        .any(|g| cx.is_global_ref(obj, g))
            });
        if !is_regexp {
            return;
        }
        let args: Vec<NodeId> = ast
            .list(args)
            .iter()
            .map(|&a| skip_parens(ast, a))
            .collect();
        let span = ast.span(node);
        let regex_arg = args.first().and_then(|&a| match *cx.node(a) {
            Node::RegexLit { pattern, flags } => Some((a, ast.atom(pattern), ast.atom(flags))),
            _ => None,
        });
        let wrapped = regex_arg.is_some()
            && (args.len() == 1 || (args.len() == 2 && string_value(cx, args[1]).is_some()));
        if self.disallow_redundant_wrapping
            && wrapped
            && let Some((lit, pattern, lit_flags)) = regex_arg
        {
            if args.len() == 2 {
                let arg_flags = string_value(cx, args[1]).unwrap_or_default();
                let mut merged = String::new();
                for c in lit_flags.chars().chain(arg_flags.chars()) {
                    if !merged.contains(c) {
                        merged.push(c);
                    }
                }
                let sorted = |s: &str| {
                    let mut v: Vec<char> = s.chars().collect();
                    v.sort_unstable();
                    v
                };
                let same = sorted(&merged) == sorted(&arg_flags);
                let first = can_fix_to(cx, node, pattern, &arg_flags)
                    .then(|| safe_output(cx, node, &format!("/{pattern}/{arg_flags}")));
                let second = (!same && can_fix_to(cx, node, pattern, &merged))
                    .then(|| safe_output(cx, node, &format!("/{pattern}/{merged}")));
                let mut r = cx
                    .report(
                        span,
                        "Use regular expression literal with flags instead of the 'RegExp' constructor.",
                    )
                    .id("unexpectedRedundantRegExpWithFlags");
                if let Some(out) = first {
                    r = r.suggest(
                        "replaceWithLiteralAndFlags",
                        format!("Replace with an equivalent regular expression literal with flags '{arg_flags}'."),
                        Fix::replace(span, out),
                    );
                }
                if let Some(out) = second {
                    r.suggest(
                        "replaceWithIntendedLiteralAndFlags",
                        format!("Replace with a regular expression literal with flags '{merged}'."),
                        Fix::replace(span, out),
                    );
                }
            } else {
                let fix = can_fix_to(cx, node, pattern, lit_flags)
                    .then(|| Fix::replace(span, safe_output(cx, node, cx.text(lit))));
                let r = cx
                    .report(
                        span,
                        "Regular expression literal is unnecessarily wrapped within a 'RegExp' constructor.",
                    )
                    .id("unexpectedRedundantRegExp");
                if let Some(fix) = fix {
                    r.suggest(
                        "replaceWithLiteral",
                        "Replace with an equivalent regular expression literal.",
                        fix,
                    );
                }
            }
            return;
        }
        if !(1..=2).contains(&args.len()) {
            return;
        }
        let Some(mut content) = string_value(cx, args[0]) else {
            return;
        };
        let flags = match args.get(1) {
            Some(&f) => match string_value(cx, f) {
                Some(f) => f,
                None => return,
            },
            None => String::new(),
        };
        let allowed = |c: char| {
            c.is_ascii_alphanumeric()
                || "-_\\[](){} \t\r\n\u{b}\u{c}!@#$%^&*+=/~`.><?,'\"|:;".contains(c)
        };
        let fixable = can_fix_to(cx, node, &content, &flags) && content.chars().all(allowed);
        if fixable && !content.is_empty() {
            let re = regex::parse(&content, regex::Flags::new(&flags));
            if let Ok(re) = re {
                let mut chars: Vec<(u32, u32)> = re
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| matches!(n.kind, regex::RKind::Char { .. }))
                    .map(|(i, _)| re.byte_range(i as u32))
                    .collect();
                chars.sort_unstable();
                let mut out = String::with_capacity(content.len());
                let mut at = 0usize;
                for (s, e) in chars {
                    let (s, e) = (s as usize, e as usize);
                    if s < at {
                        continue;
                    }
                    if let Some(esc) = content.get(s..e).and_then(escaped) {
                        out.push_str(&content[at..s]);
                        out.push_str(esc);
                        at = e;
                    }
                }
                out.push_str(&content[at..]);
                content = out;
            }
        }
        let lit = format!(
            "/{}/{flags}",
            if content.is_empty() { "(?:)" } else { &content }
        );
        let fix = fixable.then(|| Fix::replace(span, safe_output(cx, node, &lit)));
        let r = cx
            .report(
                span,
                "Use a regular expression literal instead of the 'RegExp' constructor.",
            )
            .id("unexpectedRegExp");
        if let Some(fix) = fix {
            r.suggest(
                "replaceWithLiteral",
                "Replace with an equivalent regular expression literal.",
                fix,
            );
        }
    }
}
