use std::path::Path;
use std::process::Command;

use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transpile};

ts_r_testing::cap_memory!();

#[test]
fn explicit_arguments_bindings_in_async_functions() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("node not found, skipped");
        return;
    }
    let src = r#"
async function parameter(arguments: any) { return arguments; }
async function defaultParameter(arguments = 43) { return arguments; }
async function pattern({ arguments }: any) { return { arguments }; }
async function lexical(a: any) {
    let result;
    { let arguments = 45; result = arguments; }
    return [result, arguments[0]];
}
async function caught() {
    try { throw 46; }
    catch (arguments) { return { arguments }; }
}
function outer(arguments: any) { return (async () => ({ arguments }))(); }
function nested(arguments: any) {
    async function inner(a: any) { return arguments[0]; }
    return inner(48);
}
async function escaped(argum\u0065nts: any) { return argum\u0065nts; }
async function beforeDefault(arguments: any, b = 51) { return [arguments, b]; }
Promise.all([parameter(42), defaultParameter(), pattern({ arguments: 44 }),
    lexical(49), caught(), outer(47), nested(0), escaped(50), beforeDefault(52)])
    .then(xs => console.log(JSON.stringify(xs)));
"#;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("arguments_review");
    std::fs::create_dir_all(&dir).unwrap();
    for target in ["esnext", "es2015", "es2016"] {
        let opts = TransformOptions::from_json(
            &format!(
                r#"{{"target":"{target}","module":"esnext","alwaysStrict":false,"moduleDetection":"legacy"}}"#
            ),
            "arguments.ts",
        );
        let output = transpile("arguments.ts", src, &opts, &CodegenOptions::default());
        let path = dir.join(format!("{target}.cjs"));
        std::fs::write(&path, &output.code).unwrap();
        let out = ts_r_testing::limited("node", 512, 60)
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{target}: {}\n{}",
            String::from_utf8_lossy(&out.stderr),
            output.code
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            r#"[42,43,{"arguments":44},[45,49],{"arguments":46},{"arguments":47},48,50,[52,51]]"#,
            "{target}"
        );
    }
}
