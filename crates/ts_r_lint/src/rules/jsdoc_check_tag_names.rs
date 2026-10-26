//! `jsdoc/check-tag-names`: block tags are ones the mode knows (or the options and
//! settings add), under their preferred name, and inline tags are on an allow-list.
//!
//! What the plugin does, in our words:
//! - Every block is visited once. Per tag: JSX pragma tags pass with `jsxTags`; with
//!   `typed`, tags a type system makes redundant are reported (always, outside an ambient
//!   `declare`/`.d.ts` context, or `@template` without a name) with a fix that drops the
//!   tag (or, when it has a description, just its type and maybe its name).
//! - Otherwise the tag must be a name or alias of the mode's table, or come from
//!   `definedTags`, `tagNamePreference` (keys and values) or `structuredTags`. A known tag
//!   is then mapped to its preferred name (a preference value is kept, a preference key
//!   gives its value, an alias gives its canonical name); a different name is reported
//!   with a fix renaming the first `@tag` in the comment. A `false` preference (or an
//!   object without `replacement`) blocks the tag.
//! - Inline `{@tag}`s, block description first then each tag's, must be in `inlineTags`.

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::jsdoc::{Doc, Finder, Target};
use crate::jsdoc_tree::inline_tag_names;
use crate::jsdoc_util::{each_block, removal_fix, remove_tag_with};
use serde_json::Map;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-tag-names",
        "Reports invalid block tag names.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<CheckTagNames>,
};

