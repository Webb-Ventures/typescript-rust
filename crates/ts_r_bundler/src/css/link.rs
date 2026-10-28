//! Bundle-wide CSS decisions: the final names of local names (`local-css`), the class lists
//! JavaScript sees (with `composes`), and the errors for references to files a stylesheet
//! can't use.
//!
//! Names (observed in esbuild 0.28.2): `<stem>_<name>`, the stem being the file name up to
//! its first `.` made an identifier (`st-y.module.css` gives `st_y_foo`); a name already
//! taken, by an earlier stylesheet in import order or by a global class or ID anywhere in
//! the bundle, gets `2`, `3`, ... appended. With `--minify-identifiers` names are short
//! (our sequence; esbuild's frequency-ordered choice is not reproduced). A class's value
//! is its `composes` names (in order, recursively) followed by its own name.

use rustc_hash::{FxHashMap, FxHashSet};

use super::{ComposeFrom, Sheet};
use crate::graph::{Graph, ModuleId, Target};
use crate::link::Link;
use crate::meta::RecordKind;
use crate::options::Loader;
use crate::{Message, Severity};

/// Final local names and JavaScript values, per module (empty for other modules).
#[derive(Clone, Debug, Default)]
pub struct CssNames {
    pub names: Vec<Vec<String>>,
    pub values: Vec<Vec<String>>,
}

/// Limit for `composes` chains.
const MAX_COMPOSE_DEPTH: usize = 64;

pub fn assign(g: &Graph, l: &Link, minify: bool) -> CssNames {
    let n = g.modules.len();
    let mut names = vec![Vec::new(); n];
    let mut taken: FxHashSet<String> = FxHashSet::default();
    for (i, m) in g.modules.iter().enumerate() {
        if l.included[i]
            && let Some(sheet) = &m.source.css
        {
            taken.extend(sheet.globals.iter().cloned());
        }
    }
    let mut cursor = 0usize;
    for (i, m) in g.modules.iter().enumerate() {
        let Some(sheet) = &m.source.css else { continue };
        if !l.included[i] || sheet.locals.is_empty() {
            continue;
        }
        let file = crate::graph::split_query(&m.path).0;
        let file = file.rsplit('/').next().unwrap_or(file);
        let stem = file.split('.').next().unwrap_or(file);
        let stem = ts_r_minify::names::sanitize(stem);
        names[i] = sheet
            .locals
            .iter()
            .map(|local| {
                if minify {
                    let n = ts_r_minify::rename::next_free_name(&mut cursor, &|c| {
                        // `$` is not a CSS name character.
                        taken.contains(c) || c.contains('$')
                    });
                    taken.insert(n.clone());
                    return n;
                }
                let base = format!("{stem}_{local}");
                let mut cand = base.clone();
                let mut k = 2u32;
                while taken.contains(&cand) {
                    cand = format!("{base}{k}");
                    k += 1;
                }
                taken.insert(cand.clone());
                cand
            })
            .collect();
    }
    let mut out = CssNames {
        names,
        values: vec![Vec::new(); n],
    };
    for i in 0..n {
        let Some(sheet) = &g.modules[i].source.css else {
            continue;
        };
        if out.names[i].is_empty() {
            continue;
        }
        let values = (0..sheet.locals.len() as u32)
            .map(|k| {
                let mut list = Vec::new();
                let mut seen = Vec::new();
                class_list(g, &out, i as ModuleId, k, &mut list, &mut seen, 0);
                list.join(" ")
            })
            .collect();
        out.values[i] = values;
    }
    out
}

