//! `jsdoc/ts-no-empty-object-type`: no `{}` object type in a tag type (typescript mode).
//!
//! What the plugin does, in our words: in `typescript` mode only, every tag that may carry
//! a type (except `@import`) and has one is parsed; a type that fails to parse is skipped,
//! and each object type with no members in it is one report on the tag's line. We don't
//! build the type tree: an object type with no members is a `{`, optional whitespace and
//! `}` outside quotes, and a type whose brackets or quotes don't balance counts as one
//! that fails to parse.

use super::prelude::*;
use crate::jsdoc_util::each_block;
use serde_json::Map;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/ts-no-empty-object-type",
        "Warns against use of the empty object type",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<TsNoEmptyObjectType>,
};

/// Tags whose type position typescript mode does not allow (`typeAllowed: false`).
const NO_TYPE_POSITION: &[&str] = &[
    "constructs",
    "emits",
    "export",
    "exports",
    "external",
    "fires",
    "function",
    "host",
    "interface",
    "listens",
    "mixin",
    "package",
    "private",
    "protected",
    "public",
    "requires",
    "static",
    "tutorial",
];

#[derive(Debug, Default)]
struct TsNoEmptyObjectType;

/// `tagMightHaveTypePosition`, with `settings.jsdoc.structuredTags` overriding the
/// defaults (a required type always may be there; otherwise a `type` of `false` forbids).
pub(crate) fn might_have_type(structured: Option<&Map<String, Value>>, tag: &str) -> bool {
    if let Some(s) = structured.and_then(|m| m.get(tag)) {
        let required = s
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|r| r.iter().any(|x| x.as_str() == Some("type")));
        return required
            || match s.get("type") {
                None => true,
                Some(Value::Bool(b)) => *b,
                Some(Value::Null) => false,
                Some(Value::String(t)) => !t.is_empty(),
                Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
                Some(_) => true,
            };
    }
    !NO_TYPE_POSITION.contains(&tag)
}

/// The number of member-less object types in `ty`, or 0 when its brackets or quotes don't
/// balance.
fn empty_objects(ty: &str) -> usize {
    let b = ty.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut count = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        None => return 0,
                        Some(b'\\') => j += 2,
                        Some(&c) if c == q => break,
                        Some(_) => j += 1,
                    }
                }
                i = j;
            }
            b'{' => {
                let rest = &ty[i + 1..];
                if rest.trim_start().starts_with('}') {
                    count += 1;
                }
                stack.push(b'}');
            }
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            b'<' => stack.push(b'>'),
            b'>' if i > 0 && b[i - 1] == b'=' => {}
            c @ (b'}' | b')' | b']' | b'>') => {
                if stack.pop() != Some(c) {
                    return 0;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if stack.is_empty() { count } else { 0 }
}

impl Rule for TsNoEmptyObjectType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let structured = cx
            .env()
            .settings
            .get("jsdoc")
            .and_then(|s| s.get("structuredTags"))
            .and_then(Value::as_object);
        each_block(cx, |cx, finder, doc| {
            if finder.settings.mode != "typescript" {
                return;
            }
            for (i, tag) in doc.block.tags.iter().enumerate() {
                if tag.tag == "import"
                    || tag.typ.is_empty()
                    || !might_have_type(structured, &tag.tag)
                {
                    continue;
                }
                for _ in 0..empty_objects(&tag.typ) {
                    let span = doc.line_span(finder, doc.block.tag_line(i));
                    cx.report(span, "No empty object type.");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_member_less_object_types() {
        assert_eq!(empty_objects("{}"), 1);
        assert_eq!(empty_objects("(string|{ })"), 1);
        assert_eq!(empty_objects("{a: {}, b: {}}"), 2);
        assert_eq!(empty_objects("{a: string}"), 0);
        assert_eq!(empty_objects("'{}'"), 0);
        assert_eq!(empty_objects("() => {}"), 1);
    }

    #[test]
    fn unbalanced_types_do_not_parse() {
        assert_eq!(empty_objects("BadType<{}"), 0);
        assert_eq!(empty_objects("{}}"), 0);
        assert_eq!(empty_objects("'{}"), 0);
        assert_eq!(empty_objects(&"{".repeat(100_000)), 0);
    }
}