type Entry = (&'static str, &'static [&'static str]);

const JSDOC_TAGS: &[Entry] = &[
    ("modifies", &[]),
    ("abstract", &["virtual"]),
    ("access", &[]),
    ("alias", &[]),
    ("async", &[]),
    ("augments", &["extends"]),
    ("author", &[]),
    ("borrows", &[]),
    ("callback", &[]),
    ("class", &["constructor"]),
    ("classdesc", &[]),
    ("constant", &["const"]),
    ("constructs", &[]),
    ("copyright", &[]),
    ("default", &["defaultvalue"]),
    ("deprecated", &[]),
    ("description", &["desc"]),
    ("enum", &[]),
    ("event", &[]),
    ("example", &[]),
    ("exports", &[]),
    ("external", &["host"]),
    ("file", &["fileoverview", "overview"]),
    ("fires", &["emits"]),
    ("function", &["func", "method"]),
    ("generator", &[]),
    ("global", &[]),
    ("hideconstructor", &[]),
    ("ignore", &[]),
    ("implements", &[]),
    ("inheritdoc", &[]),
    ("inheritDoc", &[]),
    ("inner", &[]),
    ("instance", &[]),
    ("interface", &[]),
    ("kind", &[]),
    ("lends", &[]),
    ("license", &[]),
    ("listens", &[]),
    ("member", &["var"]),
    ("memberof", &[]),
    ("memberof!", &[]),
    ("mixes", &[]),
    ("mixin", &[]),
    ("module", &[]),
    ("name", &[]),
    ("namespace", &[]),
    ("override", &[]),
    ("package", &[]),
    ("param", &["arg", "argument"]),
    ("private", &[]),
    ("property", &["prop"]),
    ("protected", &[]),
    ("public", &[]),
    ("readonly", &[]),
    ("requires", &[]),
    ("returns", &["return"]),
    ("see", &[]),
    ("since", &[]),
    ("static", &[]),
    ("summary", &[]),
    ("this", &[]),
    ("throws", &["exception"]),
    ("todo", &[]),
    ("tutorial", &[]),
    ("type", &[]),
    ("typedef", &[]),
    ("variation", &[]),
    ("version", &[]),
    ("yields", &["yield"]),
];

/// TSDoc and TypeDoc tags, then the ones TypeScript itself reads.
const TYPESCRIPT_EXTRA: &[Entry] = &[
    ("alpha", &[]),
    ("beta", &[]),
    ("decorator", &[]),
    ("eventProperty", &[]),
    ("experimental", &[]),
    ("packageDocumentation", &[]),
    ("privateRemarks", &[]),
    ("remarks", &[]),
    ("sealed", &[]),
    ("category", &[]),
    ("categoryDescription", &[]),
    ("disableGroups", &[]),
    ("document", &[]),
    ("expand", &[]),
    ("expandType", &[]),
    ("group", &[]),
    ("groupDescription", &[]),
    ("hidden", &[]),
    ("hideCategories", &[]),
    ("hideGroups", &[]),
    ("inline", &[]),
    ("inlineType", &[]),
    ("mergeModuleWith", &[]),
    ("preventExpand", &[]),
    ("preventInline", &[]),
    ("primaryExport", &[]),
    ("showCategories", &[]),
    ("showGroups", &[]),
    ("sortStrategy", &[]),
    ("useDeclaredType", &[]),
    ("import", &[]),
    ("internal", &[]),
    ("jsx", &[]),
    ("overload", &[]),
    ("satisfies", &[]),
    ("template", &["typeParam"]),
];

/// TypeScript-mode tags Closure mode leaves out (`returns` becomes an alias of `return`).
const NOT_IN_CLOSURE: &[&str] = &["inheritdoc", "internal", "overload", "returns", "satisfies"];

const CLOSURE_EXTRA: &[Entry] = &[
    ("closurePrimitive", &[]),
    ("customElement", &[]),
    ("expose", &[]),
    ("idGenerator", &[]),
    ("meaning", &[]),
    ("mixinClass", &[]),
    ("mixinFunction", &[]),
    ("ngInject", &[]),
    ("owner", &[]),
    ("typeSummary", &[]),
    ("wizaction", &[]),
    ("define", &[]),
    ("dict", &[]),
    ("export", &[]),
    ("externs", &[]),
    ("final", &[]),
    ("implicitCast", &[]),
    ("noalias", &[]),
    ("nocollapse", &[]),
    ("nocompile", &[]),
    ("noinline", &[]),
    ("nosideeffects", &[]),
    ("polymer", &[]),
    ("polymerBehavior", &[]),
    ("preserve", &[]),
    ("record", &[]),
    ("return", &["returns"]),
    ("struct", &[]),
    ("suppress", &[]),
    ("unrestricted", &[]),
];

const JSX_TAGS: &[&str] = &["jsx", "jsxFrag", "jsxImportSource", "jsxRuntime"];

const TYPED_ALWAYS: &[&str] = &[
    "augments",
    "callback",
    "class",
    "enum",
    "implements",
    "private",
    "property",
    "protected",
    "public",
    "readonly",
    "this",
    "type",
    "typedef",
];

const TYPED_OUTSIDE_DECLARE: &[&str] = &[
    "abstract",
    "access",
    "class",
    "constant",
    "constructs",
    "export",
    "exports",
    "function",
    "global",
    "inherits",
    "instance",
    "interface",
    "member",
    "memberof",
    "memberOf",
    "method",
    "mixes",
    "mixin",
    "module",
    "name",
    "namespace",
    "override",
    "property",
    "requires",
    "static",
    "this",
];

const TYPED_NEEDING_NAME: &[&str] = &["template"];

const DEFAULT_INLINE: &[&str] = &[
    "link",
    "linkcode",
    "linkplain",
    "tutorial",
    "inheritDoc",
    "label",
    "include",
    "includeCode",
];

/// Upper bound on the ancestors walked looking for `declare`.
const MAX_ANCESTORS: usize = 10_000;

#[derive(Debug)]
struct CheckTagNames {
    defined: Vec<String>,
    enable_fixer: bool,
    inline: Vec<String>,
    jsx: bool,
    typed: bool,
}

impl FromOptions for CheckTagNames {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let inline = match o.get("inlineTags") {
            Some(_) => str_list(o, "inlineTags"),
            None => DEFAULT_INLINE.iter().map(|s| (*s).to_owned()).collect(),
        };
        Ok(CheckTagNames {
            defined: str_list(o, "definedTags"),
            enable_fixer: bool_field(o, "enableFixer", true),
            inline,
            jsx: bool_field(o, "jsxTags", false),
            typed: bool_field(o, "typed", false),
        })
    }
}

/// The tag table of a `settings.jsdoc.mode` (`None` for an unknown mode, which falls back
/// to the jsdoc table after a warning).
fn table(mode: &str) -> Option<Vec<Entry>> {
    let ts = || JSDOC_TAGS.iter().chain(TYPESCRIPT_EXTRA).copied();
    match mode {
        "jsdoc" => Some(JSDOC_TAGS.to_vec()),
        "typescript" => Some(ts().collect()),
        "closure" | "permissive" => Some(
            ts().filter(|(k, _)| !NOT_IN_CLOSURE.contains(k))
                .chain(CLOSURE_EXTRA.iter().copied())
                .collect(),
        ),
        _ => None,
    }
}

/// JavaScript truthiness of a settings value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// A settings value interpolated into a message (`${value}`).
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        Value::Object(_) => "[object Object]".to_owned(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `text` with its first `@tag` that ends at a word boundary renamed to `@to`.
fn rename_first(text: &str, tag: &str, to: &str) -> String {
    let needle = format!("@{tag}");
    let mut from = 0;
    while let Some(off) = text[from..].find(&needle) {
        let at = from + off;
        let end = at + needle.len();
        if is_word(needle.chars().next_back()) != is_word(text[end..].chars().next()) {
            return format!("{}@{to}{}", &text[..at], &text[end..]);
        }
        from = at + 1;
        while !text.is_char_boundary(from) {
            from += 1;
        }
    }
    text.to_owned()
}

struct FileState<'s> {
    table: Vec<Entry>,
    /// The mode is unknown and not yet reported.
    warn_mode: Option<&'s str>,
    prefs: &'s Map<String, Value>,
    /// Tag names the options and settings allow besides the mode's table.
    extra_valid: Vec<String>,
    /// `tagNamePreference` has a value that is neither falsy, a string nor an object.
    bad_pref: bool,
    is_dts: bool,
}

impl FileState<'_> {
    fn known(&mut self, cx: &mut Ctx<'_, '_>, name: &str) -> bool {
        if let Some(mode) = self.warn_mode.take() {
            cx.report(
                Span::at(0),
                format!("Unrecognized value `{mode}` for `settings.jsdoc.mode`."),
            )
            .loc(Loc::at(1, 2));
        }
        self.table
            .iter()
            .any(|(k, aliases)| *k == name || aliases.contains(&name))
            || self.extra_valid.iter().any(|t| t == name)
    }

    /// `getPreferredTagNameSimple`: the preference value for `name`.
    fn preferred(&self, name: &str) -> Value {
        let is_value = self.prefs.values().any(|v| match v {
            Value::String(s) => s == name,
            Value::Object(o) => o.get("replacement").and_then(Value::as_str) == Some(name),
            _ => false,
        });
        if is_value {
            return Value::String(name.to_owned());
        }
        // Later keys win when `tag x` and `x` both appear.
        let by_key = self
            .prefs
            .iter()
            .rev()
            .find(|(k, _)| k.strip_prefix("tag ").unwrap_or(k) == name);
        if let Some((_, v)) = by_key {
            return v.clone();
        }
        let canonical = self
            .table
            .iter()
            .find(|(_, aliases)| aliases.contains(&name))
            .map_or(name, |(k, _)| k);
        Value::String(canonical.to_owned())
    }
}

