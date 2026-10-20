//! Declaration emit on in-memory projects. Expected texts are tsc 7.0.2's output for the
//! same inputs (the upstream baselines named on each test).

use ts_r_checker::{Checker, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_emit::{CheckerResolver, EmitConfig, EmitResult, OutputKind};

ts_r_testing::cap_memory!();

fn emit(config: &str, files: &[(&str, &str)]) -> EmitResult {
    let mut fs = MemoryFs::new();
    fs.add_file("/p/tsconfig.json", config);
    for (p, text) in files {
        fs.add_file(&format!("/p/{p}"), text);
    }
    fs.set_cwd("/p");
    let parsed = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &parsed.file_names, &parsed.options, None, 1);
    let checker = Checker::new(&program);
    let resolver = CheckerResolver::new(&checker, &parsed.options);
    let host = ts_r_checker::typenode::specifier_host(&fs, &parsed.options);
    let cfg = EmitConfig {
        options: &parsed.options,
        config: Some(&parsed),
        threads: 1,
        case_sensitive: true,
        specifiers: Some(&host),
    };
    ts_r_emit::emit(&program, &resolver, &cfg)
}

fn output<'r>(r: &'r EmitResult, path: &str) -> &'r str {
    match r.outputs.iter().find(|o| o.path == path) {
        Some(o) => &o.text,
        None => panic!(
            "no {path} in {:?} ({:?})",
            r.outputs.iter().map(|o| &o.path).collect::<Vec<_>>(),
            r.diagnostics
        ),
    }
}

const CONFIG: &str = r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["index.ts"]}"#;

#[test]
fn deeply_nested_source_annotation_is_refused() {
    let source = format!("export type T = string{};", "[]".repeat(10_000));
    let r = emit(CONFIG, &[("index.ts", &source)]);
    assert!(r.outputs.is_empty());
    assert_eq!(codes(&r), vec![ts_r_emit::dts::TSR_UNRESOLVED_TYPE]);
    assert!(r.diagnostics[0].message.contains("nests too deeply"));
}

#[test]
fn local_type_dependencies_reach_closure() {
    let mut source = "type A0 = number;\n".to_owned();
    let mut expected = source.clone();
    for i in 1..=12 {
        source.push_str(&format!("type A{i} = {{ x: A{} }};\n", i - 1));
        expected.push_str(&format!("type A{i} = {{\n    x: A{};\n}};\n", i - 1));
    }
    source.push_str("export type Api = A12;\n");
    expected.push_str("export type Api = A12;\nexport {};\n");
    let r = emit(CONFIG, &[("index.ts", &source)]);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(output(&r, "/p/out/index.d.ts"), expected);
}

#[test]
fn inferred_method_preserves_generic_default() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "export const api = { f<T = string>(): T { return null!; } };\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const api: {\n    f<T = string>(): T;\n};\n"
    );
}

#[test]
fn imported_alias_shadowed_by_type_parameter() {
    let r = emit(
        CONFIG,
        &[
            ("m.ts", "export class C { x = 1; }\n"),
            (
                "index.ts",
                "import { C as A } from './m'; export function f<A>() { return new A(); }\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare function f<A>(): import(\"./m\").C;\n"
    );
}

/// declarationMapsMultifile: the declaration ends at its `sourceMappingURL` comment and the
/// map names the declaration and its source.
#[test]
fn declaration_map() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"declarationMap":true,"outDir":"out","types":[]},"files":["index.ts"]}"#;
    let r = emit(
        config,
        &[("index.ts", "export class Foo {\n    x = 1;\n}\n")],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare class Foo {\n    x: number;\n}\n//# sourceMappingURL=index.d.ts.map"
    );
    let map = r
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/index.d.ts.map")
        .map(|o| (o.kind, o.text.as_str()));
    let Some((OutputKind::DtsMap, json)) = map else {
        panic!("no declaration map: {map:?}");
    };
    assert!(
        json.starts_with(
            r#"{"version":3,"file":"index.d.ts","sourceRoot":"","sources":["../index.ts"]"#
        ),
        "{json}"
    );
    // Declaration starts, source names and statement ends, as tsc 7.0.2 maps them.
    assert!(
        json.ends_with(r#""mappings":"AAAA,qBAAa,GAAG;IACZ,CAAC,SAAK;CACT"}"#),
        "{json}"
    );
}

