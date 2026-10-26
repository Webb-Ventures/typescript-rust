//! `jsdoc/check-types`: names inside JSDoc types follow `settings.jsdoc.preferredTypes`, the
//! types a structured tag allows, and the native casing of `string`, `Array`, `object`, ...

use super::prelude::*;
use crate::diagnostic::{Fix, Loc};
use crate::jsdoc_tags::{SMode, Structure};
use crate::jsdoc_type::{self as jt, Id, N, TypeTree};
use crate::jsdoc_util::each_block;
use serde_json::Map;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-types",
        "Reports types deemed invalid (customizable and with defaults, for preventing and/or recommending replacements).",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<CheckTypes>,
};

#[derive(Debug, Default)]
struct CheckTypes {
    /// `(tag, None)` exempts every type on the tag; `Some(list)` only those exact types.
    exempt: Vec<(String, Option<Vec<String>>)>,
    no_defaults: bool,
    unify: bool,
}

impl FromOptions for CheckTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let Some(o) = opts.first() else {
            return Ok(CheckTypes::default());
        };
        let flag = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
        let exempt = o
            .get("exemptTagContexts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let tag = e.get("tag")?.as_str()?.to_owned();
                        let types = match e.get("types") {
                            Some(Value::Bool(true)) => None,
                            Some(Value::Array(t)) => Some(
                                t.iter()
                                    .filter_map(|x| x.as_str().map(str::to_owned))
                                    .collect(),
                            ),
                            _ => Some(Vec::new()),
                        };
                        Some((tag, types))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(CheckTypes {
            exempt,
            no_defaults: flag("noDefaults"),
            unify: flag("unifyParentAndChildTypeChecks"),
        })
    }
}

/// Types whose casing is checked when no preference covers them.
const NATIVE: &[&str] = &[
    "undefined",
    "null",
    "boolean",
    "number",
    "bigint",
    "string",
    "symbol",
    "object",
    "Array",
    "Function",
    "Date",
    "RegExp",
];

/// One bad type: the name, the suggestion (a string or a list of allowed types), and the
/// preference's own message.
struct Invalid {
    bad: String,
    preferred: Option<Value>,
    message: Option<String>,
}

/// The preferences a file is checked against: the user's map, with the `Object` defaults of
/// the mode underneath unless the user maps any `Object` spelling.
fn preferred_types(settings: &Value, mode: &str) -> Map<String, Value> {
    let user = settings
        .get("preferredTypes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let maps_object = ["Object", "object", "object.<>", "Object.<>", "object<>"]
        .iter()
        .any(|k| user.contains_key(*k));
    let mut out = Map::new();
    if !maps_object {
        let msg = |c: char| {
            serde_json::json!({
                "message": format!(
                    "Use object shorthand or index signatures instead of `{c}bject`, e.g., `{{[key: string]: string}}`"
                ),
                "replacement": false,
            })
        };
        out.insert("Object".into(), Value::from("object"));
        if mode == "typescript" {
            out.insert("object.<>".into(), msg('o'));
            out.insert("Object.<>".into(), msg('O'));
            out.insert("object<>".into(), msg('o'));
            out.insert("Object<>".into(), msg('O'));
        } else {
            for k in ["object.<>", "Object.<>", "object<>"] {
                out.insert(k.into(), Value::from("Object<>"));
            }
        }
    }
    for (k, v) in user {
        out.insert(k, v);
    }
    out
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

/// `{{tagName}}`-style placeholders, as ESLint fills them from report data.
fn interpolate(msg: &str, tag_name: &str, tag_value: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(at) = rest.find("{{") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[at..]);
            return out;
        };
        let key = &after[..end];
        if key.contains(['{', '}']) {
            out.push('{');
            rest = &rest[at + 1..];
            continue;
        }
        match key.trim() {
            "tagName" => out.push_str(tag_name),
            "tagValue" => out.push_str(tag_value),
            _ => out.push_str(&rest[at..at + 4 + end]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// What a name inside a type is checked under.
struct Lookup<'a> {
    prefs: &'a Map<String, Value>,
    unify: bool,
}

impl Lookup<'_> {
    /// Whether a preference exists for `key` and either applies here or asks to apply to
    /// parent and child alike.
    fn applies(&self, key: &str, name_of_generic: bool) -> bool {
        let Some(p) = self.prefs.get(key) else {
            return false;
        };
        self.unify || name_of_generic || unify_flag(p)
    }

    /// Whether a preference matches the name `name` at this spot, the key it matched under,
    /// and whether the match was through the parent generic.
    fn info(
        &self,
        t: &TypeTree,
        name: &str,
        parent: Option<Id>,
        prop: &str,
    ) -> (bool, String, bool) {
        let generic = parent.and_then(|p| match &t.nodes[p] {
            N::Generic { square, dot, .. } => Some((*square, *dot)),
            _ => None,
        });
        let name_of_generic = generic.is_some() && prop == "left";
        let mut key = name.to_owned();
        let mut generic_match = false;
        if let Some((false, dot)) = generic {
            let fixes: &[&str] = if dot { &[".", ".<>"] } else { &["<>"] };
            if let Some(f) = fixes
                .iter()
                .find(|f| self.applies(&format!("{name}{f}"), name_of_generic))
            {
                key.push_str(f);
                generic_match = true;
            }
        }
        if !generic_match
            && !prop.is_empty()
            && let Some((square, dot)) = generic
        {
            let fixes: &[&str] = match (dot, square) {
                (true, _) => &[".", ".<>"],
                (false, false) => &["<>"],
                (false, true) => &["[]"],
            };
            if let Some(f) = fixes.iter().find(|f| self.applies(f, name_of_generic)) {
                key = (*f).to_owned();
                generic_match = true;
            }
        }
        let own = self.prefs.get(name);
        let direct = own.is_some() && !self.prefs.values().any(|v| v.as_str() == Some(name));
        let specific_unify = own.is_some_and(unify_flag);
        if !prop.is_empty() && direct && (self.unify || specific_unify) {
            generic_match = true;
        }
        (
            generic_match || (direct && prop.is_empty()),
            key,
            generic_match,
        )
    }
}

fn unify_flag(p: &Value) -> bool {
    p.as_object()
        .and_then(|o| o.get("unifyParentAndChildTypeChecks"))
        .is_some_and(truthy)
}

/// The value a name node reads as (`*` for the any type).
fn node_name(n: &N) -> Option<String> {
    match n {
        N::Name(v) => Some(v.clone()),
        N::Any => Some("*".into()),
        _ => None,
    }
}

/// Native casing: the first native type spelled like `name` in another case, unless the
/// preferences already map that native type.
fn native_casing(
    t: &TypeTree,
    prefs: &Map<String, Value>,
    name: &str,
    parent: Option<Id>,
) -> Option<&'static str> {
    let under_object_generic = parent.is_some_and(|p| match &t.nodes[p] {
        N::Generic { left, elements, .. } => {
            !elements.is_empty() && matches!(&t.nodes[*left], N::Name(v) if v == "Object")
        }
        _ => false,
    });
    for &native in NATIVE {
        if native == "object" && (!prefs.get(name).is_some_and(truthy) || under_object_generic) {
            continue;
        }
        if native != name
            && native.to_lowercase() == name.to_lowercase()
            && !prefs.contains_key(native)
        {
            return Some(native);
        }
    }
    None
}

