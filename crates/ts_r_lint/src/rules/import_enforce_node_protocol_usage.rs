//! `import/enforce-node-protocol-usage`: with `"always"`, a Node builtin imported without
//! the `node:` prefix (when both spellings are builtins); with `"never"`, one imported
//! with it (when the bare name is a builtin too). Specifiers come from import and
//! `export ... from` declarations, `import("x")` and `require("x")` (plain identifier
//! callee, not optional, exactly one string argument). "Builtin" follows is-core-module for
//! the Node version in `settings["import/node-version"]` (a plain `x.y.z`), or the
//! newest Node it knows when the setting is absent. The fix adds or removes `node:`
//! inside the quotes.

use super::import_util::string_literal;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/enforce-node-protocol-usage",
        "Enforce either using, or omitting, the `node:` protocol when importing Node.js builtin modules.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<EnforceNodeProtocolUsage>,
};

#[derive(Debug)]
struct EnforceNodeProtocolUsage {
    always: bool,
}

impl FromOptions for EnforceNodeProtocolUsage {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first().and_then(Value::as_str) {
            Some("always") => Ok(EnforceNodeProtocolUsage { always: true }),
            Some("never") => Ok(EnforceNodeProtocolUsage { always: false }),
            _ => Err("expected \"always\" or \"never\"".to_owned()),
        }
    }
}

const MAX: u32 = u32::MAX;

