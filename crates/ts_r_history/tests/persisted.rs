//! The persisted history after an edit equals a fresh check: dependencies that the export
//! shapes or the manifest must track.

use std::path::{Path, PathBuf};

use ts_r_checker::LibStore;
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_history::persist::{self, Mode};

ts_r_testing::cap_memory!();

const OPTIONS: &str =
    r#""strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler""#;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ts-r-persisted-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn project(options: &str, roots: &[&str], files: &[(&str, &str)]) -> MemoryFs {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    let roots: Vec<String> = roots.iter().map(|r| format!("\"{r}\"")).collect();
    fs.add_file(
        "/p/tsconfig.json",
        &format!(
            "{{\"compilerOptions\": {{{options}}}, \"files\": [{}]}}",
            roots.join(",")
        ),
    );
    for (p, src) in files {
        fs.add_file(&format!("/p/{p}"), src);
    }
    fs
}

/// Checks through the history in `cache`; returns the mode and `file:line TScode` lines.
fn check(fs: &MemoryFs, cache: &ts_r_cache::Cache, libs: &LibStore) -> (Mode, Vec<String>) {
    let cfg = load_tsconfig(fs, "/p/tsconfig.json");
    let out = persist::check(fs, Some(cache), &cfg, Some(libs), 1);
    let lines = out
        .diagnostics
        .iter()
        .map(|d| {
            let (path, text) = match out.sources.get(d.file.0 as usize) {
                Some(Some((p, t))) => (p.rsplit('/').next().unwrap_or(p), t.as_str()),
                _ => ("<lib>", ""),
            };
            let line = text[..(d.span.start as usize).min(text.len())]
                .matches('\n')
                .count()
                + 1;
            format!("{path}:{line} TS{}", d.code)
        })
        .collect();
    (out.mode, lines)
}

fn open_cache(dir: &Path) -> ts_r_cache::Cache {
    ts_r_cache::Cache::open_in(dir, persist::NAMESPACE, persist::FORMAT_VERSION).expect("cache")
}

