//! `@typescript-eslint/no-unnecessary-template-expression`: no `${}` around a lone string
//! or around a literal that can be written into the template directly.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-template-expression",
        "Disallow unnecessary template expressions",
        TS_STRICT_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<NoUnnecessaryTemplateExpression>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryTemplateExpression;

const MESSAGE: &str = "Template literal expression is unnecessary and can be simplified.";

impl Rule for NoUnnecessaryTemplateExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TemplateLit, NodeKind::TemplateLitType]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (parts, is_type) = match *cx.node(node) {
            Node::TemplateLit { parts } => (parts, false),
            Node::TemplateLitType { parts } => (parts, true),
            _ => return,
        };
        if cx
            .parent(node)
            .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
        {
            return;
        }
        let ast = cx.ast();
        let src = cx.src();
        let list = ast.list(parts);
        let elements: Vec<NodeId> = list.iter().copied().step_by(2).collect();
        let exprs: Vec<NodeId> = list.iter().copied().skip(1).step_by(2).collect();
        if elements.len() != exprs.len() + 1 {
            return;
        }
        let spans: Vec<Span> = elements.iter().map(|&el| cx.span(el)).collect();
        let raws: Vec<&str> = spans.iter().map(|s| s.text(src)).collect();
        let raw = |i: usize| raws[i];
        let interp = |i: usize| Span::new(spans[i].end, spans[i + 1].start);

        // `${str}` alone: the expression itself.
        if exprs.len() == 1
            && raw(0).is_empty()
            && raw(1).is_empty()
            && ast.comments_in(interp(0)).is_empty()
            && is_string_typed(cx, exprs[0], is_type)
        {
            let e = exprs[0];
            let es = cx.span(e);
            let text = es.text(src);
            let text = if !is_type && needs_parens(cx, node, e) {
                format!("({text})")
            } else {
                text.to_string()
            };
            let fix = Fix::replace(cx.span(node), text);
            let span = Span::new(
                es.start.saturating_sub(2),
                (es.end + 1).min(src.len() as u32),
            );
            cx.report(span, MESSAGE)
                .id("noUnnecessaryTemplateExpression")
                .fix(fix);
            return;
        }

        let inlines: Vec<Option<Inline>> =
            exprs.iter().map(|&e| inline(cx, e, is_type, 0)).collect();
        for (i, il) in inlines.iter().enumerate() {
            let Some(il) = il else { continue };
            let span = interp(i);
            if !ast.comments_in(span).is_empty() {
                continue;
            }
            let next = raw(i + 1);
            let prev = raw(i);
            // Whitespace that ends a line stays visible when it is interpolated.
            if next.starts_with(['\n', '\r', '\u{2028}', '\u{2029}'])
                && let Some(v) = &il.value
                && v.chars().all(char::is_whitespace)
                && (!v.is_empty() || prev.ends_with([' ', '\t']))
            {
                continue;
            }
            let following = following_text(&elements, &inlines, i, |j| raw(j));
            let mut text = il.text.clone();
            if ends_with_unescaped_dollar(&text) && following.starts_with('{') {
                text.insert(text.len() - 1, '\\');
            }
            let starts = format!("{}{following}", il.value.as_deref().unwrap_or(&il.text));
            let mut fix_span = span;
            if ends_with_unescaped_dollar(prev) && starts.starts_with('{') {
                fix_span = Span::new(span.start - 1, span.end);
                text.insert_str(0, "\\$");
            }
            let fix = Fix::replace(fix_span, text);
            cx.report(span, MESSAGE)
                .id("noUnnecessaryTemplateExpression")
                .fix(fix);
        }
    }
}

/// A literal written into the template: its escaped template text, and its value when known.
struct Inline {
    text: String,
    value: Option<String>,
}

fn inline(cx: &Ctx<'_, '_>, e: NodeId, is_type: bool, depth: u32) -> Option<Inline> {
    if depth > 16 {
        return None;
    }
    let ast = cx.ast();
    let src = cx.src();
    let e = if is_type { e } else { skip_parens(ast, e) };
    let plain = |s: String| {
        Some(Inline {
            text: s.clone(),
            value: Some(s),
        })
    };
    match *cx.node(e) {
        Node::StringLit { value } => {
            let t = cx.span(e).text(src);
            let raw = t.get(1..t.len().saturating_sub(1))?;
            if has_legacy_escape(raw) {
                return None;
            }
            Some(Inline {
                text: escape_string_raw(raw),
                value: Some(ast.atom(value).to_string()),
            })
        }
        Node::NumberLit { .. } | Node::BigIntLit { .. } => {
            plain(static_value_string(ast, src, e)?.into_owned())
        }
        Node::BoolLit { value } => plain(value.to_string()),
        Node::NullLit {} => plain("null".to_owned()),
        Node::KeywordType {
            kind: KeywordTypeKind::Null,
        } if is_type => plain("null".to_owned()),
        Node::KeywordType {
            kind: KeywordTypeKind::Undefined,
        } if is_type => plain("undefined".to_owned()),
        Node::Ident { .. } if !is_type => {
            let name = ident_name(ast, e)?;
            if !matches!(name, "undefined" | "Infinity" | "NaN") || !is_global(cx, e) {
                return None;
            }
            plain(name.to_owned())
        }
        Node::RegexLit { .. } if !is_type => {
            // A regexp converts to `/source/flags` with its flags in canonical order.
            let t = cx.span(e).text(src);
            let (pattern, flags) = t.rsplit_once('/')?;
            let mut v = format!("{pattern}/");
            v.extend("dgimsuvy".chars().filter(|&c| flags.contains(c)));
            if v.len() != t.len() {
                return None;
            }
            Some(Inline {
                text: escape_value(&v),
                value: Some(v),
            })
        }
        Node::TemplateLit { parts } | Node::TemplateLitType { parts } => {
            let list = ast.list(parts);
            let mut value = Some(String::new());
            for (k, &p) in list.iter().enumerate() {
                if k % 2 == 0 {
                    let Node::TemplateElement {
                        cooked,
                        invalid_escape,
                    } = *cx.node(p)
                    else {
                        return None;
                    };
                    if invalid_escape {
                        return None;
                    }
                    if let Some(v) = &mut value {
                        v.push_str(ast.atom(cooked));
                    }
                } else {
                    let inner = inline(cx, p, is_type, depth + 1);
                    match (&mut value, inner.and_then(|i| i.value)) {
                        (Some(v), Some(iv)) => v.push_str(&iv),
                        _ => value = None,
                    }
                }
            }
            let t = cx.span(e).text(src);
            let raw = t.get(1..t.len().saturating_sub(1))?;
            Some(Inline {
                text: raw.to_string(),
                value,
            })
        }
        _ => None,
    }
}

