//! Every fixture under `tests/fixtures` transpiles to the same JavaScript (by AST shape) as
//! tsc 7's stored output next to it (`<name>.tsc.js`, written by
//! `cargo run --example oracle -- --bless tests/fixtures`).

use std::path::{Path, PathBuf};

use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transpile};

mod support;

ts_r_testing::cap_memory!();

/// Fixtures tsc emits differently, with the reason. Keep this list short.
const KNOWN_GAPS: &[(&str, &str)] = &[(
    "namespace/basic.ts",
    "exported destructuring inside a namespace is not flattened into temporaries",
)];

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
            .expect("read fixture dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for e in entries {
            walk(&e, out);
        }
    } else if support::is_fixture(p) {
        out.push(p.to_path_buf());
    }
}

#[test]
fn fixtures_match_tsc() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(files.len() >= 20, "fixtures missing: {}", files.len());
    let mut failures = Vec::new();
    let mut gaps_hit = 0;
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(f).expect("read fixture");
        let expected = std::fs::read_to_string(support::stored_path(f)).unwrap_or_else(|_| {
            panic!("no stored tsc output for {rel}; run the oracle with --bless")
        });
        let json = support::options_json(&src);
        let path = f.to_string_lossy().to_string();
        let opts = TransformOptions::from_json(&json, &path);
        let out = transpile(&path, &src, &opts, &CodegenOptions::default());
        let diff = support::compare(&out.code, &expected);
        let known = KNOWN_GAPS.iter().any(|(n, _)| *n == rel);
        match (diff, known) {
            (None, true) => failures.push(format!("{rel}: now matches; remove it from KNOWN_GAPS")),
            (Some(_), true) => gaps_hit += 1,
            (Some(d), false) => failures.push(format!("{rel} {json}\n{d}")),
            (None, false) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert_eq!(gaps_hit, KNOWN_GAPS.len());
}

/// tsc prints the specifier of the `require` call it writes for an import with double quotes,
/// whatever the import's own quotes were; `import()` keeps its quotes.
#[test]
fn require_specifiers_print_double_quoted() {
    let src =
        "import x = require('a');\nimport y from 'b';\nexport { x, y };\nconst w = import('c');\n";
    let opts = TransformOptions::from_json(r#"{"module":"commonjs"}"#, "a.ts");
    let out = transpile("a.ts", src, &opts, &CodegenOptions::default());
    assert!(out.code.contains(r#"require("a")"#), "{}", out.code);
    assert!(out.code.contains(r#"require("b")"#), "{}", out.code);
    assert!(out.code.contains("require('c')"), "{}", out.code);
}

#[test]
fn wide_exported_destructuring_keeps_member_and_property_ranges() {
    use std::fmt::Write;
    use ts_r_ast::{Node, SourceKind};

    const BINDINGS: usize = 10_000;
    let mut src = String::from("export const first = 1; export const {");
    for i in 0..BINDINGS {
        write!(src, "binding_{i},").unwrap();
    }
    src.push_str("renamed: [nested = 1, { deep }], ...rest } = value;");
    let mut parsed = ts_r_parser::parse(&src, SourceKind::Ts);
    assert!(parsed.diagnostics.is_empty());
    let opts = TransformOptions::from_json(r#"{"module":"commonjs"}"#, "a.ts");
    let result = ts_r_transform::transform(&mut parsed.ast, &src, &opts);
    assert!(result.diagnostics.is_empty());
    let mut members = 0;
    for &(id, range) in &result.map_ranges {
        let Node::Member { obj, prop, .. } = *parsed.ast.node(id) else {
            continue;
        };
        assert!(
            matches!(parsed.ast.node(obj), Node::Ident { name } if parsed.ast.atom(*name) == "exports")
        );
        let Node::Ident { name } = *parsed.ast.node(prop) else {
            panic!("export property is not an identifier");
        };
        assert_eq!(
            &src[range.start as usize..range.end as usize],
            parsed.ast.atom(name)
        );
        let property_range = result
            .map_ranges
            .binary_search_by_key(&prop, |&(n, _)| n)
            .unwrap();
        assert_eq!(result.map_ranges[property_range].1, range);
        members += 1;
    }
    assert_eq!(members, BINDINGS + 4);
}
