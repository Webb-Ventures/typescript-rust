use std::process::Command;

use ts_r_ast::SourceKind;
use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transform, transpile};

ts_r_testing::cap_memory!();

#[test]
fn private_methods_with_super_are_refused() {
    for method in [
        "#m() { return super.m(); }",
        "#m() { return (() => super.m())(); }",
        "#m(v = super.m()) { return v; }",
        "static #m() { return super.m(); }",
        "get #m() { return super.m(); }",
        "set #m(v) { super.m(v); }",
        "#m() { return class extends super.m() {}; }",
        "#m() { return class { [super.m()]() {} }; }",
    ] {
        let src = format!("class B {{ m() {{ return 1; }} }} class C extends B {{ {method} }}");
        for target in ["es2015", "es2021"] {
            let opts = TransformOptions::from_json(
                &format!(r#"{{"target":"{target}","module":"esnext"}}"#),
                "private.ts",
            );
            let mut parsed = ts_r_parser::parse(&src, SourceKind::Ts);
            assert!(parsed.diagnostics.is_empty(), "{method}");
            let out = transform(&mut parsed.ast, &src, &opts);
            assert!(!out.unlowered.is_empty(), "{method} at {target}");
        }
    }
}

#[test]
fn repeated_private_classes_with_outer_suspension_are_refused() {
    for src in [
        "async function f() { for (;;) { const C = class { #x = 1; [await 1]() {} }; } }",
        "function* f() { for (;;) { const C = class { #x = 1; [yield 1]() {} }; } }",
    ] {
        let opts =
            TransformOptions::from_json(r#"{"target":"es2021","module":"esnext"}"#, "private.ts");
        let mut parsed = ts_r_parser::parse(src, SourceKind::Ts);
        assert!(parsed.diagnostics.is_empty(), "{src}");
        let out = transform(&mut parsed.ast, src, &opts);
        assert!(!out.unlowered.is_empty(), "{src}");
    }
}

#[test]
fn private_method_keeps_nested_methods_super() {
    let src = "class B {} class C { #m() { return class extends B { m() { return super.toString(); } }; } }";
    let opts =
        TransformOptions::from_json(r#"{"target":"es2021","module":"esnext"}"#, "private.ts");
    let mut parsed = ts_r_parser::parse(src, SourceKind::Ts);
    let out = transform(&mut parsed.ast, src, &opts);
    assert!(out.unlowered.is_empty(), "{:?}", out.unlowered);
}

#[test]
fn repeated_private_class_expressions_keep_their_storage() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("node not found, skipped");
        return;
    }
    let src = r#"
const xs = [];
for (let i = 0; i < 2; i++) {
    xs.push(new class {
        #x = i;
        #read() { return this.#x; }
        get() { return this.#read(); }
    });
}
console.log(xs[0].get(), xs[1].get());
const cs = [];
for (let i = 0; i < 2; i++) {
    cs.push(class C {
        static #x = i;
        static get() { return C.#x; }
    });
}
console.log(cs[0].get(), cs[1].get());
const ds = [];
for (let i = 0; i < 2; i++) {
    class D {
        #x = i;
        get() { return this.#x; }
    }
    ds.push(new D());
}
console.log(ds[0].get(), ds[1].get());
"#;
    for target in ["es2015", "es2021", "esnext"] {
        let opts = TransformOptions::from_json(
            &format!(r#"{{"target":"{target}","module":"esnext"}}"#),
            "private.ts",
        );
        let mut parsed = ts_r_parser::parse(src, SourceKind::Ts);
        let out = transform(&mut parsed.ast, src, &opts);
        assert!(out.unlowered.is_empty(), "{target}: {:?}", out.unlowered);
        let out = transpile("private.ts", src, &opts, &CodegenOptions::default());
        let run = ts_r_testing::limited("node", 512, 60)
            .args(["--input-type=module", "-e"])
            .arg(&out.code)
            .output()
            .expect("node runs");
        assert!(
            run.status.success(),
            "{target}: {}\n{}",
            String::from_utf8_lossy(&run.stderr),
            out.code
        );
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "0 1\n0 1\n0 1\n",
            "{target}"
        );
    }
}
