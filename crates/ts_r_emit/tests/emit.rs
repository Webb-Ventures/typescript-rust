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
/// (`using` is lowered below ESNext and kept as written at ESNext).
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

/// A file outside `rootDir` that is both imported and matched by `include` is reported at
/// the import, with both reasons; one only matched by `include` is global (tsc 7.0.2: the
/// config's absolute path in the include reason).
#[test]
fn root_dir_imported_and_included_is_located_with_both_reasons() {
    let files = [
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"noEmit":true,"rootDir":"src","types":[]},"include":["src","lib"]}"#,
        ),
        (
            "src/a.ts",
            "import { l } from \"../lib/l\";\nexport const a: string = l;\n",
        ),
        ("lib/l.ts", "export const l = \"x\";\n"),
        ("lib/m.ts", "export const m = 1;\n"),
    ];
    let (fs, parsed) = project(&files);
    let program = load_program(&fs, &parsed.file_names, &parsed.options, None, 1);
    let cfg = EmitConfig {
        options: &parsed.options,
        config: Some(&parsed),
        threads: 1,
        case_sensitive: true,
        specifiers: None,
    };
    let (located, global) = ts_r_emit::root_dir_diagnostics(&program, &cfg);
    assert_eq!(located.len(), 1, "{located:?}");
    let d = &located[0];
    assert_eq!(program.files[d.file.0 as usize].path, "/p/src/a.ts");
    assert_eq!(
        d.message,
        "File '/p/lib/l.ts' is not under 'rootDir' '/p/src'. 'rootDir' is expected to contain all source files.\n  The file is in the program because:\n    Imported via \"../lib/l\" from file '/p/src/a.ts'\n    Matched by include pattern 'lib' in '/p/tsconfig.json'"
    );
    assert_eq!(global.len(), 1, "{global:?}");
    assert!(global[0].message.contains("'/p/lib/m.ts'"));
    assert_eq!(
        global[0].related,
        vec![
            "The file is in the program because:".to_owned(),
            "  Matched by include pattern 'lib' in '/p/tsconfig.json'".to_owned()
        ]
    );
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

fn conflicts(files: &[(&str, &str)]) -> Vec<(u32, String)> {
    let (fs, parsed) = project(files);
    let program = load_program(&fs, &parsed.file_names, &parsed.options, None, 1);
    let cfg = EmitConfig {
        options: &parsed.options,
        config: Some(&parsed),
        threads: 1,
        case_sensitive: true,
        specifiers: None,
    };
    ts_r_emit::output_path_conflicts(&program, &cfg)
}

/// TS5055 for an output that is an input, TS5056 for an output two sources write; only
/// emitted files count (JavaScript needs `allowJs`, `node_modules` files are libraries').
/// Baselines: `jsFileCompilationWithJsEmitPathSameAsInput`,
/// `moduleResolution_explicitNodeModulesImport`, `parserReturnStatement4`.
#[test]
fn output_path_conflicts() {
    let config = r#"{ "compilerOptions": { "allowJs": true, "outDir": "out", "rootDir": "." }, "files": ["a.ts", "out/a.js"] }"#;
    let found = conflicts(&[
        ("tsconfig.json", config),
        ("a.ts", "export {};\n"),
        ("out/a.js", "var x;\n"),
    ]);
    assert_eq!(found, vec![(5055, "/p/out/a.js".to_owned())]);

    let config = r#"{ "compilerOptions": { "allowJs": true }, "files": ["a.ts", "a.js"] }"#;
    let found = conflicts(&[
        ("tsconfig.json", config),
        ("a.ts", "export {};\n"),
        ("a.js", "var x;\n"),
    ]);
    // Raw, as tsc collects them: sorting the diagnostics drops the repeated TS5055.
    let a = "/p/a.js".to_owned();
    assert_eq!(found, vec![(5055, a.clone()), (5055, a.clone()), (5056, a)]);

    let config = r#"{ "compilerOptions": { "outDir": "out" }, "files": ["a.ts", "a.tsx"] }"#;
    let found = conflicts(&[
        ("tsconfig.json", config),
        ("a.ts", "export {};\n"),
        ("a.tsx", "export {};\n"),
    ]);
    assert_eq!(found, vec![(5056, "/p/out/a.js".to_owned())]);

    // No allowJs: the JavaScript root is not emitted, so its own path is no conflict.
    let config = r#"{ "files": ["a.js"] }"#;
    assert!(conflicts(&[("tsconfig.json", config), ("a.js", "var x;\n")]).is_empty());

    // A library file reached by a relative import is not emitted.
    let config = r#"{ "compilerOptions": { "allowJs": true, "maxNodeModuleJsDepth": 1 }, "files": ["src/a.ts"] }"#;
    let found = conflicts(&[
        ("tsconfig.json", config),
        ("src/a.ts", "import \"../node_modules/foo\";\n"),
        ("node_modules/foo/index.js", "var x;\n"),
    ]);
    assert!(found.is_empty(), "{found:?}");

    let config = r#"{ "compilerOptions": { "noEmit": true }, "files": ["a.ts", "a.tsx"] }"#;
    let found = conflicts(&[
        ("tsconfig.json", config),
        ("a.ts", "export {};\n"),
        ("a.tsx", "export {};\n"),
    ]);
    assert!(found.is_empty());
}