/// symbolLinkDeclarationEmitModuleNamesImportRef: a package whose `typings` entry declares
/// the type is named by the package name, not its file path.
#[test]
fn package_entry_point_specifier() {
    let r = emit(
        CONFIG,
        &[
            (
                "node_modules/styled/package.json",
                r#"{"name":"styled","version":"1.0.0","typings":"typings/styled.d.ts"}"#,
            ),
            (
                "node_modules/styled/typings/styled.d.ts",
                "export interface Value { x: number }\nexport declare function get(): Value;\n",
            ),
            (
                "index.ts",
                "import { get } from \"styled\";\nexport function getStyles() { return get(); }\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare function getStyles(): import(\"styled\").Value;\n"
    );
}

/// declarationEmitCrossFileCopiedGeneratedImportType: a relative specifier drops `/index`
/// and the extension.
#[test]
fn relative_specifier_drops_index() {
    let r = emit(
        CONFIG,
        &[
            (
                "lib/index.ts",
                "export interface Foo { a: number }\nexport declare function make(): Foo;\n",
            ),
            ("mid.ts", "export { make as m } from \"./lib/index\";\n"),
            (
                "index.ts",
                "import { m } from \"./mid\";\nexport const a = m();\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const a: import(\"./lib\").Foo;\n"
    );
}

/// declarationsIndirectGeneratedAliasReference: a symbol re-exported by a module the file
/// imports is named through that module.
#[test]
fn reexporting_module_names_symbol() {
    let r = emit(
        CONFIG,
        &[
            (
                "node_modules/mod/ctor.d.ts",
                "export interface Ctor { x: number }\nexport type Ext<T> = { ext: T };\nexport declare function extend<T>(x: T): Ext<Ctor>;\n",
            ),
            (
                "node_modules/mod/index.d.ts",
                "import { Ctor, extend } from \"./ctor\";\nexport default Ctor;\nexport { extend };\n",
            ),
            (
                "index.ts",
                "import { extend } from \"mod\";\nexport const MyComp = extend(1);\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const MyComp: import(\"mod/ctor\").Ext<import(\"mod\").default>;\n"
    );
}

fn codes(r: &EmitResult) -> Vec<u32> {
    r.diagnostics.iter().map(|d| d.code).collect()
}

/// isolatedDeclarationErrorsExpressions / isolatedDeclarationErrorsObjects: a type that
/// does not follow from syntax is TS9xxx and the file's `.d.ts` is not written; literals,
/// `as` types and `as const` arrays are fine.
#[test]
fn isolated_declarations() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"isolatedDeclarations":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["ok.ts","bad.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "ok.ts",
                "export const a = 1;\nexport let b = `s${a}`;\nexport const c = [1, 2] as const;\nexport const d = { x: 1, y: \"s\" as string };\nexport const f = () => \"S\";\n",
            ),
            (
                "bad.ts",
                "declare function r(): number;\nexport const a = r();\nexport let o = { v: a, ...{} };\nexport let arr = [1];\nexport function g() {}\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/ok.d.ts"),
        "export declare const a = 1;\nexport declare let b: string;\nexport declare const c: readonly [1, 2];\nexport declare const d: {\n    x: number;\n    y: string;\n};\nexport declare const f: () => string;\n"
    );
    assert!(r.outputs.iter().all(|o| o.path != "/p/out/bad.d.ts"));
    assert_eq!(
        codes(&r),
        vec![9010, 9013, 9015, 9017, 9007],
        "{:?}",
        r.diagnostics
    );
}

/// declarationEmitUsingTypeAlias1: a type only reachable through a `node_modules` path
/// that the package's `exports` block is TS2883, not a deep import.
#[test]
fn not_portable_type() {
    let r = emit(
        CONFIG,
        &[
            (
                "index.ts",
                "import { SomeType } from \"some-dep\";\nexport const foo = (thing: SomeType) => thing;\n",
            ),
            (
                "node_modules/some-dep/package.json",
                r#"{"name":"some-dep","exports":{".":"./dist/index.js"}}"#,
            ),
            (
                "node_modules/some-dep/dist/index.d.ts",
                "export type SomeType = import('./inner').SomeType;\n",
            ),
            (
                "node_modules/some-dep/dist/inner.d.ts",
                "export declare type SomeType = { arg: string };\n",
            ),
        ],
    );
    assert!(r.outputs.iter().all(|o| o.path != "/p/out/index.d.ts"));
    let d = r.diagnostics.iter().find(|d| d.code == 2883);
    assert_eq!(
        d.map(|d| d.message.as_str()),
        Some(
            "The inferred type of 'foo' cannot be named without a reference to 'SomeType' from './node_modules/some-dep/dist/inner'. This is likely not portable. A type annotation is necessary."
        ),
        "{:?}",
        r.diagnostics
    );
}

/// An `@types` package whose `types` condition maps `import` to an `.d.mts` wrapper and
/// `default` to the `export =` declaration (the `@types/ws` layout): the declaration is
/// still named `"sock"` from an ES module, and the `export =` namespace's members through
/// the module (tsc 7.0.2 output for the same project).
#[test]
fn export_equals_namespace_through_default_condition() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"preserve","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["index.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "index.ts",
                "import { SockServer } from \"sock\";\nexport function make() {\n    const s = new SockServer();\n    return { s };\n}\n",
            ),
            (
                "node_modules/@types/sock/package.json",
                r#"{"name":"@types/sock","version":"1.0.0","exports":{".":{"types":{"import":"./index.d.mts","default":"./index.d.ts"}}}}"#,
            ),
            (
                "node_modules/@types/sock/index.d.ts",
                "declare class Sock { send(): void; }\ndeclare namespace Sock {\n    class Server<T extends typeof Sock = typeof Sock> { t: T; }\n    const SockServer: typeof Server;\n    interface SockServer extends Server {}\n}\nexport = Sock;\n",
            ),
            (
                "node_modules/@types/sock/index.d.mts",
                "export { SockServer } from \"./index.js\";\nimport Sock = require(\"./index.js\");\nexport default Sock;\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare function make(): {\n    s: import(\"sock\").Server<typeof import(\"sock\")>;\n};\n"
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}

/// es5ExportEqualsDts: a member of the module's `export =` namespace is named through the
/// module, even in the module's own declaration file.
#[test]
fn export_equals_namespace_member_in_own_file() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "class A {\n    foo() {\n        var aVal: A.B;\n        return aVal;\n    }\n}\nnamespace A {\n    export interface B { }\n}\nexport = A\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "declare class A {\n    foo(): import(\".\").B;\n}\ndeclare namespace A {\n    interface B {\n    }\n}\nexport = A;\n"
    );
}

/// emitClassExpressionInDeclarationFile2: an exported anonymous class type cannot keep
/// private members (TS4094).
#[test]
fn anonymous_class_private_member() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "export var noPrivates = class { static getTags() { } private p = 1; };\n",
        )],
    );
    assert_eq!(codes(&r), vec![4094], "{:?}", r.diagnostics);
}

