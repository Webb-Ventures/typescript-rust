//! esbuild-format metafile (`--metafile`): `inputs` (bytes, imports, format) and `outputs`
//! (bytes, inputs with `bytesInOutput`, imports, exports, entryPoint). Keys are paths
//! relative to the working directory, in deterministic order.

use std::fmt::Write as _;

use ts_r_codegen::sourcemap::json_string;
use ts_r_config::path;

use crate::bundle::OutputFile;
use crate::chunk::ChunkOutput;
use crate::graph::{Graph, Target};
use crate::link::{Kind, Link};
use crate::meta::RecordKind;
use crate::options::{BuildOptions, Format};
use crate::plan::{Plan, relative_import};

fn q(s: &str) -> String {
    let mut o = String::new();
    json_string(&mut o, s);
    o
}

fn rel(opts: &BuildOptions, p: &str) -> String {
    if path::is_rooted(p) {
        path::relative(&opts.working_dir, p, true)
    } else {
        p.to_owned()
    }
}

/// Renders the metafile JSON.
pub fn render(
    graph: &Graph,
    link: &Link,
    plan: &Plan,
    chunks: &[ChunkOutput],
    outputs: &[OutputFile],
    opts: &BuildOptions,
) -> String {
    let mut s = String::from("{\n  \"inputs\": {");
    let mut first = true;
    for (i, m) in graph.modules.iter().enumerate() {
        if m.path.starts_with(crate::graph::DISABLED_PREFIX) && !link.included[i] {
            continue;
        }
        if !first {
            s.push(',');
        }
        first = false;
        let bytes = m
            .source
            .css
            .as_ref()
            .map_or(m.source.js.len(), |c| c.src.len());
        let _ = write!(
            s,
            "\n    {}: {{\n      \"bytes\": {bytes},\n      \"imports\": [",
            q(&m.pretty)
        );
        let mut fi = true;
        for (ri, r) in m.meta.records.iter().enumerate() {
            let (p, external) = match &m.targets[ri] {
                Target::Module(t) => (graph.modules[*t as usize].pretty.clone(), false),
                Target::External(e) => (crate::graph::split_external(e).0.to_owned(), true),
                Target::Missing => continue,
            };
            let kind = match r.kind {
                RecordKind::Import | RecordKind::ReExport => "import-statement",
                RecordKind::Require | RecordKind::GlobRequire => "require-call",
                RecordKind::Dynamic | RecordKind::GlobDynamic => "dynamic-import",
                RecordKind::CssImport => "import-rule",
                RecordKind::CssUrl => "url-token",
                RecordKind::CssComposes => "composes-from",
            };
            if !fi {
                s.push(',');
            }
            fi = false;
            let _ = write!(
                s,
                "\n        {{\n          \"path\": {},\n          \"kind\": \"{kind}\",{}\n          \"original\": {}\n        }}",
                q(&p),
                if external {
                    "\n          \"external\": true,"
                } else {
                    ""
                },
                q(&r.spec)
            );
        }
        s.push_str(if fi { "]" } else { "\n      ]" });
        let format = match link.kinds[i] {
            Kind::Esm if m.meta.has_esm_syntax => Some("esm"),
            Kind::Cjs if m.loader.is_js() => Some("cjs"),
            _ => None,
        };
        if let Some(f) = format {
            let _ = write!(s, ",\n      \"format\": \"{f}\"");
        }
        s.push_str("\n    }");
    }
    s.push_str(if first { "}" } else { "\n  }" });
    s.push_str(",\n  \"outputs\": {");
    let outdir = &plan.outdir;
    let mut first = true;
    let mut emit = |s: &mut String, key: &str, body: &str| {
        if !first {
            s.push(',');
        }
        first = false;
        let _ = write!(s, "\n    {}: {{{body}\n    }}", q(key));
    };
    for o in outputs {
        let key = rel(opts, &o.path);
        let chunk = plan
            .chunks
            .iter()
            .position(|c| format!("{outdir}/{}", c.file) == o.path);
        let mut body = String::new();
        match chunk {
            Some(ci) => {
                let c = &plan.chunks[ci];
                body.push_str("\n      \"imports\": [");
                let mut fi = true;
                let mut item = |body: &mut String, p: &str, kind: &str, ext: bool| {
                    if !fi {
                        body.push(',');
                    }
                    fi = false;
                    let _ = write!(
                        body,
                        "\n        {{\n          \"path\": {},\n          \"kind\": \"{kind}\"{}\n        }}",
                        q(p),
                        if ext {
                            ",\n          \"external\": true"
                        } else {
                            ""
                        }
                    );
                };
                for (oc, _) in &c.imports {
                    let p = rel(opts, &format!("{outdir}/{}", plan.chunks[*oc].file));
                    item(&mut body, &p, "import-statement", false);
                }
                for x in &c.ext_imports {
                    item(
                        &mut body,
                        crate::graph::split_external(&x.path).0,
                        "import-statement",
                        true,
                    );
                }
                for (p, _) in &c.ext_requires {
                    item(
                        &mut body,
                        crate::graph::split_external(p).0,
                        "require-call",
                        true,
                    );
                }
                // Dynamic imports of other chunks.
                for &m in &c.modules {
                    let module = &graph.modules[m as usize];
                    for (ri, r) in module.meta.records.iter().enumerate() {
                        if r.kind == RecordKind::Dynamic
                            && let Target::Module(t) = module.targets[ri]
                            && let Some(tc) = plan
                                .chunks
                                .iter()
                                .position(|x| x.entry == Some(t) && x.user_entry.is_none())
                        {
                            let p = rel(opts, &format!("{outdir}/{}", plan.chunks[tc].file));
                            let _ = relative_import(&c.file, &plan.chunks[tc].file);
                            item(&mut body, &p, "dynamic-import", false);
                        }
                    }
                }
                body.push_str(if fi { "]" } else { "\n      ]" });
                let mut exports: Vec<String> = Vec::new();
                if let Some(e) = c.entry
                    && plan.format == Format::Esm
                    && let Some(list) = &link.exports[e as usize]
                {
                    exports.extend(list.iter().map(|(n, _)| n.clone()));
                }
                if c.entry.is_none() {
                    exports.extend(c.exports.iter().map(|e| {
                        e.rsplit_once(" as ")
                            .map_or(e.clone(), |(_, b)| b.to_owned())
                    }));
                }
                exports.sort();
                exports.dedup();
                body.push_str(",\n      \"exports\": [");
                for (i, e) in exports.iter().enumerate() {
                    if i > 0 {
                        body.push_str(", ");
                    }
                    body.push_str(&q(e));
                }
                body.push(']');
                if let Some(e) = c.entry {
                    let _ = write!(
                        body,
                        ",\n      \"entryPoint\": {}",
                        q(&graph.modules[e as usize].pretty)
                    );
                }
                body.push_str(",\n      \"inputs\": {");
                let mut fi = true;
                for &(m, n) in &chunks[ci].module_bytes {
                    if !fi {
                        body.push(',');
                    }
                    fi = false;
                    let _ = write!(
                        body,
                        "\n        {}: {{\n          \"bytesInOutput\": {n}\n        }}",
                        q(&graph.modules[m as usize].pretty)
                    );
                }
                body.push_str(if fi { "}" } else { "\n      }" });
            }
            None => {
                body.push_str(
                    "\n      \"imports\": [],\n      \"exports\": [],\n      \"inputs\": {}",
                );
            }
        }
        let _ = write!(body, ",\n      \"bytes\": {}", o.contents.len());
        emit(&mut s, &key, &body);
    }
    s.push_str(if first { "}" } else { "\n  }" });
    s.push_str("\n}\n");
    s
}

/// Metafile of a non-bundle build: `(input, input bytes, output path, output bytes)` per
/// entry.
pub fn render_transform(entries: &[(String, usize, String, usize)], wd: &str) -> String {
    let mut s = String::from("{\n  \"inputs\": {");
    for (i, (input, bytes, _, _)) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(
            s,
            "\n    {}: {{\n      \"bytes\": {bytes},\n      \"imports\": []\n    }}",
            q(input)
        );
    }
    s.push_str(if entries.is_empty() { "}" } else { "\n  }" });
    s.push_str(",\n  \"outputs\": {");
    for (i, (input, bytes, out, out_bytes)) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(
            s,
            "\n    {}: {{\n      \"imports\": [],\n      \"exports\": [],\n      \"entryPoint\": {},\n      \"inputs\": {{\n        {}: {{\n          \"bytesInOutput\": {bytes}\n        }}\n      }},\n      \"bytes\": {out_bytes}\n    }}",
            q(&path::relative(wd, out, true)),
            q(input),
            q(input)
        );
    }
    s.push_str(if entries.is_empty() { "}" } else { "\n  }" });
    s.push_str("\n}\n");
    s
}
