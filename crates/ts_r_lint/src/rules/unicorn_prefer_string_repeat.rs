//! `unicorn/prefer-string-repeat`: a string literal or substitution-free, untagged template
//! made of one whitespace character repeated at least `minimumRepetitions` times (default
//! 3) is fixed to `' '.repeat(n)` (the character written as `' '`, `\t`, `\n`, `\r`, `\f` or
//! `\u{HEX}`). Literals the syntax requires (keys, module names, types, directives, ...;
//! [`is_string_literal_required`]) and Jest inline snapshots are left alone. The call is
//! parenthesized as an unparenthesized `new` callee, and a glued keyword gets a space.

use super::prelude::*;
use super::unicorn_ast::{is_jest_inline_snapshot, is_js_space, is_string_literal_required};
use super::unicorn_util::{in_unparenthesized_new_callee, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-repeat",
        "Prefer `String#repeat()` for repeated whitespace.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferStringRepeat>,
};

#[derive(Debug)]
struct PreferStringRepeat {
    minimum: usize,
}

impl FromOptions for PreferStringRepeat {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let minimum = obj_opt(opts, 0)
            .get("minimumRepetitions")
            .and_then(Value::as_u64)
            .unwrap_or(3) as usize;
        Ok(PreferStringRepeat { minimum })
    }
}

fn quote(c: char) -> String {
    match c {
        ' ' => "' '".to_owned(),
        '\t' => r"'\t'".to_owned(),
        '\n' => r"'\n'".to_owned(),
        '\r' => r"'\r'".to_owned(),
        '\u{c}' => r"'\f'".to_owned(),
        c => format!("'\\u{{{:X}}}'", c as u32),
    }
}

impl Rule for PreferStringRepeat {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StringLit, NodeKind::TemplateLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let value = match *cx.node(node) {
            Node::StringLit { value } => ast.atom(value).to_owned(),
            Node::TemplateLit { parts } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                let parts = ast.list(parts);
                let [part] = parts else {
                    return;
                };
                match *ast.node(*part) {
                    Node::TemplateElement {
                        cooked,
                        invalid_escape: false,
                    } => ast.atom(cooked).to_owned(),
                    _ => return,
                }
            }
            _ => return,
        };
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            return;
        };
        let count = value.chars().count();
        if count < self.minimum || !is_js_space(first) || !chars.all(|c| c == first) {
            return;
        }
        if is_string_literal_required(cx, node) || is_jest_inline_snapshot(cx, node) {
            return;
        }
        let mut text = format!("{}.repeat({count})", quote(first));
        if in_unparenthesized_new_callee(cx, node) {
            text = format!("({text})");
        }
        let span = cx.span(node);
        let fix = with_edits(Fix::replace(span, text), space_around_keyword(cx, node));
        cx.report(span, "Use `String#repeat()` for repeated whitespace.")
            .id("prefer-string-repeat")
            .fix(fix);
    }
}
