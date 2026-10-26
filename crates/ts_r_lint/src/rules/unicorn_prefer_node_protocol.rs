//! `unicorn/prefer-node-protocol`: import Node.js builtin modules with the `node:` prefix.
//!
//! A module specifier is checked where it is the source of an import, re-export, `import()`
//! or `import("x")` type, or the single argument of a plain `require(x)` or
//! `process.getBuiltinModule(x)` call (no optional chaining, no spread). Its value (a string
//! or a template without substitutions, escapes decoded) must be a builtin both bare and with
//! `node:`, so `test` or `sqlite`, which exist only as `node:test`, are left alone. The fix
//! inserts `node:` after the opening quote.

use super::prelude::*;
use super::ts_no_require_imports::plain_string;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-node-protocol",
        "Prefer using the `node:` protocol when importing Node.js builtin modules.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferNodeProtocol>,
};

/// Builtins that Node also accepts without the `node:` prefix (the `builtin-modules` list
/// eslint-plugin-unicorn checks against).
pub(crate) const BARE_BUILTINS: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "inspector/promises",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/iter",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
    "zlib/iter",
];

#[derive(Debug, Default)]
struct PreferNodeProtocol;

impl Rule for PreferNodeProtocol {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportDecl,
            NodeKind::ExportNamed,
            NodeKind::ExportAll,
            NodeKind::ImportCall,
            NodeKind::ImportType,
            NodeKind::Call,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let source = match *cx.node(node) {
            Node::ImportDecl { source, .. } | Node::ExportAll { source, .. } => source,
            Node::ExportNamed {
                source: Some(source),
                ..
            } => source,
            Node::ImportCall { arg, .. } | Node::ImportType { arg, .. } => arg,
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let [arg] = ast.list(args) else {
                    return;
                };
                if matches!(ast.node(*arg), Node::Spread { .. }) || !is_loader(cx, callee) {
                    return;
                }
                *arg
            }
            _ => return,
        };
        let source = skip_parens(ast, source);
        let Some(value) = plain_string(cx, source) else {
            return;
        };
        if !BARE_BUILTINS.contains(&value) {
            return;
        }
        let span = cx.span(source);
        cx.report(span, format!("Prefer `node:{value}` over `{value}`."))
            .id("prefer-node-protocol")
            .fix(Fix::insert(span.start + 1, "node:"));
    }
}

/// `require` or `process.getBuiltinModule` (not optional, not computed).
fn is_loader(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let ast = cx.ast();
    let callee = skip_parens(ast, callee);
    match *ast.node(callee) {
        Node::Ident { .. } => ident_name(ast, callee) == Some("require"),
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            ident_name(ast, skip_parens(ast, obj)) == Some("process")
                && ident_name(ast, prop) == Some("getBuiltinModule")
        }
        _ => false,
    }
}