/// declarationEmitFunctionKeywordProp / declarationEmitDefaultExportWithStaticAssignment:
/// expando properties are a namespace after the function; reserved names are exported
/// from generated locals, assigned declarations are re-exported, and an expando `const`
/// arrow is written as a function.
#[test]
fn expando_namespace() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "export enum E { A }\nexport function f() {}\nf.class = true;\nf.normal = 1;\nf.e = E;\nexport const g = (x: number) => x;\ng.y = \"s\";\nexport default function d() {}\nd.z = 1;\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare enum E {\n    A = 0\n}\nexport declare function f(): void;\nexport declare namespace f {\n    var _a: boolean;\n    export { _a as class };\n    export var normal: number;\n    export { E as e };\n}\nexport declare function g(x: number): number;\nexport declare namespace g {\n    var y: string;\n}\ndeclare function d(): void;\nexport default d;\ndeclare namespace d {\n    var z: number;\n}\n"
    );
}

/// declarationEmitObjectLiteralAccessors1 / declarationEmitRetainsJsdocyComments: JSDoc
/// on its own line before an object literal member is copied onto the member of the
/// inferred type (not one after `{` on the same line); an annotated accessor pair stays a
/// pair.
#[test]
fn object_literal_member_comments() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "export const o = {\n    /** a */\n    a: 1,\n    b: { /** c */ c: 1 },\n    /** g */\n    get x(): string { return \"\"; },\n    /** s */\n    set x(v: number) {},\n};\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const o: {\n    /** a */\n    a: number;\n    b: {\n        c: number;\n    };\n    /** g */\n    get x(): string;\n    /** s */\n    set x(v: number);\n};\n"
    );
}

#[test]
fn unbindable_computed_members_first() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "declare const plain: symbol;\ndeclare const s: string;\ndeclare const n: number;\nexport class A {\n    first = 1;\n    [plain]() { return this; }\n    [s]() { return 1; }\n    static [n]() { return 1; }\n}\nexport interface I {\n    first: number;\n    [s]: string;\n}\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "declare const plain: symbol;\ndeclare const s: string;\ndeclare const n: number;\nexport declare class A {\n    static [n]: () => number;\n    [s]: () => number;\n    [plain]: () => this;\n    first: number;\n}\nexport interface I {\n    first: number;\n}\nexport {};\n"
    );
}

