//! `import/no-absolute-path`: module specifiers that are absolute paths (`/x`), in
//! imports, re-exports, `import()`, `require()` (and AMD with `amd`). The fix rewrites the
//! path relative to the file's directory (POSIX style, `./` added when needed).

use super::import_util::{MODULE_KINDS, ModuleSystems, module_refs, string_literal};
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-absolute-path",
        "Forbid import of modules using absolute paths.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create::<NoAbsolutePath>,
};

#[derive(Debug)]
struct NoAbsolutePath {
    systems: ModuleSystems,
}

impl FromOptions for NoAbsolutePath {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoAbsolutePath {
            systems: ModuleSystems::from_options(opts, true, true),
        })
    }
}

/// `to` relative to the directory `from` (both absolute, `/`-separated).
fn posix_relative(from: &str, to: &str) -> String {
    let a: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<&str> = std::iter::repeat_n("..", a.len() - common).collect();
    parts.extend(&b[common..]);
    parts.join("/")
}

impl Rule for NoAbsolutePath {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        for r in module_refs(ast, node, self.systems) {
            let value = string_literal(ast, r.source).unwrap_or("");
            if !value.starts_with('/') {
                continue;
            }
            let file = cx.file.path.replace('\\', "/");
            let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
            let mut rel = posix_relative(dir, value);
            if !rel.starts_with('.') {
                rel = format!("./{rel}");
            }
            let text = serde_json::to_string(&rel).unwrap_or_default();
            let span = cx.span(r.source);
            cx.report(span, "Do not import modules using an absolute path")
                .fix(Fix::replace(span, text));
        }
    }
}
