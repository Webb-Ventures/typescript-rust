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

/// A program resolver that knows every enum member the file alone cannot fold is 7.
struct SevenResolver;

impl ts_r_transform::EmitResolver for SevenResolver {
    fn enum_member_value(
        &self,
        _file: ts_r_diagnostics::FileId,
        _member: ts_r_ast::NodeId,
    ) -> Option<ts_r_transform::ConstValue> {
        Some(ts_r_transform::ConstValue::Number(7.0))
    }
}

#[test]
fn resolver_folds_members_the_file_cannot() {
    let src = "import { O } from './o';\nenum E { A = 1, B = O.X, C }\n";
    let opts = TransformOptions::from_json(r#"{"target":"esnext"}"#, "a.ts");
    let mut parsed = ts_r_parser::parse(src, ts_r_ast::SourceKind::from_path("a.ts"));
    ts_r_transform::transform_with(
        &mut parsed.ast,
        src,
        &opts,
        &SevenResolver,
        ts_r_diagnostics::FileId(0),
    );
    let code = ts_r_codegen::print(&parsed.ast, src, &CodegenOptions::default()).code;
    assert!(code.contains(r#"E[E["A"] = 1] = "A""#), "{code}");
    assert!(code.contains(r#"E[E["B"] = 7] = "B""#), "{code}");
    assert!(code.contains(r#"E[E["C"] = 8] = "C""#), "{code}");
    // The file-local tools keep the reference.
    let out = transpile("a.ts", src, &opts, &CodegenOptions::default());
    assert!(out.code.contains(r#"E["B"] = O.X"#), "{}", out.code);
}