/// Parameter properties keep their own JSDoc (whose text may hold `,` and `(`), and
/// `stripInternal` honours any leading comment but not the previous line's trailing one
/// (observed tsc 7.0.2 on rxjs `AjaxResponse` / `WebSocketSubject`).
#[test]
fn parameter_property_comments_and_strip_internal() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[],"strict":true,"stripInternal":true},"files":["index.ts"]}"#;
    let r = emit(
        config,
        &[(
            "index.ts",
            "export class A {\n    /** @internal */\n    // note\n    hidden = 1;\n    k = 1; /** @internal */\n    kept = 1;\n    constructor(\n        /** First, (the) one. */\n        public readonly a: number,\n        /** Second, too. */\n        public b?: string,\n    ) {}\n}\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare class A {\n    /** First, (the) one. */\n    readonly a: number;\n    /** Second, too. */\n    b?: string | undefined;\n    k: number; /** @internal */\n    kept: number;\n    constructor(\n    /** First, (the) one. */\n    a: number, \n    /** Second, too. */\n    b?: string | undefined);\n}\n"
    );
}

/// Another file's annotation is copied with its text (`'a' | 'b'`, `Array<string>`), its
/// names written from here (`import("./b").Q`, `import("./id").Id`), and its literal values
/// as written (observed with tsc 7.0.2; declarationEmitNoInvalidCommentReuse1).
#[test]
fn cross_file_annotation_reuse() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"esnext","declaration":true,"emitDeclarationOnly":true,"strict":true,"outDir":"out","types":[]},"files":["a.ts"]}"#;
    let r = emit(
        config,
        &[
            ("id.d.ts", "export type Id<T> = T;\n"),
            (
                "b.ts",
                "export interface Q { f: (x: 'a' | 'b', y: Array<string>) => Array<number> }\nexport declare const q: Q;\nexport const lits = { a: 'x', t: `t` } as const;\nexport declare const o: { foo: import(\"./id\" ).Id<{}> };\n",
            ),
            (
                "a.ts",
                "import { q, lits, o } from './b';\nexport const c1 = q.f;\nexport const c2 = { q };\nexport const c3 = lits;\nexport const c4 = o;\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/a.d.ts"),
        "export declare const c1: (x: 'a' | 'b', y: Array<string>) => Array<number>;\nexport declare const c2: {\n    q: import(\"./b\").Q;\n};\nexport declare const c3: {\n    readonly a: 'x';\n    readonly t: `t`;\n};\nexport declare const c4: {\n    foo: import(\"./id\").Id<{}>;\n};\n"
    );
}

/// declarationEmitExportAliasVisibiilityMarking and allowSyntheticDefaultImportsCanPaint-
/// CrossModuleDeclaration: a local declaration another module exports (`export { Suit }`,
/// `export default Color`, `export = Conn`) is named by its export; a default-exported
/// arrow function is written as a function declaration after the export.
#[test]
fn exported_locals_and_default_functions() {
    let config = r#"{"compilerOptions":{"target":"es2015","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["Card.ts","index.ts","file2.ts","usage.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "Types.ts",
                "type Suit = 'Hearts' | 'Spades';\nexport { Suit };\n",
            ),
            (
                "Card.ts",
                "import { Suit } from './Types';\nexport default (suit: Suit) => ({ suit });\n",
            ),
            (
                "index.ts",
                "import card from './Card';\nexport const r = card('Hearts');\n",
            ),
            (
                "color.ts",
                "interface Color { c: string; }\nexport default Color;\n",
            ),
            (
                "file1.ts",
                "import Color from \"./color\";\nexport declare function styled(): Color;\n",
            ),
            (
                "file2.ts",
                "import { styled } from \"./file1\";\nexport const A = styled();\n",
            ),
            ("foo.ts", "class Conn { item = 3; }\nexport = Conn;\n"),
            (
                "usage.ts",
                "type Conn = import(\"./foo\");\ndeclare var x: Conn;\nexport class Wrap { constructor(c = x) {} }\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/Card.d.ts"),
        "import { Suit } from './Types';\nexport default _default;\ndeclare function _default(suit: Suit): {\n    suit: Suit;\n};\n"
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const r: {\n    suit: import(\"./Types\").Suit;\n};\n"
    );
    assert_eq!(
        output(&r, "/p/out/file2.d.ts"),
        "export declare const A: import(\"./color\").default;\n"
    );
    assert_eq!(
        output(&r, "/p/out/usage.d.ts"),
        "export declare class Wrap {\n    constructor(c?: import(\"./foo\"));\n}\n"
    );
}

