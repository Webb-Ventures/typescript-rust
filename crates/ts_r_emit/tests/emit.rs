//! Program emit on in-memory projects: determinism across thread counts and the observed
//! tsc 7.0.2 behaviours that decide whether and where outputs are written.

use ts_r_checker::{Checker, ProgramFiles, load_program};
use ts_r_config::{MemoryFs, ParsedConfig, load_tsconfig};
use ts_r_emit::{CheckerResolver, EmitConfig, EmitResult, MemoryWriter, OutputKind};

ts_r_testing::cap_memory!();

fn project(files: &[(&str, &str)]) -> (MemoryFs, ParsedConfig) {
    let mut fs = MemoryFs::new();
    for (p, text) in files {
        fs.add_file(&format!("/p/{p}"), text);
    }
    fs.set_cwd("/p");
    let parsed = load_tsconfig(&fs, "/p/tsconfig.json");
    (fs, parsed)
}

fn emit_with(fs: &MemoryFs, parsed: &ParsedConfig, threads: usize) -> (ProgramFiles, EmitResult) {
    let program = load_program(fs, &parsed.file_names, &parsed.options, None, threads);
    let result = {
        let checker = Checker::new(&program);
        let resolver = CheckerResolver::new(&checker, &parsed.options);
        let host = ts_r_checker::typenode::specifier_host(fs, &parsed.options);
        let cfg = EmitConfig {
            options: &parsed.options,
            config: Some(parsed),
            threads,
            case_sensitive: true,
            specifiers: Some(&host),
        };
        ts_r_emit::emit(&program, &resolver, &cfg)
    };
    (program, result)
}

fn emit(files: &[(&str, &str)]) -> EmitResult {
    let (fs, parsed) = project(files);
    emit_with(&fs, &parsed, 1).1
}

fn paths(r: &EmitResult) -> Vec<&str> {
    r.outputs.iter().map(|o| o.path.as_str()).collect()
}

fn codes(r: &EmitResult) -> Vec<u32> {
    r.diagnostics.iter().map(|d| d.code).collect()
}

const CONFIG: &str = r#"{"compilerOptions":{"target":"es2022","module":"esnext","rootDir":"src","outDir":"out","types":[]}}"#;

#[test]
fn local_const_enum_with_imported_initializer() {
    let r = emit(&[
        ("tsconfig.json", CONFIG),
        ("src/a.ts", "export const enum A { Y = 1 }"),
        (
            "src/b.ts",
            "import { A } from './a'; const enum B { X = A.Y } console.log(B.X);",
        ),
    ]);
    assert!(r.diagnostics.is_empty() && !r.skipped, "{:?}", codes(&r));
    let b = r.outputs.iter().find(|o| o.path == "/p/out/b.js").unwrap();
    assert!(b.text.contains("console.log(1 /* B.X */)"), "{}", b.text);
    assert!(!b.text.contains("import"), "{}", b.text);
}

#[test]
fn cyclic_const_enums_are_refused_during_unchecked_emit() {
    let r = emit(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2022","module":"esnext","rootDir":"src","outDir":"out","types":[],"noCheck":true}}"#,
        ),
        (
            "src/a.ts",
            "import { B } from './b'; export const enum A { X = B.X }",
        ),
        (
            "src/b.ts",
            "import { A } from './a'; export const enum B { X = A.X }",
        ),
        ("src/c.ts", "import { A } from './a'; console.log(A.X);"),
    ]);
    assert!(r.skipped);
    assert!(
        codes(&r).contains(&ts_r_emit::TSR_RESOLVER_UNKNOWN),
        "{:?}",
        codes(&r)
    );
    assert!(!paths(&r).contains(&"/p/out/c.js"));
}

