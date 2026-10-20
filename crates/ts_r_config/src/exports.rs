//! package.json `exports` / `imports` map evaluation (Node's documented semantics plus
//! TypeScript's "keep trying the next condition when a target doesn't resolve").
//!
//! Lives in `ts_r_config` because tsconfig `extends` needs it; the resolver reuses it.

use crate::jsonc::{Value, ValueKind};

/// Result of evaluating an exports/imports map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<T> {
    /// A target resolved.
    Found(T),
    /// The map explicitly excludes the subpath (`null` target).
    Excluded,
    /// No entry matched (or no target resolved).
    NotMatched,
}

impl<T> Lookup<T> {
    /// `Some` for [`Lookup::Found`].
    pub fn found(self) -> Option<T> {
        match self {
            Lookup::Found(t) => Some(t),
            _ => None,
        }
    }
}

/// How a target string is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    /// `exports`: targets must start with `./`.
    Exports,
    /// `imports`: targets may also be bare package specifiers.
    Imports,
}

/// True when `exports` is a subpath map (keys start with `.`) rather than a condition map
/// or a single target.
pub fn is_subpath_map(exports: &Value) -> bool {
    match exports.as_object() {
        Some(members) => members.first().is_some_and(|m| m.key.starts_with('.')),
        None => false,
    }
}

/// Finds the entry for `key` (`.`, `./sub`, `#imp`) in a subpath map: exact keys first, then
/// the most specific `*` pattern or legacy trailing-`/` folder key (a folder key `./dir/`
/// ranks like the pattern `./dir/*`). Returns the value and the `*` replacement (folder
/// remainders are prefixed with `\0`).
pub fn match_subpath<'v>(map: &'v Value, key: &str) -> Option<(&'v Value, Option<String>)> {
    let members = map.as_object()?;
    if !key.contains('*')
        && let Some(m) = members
            .iter()
            .rev()
            .find(|m| m.key == key && !m.key.contains('*'))
    {
        return Some((&m.value, None));
    }
    // (value, replacement, key ranked as a pattern)
    let mut best: Option<(&'v Value, String, String)> = None;
    for m in members {
        let k = m.key.as_str();
        let candidate = if let Some(star) = k.find('*') {
            if k[star + 1..].contains('*') {
                continue;
            }
            let (prefix, suffix) = (&k[..star], &k[star + 1..]);
            if !(key.len() >= k.len()
                && key.starts_with(prefix)
                && key.ends_with(suffix)
                && key != prefix)
            {
                continue;
            }
            (
                key[prefix.len()..key.len() - suffix.len()].to_owned(),
                k.to_owned(),
            )
        } else if k.ends_with('/') && key.starts_with(k) {
            (format!("\u{0}{}", &key[k.len()..]), format!("{k}*"))
        } else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(_, _, bk)| pattern_key_compare(&candidate.1, bk))
        {
            best = Some((&m.value, candidate.0, candidate.1));
        }
    }
    best.map(|(v, r, _)| (v, Some(r)))
}

/// Node's PATTERN_KEY_COMPARE: true when `a` sorts before (is more specific than) `b`.
fn pattern_key_compare(a: &str, b: &str) -> bool {
    let a_star = a.find('*').unwrap_or(a.len());
    let b_star = b.find('*').unwrap_or(b.len());
    let a_base = a_star + 1;
    let b_base = b_star + 1;
    if a_base != b_base {
        return a_base > b_base;
    }
    a.len() > b.len()
}