/// The text after interpolation `i` once following literals are written in (as far as it
/// is known).
fn following_text<'s>(
    elements: &[NodeId],
    inlines: &[Option<Inline>],
    i: usize,
    raw: impl Fn(usize) -> &'s str,
) -> String {
    let mut s = String::new();
    let mut j = i + 1;
    while j < elements.len() {
        s.push_str(raw(j));
        if !s.is_empty() {
            break;
        }
        match inlines.get(j).and_then(|il| il.as_ref()) {
            Some(Inline { value: Some(v), .. }) => s.push_str(v),
            _ => break,
        }
        if !s.is_empty() {
            break;
        }
        j += 1;
    }
    s
}

/// `\1`, `\01`: octal escapes, which templates reject.
fn has_legacy_escape(raw: &str) -> bool {
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let n = b.get(i + 1).copied().unwrap_or(0);
            if (b'1'..=b'9').contains(&n)
                || (n == b'0' && b.get(i + 2).is_some_and(u8::is_ascii_digit))
            {
                return true;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// A string literal's source text, rewritten for a template: escapes stay, backticks and
/// `${` get escaped.
fn escape_string_raw(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push('\\');
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '`' => out.push_str("\\`"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out
}

/// A value written into a template so it reads back unchanged.
fn escape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out
}

/// The text ends in a `$` that no backslash escapes.
fn ends_with_unescaped_dollar(s: &str) -> bool {
    let Some(rest) = s.strip_suffix('$') else {
        return false;
    };
    let slashes = rest.bytes().rev().take_while(|&b| b == b'\\').count();
    slashes % 2 == 0
}

/// `ident` names a global (no declaration outside the default library).
fn is_global(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ti = cx.types();
    match ti.symbol_at(ident) {
        None => true,
        Some(s) => ti.symbol_decls(s).iter().all(|d| d.in_lib),
    }
}

/// Every part of the type is a string (type positions: a string type that is not an enum or
/// a type parameter, which the template would change).
fn is_string_typed(cx: &Ctx<'_, '_>, e: NodeId, is_type: bool) -> bool {
    let ti = cx.types();
    let t = if is_type {
        ti.type_of_type_node(e)
    } else {
        ti.type_at(e)
    };
    let Some(t) = t else { return false };
    if has_unresolved_part(ti, t) || is_any(ti, t) {
        return false;
    }
    let t = if is_type { t } else { constrained(ti, t) };
    let ps = parts(ti, t);
    !ps.is_empty()
        && ps.iter().all(|&p| {
            let f = ti.flags(p);
            if is_type {
                f.intersects(TypeFlags::STRING_LIKE)
                    && !f.intersects(TypeFlags::ENUM_LIKE | TypeFlags::INSTANTIABLE)
            } else if f.contains(TypeFlags::INTERSECTION) {
                ti.intersection_members(p).into_iter().any(|m| {
                    ti.flags(constrained(ti, m))
                        .intersects(TypeFlags::STRING_LIKE)
                })
            } else {
                f.intersects(TypeFlags::STRING_LIKE)
            }
        })
}

/// The expression replacing the template needs parentheses where the template stood.
fn needs_parens(cx: &Ctx<'_, '_>, template: NodeId, e: NodeId) -> bool {
    let low = matches!(
        cx.node(e),
        Node::Binary { .. }
            | Node::Assign { .. }
            | Node::Conditional { .. }
            | Node::Seq { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::Arrow { .. }
            | Node::Yield { .. }
            | Node::Await { .. }
            | Node::Unary { .. }
    );
    low && cx.parent(template).is_some_and(|p| match *cx.node(p) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == template,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == template,
        Node::TaggedTemplate { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Binary { .. }
        | Node::Await { .. } => true,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn regexp_flags_are_inlined_in_canonical_order() {
        let rules = r#"{"@typescript-eslint/no-unnecessary-template-expression": "error"}"#;
        let code = "export const a = `${/a/ig}`;\nexport const b = `${/[/]x/ysd}`;\n";
        let Some(r) = crate::typed::lint_rules_for_test(code, rules) else {
            return;
        };
        let fixes: Vec<_> = r
            .diagnostics
            .iter()
            .filter_map(|d| d.fix.as_ref())
            .collect();
        let fixed = crate::fix::apply_fixes(code, &fixes).0;
        assert_eq!(
            fixed,
            "export const a = `/a/gi`;\nexport const b = `/[/]x/dsy`;\n"
        );
    }
}