/// The same program emitted with 1 and 4 threads gives identical bytes, maps, order and
/// diagnostics (record 0012: output is deterministic).
#[test]
fn one_and_four_threads_agree() {
    let mut files: Vec<(String, String)> = vec![(
        "tsconfig.json".into(),
        r#"{"compilerOptions":{"target":"es2022","module":"commonjs","rootDir":"src","outDir":"out","sourceMap":true,"importHelpers":false,"types":[]}}"#.into(),
    )];
    for i in 0..48 {
        let next = (i + 1) % 48;
        let text = format!(
            "import {{ f{next}, type T{next} }} from \"./m{next}\";\nimport * as ns from \"./m{next}\";\n\
             export interface T{i} {{ x: number }}\nexport enum E{i} {{ A = {i}, B }}\n\
             export class C{i} {{ static s = {i}; #p = 1; get p(): number {{ return this.#p; }} }}\n\
             export function f{i}(t?: T{next}): number {{ return (t?.x ?? 0) + E{i}.B; }}\n\
             export const v{i} = () => f{next}() + ns.v{next}?.length;\n\
             // a comment {i}\nexport default class {{}}\n"
        );
        files.push((format!("src/m{i}.ts"), text));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let (fs, parsed) = project(&refs);
    let (_, one) = emit_with(&fs, &parsed, 1);
    let (_, four) = emit_with(&fs, &parsed, 4);
    assert_eq!(one.outputs.len(), 96, "{:?}", codes(&one));
    assert_eq!(paths(&one), paths(&four));
    for (a, b) in one.outputs.iter().zip(&four.outputs) {
        assert_eq!(a.text, b.text, "{}", a.path);
    }
    assert_eq!(codes(&one), codes(&four));
    assert_eq!(one.skipped, four.skipped);
    // Program order (dependencies first), `.map` before `.js`.
    assert_eq!(one.outputs[0].kind, OutputKind::JsMap);
    assert_eq!(one.outputs[1].kind, OutputKind::Js);
}

/// `noEmit` produces nothing and counts as skipped; a target below ES2022 is written when
/// nothing in the file needs a lowering ts-r lacks, declarations included;
/// `emitDeclarationOnly` writes only the declarations.
#[test]
fn refusals() {
    let src = ("src/a.ts", "export const a = 1;\n");
    let r = emit(&[
        src,
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"noEmit":true,"types":[]}}"#,
        ),
    ]);
    assert!(r.outputs.is_empty() && r.diagnostics.is_empty() && r.skipped);
    let r = emit(&[
        src,
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2020","types":[]}}"#,
        ),
    ]);
    assert_eq!(paths(&r), ["/p/src/a.js"]);
    assert!(r.diagnostics.is_empty() && !r.skipped);
    let r = emit(&[
        src,
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2022","declaration":true,"types":[]}}"#,
        ),
    ]);
    assert_eq!(paths(&r), ["/p/src/a.js", "/p/src/a.d.ts"]);
    assert!(r.diagnostics.is_empty() && !r.skipped);
    let r = emit(&[
        src,
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2020","declaration":true,"types":[]}}"#,
        ),
    ]);
    assert_eq!(paths(&r), ["/p/src/a.js", "/p/src/a.d.ts"]);
    assert!(r.diagnostics.is_empty() && !r.skipped);
    let r = emit(&[
        src,
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2022","emitDeclarationOnly":true,"declaration":true,"types":[]}}"#,
        ),
    ]);
    assert_eq!(paths(&r), ["/p/src/a.d.ts"]);
    assert!(r.diagnostics.is_empty() && !r.skipped);
}

/// Syntax the lowering does not handle yet is refused per file (TSR2003), here a private
/// member as a `for...of` destructuring target below ES2022; other files are still emitted
/// (`using` is lowered, as is a top-level `using` followed by exports at ESNext).
#[test]
fn unlowered_syntax_refused() {
    let config = |t: &str| {
        format!(
            r#"{{"compilerOptions":{{"target":"{t}","module":"esnext","rootDir":"src","outDir":"out","types":[]}}}}"#
        )
    };
    let es2015 = config("es2015");
    let r = emit(&[
        (
            "src/a.ts",
            "export class C { #x = 1; m(a: number[][]) { for ([this.#x] of a); } }\n",
        ),
        ("src/b.ts", "export function f() { using x = null; }\n"),
        ("tsconfig.json", &es2015),
    ]);
    assert_eq!(paths(&r), ["/p/out/b.js"]);
    assert_eq!(codes(&r), [100_000 + 2003]);
    assert!(
        r.diagnostics[0]
            .message
            .starts_with("Lowering a private name")
    );
    let esnext = config("esnext");
    let r = emit(&[
        (
            "src/a.ts",
            "export const x = 1;\nusing z = null;\nexport { z };\n",
        ),
        ("src/b.ts", "using z = null;\nexport const w = 1;\n"),
        ("tsconfig.json", &esnext),
    ]);
    assert_eq!(paths(&r), ["/p/out/a.js", "/p/out/b.js"]);
    assert!(codes(&r).is_empty());
}

