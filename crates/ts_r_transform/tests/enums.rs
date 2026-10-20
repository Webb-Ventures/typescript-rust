use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transpile};

ts_r_testing::cap_memory!();

#[test]
fn long_member_chain_is_preserved() {
    let expr = format!("x{};", ".a".repeat(10_000));
    let opts = TransformOptions::from_json(r#"{"target":"esnext"}"#, "a.ts");
    for prefix in ["declare const x: any;", "import x from './a';"] {
        let src = format!("{prefix}\n{expr}");
        let out = transpile("a.ts", &src, &opts, &CodegenOptions::default());
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.code.contains(&expr));
    }
}