/// Evaluates a target value (string / array / condition object / null).
///
/// `replacement` is the `*` match (or a `\0`-prefixed folder remainder). `try_target`
/// receives the substituted target string and returns `Some` when it resolves.
pub fn resolve_target<T>(
    target: &Value,
    replacement: Option<&str>,
    conditions: &dyn Fn(&str) -> bool,
    kind: MapKind,
    try_target: &mut dyn FnMut(&str) -> Option<T>,
) -> Lookup<T> {
    match &target.kind {
        ValueKind::String(s) => {
            let valid = match kind {
                MapKind::Exports => s.starts_with("./"),
                MapKind::Imports => !s.starts_with("../") && !s.starts_with('/'),
            };
            if !valid {
                return Lookup::NotMatched;
            }
            let substituted = match replacement {
                Some(r) if r.starts_with('\u{0}') => format!("{s}{}", &r[1..]),
                Some(r) => s.replace('*', r),
                None => s.clone(),
            };
            match try_target(&substituted) {
                Some(t) => Lookup::Found(t),
                None => Lookup::NotMatched,
            }
        }
        ValueKind::Null => Lookup::Excluded,
        ValueKind::Array(items) => {
            for it in items {
                match resolve_target(it, replacement, conditions, kind, try_target) {
                    Lookup::Found(t) => return Lookup::Found(t),
                    Lookup::Excluded | Lookup::NotMatched => continue,
                }
            }
            Lookup::NotMatched
        }
        ValueKind::Object(members) => {
            for m in members {
                if m.key == "default" || conditions(&m.key) {
                    match resolve_target(&m.value, replacement, conditions, kind, try_target) {
                        Lookup::Found(t) => return Lookup::Found(t),
                        Lookup::Excluded => return Lookup::Excluded,
                        Lookup::NotMatched => continue,
                    }
                }
            }
            Lookup::NotMatched
        }
        _ => Lookup::NotMatched,
    }
}

/// Evaluates `exports` for `subpath_key` (`.` or `./x`).
pub fn evaluate_exports<T>(
    exports: &Value,
    subpath_key: &str,
    conditions: &dyn Fn(&str) -> bool,
    try_target: &mut dyn FnMut(&str) -> Option<T>,
) -> Lookup<T> {
    if is_subpath_map(exports) {
        match match_subpath(exports, subpath_key) {
            Some((v, rep)) => {
                resolve_target(v, rep.as_deref(), conditions, MapKind::Exports, try_target)
            }
            None => Lookup::NotMatched,
        }
    } else if subpath_key == "." {
        resolve_target(exports, None, conditions, MapKind::Exports, try_target)
    } else {
        Lookup::NotMatched
    }
}

/// Convenience for simple string-set conditions: returns the first target string that
/// matches (without checking the file system).
pub fn resolve_exports(exports: &Value, subpath_key: &str, conditions: &[&str]) -> Option<String> {
    let cond = |c: &str| conditions.contains(&c);
    evaluate_exports(exports, subpath_key, &cond, &mut |t| Some(t.to_owned())).found()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonc::parse;

    fn v(s: &str) -> Value {
        parse(s, "p").root.unwrap()
    }

    #[test]
    fn string_and_conditions() {
        assert_eq!(
            resolve_exports(&v(r#""./a.js""#), ".", &[]),
            Some("./a.js".into())
        );
        let e = v(r#"{"import": "./i.js", "require": "./r.js"}"#);
        assert_eq!(
            resolve_exports(&e, ".", &["require"]),
            Some("./r.js".into())
        );
        assert_eq!(resolve_exports(&e, ".", &["import"]), Some("./i.js".into()));
        assert_eq!(resolve_exports(&e, "./x", &["import"]), None);
    }

    #[test]
    fn patterns_prefer_longest_prefix() {
        let e = v(r#"{"./*": "./a/*.js", "./sub/*": "./b/*.js", "./sub/x": null}"#);
        assert_eq!(resolve_exports(&e, "./q", &[]), Some("./a/q.js".into()));
        assert_eq!(resolve_exports(&e, "./sub/q", &[]), Some("./b/q.js".into()));
        assert_eq!(resolve_exports(&e, "./sub/x", &[]), None);
    }

    #[test]
    fn null_excludes_and_arrays_fall_back() {
        let e = v(r#"{".": [{"worker": "./w.js"}, "./main.js"], "./private/*": null}"#);
        assert_eq!(
            resolve_exports(&e, ".", &["import"]),
            Some("./main.js".into())
        );
        let r = evaluate_exports(&e, "./private/x", &|_| true, &mut |t| Some(t.to_owned()));
        assert_eq!(r, Lookup::Excluded);
    }

    #[test]
    fn invalid_targets_are_ignored() {
        let e = v(r#"{".": "../escape.js"}"#);
        assert_eq!(resolve_exports(&e, ".", &[]), None);
    }

    #[test]
    fn folder_mappings() {
        let e = v(r#"{"./dir/": "./lib/"}"#);
        assert_eq!(
            resolve_exports(&e, "./dir/a.js", &[]),
            Some("./lib/a.js".into())
        );
    }
}