#[test]
fn default_function_expression_names_are_unique() {
    for (source, expected) in [
        (
            "export const named = 1; export default (function named(): string { return 'x'; });",
            "export declare const named = 1;\nexport default named_1;\ndeclare function named_1(): string;\n",
        ),
        (
            "export const named = 1; export const named_1 = 2; export default (function named(): string { return 'x'; });",
            "export declare const named = 1;\nexport declare const named_1 = 2;\nexport default named_2;\ndeclare function named_2(): string;\n",
        ),
        (
            "export default (function named(): string { return 'x'; });",
            "export default named;\ndeclare function named(): string;\n",
        ),
    ] {
        let r = emit(CONFIG, &[("index.ts", source)]);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        assert_eq!(output(&r, "/p/out/index.d.ts"), expected);
    }
}

#[test]
fn repeated_spreads_keep_declaration_origins_flat() {
    let mut src = String::from("export declare const x0: { a: number };\n");
    let mut expected = String::from("export declare const x0: {\n    a: number;\n};\n");
    for i in 1..=8192 {
        src.push_str(&format!("export const x{i} = {{ ...x{} }};\n", i - 1));
        expected.push_str(&format!(
            "export declare const x{i}: {{\n    a: number;\n}};\n"
        ));
    }
    let r = emit(CONFIG, &[("index.ts", &src)]);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(output(&r, "/p/out/index.d.ts"), expected);
}

#[test]
fn large_syntactic_object_members() {
    let mut src = String::from("let v: number = 1;\nexport const o = {\n");
    let mut expected = String::from("export declare const o: {\n");
    for i in 0..10_000 {
        src.push_str(&format!("p{i}: v,\n"));
        expected.push_str(&format!("    p{i}: number;\n"));
    }
    src.push_str("};\n");
    expected.push_str("};\n");
    let r = emit(CONFIG, &[("index.ts", &src)]);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(output(&r, "/p/out/index.d.ts"), expected);
}

#[test]
fn copied_type_parameters_do_not_capture_generated_names() {
    let r = emit(
        CONFIG,
        &[
            (
                "a.ts",
                "export interface Obj { fn<T>(cb: <T_1>() => T): void }\nexport declare const obj: Obj;\n",
            ),
            (
                "index.ts",
                "import { obj } from './a';\nexport function wrap<T>() { return { ...obj }; }\n",
            ),
        ],
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare function wrap<T>(): {\n    fn: <T_1>(cb: <T_1_1>() => T_1) => void;\n};\n"
    );
}

/// declarationEmitMappedTypeTemplateTypeofSymbol: a mapped type's property keyed by a unique
/// symbol cannot be written (TS4118) and the file gets no `.d.ts`.
#[test]
fn mapped_unique_symbol_property() {
    let config = r#"{"compilerOptions":{"target":"es2015","module":"commonjs","strict":true,"declaration":true,"outDir":"out","types":[]},"files":["a.d.ts","c.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "a.d.ts",
                "export declare const timestampSymbol: unique symbol;\nexport declare const Timestamp: {\n    [TKey in typeof timestampSymbol]: true;\n};\nexport declare function now(): typeof Timestamp;\n",
            ),
            (
                "c.ts",
                "import { now } from \"./a\";\nexport const timestamp = now();\n",
            ),
        ],
    );
    assert_eq!(codes(&r), vec![4118], "{:?}", r.diagnostics);
    assert_eq!(
        r.diagnostics[0].message,
        "The type of this node cannot be serialized because its property '[timestampSymbol]' cannot be serialized."
    );
    assert!(r.outputs.iter().all(|f| !f.path.ends_with("c.d.ts")));
}

/// nodeModulesImportTypeModeDeclarationEmitErrors1: a malformed import type attribute
/// argument recovers as empty `with` attributes. An inferred type whose `as` annotation holds
/// an `assert` import type is written from the type, not reused (an error type is `any`).
#[test]
fn malformed_and_assert_import_attributes() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["index.ts","other.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "node_modules/pkg/package.json",
                r#"{"name":"pkg","version":"1.0.0"}"#,
            ),
            (
                "node_modules/pkg/index.d.ts",
                "export interface R { r: 1 }\n",
            ),
            (
                "index.ts",
                "export type L = import(\"pkg\", {\"resolution-mode\": \"require\"}).R;\nexport type M = import(\"pkg\", [ {\"resolution-mode\": \"require\"} ]).R;\n",
            ),
            (
                "other.ts",
                "export const a = null as any as import(\"pkg\", { assert: {} }).R;\nexport const b = null as any as import(\"pkg\", { assert: {} }).Nope;\nexport const c = null as any as import(\"pkg\", { with: {} }).R;\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export type L = import(\"pkg\", { with: {} });\nexport type M = import(\"pkg\", { with: {} })[{\n    \"resolution-mode\": \"require\";\n}];\n"
    );
    assert_eq!(
        output(&r, "/p/out/other.d.ts"),
        "export declare const a: import(\"pkg\").R;\nexport declare const b: any;\nexport declare const c: import(\"pkg\", { with: {} }).R;\n"
    );
}

