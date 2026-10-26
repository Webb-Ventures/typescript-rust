//! `import/no-relative-packages`: a relative import (`../x`, `./x`, `.`) that lands in
//! another package (the nearest `package.json` with a `name` differs from the linted
//! file's). The fix imports it by package name: `<name>/<dir of the file in the package>`,
//! plus the specifier's last segment unless it names the package directory itself.

use super::import_util::{MODULE_KINDS, VisitorOptions, resolve};
use super::prelude::*;
use crate::modules::ModCtx;
use crate::modules::import_type::{ImportType, import_type};
use ts_r_config::path;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-relative-packages",
        "Forbid importing packages through relative paths.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoRelativePackages>,
};

#[derive(Debug)]
struct NoRelativePackages {
    visitor: VisitorOptions,
}

impl FromOptions for NoRelativePackages {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoRelativePackages {
            visitor: VisitorOptions::from_options(opts, true, false),
        })
    }
}

/// The nearest `package.json` with a `name` at or above `dir` (one without a name passes
/// the search on to the directory above its own): the package directory and its name.
pub(crate) fn named_package(m: &ModCtx, dir: &str) -> Option<(String, String)> {
    let mut start = dir.to_owned();
    for _ in 0..64 {
        let found = m.host().package_dir(&start)?;
        let pkg = m.host().package_json(&found)?;
        if let Some(name) = pkg["name"].as_str().filter(|n| !n.is_empty()) {
            return Some((found, name.to_owned()));
        }
        let up = path::directory_of(&found).to_owned();
        if up == found {
            return None;
        }
        start = up;
    }
    None
}

/// Node's `path.join` of `/`-separated parts (empty parts dropped, `.`/`..` folded).
fn join(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        return ".".to_owned();
    }
    path::normalize(&joined)
}

impl Rule for NoRelativePackages {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        for (r, value) in self.visitor.refs(cx.ast(), node) {
            let Some(resolved) = resolve(cx, value) else {
                continue;
            };
            let m = cx.file.modules();
            let kind = import_type(m, value, resolved.path());
            if !matches!(
                kind,
                ImportType::Parent | ImportType::Index | ImportType::Sibling
            ) {
                continue;
            }
            let Some(target) = resolved.path() else {
                continue;
            };
            let (Some((import_root, import_name)), Some((_, own_name))) = (
                named_package(m, path::directory_of(target)),
                named_package(m, m.dir()),
            ) else {
                continue;
            };
            if import_name == own_name {
                continue;
            }
            let base = path::file_name(value);
            let proper_path = crate::modules::relative(&import_root, target);
            let proper_dir = path::directory_of(&proper_path);
            let proper_dir = if proper_dir.is_empty() {
                "."
            } else {
                proper_dir
            };
            let last = if base == path::file_name(&import_root) {
                ""
            } else {
                base
            };
            let proper = join(&[&import_name, proper_dir, last]);
            let span = cx.span(r.source);
            let text = serde_json::to_string(&proper).unwrap_or_default();
            cx.report(
                span,
                format!(
                    "Relative import from another package is not allowed. Use `{proper}` instead of `{value}`"
                ),
            )
            .fix(Fix::replace(span, text));
        }
    }
}
