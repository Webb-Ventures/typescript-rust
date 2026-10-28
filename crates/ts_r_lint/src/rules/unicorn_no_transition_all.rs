//! `unicorn/no-transition-all`: `style.transition = 'all ...'`, `style.transitionProperty =
//! '..., all'` and `style.setProperty('transition', 'all')` animate every property. The
//! receiver must be typed as the DOM's `CSSStyleDeclaration` or `CSSStyleProperties`, so
//! without type information nothing is reported (the plugin's CSS-file half needs a CSS
//! language, which ts-r-lint does not lint).

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::static_string_value;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-transition-all",
        "Disallow `all` as a transition property.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoTransitionAll>,
};

#[derive(Debug, Default)]
struct NoTransitionAll;

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    static_string_value(ast, skip_wrappers(ast, id))
}

/// A CSS identifier with its escapes decoded (`\61 ll` is `all`), or `None` when `token`
/// is not an identifier.
fn css_ident(token: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = token.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut hex = String::new();
            while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
                hex.extend(chars.next());
            }
            if hex.is_empty() {
                out.push(chars.next()?);
            } else {
                if chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                    chars.next();
                }
                let v = u32::from_str_radix(&hex, 16).ok()?;
                out.push(
                    char::from_u32(v)
                        .filter(|&c| c != '\0')
                        .unwrap_or('\u{fffd}'),
                );
            }
        } else if c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
            out.push(c);
        } else {
            return None;
        }
    }
    let first = out.chars().next()?;
    (!first.is_ascii_digit()).then_some(out)
}

/// The value lists `all` as one of its top-level components (not inside a function such as
/// `var(--x, all)`). A value that does not parse as a plain value (`!important`) does not.
fn has_transition_all(value: &str) -> bool {
    let b = value.as_bytes();
    let (mut depth, mut start, mut i) = (0u32, 0, 0);
    let mut found = false;
    let mut flush = |from: usize, to: usize, depth: u32| {
        if depth == 0 && from < to {
            let token = &value[from..to];
            if css_ident(token).is_some_and(|t| t.eq_ignore_ascii_case("all")) {
                found = true;
            }
        }
    };
    while i < b.len() {
        match b[i] {
            b'\\' => {
                // An escape: one character, or up to six hex digits and one optional space.
                let hex = b[i + 1..]
                    .iter()
                    .take(6)
                    .take_while(|c| c.is_ascii_hexdigit())
                    .count();
                i += hex.max(1);
                if hex > 0 && b.get(i + 1).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
            }
            b'!' | b';' | b'{' | b'}' => return false,
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                if i >= b.len() {
                    return false;
                }
            }
            b'(' | b'[' => {
                flush(start, i, depth);
                depth += 1;
                start = i + 1;
            }
            b')' | b']' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                start = i + 1;
            }
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b',' | b'/' if depth == 0 => {
                flush(start, i, depth);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    flush(start, b.len().min(value.len()), depth);
    depth == 0 && found
}

fn is_valid_priority(ast: &Ast, priority: Option<NodeId>) -> bool {
    let Some(p) = priority else {
        return true;
    };
    if let Some(s) = static_string(ast, p) {
        return s.is_empty() || s.eq_ignore_ascii_case("important");
    }
    !matches!(
        ast.node(skip_wrappers(ast, p)),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BoolLit { .. }
            | Node::BigIntLit { .. }
            | Node::RegexLit { .. }
    )
}

fn is_style_declaration(ti: &dyn TypeInfo, t: Ty, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    if ti.flags(t).contains(TypeFlags::UNION) {
        let members: Vec<Ty> = ti
            .union_members(t)
            .into_iter()
            .filter(|&m| {
                !ti.flags(m)
                    .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID)
            })
            .collect();
        return !members.is_empty()
            && members
                .iter()
                .all(|&m| is_style_declaration(ti, m, depth + 1));
    }
    ti.type_symbol(t).is_some_and(|s| {
        matches!(
            ti.symbol_name(s).as_str(),
            "CSSStyleDeclaration" | "CSSStyleProperties"
        ) && is_lib_symbol(ti, s)
    })
}

impl NoTransitionAll {
    fn check(&self, cx: &mut Ctx<'_, '_>, receiver: NodeId, value: NodeId) {
        let ast = cx.ast();
        let ti = cx.types();
        let hit = static_string(ast, value).is_some_and(|v| has_transition_all(&v))
            && ti
                .type_at(receiver)
                .is_some_and(|t| is_style_declaration(ti, t, 0));
        if hit {
            cx.report(
                cx.span(skip_parens(ast, value)),
                "Do not use `all` as a transition property.",
            )
            .id("no-transition-all");
        }
    }
}

impl Rule for NoTransitionAll {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !cx.types().is_available() {
            return;
        }
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => {
                let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, left)) else {
                    return;
                };
                if matches!(
                    ident_name(ast, prop),
                    Some("transition" | "transitionProperty")
                ) {
                    self.check(cx, obj, right);
                }
            }
            Node::Call { .. } => {
                let Some(call) = method_call(ast, node).filter(|m| m.name == "setProperty") else {
                    return;
                };
                let args: Vec<NodeId> = call.args.iter().map(|&a| skip_parens(ast, a)).collect();
                if args.len() < 2
                    || args
                        .iter()
                        .take(3)
                        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
                {
                    return;
                }
                let property = static_string(ast, args[0]).map(|s| s.to_lowercase());
                if matches!(
                    property.as_deref(),
                    Some("transition" | "transition-property")
                ) && is_valid_priority(ast, args.get(2).copied())
                {
                    self.check(cx, call.obj, args[1]);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::has_transition_all;

    #[test]
    fn values() {
        assert!(has_transition_all("all"));
        assert!(has_transition_all("opacity 1s, ALL 150ms"));
        assert!(has_transition_all("\\61 ll 150ms"));
        assert!(!has_transition_all("var(--transition, all)"));
        assert!(!has_transition_all("opacity 150ms"));
        assert!(!has_transition_all("all !important"));
        assert!(!has_transition_all("allow"));
    }
}