/// A type declared in a package chunk its `exports` hide is named through the package entry
/// that re-exports it, under the re-exported name (tsc 7.0.2's output for the same project).
#[test]
fn hidden_chunk_named_through_reexport() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"nodenext","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["index.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "node_modules/vit/package.json",
                r#"{"name":"vit","version":"1.0.0","exports":{".":{"types":"./dist/index.d.ts"}}}"#,
            ),
            (
                "node_modules/vit/dist/chunk.d.ts",
                "interface w<T> { calls: T[] }\nexport { w };\n",
            ),
            (
                "node_modules/vit/dist/index.d.ts",
                "export { w as Mock } from \"./chunk.js\";\n",
            ),
            (
                "node_modules/lib2/package.json",
                r#"{"name":"lib2","version":"1.0.0","types":"index.d.ts"}"#,
            ),
            (
                "node_modules/lib2/index.d.ts",
                "import type { Mock } from \"vit\";\nexport declare function spy(): Mock<string>;\n",
            ),
            (
                "index.ts",
                "import { spy } from \"lib2\";\nexport const s = spy();\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const s: import(\"vit\").Mock<string>;\n"
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}

/// A type declared in a package subpath is named through the package's entry point, which
/// re-exports it, rather than through the subpath (tsc 7.0.2's output for the same project).
#[test]
fn package_entry_over_subpath() {
    let r = emit(
        CONFIG,
        &[
            (
                "node_modules/styled/package.json",
                r#"{"name":"styled","version":"1.0.0"}"#,
            ),
            (
                "node_modules/styled/types/react/index.d.ts",
                "export interface StyledComponent<A> { a: A }\n",
            ),
            (
                "node_modules/styled/index.d.ts",
                "export * from \"./types/react\";\n",
            ),
            (
                "node_modules/emo/package.json",
                r#"{"name":"emo","version":"1.0.0"}"#,
            ),
            (
                "node_modules/emo/index.d.ts",
                "import { StyledComponent } from \"styled\";\nexport default function styled(tag: string): StyledComponent<string>;\n",
            ),
            (
                "index.ts",
                "import styled from \"emo\";\nexport const Form = styled(\"div\");\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const Form: import(\"styled\").StyledComponent<string>;\n"
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}

/// declarationsWithRecursiveInternalTypesProduceUniqueTypeParams: a recursive generic
/// object literal writes its declared type and 11 nested instantiations, then
/// `/*elided*/ any` (tsc 7.0.2's output).
#[test]
fn nested_instantiations_elided() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "export const obj = <T>(t: T) => {\n    return { v: t, next: <U>(u: U) => obj<T & U>(null as any) };\n};\n",
        )],
    );
    let mut want = String::from("export declare const obj: <T>(t: T) => {\n    v: T;\n");
    let mut names = vec!["T".to_owned()];
    for level in 0..12 {
        let u = if level == 0 {
            "U".to_owned()
        } else {
            format!("U_{level}")
        };
        let pad = "    ".repeat(level + 1);
        if level == 11 {
            want += &format!("{pad}next: <{u}>(u: {u}) => /*elided*/ any;\n");
            break;
        }
        names.push(u.clone());
        want += &format!("{pad}next: <{u}>(u: {u}) => {{\n");
        want += &format!("{pad}    v: {};\n", names.join(" & "));
    }
    for level in (0..11).rev() {
        want += &format!("{}}};\n", "    ".repeat(level + 1));
    }
    want += "};\n";
    assert_eq!(output(&r, "/p/out/index.d.ts"), want);
}