/// is-core-module 2.17's table: module name, Node version ranges (`[min, max)`, versions
/// as `major * 1_000_000 + minor * 1_000 + patch`) in which it is a builtin.
#[rustfmt::skip]
static CORE: &[(&str, &[(u32, u32)])] = &[
    ("_debug_agent", &[(1_000_000, 8_000_000)]),
    ("_debugger", &[(0, 8_000_000)]),
    ("_http_agent", &[(11_001, MAX)]),
    ("_http_client", &[(11_001, MAX)]),
    ("_http_common", &[(11_001, MAX)]),
    ("_http_incoming", &[(11_001, MAX)]),
    ("_http_outgoing", &[(11_001, MAX)]),
    ("_http_server", &[(11_001, MAX)]),
    ("_linklist", &[(0, 8_000_000)]),
    ("_stream_duplex", &[(9_004, 26_000_000)]),
    ("_stream_passthrough", &[(9_004, 26_000_000)]),
    ("_stream_readable", &[(9_004, 26_000_000)]),
    ("_stream_transform", &[(9_004, 26_000_000)]),
    ("_stream_wrap", &[(1_004_001, 26_000_000)]),
    ("_stream_writable", &[(9_004, 26_000_000)]),
    ("_tls_common", &[(11_013, MAX)]),
    ("_tls_legacy", &[(11_003, 10_000_000)]),
    ("_tls_wrap", &[(11_003, MAX)]),
    ("assert", &[(0, MAX)]),
    ("assert/strict", &[(15_000_000, MAX)]),
    ("async_hooks", &[(8_000_000, MAX)]),
    ("buffer", &[(0, MAX)]),
    ("buffer_ieee754", &[(5_000, 9_007)]),
    ("child_process", &[(0, MAX)]),
    ("cluster", &[(5_000, MAX)]),
    ("console", &[(0, MAX)]),
    ("constants", &[(0, MAX)]),
    ("crypto", &[(0, MAX)]),
    ("dgram", &[(0, MAX)]),
    ("diagnostics_channel", &[(14_017_000, 15_000_000), (15_001_000, MAX)]),
    ("dns", &[(0, MAX)]),
    ("dns/promises", &[(15_000_000, MAX)]),
    ("domain", &[(7_012, MAX)]),
    ("events", &[(0, MAX)]),
    ("freelist", &[(0, 6_000_000)]),
    ("fs", &[(0, MAX)]),
    ("fs/promises", &[(10_000_000, 10_001_000), (14_000_000, MAX)]),
    ("http", &[(0, MAX)]),
    ("http2", &[(8_008_000, MAX)]),
    ("https", &[(0, MAX)]),
    ("inspector", &[(8_000_000, MAX)]),
    ("inspector/promises", &[(19_000_000, MAX)]),
    ("module", &[(0, MAX)]),
    ("net", &[(0, MAX)]),
    ("node-inspect/lib/_inspect", &[(7_006_000, 12_000_000)]),
    ("node-inspect/lib/internal/inspect_client", &[(7_006_000, 12_000_000)]),
    ("node-inspect/lib/internal/inspect_repl", &[(7_006_000, 12_000_000)]),
    ("node:_http_agent", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_http_client", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_http_common", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_http_incoming", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_http_outgoing", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_http_server", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_stream_duplex", &[(14_018_000, 15_000_000), (16_000_000, 26_000_000)]),
    ("node:_stream_passthrough", &[(14_018_000, 15_000_000), (16_000_000, 26_000_000)]),
    ("node:_stream_readable", &[(14_018_000, 15_000_000), (16_000_000, 26_000_000)]),
    ("node:_stream_transform", &[(14_018_000, 15_000_000), (16_000_000, 26_000_000)]),
    ("node:_stream_wrap", &[(14_018_000, 15_000_000), (16_000_000, 26_000_000)]),
    ("node:_stream_writable", &[(14_018_000, 15_000_000), (16_000_000, 26_000_000)]),
    ("node:_tls_common", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:_tls_wrap", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:assert", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:assert/strict", &[(16_000_000, MAX)]),
    ("node:async_hooks", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:buffer", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:child_process", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:cluster", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:console", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:constants", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:crypto", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:dgram", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:diagnostics_channel", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:dns", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:dns/promises", &[(16_000_000, MAX)]),
    ("node:domain", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:events", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:ffi", &[(26_009_000, MAX)]),
    ("node:fs", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:fs/promises", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:http", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:http2", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:https", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:inspector", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:inspector/promises", &[(19_000_000, MAX)]),
    ("node:module", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:net", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:os", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:path", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:path/posix", &[(16_000_000, MAX)]),
    ("node:path/win32", &[(16_000_000, MAX)]),
    ("node:perf_hooks", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:process", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:punycode", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:querystring", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:readline", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:readline/promises", &[(17_000_000, MAX)]),
    ("node:repl", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:sea", &[(20_012_000, 21_000_000), (21_007_000, MAX)]),
    ("node:sqlite", &[(22_013_000, 23_000_000), (23_004_000, MAX)]),
    ("node:stream", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:stream/consumers", &[(16_007_000, MAX)]),
    ("node:stream/promises", &[(16_000_000, MAX)]),
    ("node:stream/web", &[(16_005_000, MAX)]),
    ("node:string_decoder", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:sys", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:test", &[(16_017_000, 17_000_000), (18_000_000, MAX)]),
    ("node:test/mock_loader", &[(22_003_000, 22_007_000)]),
    ("node:test/reporters", &[(18_017_000, 19_000_000), (19_009_000, MAX), (20_000_000, MAX)]),
    ("node:timers", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:timers/promises", &[(16_000_000, MAX)]),
    ("node:tls", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:trace_events", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:tty", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:url", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:util", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:util/types", &[(16_000_000, MAX)]),
    ("node:v8", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:vm", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:wasi", &[(18_017_000, 19_000_000), (20_000_000, MAX)]),
    ("node:worker_threads", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("node:zlib", &[(14_018_000, 15_000_000), (16_000_000, MAX)]),
    ("os", &[(0, MAX)]),
    ("path", &[(0, MAX)]),
    ("path/posix", &[(15_003_000, MAX)]),
    ("path/win32", &[(15_003_000, MAX)]),
    ("perf_hooks", &[(8_005_000, MAX)]),
    ("process", &[(1_000_000, MAX)]),
    ("punycode", &[(5_000, MAX)]),
    ("querystring", &[(0, MAX)]),
    ("readline", &[(0, MAX)]),
    ("readline/promises", &[(17_000_000, MAX)]),
    ("repl", &[(0, MAX)]),
    ("smalloc", &[(11_005, 3_000_000)]),
    ("stream", &[(0, MAX)]),
    ("stream/consumers", &[(16_007_000, MAX)]),
    ("stream/promises", &[(15_000_000, MAX)]),
    ("stream/web", &[(16_005_000, MAX)]),
    ("string_decoder", &[(0, MAX)]),
    ("sys", &[(4_000, 7_000), (8_000, MAX)]),
    ("test/mock_loader", &[(22_003_000, 22_007_000)]),
    ("test/reporters", &[(19_009_000, 20_002_000)]),
    ("timers", &[(0, MAX)]),
    ("timers/promises", &[(15_000_000, MAX)]),
    ("tls", &[(0, MAX)]),
    ("trace_events", &[(10_000_000, MAX)]),
    ("tty", &[(0, MAX)]),
    ("url", &[(0, MAX)]),
    ("util", &[(0, MAX)]),
    ("util/types", &[(15_003_000, MAX)]),
    ("v8", &[(1_000_000, MAX)]),
    ("v8/tools/arguments", &[(10_000_000, 12_000_000)]),
    ("v8/tools/codemap", &[(4_004_000, 5_000_000), (5_002_000, 12_000_000)]),
    ("v8/tools/consarray", &[(4_004_000, 5_000_000), (5_002_000, 12_000_000)]),
    ("v8/tools/csvparser", &[(4_004_000, 5_000_000), (5_002_000, 12_000_000)]),
    ("v8/tools/logreader", &[(4_004_000, 5_000_000), (5_002_000, 12_000_000)]),
    ("v8/tools/profile_view", &[(4_004_000, 5_000_000), (5_002_000, 12_000_000)]),
    ("v8/tools/splaytree", &[(4_004_000, 5_000_000), (5_002_000, 12_000_000)]),
    ("vm", &[(0, MAX)]),
    ("wasi", &[(13_004_000, 13_005_000), (18_017_000, 19_000_000), (20_000_000, MAX)]),
    ("worker_threads", &[(11_007_000, MAX)]),
    ("zlib", &[(5_000, MAX)]),
];

/// The newest Node version the table distinguishes (used without `import/node-version`).
const LATEST: u32 = 999_000_000;

fn parse_version(v: &str) -> Option<u32> {
    let mut parts = v.split('.');
    let mut out = 0u32;
    for scale in [1_000_000u32, 1_000, 1] {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 3 {
            return None;
        }
        out = out.checked_add(p.parse::<u32>().ok()?.checked_mul(scale)?)?;
    }
    parts.next().is_none().then_some(out)
}

fn is_core(name: &str, version: u32) -> bool {
    CORE.binary_search_by(|(n, _)| (*n).cmp(name))
        .is_ok_and(|i| {
            CORE[i]
                .1
                .iter()
                .any(|&(lo, hi)| lo <= version && version < hi)
        })
}

impl Rule for EnforceNodeProtocolUsage {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportDecl,
            NodeKind::ExportNamed,
            NodeKind::ImportCall,
            NodeKind::Call,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let source = match *cx.node(node) {
            Node::ImportDecl { source, .. }
            | Node::ExportNamed {
                source: Some(source),
                ..
            } => source,
            Node::ImportCall { arg, .. } => arg,
            Node::Call {
                callee,
                args,
                optional: false,
                ..
            } if ident_name(ast, callee) == Some("require") => match ast.list(args) {
                &[arg] => arg,
                _ => return,
            },
            _ => return,
        };
        let source = skip_parens(ast, source);
        let Some(name) = string_literal(ast, source) else {
            return;
        };
        let version = match cx.env().settings.get("import/node-version") {
            None | Some(Value::Null) => LATEST,
            Some(v) => match v.as_str().and_then(parse_version) {
                Some(v) => v,
                // The plugin throws on a malformed version: nothing is reported.
                None => return,
            },
        };
        let span = cx.span(source);
        let inner = span.start + 1;
        if self.always {
            if name.starts_with("node:")
                || !is_core(name, version)
                || !is_core(&format!("node:{name}"), version)
            {
                return;
            }
            cx.report(span, format!("Prefer `node:{name}` over `{name}`."))
                .fix(Fix::insert(inner, "node:"));
        } else {
            let Some(bare) = name.strip_prefix("node:") else {
                return;
            };
            if !is_core(bare, version) {
                return;
            }
            cx.report(span, format!("Prefer `{bare}` over `node:{bare}`."))
                .fix(Fix::replace(Span::new(inner, inner + 5), ""));
        }
    }
}