#[test]
fn node_modules_js_depth_survives_history_reconstruction() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("js-depth");
    let cache = open_cache(&dir);
    for value in ["0", "'ok'", "0"] {
        let fs = project(
            &format!(r#"{OPTIONS}, "allowJs": true, "maxNodeModuleJsDepth": 1, "noEmit": true"#),
            &["main.ts"],
            &[
                (
                    "main.ts",
                    "import { x } from './node_modules/foo';\nimport { y } from 'bar';\nexport const z: string = x;\nexport const w: string = y;\n",
                ),
                (
                    "node_modules/foo/index.js",
                    &format!("exports.x = {value};\n"),
                ),
                (
                    "node_modules/bar/package.json",
                    r#"{"name":"bar","main":"index.js"}"#,
                ),
                (
                    "node_modules/bar/index.js",
                    &format!("exports.y = {value};\n"),
                ),
            ],
        );
        let want = if value == "0" {
            vec!["main.ts:3 TS2322", "main.ts:4 TS2322"]
        } else {
            vec![]
        };
        for _ in 0..2 {
            assert_eq!(check(&fs, &cache, &libs).1, want);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_modules_js_depth_revisits_shallower_routes() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let fs = project(
        &format!(r#"{OPTIONS}, "allowJs": true, "maxNodeModuleJsDepth": 2, "noEmit": true"#),
        &["main.ts"],
        &[
            (
                "main.ts",
                "import { value } from 'pkgA';\nimport { other } from './one';\nexport const a: string = value;\nexport const b: string = other;\n",
            ),
            (
                "node_modules/pkgA/index.js",
                "export { value } from 'shared';\n",
            ),
            ("one.ts", "export { other } from './two';\n"),
            (
                "two.ts",
                "import { value } from 'shared';\nexport const other = value;\n",
            ),
            (
                "node_modules/shared/index.js",
                "export { value } from './child.js';\n",
            ),
            ("node_modules/shared/child.js", "export const value = 1;\n"),
        ],
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = ts_r_checker::load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    assert!(
        program
            .files
            .iter()
            .any(|f| f.path == "/p/node_modules/shared/child.js")
    );
    let dir = temp("js-depth-shallower");
    let cache = open_cache(&dir);
    for _ in 0..2 {
        assert_eq!(
            check(&fs, &cache, &libs).1,
            vec!["main.ts:3 TS2322", "main.ts:4 TS2322"]
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn partial_libraries_keep_missing_global_diagnostics() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("partial-libs");
    let cache = open_cache(&dir);
    let mut history = ts_r_history::incremental::History::default();
    for (src, boolean_missing) in [
        ("const value = 1;\n", true),
        ("const value = 2;\n", true),
        ("interface Boolean {}\nconst value = 2;\n", false),
        ("const value = 2;\n", true),
    ] {
        let fs = project(
            r#""noEmit": true, "lib": ["es2015.symbol"], "skipLibCheck": true"#,
            &["main.ts"],
            &[("main.ts", src)],
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program =
            ts_r_checker::load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let fresh = ts_r_checker::check_program(&program, ts_r_checker::CheckRun::default());
        assert_eq!(
            fresh
                .diagnostics
                .iter()
                .any(|d| { d.code == 2318 && d.message.contains("'Boolean'") }),
            boolean_missing
        );
        assert!(
            fresh
                .diagnostics
                .iter()
                .any(|d| { d.code == 2318 && d.message.contains("'CallableFunction'") })
        );
        for _ in 0..2 {
            let (diags, _) = history.check(
                &program,
                ts_r_checker::CheckRun::default(),
                ts_r_checker::Checker::new,
            );
            assert_eq!(diags, fresh.diagnostics);
            let out = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
            assert_eq!(out.diagnostics, fresh.diagnostics);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn package_metadata_changes_invalidate_untyped_import_diagnostics() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("package-metadata");
    let cache = open_cache(&dir);
    let mut history = ts_r_history::incremental::History::default();
    for package in [
        r#"{"name":"pkg","main":"index.js"}"#,
        r#"{"name":"pkg","version":"1.0.0","main":"index.js"}"#,
        r#"{"name":"pkg","main":"index.js"}"#,
    ] {
        let fs = project(
            &format!(r#"{OPTIONS}, "noEmit": true"#),
            &["main.ts"],
            &[
                ("main.ts", "import { value } from 'pkg';\nvalue;\n"),
                ("node_modules/pkg/package.json", package),
                ("node_modules/pkg/index.js", "exports.value = 1;\n"),
            ],
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program =
            ts_r_checker::load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let fresh = ts_r_checker::check_program(&program, ts_r_checker::CheckRun::default());
        assert_eq!(fresh.diagnostics.len(), 1);
        assert_eq!(fresh.diagnostics[0].code, 7016);
        assert_eq!(
            fresh.diagnostics[0].message.contains("@types/pkg"),
            package.contains("version")
        );
        for _ in 0..2 {
            let (diags, _) = history.check(
                &program,
                ts_r_checker::CheckRun::default(),
                ts_r_checker::Checker::new,
            );
            assert_eq!(diags, fresh.diagnostics);
            let out = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
            assert_eq!(out.diagnostics, fresh.diagnostics);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolution_modes_survive_history_replay() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("resolution-modes");
    let cache = open_cache(&dir);
    let files = |main: &str, ty: &str| {
        project(
            r#""strict": true, "target": "es2022", "module": "nodenext""#,
            &["user.cts"],
            &[
                ("main.cts", main),
                (
                    "user.cts",
                    "import { i } from \"./main.cjs\";\nexport const n: number = i.i;\n",
                ),
                (
                    "node_modules/pkg/package.json",
                    r#"{"name":"pkg","exports":{"import":"./import.d.mts","require":"./require.d.cts"}}"#,
                ),
                ("node_modules/pkg/import.d.mts", ty),
                (
                    "node_modules/pkg/require.d.cts",
                    "export interface R { r: number }\n",
                ),
            ],
        )
    };
    let main = "import type { I } from \"pkg\" with { \"resolution-mode\": \"import\" };\nimport type { R } from \"pkg\";\nexport const i: I = { i: 1 };\nexport const r: R = { r: 1 };\n";
    let ty = "export interface I { i: number }\n";
    let run = |fs: &MemoryFs, mode, codes: &[u32]| {
        let cfg = load_tsconfig(fs, "/p/tsconfig.json");
        let out = persist::check(fs, Some(&cache), &cfg, Some(&libs), 1);
        let fresh = persist::check(fs, None, &cfg, Some(&libs), 1);
        assert_eq!(out.mode, mode);
        assert_eq!(out.files, fresh.files);
        assert_eq!(out.diagnostics, fresh.diagnostics);
        assert_eq!(
            out.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
            codes
        );
    };
    run(&files(main, ty), Mode::Cold, &[]);
    run(&files(main, ty), Mode::Unchanged, &[]);
    let edited = format!("{main}\n");
    run(&files(&edited, ty), Mode::Incremental, &[]);
    run(&files(&edited, ty), Mode::Unchanged, &[]);
    run(
        &files(&edited, "export interface I { i: string }\n"),
        Mode::Incremental,
        &[2322, 2322],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commonjs_mts_resolution_mode_survives_history_replay() {
    let dir = temp("commonjs-mts-resolution-mode");
    // A minimal lib with the core global types (without them every check reports TS2318;
    // `ImportAttributes` for the import attributes).
    std::fs::create_dir_all(dir.join("libs")).unwrap();
    std::fs::write(
        dir.join("libs/lib.es5.d.ts"),
        "interface Array<T> {}\ninterface Boolean {}\ninterface CallableFunction {}\n\
         interface Function {}\ninterface IArguments {}\ninterface NewableFunction {}\n\
         interface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n\
         interface ImportAttributes {}\n",
    )
    .unwrap();
    let libs = LibStore::new(dir.join("libs"));
    let cache = open_cache(&dir);
    let main = "import type { R } from \"pkg\" with { \"resolution-mode\": \"require\" };\n";
    for (mode, source) in [
        (Mode::Cold, main.to_owned()),
        (Mode::Unchanged, main.to_owned()),
        (Mode::Incremental, format!("{main}\n")),
        (Mode::Unchanged, format!("{main}\n")),
    ] {
        let fs = project(
            r#""module": "commonjs", "moduleResolution": "bundler", "noEmit": true, "lib": ["es5"], "types": []"#,
            &["a.mts"],
            &[
                ("a.mts", &source),
                (
                    "node_modules/pkg/package.json",
                    r#"{"name":"pkg","exports":{"import":"./import.d.mts","require":"./require.d.cts"}}"#,
                ),
                ("node_modules/pkg/import.d.mts", "export interface I {}\n"),
                ("node_modules/pkg/require.d.cts", "export interface R {}\n"),
            ],
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let out = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        let fresh = persist::check(&fs, None, &cfg, Some(&libs), 1);
        assert_eq!(out.diagnostics, fresh.diagnostics);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(out.files, fresh.files);
        assert_eq!(out.mode, mode);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inferred_exports_depend_on_what_they_are_inferred_from() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("inferred");
    let cache = open_cache(&dir);
    let roots = ["a.ts", "b.ts", "c.ts"];
    let b = "import { x } from \"./a\";\nexport const y = x;\n";
    let c = "import { y } from \"./b\";\nexport const z: number = y;\n";
    let fs = project(
        OPTIONS,
        &roots,
        &[("a.ts", "export const x = 1;\n"), ("b.ts", b), ("c.ts", c)],
    );
    assert_eq!(check(&fs, &cache, &libs), (Mode::Cold, vec![]));
    let fs = project(
        OPTIONS,
        &roots,
        &[
            ("a.ts", "export const x = \"s\";\n"),
            ("b.ts", b),
            ("c.ts", c),
        ],
    );
    assert_eq!(
        check(&fs, &cache, &libs),
        (Mode::Incremental, vec!["c.ts:2 TS2322".to_owned()])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dependency_files_depend_on_the_globals_they_name() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("external-globals");
    let cache = open_cache(&dir);
    let roots = ["globals.d.ts", "c.ts"];
    let files = |x: &str| {
        project(
            OPTIONS,
            &roots,
            &[
                (
                    "node_modules/pkg/package.json",
                    r#"{"name":"pkg","types":"index.d.ts"}"#,
                ),
                (
                    "node_modules/pkg/index.d.ts",
                    "export declare const value: AppConfig;\n",
                ),
                (
                    "globals.d.ts",
                    &format!("interface AppConfig {{ x: {x} }}\n"),
                ),
                (
                    "c.ts",
                    "import { value } from \"pkg\";\nexport const n: number = value.x;\n",
                ),
            ],
        )
    };
    assert_eq!(check(&files("number"), &cache, &libs), (Mode::Cold, vec![]));
    assert_eq!(
        check(&files("string"), &cache, &libs),
        (Mode::Incremental, vec!["c.ts:2 TS2322".to_owned()])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_lib_that_appears_invalidates_the_output() {
    let Some(found) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let dir = temp("missing-lib");
    let lib_dir = dir.join("libs");
    std::fs::create_dir_all(&lib_dir).expect("lib dir");
    for e in std::fs::read_dir(found.dir()).expect("libs").flatten() {
        let name = e.file_name();
        if name != "lib.dom.d.ts" {
            std::fs::copy(e.path(), lib_dir.join(&name)).expect("copy lib");
        }
    }
    let cache = open_cache(&dir.join("cache"));
    let fs = project(
        r#""strict": true, "target": "es2022", "lib": ["es2022", "dom"]"#,
        &["a.ts"],
        &[("a.ts", "document;\n")],
    );
    let (mode, lines) = check(&fs, &cache, &LibStore::new(&lib_dir));
    assert_eq!(mode, Mode::Cold);
    assert_eq!(lines, ["a.ts:1 TS2584"]);
    std::fs::copy(
        found.dir().join("lib.dom.d.ts"),
        lib_dir.join("lib.dom.d.ts"),
    )
    .expect("copy dom");
    assert_eq!(
        check(&fs, &cache, &LibStore::new(&lib_dir)),
        (Mode::Incremental, vec![])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wildcard_types_survive_edits_and_track_package_changes() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    for (name, root, extra) in [
        ("custom", "types", r#", "typeRoots": ["./types"]"#),
        ("default", "node_modules/@types", ""),
    ] {
        let dir = temp(&format!("wildcard-{name}"));
        let cache = open_cache(&dir);
        let options = format!(r#"{OPTIONS}, "types": ["*"]{extra}"#);
        let files = |edited: bool, foo: bool, bar: bool| {
            let mut fs = project(
                &options,
                &["main.ts"],
                &[(
                    "main.ts",
                    if edited {
                        "const n: number = foo;\n\n"
                    } else {
                        "const n: number = foo;\n"
                    },
                )],
            );
            fs.add_dir(&format!("/p/{root}/empty"));
            fs.add_file(
                &format!("/p/{root}/.hidden/index.d.ts"),
                "not valid TypeScript",
            );
            if foo {
                fs.add_file(
                    &format!("/p/{root}/foo/index.d.ts"),
                    "declare const foo: number;\n",
                );
            }
            if bar {
                fs.add_file(
                    &format!("/p/{root}/bar/index.d.ts"),
                    "declare const bar: number;\n",
                );
            }
            fs
        };
        let run = |fs: &MemoryFs, mode, codes: &[u32]| {
            let cfg = load_tsconfig(fs, "/p/tsconfig.json");
            let out = persist::check(fs, Some(&cache), &cfg, Some(&libs), 1);
            let fresh = persist::check(fs, None, &cfg, Some(&libs), 1);
            assert_eq!(out.mode, mode);
            assert_eq!(out.files, fresh.files);
            assert_eq!(out.diagnostics, fresh.diagnostics);
            assert_eq!(
                out.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
                codes
            );
            if mode == Mode::Unchanged {
                assert_eq!(out.parsed, 0);
            }
        };
        run(&files(false, true, false), Mode::Cold, &[]);
        run(&files(false, true, false), Mode::Unchanged, &[]);
        run(&files(true, true, false), Mode::Incremental, &[]);
        run(&files(true, true, true), Mode::Incremental, &[]);
        run(&files(true, true, true), Mode::Unchanged, &[]);
        run(&files(true, false, true), Mode::Incremental, &[2304]);
        run(&files(true, true, false), Mode::Incremental, &[]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