/// TS5009 when `outDir` is set, there is no config file or `rootDir`, and the emitted files
/// sit on different drives (baselines `commonSourceDir1`..`commonSourceDir4`).
#[test]
fn no_common_source_directory() {
    let run = |second: &str, case_sensitive: bool| -> Vec<u32> {
        let mut fs = if case_sensitive {
            MemoryFs::new()
        } else {
            MemoryFs::case_insensitive()
        };
        fs.set_cwd("/p");
        fs.add_file("A:/foo/bar.ts", "var x: number;\n");
        fs.add_file(second, "var y: number;\n");
        fs.add_file(
            "/p/tsconfig.json",
            r#"{ "compilerOptions": { "outDir": "A:/" }, "files": [] }"#,
        );
        let parsed = load_tsconfig(&fs, "/p/tsconfig.json");
        let roots = vec!["A:/foo/bar.ts".to_owned(), second.to_owned()];
        let program = load_program(&fs, &roots, &parsed.options, None, 1);
        let cfg = EmitConfig {
            options: &parsed.options,
            config: None,
            threads: 1,
            case_sensitive,
            specifiers: None,
        };
        let (_, global) = ts_r_emit::root_dir_diagnostics(&program, &cfg);
        global.iter().map(|d| d.code).collect()
    };
    assert_eq!(run("A:/foo/baz.ts", true), Vec::<u32>::new());
    assert_eq!(run("B:/foo/baz.ts", true), vec![5009]);
    assert_eq!(run("a:/foo/baz.ts", true), vec![5009]);
    assert_eq!(run("a:/foo/baz.ts", false), Vec::<u32>::new());
}

/// With the automatic JSX runtime, an unused `React` import is elided only when the runtime
/// module resolves in the program (tsc then does not mark the classic factory's root used).
#[test]
fn jsx_runtime_found_elides_unused_react_import() {
    const JSX_CONFIG: &str = r#"{"compilerOptions":{"target":"es2022","module":"esnext","jsx":"react-jsx","rootDir":"src","outDir":"out","types":[]}}"#;
    const REACT: &str = "declare module \"react\" { const React: any; export default React; }\n\
        declare namespace JSX { interface IntrinsicElements { [k: string]: any } }\n";
    const RUNTIME: &str =
        "declare module \"react/jsx-runtime\" { export function jsx(...a: any[]): any; }\n";
    const APP: &str = "import React from \"react\";\nexport const a = <div />;\n";
    let found = emit(&[
        ("tsconfig.json", JSX_CONFIG),
        ("src/react.d.ts", REACT),
        ("src/rt.d.ts", RUNTIME),
        ("src/a.tsx", APP),
    ]);
    let a = found
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/a.js")
        .unwrap();
    assert!(!a.text.contains("from \"react\""), "{}", a.text);
    assert!(a.text.contains("react/jsx-runtime"), "{}", a.text);
    let missing = emit(&[
        ("tsconfig.json", JSX_CONFIG),
        ("src/react.d.ts", REACT),
        ("src/a.tsx", APP),
    ]);
    let a = missing
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/a.js")
        .unwrap();
    assert!(a.text.contains("import React from \"react\""), "{}", a.text);
}