/// A signature written from an instantiated type keeps its type parameters' defaults, and
/// its `?` parameters say `| undefined` (tsc 7.0.2's output, with or without
/// `strictNullChecks`); one whose annotation is reused is written as declared.
#[test]
fn instantiated_signature_defaults_and_optional_params() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "interface Box<T> {\n    map<U = T>(f?: (x: T) => U): U;\n    get(i?: number): T;\n    opt(x?: T | undefined): void;\n}\ndeclare const b: Box<number>;\nexport const m = b.map;\nexport const g = b.get;\nexport const o = b.opt;\nexport const id = <T = string>(x?: T) => x;\n",
        )],
    );
    assert_eq!(
        output(&r, "/p/out/index.d.ts"),
        "export declare const m: <U = number>(f?: ((x: number) => U) | undefined) => U;\nexport declare const g: (i?: number) => number;\nexport declare const o: (x?: number | undefined) => void;\nexport declare const id: <T = string>(x?: T) => T | undefined;\n"
    );
}

/// declarationEmitPrivatePromiseLikeInterface (without the lib types): an inferred type
/// that would be written past tsc's serialization limit is TS7056 at its declaration, and
/// the file gets no `.d.ts` (tsc 7.0.2's output for the same project).
#[test]
fn inferred_type_too_long() {
    let config = r#"{"compilerOptions":{"target":"es2015","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["http-client.ts","Api.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "http-client.ts",
                "type TPromise<R, E = any> = {\n    then<A = R, B = never>(\n        onfulfilled?: ((value: R) => A | TPromise<A>) | undefined | null,\n        onrejected?: ((reason: E) => B | TPromise<B>) | undefined | null,\n    ): TPromise<A | B, E>;\n    catch<C = never>(\n        onrejected?: ((reason: E) => C | TPromise<C>) | undefined | null,\n    ): TPromise<R | C, E>;\n};\n\nexport interface HttpResponse<D, F = unknown> {\n    data: D;\n    error: F;\n}\n\nexport class HttpClient {\n    public request = <T = any, F = any>(): TPromise<HttpResponse<T, F>> => {\n        return '' as any;\n    };\n}\n",
            ),
            (
                "Api.ts",
                "import { HttpClient } from \"./http-client\";\n\nexport class Api {\n    constructor(private http: HttpClient) { }\n\n    abc1 = () => this.http.request();\n}\n",
            ),
        ],
    );
    assert_eq!(codes(&r), vec![7056], "{:?}", r.diagnostics);
    assert!(r.outputs.iter().all(|f| !f.path.ends_with("Api.d.ts")));
    assert!(
        r.outputs
            .iter()
            .any(|f| f.path.ends_with("http-client.d.ts"))
    );
}

/// declarationEmitReexportedSymlinkReference2: a type of a symlinked workspace package is
/// named by the linked package's path that re-exports it, not by a path into its target.
#[test]
fn symlinked_package_reexport() {
    let mut fs = MemoryFs::new();
    let files: &[(&str, &str)] = &[
        ("/m/pkg1/dist/index.d.ts", "export * from './types';\n"),
        (
            "/m/pkg1/dist/types.d.ts",
            "export declare type A = {\n    id: string;\n};\nexport declare type B = {\n    id: number;\n};\nexport declare type IdType = A | B;\nexport declare class MetadataAccessor<T, D extends IdType = IdType> {\n    readonly key: string;\n    private constructor();\n    toString(): string;\n    static create<T, D extends IdType = IdType>(key: string): MetadataAccessor<T, D>;\n}\n",
        ),
        (
            "/m/pkg1/package.json",
            r#"{"name":"@raymondfeng/pkg1","version":"1.0.0","main":"dist/index.js","typings":"dist/index.d.ts"}"#,
        ),
        (
            "/m/pkg2/dist/index.d.ts",
            "import \"./secondary\";\nexport * from './types';\n",
        ),
        (
            "/m/pkg2/dist/types.d.ts",
            "export {MetadataAccessor} from '@raymondfeng/pkg1';\n",
        ),
        (
            "/m/pkg2/dist/secondary.d.ts",
            "export {IdType} from '@raymondfeng/pkg1';\n",
        ),
        (
            "/m/pkg2/package.json",
            r#"{"name":"@raymondfeng/pkg2","version":"1.0.0","main":"dist/index.js","typings":"dist/index.d.ts"}"#,
        ),
        ("/m/pkg3/src/index.ts", "export * from './keys';\n"),
        (
            "/m/pkg3/src/keys.ts",
            "import {MetadataAccessor} from \"@raymondfeng/pkg2\";\n\nexport const ADMIN = MetadataAccessor.create<boolean>('1');\n",
        ),
        (
            "/m/pkg3/tsconfig.json",
            r#"{"compilerOptions":{"outDir":"dist","rootDir":"src","target":"es2015","module":"commonjs","strict":true,"esModuleInterop":true,"declaration":true,"emitDeclarationOnly":true,"types":[]}}"#,
        ),
    ];
    for (p, text) in files {
        fs.add_file(p, text);
    }
    fs.add_symlink("/m/pkg2/node_modules/@raymondfeng/pkg1", "/m/pkg1");
    fs.add_symlink("/m/pkg3/node_modules/@raymondfeng/pkg2", "/m/pkg2");
    fs.set_cwd("/m/pkg3");
    let parsed = load_tsconfig(&fs, "/m/pkg3/tsconfig.json");
    let program = load_program(&fs, &parsed.file_names, &parsed.options, None, 1);
    let checker = Checker::new(&program);
    let resolver = CheckerResolver::new(&checker, &parsed.options);
    let host = ts_r_checker::typenode::specifier_host(&fs, &parsed.options);
    let cfg = EmitConfig {
        options: &parsed.options,
        config: Some(&parsed),
        threads: 1,
        case_sensitive: true,
        specifiers: Some(&host),
    };
    let r = ts_r_emit::emit(&program, &resolver, &cfg);
    assert_eq!(
        output(&r, "/m/pkg3/dist/keys.d.ts"),
        "import { MetadataAccessor } from \"@raymondfeng/pkg2\";\nexport declare const ADMIN: MetadataAccessor<boolean, import(\"@raymondfeng/pkg2/dist/secondary\").IdType>;\n"
    );
}

