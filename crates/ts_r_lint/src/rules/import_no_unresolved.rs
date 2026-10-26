//! `import/no-unresolved`: module specifiers no configured resolver finds. On a
//! case-insensitive file system it also reports a specifier whose case differs from the
//! files on disk (`caseSensitive`, on by default, checks the path below the cwd;
//! `caseSensitiveStrict` checks all of it). Type-only imports and exports are skipped.

use super::import_util::{MODULE_KINDS, VisitorOptions, is_type_only_declaration, resolve};
use super::prelude::*;
use ts_r_config::path;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-unresolved",
        "Ensure imports point to a file/module that can be resolved.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoUnresolved>,
};

#[derive(Debug)]
struct NoUnresolved {
    visitor: VisitorOptions,
    case_sensitive: bool,
    case_sensitive_strict: bool,
}

impl FromOptions for NoUnresolved {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnresolved {
            visitor: VisitorOptions::from_options(opts, true, false),
            case_sensitive: bool_field(o, "caseSensitive", true),
            case_sensitive_strict: bool_field(o, "caseSensitiveStrict", false),
        })
    }
}

/// Every component of `p` (below the cwd unless `strict`) is listed under that name in its
/// directory.
pub(crate) fn exists_with_case(
    fs: &dyn ts_r_config::FileSystem,
    p: &str,
    cwd: &str,
    strict: bool,
) -> bool {
    let mut cur = p.to_owned();
    for _ in 0..256 {
        if !strict && path::eq(&cur, cwd, false) {
            return true;
        }
        let dir = path::directory_of(&cur).to_owned();
        if dir.is_empty() || dir == cur {
            return true;
        }
        let base = path::file_name(&cur);
        if !fs.read_dir(&dir).iter().any(|e| e.name == base) {
            return false;
        }
        cur = dir;
    }
    true
}

impl Rule for NoUnresolved {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if is_type_only_declaration(ast, node) {
            return;
        }
        for (r, value) in self.visitor.refs(ast, node) {
            let Some(resolved) = resolve(cx, value) else {
                continue;
            };
            let span = cx.span(r.source);
            if !resolved.found() {
                cx.report(span, format!("Unable to resolve path to module '{value}'."));
                continue;
            }
            let m = cx.file.modules();
            let insensitive = !m.fs().case_sensitive();
            let check = insensitive && (self.case_sensitive || self.case_sensitive_strict);
            if check
                && let Some(p) = resolved.path()
                && !exists_with_case(m.fs(), p, &m.cwd, self.case_sensitive_strict)
            {
                cx.report(
                    span,
                    format!("Casing of {value} does not match the underlying filesystem."),
                );
            }
        }
    }
}
