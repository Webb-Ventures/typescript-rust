//! Regressions from code review: each test is the reviewer's failure scenario.

use ts_r_ast::SourceKind;
use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transform, transpile};

ts_r_testing::cap_memory!();

fn js(src: &str, options: &str) -> String {
    let opts = TransformOptions::from_json(options, "input.ts");
    let out = transpile("input.ts", src, &opts, &CodegenOptions::default());
    assert!(out.build_errors.is_empty(), "{:?}", out.build_errors);
    out.code
}

/// The code with every run of whitespace made one space (layout does not matter here).
fn flat(code: &str) -> String {
    code.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn parenthesized_optional_super_call_keeps_this() {
    let src = "class B { f() { return this; } } class D extends B { async m() { return (super.f)?.(); } }";
    let out = js(src, r#"{"target":"es2016"}"#);
    assert!(out.contains("_a.call(this)"), "{out}");
    assert!(!out.contains("call(_super)"), "{out}");
}

#[test]
fn super_in_extracted_static_private_method_is_refused() {
    let src = "declare const dec: any; class B { static m() { return 1; } }\n@dec class C extends B { static #m() { return super.m(); } static g() { return C.#m(); } }";
    let opts = TransformOptions::from_json(r#"{"target":"es2022"}"#, "input.ts");
    let mut parsed = ts_r_parser::parse(src, SourceKind::Ts);
    let r = transform(&mut parsed.ast, src, &opts);
    assert!(!r.unlowered.is_empty());
    let out = js(src, r#"{"target":"es2022"}"#);
    // Never a `super` in a plain function (a SyntaxError).
    assert!(!out.contains("function _C_m"), "{out}");
    // Without `super` the method is still extracted.
    let ok = js(
        "declare const dec: any; @dec class C { static #m() { return 1; } static g() { return C.#m(); } }",
        r#"{"target":"es2022"}"#,
    );
    assert!(ok.contains("_C_m = function _C_m()"), "{ok}");
}

#[test]
fn nested_class_shadows_only_the_private_names_it_declares() {
    let src = "declare const dec: any;\n@dec class C { static #x = 1; static #y = 2; static make() { return class N { #x = 3; read() { return C.#y + this.#x; } }; } }";
    let out = flat(&js(src, r#"{"target":"es2022"}"#));
    assert!(
        out.contains(r#"__classPrivateFieldGet(C, _classThis, "f", _C_y)"#),
        "{out}"
    );
    assert!(out.contains("#x = 3;"), "{out}");
    assert!(out.contains("this.#x"), "{out}");
    assert!(!out.contains("C.#y"), "{out}");
}

#[test]
fn directly_exported_enum_and_namespace_keep_list_aliases() {
    let src = "export enum E { A } export { E as Alias };\nexport namespace N { export const x = 1; } export { N as NA };";
    let out = js(src, r#"{"module":"commonjs"}"#);
    assert!(
        out.contains("(exports.Alias = exports.E = E = {})"),
        "{out}"
    );
    assert!(out.contains("(exports.NA = exports.N = N = {})"), "{out}");
}

/// Quadratic in the namespaces sharing a name before (each check scanned every namespace):
/// minutes for these.
#[test]
fn many_same_named_inner_namespaces() {
    let src: String = (0..3000)
        .map(|i| {
            format!("namespace M_{i} {{ export namespace Utils {{ export const x = 1; }} }}\n")
        })
        .collect();
    let out = js(&src, "{}");
    assert!(
        out.contains("(Utils = M_2999.Utils || (M_2999.Utils = {}))"),
        "{out:.300}"
    );
}

#[test]
fn metadata_names_resolve_to_their_lexical_binding() {
    let src = "import { Foo } from './dep';\ndeclare const d: any;\nconsole.log(Foo);\nfunction f() { class Foo {} class C { @d p!: Foo; } return C; }";
    let out = js(
        src,
        r#"{"module":"commonjs","experimentalDecorators":true,"emitDecoratorMetadata":true}"#,
    );
    assert!(out.contains(r#"__metadata("design:type", Foo)"#), "{out}");
    assert!(out.contains("console.log(dep_1.Foo)"), "{out}");
}

/// Each alias scanned every top-level statement before: quadratic.
#[test]
fn many_import_aliases() {
    let mut src = String::from("declare namespace Lib { const C0: number; }\n");
    for i in 0..20_000 {
        src.push_str(&format!("import a{i} = Lib.C{i};\n"));
    }
    let out = js(&src, "{}");
    assert!(out.contains("var a19999 = Lib.C19999;"), "{out:.300}");
    let mut src = String::from("namespace N { export const v = 1; }\n");
    for i in 0..20_000 {
        src.push_str(&format!("export import A_{i} = N;\n"));
    }
    let out = js(&src, r#"{"module":"commonjs"}"#);
    assert!(out.contains("exports.A_19999 = N;"), "{out:.300}");
}

#[test]
fn decorated_class_with_this_in_computed_key_does_not_panic() {
    let opts = r#"{"target":"es2022","useDefineForClassFields":false}"#;
    for src in [
        "declare const dec: any; function f() { return class { @dec [this.key] = 1; }; }",
        "declare const dec: any; function g() { @dec class D { @dec [this.key] = 1; } return D; }",
    ] {
        let out = flat(&js(src, opts));
        // The capture runs in the decorator IIFE, where its temporaries are declared,
        // before the class that calls it.
        let iife = out.find("(() => { var _a, _b;").expect(&out);
        let capture = out.find("_b = () => {").expect(&out);
        let class = out.find("class {").expect(&out);
        assert!(iife < capture && capture < class, "{out}");
        assert!(out.contains("static { _b(); }"), "{out}");
    }
}

/// `const x1 = x0 + x0` chains took 2^n evaluations.
#[test]
fn const_chains_are_evaluated_once() {
    let mut src = String::from("const x0 = 1;\n");
    for i in 1..=40 {
        src.push_str(&format!("const x{i} = x{} + x{};\n", i - 1, i - 1));
    }
    src.push_str("enum E { A = x40 }\n");
    let out = js(&src, "{}");
    assert!(out.contains(r#"E[E["A"] = 1099511627776] = "A";"#), "{out}");
    // A doubling string stops folding before it gets huge.
    let mut src = String::from("const s0 = \"ab\";\n");
    for i in 1..=40 {
        src.push_str(&format!("const s{i} = s{} + s{};\n", i - 1, i - 1));
    }
    src.push_str("enum S { A = s40, B = s2 }\n");
    let out = js(&src, "{}");
    assert!(out.contains(r#"S[S["A"] = s40] = "A";"#), "{out}");
    assert!(out.contains(r#"S["B"] = "abababab";"#), "{out}");
}

#[test]
fn hoisted_exported_loop_variable_keeps_the_label_on_the_loop() {
    let src = "outer: for (var x = 0; x < 2; x++) { continue outer; } export { x };";
    let out = flat(&js(src, r#"{"module":"commonjs","target":"es2022"}"#));
    assert!(
        out.contains("var x = 0; exports.x = x; outer: for (;"),
        "{out}"
    );
}

#[test]
fn computed_accessor_storage_skips_names_taken_in_the_class() {
    let src = "const key = \"b\"; class C { accessor _a = 1; accessor [key] = 2; }";
    let out = js(src, r#"{"target":"es2022"}"#);
    assert!(out.contains("#_a_accessor_storage = 1;"), "{out}");
    assert!(out.contains("#_b_accessor_storage = 2;"), "{out}");
    let out = js(src, r#"{"target":"es2015"}"#);
    assert!(
        out.contains("_C__a_accessor_storage.set(this, 1);"),
        "{out}"
    );
    assert!(
        out.contains("_C__b_accessor_storage.set(this, 2);"),
        "{out}"
    );
}

#[test]
fn static_field_initializer_scope_does_not_contain_itself() {
    let src = "declare const dec: any; declare const f: any; class Base { static x: any; }\n@dec class C extends Base { static x = f(super.x = 1); }";
    let out = flat(&js(src, r#"{"target":"es2022"}"#));
    assert!(
        out.contains(r#"static x = (() => { var _a; return f((Reflect.set(_classSuper, "x", _a = 1, _classThis), _a)); })();"#),
        "{out}"
    );
}

#[test]
fn parenthesized_static_super_targets_are_writes() {
    let src = "declare const dec: any; class B { static x: any; }\n@dec class C extends B { static { (super.x) = 1; (super.x)++; } }";
    let out = flat(&js(src, r#"{"target":"es2022"}"#));
    assert!(
        out.contains(r#"Reflect.set(_classSuper, "x", 1, _classThis);"#),
        "{out}"
    );
    assert!(
        out.contains(r#"Reflect.set(_classSuper, "x", (_a = Reflect.get(_classSuper, "x", _classThis), _a++, _a), _classThis);"#),
        "{out}"
    );
    assert!(!out.contains(") = 1"), "{out}");
}

#[test]
fn parameter_default_temporaries_get_their_own_scope() {
    let src = "declare const dec: any; class B { static x: any; }\n@dec class C extends B { static f = (x = super.x++) => x; }";
    let out = flat(&js(src, r#"{"target":"es2022"}"#));
    assert!(
        out.contains(r#"static f = (x = (() => { var _a, _b; return Reflect.set("#),
        "{out}"
    );
    assert!(out.contains("_classThis), _a; })()) => x;"), "{out}");
}

/// A chain released one alias per pass over all of them before: quadratic.
#[test]
fn dropped_alias_chains_release_in_one_pass() {
    let mut src = String::from("import * as ns from \"m\";\nimport A0 = ns;\n");
    for i in 1..20_000 {
        src.push_str(&format!("import A{i} = A{};\n", i - 1));
    }
    src.push_str("export {};\n");
    assert_eq!(js(&src, "{}"), "export {};\n");
}
