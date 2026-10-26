//! `unicorn/no-zero-fractions`: no trailing zeros or dangling dot in a number's fraction
//! (`1.0`, `1.50`, `1.`), and a leading zero before a bare fraction (`.5`).
//!
//! The literal is split into integer digits, the dot with its fraction, and the rest (an
//! exponent); trailing `0`, `_` and `.` are dropped from the fraction. The report covers the
//! removed characters (or the fraction, for a missing leading zero). The fix rewrites the
//! literal; a result that is a plain integer used as a member object gets parentheses
//! (`(1).toFixed()`), with a leading `;` where the statement would join the previous one, and
//! a keyword written right against the literal (`return.5`) gets a space.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-zero-fractions",
        "Disallow number literals with zero fractions or dangling dots.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoZeroFractions>,
};

#[derive(Debug, Default)]
struct NoZeroFractions;

impl Rule for NoZeroFractions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let span = cx.span(node);
        let raw = span.text(cx.src());
        let Some(problem) = analyse(raw) else {
            return;
        };
        let (id, message) = if problem.leading_zero {
            (
                "leading-zero",
                "Use a leading zero before the decimal point.",
            )
        } else if problem.dangling_dot {
            ("dangling-dot", "Don't use a dangling dot in the number.")
        } else {
            ("zero-fraction", "Don't use a zero fraction in the number.")
        };
        let loc = Span::new(span.start + problem.start, span.start + problem.end);
        let fix = fix(cx, node, problem.formatted);
        cx.report(loc, message).id(id).fix(fix);
    }
}

struct Problem {
    formatted: String,
    start: u32,
    end: u32,
    leading_zero: bool,
    dangling_dot: bool,
}

fn analyse(raw: &str) -> Option<Problem> {
    let before_len = raw
        .bytes()
        .take_while(|c| c.is_ascii_digit() || *c == b'_')
        .count();
    let before = &raw[..before_len];
    let rest = &raw[before_len..];
    if !rest.starts_with('.') {
        return None;
    }
    let dot_len = 1 + rest[1..]
        .bytes()
        .take_while(|c| c.is_ascii_digit() || *c == b'_')
        .count();
    let dot_and_fraction = &rest[..dot_len];
    let after = &rest[dot_len..];
    let fixed = dot_and_fraction.trim_end_matches(['.', '0', '_']);
    let formatted = format!(
        "{}{fixed}{after}",
        if before.is_empty() { "0" } else { before }
    );
    if formatted == raw {
        return None;
    }
    let end = (before_len + dot_len) as u32;
    let leading_zero = before.is_empty() && !fixed.is_empty();
    let start = if leading_zero {
        0
    } else {
        end - raw.len().saturating_sub(formatted.len()) as u32
    };
    Some(Problem {
        formatted,
        start,
        end,
        leading_zero,
        dangling_dot: dot_and_fraction == ".",
    })
}

fn fix(cx: &Ctx<'_, '_>, node: NodeId, formatted: String) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let parent = cx.parent(node);
    let parenthesized = parent.is_some_and(|p| matches!(ast.node(p), Node::Paren { .. }));
    let member_object =
        parent.is_some_and(|p| matches!(*ast.node(p), Node::Member { obj, .. } if obj == node));
    let mut text = formatted;
    if member_object && !parenthesized && text.bytes().all(|c| c.is_ascii_digit()) {
        text = format!("({text})");
        if starts_continuable_statement(cx, node) {
            text.insert(0, ';');
        }
    }
    // The outermost parentheses around the literal.
    let mut outer = node;
    while let Some(p) = cx.parent(outer) {
        if !matches!(ast.node(p), Node::Paren { .. }) {
            break;
        }
        outer = p;
    }
    let range = cx.span(outer);
    let keyword_before = src[..range.start as usize]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphabetic());
    let span = cx.span(node);
    if keyword_before && outer == node {
        text.insert(0, ' ');
    }
    let mut fix = Fix::replace(span, text);
    if keyword_before && outer != node {
        fix = fix.and(Span::at(range.start), " ");
    }
    fix
}

/// `node` starts an expression statement that follows one a `(` would continue.
fn starts_continuable_statement(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let start = cx.span(node).start;
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        if cx.span(p).start != start {
            return false;
        }
        if matches!(ast.node(p), Node::ExprStmt { .. }) {
            return prev_statement(ast, cx.file.parents, p)
                .is_some_and(|prev| stmt_continuable(ast, cx.src(), prev));
        }
        cur = p;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::analyse;

    #[test]
    fn splits_fraction_and_exponent() {
        let p = analyse("123_000.000_400E-10").unwrap();
        assert_eq!(p.formatted, "123_000.000_4E-10");
        assert_eq!((p.start, p.end), (13, 15));
        let p = analyse(".50e2").unwrap();
        assert_eq!(p.formatted, "0.5e2");
        assert!(p.leading_zero);
        assert!(analyse("1.5").is_none());
        assert!(analyse("0x10").is_none());
    }
}