impl CheckTagNames {
    /// Reports a tag a type system makes redundant, with the fix that drops it (or, with a
    /// description, clears its type, and with `drop_tag` its tag too).
    fn report_redundant(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &Finder<'_>,
        doc: &mut Doc,
        i: usize,
        msg: String,
        drop_tag: bool,
    ) {
        let span = doc.line_span(finder, doc.block.tag_line(i));
        if !self.enable_fixer {
            cx.report(span, msg);
            return;
        }
        let tag = &doc.block.tags[i];
        if tag.description.trim().is_empty() {
            remove_tag_with(&mut doc.block, i, 0, true);
        } else if let Some(&first) = tag.source.first() {
            let t = &mut doc.block.lines[first].tokens;
            t.typ.clear();
            t.post_type.clear();
            if drop_tag {
                t.tag.clear();
                t.post_tag.clear();
            }
        }
        let text = doc.block.stringify();
        let fix = if text.is_empty() {
            removal_fix(finder.src, doc.comment)
        } else {
            doc.fix()
        };
        cx.report(span, msg).fix(fix);
    }

    /// Whether the typed checks reported the tag.
    fn typed_check(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &Finder<'_>,
        doc: &mut Doc,
        i: usize,
        is_dts: bool,
    ) -> bool {
        let name = doc.block.tags[i].tag.clone();
        if TYPED_ALWAYS.contains(&name.as_str()) {
            let msg = format!("'@{name}' is redundant when using a type system.");
            self.report_redundant(cx, finder, doc, i, msg, true);
            return true;
        }
        if TYPED_OUTSIDE_DECLARE.contains(&name.as_str())
            && let Some(t) = doc.target
            && !(is_dts && at_top_level(finder, t))
            && !in_ambient_context(finder, t, is_dts)
        {
            let msg = format!(
                "'@{name}' is redundant outside of ambient (`declare`/`.d.ts`) contexts when using a type system."
            );
            self.report_redundant(cx, finder, doc, i, msg, false);
            return true;
        }
        if TYPED_NEEDING_NAME.contains(&name.as_str()) && doc.block.tags[i].name.is_empty() {
            let msg = format!("'@{name}' without a name is redundant when using a type system.");
            self.report_redundant(cx, finder, doc, i, msg, false);
            return true;
        }
        false
    }

