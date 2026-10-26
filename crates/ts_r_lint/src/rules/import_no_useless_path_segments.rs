//! `import/no-useless-path-segments`: relative specifiers with segments that change
//! nothing: `./../x` or `./x/` (when the normalized path resolves to the same file),
//! `../../dir/x` from inside `dir`, and with `noUselessIndex` an explicit `/index` (or
//! `/index.<ext>` for the `import/extensions`). The fix writes the shorter path.

use super::import_util::{MODULE_KINDS, VisitorOptions, resolve};
use super::prelude::*;
use crate::modules::Resolved;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-useless-path-segments",
        "Forbid unnecessary path segments in import and require statements.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoUselessPathSegments>,
};

#[derive(Debug)]
struct NoUselessPathSegments {
    visitor: VisitorOptions,
    no_useless_index: bool,
}

impl FromOptions for NoUselessPathSegments {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUselessPathSegments {
            visitor: VisitorOptions {
                systems: super::import_util::ModuleSystems::from_options(opts, true, false),
                ignore: Vec::new(),
            },
            no_useless_index: bool_field(o, "noUselessIndex", false),
        })
    }
}

/// Node's `path.posix.normalize`.
fn posix_normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".to_owned();
    }
    let absolute = p.starts_with('/');
    let trailing = p.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = out.join("/");
    if s.is_empty() && !absolute {
        s.push('.');
    }
    if trailing && !s.is_empty() {
        s.push('/');
    }
    if absolute { format!("/{s}") } else { s }
}

/// Without a trailing `/`, and starting with `./` unless it starts with `.`/`..` already.
fn to_relative(p: &str) -> String {
    let stripped = p.strip_suffix('/').unwrap_or(p);
    let dotted = stripped == "."
        || stripped == ".."
        || stripped.starts_with("./")
        || stripped.starts_with("../");
    if dotted {
        stripped.to_owned()
    } else {
        format!("./{stripped}")
    }
}

/// `path.dirname` of a `/`-separated relative path.
fn dirname(p: &str) -> &str {
    let t = p.strip_suffix('/').unwrap_or(p);
    match t.rfind('/') {
        Some(0) => "/",
        Some(i) => &t[..i],
        None => ".",
    }
}

impl NoUselessPathSegments {
    fn report(cx: &mut Ctx<'_, '_>, source: NodeId, value: &str, proposed: &str) {
        let span = cx.span(source);
        let text = serde_json::to_string(proposed).unwrap_or_default();
        cx.report(
            span,
            format!("Useless path segments for \"{value}\", should be \"{proposed}\""),
        )
        .fix(Fix::replace(span, text));
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, source: NodeId, value: &str) {
        if !value.starts_with('.') {
            return;
        }
        let Some(resolved) = resolve(cx, value) else {
            return;
        };
        let normed = to_relative(&posix_normalize(value));
        if normed != value {
            let Some(resolved_normed) = resolve(cx, &normed) else {
                return;
            };
            if resolved == resolved_normed {
                return Self::report(cx, source, value, &normed);
            }
        }
        if self.no_useless_index {
            let exts = cx.file.modules().settings.file_extensions();
            if is_useless_index(value, &exts) {
                let parent = dirname(value);
                if parent != "." && parent != ".." {
                    for ext in &exts {
                        let found = resolve(cx, &format!("{parent}{ext}"));
                        if found.as_ref().and_then(Resolved::path).is_some() {
                            return Self::report(cx, source, value, &format!("{parent}/"));
                        }
                    }
                }
                return Self::report(cx, source, value, parent);
            }
        }
        if value.starts_with("./") {
            return;
        }
        let Some(target) = resolved.path() else {
            return;
        };
        let expected = crate::modules::relative(cx.file.modules().dir(), target);
        let expected_parents = expected.split('/').filter(|s| *s == "..").count();
        let rest = value.strip_prefix("./").unwrap_or(value);
        let split: Vec<&str> = rest.split('/').collect();
        let own_parents = split.iter().filter(|s| **s == "..").count();
        if own_parents <= expected_parents {
            return;
        }
        let diff = own_parents - expected_parents;
        let mut parts: Vec<&str> = split.iter().take(expected_parents).copied().collect();
        parts.extend(split.iter().skip(own_parents + diff).copied());
        let proposed = to_relative(&parts.join("/"));
        Self::report(cx, source, value, &proposed);
    }
}

/// `/index` or `/index<ext>` at the end (`.*\/index(<exts>)?$`).
fn is_useless_index(value: &str, exts: &[String]) -> bool {
    let Some(i) = value.rfind("/index") else {
        return false;
    };
    let rest = &value[i + "/index".len()..];
    rest.is_empty() || exts.iter().any(|e| e == rest)
}

impl Rule for NoUselessPathSegments {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        for (r, value) in self.visitor.refs(cx.ast(), node) {
            self.check(cx, r.source, value);
        }
    }
}
