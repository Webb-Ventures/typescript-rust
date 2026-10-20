//! `@typescript-eslint/prefer-regexp-exec`: `re.exec(s)` over `s.match(re)` when `re` has
//! no global flag.

use super::prelude::*;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-regexp-exec",
        "Enforce `RegExp#exec` over `String#match` if no global flag is provided",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<PreferRegexpExec>,
};

#[derive(Debug, Default)]
struct PreferRegexpExec;

/// What the argument of `match` statically is.
enum Arg {
    /// A regular expression without the `g` flag (a literal or a `RegExp` construction).
    Regex,
    /// A string that is a valid pattern.
    Str(String),
}

impl Rule for PreferRegexpExec {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let [arg] = ast.list(args)[..] else {
            return;
        };
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = *cx.node(callee)
        else {
            return;
        };
        if ident_name(ast, prop) != Some("match") {
            return;
        }
        let ti = cx.types();
        let Some(ot) = ti.type_at(obj) else {
            return;
        };
        if !is_string_like_type(ti, ot) {
            return;
        }
        let Some(kind) = classify(cx, arg, 0) else {
            return;
        };
        let src = cx.src();
        let arg_inner = skip_parens(ast, arg);
        let obj_inner = skip_parens(ast, obj);
        let obj_text = if matches!(cx.node(obj_inner), Node::Seq { .. }) {
            format!("({})", cx.span(obj_inner).text(src))
        } else {
            cx.span(obj_inner).text(src).to_string()
        };
        let regex_text = match kind {
            Arg::Regex => {
                let t = cx.span(arg_inner).text(src);
                if needs_member_parens(ast, arg_inner) {
                    format!("({t})")
                } else {
                    t.to_string()
                }
            }
            Arg::Str(pattern) => {
                if matches!(cx.node(arg_inner), Node::StringLit { .. }) {
                    format!("/{}/", literal_source(&pattern))
                } else {
                    format!("RegExp({})", cx.span(arg_inner).text(src))
                }
            }
        };
        let text = format!("{regex_text}.exec({obj_text})");
        let span = cx.span(node);
        cx.report_node(prop, "Use the `RegExp#exec()` method instead.")
            .id("regExpExecOverStringMatch")
            .fix(Fix::replace(span, text));
    }
}

fn classify(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> Option<Arg> {
    if depth > 8 {
        return None;
    }
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *cx.node(e) {
        Node::RegexLit { flags, .. } => (!ast.atom(flags).contains('g')).then_some(Arg::Regex),
        Node::New { .. } | Node::Call { .. } => {
            let args = regexp_call_args(cx, e, false, false)?;
            match args {
                [] => Some(Arg::Regex),
                [pattern] => lends_no_global(cx, *pattern).then_some(Arg::Regex),
                [pattern, flags] => {
                    let flags = skip_parens(ast, *flags);
                    if is_undefined(ast, flags) && cx.is_global_ref(flags, "undefined") {
                        return lends_no_global(cx, *pattern).then_some(Arg::Regex);
                    }
                    let f = static_string_eval(cx, flags)?;
                    (!f.contains('g')).then_some(Arg::Regex)
                }
                _ => None,
            }
        }
        Node::StringLit { .. } | Node::TemplateLit { .. } => {
            let s = static_string_eval(cx, e)?;
            regex::parse(&s, regex::Flags::default()).ok()?;
            Some(Arg::Str(s))
        }
        Node::Ident { .. } => classify(cx, const_init(cx, e)?, depth + 1),
        _ => None,
    }
}

/// Whether `RegExp(pattern)` without flags is known to lack `g`: a regex pattern lends its
/// own flags, so it must be a string or a regex literal without `g`.
fn lends_no_global(cx: &Ctx<'_, '_>, pattern: NodeId) -> bool {
    let pattern = skip_parens(cx.ast(), pattern);
    if let Node::RegexLit { flags, .. } = *cx.node(pattern) {
        return !cx.ast().atom(flags).contains('g');
    }
    let ti = cx.types();
    ti.type_at(pattern)
        .is_some_and(|t| is_string_like_type(ti, t))
}

/// A pattern as the body of a regex literal, as `RegExp(p).toString()` writes it: `/`
/// escaped, line terminators as escapes, and `(?:)` for the empty pattern.
fn literal_source(p: &str) -> String {
    if p.is_empty() {
        return "(?:)".to_owned();
    }
    let mut out = String::with_capacity(p.len());
    let mut escaped = false;
    for ch in p.chars() {
        let line = match ch {
            '\n' => Some("n"),
            '\r' => Some("r"),
            '\u{2028}' => Some("u2028"),
            '\u{2029}' => Some("u2029"),
            _ => None,
        };
        if let Some(l) = line {
            // After a `\` only the escape's letters are left to write.
            if !escaped {
                out.push('\\');
            }
            out.push_str(l);
            escaped = false;
            continue;
        }
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '/' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn needs_member_parens(ast: &ts_r_ast::Ast, e: NodeId) -> bool {
    !matches!(
        *ast.node(e),
        Node::Ident { .. }
            | Node::RegexLit { .. }
            | Node::Paren { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::Call { .. }
            | Node::New { .. }
    )
}

#[cfg(test)]
mod tests {
    fn fixed(code: &str) -> Option<String> {
        crate::typed::lint_for_test(super::RULE.meta.name, code).map(|(_, out)| out)
    }

    #[test]
    fn string_patterns_serialize_like_regexp_to_string() {
        let Some(out) = fixed("const a = 'abc'.match('');\nconst b = 'a\\nb'.match('\\n');\n")
        else {
            return;
        };
        assert_eq!(
            out,
            "const a = /(?:)/.exec('abc');\nconst b = /\\n/.exec('a\\nb');\n"
        );
    }

    #[test]
    fn regex_patterns_lend_their_global_flag() {
        let code = "declare const re: RegExp;\n\
                    'aba'.match(new RegExp(/a/g));\n\
                    'aba'.match(new RegExp(/a/g, undefined));\n\
                    'aba'.match(RegExp(re));\n\
                    'aba'.match(new RegExp(/a/));\n";
        let Some((msgs, _)) = crate::typed::lint_for_test(super::RULE.meta.name, code) else {
            return;
        };
        assert_eq!(msgs.len(), 1, "{msgs:?}");
    }
}