/// Rewrites a matched name (and its parent generic's brackets) to the preferred spelling, as
/// the fix will print it.
fn adjust(
    t: &mut TypeTree,
    id: Id,
    parent: Option<Id>,
    preferred: &str,
    generic: bool,
    name: &str,
) {
    let mut ret = preferred.to_owned();
    if generic {
        if let Some(N::Generic { square, dot, .. }) = parent.map(|p| &mut t.nodes[p]) {
            if preferred == "[]" {
                *square = true;
                *dot = false;
                ret = "Array".into();
            } else if let Some(s) = preferred
                .strip_suffix(".<>")
                .or_else(|| preferred.strip_suffix('.'))
            {
                *square = false;
                *dot = true;
                ret = s.to_owned();
            } else if let Some(s) = preferred.strip_suffix("<>") {
                *square = false;
                *dot = false;
                ret = s.to_owned();
            } else if *square && (name == "[]" || name == "Array") {
                *square = false;
                *dot = false;
            }
        }
    } else if matches!(t.nodes[id], N::Any) {
        t.nodes[id] = N::Name(String::new());
    }
    let value = if ret.is_empty() {
        name.to_owned()
    } else {
        [".<>", "<>", ".", "[]"]
            .iter()
            .find_map(|s| ret.strip_suffix(s))
            .unwrap_or(&ret)
            .to_owned()
    };
    if let N::Name(v) = &mut t.nodes[id] {
        *v = value;
    }
}

const SETTINGS_ERROR: &str =
    "Invalid `settings.jsdoc.preferredTypes`. Values must be falsy, a string, or an object.";

