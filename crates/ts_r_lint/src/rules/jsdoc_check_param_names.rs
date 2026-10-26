//! `jsdoc/check-param-names`: `@param` names match the function's parameters, in order,
//! without duplicates; destructured parameters have their properties documented (and no
//! others); and every dotted path follows the parameter it extends.
//!
//! What the plugin does, in our words:
//! - `@param` tags are walked in order and the first problem stops the block. Dotted names
//!   are skipped when pairing tags with parameters (the `n`th undotted tag documents the
//!   `n`th parameter); a leading `this` parameter is skipped unless the tag is named `this`.
//! - A destructured root (or a parameter typed with a type literal) expects one `@param`
//!   per flattened property path under the tag's name (or under the parameter's name when it
//!   has one). Properties under a tag whose type is not object-like
//!   (`checkTypesPattern`) are not required; rest properties are not required unless
//!   `checkRestProperty`. A missing property is reported on the line before the first tag
//!   whose path does not start with it (counted from the root tag's line, so it can land
//!   past the end of the comment); extra properties under the root are reported on their tag.
//! - For a plain name that doesn't match, the message lists every expected name (a
//!   destructured parameter shows the tag at its position, a rest parameter `...name`).
//!   With `disableMissingParamChecks`, documented names in order are enough.

use super::prelude::*;
use crate::jsdoc::Doc;
use crate::jsdoc_util::{
    ContextOpts, ItemName, ParamName, function_param_names, path_does_not_begin_with,
    regex_string_matches, remove_tag, same_path, target_params,
};

const ALLOWED: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "TSDeclareFunction",
    "TSMethodSignature",
    "TSPropertySignature",
];

const DEFAULT_TYPES: &str = "/^(?:[oO]bject|[aA]rray|PlainObject|Generic(?:Object|Array))$/";

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-param-names",
        "Checks for dupe `@param` names, that nested param names have roots, and that parameter names in function declarations match JSDoc param names.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<CheckParamNames>,
};

#[derive(Debug)]
struct CheckParamNames {
    ctx: ContextOpts,
    allow_extra_trailing: bool,
    check_destructured: bool,
    check_rest_property: bool,
    check_types: String,
    disable_extra_property_reporting: bool,
    disable_missing_param_checks: bool,
    enable_fixer: bool,
    use_default_object_properties: bool,
}

impl FromOptions for CheckParamNames {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ctx = ContextOpts::parse_with(opts, ALLOWED)?;
        let o = &ctx.opts;
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        Ok(CheckParamNames {
            allow_extra_trailing: flag("allowExtraTrailingParamDocs", false),
            check_destructured: flag("checkDestructured", true),
            check_rest_property: flag("checkRestProperty", false),
            check_types: o
                .get("checkTypesPattern")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_TYPES)
                .to_owned(),
            disable_extra_property_reporting: flag("disableExtraPropertyReporting", false),
            disable_missing_param_checks: flag("disableMissingParamChecks", false),
            enable_fixer: flag("enableFixer", false),
            use_default_object_properties: flag("useDefaultObjectProperties", false),
            ctx,
        })
    }
}

/// One `@param` tag: its index in the block, name (trimmed) and type.
struct ParamTag {
    idx: usize,
    name: String,
    raw_name: String,
    typ: String,
}

/// A report on a comment-relative line, at column 0.
fn report_line(
    cx: &mut Ctx<'_, '_>,
    finder: &crate::jsdoc::Finder<'_>,
    doc: &Doc,
    rel: i64,
    msg: String,
) {
    let (span, loc) = doc.report_at(finder, rel, None);
    cx.report(span, msg).loc(loc);
}

impl CheckParamNames {
    fn types_match(&self, ty: &str) -> bool {
        regex_string_matches(&self.check_types, ty)
    }