/// change-to-type-that-gets-used-as-global-through-export-in-another-file: an initialized
/// constant or readonly property whose annotation names the still-fresh literal type of
/// `export default 1` is written with the literal as its initializer; `typeof k` and
/// literal type annotations are kept (tsc 7.0.2's output for the same project).
#[test]
fn annotated_fresh_literal_constant() {
    let config = r#"{"compilerOptions":{"target":"es2022","module":"commonjs","declaration":true,"emitDeclarationOnly":true,"outDir":"out","types":[]},"files":["class1.ts","constants.ts","types.d.ts"]}"#;
    let r = emit(
        config,
        &[
            (
                "class1.ts",
                "const a: MagicNumber = 1;\ndeclare const b: MagicNumber;\nlet c: MagicNumber = 1;\nconst d: Local = 1;\ntype Local = 1;\nconst e: typeof k = 1;\nconst k = 1;\nclass C { static readonly r: MagicNumber = 1; readonly q: MagicNumber = 1; readonly s: 2 = 2; }\nconst f: 1 = 1;\nconst g: \"x\" = \"x\";\nconst h: true = true;\nenum E { A, B }\nconst i: E.A = E.A;\nconst j: typeof E.B = E.B;\nconsole.log(a, b, c, d, e, C, f, g, h, i, j);\n",
            ),
            ("constants.ts", "export default 1;\n"),
            (
                "types.d.ts",
                "type MagicNumber = typeof import('./constants').default\n",
            ),
        ],
    );
    assert_eq!(
        output(&r, "/p/out/class1.d.ts"),
        "declare const a = 1;\ndeclare const b: MagicNumber;\ndeclare let c: MagicNumber;\ndeclare const d: Local;\ntype Local = 1;\ndeclare const e: typeof k;\ndeclare const k = 1;\ndeclare class C {\n    static readonly r = 1;\n    readonly q = 1;\n    readonly s: 2;\n}\ndeclare const f: 1;\ndeclare const g: \"x\";\ndeclare const h: true;\ndeclare enum E {\n    A = 0,\n    B = 1\n}\ndeclare const i: E.A;\ndeclare const j: typeof E.B;\n"
    );
}

/// synthesized-module-specifiers-resolve-correctly: a `[Symbol.x]` member is kept when
/// `SymbolConstructor` declares `x` (even as `symbol`) and dropped when it does not
/// (TS2339; tsc 7.0.2's output).
#[test]
fn unresolved_symbol_constructor_key() {
    let r = emit(
        CONFIG,
        &[(
            "index.ts",
            "declare global {\n    interface SymbolConstructor { readonly species: symbol; }\n    var Symbol: SymbolConstructor;\n}\nexport type Nominal<T, Name extends string> = T & {\n    [Symbol.species]: Name;\n    [Symbol.nope]: Name;\n};\nexport interface I { [Symbol.species]: 1; [Symbol.nope]: 4; b: 3 }\n",
        )],
    );
    let out = output(&r, "/p/out/index.d.ts");
    assert!(
        out.contains(
            "export type Nominal<T, Name extends string> = T & {\n    [Symbol.species]: Name;\n};\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("export interface I {\n    [Symbol.species]: 1;\n    b: 3;\n}\n"),
        "{out}"
    );
}
