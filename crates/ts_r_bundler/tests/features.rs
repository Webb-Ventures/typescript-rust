//! Target features (`--target`, `--supported:<feature>=false`) and what they change in the
//! output, as the esbuild bundler oracle checks them.

ts_r_testing::cap_memory!();

use std::sync::Arc;

use ts_r_bundler::{
    BuildContext, BuildOptions, BuildResult, EntryPoint, Feature, Format, Platform,
};
use ts_r_config::MemoryFs;

fn project(files: &[(&str, &str)]) -> MemoryFs {
    let mut fs = MemoryFs::new();
    for (p, c) in files {
        fs.add_file(&format!("/proj/{p}"), c);
    }
    fs
}

fn opts(bundle: bool, unsupported: &[&str]) -> BuildOptions {
    BuildOptions {
        entry_points: vec![EntryPoint {
            path: "entry.js".into(),
            name: None,
        }],
        working_dir: "/proj".into(),
        bundle,
        outdir: Some("out".into()),
        write: false,
        threads: 2,
        platform: Platform::Node,
        format: bundle.then_some(Format::Esm),
        supported: unsupported
            .iter()
            .map(|f| (Feature::parse(f).expect("feature"), false))
            .collect(),
        ..BuildOptions::default()
    }
}

fn build(fs: &MemoryFs, o: BuildOptions) -> BuildResult {
    BuildContext::new(o, Arc::new(fs.clone()))
        .expect("options")
        .build()
}

fn errors(r: &BuildResult) -> Vec<String> {
    r.messages
        .iter()
        .filter(|m| m.is_error())
        .map(|m| m.text.clone())
        .collect()
}

fn out(r: &BuildResult) -> String {
    assert!(errors(r).is_empty(), "{:?}", errors(r));
    r.outputs.iter().map(|o| o.text().to_owned()).collect()
}

#[test]
fn import_meta_below_its_target_is_an_empty_object() {
    let fs = project(&[
        ("entry.js", "import './b.js'; console.log(import.meta.url);"),
        ("b.js", "console.log(import.meta.url);"),
    ]);
    let r = build(&fs, opts(true, &["import-meta"]));
    let text = out(&r);
    assert!(text.contains("var import_meta = {};"), "{text}");
    assert!(text.contains("var import_meta2 = {};"), "{text}");
    assert!(!text.contains("import.meta"), "{text}");
    let warnings = r.messages.iter().filter(|m| !m.is_error()).count();
    assert_eq!(warnings, 2);
    // Without bundling: one `const`, or `var` when `const` is unsupported too.
    let fs = project(&[("entry.js", "console.log(import.meta.url);")]);
    let text = out(&build(&fs, opts(false, &["import-meta"])));
    assert!(text.starts_with("const import_meta = {};"), "{text}");
    let text = out(&build(&fs, opts(false, &["import-meta", "const-and-let"])));
    assert!(text.starts_with("var import_meta = {};"), "{text}");
    // Supported: kept.
    let text = out(&build(&fs, opts(false, &[])));
    assert!(text.contains("import.meta.url"), "{text}");
}

#[test]
fn string_module_names_only_fail_where_they_reach_the_output() {
    let fs = project(&[
        (
            "entry.js",
            "import { \"a b\" as x } from './in.js'; export { x as \"c d\", x as \"ok\" }; export * from './star.js';",
        ),
        ("in.js", "let y = 1; export { y as \"a b\" };"),
        (
            "star.js",
            "import { \"e f\" as z } from 'ext'; export { z as \"g h\" };",
        ),
    ]);
    let mut o = opts(true, &["arbitrary-module-namespace-names"]);
    o.external = vec!["ext".into()];
    let e = errors(&build(&fs, o.clone()));
    assert_eq!(e.len(), 3, "{e:?}");
    assert!(e.iter().any(|m| m.contains("\"c d\" as an export")));
    assert!(e.iter().any(|m| m.contains("\"e f\" as an import")));
    assert!(e.iter().any(|m| m.contains("\"g h\" as an export")));
    // CommonJS output has property keys only.
    o.format = Some(Format::Cjs);
    let e = errors(&build(&fs, o));
    assert!(e.iter().all(|m| !m.contains("export name")), "{e:?}");
}

#[test]
fn json_proto_key_below_es2015_is_printed_as_is() {
    let fs = project(&[
        ("entry.js", "import d from './d.json'; console.log(d);"),
        ("d.json", r#"{"__proto__": {"a": 1}}"#),
    ]);
    let text = out(&build(&fs, opts(true, &[])));
    assert!(text.contains("[\"__proto__\"]"), "{text}");
    let mut o = opts(true, &[]);
    o.target = Some(2014);
    let text = out(&build(&fs, o));
    assert!(text.contains("\"__proto__\":"), "{text}");
}

#[test]
fn writing_a_constant_is_an_error_when_lowering_top_level_using() {
    let fs = project(&[(
        "entry.js",
        "const x = 1; using u = null; x = 3; function f() { const y = 1; y = 2; }",
    )]);
    let e = errors(&build(&fs, opts(false, &["using"])));
    assert_eq!(e, ["Cannot assign to \"x\" because it is a constant"]);
    assert!(errors(&build(&fs, opts(false, &[]))).is_empty());
}

#[test]
fn lowered_anonymous_default_class_takes_the_default_export_name() {
    let fs = project(&[
        (
            "entry.js",
            "import C from './thing.js'; console.log(C.name, C.y);",
        ),
        ("thing.js", "export default class { static y = 1; }"),
    ]);
    let text = out(&build(&fs, opts(true, &["class-static-field"])));
    assert!(text.contains("thing_default"), "{text}");
    assert!(!text.contains("default_1"), "{text}");
}