/// Decorator metadata from the checker (tsc 7.0.2's output for the same project): an imported
/// class reads through its module variable, an interface is `Object`, an alias of `string`
/// is `String`, a class exported with `export type` is `Function` and keeps no import, an
/// unresolved name is guarded, and a decorated `declare` field is still decorated.
#[test]
fn decorator_metadata_from_the_checker() {
    let r = emit(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"es2022","module":"commonjs","rootDir":"src","outDir":"out","types":[],"experimentalDecorators":true,"emitDecoratorMetadata":true,"strict":true,"noEmitHelpers":true}}"#,
        ),
        (
            "src/a.ts",
            "export class Foo {}\nexport interface I {}\nexport type Alias = string;\nclass Hidden {}\nexport type { Hidden };\n",
        ),
        (
            "src/b.ts",
            r#"import { Foo, I, Alias, Hidden } from "./a";
declare function dec(...a: any[]): any;
export class C {
    @dec f!: Foo;
    @dec i!: I;
    @dec s!: Alias;
    @dec h!: Hidden;
    @dec u!: Missing;
    @dec declare d: Foo;
}
"#,
        ),
    ]);
    let b = r.outputs.iter().find(|o| o.path == "/p/out/b.js").unwrap();
    let want = r#""use strict";
var _a;
Object.defineProperty(exports, "__esModule", { value: true });
exports.C = void 0;
const a_1 = require("./a");
class C {
    f;
    i;
    s;
    h;
    u;
}
exports.C = C;
__decorate([
    dec,
    __metadata("design:type", a_1.Foo)
], C.prototype, "f", void 0);
__decorate([
    dec,
    __metadata("design:type", Object)
], C.prototype, "i", void 0);
__decorate([
    dec,
    __metadata("design:type", String)
], C.prototype, "s", void 0);
__decorate([
    dec,
    __metadata("design:type", Function)
], C.prototype, "h", void 0);
__decorate([
    dec,
    __metadata("design:type", typeof (_a = typeof Missing !== "undefined" && Missing) === "function" ? _a : Object)
], C.prototype, "u", void 0);
__decorate([
    dec,
    __metadata("design:type", a_1.Foo)
], C.prototype, "d", void 0);
"#;
    assert_eq!(b.text, want);
}

/// Enum members whose initializers read another file are folded from the checker, with
/// `isolatedModules` too (computedEnumMemberSyntacticallyString2).
#[test]
fn enum_members_from_other_files_are_folded() {
    let src = "import { BAR } from './bar';\nconst LOCAL = 'LOCAL';\nenum Foo {\n  A = `${BAR}`,\n  B = LOCAL,\n  C = B,\n  D = C + 'BAR',\n  E1 = (`${BAR}`) as string,\n  E2 = `${BAR}`!,\n  F = BAR,\n  G = 2 + BAR,\n  H = A,\n  I = H + BAR,\n  J = H\n}\n";
    for isolated in [false, true] {
        let config = format!(
            r#"{{"compilerOptions":{{"target":"esnext","module":"esnext","rootDir":"src","outDir":"out","types":[],"isolatedModules":{isolated}}}}}"#
        );
        let r = emit(&[
            ("tsconfig.json", &config),
            ("src/foo.ts", src),
            ("src/bar.ts", "export const BAR = 'bar';"),
        ]);
        let foo = r
            .outputs
            .iter()
            .find(|o| o.path == "/p/out/foo.js")
            .unwrap();
        for line in [
            r#"Foo["A"] = "bar";"#,
            r#"Foo["D"] = "LOCALBAR";"#,
            "Foo[Foo[\"E1\"] = (`${BAR}`)] = \"E1\";",
            "Foo[Foo[\"E2\"] = `${BAR}`] = \"E2\";",
            r#"Foo["F"] = "bar";"#,
            r#"Foo["G"] = "2bar";"#,
            r#"Foo["I"] = "barbar";"#,
            r#"Foo["J"] = "bar";"#,
        ] {
            assert!(
                foo.text.contains(line),
                "isolated {isolated}: {line}\n{}",
                foo.text
            );
        }
    }
}

