use ts_r_checker::{Checker, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_emit::{CheckerResolver, EmitConfig};

ts_r_testing::cap_memory!();

fn declaration(source: &str) -> String {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["index.ts"]}"#,
    );
    fs.add_file("/p/index.ts", source);
    fs.set_cwd("/p");
    let parsed = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &parsed.file_names, &parsed.options, None, 1);
    let checker = Checker::new(&program);
    let resolver = CheckerResolver::new(&checker, &parsed.options);
    let cfg = EmitConfig {
        options: &parsed.options,
        config: Some(&parsed),
        threads: 1,
        case_sensitive: true,
        specifiers: None,
    };
    let result = ts_r_emit::emit(&program, &resolver, &cfg);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    result
        .outputs
        .into_iter()
        .find(|o| o.path == "/p/out/index.d.ts")
        .expect("declaration output")
        .text
}

#[test]
fn return_assertion_to_local_alias_is_structural() {
    assert_eq!(
        declaration(
            "export function f() { type Hidden = { x: number }; return null as unknown as Hidden; }",
        ),
        "export declare function f(): {\n    x: number;\n};\n",
    );
}

#[test]
fn return_assertion_to_local_value_is_structural() {
    assert_eq!(
        declaration(
            "export function f() { const hidden = { x: 1 }; return null as unknown as typeof hidden; }",
        ),
        "export declare function f(): {\n    x: number;\n};\n",
    );
}

#[test]
fn returned_arrow_cannot_name_outer_body_alias() {
    assert_eq!(
        declaration(
            "export function f() { type Hidden = { x: number }; return () => null as unknown as Hidden; }",
        ),
        "export declare function f(): () => {\n    x: number;\n};\n",
    );
}

#[test]
fn return_assertions_preserve_accessible_types_and_parameters() {
    let text = declaration(
        "type Visible = { x: number }; export function f() { return null as unknown as Visible; } export function g<T>() { return null as unknown as T; } export function h() { return <T>(x: T) => x as T; } export function s() { return 'foo' as const; }",
    );
    assert!(text.contains("function f(): Visible;"), "{text}");
    assert!(text.contains("function g<T>(): T;"), "{text}");
    assert!(text.contains("function h(): <T>(x: T) => T;"), "{text}");
    assert!(text.contains("function s(): 'foo';"), "{text}");
}
