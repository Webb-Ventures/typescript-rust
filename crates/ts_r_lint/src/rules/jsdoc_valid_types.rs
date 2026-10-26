//! `jsdoc/valid-types`: tag types, names and namepaths parse in the configured mode, and
//! tags carry the name or type their structure requires (or none they may not have).

use super::prelude::*;
use crate::jsdoc_tags::{Answer, Modes, SMode};
use crate::jsdoc_tree::{block_inline_tags, tag_inline_tags};
use crate::jsdoc_type::{self as jt, Mode, N};
use crate::jsdoc_util::{each_block, tag_description};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/valid-types",
        "Requires all types/namepaths to be valid JSDoc, Closure compiler, or TypeScript types (configurable in settings).",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<ValidTypes>,
};

#[derive(Debug, Default)]
struct ValidTypes {
    allow_empty_namepaths: bool,
}

impl FromOptions for ValidTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ValidTypes {
            allow_empty_namepaths: opts
                .first()
                .and_then(|o| o.get("allowEmptyNamepaths"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

const INLINE_TAGS: &[&str] = &[
    "include",
    "includeCode",
    "inheritDoc",
    "inheritdoc",
    "label",
    "link",
    "linkcode",
    "linkplain",
    "tutorial",
];

/// Closure Compiler's `@suppress` names.
const SUPPRESS_TYPES: &[&str] = &[
    "accessControls",
    "checkDebuggerStatement",
    "checkEs5InheritanceCorrectnessConditions",
    "checkPrototypalTypes",
    "checkRegExp",
    "checkTypes",
    "checkVars",
    "closureClassChecks",
    "closureDepMethodUsageChecks",
    "const",
    "constantProperty",
    "dangerousUnrecognizedTypeError",
    "deprecated",
    "duplicate",
    "es5Strict",
    "externsValidation",
    "extraProvide",
    "extraRequire",
    "globalThis",
    "invalidCasts",
    "lateProvide",
    "legacyGoogScopeRequire",
    "lintChecks",
    "lintVarDeclarations",
    "messageConventions",
    "misplacedTypeAnnotation",
    "missingOverride",
    "missingPolyfill",
    "missingProperties",
    "missingProvide",
    "missingRequire",
    "missingReturn",
    "missingSourcesWarnings",
    "moduleLoad",
    "msgDescriptions",
    "nonStandardJsDocs",
    "partialAlias",
    "polymer",
    "reportUnknownTypes",
    "strictCheckTypes",
    "strictMissingProperties",
    "strictModuleDepCheck",
    "strictPrimitiveOperators",
    "suspiciousCode",
    "switch",
    "transitionalSuspiciousCodeWarnings",
    "undefinedNames",
    "undefinedVars",
    "underscore",
    "unknownDefines",
    "untranspilableFeatures",
    "unusedLocalVariables",
    "unusedPrivateMembers",
    "uselessCode",
    "useOfGoogProvide",
    "visibility",
    "with",
];

/// `parseNamePath` with special prefixes allowed; `permissive` reads as `jsdoc`.
pub(crate) fn namepath_parses(path: &str, mode: &str) -> bool {
    let m = Mode::from_setting(mode).unwrap_or(Mode::Jsdoc);
    jt::parse_name_path(path, m, true).is_some()
}

/// `parseClosureTemplateTag`: the names a `@template` declares.
pub(crate) fn closure_template_names(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in name.chars() {
        match c {
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if c == ',' && depth == 0 {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| {
            let t = p.trim();
            match t.find('=') {
                Some(i) => {
                    let n = &t[..i];
                    n.strip_prefix('[').unwrap_or(n).to_owned()
                }
                None => t.to_owned(),
            }
        })
        .collect()
}

/// `/as\s+/`: where the first `as` followed by whitespace starts, and the match length.
fn as_expression(s: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(o) = s[from..].find("as") {
        let at = from + o;
        let rest = &s[at + 2..];
        let ws = rest.len() - rest.trim_start_matches(|c: char| c.is_whitespace()).len();
        if ws > 0 {
            return Some((at, 2 + ws));
        }
        from = at + 1;
    }
    None
}

impl Rule for ValidTypes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings_value = cx
            .env()
            .settings
            .get("jsdoc")
            .cloned()
            .unwrap_or(Value::Null);
        each_block(cx, |cx, finder, doc| {
            let mode = finder.settings.mode.as_str();
            let modes = Modes::new(SMode::from_setting(mode), &settings_value);
            let cur = &modes.current;
            let mode_info = |a: Answer, yes_is_plain: bool| match (a, yes_is_plain) {
                (Answer::Yes, true) | (Answer::No, false) => String::new(),
                _ => format!(" in \"{mode}\" mode"),
            };
            let mut reports: Vec<(Option<usize>, String)> = Vec::new();
            for i in 0..doc.block.tags.len() {
                let tag = doc.block.tags[i].clone();
                let name = tag.name.as_str();
                let mut report = |msg: String| reports.push((Some(i), msg));
                let valid_namepath =
                    |np: &str, tag_name: Option<&str>, report: &mut dyn FnMut(String)| {
                        if namepath_parses(np, mode) {
                            return true;
                        }
                        let handled = match tag_name {
                            Some("memberof" | "memberof!") => {
                                np.ends_with(['#', '.', '~'])
                                    && namepath_parses(&np[..np.len() - 1], mode)
                            }
                            Some("module" | "requires") => {
                                !np.starts_with("module:")
                                    && namepath_parses(&format!("module:{np}"), mode)
                            }
                            Some("prop" | "property") => {
                                mode == "jsdoc"
                                    && !np.is_empty()
                                    && np.bytes().all(|b| b.is_ascii_digit())
                            }
                            Some("borrows") => {
                                np.starts_with(['#', '.', '~']) && namepath_parses(&np[1..], mode)
                            }
                            _ => false,
                        };
                        if !handled {
                            report(format!("Syntax error in namepath: {np}"));
                        }
                        handled
                    };
                if !tag.problems.is_empty() {
                    report(format!("Invalid name: {}", tag.problems.join("; ")));
                    continue;
                }
                if tag.tag == "import" {
                    continue;
                }
                if tag.tag == "borrows" {
                    let desc = tag_description(&doc.block, i);
                    let this_np = match as_expression(&desc) {
                        Some((at, len)) => format!("{}{}", &desc[..at], &desc[at + len..]),
                        None => desc.clone(),
                    };
                    let this_np = this_np.trim();
                    if as_expression(&desc).is_none() || this_np.is_empty() {
                        report(format!(
                            "@borrows must have an \"as\" expression. Found \"{desc}\""
                        ));
                        continue;
                    }
                    if valid_namepath(this_np, Some("borrows"), &mut report) {
                        valid_namepath(name, None, &mut report);
                    }
                    continue;
                }
                if tag.tag == "suppress"
                    && mode == "closure"
                    && let Some(t) = jt::try_parse(&tag.typ, false)
                {
                    for (id, _, _) in t.walk() {
                        let value = match &t.nodes[id] {
                            N::Name(v)
                            | N::Str { value: v, .. }
                            | N::Property { value: v, .. }
                            | N::SpecialNamePath { value: v, .. }
                            | N::Symbol { value: v, .. }
                            | N::BigInt(v) => v,
                            _ => continue,
                        };
                        if !SUPPRESS_TYPES.contains(&value.as_str()) {
                            report(format!("Syntax error in suppress type: {value}"));
                        }
                    }
                }
                let might_name = modes.might_name_position(&tag.tag);
                if might_name != Answer::Yes && !name.is_empty() {
                    let info = if might_name == Answer::No {
                        String::new()
                    } else {
                        mode_info(might_name, true)
                    };
                    report(format!("@{} should not have a name{info}.", tag.tag));
                    continue;
                }
                let mut typ = tag.typ.clone();
                if typ.trim().starts_with('@') {
                    typ.clear();
                }
                let might_type = modes.might_type(&tag.tag);
                if might_type != Answer::Yes && !typ.is_empty() {
                    let info = if might_type == Answer::No {
                        String::new()
                    } else {
                        mode_info(might_type, true)
                    };
                    report(format!(
                        "@{} should not have a bracketed type{info}.",
                        tag.tag
                    ));
                    continue;
                }
                let must_name = modes.must_name(&tag.tag);
                if must_name != Answer::No
                    && name.is_empty()
                    && !self.allow_empty_namepaths
                    && !matches!(
                        tag.tag.as_str(),
                        "arg" | "argument" | "param" | "prop" | "property"
                    )
                    && (tag.tag != "see" || !tag_description(&doc.block, i).contains("{@link"))
                {
                    report(format!(
                        "Tag @{} must have a name/namepath{}.",
                        tag.tag,
                        mode_info(must_name, true)
                    ));
                    continue;
                }
                let must_type = modes.must_type(&tag.tag);
                if must_type != Answer::No && typ.is_empty() && tag.tag != "next" {
                    report(format!(
                        "Tag @{} must have a type{}.",
                        tag.tag,
                        mode_info(must_type, true)
                    ));
                    continue;
                }
                let missing = modes.missing_type_or_namepath(&tag.tag, &typ, name);
                if missing != Answer::No && !self.allow_empty_namepaths {
                    report(format!(
                        "Tag @{} must have either a type or namepath{}.",
                        tag.tag,
                        mode_info(missing, true)
                    ));
                    continue;
                }
                if might_type == Answer::Yes
                    && !typ.is_empty()
                    && (typ != "const" || tag.tag != "type")
                {
                    match jt::parse_for_setting(&typ, mode, true) {
                        None => report(format!("Syntax error in type: {typ}")),
                        Some(t) if matches!(mode, "closure" | "typescript") => {
                            for (id, _, _) in t.walk() {
                                let right = match &t.nodes[id] {
                                    N::ObjectField { right, .. } | N::KeyValue { right, .. } => {
                                        *right
                                    }
                                    _ => None,
                                };
                                if let Some(r) = right
                                    && matches!(t.nodes[r], N::Nullable { prefix: false, .. })
                                {
                                    report("Syntax error in type: JsdocTypeNullable".to_owned());
                                }
                            }
                        }
                        Some(_) => {}
                    }
                }
                let has_namepath =
                    (must_name != Answer::No || cur.might_namepath(&tag.tag)) && !name.is_empty();
                if has_namepath {
                    if mode != "jsdoc" && tag.tag == "template" {
                        if !namepath_parses(name.trim(), mode) {
                            for np in closure_template_names(name) {
                                valid_namepath(&np, None, &mut report);
                            }
                        }
                    } else {
                        valid_namepath(name, Some(&tag.tag), &mut report);
                    }
                }
                let has_name = cur.might_name(&tag.tag) && !name.is_empty();
                if has_name && mode == "typescript" {
                    if jt::parse_name(name, Mode::Typescript).is_none() {
                        report(format!("Syntax error in name: {name}"));
                    }
                } else if has_name {
                    valid_namepath(name, Some(&tag.tag), &mut report);
                }
                for t in tag_inline_tags(&doc.block, i) {
                    if INLINE_TAGS.contains(&t.tag.as_str())
                        && t.text.is_empty()
                        && t.namepath_or_url.is_empty()
                    {
                        report(format!("Inline tag \"{}\" missing content", t.tag));
                    }
                }
            }
            for t in block_inline_tags(&doc.block) {
                if INLINE_TAGS.contains(&t.tag.as_str())
                    && t.text.is_empty()
                    && t.namepath_or_url.is_empty()
                {
                    reports.push((None, format!("Inline tag \"{}\" missing content", t.tag)));
                }
            }
            for (tag, msg) in reports {
                let span = match tag {
                    Some(i) => doc.line_span(finder, doc.block.tag_line(i)),
                    None => doc.comment,
                };
                cx.report(span, msg);
            }
        });
    }
}