/// `export import A = ns.A` of an interface imported from another file is dropped, and
/// so is the namespace import only it used (typeOnlyExportImportAliasElidesNamespaceImport).
/// Unused aliases release their import the same way. Expected outputs are tsc 7.0.2's.
#[test]
fn dropped_aliases_release_their_imports() {
    let r = emit(&[
        ("tsconfig.json", CONFIG),
        (
            "src/a.ts",
            "export interface A { a: number; }\nexport const v = 1;\n",
        ),
        (
            "src/b.ts",
            "import * as ns from \"./a\";\n\nexport import A = ns.A;\n",
        ),
        // An unused alias is dropped, and with it the import only it used, value or not.
        (
            "src/c.ts",
            "import * as ns from \"./a\";\nimport A = ns.A;\n",
        ),
        (
            "src/d.ts",
            "import * as ns from \"./a\";\nimport V = ns.v;\n",
        ),
        // A chain of unused aliases releases its root too.
        (
            "src/e.ts",
            "import * as ns from \"./a\";\nimport N = ns;\nimport V = N.v;\n",
        ),
        (
            "src/f.ts",
            "import * as ns from \"./a\";\nexport import V = ns.v;\n",
        ),
    ]);
    assert!(!r.skipped, "{:?}", codes(&r));
    let out = |name: &str| {
        let path = format!("/p/out/{name}.js");
        r.outputs
            .iter()
            .find(|o| o.path == path)
            .unwrap()
            .text
            .clone()
    };
    for name in ["b", "c", "d", "e"] {
        assert_eq!(out(name), "export {};\n", "{name}.js");
    }
    assert_eq!(
        out("f"),
        "import * as ns from \"./a\";\nexport var V = ns.v;\n"
    );
}

/// JavaScript declarations are refused (TSR2006, the file's `.js` still written) when the
/// file uses a pattern tsc declares from that the emitter does not write: CommonJS
/// exports, a dotted `@typedef` name, a `@callback` rest parameter, `@type` on a function
/// declaration, a `this` property a base class declares, a class's statics.
#[test]
fn javascript_declarations_refused_for_unwritten_patterns() {
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["a.js"] }"#;
    for src in [
        "module.exports = { a: 1 };\nmodule.exports.b = 2;\n",
        "const y = require(\"./y\");\nmodule.exports = y;\n",
        "exports.a = 1;\n",
        "/** @typedef {number} ns.T */\nexport const x = 1;\n",
        "/**\n * @callback F\n * @param {...string} args\n */\nexport const x = 1;\n",
        "/** @type {(a: number) => number} */\nexport function f(a) { return a; }\n",
        "export class A { x = 1; }\nexport class B extends A { constructor() { super(); this.x = 2; } }\n",
        "export class C {}\nC.count = 1;\n",
        "export class D { m() { this.v = this.v + 1; } }\n",
        "export class E { constructor() { this[\"k\"] = 1; } }\n",
    ] {
        let r = emit(&[("tsconfig.json", config), ("a.js", src)]);
        assert_eq!(
            codes(&r),
            vec![ts_r_emit::dts::TSR_JS_DECLARATIONS],
            "{src}"
        );
        assert!(r.skipped, "{src}");
        assert_eq!(paths(&r), vec!["/p/out/a.js"], "{src}");
    }
    // Patterns that declare nothing are written.
    for src in [
        "export function f() { this.x = 1; }\n",
        "export class F { constructor() { this.constructor = 1; } }\n",
        "const k = \"k\";\nexport class G { constructor() { this[k] = 1; } }\n",
        "export class H { static m() {} }\nH.m.x = 1;\n",
    ] {
        let r = emit(&[("tsconfig.json", config), ("a.js", src)]);
        assert!(
            r.diagnostics.is_empty() && !r.skipped,
            "{src}: {:?}",
            codes(&r)
        );
        assert_eq!(paths(&r), vec!["/p/out/a.js", "/p/out/a.d.ts"], "{src}");
    }
}