/// Appends local `k` of `m`'s class list: what it composes, then itself.
fn class_list(
    g: &Graph,
    names: &CssNames,
    m: ModuleId,
    k: u32,
    list: &mut Vec<String>,
    seen: &mut Vec<(ModuleId, u32)>,
    depth: usize,
) {
    if depth > MAX_COMPOSE_DEPTH || seen.contains(&(m, k)) {
        return;
    }
    seen.push((m, k));
    let module = &g.modules[m as usize];
    let Some(sheet) = &module.source.css else {
        return;
    };
    for c in sheet.composes.iter().filter(|c| c.class == k) {
        for name in &c.names {
            match c.from {
                ComposeFrom::Global => push(list, name.clone()),
                ComposeFrom::Local => {
                    if let Some(j) = sheet.locals.iter().position(|x| x == name) {
                        class_list(g, names, m, j as u32, list, seen, depth + 1);
                    }
                }
                ComposeFrom::Record(r) => {
                    if let Some(Target::Module(t)) = module.targets.get(r as usize)
                        && let Some(ts) = &g.modules[*t as usize].source.css
                        && let Some(j) = ts.locals.iter().position(|x| x == name)
                    {
                        class_list(g, names, *t, j as u32, list, seen, depth + 1);
                    }
                }
            }
        }
    }
    if let Some(own) = names.names[m as usize].get(k as usize) {
        push(list, own.clone());
    }
}

fn push(list: &mut Vec<String>, s: String) {
    if !list.contains(&s) {
        list.push(s);
    }
}

/// Errors for references a stylesheet can't use: `@import` of a non-CSS file, `url()` of
/// JavaScript, JSON or CSS, `composes` from a non-CSS file or of a name the file lacks.
pub fn check(g: &Graph, l: &Link) -> Vec<Message> {
    let mut out = Vec::new();
    for (i, m) in g.modules.iter().enumerate() {
        let Some(sheet) = &m.source.css else { continue };
        if !l.included[i] {
            continue;
        }
        for (r, rec) in m.meta.records.iter().enumerate() {
            let Some(Target::Module(t)) = m.targets.get(r) else {
                continue;
            };
            let tm = &g.modules[*t as usize];
            let what = tm.pretty.clone();
            let loader = tm.loader.as_str();
            let text = match rec.kind {
                RecordKind::CssImport if !tm.loader.is_css() && tm.loader != Loader::Empty => {
                    Some(format!(
                        "Cannot import {what:?} into a CSS file\nAn \"@import\" rule can only be used to import another CSS file and {what:?} is not a CSS file (it was loaded with the {loader:?} loader)."
                    ))
                }
                RecordKind::CssUrl if tm.loader.is_css() => Some(format!(
                    "Cannot use {what:?} as a URL\nYou can't use a \"url()\" token to reference a CSS file, and {what:?} is a CSS file (it was loaded with the {loader:?} loader)."
                )),
                RecordKind::CssUrl if tm.loader.is_js() || tm.loader == Loader::Json => {
                    Some(format!(
                        "Cannot use {what:?} as a URL\nYou can't use a \"url()\" token to reference the file {what:?} because it was loaded with the {loader:?} loader, which doesn't provide a URL to embed in the resulting CSS."
                    ))
                }
                RecordKind::CssComposes if !tm.loader.is_css() => Some(format!(
                    "Cannot use \"composes\" with {what:?}\nYou can only use \"composes\" with CSS files and {what:?} is not a CSS file (it was loaded with the {loader:?} loader)."
                )),
                _ => None,
            };
            if let Some(text) = text {
                out.push(m.message_at(Severity::Error, rec.start, text));
            }
        }
        for c in &sheet.composes {
            let ComposeFrom::Record(r) = c.from else {
                continue;
            };
            let Some(Target::Module(t)) = m.targets.get(r as usize) else {
                continue;
            };
            let tm = &g.modules[*t as usize];
            let Some(ts) = &tm.source.css else { continue };
            for name in &c.names {
                let local = ts.locals.iter().any(|x| x == name);
                let text = if local {
                    None
                } else if ts.globals.contains(name) {
                    Some(format!("Cannot use global name {name:?} with \"composes\""))
                } else {
                    Some(format!(
                        "The name {name:?} never appears in {:?}",
                        tm.pretty
                    ))
                };
                if let Some(text) = text {
                    out.push(m.message_at(Severity::Error, c.pos, text));
                }
            }
        }
    }
    out
}

/// Placeholder string literal (as printed) to final value, for a module's JavaScript.
pub fn js_replacements(sheet: &Sheet, values: &[String]) -> FxHashMap<String, String> {
    let mut map = FxHashMap::default();
    for (k, v) in values.iter().enumerate() {
        map.insert(
            format!(
                "\"{}\"",
                crate::prepare::css_placeholder(&sheet.id, k as u32)
            ),
            v.clone(),
        );
    }
    map
}
