//! `require-unicode-regexp`: regex literals and `RegExp` calls without the `u` (or `v`)
//! flag.

use super::prefer_regex_literals::is_global_regexp;
use super::prelude::*;
use super::unicorn_static::static_value;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "require-unicode-regexp",
        "Enforce the use of `u` or `v` flag on regular expressions",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<RequireUnicodeRegexp>,
};

#[derive(Debug)]
struct RequireUnicodeRegexp {
    /// `Some('u')` / `Some('v')`, or `None` for either.
    require: Option<char>,
}

impl FromOptions for RequireUnicodeRegexp {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(RequireUnicodeRegexp {
            require: obj_opt(opts, 0)
                .get("requireFlag")
                .and_then(Value::as_str)
                .and_then(|s| s.chars().next()),
        })
    }
}

impl RequireUnicodeRegexp {
    fn missing(&self, flags: &str) -> bool {
        match self.require {
            Some(f) => !flags.contains(f),
            None => !flags.contains('u') && !flags.contains('v'),
        }
    }

    fn valid_with_flag(&self, cx: &Ctx<'_, '_>, pattern: &str) -> bool {
        let ecma = crate::globals::normalize_ecma_version(cx.env().ecma_version);
        let flag = self.require.unwrap_or('u');
        if (flag == 'u' && ecma <= 5) || (flag == 'v' && ecma <= 2023) {
            return false;
        }
        regex::parse(pattern, regex::Flags::new(&flag.to_string())).is_ok()
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, span: Span, fix: Option<Fix>) {
        let (id, msg, sid, desc) = if self.require == Some('v') {
            (
                "requireVFlag",
                "Use the 'v' flag.",
                "addVFlag",
                "Add the 'v' flag.",
            )
        } else {
            (
                "requireUFlag",
                "Use the 'u' flag.",
                "addUFlag",
                "Add the 'u' flag.",
            )
        };
        let r = cx.report(span, msg).id(id);
        if let Some(fix) = fix {
            r.suggest(sid, desc, fix);
        }
    }
}

/// ESLint's `getStringIfConstant`.
fn string_if_constant(cx: &Ctx<'_, '_>, n: NodeId) -> Option<String> {
    static_value(cx, n)?.to_js_string()
}

impl Rule for RequireUnicodeRegexp {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit, NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let replace = self.require.unwrap_or('u');
        let other = if self.require == Some('u') { 'v' } else { 'u' };
        match *cx.node(node) {
            Node::RegexLit { pattern, flags } => {
                if !self.missing(ast.atom(flags)) {
                    return;
                }
                let span = ast.span(node);
                let fix = self.valid_with_flag(cx, ast.atom(pattern)).then(|| {
                    let text = cx.text(node);
                    let slash = text.rfind('/').unwrap_or(0);
                    match text[slash..].find(other) {
                        Some(i) if self.require.is_some() => {
                            let at = span.start + (slash + i) as u32;
                            Fix::replace(Span::new(at, at + 1), replace.to_string())
                        }
                        _ => Fix::insert(span.end, replace.to_string()),
                    }
                });
                self.report(cx, span, fix);
            }
            Node::New { callee, args, .. } | Node::Call { callee, args, .. } => {
                if !is_global_regexp(cx, callee) {
                    return;
                }
                let args = ast.list(args);
                let pattern_node = args.first().copied();
                if pattern_node.is_some_and(|p| matches!(cx.node(p), Node::Spread { .. })) {
                    return;
                }
                let flags_node = args.get(1).copied();
                let pattern = pattern_node.and_then(|p| string_if_constant(cx, p));
                let flags = flags_node.and_then(|f| string_if_constant(cx, f));
                let missing = match &flags {
                    Some(f) => self.missing(f),
                    None => flags_node.is_none(),
                };
                if !missing {
                    return;
                }
                let span = ast.span(node);
                let suggest = pattern
                    .as_deref()
                    .is_some_and(|p| self.valid_with_flag(cx, p));
                let fix = suggest
                    .then(|| self.call_fix(cx, node, flags_node, flags.as_deref()))
                    .flatten();
                self.report(cx, span, fix);
            }
            _ => {}
        }
    }
}

impl RequireUnicodeRegexp {
    fn call_fix(
        &self,
        cx: &Ctx<'_, '_>,
        call: NodeId,
        flags_node: Option<NodeId>,
        flags: Option<&str>,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let replace = self.require.unwrap_or('u');
        let other = if self.require == Some('u') { 'v' } else { 'u' };
        let Some(f) = flags_node else {
            // After the last argument, before the closing `)`.
            let end = ast.span(call).end as usize;
            let close = skip_trivia_backward(ast, src, end).saturating_sub(1);
            let last = skip_trivia_backward(ast, src, close);
            return Some(if src.as_bytes().get(last.wrapping_sub(1)) == Some(&b',') {
                Fix::insert(last as u32, format!(" \"{replace}\","))
            } else {
                Fix::insert(last as u32, format!(", \"{replace}\""))
            });
        };
        let f = skip_parens(ast, f);
        let template = match *cx.node(f) {
            Node::StringLit { .. } => false,
            Node::TemplateLit { .. } => true,
            _ => return None,
        };
        let text = cx.text(f);
        let span = ast.span(f);
        if flags.is_some_and(|fl| fl.contains(other)) {
            if text.contains('\\') || (template && text.contains("${")) {
                return None;
            }
            let i = text.find(other)?;
            let at = span.start + i as u32;
            return Some(Fix::replace(Span::new(at, at + 1), replace.to_string()));
        }
        Some(Fix::insert(span.end - 1, replace.to_string()))
    }
}