/// A CommonJS file whose only export is `module.exports = v;` is `export = v;` first, then
/// its declarations (tsc 7.0.2, jsDeclarationsExportAssignedClassInstance1); a value that
/// is no entity name goes through `declare const _exports`.
#[test]
fn javascript_commonjs_export_assignment_is_written() {
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["a.js"] }"#;
    for (src, want) in [
        (
            "class Foo {}\nmodule.exports = new Foo();\n",
            "declare const _exports: Foo;\nexport = _exports;\ndeclare class Foo {\n}\n",
        ),
        (
            "/** @typedef {number} N */\nfunction foo() {}\nmodule.exports = foo;\n",
            "export = foo;\nexport type N = number;\n/** @typedef {number} N */\ndeclare function foo(): void;\n",
        ),
        // A local `module` does not stop it: tsc 7.0.2 writes the same `export =`.
        (
            "const module = { exports: 0 };\nmodule.exports = 1;\n",
            "declare const _exports = 1;\nexport = _exports;\n",
        ),
    ] {
        let r = emit(&[("tsconfig.json", config), ("a.js", src)]);
        assert!(
            r.diagnostics.is_empty() && !r.skipped,
            "{src}: {:?}",
            codes(&r)
        );
        let d = r
            .outputs
            .iter()
            .find(|o| o.path == "/p/out/a.d.ts")
            .unwrap();
        assert_eq!(d.text, want, "{src}");
    }
}

/// A `@property` description is trimmed of the blank lines around it, however many there
/// are (by index, not by shifting the lines; expectation from tsc 7.0.2 with 5 blank lines).
#[test]
fn javascript_property_description_with_many_blank_lines() {
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["a.js"] }"#;
    let blank = " *\n".repeat(100_000);
    let src = format!(
        "/**\n * @typedef {{Object}} T\n * @property {{number}} x\n{blank} * the x\n *\n * more\n{blank} * @property {{number}} y\n{blank} */\n\n/** @type {{T}} */\nexport const t = {{ x: 1, y: 2 }};\n"
    );
    let r = emit(&[("tsconfig.json", config), ("a.js", &src)]);
    assert!(r.diagnostics.is_empty() && !r.skipped, "{:?}", codes(&r));
    let d = r
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/a.d.ts")
        .unwrap();
    let want = "export type T = {\n    /**\n     * the x\n     *\n     * more\n     */\n    x: number;\n    y: number;\n};\n";
    let tail: Vec<&str> = d.text.lines().rev().take(12).collect();
    assert!(d.text.contains(want), "{tail:?}");
}

/// A `@typedef` in a class body is hoisted before the class; the comments before an empty
/// `;` member go with it (observed with tsc 7.0.2, typedefOnSemicolonClassElement).
#[test]
fn javascript_typedef_in_a_class_body_after_a_semicolon_member() {
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["a.js"] }"#;
    let src =
        "export class P {\n  /** @typedef {string} A */\n  ;\n  /** @type {A} */\n  a = 'ok'\n}\n";
    let r = emit(&[("tsconfig.json", config), ("a.js", src)]);
    assert!(r.diagnostics.is_empty() && !r.skipped, "{:?}", codes(&r));
    let d = r
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/a.d.ts")
        .unwrap();
    assert_eq!(
        d.text,
        "export type A = string;\nexport declare class P {\n    /** @type {A} */\n    a: A;\n}\n"
    );
}

