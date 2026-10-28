//! `jsdoc/match-name`: tag names (the name part, `@param name`) must match `allowName` and
//! must not match `disallowName`, per `match` entry.
//!
//! The `match` entries are the rule's contexts (`context` defaulting to `any`, an optional
//! `comment` selector); under `any` each block is checked once per entry whose `comment`
//! matches it, with that entry's options. Names lose a leading `[` and a trailing
//! `=default]` / `]`. An entry with neither regex reports the first applicable tag
//! ("Prohibited context") on the whole block. With `disallowName` and `replacement` the fix
//! replaces the regex match in the tag's name token; the fixes of one block accumulate
//! (each rewrites the whole comment with every rename so far), as the plugin's do.

use super::prelude::*;
use crate::jsdoc::{self, Contexts, Settings};
use serde_json::Map;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/match-name",
        "Reports the name portion of a JSDoc tag if matching or not matching a given regular expression.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<MatchName>,
};

#[derive(Debug, Default)]
struct Entry {
    allow: Option<(String, String)>,
    disallow: Option<(String, String)>,
    replacement: Option<String>,
    tags: Vec<String>,
    message: Option<String>,
}

#[derive(Debug, Default)]
struct MatchName {
    /// `{ "contexts": match }`, or `None` without a `match` option.
    opts: Option<Map<String, Value>>,
    entries: Vec<Entry>,
}

/// `getRegexFromString`: `/pattern/flags` or a bare pattern (flag `v`).
fn regex_of(s: &str) -> (String, String) {
    if let Some(r) = s.strip_prefix('/')
        && let Some(i) = r.rfind('/')
    {
        let flags = &r[i + 1..];
        if flags.chars().all(|c| "dgimsuvy".contains(c)) {
            let flags = if flags.is_empty() { "v" } else { flags };
            return (r[..i].to_owned(), flags.to_owned());
        }
    }
    (s.to_owned(), "v".to_owned())
}

fn regex_test(re: &(String, String), text: &str) -> bool {
    crate::regex::parse(&re.0, crate::regex::Flags::new(&re.1))
        .is_ok_and(|r| r.is_match(text, re.1.contains('i')))
}

fn regex_display(re: &(String, String)) -> String {
    format!("/{}/{}", re.0, re.1)
}

/// `name.replace(re, replacement)`: the first match (every match with `g`), `$$` and `$&`
/// expanded in the replacement.
fn regex_replace(re: &(String, String), name: &str, replacement: &str) -> String {
    let Some(matches) = crate::regex_exec::match_all(&re.0, &re.1, name) else {
        return name.to_owned();
    };
    let mut out = String::new();
    let mut rest = name;
    for m in matches {
        let Some(Some(whole)) = m.first() else {
            continue;
        };
        let Some(at) = rest.find(whole.as_str()) else {
            continue;
        };
        out.push_str(&rest[..at]);
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            match (c, chars.peek()) {
                ('$', Some('$')) => {
                    chars.next();
                    out.push('$');
                }
                ('$', Some('&')) => {
                    chars.next();
                    out.push_str(whole);
                }
                _ => out.push(c),
            }
        }
        rest = &rest[at + whole.len()..];
        if whole.is_empty() {
            // An empty match: copy one character so the scan moves on.
            let mut it = rest.chars();
            if let Some(c) = it.next() {
                out.push(c);
            }
            rest = it.as_str();
        }
    }
    out.push_str(rest);
    out
}

impl FromOptions for MatchName {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let Some(list) = opts
            .first()
            .and_then(|o| o.get("match"))
            .and_then(Value::as_array)
        else {
            return Ok(MatchName::default());
        };
        let mut entries = Vec::new();
        for v in list {
            let o = v
                .as_object()
                .ok_or("match-name: `match` entries are objects")?;
            let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
            entries.push(Entry {
                allow: s("allowName").map(|r| regex_of(&r)),
                disallow: s("disallowName").map(|r| regex_of(&r)),
                replacement: s("replacement"),
                tags: match o.get("tags").and_then(Value::as_array) {
                    Some(a) => a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                    None => vec!["*".to_owned()],
                },
                message: s("message"),
            });
        }
        let mut m = Map::new();
        m.insert("contexts".to_owned(), Value::Array(list.clone()));
        Contexts::from_options(&m, &Settings::default_for_validation())?;
        Ok(MatchName {
            opts: Some(m),
            entries,
        })
    }
}

/// The name without a leading `[` and a trailing `]` or `=...]`.
fn bare_name(name: &str) -> &str {
    let n = name.strip_prefix('[').unwrap_or(name);
    if let Some(head) = n.strip_suffix(']') {
        return match head.find('=') {
            Some(i) => &head[..i],
            None => head,
        };
    }
    n
}

impl Rule for MatchName {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = Settings::from_cx(cx);
        let Some(opts) = &self.opts else {
            let Ok(contexts) = Contexts::from_options(&Map::new(), &settings) else {
                return;
            };
            jsdoc::for_each_doc(cx, &settings, &contexts, |cx, _f, doc| {
                cx.report(
                    doc.comment,
                    "Rule `no-restricted-syntax` is missing a `match` option.",
                );
            });
            return;
        };
        let Ok(mut contexts) = Contexts::from_options(opts, &settings) else {
            return;
        };
        contexts.additive = true;
        jsdoc::for_each_doc(cx, &settings, &contexts, |cx, f, doc| {
            let Some(e) = doc.last_index.and_then(|i| self.entries.get(i)) else {
                return;
            };
            let all = e.tags.iter().any(|t| t == "*");
            let applicable: Vec<usize> = (0..doc.block.tags.len())
                .filter(|&i| all || e.tags.contains(&doc.block.tags[i].tag))
                .collect();
            let has_regex = e.allow.is_some() || e.disallow.is_some();
            let mut reported = false;
            for i in applicable {
                let tag_name = bare_name(&doc.block.tags[i].name).to_owned();
                let allowed = e.allow.as_ref().is_none_or(|r| regex_test(r, &tag_name));
                let disallowed = e
                    .disallow
                    .as_ref()
                    .is_some_and(|r| regex_test(r, &tag_name));
                if has_regex && allowed && !disallowed {
                    continue;
                }
                if !has_regex && reported {
                    continue;
                }
                let message = match &e.message {
                    Some(m) => m.clone(),
                    None if has_regex && disallowed => format!(
                        "Only allowing names not matching `{}` but found \"{tag_name}\".",
                        e.disallow.as_ref().map(regex_display).unwrap_or_default()
                    ),
                    None if has_regex => format!(
                        "Only allowing names matching `{}` but found \"{tag_name}\".",
                        e.allow.as_ref().map(regex_display).unwrap_or_default()
                    ),
                    None => format!("Prohibited context for \"{tag_name}\"."),
                };
                let span = if has_regex {
                    doc.line_span(f, doc.block.tag_line(i))
                } else {
                    doc.comment
                };
                let fix = match (&e.disallow, &e.replacement) {
                    (Some(re), Some(rep)) => {
                        let b = &mut doc.block;
                        let line = b.tags[i]
                            .source
                            .iter()
                            .copied()
                            .find(|&id| !b.lines[id].tokens.name.is_empty());
                        if let Some(id) = line {
                            let n = regex_replace(re, &b.lines[id].tokens.name, rep);
                            b.lines[id].tokens.name = n;
                        }
                        Some(doc.fix())
                    }
                    _ => None,
                };
                let r = cx.report(span, message);
                if let Some(fix) = fix {
                    r.fix(fix);
                }
                if !has_regex {
                    reported = true;
                }
            }
        });
    }
}