/// An output that is an input is not produced: TS5055, the file's other outputs dropped too.
#[test]
fn overwrite_protection() {
    let r = emit(&[
        ("a.js", "export const a = 1;\n"),
        ("b.ts", "export const b = 2;\n"),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"allowJs":true,"target":"es2022","sourceMap":true,"types":[]}}"#,
        ),
    ]);
    assert_eq!(paths(&r), ["/p/b.js.map", "/p/b.js"]);
    assert_eq!(codes(&r), [5055]);
    assert!(r.skipped);
}

#[test]
fn duplicate_outputs_are_not_written() {
    for source_map in [false, true] {
        let config = format!(
            r#"{{"compilerOptions":{{"target":"es2022","jsx":"react-jsx","rootDir":".","outDir":"dist","sourceMap":{source_map},"types":[]}},"files":["foo.ts","foo.tsx","bar.ts"]}}"#
        );
        let r = emit(&[
            ("foo.ts", "export const a = 1;\n"),
            ("foo.tsx", "export const b = 2;\n"),
            ("bar.ts", "export const c = 3;\n"),
            ("tsconfig.json", &config),
        ]);
        assert!(r.skipped);
        assert_eq!(codes(&r), vec![5056; if source_map { 2 } else { 1 }]);
        assert!(r.diagnostics.iter().any(|d| d.message ==
            "Cannot write file '/p/dist/foo.js' because it would be overwritten by multiple input files."));
        let expected = if source_map {
            vec!["/p/dist/bar.js.map", "/p/dist/bar.js"]
        } else {
            vec!["/p/dist/bar.js"]
        };
        let mut writer = MemoryWriter::default();
        let (written, failed) = ts_r_emit::write_outputs(&r, &mut writer);
        assert!(failed.is_empty());
        assert_eq!(paths(&r), expected);
        assert_eq!(written, expected);
    }
}

#[test]
fn root_dir_importers_follow_program_order_under_no_emit() {
    let mut files = vec![
        (
            "tsconfig.json".to_owned(),
            r#"{"compilerOptions":{"target":"es2022","module":"esnext","rootDir":"src","noEmit":true,"types":[]},"files":["src/a.ts","src/b.ts"]}"#.to_owned(),
        ),
        ("src/a.ts".to_owned(), "import '../shared/barrel';\n".to_owned()),
        ("src/b.ts".to_owned(), "import '../shared/barrel';\n".to_owned()),
    ];
    let mut barrel = String::new();
    for i in 0..128 {
        barrel.push_str(&format!("export * from './m{i}';\n"));
        files.push((
            format!("shared/m{i}.ts"),
            format!("export const v{i} = {i};\n"),
        ));
    }
    files.push(("shared/barrel.ts".to_owned(), barrel));
    let refs: Vec<_> = files
        .iter()
        .map(|(p, text)| (p.as_str(), text.as_str()))
        .collect();
    let (fs, parsed) = project(&refs);
    let program = load_program(&fs, &parsed.file_names, &parsed.options, None, 1);
    let cfg = EmitConfig {
        options: &parsed.options,
        config: Some(&parsed),
        threads: 1,
        case_sensitive: true,
        specifiers: None,
    };
    let (located, global) = ts_r_emit::root_dir_diagnostics(&program, &cfg);
    assert!(global.is_empty());
    assert_eq!(located.len(), 129);
    for d in located {
        assert_eq!(d.code, 6059);
        let importer = &program.files[d.file.0 as usize];
        let specifier = &importer.text[d.span.start as usize..d.span.end as usize];
        if d.message.contains("/p/shared/barrel.ts") {
            assert_eq!(importer.path, "/p/src/a.ts");
            assert_eq!(specifier, "'../shared/barrel'");
        } else {
            assert_eq!(importer.path, "/p/shared/barrel.ts");
            assert!(specifier.starts_with("'./m"), "{specifier}");
        }
    }
}

/// JSON modules are copied only into `outDir`, printed (tsc reformats them), before the
/// importer.
#[test]
fn json_copies() {
    let files = [
        (
            "src/a.ts",
            "import d from \"./d.json\";\nexport const x = d.x;\n",
        ),
        ("src/d.json", "{\"x\":   1}"),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2022","module":"esnext","moduleResolution":"bundler","resolveJsonModule":true,"rootDir":"src","outDir":"out","types":[]}}"#,
        ),
    ];
    let r = emit(&files);
    assert_eq!(paths(&r), ["/p/out/d.json", "/p/out/a.js"]);
    assert_eq!(r.outputs[0].text, "{ \"x\": 1 }\n");
    let no_out = files[2].1.replace(r#","rootDir":"src","outDir":"out""#, "");
    let r = emit(&[files[0], files[1], ("tsconfig.json", &no_out)]);
    assert_eq!(paths(&r), ["/p/src/a.js"]);
}

