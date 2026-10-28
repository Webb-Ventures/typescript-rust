//! `unicorn/consistent-template-literal-escape`: in an untagged template literal (or
//! template literal type), `$\{` and `\$\{` that aren't themselves escaped are written
//! `\${`. Reported per template element (with its delimiters); the fix rewrites the raw
//! text.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-template-literal-escape",
        "Enforce consistent style for escaping `${` in template literals.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ConsistentTemplateLiteralEscape>,
};

#[derive(Debug, Default)]
struct ConsistentTemplateLiteralEscape;

/// `raw` with every unescaped `$\{` / `\$\{` turned into `\${`.
fn fixed(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest
            .strip_prefix(r"\$\{")
            .or_else(|| rest.strip_prefix(r"$\{"))
        {
            out.push_str(r"\${");
            rest = r;
        } else if c == '\\' {
            // An escape pair: the next character can't start a match.
            let len = 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
            out.push_str(&rest[..len]);
            rest = &rest[len..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

impl Rule for ConsistentTemplateLiteralEscape {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TemplateLit, NodeKind::TemplateLitType]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parts = match *cx.node(node) {
            Node::TemplateLit { parts } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                parts
            }
            Node::TemplateLitType { parts } => parts,
            _ => return,
        };
        let parts = ast.list(parts);
        let last = parts.len().saturating_sub(1);
        for (i, &part) in parts.iter().enumerate() {
            if i % 2 == 1 {
                continue;
            }
            let raw_span = cx.span(part);
            let raw = raw_span.text(cx.src());
            let new = fixed(raw);
            if new == raw {
                continue;
            }
            let close = if i == last { 1 } else { 2 };
            let span = Span::new(raw_span.start.saturating_sub(1), raw_span.end + close);
            cx.report(
                span,
                "Use `\\${` instead of `$\\{` to escape in template literals.",
            )
            .id("consistent-template-literal-escape")
            .fix(Fix::replace(raw_span, new));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixed;

    #[test]
    fn rewrites_only_unescaped_forms() {
        assert_eq!(fixed(r"a$\{b"), r"a\${b");
        assert_eq!(fixed(r"\$\{"), r"\${");
        assert_eq!(fixed(r"\\$\{"), r"\\\${");
        assert_eq!(fixed(r"\${"), r"\${");
        assert_eq!(fixed("é\\"), "é\\");
    }
}