    fn check_block(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &Finder<'_>,
        doc: &mut Doc,
        st: &mut FileState<'_>,
    ) {
        if st.bad_pref {
            cx.report(
                Span::at(0),
                "Invalid `settings.jsdoc.tagNamePreference`. Values must be falsy, a string, or an object.",
            )
            .loc(Loc::at(1, 2));
        }
        for i in 0..doc.block.tags.len() {
            let name = doc.block.tags[i].tag.clone();
            if self.jsx && JSX_TAGS.contains(&name.as_str()) {
                continue;
            }
            if self.typed && self.typed_check(cx, finder, doc, i, st.is_dts) {
                continue;
            }
            let valid = st.known(cx, &name)
                || self.defined.contains(&name)
                || (self.typed && TYPED_NEEDING_NAME.contains(&name.as_str()));
            if !valid {
                let span = doc.line_span(finder, doc.block.tag_line(i));
                cx.report(span, format!("Invalid JSDoc tag name \"{name}\"."));
                continue;
            }
            let pref = st.preferred(&name);
            let blocked = match &pref {
                Value::Bool(false) => true,
                Value::Object(o) => !o.get("replacement").is_some_and(truthy),
                _ => false,
            };
            if blocked {
                let msg = match &pref {
                    Value::Object(o) if o.get("message").is_some_and(truthy) => {
                        o.get("message").map(js_string).unwrap_or_default()
                    }
                    _ => format!("Blacklisted tag found (`@{name}`)"),
                };
                let first = doc
                    .block
                    .tags
                    .iter()
                    .position(|t| t.tag == name)
                    .unwrap_or(i);
                let span = doc.line_span(finder, doc.block.tag_line(first));
                cx.report(span, msg);
                continue;
            }
            if !truthy(&pref) {
                continue;
            }
            let (message, replacement) = match &pref {
                Value::Object(o) => (
                    o.get("message").filter(|m| truthy(m)).map(js_string),
                    o.get("replacement").cloned().unwrap_or(Value::Null),
                ),
                other => (None, other.clone()),
            };
            if replacement.as_str() == Some(name.as_str()) {
                continue;
            }
            let to = js_string(&replacement);
            let message = message.unwrap_or_else(|| {
                format!(
                    "Invalid JSDoc tag (preference). Replace \"{name}\" JSDoc tag with \"{to}\"."
                )
            });
            let text = rename_first(doc.comment.text(finder.src), &name, &to);
            let span = doc.line_span(finder, doc.block.tag_line(i));
            cx.report(span, message)
                .fix(crate::diagnostic::Fix::replace(doc.comment, text));
        }
        self.check_inline(cx, finder, doc);
    }

