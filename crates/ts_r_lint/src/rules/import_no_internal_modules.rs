//! `import/no-internal-modules`: imports that reach into another module's directory.
//!
//! Only `parent`, `index`, `sibling`, `external` and `internal` imports are checked. The
//! specifier's steps are folded (`.` dropped, `..` pops). With `allow` globs (the default
//! mode, minimatch 3 regex semantics) a path of more than one non-scope step is a violation unless the folded path
//! (bare or with a leading `/`) or the resolved file matches a glob; an unresolved path
//! is never one. With `forbid` globs it is a violation when the folded path or the
//! resolved file matches one.

use super::import_util::{MODULE_KINDS, ModuleSystems, VisitorOptions, resolve};
use super::prelude::*;
use crate::modules::import_type::{ImportType, import_type};
use crate::modules::minimatch;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-internal-modules",
        "Forbid importing the submodules of other modules.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoInternalModules>,
};

#[derive(Debug)]
struct NoInternalModules {
    allow: Vec<String>,
    /// `Some` when `forbid` is given (then `allow` is not used).
    forbid: Option<Vec<String>>,
}

impl FromOptions for NoInternalModules {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let globs = |k: &str| str_list(o, k);
        Ok(NoInternalModules {
            allow: globs("allow"),
            forbid: o
                .get("forbid")
                .filter(|v| !v.is_null())
                .map(|_| globs("forbid")),
        })
    }
}

fn steps(p: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for step in p.split(['/', '\\']) {
        match step {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out
}

fn any_match(globs: &[String], p: &str) -> bool {
    globs.iter().any(|g| minimatch::matches(g, p))
}

impl NoInternalModules {
    fn violation(&self, cx: &mut Ctx<'_, '_>, value: &str) -> bool {
        let steps = steps(value);
        let joined = steps.join("/");
        let rooted = format!("/{joined}");
        if let Some(forbid) = &self.forbid {
            if any_match(forbid, &joined) || any_match(forbid, &rooted) {
                return true;
            }
            let resolved = resolve(cx, value);
            return resolved
                .as_ref()
                .and_then(|r| r.path())
                .is_some_and(|p| any_match(forbid, &p.replace('\\', "/")));
        }
        if steps.iter().filter(|s| !s.starts_with('@')).count() <= 1 {
            return false;
        }
        if any_match(&self.allow, &joined) || any_match(&self.allow, &rooted) {
            return false;
        }
        let resolved = resolve(cx, value);
        match resolved.as_ref().and_then(|r| r.path()) {
            None => false,
            Some(p) => !any_match(&self.allow, &p.replace('\\', "/")),
        }
    }
}

impl Rule for NoInternalModules {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let visitor = VisitorOptions {
            systems: ModuleSystems {
                esmodule: true,
                commonjs: true,
                amd: false,
            },
            ignore: Vec::new(),
        };
        for (r, value) in visitor.refs(cx.ast(), node) {
            let Some(resolved) = resolve(cx, value) else {
                continue;
            };
            let kind = import_type(cx.file.modules(), value, resolved.path());
            let candidate = matches!(
                kind,
                ImportType::Parent
                    | ImportType::Index
                    | ImportType::Sibling
                    | ImportType::External
                    | ImportType::Internal
            );
            if candidate && self.violation(cx, value) {
                let span = cx.span(r.source);
                cx.report(span, format!("Reaching to \"{value}\" is not allowed."));
            }
        }
    }
}
