//! `import/no-nodejs-modules`: imports and `require()`s of Node.js builtin modules (with
//! or without `node:`), except names listed in `allow`. Names in the
//! `import/core-modules` setting count as builtins too.

use super::import_util::{MODULE_KINDS, ModuleSystems, module_refs, string_literal};
use super::prelude::*;
use super::unicorn_prefer_node_protocol::BARE_BUILTINS;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-nodejs-modules",
        "Forbid Node.js builtin modules.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoNodejsModules>,
};

#[derive(Debug)]
struct NoNodejsModules {
    allow: Vec<String>,
}

impl FromOptions for NoNodejsModules {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoNodejsModules {
            allow: str_list(obj_opt(opts, 0), "allow"),
        })
    }
}

/// Builtins Node only accepts with the `node:` prefix.
const PREFIX_ONLY: &[&str] = &["sea", "sqlite", "test", "test/reporters"];

/// The package part of a specifier: `@scope/name` or the first path segment.
pub(crate) fn base_module(name: &str) -> &str {
    let mut parts = name.match_indices('/').map(|(i, _)| i);
    let cut = if name.starts_with('@') {
        parts.nth(1)
    } else {
        parts.next()
    };
    cut.map_or(name, |i| &name[..i])
}

/// True for a Node.js builtin module name (`fs`, `fs/promises`, `node:test`).
pub(crate) fn is_builtin(cx: &Ctx<'_, '_>, name: &str) -> bool {
    let base = base_module(name);
    let core = match base.strip_prefix("node:") {
        Some(b) => BARE_BUILTINS.contains(&b) || PREFIX_ONLY.contains(&b),
        None => BARE_BUILTINS.contains(&base),
    };
    core || cx.env().settings["import/core-modules"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(base)))
}

impl Rule for NoNodejsModules {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let systems = ModuleSystems {
            esmodule: true,
            commonjs: true,
            amd: false,
        };
        for r in module_refs(ast, node, systems) {
            let name = string_literal(ast, r.source).unwrap_or("");
            if !self.allow.iter().any(|a| a == name) && is_builtin(cx, name) {
                cx.report_node(
                    r.importer,
                    format!("Do not import Node.js builtin module \"{name}\""),
                );
            }
        }
    }
}