    /// `getInlineTags`: the block description's, reported at the first line mentioning them
    /// (else the last line), then each tag's, at its first line mentioning them (else
    /// line 0).
    fn check_inline(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &Doc) {
        let b = &doc.block;
        let desc: Vec<&str> = b
            .source
            .iter()
            .map(|&id| &b.lines[id].tokens)
            .take_while(|t| t.tag.is_empty())
            .map(|t| t.description.trim())
            .filter(|d| !d.is_empty())
            .collect();
        let desc = desc.join(" ");
        let mut found: Vec<(&str, i64)> = Vec::new();
        for tag in inline_tag_names(&desc) {
            let needle = format!("{{@{tag}");
            let at = b
                .source
                .iter()
                .position(|&id| b.lines[id].tokens.description.contains(&needle));
            let line = at.map_or(b.source.len() as i64 - 1, |p| p as i64);
            found.push((tag, line));
        }
        for t in &b.tags {
            for tag in inline_tag_names(&t.description) {
                let needle = format!("{{@{tag}");
                let line = t
                    .source
                    .iter()
                    .find(|&&id| b.lines[id].tokens.description.contains(&needle))
                    .map_or(0, |&id| b.lines[id].number);
                found.push((tag, line));
            }
        }
        for (tag, line) in found {
            if !self.inline.iter().any(|t| t == tag) {
                let span = doc.line_span(finder, line);
                cx.report(span, format!("Invalid JSDoc inline tag name \"{tag}\""));
            }
        }
    }
}

/// The ESTree parent of the target is the Program.
fn at_top_level(finder: &Finder<'_>, t: Target) -> bool {
    let parent = match t {
        Target::Export(id) | Target::Node(id) => finder.parent(id),
    };
    parent.is_none_or(|p| matches!(finder.ast.node(p), Node::Program { .. }))
}

/// Whether the target or an ancestor is `declare`d; reaching the Program, whether the file
/// is a `.d.ts`.
fn in_ambient_context(finder: &Finder<'_>, t: Target, is_dts: bool) -> bool {
    // An `export` wrapper has no `declare` of its own.
    let mut cur = match t {
        Target::Node(id) => Some(id),
        Target::Export(id) => finder.parent(id),
    };
    for _ in 0..MAX_ANCESTORS {
        let Some(id) = cur else {
            return is_dts;
        };
        let node = finder.ast.node(id);
        match node {
            Node::Program { .. } => return is_dts,
            Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::PropertyDef { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Enum { .. }
            | Node::Module { .. }
                if node.modifiers().contains(Modifiers::DECLARE) =>
            {
                return true;
            }
            _ => {}
        }
        cur = finder.parent(id);
    }
    false
}

impl Rule for CheckTagNames {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let env = cx.env();
        let empty = Map::new();
        let jsdoc_settings = env
            .settings
            .get("jsdoc")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let prefs = jsdoc_settings
            .get("tagNamePreference")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let mode = jsdoc_settings
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("typescript");
        let mut extra_valid: Vec<String> = Vec::new();
        let mut bad_pref = false;
        for v in prefs.values() {
            match v {
                Value::String(s) if !s.is_empty() => extra_valid.push(s.clone()),
                Value::Object(o) => {
                    if let Some(r) = o.get("replacement").filter(|r| truthy(r)) {
                        extra_valid.push(js_string(r));
                    }
                }
                v if truthy(v) => bad_pref = true,
                _ => {}
            }
        }
        extra_valid.extend(prefs.keys().cloned());
        if let Some(st) = jsdoc_settings
            .get("structuredTags")
            .and_then(Value::as_object)
        {
            extra_valid.extend(st.keys().cloned());
        }
        let (table, warn_mode) = match table(mode) {
            Some(t) => (t, None),
            None => (JSDOC_TAGS.to_vec(), Some(mode)),
        };
        let mut st = FileState {
            table,
            warn_mode,
            prefs,
            extra_valid,
            bad_pref,
            is_dts: cx.file.path.ends_with(".d.ts"),
        };
        each_block(cx, |cx, finder, doc| {
            self.check_block(cx, finder, doc, &mut st)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_the_first_tag_at_a_word_boundary() {
        let text = "/**\n * @argument a\n * @arg b\n */";
        assert_eq!(
            rename_first(text, "arg", "param"),
            "/**\n * @argument a\n * @param b\n */"
        );
        assert_eq!(rename_first("/** @x */", "y", "z"), "/** @x */");
    }

    #[test]
    fn closure_mode_maps_returns_to_return() {
        let t = table("closure").unwrap_or_default();
        assert!(
            t.iter()
                .any(|(k, a)| *k == "return" && a.contains(&"returns"))
        );
        assert!(!t.iter().any(|(k, _)| *k == "returns"));
        assert!(table("badMode").is_none());
    }
}