#[test]
fn javascript_commonjs_refused_with_an_ambient_global_module() {
    // A global `module` / `exports` declared in a declaration file (as @types/node does) is
    // still the CommonJS module object: tsc 7 writes `export = _exports`, so the file is
    // refused rather than written as `export {};`.
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["types.d.ts", "a.js"] }"#;
    for (types, src) in [
        (
            "declare var module: { exports: any };\n",
            "module.exports = { a: 1 };\nfunction f() { module.exports = 2; }\n",
        ),
        ("declare var exports: any;\n", "exports.a = 1;\n"),
    ] {
        let r = emit(&[
            ("tsconfig.json", config),
            ("types.d.ts", types),
            ("a.js", src),
        ]);
        assert_eq!(
            codes(&r),
            vec![ts_r_emit::dts::TSR_JS_DECLARATIONS],
            "{src}"
        );
        assert!(r.skipped, "{src}");
    }
    // A local of that name is not the module object.
    let src = "const module = { exports: {} };\nmodule.exports = 1;\nexport const a = 1;\n";
    let r = emit(&[("tsconfig.json", config), ("types.d.ts", ""), ("a.js", src)]);
    assert!(r.diagnostics.is_empty() && !r.skipped, "{:?}", codes(&r));
}

#[test]
fn javascript_property_names_module_and_exports_are_not_commonjs() {
    // `module` / `exports` as a property, method, binding or label name reads nothing
    // (tsc 7 writes these declarations).
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["a.js"] }"#;
    let src = "export const settings = { module: 1, exports: 2 };\n\
               export class K { module() {} exports = 1; }\n\
               export function f({ module: m }) { return m; }\n\
               module: for (;;) { break module; }\n";
    let r = emit(&[("tsconfig.json", config), ("a.js", src)]);
    assert!(r.diagnostics.is_empty() && !r.skipped, "{:?}", codes(&r));
    let d = r
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/a.d.ts")
        .unwrap();
    assert_eq!(
        d.text,
        "export declare const settings: {\n    module: number;\n    exports: number;\n};\n\
         export declare class K {\n    module(): void;\n    exports: number;\n}\n\
         export declare function f({ module: m }: {\n    module: any;\n}): any;\n"
    );
    // A shorthand property or a computed key reads the name: CommonJS.
    for src in [
        "export const o = { module };\n",
        "export const o = { [exports.k]: 1 };\n",
    ] {
        let r = emit(&[("tsconfig.json", config), ("a.js", src)]);
        assert_eq!(
            codes(&r),
            vec![ts_r_emit::dts::TSR_JS_DECLARATIONS],
            "{src}"
        );
    }
}

#[test]
fn javascript_many_this_properties_are_linear() {
    // 20,000 `this.pN = 0` in one constructor: collecting the properties and their
    // comments must not rescan the constructor per property (it was quadratic).
    let config = r#"{ "compilerOptions": { "allowJs": true, "declaration": true, "outDir": "out", "types": [] }, "files": ["a.js"] }"#;
    let n = 50_000;
    let mut src = String::from("export class C {\n    constructor() {\n");
    for i in 0..n {
        src.push_str(&format!("        /** p{i} */\n        this.p{i} = 0;\n"));
    }
    src.push_str("        this.p0 = 1;\n    }\n}\n");
    let start = std::time::Instant::now();
    let r = emit(&[("tsconfig.json", config), ("a.js", &src)]);
    let took = start.elapsed();
    assert!(r.diagnostics.is_empty() && !r.skipped, "{:?}", codes(&r));
    let d = r
        .outputs
        .iter()
        .find(|o| o.path == "/p/out/a.d.ts")
        .unwrap();
    // The checker's flow analysis gives up on a constructor this long and adds
    // `| undefined`; this test is about the emitter's linearity, not that type.
    assert_eq!(d.text.matches(": number").count(), n);
    assert!(
        d.text.contains("    /** p1 */\n    p1: number"),
        "{}",
        &d.text[..400]
    );
    assert!(took.as_secs() < 20, "took {took:?}");
}