impl Rule for CheckTypes {
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
        let structured: Vec<(String, Value)> = settings_value
            .get("structuredTags")
            .and_then(Value::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        each_block(cx, |cx, finder, doc| {
            let mode = finder.settings.mode.as_str();
            let prefs = preferred_types(&settings_value, mode);
            let lookup = Lookup {
                prefs: &prefs,
                unify: self.unify,
            };
            let structure = Structure::new(SMode::from_setting(mode), &settings_value);
            let typed: Vec<usize> = (0..doc.block.tags.len())
                .filter(|&i| structure.might_type(&doc.block.tags[i].tag))
                .collect();
            let comment_text = doc.comment.text(finder.src).to_owned();
            for (idx, &ti) in typed.iter().enumerate() {
                let tag = &doc.block.tags[ti];
                let parsed = match jt::Mode::from_setting(mode) {
                    Some(m) => jt::parse(&tag.typ, m, false),
                    None => jt::try_parse(&tag.typ, false),
                };
                let Some(mut tree) = parsed else {
                    continue;
                };
                let mut invalid: Vec<Invalid> = Vec::new();
                for (id, parent, prop) in tree.walk() {
                    let Some(value) = node_name(&tree.nodes[id]) else {
                        continue;
                    };
                    let is_any = matches!(tree.nodes[id], N::Any);
                    let (matched, key, generic) = lookup.info(&tree, &value, parent, prop);
                    let mut preferred: Option<String> = None;
                    if matched {
                        let bad = if key == "[]" {
                            key.clone()
                        } else {
                            value.clone()
                        };
                        let setting = prefs.get(&key).cloned().unwrap_or(Value::Null);
                        match &setting {
                            s if !truthy(s) => invalid.push(Invalid {
                                bad: bad.clone(),
                                preferred: None,
                                message: None,
                            }),
                            Value::String(s) => {
                                preferred = Some(s.clone());
                                invalid.push(Invalid {
                                    bad: bad.clone(),
                                    preferred: Some(setting.clone()),
                                    message: None,
                                });
                            }
                            Value::Object(o) => {
                                let skip_root = o.get("skipRootChecking").is_some_and(truthy);
                                let next = typed.get(idx + 1).map(|&n| &doc.block.tags[n]);
                                let covered = skip_root
                                    && next.is_some_and(|n| {
                                        n.name.starts_with(&format!("{}.", tag.name))
                                    });
                                if !covered {
                                    let replacement = o.get("replacement").cloned();
                                    preferred = replacement
                                        .as_ref()
                                        .and_then(Value::as_str)
                                        .filter(|s| !s.is_empty())
                                        .map(str::to_owned);
                                    invalid.push(Invalid {
                                        bad: bad.clone(),
                                        preferred: replacement,
                                        message: o
                                            .get("message")
                                            .and_then(Value::as_str)
                                            .filter(|m| !m.is_empty())
                                            .map(str::to_owned),
                                    });
                                }
                            }
                            _ => {
                                cx.report(Span::at(0), SETTINGS_ERROR).loc(Loc::at(1, 2));
                                continue;
                            }
                        }
                        if let Some(p) = &preferred {
                            adjust(&mut tree, id, parent, p, generic, &bad);
                        }
                        continue;
                    }
                    let allowed = structured.iter().find_map(|(t, v)| {
                        let types = v.get("type")?.as_array()?;
                        (*t == tag.tag && !types.iter().any(|x| x.as_str() == Some(&value)))
                            .then(|| Value::Array(types.clone()))
                    });
                    if let Some(types) = allowed {
                        invalid.push(Invalid {
                            bad: value,
                            preferred: Some(types),
                            message: None,
                        });
                    } else if !self.no_defaults
                        && !is_any
                        && let Some(native) = native_casing(&tree, &prefs, &value, parent)
                    {
                        invalid.push(Invalid {
                            bad: value.clone(),
                            preferred: Some(Value::from(native)),
                            message: None,
                        });
                        adjust(&mut tree, id, parent, native, generic, &value);
                    }
                }
                if invalid.is_empty() {
                    continue;
                }
                let fixed = tree.stringify();
                let fix_text =
                    comment_text.replacen(&format!("{{{}}}", tag.typ), &format!("{{{fixed}}}"), 1);
                let tag_value = if tag.name.is_empty() {
                    String::new()
                } else {
                    format!(" \"{}\"", tag.name)
                };
                let exempt = self.exempt.iter().any(|(t, types)| {
                    *t == tag.tag && types.as_ref().is_none_or(|ts| ts.contains(&tag.typ))
                });
                if exempt {
                    continue;
                }
                let span = doc.line_span(finder, doc.block.tag_line(ti));
                for inv in invalid {
                    let pref = inv.preferred.filter(truthy);
                    let msg = match &inv.message {
                        Some(m) => interpolate(m, &tag.tag, &tag_value),
                        None => match &pref {
                            Some(p) => format!(
                                "Invalid JSDoc @{}{tag_value} type \"{}\"; prefer: {}.",
                                tag.tag,
                                inv.bad,
                                serde_json::to_string(p).unwrap_or_default()
                            ),
                            None => {
                                format!(
                                    "Invalid JSDoc @{}{tag_value} type \"{}\".",
                                    tag.tag, inv.bad
                                )
                            }
                        },
                    };
                    let r = cx.report(span, msg);
                    if pref.is_some() {
                        r.fix(Fix::replace(doc.comment, fix_text.clone()));
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(
            interpolate(
                "Bad @{{tagName}}{{ tagValue }} {{other}}",
                "param",
                " \"a\""
            ),
            "Bad @param \"a\" {{other}}"
        );
        assert_eq!(interpolate("open {{ only", "x", "y"), "open {{ only");
    }
}
