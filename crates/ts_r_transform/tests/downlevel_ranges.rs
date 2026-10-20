use ts_r_ast::{Node, SourceKind};
use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transform, transpile};

ts_r_testing::cap_memory!();

#[test]
fn copied_names_keep_ranges_across_chains() {
    let src = "base?.y; f()?.x; base?.z; g()?.x;";
    let opts = TransformOptions::from_json(r#"{"target":"es2019"}"#, "a.ts");
    let mut parsed = ts_r_parser::parse(src, SourceKind::Ts);
    let result = transform(&mut parsed.ast, src, &opts);
    let ranges: Vec<_> = result
        .map_ranges
        .iter()
        .filter_map(|&(id, span)| match parsed.ast.node(id) {
            Node::Ident { name } if parsed.ast.atom(*name) == "base" => Some(span),
            _ => None,
        })
        .collect();
    for (start, _) in src.match_indices("base") {
        let start = start as u32;
        assert_eq!(
            ranges
                .iter()
                .filter(|r| r.start == start && r.end == start + 4)
                .count(),
            2,
            "ranges at {start}: {ranges:?}"
        );
    }
}

#[test]
fn many_unmapped_temporaries() {
    let src = "f()?.x;\n".repeat(10_000);
    let opts = TransformOptions::from_json(r#"{"target":"es2019"}"#, "a.ts");
    let out = transpile("a.ts", &src, &opts, &CodegenOptions::default());
    assert!(out.diagnostics.is_empty());
    assert_eq!(out.code.matches("= f()").count(), 10_000);
    assert!(!out.code.contains("?."));
}