    /// `validateParameterNames`: true when a problem was reported.
    fn names(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &crate::jsdoc::Finder<'_>,
        doc: &mut Doc,
        target: &str,
        params: &[ParamName],
        tags: &[ParamTag],
    ) -> bool {
        let line_of = |doc: &Doc, idx: usize| doc.block.tag_line(idx);
        let mut dotted = 0usize;
        let mut this_offset: i64 = 0;
        for (index, tag) in tags.iter().enumerate() {
            if let Some(dupe) = tags
                .iter()
                .enumerate()
                .find(|&(j, t)| t.raw_name == tag.raw_name && j != index)
                .map(|(_, t)| t.idx)
            {
                let msg = format!("Duplicate @{target} \"{}\"", tag.raw_name);
                let (span, loc) = doc.report_at(finder, line_of(doc, dupe), None);
                if self.enable_fixer {
                    remove_tag(&mut doc.block, dupe, 0);
                    let fix = doc.fix();
                    cx.report(span, msg).loc(loc).fix(fix);
                } else {
                    cx.report(span, msg).loc(loc);
                }
                return true;
            }
            if tag.raw_name.contains('.') {
                dotted += 1;
                continue;
            }
            let at = |off: i64| -> Option<&ParamName> {
                let i = index as i64 - dotted as i64 + off;
                usize::try_from(i).ok().and_then(|i| params.get(i))
            };
            let mut param = at(this_offset);
            if matches!(param, Some(ParamName::Name(n)) if n == "this") && tag.name != "this" {
                this_offset += 1;
                param = at(this_offset);
            }
            let param = match param {
                None | Some(ParamName::Missing) => {
                    if self.allow_extra_trailing {
                        continue;
                    }
                    report_line(
                        cx,
                        finder,
                        doc,
                        line_of(doc, tag.idx),
                        format!(
                            "@{target} \"{}\" does not match an existing function parameter.",
                            tag.raw_name
                        ),
                    );
                    return true;
                }
                Some(p) => p,
            };
            match param {
                ParamName::Item {
                    name: ItemName::List(list),
                    ..
                } => {
                    let expected = list
                        .get(index)
                        .map(|n| n.clone().unwrap_or_else(|| "undefined".to_owned()))
                        .unwrap_or_else(|| "undefined".to_owned());
                    if tag.name == expected {
                        this_offset -= 1;
                        continue;
                    }
                    report_line(
                        cx,
                        finder,
                        doc,
                        line_of(doc, tag.idx),
                        format!(
                            "Expected @{target} name to be \"{expected}\". Got \"{}\".",
                            tag.name
                        ),
                    );
                    return true;
                }
                ParamName::Root(root, inner) => {
                    if !self.check_destructured {
                        continue;
                    }
                    if !tag.typ.is_empty() && !self.types_match(&tag.typ) {
                        continue;
                    }
                    let crate::jsdoc_util::Inner::Flat(flat) = &**inner else {
                        continue;
                    };
                    if let Some(a) = &flat.annotation
                        && &tag.name != a
                    {
                        report_line(
                            cx,
                            finder,
                            doc,
                            line_of(doc, tag.idx),
                            format!(
                                "@{target} \"{}\" does not match parameter name \"{a}\"",
                                tag.name
                            ),
                        );
                    }
                    let tag_name = root.clone().unwrap_or_else(|| tag.name.clone());
                    let expected: Vec<String> = flat
                        .names
                        .iter()
                        .map(|n| format!("{tag_name}.{n}"))
                        .collect();
                    let mut missing: Vec<(String, i64)> = Vec::new();
                    let mut not_checking: Vec<String> = Vec::new();
                    for (idx, name) in expected.iter().enumerate() {
                        if not_checking.iter().any(|n| name.starts_with(n.as_str())) {
                            continue;
                        }
                        match tags.iter().position(|t| same_path(name, &t.name)) {
                            None => {
                                if !self.check_rest_property
                                    && flat.rests.get(idx).copied().unwrap_or(false)
                                {
                                    continue;
                                }
                                let at = tags
                                    .iter()
                                    .position(|t| path_does_not_begin_with(name, &t.name))
                                    .unwrap_or(tags.len());
                                let line = line_of(doc, tag.idx) - 1 + at as i64;
                                missing.push((name.clone(), if line == 0 { 1 } else { line }));
                            }
                            Some(i) => {
                                let ty = &tags[i].typ;
                                if !ty.is_empty() && !self.types_match(ty) {
                                    not_checking.push(name.clone());
                                }
                            }
                        }
                    }
                    for (name, line) in &missing {
                        report_line(
                            cx,
                            finder,
                            doc,
                            *line,
                            format!("Missing @{target} \"{name}\""),
                        );
                    }
                    if !flat.has_property_rest || self.check_rest_property {
                        let prefix = format!("{}.", tag.name);
                        let extra: Vec<&ParamTag> = tags
                            .iter()
                            .filter(|t| {
                                t.name.starts_with(&prefix)
                                    && !expected.iter().any(|e| same_path(&t.name, e))
                                    && !same_path(&tag.raw_name, &t.name)
                                    && (!self.disable_extra_property_reporting
                                        || flat.names.iter().any(|p| {
                                            p.split('.').count() + 1 >= t.name.split('.').count()
                                        }))
                            })
                            .collect();
                        for t in &extra {
                            report_line(
                                cx,
                                finder,
                                doc,
                                line_of(doc, t.idx),
                                format!(
                                    "@{target} \"{}\" does not exist on {}",
                                    t.name, tag.raw_name
                                ),
                            );
                        }
                        if !extra.is_empty() {
                            return true;
                        }
                    }
                    if !missing.is_empty() {
                        return true;
                    }
                }
                ParamName::Name(_) | ParamName::Item { .. } => {
                    let own = match param {
                        ParamName::Name(n) => n.clone(),
                        ParamName::Item { name, .. } => name.display(),
                        _ => String::new(),
                    };
                    if own == tag.name {
                        continue;
                    }
                    let actual: Vec<&str> = tags
                        .iter()
                        .filter(|t| !t.raw_name.contains('.'))
                        .map(|t| t.name.as_str())
                        .collect();
                    // `None` is JavaScript's `undefined` (shown empty when joined); `true`
                    // marks a rest parameter, an object to the plugin.
                    let expected: Vec<(Option<String>, bool)> = params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| match p {
                            ParamName::Root(..) => (actual.get(i).map(|s| (*s).to_owned()), false),
                            ParamName::Name(n) => (Some(n.clone()), false),
                            ParamName::Item {
                                name,
                                rest_element: true,
                                ..
                            } => (Some(format!("...{}", name.display())), true),
                            ParamName::Item { .. } => (Some("[object Object]".to_owned()), true),
                            ParamName::Missing => (None, false),
                        })
                        .filter(|(e, _)| e.as_deref() != Some("this"))
                        .collect();
                    if self.disable_missing_param_checks {
                        // Rest parameters compare as objects, which never equal a name.
                        let used: Vec<&str> = expected
                            .iter()
                            .filter(|(_, object)| !object)
                            .filter_map(|(e, _)| e.as_deref())
                            .filter(|e| !e.is_empty() && actual.contains(e))
                            .collect();
                        if actual
                            .iter()
                            .enumerate()
                            .all(|(i, a)| used.get(i) == Some(a))
                        {
                            continue;
                        }
                    }
                    let shown: Vec<&str> = expected
                        .iter()
                        .map(|(e, _)| e.as_deref().unwrap_or(""))
                        .collect();
                    report_line(
                        cx,
                        finder,
                        doc,
                        line_of(doc, tag.idx),
                        format!(
                            "Expected @{target} names to be \"{}\". Got \"{}\".",
                            shown.join(", "),
                            actual.join(", ")
                        ),
                    );
                    return true;
                }
                ParamName::Missing => {}
            }
        }
        false
    }

    /// `validateParameterNamesDeep`: dotted paths follow the parameter they extend.
    fn deep(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &crate::jsdoc::Finder<'_>,
        doc: &Doc,
        target: &str,
        tags: &[ParamTag],
    ) {
        let mut last_real: Option<&str> = None;
        for t in tags {
            let n = t.raw_name.as_str();
            let Some(dot) = n.find('.') else {
                last_real = Some(n);
                continue;
            };
            let line = doc.block.tag_line(t.idx);
            let Some(real) = last_real else {
                report_line(
                    cx,
                    finder,
                    doc,
                    line,
                    format!(
                        "@{target} path declaration (\"{n}\") appears before any real parameter."
                    ),
                );
                return;
            };
            let root = &n[..dot];
            let root = root.strip_suffix("[]").unwrap_or(root);
            if root != real {
                report_line(
                    cx,
                    finder,
                    doc,
                    line,
                    format!(
                        "@{target} path declaration (\"{n}\") root node name (\"{root}\") does not match previous real parameter name (\"{real}\")."
                    ),
                );
                return;
            }
        }
    }
}

impl Rule for CheckParamNames {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.ctx.each_doc(cx, |cx, finder, doc| {
            let Some(t) = doc.target else { return };
            let ty = finder.es_type(t);
            if !ALLOWED.contains(&ty) {
                return;
            }
            let ast = finder.ast;
            let Some(params) = target_params(ast, t.id()) else {
                return;
            };
            let Some(target) = doc.preferred(cx, finder, "param") else {
                return;
            };
            let tags: Vec<ParamTag> = doc
                .block
                .tags
                .iter()
                .enumerate()
                .filter(|(_, t)| t.tag == target)
                .map(|(idx, t)| ParamTag {
                    idx,
                    name: t.name.trim().to_owned(),
                    raw_name: t.name.clone(),
                    typ: t.typ.clone(),
                })
                .collect();
            if tags.is_empty() {
                return;
            }
            let names =
                function_param_names(ast, finder.src, params, self.use_default_object_properties);
            if self.names(cx, finder, doc, &target, &names, &tags) || !self.check_destructured {
                return;
            }
            self.deep(cx, finder, doc, &target, &tags);
        });
    }
}
