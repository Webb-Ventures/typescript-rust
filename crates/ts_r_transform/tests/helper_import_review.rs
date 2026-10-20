use std::process::Command;

use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transform, transpile};

ts_r_testing::cap_memory!();

#[test]
fn commonjs_imports_helpers_requested_by_lowering() {
    let src = "export async function f() { return 1; }";
    let emit = |import_helpers| {
        let opts = TransformOptions::from_json(
            &format!(
                r#"{{"target":"es2015","module":"commonjs","importHelpers":{import_helpers}}}"#
            ),
            "helpers.ts",
        );
        transpile("helpers.ts", src, &opts, &CodegenOptions::default()).code
    };
    let imported = emit(true);
    assert_eq!(
        imported.matches("require(\"tslib\")").count(),
        1,
        "{imported}"
    );
    assert!(imported.contains("tslib_1.__awaiter"), "{imported}");
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    // Use the inline helper as the imported runtime, so this test needs no tslib install.
    let inline = emit(false);
    let code = format!(
        "const exports = {{}};\n{inline}\n(function(require, exports) {{\n{imported}\nexports.f().then(console.log);\n}})(name => {{ if (name !== 'tslib') throw name; return {{ __awaiter }}; }}, {{}});"
    );
    let run = ts_r_testing::limited("node", 512, 60)
        .args(["-e"])
        .arg(code)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n");
}

#[test]
fn long_ordinary_access_chain_needs_no_lowering() {
    let src = format!("a{};", ".b".repeat(20_000));
    let opts = TransformOptions::from_json(r#"{"target":"es2019"}"#, "chain.ts");
    let mut parsed = ts_r_parser::parse(&src, ts_r_ast::SourceKind::Ts);
    assert!(parsed.diagnostics.is_empty());
    let out = transform(&mut parsed.ast, &src, &opts);
    assert!(out.diagnostics.is_empty());
    assert!(out.unlowered.is_empty());
    assert!(out.prelude.is_empty());
}