/// Outputs reach the writer in listing order.
#[test]
fn writer_order() {
    let r = emit(&[
        (
            "src/a.ts",
            "import { b } from \"./b\";\nexport const a = b;\n",
        ),
        ("src/b.ts", "export const b = 2;\n"),
        ("tsconfig.json", CONFIG),
    ]);
    let mut w = MemoryWriter::default();
    let (written, failed) = ts_r_emit::write_outputs(&r, &mut w);
    assert!(failed.is_empty());
    assert_eq!(written, ["/p/out/b.js", "/p/out/a.js"]);
    assert_eq!(w.order, written);
    assert_eq!(
        ts_r_emit::listing(&written),
        "TSFILE: /p/out/b.js\nTSFILE: /p/out/a.js\n"
    );
}

/// `Layout::Tsc` program emit is byte-equal to tsc 7.0.2 on each `tests/tsc_layout/<case>/`
/// project: its `expected/` holds tsc's outputs for the same tsconfig and sources.
/// Synthesized layout (one-line accessors and getters, `__decorate([` lists), comments of
/// replaced statements, names written with escapes, triple-slash directives, decorated
/// CommonJS exports.
#[test]
fn tsc_layout_projects() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsc_layout");
    let mut cases: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    cases.sort();
    assert!(cases.len() >= 5);
    for case in cases {
        let mut files: Vec<(String, String)> = Vec::new();
        let mut expected: Vec<(String, String)> = Vec::new();
        for e in std::fs::read_dir(&case).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "expected" {
                for o in std::fs::read_dir(e.path()).unwrap().flatten() {
                    let text = std::fs::read_to_string(o.path()).unwrap();
                    expected.push((o.file_name().to_string_lossy().to_string(), text));
                }
            } else {
                files.push((name, std::fs::read_to_string(e.path()).unwrap()));
            }
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let r = emit(&refs);
        for (name, want) in &expected {
            let path = format!("/p/out/{name}");
            let got = r.outputs.iter().find(|o| o.path == path);
            let got = got.map(|o| o.text.as_str()).unwrap_or("<not emitted>");
            assert_eq!(got, want, "{}: {name}", case.display());
        }
    }
}

/// A JSON file only imported is written when it lands under `outDir` from inside `rootDir`;
/// outside `rootDir` it is not written and no source of the layout: no TS6059, and not in
/// the common source directory behind TS5011 (tsc 7's `tsbuild/resolveJsonModule/include-
/// only*` scenarios).
#[test]
fn imported_json_outside_root_dir() {
    let json_options = r#""module":"commonjs","resolveJsonModule":true,"esModuleInterop":true,"outDir":"dist","types":[]"#;
    let layout = |config: String, json_path: &str, specifier: &str| {
        let index = format!("import hello from \"{specifier}\";\nexport default hello.hello;\n");
        let (fs, parsed) = project(&[
            ("tsconfig.json", config.as_str()),
            ("src/index.ts", index.as_str()),
            (json_path, r#"{"hello":"world"}"#),
        ]);
        let (program, result) = emit_with(&fs, &parsed, 1);
        let cfg = EmitConfig {
            options: &parsed.options,
            config: Some(&parsed),
            threads: 1,
            case_sensitive: true,
            specifiers: None,
        };
        let (located, global) = ts_r_emit::root_dir_diagnostics(&program, &cfg);
        let mut codes: Vec<u32> = located.iter().map(|d| d.code).collect();
        codes.extend(global.iter().map(|d| d.code));
        let written: Vec<String> = paths(&result).iter().map(|p| (*p).to_owned()).collect();
        (written, codes)
    };
    // Inside: written under `outDir`.
    let (written, codes) = layout(
        format!(r#"{{"compilerOptions":{{{json_options},"rootDir":"."}},"include":["src/**/*"]}}"#),
        "src/hello.json",
        "./hello.json",
    );
    assert!(
        written.contains(&"/p/dist/src/hello.json".to_owned()),
        "{written:?}"
    );
    assert_eq!(codes, Vec::<u32>::new());
    // Outside an explicit `rootDir`: not written, no TS6059.
    let (written, codes) = layout(
        format!(
            r#"{{"compilerOptions":{{{json_options},"rootDir":"src"}},"include":["src/**/*"]}}"#
        ),
        "hello.json",
        "../hello.json",
    );
    assert!(!written.iter().any(|p| p.ends_with(".json")), "{written:?}");
    assert_eq!(codes, Vec::<u32>::new());
}
