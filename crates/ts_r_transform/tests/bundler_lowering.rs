//! Lowering that ts-r-build's targets reach (`--supported:<feature>=false`, the esbuild
//! bundler oracle): private names under logical assignment, `super` in async arrows inside
//! plain methods, and the bundler's name for an anonymous default class.

use std::process::Command;

use ts_r_ast::SourceKind;
use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transform, transpile};

ts_r_testing::cap_memory!();

fn opts(target: &str) -> TransformOptions {
    TransformOptions::from_json(
        &format!(r#"{{"target":"{target}","module":"esnext"}}"#),
        "lower.js",
    )
}

/// Lowers `src` to `target` and runs it with node; `None` when node is missing.
fn run(src: &str, target: &str) -> Option<String> {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("node not found, skipped");
        return None;
    }
    let o = opts(target);
    let mut parsed = ts_r_parser::parse(src, SourceKind::Js);
    let out = transform(&mut parsed.ast, src, &o);
    assert!(out.unlowered.is_empty(), "{target}: {:?}", out.unlowered);
    let out = transpile("lower.js", src, &o, &CodegenOptions::default());
    let r = ts_r_testing::limited("node", 512, 60)
        .args(["--input-type=module", "-e"])
        .arg(&out.code)
        .output()
        .expect("node runs");
    assert!(
        r.status.success(),
        "{target}: {}\n{}",
        String::from_utf8_lossy(&r.stderr),
        out.code
    );
    Some(String::from_utf8_lossy(&r.stdout).into_owned())
}

#[test]
fn private_logical_assignment() {
    let src = r#"
class C {
    #x; #y = 2; #z = 0;
    m() {
        this.#x ??= 1;
        this.#y **= 2;
        this.#z ||= 5;
        this.#y &&= this.#y + 1;
        return [this.#x, this.#y, this.#z];
    }
}
console.log(new C().m().join());
"#;
    for target in ["es2015", "es2019", "es2020", "es2021"] {
        if let Some(out) = run(src, target) {
            assert_eq!(out, "1,5,5\n", "{target}");
        }
    }
}

#[test]
fn super_in_async_arrow_of_plain_method() {
    let src = r#"
class B { foo() { return 'b'; } get g() { return 'g'; } set s(v) { this.v = v; } }
class C extends B {
    constructor() { super(); this.p = (async () => super.foo())(); }
    m() { return (async () => { super.s = 'v'; return super.foo() + super.g + this.v; })(); }
    get h() { return (async () => super.foo())(); }
    static t() { return (async () => super.name)(); }
}
const c = new C();
Promise.all([c.p, c.m(), c.h, C.t()]).then(v => console.log(v.join()));
"#;
    for target in ["es2015", "es2016"] {
        if let Some(out) = run(src, target) {
            assert_eq!(out, "b,bgv,b,B\n", "{target}");
        }
    }
}

#[test]
fn anonymous_default_class_takes_the_bundler_name() {
    let src = "export default class { static y = 1; }";
    let mut o = opts("es2021");
    o.default_class_name = Some("entry_default".into());
    let out = transpile("entry.js", src, &o, &CodegenOptions::default());
    assert!(out.code.contains("entry_default"), "{}", out.code);
    assert!(!out.code.contains("default_1"), "{}", out.code);
}
