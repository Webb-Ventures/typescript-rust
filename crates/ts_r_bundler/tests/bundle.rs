//! End-to-end bundles from an in-memory project; outputs run under Node (via
//! `scripts/limit.sh`) when it is installed.

ts_r_testing::cap_memory!();

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use ts_r_bundler::{BuildContext, BuildOptions, BuildResult, EntryPoint, Format, Platform};
use ts_r_config::MemoryFs;

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn project(files: &[(&str, &str)]) -> MemoryFs {
    let mut fs = MemoryFs::new();
    for (p, c) in files {
        fs.add_file(&format!("/proj/{p}"), c);
    }
    fs
}

fn opts(entries: &[&str]) -> BuildOptions {
    BuildOptions {
        entry_points: entries
            .iter()
            .map(|e| EntryPoint {
                path: (*e).to_owned(),
                name: None,
            })
            .collect(),
        working_dir: "/proj".into(),
        bundle: true,
        outdir: Some("out".into()),
        write: false,
        threads: 2,
        platform: Platform::Node,
        format: Some(Format::Esm),
        ..BuildOptions::default()
    }
}

fn run_build(fs: &MemoryFs, o: BuildOptions) -> BuildResult {
    let ctx = BuildContext::new(o, Arc::new(fs.clone())).expect("options");
    let r = ctx.build();
    assert!(
        !r.has_errors(),
        "build errors:\n{}",
        r.messages
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    r
}

/// Writes the outputs to a scratch directory and runs `entry` (relative to `/proj/out`).
fn run_node(name: &str, r: &BuildResult, entry: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("bundler-e2e")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for o in &r.outputs {
        let rel = o
            .path
            .strip_prefix("/proj/out/")
            .expect("output under /proj/out");
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, &o.contents).expect("write");
    }
    let out = ts_r_testing::limited("node", 512, 30)
        .arg(dir.join(entry))
        .output()
        .expect("run node");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "node failed for {name}:\n{}\n--- output ---\n{}",
        String::from_utf8_lossy(&out.stderr),
        r.outputs
            .iter()
            .map(|o| format!("== {}\n{}", o.path, o.text()))
            .collect::<String>()
    );
    if std::env::var_os("TS_R_E2E_DUMP").is_some() {
        for o in &r.outputs {
            eprintln!("== {name}: {}\n{}", o.path, o.text());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    stdout
}

fn file<'a>(r: &'a BuildResult, path: &str) -> &'a str {
    r.outputs
        .iter()
        .find(|o| o.path == path)
        .map(|o| o.text())
        .unwrap_or_else(|| panic!("no output {path}"))
}

#[test]
fn nodenext_auto_keeps_script_top_level_this() {
    for (detection, expected) in [
        ("auto", "false\n"),
        ("force", "true\n"),
        ("legacy", "false\n"),
    ] {
        let config = format!(
            r#"{{"compilerOptions":{{"module":"nodenext","moduleDetection":"{detection}"}}}}"#
        );
        let fs = project(&[
            ("tsconfig.json", &config),
            ("package.json", "{}"),
            ("a.ts", "console.log(this === undefined);\n"),
        ]);
        let mut o = opts(&["a.ts"]);
        o.format = Some(Format::Cjs);
        let r = run_build(&fs, o);
        if have_node() {
            assert_eq!(
                run_node(&format!("nodenext-{detection}"), &r, "a.js"),
                expected
            );
        }
    }
}

#[test]
fn esm_scope_hoisting_and_deconflicting() {
    let fs = project(&[
        (
            "src/main.ts",
            "import { a, count } from './a';\nimport * as b from './b';\nconst x: number = 1;\nconsole.log(a(), b.x, x, count, b.default);\n",
        ),
        (
            "src/a.ts",
            "const x = 'a';\nexport let count = 0;\nexport function a() { count++; return x; }\n",
        ),
        ("src/b.ts", "export const x = 'b';\nexport default 42;\n"),
    ]);
    let r = run_build(&fs, opts(&["src/main.ts"]));
    let code = file(&r, "/proj/out/main.js");
    assert!(!code.contains("import "), "{code}");
    if have_node() {
        assert_eq!(run_node("esm", &r, "main.js"), "a b 1 1 42\n");
    }
}

#[test]
fn tree_shaking_drops_unused_exports_and_pure_calls() {
    let fs = project(&[
        (
            "src/main.js",
            "import { used } from './lib.js';\nconsole.log(used());\n",
        ),
        (
            "src/lib.js",
            "export function used() { return 'used'; }\nexport function unused() { return 'UNUSED_MARKER'; }\nexport const big = /*#__PURE__*/ makeBig();\nfunction makeBig() { return 'BIG_MARKER'; }\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    let code = file(&r, "/proj/out/main.js");
    assert!(!code.contains("UNUSED_MARKER"), "{code}");
    assert!(!code.contains("BIG_MARKER"), "{code}");
    if have_node() {
        assert_eq!(run_node("shake", &r, "main.js"), "used\n");
    }
}

#[test]
fn direct_eval_keeps_module_bindings() {
    // `eval('secret')` names `secret` only by string, so a module with direct eval keeps
    // all of its declarations, wrapped (`require`d) or hoisted (imported).
    let fs = project(&[
        ("src/main.cjs", "console.log(require('./dep.mjs').f());\n"),
        (
            "src/main2.mjs",
            "import { f } from './dep.mjs';\nconsole.log(f());\n",
        ),
        (
            "src/dep.mjs",
            "const secret = 42;\nexport function f() { return eval('secret'); }\n",
        ),
    ]);
    for entry in ["main.cjs", "main2.mjs"] {
        let mut o = opts(&[&format!("src/{entry}")]);
        o.format = Some(Format::Cjs);
        let r = run_build(&fs, o);
        let out = format!("/proj/out/{}.js", entry.split('.').next().unwrap_or(""));
        let code = file(&r, &out);
        assert!(code.contains("42"), "{code}");
        if have_node() {
            assert_eq!(
                run_node(
                    &format!("eval-{entry}"),
                    &r,
                    out.trim_start_matches("/proj/out/")
                ),
                "42\n"
            );
        }
    }
}

#[test]
fn side_effects_false_package_is_dropped() {
    let fs = project(&[
        (
            "src/main.js",
            "import 'pkg';\nimport { f } from 'pkg';\nconsole.log('ok');\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{"name":"pkg","sideEffects":false,"main":"index.js"}"#,
        ),
        (
            "node_modules/pkg/index.js",
            "console.log('SIDE_EFFECT');\nexport function f() {}\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    let code = file(&r, "/proj/out/main.js");
    assert!(!code.contains("SIDE_EFFECT"), "{code}");
}

#[test]
fn cjs_interop() {
    let fs = project(&[
        (
            "src/main.mjs",
            "import def, { named } from './c.cjs';\nimport * as ns from './c.cjs';\nconst e = require('./e.js');\nconsole.log(def.named, named, ns.named, e.value, e.default);\n",
        ),
        ("src/c.cjs", "exports.named = 'n';\n"),
        (
            "src/e.js",
            "export const value = 'v';\nexport default 'd';\n",
        ),
    ]);
    for format in [Format::Esm, Format::Cjs, Format::Iife] {
        let mut o = opts(&["src/main.mjs"]);
        o.format = Some(format);
        let r = run_build(&fs, o);
        if have_node() {
            assert_eq!(
                run_node(&format!("cjs-{}", format.as_str()), &r, "main.js"),
                "n n n v d\n",
                "{format:?}"
            );
        }
    }
}

#[test]
fn json_and_text_loaders() {
    let fs = project(&[
        (
            "src/main.js",
            "import data from './d.json';\nimport { version } from './d.json';\nimport txt from './t.txt';\nconsole.log(data.name, version, txt);\n",
        ),
        ("src/d.json", r#"{"name":"pkg","version":"1.2.3"}"#),
        ("src/t.txt", "hello"),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    if have_node() {
        assert_eq!(run_node("json", &r, "main.js"), "pkg 1.2.3 hello\n");
    }
}

#[test]
fn entry_exports_in_each_format() {
    let fs = project(&[
        (
            "src/lib.ts",
            "export const a = 1;\nexport default function f() { return 2; }\n",
        ),
        (
            "src/use.cjs",
            "const m = require('./lib.js');\nconsole.log(m.a, m.default());\n",
        ),
    ]);
    let mut o = opts(&["src/lib.ts"]);
    o.format = Some(Format::Cjs);
    let r = run_build(&fs, o);
    if have_node() {
        let mut r2 = BuildResult {
            outputs: r.outputs.clone(),
            ..BuildResult::default()
        };
        r2.outputs.push(ts_r_bundler::OutputFile {
            path: "/proj/out/use.cjs".into(),
            contents: b"const m = require('./lib.js');\nconsole.log(m.a, m.default());\n".to_vec(),
        });
        assert_eq!(run_node("exports-cjs", &r2, "use.cjs"), "1 2\n");
    }
    let mut o = opts(&["src/lib.ts"]);
    o.format = Some(Format::Iife);
    o.global_name = Some("Lib".into());
    o.footer = Some("console.log(Lib.a, Lib.default());".into());
    let r = run_build(&fs, o);
    if have_node() {
        assert_eq!(run_node("exports-iife", &r, "lib.js"), "1 2\n");
    }
}

#[test]
fn commonjs_star_exports_reach_the_entry_through_barrels() {
    for (name, index) in [
        (
            "direct",
            "export * from './dep.cjs';\nexport * from './barrel.js';\n",
        ),
        ("barrel", "export * from './barrel.js';\n"),
    ] {
        let fs = project(&[
            ("src/index.js", &format!("{index}export const own = 1;\n")),
            (
                "src/barrel.js",
                "export * from './dep.cjs';\nexport const bar = 'b';\n",
            ),
            (
                "src/dep.cjs",
                "exports.foo = 42;\nexports.bar = 'cjs';\nexports.own = 'cjs';\nexports.default = 'cjs';\n",
            ),
        ]);
        let mut o = opts(&["src/index.js"]);
        o.format = Some(Format::Cjs);
        let r = run_build(&fs, o);
        if have_node() {
            let mut r2 = BuildResult {
                outputs: r.outputs.clone(),
                ..BuildResult::default()
            };
            r2.outputs.push(ts_r_bundler::OutputFile {
                path: "/proj/out/use.cjs".into(),
                contents:
                    b"const m = require('./index.js');\nconsole.log(m.foo, m.bar, m.own, m.default);\n"
                        .to_vec(),
            });
            // Static exports win over the CommonJS module's names; `default` is never
            // re-exported.
            assert_eq!(
                run_node(&format!("cjs-star-{name}"), &r2, "use.cjs"),
                "42 b 1 undefined\n",
                "{name}"
            );
        }
    }
}

#[test]
fn star_export_diamonds_resolve_in_linear_time() {
    // Two modules per level, each `export *` from both modules of the level below: every
    // export name is reachable along 2^30 paths.
    const LEVELS: usize = 30;
    let mut files: Vec<(String, String)> =
        vec![("src/leaf.js".into(), "export const x = 'leaf';\n".into())];
    for i in 0..LEVELS {
        let below = if i == 0 {
            "export * from './leaf.js';\n".to_owned()
        } else {
            format!(
                "export * from './a{}.js';\nexport * from './b{}.js';\n",
                i - 1,
                i - 1
            )
        };
        files.push((format!("src/a{i}.js"), below.clone()));
        files.push((format!("src/b{i}.js"), below));
    }
    let top = LEVELS - 1;
    let main = format!(
        "import {{ x }} from './a{top}.js';\nimport * as ns from './b{top}.js';\nconsole.log(x, Object.keys(ns).join());\n"
    );
    let mut with_main = files.clone();
    with_main.push(("src/main.js".into(), main));
    let fs = project(
        &with_main
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    );
    let r = run_build(&fs, opts(&["src/main.js"]));
    if have_node() {
        assert_eq!(run_node("star-diamonds", &r, "main.js"), "leaf x\n");
    }

    // A missing name explores every path too.
    files.push((
        "src/main.js".into(),
        format!("import {{ nope }} from './a{top}.js';\nconsole.log(nope);\n"),
    ));
    let fs = project(
        &files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    );
    let ctx = BuildContext::new(opts(&["src/main.js"]), Arc::new(fs)).expect("options");
    let r = ctx.build();
    assert!(
        r.messages
            .iter()
            .any(|m| m.text.contains("No matching export") && m.text.contains("\"nope\"")),
        "{:?}",
        r.messages
    );
}

#[test]
fn code_splitting_shares_modules() {
    let fs = project(&[
        (
            "src/a.js",
            "import { shared } from './shared.js';\nconsole.log('a', shared);\nimport('./lazy.js').then(m => console.log(m.lazy));\n",
        ),
        (
            "src/b.js",
            "import { shared } from './shared.js';\nconsole.log('b', shared);\n",
        ),
        ("src/shared.js", "export const shared = 'S';\n"),
        (
            "src/lazy.js",
            "import { shared } from './shared.js';\nexport const lazy = 'L' + shared;\n",
        ),
    ]);
    let mut o = opts(&["src/a.js", "src/b.js"]);
    o.splitting = true;
    let r = run_build(&fs, o);
    let a = file(&r, "/proj/out/a.js");
    assert!(
        !a.contains("'S'") && !a.contains("\"S\""),
        "shared code duplicated:\n{a}"
    );
    if have_node() {
        let mut r2 = BuildResult {
            outputs: r.outputs.clone(),
            ..BuildResult::default()
        };
        r2.outputs.push(ts_r_bundler::OutputFile {
            path: "/proj/out/package.json".into(),
            contents: br#"{"type":"module"}"#.to_vec(),
        });
        assert_eq!(run_node("split-a", &r2, "a.js"), "a S\nLS\n");
        assert_eq!(run_node("split-b", &r2, "b.js"), "b S\n");
    }
}

#[test]
fn minified_splitting_shares_commonjs() {
    let fs = project(&[
        (
            "src/a.js",
            "import counter from './counter.cjs';\nimport { label } from './label.js';\ncounter.bump();\nconsole.log(label('a'), counter.value);\nimport('./lazy.js').then(m => console.log(m.default()));\n",
        ),
        (
            "src/b.js",
            "import counter from './counter.cjs';\nimport { label } from './label.js';\nconsole.log(label('b'), counter.value);\n",
        ),
        (
            "src/counter.cjs",
            "exports.value = 0;\nexports.bump = function () { exports.value++; };\n",
        ),
        (
            "src/label.js",
            "export function label(longArgumentName) { return '[' + longArgumentName + ']'; }\nexport function unusedHelper() { return 'dropped'; }\n",
        ),
        (
            "src/lazy.js",
            "import counter from './counter.cjs';\nexport default () => 'lazy ' + counter.value;\n",
        ),
    ]);
    let mut o = opts(&["src/a.js", "src/b.js"]);
    o.splitting = true;
    o.minify_whitespace = true;
    o.minify_identifiers = true;
    o.minify_syntax = true;
    let r = run_build(&fs, o);
    let all: String = r.outputs.iter().map(|o| o.text()).collect();
    assert!(!all.contains("longArgumentName"), "{all}");
    assert!(!all.contains("dropped"), "{all}");
    assert_eq!(all.matches(".bump=function").count(), 1, "{all}");
    if have_node() {
        let mut r2 = BuildResult {
            outputs: r.outputs.clone(),
            ..BuildResult::default()
        };
        r2.outputs.push(ts_r_bundler::OutputFile {
            path: "/proj/out/package.json".into(),
            contents: br#"{"type":"module"}"#.to_vec(),
        });
        assert_eq!(run_node("split-min-a", &r2, "a.js"), "[a] 1\nlazy 1\n");
        assert_eq!(run_node("split-min-b", &r2, "b.js"), "[b] 0\n");
    }
}

#[test]
fn minified_output_runs() {
    let fs = project(&[
        (
            "src/main.ts",
            "import { sum } from './m';\nfunction outer(longParameterName: number) { const inner = longParameterName * 2; return inner; }\nif (process.env.NODE_ENV === 'production') { console.log('prod'); } else { console.log('dev'); }\nconsole.log(sum([1, 2, 3]), outer(4));\n",
        ),
        (
            "src/m.ts",
            "export function sum(xs: number[]) { let total = 0; for (const x of xs) total += x; return total; }\n",
        ),
    ]);
    let mut o = opts(&["src/main.ts"]);
    o.minify_whitespace = true;
    o.minify_identifiers = true;
    o.minify_syntax = true;
    o.define = vec![("process.env.NODE_ENV".into(), "\"production\"".into())];
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.js");
    assert!(!code.contains("longParameterName"), "{code}");
    assert!(!code.contains("dev"), "{code}");
    if have_node() {
        assert_eq!(run_node("minify", &r, "main.js"), "prod\n6 8\n");
    }
}

#[test]
fn rebuild_reuses_fragments() {
    let mut fs = project(&[
        (
            "src/main.js",
            "import { a } from './a.js';\nimport { b } from './b.js';\nconsole.log(a, b);\n",
        ),
        ("src/a.js", "export const a = 1;\n"),
        ("src/b.js", "export const b = 2;\n"),
    ]);
    let ctx = BuildContext::new(opts(&["src/main.js"]), Arc::new(fs.clone())).expect("options");
    let r1 = ctx.build();
    assert!(!r1.has_errors());
    assert_eq!(r1.stats.fragments_printed, 3);
    let r2 = ctx.build();
    assert_eq!(r2.stats.fragments_printed, 0);
    assert_eq!(r1.outputs, r2.outputs);
    // A changed file is re-read through a new context over the updated file system.
    fs.add_file("/proj/src/b.js", "export const b = 3;\n");
    let ctx2 = BuildContext::new(opts(&["src/main.js"]), Arc::new(fs)).expect("options");
    let r3 = ctx2.build();
    assert!(file(&r3, "/proj/out/main.js").contains('3'));
}

/// A file system that can change under a live build context.
struct SharedFs(std::sync::RwLock<MemoryFs>);

impl ts_r_config::FileSystem for SharedFs {
    fn read_file(&self, p: &str) -> Option<String> {
        self.0.read().expect("lock").read_file(p)
    }
    fn file_exists(&self, p: &str) -> bool {
        self.0.read().expect("lock").file_exists(p)
    }
    fn directory_exists(&self, p: &str) -> bool {
        self.0.read().expect("lock").directory_exists(p)
    }
    fn read_dir(&self, p: &str) -> Vec<ts_r_config::fs::DirEntry> {
        self.0.read().expect("lock").read_dir(p)
    }
    fn realpath(&self, p: &str) -> String {
        self.0.read().expect("lock").realpath(p)
    }
    fn case_sensitive(&self) -> bool {
        true
    }
}

#[test]
fn invalidate_rereads_only_changed_files() {
    let fs = Arc::new(SharedFs(std::sync::RwLock::new(project(&[
        (
            "src/main.js",
            "import { a } from './a.js';\nimport { b } from './b.js';\nconsole.log(a, b);\n",
        ),
        ("src/a.js", "export const a = 1;\n"),
        ("src/b.js", "export const b = 2;\n"),
    ]))));
    let ctx = BuildContext::new(opts(&["src/main.js"]), fs.clone()).expect("options");
    assert_eq!(ctx.build().stats.parsed, 3);
    let edit = |p: &str, c: &str| {
        fs.0.write().expect("lock").add_file(p, c);
    };
    // A content edit: only that file is read and parsed again.
    edit("/proj/src/b.js", "export const b = 3;\n");
    ctx.invalidate(&["src/b.js".to_owned()]);
    let r = ctx.build();
    assert_eq!(r.stats.parsed, 1);
    assert!(
        file(&r, "/proj/out/main.js").contains("b = 3"),
        "{}",
        file(&r, "/proj/out/main.js")
    );
    // A new file is structural: resolution starts over and finds it.
    edit("/proj/src/c.js", "export const c = 'new';\n");
    edit(
        "/proj/src/main.js",
        "import { a } from './a.js';\nimport { c } from './c.js';\nconsole.log(a, c);\n",
    );
    ctx.invalidate(&["/proj/src/c.js".to_owned(), "/proj/src/main.js".to_owned()]);
    let r = ctx.build();
    assert!(!r.has_errors(), "{:?}", r.messages);
    assert!(file(&r, "/proj/out/main.js").contains("'new'"));
}

#[test]
fn invalidate_rereads_changed_tsconfig_options() {
    let fs = Arc::new(SharedFs(std::sync::RwLock::new(project(&[
        ("src/main.tsx", "export const v = <div />;\n"),
        (
            "tsconfig.json",
            r#"{"extends": "./tsconfig.base.json", "compilerOptions": {"jsx": "react"}}"#,
        ),
        (
            "tsconfig.base.json",
            r#"{"compilerOptions": {"jsxFactory": "h"}}"#,
        ),
    ]))));
    let ctx = BuildContext::new(opts(&["src/main.tsx"]), fs.clone()).expect("options");
    let out = |ctx: &BuildContext| file(&ctx.build(), "/proj/out/main.js").to_owned();
    assert!(out(&ctx).contains("h(\"div\""), "{}", out(&ctx));
    let edit = |p: &str, c: &str| {
        fs.0.write().expect("lock").add_file(p, c);
    };
    // The config the tsconfig extends changes.
    edit(
        "/proj/tsconfig.base.json",
        r#"{"compilerOptions": {"jsxFactory": "g"}}"#,
    );
    ctx.invalidate(&["tsconfig.base.json".to_owned()]);
    assert!(out(&ctx).contains("g(\"div\""), "{}", out(&ctx));
    // The tsconfig itself changes.
    edit(
        "/proj/tsconfig.json",
        r#"{"extends": "./tsconfig.base.json", "compilerOptions": {"jsx": "react", "jsxFactory": "k"}}"#,
    );
    ctx.invalidate(&["tsconfig.json".to_owned()]);
    assert!(out(&ctx).contains("k(\"div\""), "{}", out(&ctx));
}

#[test]
fn sourcemap_and_metafile() {
    let fs = project(&[
        ("src/main.ts", "import { a } from './a';\nconsole.log(a);\n"),
        ("src/a.ts", "export const a: string = 'x';\n"),
    ]);
    let mut o = opts(&["src/main.ts"]);
    o.sourcemap = ts_r_bundler::SourceMapMode::Linked;
    o.metafile = true;
    let r = run_build(&fs, o);
    let map = file(&r, "/proj/out/main.js.map");
    assert!(map.contains("\"../src/a.ts\""), "{map}");
    assert!(file(&r, "/proj/out/main.js").ends_with("//# sourceMappingURL=main.js.map\n"));
    let meta = r.metafile.as_deref().expect("metafile");
    assert!(meta.contains("\"src/a.ts\""), "{meta}");
    assert!(meta.contains("\"out/main.js\""), "{meta}");
    assert!(meta.contains("\"entryPoint\": \"src/main.ts\""), "{meta}");
}

/// Windows paths (`C:/...`) are absolute too: they are build inputs (guard errors are looked
/// up by them) and print relative in source maps and the metafile.
#[test]
fn drive_letter_paths_are_inputs_and_print_relative() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "C:/proj/src/main.ts",
        "import { a } from './a';\nconsole.log(a);\n",
    );
    fs.add_file("C:/proj/src/a.ts", "export const a: string = 'x';\n");
    let mut o = opts(&["src/main.ts"]);
    o.working_dir = "C:/proj".into();
    o.sourcemap = ts_r_bundler::SourceMapMode::Linked;
    o.metafile = true;
    let r = run_build(&fs, o);
    assert_eq!(r.inputs, ["C:/proj/src/main.ts", "C:/proj/src/a.ts"]);
    let map = file(&r, "C:/proj/out/main.js.map");
    assert!(map.contains("\"../src/a.ts\""), "{map}");
    let meta = r.metafile.as_deref().expect("metafile");
    assert!(meta.contains("\"src/a.ts\""), "{meta}");
    assert!(!meta.contains("C:/"), "{meta}");
}

#[test]
fn minified_fragments_share_a_line_and_keep_their_mappings() {
    let fs = project(&[
        (
            "src/main.ts",
            "import { b } from './b';\nconsole.log(b());\n",
        ),
        ("src/b.ts", "export const b = () => 'from-b';\n"),
    ]);
    let mut o = opts(&["src/main.ts"]);
    o.sourcemap = ts_r_bundler::SourceMapMode::Linked;
    minify_all(&mut o);
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.js");
    let map = file(&r, "/proj/out/main.js.map");
    // One line of code, then the source-map comment on its own line.
    let first = code.lines().next().unwrap();
    assert_eq!(code.lines().count(), 2, "{code}");
    let call = first.find("console.log(").expect("call") as u32;
    assert!(first.find("from-b").unwrap() < call as usize, "{code}");
    let mappings = map
        .split("\"mappings\": \"")
        .nth(1)
        .and_then(|m| m.split('"').next())
        .unwrap();
    let main = if map.find("\"../src/main.ts\"") < map.find("\"../src/b.ts\"") {
        0
    } else {
        1
    };
    let segs = ts_r_codegen::sourcemap::decode_mappings(mappings);
    // `console.log(b())` starts line 2, column 0 of main.ts.
    assert!(
        segs.iter().any(|m| m.gen_line == 0
            && m.gen_col == call
            && m.source.map(|s| (s.0, s.1, s.2)) == Some((main, 1, 0))),
        "{code}\n{map}"
    );
}

#[test]
fn missing_import_is_an_error() {
    let fs = project(&[("src/main.js", "import './nope.js';\n")]);
    let ctx = BuildContext::new(opts(&["src/main.js"]), Arc::new(fs)).expect("options");
    let r = ctx.build();
    assert!(r.has_errors());
    assert!(
        r.messages[0].text.contains("Could not resolve"),
        "{}",
        r.messages[0]
    );
}

#[test]
fn dynamic_import_of_top_level_await_waits_through_cycles() {
    // The wrappers of `main`, `a` and `b` become async because `dep` awaits; the cycle
    // between `a` and `b` must not deadlock (esbuild issue 4498).
    let fs = project(&[
        (
            "src/entry.mjs",
            "await import('./main.mjs');\nconsole.log('done');\n",
        ),
        (
            "src/main.mjs",
            "import { a } from './a.mjs';\nconsole.log(a());\n",
        ),
        (
            "src/a.mjs",
            "import { b } from './b.mjs';\nimport { tla } from './dep.mjs';\nexport function a() { return b() + tla; }\n",
        ),
        (
            "src/b.mjs",
            "import { a } from './a.mjs';\nexport function b() { return typeof a; }\n",
        ),
        (
            "src/dep.mjs",
            "export const tla = await Promise.resolve('x');\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["src/entry.mjs"]));
    let code = file(&r, "/proj/out/entry.js");
    assert!(code.contains("async () =>"), "{code}");
    if have_node() {
        assert_eq!(run_node("tla-cycle", &r, "entry.js"), "functionx\ndone\n");
    }
}

#[test]
fn require_of_a_transitive_top_level_await_is_an_error() {
    let fs = project(&[
        ("src/entry.js", "require('./a');\n"),
        ("src/a.js", "import './b';\n"),
        ("src/b.js", "await 0;\n"),
    ]);
    let ctx = BuildContext::new(opts(&["src/entry.js"]), Arc::new(fs)).expect("options");
    let r = ctx.build();
    assert!(r.has_errors());
    assert!(
        r.messages
            .iter()
            .any(|m| m.text.contains("transitive dependency") && m.text.contains("b.js")),
        "{:?}",
        r.messages
    );
}

#[test]
fn imports_of_commonjs_run_in_order_between_inlined_modules() {
    // `cjs.js` runs before `esm.js`, and `late.js` after it: the entry's `require_x()` calls
    // go between the inlined modules, as an ES module evaluates its imports in order.
    let fs = project(&[
        (
            "src/entry.js",
            "import './cjs.js';\nimport './esm.js';\nimport './late.js';\nconsole.log('entry');\n",
        ),
        ("src/cjs.js", "console.log('cjs', typeof module);\n"),
        ("src/esm.js", "export const x = 1;\nconsole.log('esm');\n"),
        (
            "src/late.js",
            "console.log('late', this === module.exports);\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["src/entry.js"]));
    let code = file(&r, "/proj/out/entry.js");
    let (cjs, esm, late) = (
        code.find("require_cjs();").expect("cjs call"),
        code.find("// src/esm.js").expect("esm code"),
        code.find("require_late();").expect("late call"),
    );
    assert!(cjs < esm && esm < late, "{code}");
    if have_node() {
        assert_eq!(
            run_node("import-order", &r, "entry.js"),
            "cjs object\nesm\nlate true\nentry\n"
        );
    }
}

#[test]
fn top_level_this_is_exports_unless_the_file_exports_or_uses_import_meta() {
    let fs = project(&[
        (
            "src/entry.js",
            "import './imports.js';\nimport './exports.js';\nimport './meta.js';\n",
        ),
        ("src/dummy.js", "export const d = 1;\n"),
        (
            "src/imports.js",
            "import './dummy.js';\nconsole.log(typeof this);\n",
        ),
        (
            "src/exports.js",
            "export const e = 1;\nconsole.log(typeof this);\n",
        ),
        ("src/meta.js", "import.meta;\nconsole.log(typeof this);\n"),
    ]);
    let r = run_build(&fs, opts(&["src/entry.js"]));
    if have_node() {
        assert_eq!(
            run_node("this-es6", &r, "entry.js"),
            "object\nundefined\nundefined\n"
        );
    } else {
        let code = file(&r, "/proj/out/entry.js");
        assert!(code.contains("require_imports"), "{code}");
    }
}

#[test]
fn side_effects_false_keeps_effects_of_used_files() {
    let fs = project(&[
        (
            "src/main.js",
            "import { foo } from 'pkg';\nconsole.log(foo);\n",
        ),
        ("node_modules/pkg/package.json", r#"{"sideEffects":false}"#),
        (
            "node_modules/pkg/index.js",
            "export { foo } from './foo.js';\nconsole.log('barrel');\n",
        ),
        (
            "node_modules/pkg/foo.js",
            "export const foo = 123;\nconsole.log('foo');\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    if have_node() {
        assert_eq!(run_node("se-keep", &r, "main.js"), "foo\nbarrel\n123\n");
    }
}

#[test]
fn ambiguous_star_exports() {
    let fs = project(&[
        (
            "src/main.js",
            "import * as ns from './common.js';\nconsole.log(ns.x, ns.y, ns.z);\n",
        ),
        (
            "src/common.js",
            "export * from './a.js';\nexport * from './b.js';\n",
        ),
        ("src/a.js", "export const x = 1, y = 2;\n"),
        ("src/b.js", "export const y = 3, z = 4;\n"),
        (
            "src/bad.js",
            "import { y } from './common.js';\nconsole.log(y);\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    if have_node() {
        assert_eq!(run_node("ambiguous", &r, "main.js"), "1 undefined 4\n");
    }
    let ctx = BuildContext::new(opts(&["src/bad.js"]), Arc::new(fs)).expect("options");
    let r = ctx.build();
    assert!(
        r.errors()
            .any(|m| m.text.contains("Ambiguous import \"y\""))
    );
}

/// A static export wins over a name that might come from CommonJS through a nested `export *`
/// (langium's index, bundled by the Excalidraw app): not ambiguous.
#[test]
fn static_star_export_beats_nested_commonjs_star() {
    let fs = project(&[
        (
            "src/main.js",
            "import { inject, on } from './index.js';\nconsole.log(typeof inject, typeof on);\n",
        ),
        (
            "src/index.js",
            "export * from './di.js';\nexport * from './utils/index.js';\n",
        ),
        ("src/di.js", "export function inject() {}\n"),
        ("src/utils/index.js", "export * from './event.js';\n"),
        ("src/utils/event.js", "export * from './events.cjs';\n"),
        ("src/utils/events.cjs", "exports.on = function () {};\n"),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    if have_node() {
        assert_eq!(run_node("star-maybe", &r, "main.js"), "function function\n");
    }
}

/// A name no static branch exports, reachable through two `export *` chains that each end in
/// CommonJS: either may supply it at run time, so it is ambiguous (as before nested CommonJS
/// stars became fallbacks), never silently read from the first chain. A single chain, or a
/// static match, still resolves (`static_star_export_beats_nested_commonjs_star`).
#[test]
fn name_from_one_of_several_nested_commonjs_stars() {
    let fs = project(&[
        (
            "src/main.js",
            "import { foo } from './index.js';\nconsole.log(foo);\n",
        ),
        (
            "src/index.js",
            "export * from './a.js';\nexport * from './b.js';\n",
        ),
        ("src/a.js", "export * from './a.cjs';\n"),
        ("src/b.js", "export * from './b.cjs';\n"),
        ("src/a.cjs", "exports.bar = 1;\n"),
        ("src/b.cjs", "exports.foo = 42;\n"),
    ]);
    let ctx = BuildContext::new(opts(&["src/main.js"]), Arc::new(fs)).expect("options");
    let r = ctx.build();
    assert!(
        r.errors()
            .any(|m| m.text.contains("Ambiguous import \"foo\"")),
        "{:?}",
        r.errors().map(|m| &m.text).collect::<Vec<_>>()
    );
}

#[test]
fn required_package_uses_main_everywhere() {
    let fs = project(&[
        (
            "src/main.js",
            "import a from 'pkg';\nconst b = require('pkg');\nconsole.log(a, b);\n",
        ),
        (
            "node_modules/pkg/package.json",
            r#"{"main":"main.js","module":"module.js"}"#,
        ),
        ("node_modules/pkg/main.js", "module.exports = 'main';\n"),
        ("node_modules/pkg/module.js", "export default 'module';\n"),
    ]);
    let r = run_build(&fs, opts(&["src/main.js"]));
    if have_node() {
        assert_eq!(run_node("dual", &r, "main.js"), "main main\n");
    }
}

#[test]
fn transform_only_converts_to_commonjs() {
    let fs = project(&[(
        "src/lib.ts",
        "export const a: number = 1;\nexport default function f() { return 2; }\n",
    )]);
    let mut o = opts(&["src/lib.ts"]);
    o.bundle = false;
    o.format = Some(Format::Cjs);
    o.out_extension_js = ".cjs".into();
    let r = run_build(&fs, o);
    if have_node() {
        let mut r2 = BuildResult {
            outputs: r.outputs.clone(),
            ..BuildResult::default()
        };
        r2.outputs.push(ts_r_bundler::OutputFile {
            path: "/proj/out/use.cjs".into(),
            contents: b"const m = require('./lib.cjs');\nconsole.log(m.a, m.default());\n".to_vec(),
        });
        assert_eq!(run_node("transform-cjs", &r2, "use.cjs"), "1 2\n");
    }
}

fn minify_all(o: &mut BuildOptions) {
    o.minify_whitespace = true;
    o.minify_identifiers = true;
    o.minify_syntax = true;
}

/// Ecosystem (chalk, commander, ...): a minified default import of an external module printed
/// as `import ofrom"node:os"`.
#[test]
fn minified_external_default_import_keeps_space_before_from() {
    let fs = project(&[(
        "src/main.mjs",
        "import os from 'node:os';\nimport fs, { readFileSync } from 'node:fs';\nimport * as path from 'node:path';\nconsole.log(typeof os.EOL, readFileSync === fs.readFileSync, typeof path.join);\n",
    )]);
    let mut o = opts(&["src/main.mjs"]);
    minify_all(&mut o);
    o.out_extension_js = ".mjs".into();
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.mjs");
    assert!(code.contains(" from\"node:os\""), "{code}");
    if have_node() {
        assert_eq!(
            run_node("min-ext-default", &r, "main.mjs"),
            "string true function\n"
        );
    }
}

/// Ecosystem (readable-stream, whatwg-url): `require('process/')` is the npm package, not
/// the built-in.
#[test]
fn trailing_slash_require_bundles_the_package() {
    let fs = project(&[
        (
            "src/main.js",
            "console.log(JSON.stringify(require('process/')), typeof require('fs/promises').readFile);\n",
        ),
        (
            "node_modules/process/package.json",
            r#"{ "name": "process", "main": "./index.js" }"#,
        ),
        (
            "node_modules/process/index.js",
            "module.exports = { shim: true };\n",
        ),
    ]);
    let mut o = opts(&["src/main.js"]);
    o.format = Some(Format::Cjs);
    o.out_extension_js = ".cjs".into();
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.cjs");
    assert!(!code.contains("require(\"process/\")"), "{code}");
    if have_node() {
        assert_eq!(
            run_node("trailing-slash", &r, "main.cjs"),
            "{\"shim\":true} function\n"
        );
    }
}

/// Ecosystem (vitest/internal/traces): `import('x').then(f).catch(g)` handles a missing
/// module at run time, so it is a warning (as inside `try`), not a build error.
#[test]
fn dynamic_import_with_catch_handler_may_be_missing() {
    let fs = project(&[(
        "src/main.mjs",
        "import('nope-a').then((m) => m.x).catch(() => console.log('a caught'));\nimport('nope-b').then(() => {}, () => console.log('b caught'));\n",
    )]);
    let mut o = opts(&["src/main.mjs"]);
    o.out_extension_js = ".mjs".into();
    let r = run_build(&fs, o);
    let warnings = r.messages.iter().filter(|m| !m.is_error()).count();
    assert_eq!(warnings, 2, "{:?}", r.messages);
    if have_node() {
        let out = run_node("guarded-import", &r, "main.mjs");
        assert!(
            out.contains("a caught") && out.contains("b caught"),
            "{out}"
        );
    }
    let fs = project(&[("src/main.mjs", "import('nope').then((m) => m.x);\n")]);
    let ctx = BuildContext::new(opts(&["src/main.mjs"]), Arc::new(fs)).expect("options");
    assert!(ctx.build().has_errors());
}

/// Each handling call of a promise chain used to walk the whole chain below it again:
/// 100 000 `.catch(f)` calls took billions of steps. The chain is now walked once.
#[test]
fn long_promise_chain_scans_in_linear_time() {
    let src = format!(
        "const f = () => {{}};\nimport('nope'){};\n",
        ".catch(f)".repeat(100_000)
    );
    let fs = project(&[("src/main.mjs", &src)]);
    let start = std::time::Instant::now();
    let r = run_build(&fs, opts(&["src/main.mjs"]));
    let elapsed = start.elapsed();
    let warnings = r.messages.iter().filter(|m| !m.is_error()).count();
    assert_eq!(warnings, 1, "{:?}", r.messages);
    assert!(elapsed.as_secs() < 30, "took {elapsed:?}");
}

/// Ecosystem (https-proxy-agent, fs-extra/esm with the usual createRequire banner): the
/// banner's `const require` and our `require` shim collided.
#[test]
fn require_banner_replaces_the_require_shim() {
    let fs = project(&[
        (
            "src/main.mjs",
            "import dep from './dep.cjs';\nconsole.log(dep);\n",
        ),
        (
            "src/dep.cjs",
            "module.exports = require('node:path').sep;\n",
        ),
    ]);
    let mut o = opts(&["src/main.mjs"]);
    o.out_extension_js = ".mjs".into();
    o.banner = Some(
        "import { createRequire } from 'module'; const require = createRequire(import.meta.url);"
            .into(),
    );
    let r = run_build(&fs, o.clone());
    assert!(!file(&r, "/proj/out/main.mjs").contains("var require ="));
    if have_node() {
        // `path.sep`: `\` on Windows.
        assert_eq!(
            run_node("require-banner", &r, "main.mjs"),
            format!("{}\n", std::path::MAIN_SEPARATOR)
        );
    }
    o.banner = Some("// no require here".into());
    let r = run_build(&fs, o);
    assert!(file(&r, "/proj/out/main.mjs").contains("var require ="));
    if have_node() {
        assert_eq!(
            run_node("require-shim", &r, "main.mjs"),
            format!("{}\n", std::path::MAIN_SEPARATOR)
        );
    }
}

/// A CommonJS entry in CommonJS output runs at the top of the file with Node's own `module`
/// (as esbuild prints it) when nothing can tell the difference; otherwise it keeps its
/// `require_x` wrapper.
#[test]
fn commonjs_entry_runs_unwrapped_in_commonjs_output() {
    let dep_sloppy = ("src/sloppy.js", "leaked = 5;\nexports.v = typeof leaked;\n");
    let dep_strict = ("src/strict.js", "'use strict';\nexports.v = 'strict';\n");
    /// (name, files, runs unwrapped, Node output, output prefix)
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, bool, &'a str, &'a str);
    let cases: [Case<'_>; 9] = [
        (
            "top-level-arguments",
            vec![(
                "src/main.js",
                "exports.n = arguments.length;\nconsole.log(exports.n, (() => arguments.length)());\n",
            )],
            false,
            "2 2\n",
            "",
        ),
        (
            "strict-commonjs",
            vec![
                (
                    "src/main.js",
                    "'use strict';\nmodule.exports = require('./strict').v;\nconsole.log(module.exports, (function () { return this; })());\n",
                ),
                dep_strict,
            ],
            true,
            "strict undefined\n",
            "\"use strict\";",
        ),
        (
            "plain",
            vec![
                (
                    "src/main.js",
                    "const dep = require('./sloppy');\nconst x = 1;\nmodule.exports = { x, v: dep.v };\nconsole.log(JSON.stringify(module.exports), require.main === module);\n",
                ),
                dep_sloppy,
            ],
            true,
            "{\"x\":1,\"v\":\"number\"} true\n",
            "",
        ),
        (
            "strict",
            vec![
                (
                    "src/main.js",
                    "'use strict';\nconsole.log(require('./strict').v, (function () { return this; })());\n",
                ),
                dep_strict,
            ],
            true,
            "strict undefined\n",
            "\"use strict\";",
        ),
        (
            // A file-wide "use strict" would break the sloppy dependency.
            "strict-entry-sloppy-dep",
            vec![
                (
                    "src/main.js",
                    "'use strict';\nmodule.exports = require('./sloppy').v;\nconsole.log(module.exports);\n",
                ),
                dep_sloppy,
            ],
            false,
            "number\n",
            "",
        ),
        (
            "cycle",
            vec![
                (
                    "src/main.js",
                    "exports.a = 1;\nconsole.log(require('./back').seen);\n",
                ),
                ("src/back.js", "exports.seen = require('./main').a;\n"),
            ],
            false,
            "1\n",
            "",
        ),
        (
            "top-level-return",
            vec![
                (
                    "src/main.js",
                    "exports.a = 1;\nconsole.log('a');\nif (require('./strict').v) return;\nconsole.log('b');\n",
                ),
                dep_strict,
            ],
            false,
            "a\n",
            "",
        ),
        (
            // The entry's own top-level names stay clear of the ES module's.
            "names",
            vec![
                (
                    "src/main.js",
                    "const lib = require('./lib.mjs');\nfunction one() { return 1; }\nconst two = one() + 1, three = two + 1;\nlet four = three + 1;\nconsole.log(lib.sum(), one(), two, three, four);\n",
                ),
                (
                    "src/lib.mjs",
                    "const a = 10, b = 20;\nfunction add(x, y) { return x + y; }\nexport function sum() { return add(a, b); }\n",
                ),
            ],
            true,
            "30 1 2 3 4\n",
            "",
        ),
        (
            // Unmangled `exports` references would cost more than the wrapper saves.
            "many-exports",
            vec![(
                "src/main.js",
                "exports.a = 1; exports.b = 2; exports.c = 3; exports.d = 4; exports.e = 5;\nexports.f = 6; exports.g = 7; exports.h = 8; exports.i = 9; exports.j = 10;\nexports.k = 11; exports.l = 12; exports.m = 13; exports.n = 14; exports.o = 15;\nexports.p = 16; exports.q = 17; exports.r = 18; exports.s = 19; exports.t = 20;\nconsole.log(Object.keys(exports).length);\n",
            )],
            false,
            "20\n",
            "",
        ),
    ];
    for (name, files, unwrapped, want, prefix) in cases {
        let fs = project(&files);
        let mut o = opts(&["src/main.js"]);
        o.format = Some(Format::Cjs);
        minify_all(&mut o);
        let r = run_build(&fs, o);
        let code = file(&r, "/proj/out/main.js");
        assert_eq!(
            !code.trim_end().ends_with("();"),
            unwrapped,
            "{name}: {code}"
        );
        assert!(code.starts_with(prefix), "{name}: {code}");
        if have_node() {
            assert_eq!(
                run_node(&format!("cjs-entry-{name}"), &r, "main.js"),
                want,
                "{name}"
            );
        }
    }
}

/// Minified CommonJS wrappers mangle their `exports` / `module` parameters like other
/// locals and drop unused trailing ones; redeclarations (`var exports`) stay the parameter.
#[test]
fn minified_commonjs_wrapper_parameters_are_mangled() {
    let fs = project(&[
        (
            "src/main.cjs",
            "const a = require('./a.cjs'), b = require('./b.cjs'), c = require('./c.cjs'), d = require('./d.cjs');\nconsole.log(a.x, b.y, typeof c, d.z, require('./e.cjs') === undefined);\n",
        ),
        (
            "src/a.cjs",
            "exports.x = 1;\nfunction f() { return typeof module + typeof exports; }\nexports.f = f;\n",
        ),
        ("src/b.cjs", "module.exports = { y: 2 };\n"),
        (
            "src/c.cjs",
            "var exports = module.exports = function () {};\nexports.k = 3;\n",
        ),
        ("src/d.cjs", "this.z = 4;\n"),
        ("src/e.cjs", "console.log(arguments.length);\n"),
    ]);
    let mut o = opts(&["src/main.cjs"]);
    o.format = Some(Format::Cjs);
    o.out_extension_js = ".cjs".into();
    minify_all(&mut o);
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.cjs");
    assert!(!code.contains("module.exports={y"), "{code}");
    assert!(!code.contains("function(exports"), "{code}");
    // Wrappers are arrows unless the module uses its own `arguments` (e.cjs); top-level
    // `this` (d.cjs) is `exports`.
    assert!(code.matches("=>{").count() >= 4, "{code}");
    assert_eq!(code.matches("(function(").count(), 1, "{code}");
    if have_node() {
        assert_eq!(
            run_node("min-cjs-params", &r, "main.cjs"),
            "2\n1 2 function 4 false\n"
        );
    }
}

/// `var arguments;` at the top of a CommonJS module keeps the wrapper's arguments object in
/// a function wrapper; an arrow would make it a fresh `undefined` local.
#[test]
fn commonjs_var_arguments_keeps_a_function_wrapper() {
    let fs = project(&[
        (
            "src/main.js",
            "import { n } from './dep.cjs';\nconsole.log(n);\n",
        ),
        (
            "src/dep.cjs",
            "var arguments;\nexports.n = arguments.length;\n",
        ),
    ]);
    let mut o = opts(&["src/main.js"]);
    o.format = Some(Format::Iife);
    o.minify_syntax = true;
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.js");
    assert!(code.contains("__tsrCjs(function"), "{code}");
    if have_node() {
        assert_eq!(run_node("cjs-var-arguments", &r, "main.js"), "2\n");
    }
}

/// Ecosystem (@aws-sdk/*, @smithy/*): ES modules wrapped in `init_x()` (because something
/// `require()`s them) are tree-shaken like the others: an unused re-export of a
/// `sideEffects: false` package does not pull its module in.
#[test]
fn wrapped_esm_is_tree_shaken() {
    let fs = project(&[
        ("src/main.cjs", "console.log(require('pkg').f());\n"),
        (
            "node_modules/pkg/package.json",
            r#"{ "name": "pkg", "type": "module", "sideEffects": false }"#,
        ),
        ("node_modules/pkg/index.js", "export { f } from './f.js';\n"),
        (
            "node_modules/pkg/f.js",
            "import { a } from 'other';\nexport const f = () => a;\nexport const unusedExport = 'unused export';\n",
        ),
        (
            "node_modules/other/package.json",
            r#"{ "name": "other", "type": "module", "sideEffects": false }"#,
        ),
        (
            "node_modules/other/index.js",
            "export { a } from './a.js';\nexport { b } from './b.js';\n",
        ),
        ("node_modules/other/a.js", "export const a = 1;\n"),
        (
            "node_modules/other/b.js",
            "export const b = 2;\nconsole.log('b ran');\n",
        ),
    ]);
    let mut o = opts(&["src/main.cjs"]);
    o.format = Some(Format::Cjs);
    o.out_extension_js = ".cjs".into();
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/main.cjs");
    assert!(!code.contains("b ran"), "{code}");
    assert!(!code.contains("unused export"), "{code}");
    if have_node() {
        assert_eq!(run_node("wrapped-esm-shaken", &r, "main.cjs"), "1\n");
    }
}

#[test]
fn tsc_grammar_errors_esbuild_accepts_do_not_fail_the_build() {
    // tsc's checker grammar errors that esbuild accepts (TS1313, TS1183, TS18030, TS1189 in
    // sloppy code, TS1540) are not build errors; ECMAScript early errors still are.
    let fs = project(&[
        (
            "a.js",
            "if (globalThis.x) ;\nfor (var k = 1 in {});\n\
             class C { #x = 1; static f(o) { return o?.#x } }\nconsole.log(C.f(new C()), k);\n",
        ),
        (
            "b.ts",
            "declare function f() { return 1 }\nnamespace N {}\nmodule M { export const y = 2 }\n\
             import './a.js';\nconsole.log(M.y);\n",
        ),
        ("bad.js", "const z;\nconsole.log(z);\n"),
    ]);
    let r = run_build(&fs, opts(&["b.ts"]));
    if have_node() {
        assert_eq!(run_node("grammar-accepted", &r, "b.js"), "1 1\n2\n");
    }
    let ctx = BuildContext::new(opts(&["bad.js"]), Arc::new(fs.clone())).expect("options");
    let r = ctx.build();
    assert!(r.has_errors());
    assert!(
        r.messages[0].text.contains("must be initialized"),
        "{:?}",
        r.messages
    );
}

#[test]
fn with_and_delete_name_fail_only_in_strict_code() {
    let build = |files: &[(&str, &str)], entry: &str, format: Format| {
        let fs = project(files);
        let mut o = opts(&[entry]);
        o.format = Some(format);
        let r = BuildContext::new(o, Arc::new(fs)).expect("options").build();
        r.messages
            .iter()
            .filter(|m| m.is_error())
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    };
    let sloppy = [("a.js", "with (Math) PI;\ndelete globalThis;\n")];
    assert_eq!(build(&sloppy, "a.js", Format::Cjs), Vec::<String>::new());
    assert_eq!(
        build(&sloppy, "a.js", Format::Esm),
        [
            "With statements cannot be used with the \"esm\" output format due to strict mode",
            "Delete of a bare identifier cannot be used with the \"esm\" output format due to strict mode",
        ]
    );
    let module = [("a.js", "export {};\nwith (Math) PI;\n")];
    assert_eq!(
        build(&module, "a.js", Format::Cjs),
        ["With statements cannot be used in an ECMAScript module"]
    );
    let directive = [("a.js", "function f() { 'use strict'; delete x }\n")];
    assert_eq!(
        build(&directive, "a.js", Format::Cjs),
        ["Delete of a bare identifier cannot be used in strict mode"]
    );
    // tsconfig's `alwaysStrict` counts only when set (directly or through `strict`).
    let ts = |config: &'static str| [("a.ts", "with (Math) PI;\n"), ("tsconfig.json", config)];
    assert!(build(&ts("{}"), "a.ts", Format::Cjs).is_empty());
    assert!(
        build(
            &ts(r#"{"compilerOptions":{"strict":false}}"#),
            "a.ts",
            Format::Cjs
        )
        .is_empty()
    );
    for config in [
        r#"{"compilerOptions":{"strict":true}}"#,
        r#"{"compilerOptions":{"strict":false,"alwaysStrict":true}}"#,
    ] {
        assert_eq!(
            build(&ts(config), "a.ts", Format::Cjs),
            ["With statements cannot be used in strict mode"],
            "{config}"
        );
    }
}

#[test]
fn cached_preparation_rechecks_output_format() {
    use ts_r_bundler::graph::{GraphContext, ModuleCache, build_graph};
    use ts_r_bundler::prepare::{Defines, PrepareContext};
    use ts_r_bundler::resolve::Resolver;

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("bundler-e2e")
        .join("cached-preparation-format");
    let _ = std::fs::remove_dir_all(&dir);
    let fs = project(&[("a.js", "with (Math) console.log(PI);\ndelete globalThis;\n")]);
    for (format, platform, parsed, errors) in [
        (Some(Format::Cjs), Platform::Node, 1, 0),
        (None, Platform::Node, 0, 0),
        (Some(Format::Esm), Platform::Node, 1, 2),
        (None, Platform::Neutral, 1, 2),
    ] {
        let mut o = opts(&["a.js"]);
        o.format = format;
        o.platform = platform;
        let o = Arc::new(o);
        let resolver = Arc::new(Resolver::new(Arc::new(fs.clone()), &o));
        let prep =
            PrepareContext::new(Arc::clone(&o), Defines::from_options(&o).unwrap(), resolver);
        let disk = ts_r_cache::Cache::open_in(&dir, "build", 1).expect("cache");
        let ctx = GraphContext {
            prep: &prep,
            cache: &ModuleCache::default(),
            disk: Some(&disk),
            threads: 2,
            changed: None,
            parsed: Default::default(),
            read_ahead: None,
        };
        let graph = build_graph(&ctx, &["/proj/a.js".into()]);
        assert_eq!(
            graph.messages.iter().filter(|m| m.is_error()).count(),
            errors,
            "{format:?} / {platform:?}: {:?}",
            graph.messages
        );
        assert_eq!(
            ctx.parsed.load(std::sync::atomic::Ordering::Relaxed),
            parsed
        );
    }
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn colliding_output_paths_fail_before_writing() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("bundler-e2e")
        .join("colliding-output-paths");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let fs = project(&[
        (
            "src/a.ts",
            "console.log('longer output from TypeScript');\n",
        ),
        ("src/a.js", "console.log(1);\n"),
        ("src/b.ts", "console.log('unrelated output');\n"),
    ]);
    for bundle in [false, true] {
        std::fs::write(dir.join("a.js"), "existing a").expect("write");
        std::fs::write(dir.join("b.js"), "existing b").expect("write");
        let mut o = opts(&["src/a.ts", "src/a.js", "src/b.ts"]);
        o.bundle = bundle;
        o.outdir = Some(dir.to_string_lossy().replace('\\', "/"));
        o.write = true;
        let r = BuildContext::new(o, Arc::new(fs.clone()))
            .expect("options")
            .build();
        assert!(r.has_errors(), "{r:?}");
        assert!(
            r.errors()
                .any(|m| m.text.contains("Two output files share the same path")),
            "{:?}",
            r.messages
        );
        assert_eq!(std::fs::read(dir.join("a.js")).unwrap(), b"existing a");
        assert_eq!(std::fs::read(dir.join("b.js")).unwrap(), b"existing b");
    }
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn assignments_to_constants_and_imports_fail_a_bundle() {
    let fs = project(&[
        (
            "a.js",
            "import { y } from './b.js';\nconst x = 1;\nfunction f(x) { x = 2 }\n\
             x += 1;\ny = 2;\n{ using u = null; u = 3 }\n",
        ),
        ("b.js", "export let y = 1;\n"),
    ]);
    let errors = |bundle: bool| {
        let mut o = opts(&["a.js"]);
        o.bundle = bundle;
        let r = BuildContext::new(o, Arc::new(fs.clone()))
            .expect("options")
            .build();
        r.messages
            .iter()
            .filter(|m| m.is_error())
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        errors(true),
        [
            "Cannot assign to \"x\" because it is a constant",
            "Cannot assign to import \"y\"",
            "Cannot assign to \"u\" because it is a constant",
        ]
    );
    assert!(errors(false).is_empty());
}

#[test]
fn top_level_await_makes_top_level_return_an_error() {
    let errors = |src: &str| {
        let fs = project(&[("a.js", src)]);
        let r = BuildContext::new(opts(&["a.js"]), Arc::new(fs))
            .expect("options")
            .build();
        r.messages
            .iter()
            .filter(|m| m.is_error())
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        errors("await 0;\nreturn;\n"),
        ["Top-level return cannot be used inside an ECMAScript module"]
    );
    assert!(errors("async function f() { await 0 }\nreturn;\n").is_empty());
    assert!(errors("const f = async () => { await 0 };\nreturn;\n").is_empty());
}

#[test]
fn javascript_wins_over_typescript_inside_node_modules() {
    // esbuild: implicit extensions prefer `.js` to `.ts` in `node_modules`, `.ts` elsewhere.
    let fs = project(&[
        (
            "main.ts",
            "import './rel/path';\nimport 'pkg/path';\nimport 'pkg/only_ts';\n",
        ),
        ("rel/path.ts", "console.log('rel ts');\n"),
        ("rel/path.js", "console.log('rel js');\n"),
        ("node_modules/pkg/path.ts", "console.log('pkg ts');\n"),
        ("node_modules/pkg/path.js", "console.log('pkg js');\n"),
        ("node_modules/pkg/only_ts.ts", "console.log('only ts');\n"),
        ("node_modules/pkg/only_ts.css", ".x {}\n"),
    ]);
    let r = run_build(&fs, opts(&["main.ts"]));
    let code = file(&r, "/proj/out/main.js");
    for (want, not) in [
        ("rel ts", "rel js"),
        ("pkg js", "pkg ts"),
        ("only ts", ".x"),
    ] {
        assert!(code.contains(want) && !code.contains(not), "{code}");
    }
}

#[test]
fn import_phases_are_only_bundled_as_external_esm_imports() {
    // esbuild: `import defer` / `import source` (and `import.defer()` / `import.source()`)
    // stay imports of external modules in ESM output; anything else is an error.
    let fs = project(&[
        (
            "entry.js",
            "import defer * as a from './foo.json';\n\
             import defer * as b from './foo.json' with { type: 'json' };\n\
             import source c from './foo.json';\n\
             console.log(a, b, c, import.defer('./foo.json'));\n",
        ),
        ("foo.json", "{}"),
    ]);
    let messages = |format: Format, external: bool| {
        let mut o = opts(&["entry.js"]);
        o.format = Some(format);
        if external {
            o.external = vec!["*.json".into()];
        }
        let r = BuildContext::new(o, Arc::new(fs.clone()))
            .expect("options")
            .build();
        let errors: Vec<String> = r.errors().map(|m| m.text.clone()).collect();
        (errors, r)
    };
    let (errors, _) = messages(Format::Cjs, true);
    assert_eq!(
        errors,
        [
            "Bundling deferred imports with the \"cjs\" output format is not supported",
            "Bundling deferred imports with the \"cjs\" output format is not supported",
            "Bundling source phase imports with the \"cjs\" output format is not supported",
            "Bundling deferred imports with the \"cjs\" output format is not supported",
        ]
    );
    let (errors, _) = messages(Format::Iife, false);
    assert_eq!(errors.len(), 4, "{errors:?}");
    assert!(errors[0].contains("\"iife\" output format"), "{errors:?}");
    let (errors, _) = messages(Format::Esm, false);
    assert_eq!(
        errors[2],
        "Bundling with source phase imports is not supported unless they are external"
    );
    let (errors, r) = messages(Format::Esm, true);
    assert!(errors.is_empty(), "{errors:?}");
    let code = file(&r, "/proj/out/entry.js");
    // Each import keeps its phase and attributes, as its own statement.
    for want in [
        "import defer * as foo from \"./foo.json\";",
        "import defer * as foo2 from \"./foo.json\" with { type: \"json\" };",
        "import source foo_default from \"./foo.json\";",
        "import.defer('./foo.json')",
    ] {
        assert!(code.contains(want), "{want}\n{code}");
    }
}

#[test]
fn an_entry_point_given_twice_is_built_once() {
    let fs = project(&[("entry.js", "console.log(123);\n")]);
    for bundle in [true, false] {
        let mut o = opts(&["entry.js", "entry.js"]);
        o.bundle = bundle;
        let r = run_build(&fs, o);
        let paths: Vec<&str> = r.outputs.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(paths, ["/proj/out/entry.js"]);
    }
    // Different output names are different entry points.
    let mut o = opts(&["entry.js", "entry.js"]);
    o.entry_points[1].name = Some("other".into());
    let r = run_build(&fs, o);
    assert_eq!(r.outputs.len(), 2);
}

#[test]
fn a_css_entry_without_bundling_is_a_css_output() {
    let fs = project(&[
        ("entry.js", "export let x = 1;\n"),
        ("entry.css", "a { color: red }\n"),
    ]);
    let mut o = opts(&["entry.js", "entry.css"]);
    o.bundle = false;
    let r = run_build(&fs, o);
    assert_eq!(file(&r, "/proj/out/entry.css"), "a { color: red }\n");
    assert!(file(&r, "/proj/out/entry.js").contains("x = 1"));
}

#[test]
fn package_json_must_be_strict_json_when_bundling() {
    // esbuild (like Node) fails on a comment or a trailing comma in package.json, where tsc
    // and the tsconfig reader accept both.
    let build = |pkg: &str| {
        let fs = project(&[
            ("entry.js", "import fn from 'demo';\nconsole.log(fn());\n"),
            ("node_modules/demo/package.json", pkg),
            ("node_modules/demo/index.js", "module.exports = () => 1;\n"),
        ]);
        let r = BuildContext::new(opts(&["entry.js"]), Arc::new(fs))
            .expect("options")
            .build();
        r.errors()
            .map(|m| format!("{}: {}", m.file.clone().unwrap_or_default(), m.text))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        build("{\n  // note\n  \"a\": 1\n}"),
        ["node_modules/demo/package.json: JSON does not support comments"]
    );
    assert_eq!(
        build("{ \"a\": [1, 2,], \"b\": \"//,}\" }"),
        ["node_modules/demo/package.json: JSON does not support trailing commas"]
    );
    assert!(build("{ \"a\": \"// not a comment, ]\" }").is_empty());
}

#[test]
fn a_bigint_in_a_json_file_is_an_error() {
    let fs = project(&[
        ("entry.js", "import d from './d.json';\nconsole.log(d);\n"),
        ("d.json", "{ \"ok\": \"12n\", \"bad\": [123n] }"),
    ]);
    let r = BuildContext::new(opts(&["entry.js"]), Arc::new(fs))
        .expect("options")
        .build();
    let errors: Vec<String> = r.errors().map(|m| m.text.clone()).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("Unexpected \"123n\" in JSON"),
        "{errors:?}"
    );
}

#[test]
fn an_import_with_a_type_attribute_is_its_own_module_instance() {
    // esbuild: `with { type }` picks the loader (json, text, bytes) whatever the extension,
    // and makes a separate instance; `assert { ... }` is ignored.
    let fs = project(&[
        (
            "entry.js",
            "import x from './data.json';\n\
             import y from './data.json' assert { type: 'json' };\n\
             import z from './data.json' with { type: 'json' };\n\
             import t from './foo.js' with { type: 'text' };\n\
             import b from './foo.js' with { type: 'bytes' };\n\
             import j from './foo.js' with { type: 'json' };\n\
             console.log(x === y, x !== z, t, b, j);\n",
        ),
        ("data.json", "{ \"works\": true }"),
        ("foo.js", "{ \"json\": 1 }"),
    ]);
    let r = BuildContext::new(opts(&["entry.js"]), Arc::new(fs))
        .expect("options")
        .build();
    assert!(r.errors().next().is_none(), "{:?}", r.messages);
    let out: String = r.outputs.iter().map(|o| o.text()).collect();
    assert!(out.contains("// data.json with { type: 'json' }"), "{out}");
    assert!(out.contains("Uint8Array.fromBase64("), "{out}");
    assert!(out.contains("\"{ \\\"json\\\": 1 }\""), "{out}");
    assert!(out.matches("require_data").count() >= 2, "{out}");
}

#[test]
fn attributed_json_namespace_has_only_the_complete_default_value() {
    for (i, value) in [
        r#"{"__esModule":true,"x":1,"default":2}"#,
        r#"{"__proto__":123,"x":1}"#,
        "[1,2]",
        "123",
        "null",
    ]
    .iter()
    .enumerate()
    {
        for minify in [false, true] {
            let fs = project(&[
                (
                    "entry.js",
                    "import data, * as ns from './data.txt' with { type: 'json' };\n\
                     console.log(JSON.stringify(data), data === ns.default, Object.keys(ns).join(','), typeof Object.getOwnPropertyDescriptor(ns, 'default').get);\n",
                ),
                ("data.txt", value),
            ]);
            let mut o = opts(&["entry.js"]);
            o.minify_syntax = minify;
            o.minify_whitespace = minify;
            o.minify_identifiers = minify;
            let r = run_build(&fs, o);
            if have_node() {
                assert_eq!(
                    run_node(&format!("attributed-json-{i}-{minify}"), &r, "entry.js"),
                    format!("{value} true default function\n")
                );
            }
        }
    }
}

#[test]
fn json_computed_proto_keys_remain_named_exports() {
    for (i, json) in [
        r#"{"__proto__":123,"x":1}"#,
        r#"{"\u005f_proto__":123,"nested":{"__proto__":456},"x":1}"#,
    ]
    .iter()
    .enumerate()
    {
        let fs = project(&[
            (
                "entry.js",
                "import data, { __proto__ as p, x } from './data.json';\n\
                 export { __proto__ as forwarded } from './data.json';\n\
                 console.log(p, x, Object.prototype.hasOwnProperty.call(data, '__proto__'));\n",
            ),
            ("data.json", json),
        ]);
        let r = run_build(&fs, opts(&["entry.js"]));
        if have_node() {
            assert_eq!(
                run_node(&format!("json-named-proto-{i}"), &r, "entry.js"),
                "123 1 true\n"
            );
        }
    }
    let fs = project(&[
        (
            "entry.js",
            "import { missing } from './data.json'; console.log(missing);",
        ),
        ("data.json", r#"{"__proto__":123}"#),
    ]);
    let r = BuildContext::new(opts(&["entry.js"]), Arc::new(fs))
        .expect("options")
        .build();
    assert!(r.errors().any(|m| m.text.contains("No matching export")));
}

#[test]
fn a_json_import_with_a_type_attribute_has_only_a_default_export() {
    let errors = |entry: &str| {
        let fs = project(&[("entry.js", entry), ("data.json", "{ \"foo\": 123 }")]);
        let r = BuildContext::new(opts(&["entry.js"]), Arc::new(fs))
            .expect("options")
            .build();
        r.errors().map(|m| m.text.clone()).collect::<Vec<_>>()
    };
    assert_eq!(
        errors(
            "import x, { foo as x2 } from './data.json';\n\
             import y, { foo as y2 } from './data.json' with { type: 'json' };\n\
             console.log(x, x2, y, y2);\n"
        ),
        ["No matching export in \"data.json with { type: 'json' }\" for import \"foo\""]
    );
    // Attributes a bundle cannot honour fail the scan.
    assert_eq!(
        errors(
            "import w from './data.json' with { kind: 'json' };\n\
             import v from './data.json' with { type: 'css' };\n\
             console.log(w, v);\n"
        ),
        [
            "Importing with the \"kind\" attribute is not supported",
            "Importing with a type attribute of \"css\" is not supported",
        ]
    );
}

#[test]
fn a_cycle_of_named_re_exports_is_an_error() {
    let errors = |files: &[(&str, &str)]| {
        let r = BuildContext::new(opts(&["entry.js"]), Arc::new(project(files)))
            .expect("options")
            .build();
        let mut e: Vec<String> = r
            .errors()
            .map(|m| format!("{}: {}", m.file.clone().unwrap_or_default(), m.text))
            .collect();
        e.sort();
        e
    };
    // The cycle is reported where it closes on the way from each exported name (esbuild
    // also reports it from every other import on it).
    assert_eq!(
        errors(&[
            (
                "entry.js",
                "export {a as b} from './foo';\nexport {c as d} from './foo';\n"
            ),
            (
                "foo.js",
                "export {b as c} from './entry';\nexport {d as a} from './entry';\n"
            ),
        ]),
        [
            "foo.js: Detected cycle while resolving import \"b\"",
            "foo.js: Detected cycle while resolving import \"d\"",
        ]
    );
    // Two `export *` paths to one module are not a cycle, and neither is an `export *` loop.
    assert!(
        errors(&[
            (
                "entry.js",
                "export * from './p';\nexport * from './q';\nexport * from './entry';\n"
            ),
            ("p.js", "export {x} from './r';\n"),
            ("q.js", "export {x} from './r';\n"),
            ("r.js", "export const x = 1;\n"),
        ])
        .is_empty()
    );
}

#[test]
fn a_jsx_factory_of_this_is_the_module_this() {
    // `--jsx-factory=this.h`: top-level `this` of an ES module is `undefined`.
    let fs = project(&[("entry.jsx", "export {};\nconsole.log(<div />);\n")]);
    let mut o = opts(&["entry.jsx"]);
    o.jsx.factory = Some("this.h".into());
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/entry.js");
    assert!(code.contains("(void 0).h(\"div\""), "{code}");
}

#[test]
fn jsx_dev_applies_to_a_tsconfig_react_jsx() {
    let fs = project(&[
        ("entry.tsx", "console.log(<><div /></>);\n"),
        (
            "tsconfig.json",
            r#"{"compilerOptions": {"jsx": "react-jsx"}}"#,
        ),
    ]);
    let mut o = opts(&["entry.tsx"]);
    o.jsx.dev = true;
    o.external = vec!["react/jsx-dev-runtime".into()];
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/entry.js");
    assert!(code.contains("from \"react/jsx-dev-runtime\""), "{code}");
}

#[test]
fn a_missing_tsconfig_override_is_an_error() {
    let fs = project(&[("entry.ts", "console.log(1);\n")]);
    let mut o = opts(&["entry.ts"]);
    o.tsconfig = Some("missing/tsconfig.json".into());
    let r = BuildContext::new(o, Arc::new(fs.clone()))
        .expect("options")
        .build();
    let errors: Vec<String> = r.errors().map(|m| m.text.clone()).collect();
    assert_eq!(
        errors,
        ["Cannot find tsconfig file \"missing/tsconfig.json\""]
    );
}

#[test]
fn top_level_this_of_a_commonjs_file_is_its_exports() {
    let fs = project(&[
        (
            "entry.js",
            "const lib = require('./lib.js');\nconsole.log(lib.a, lib.b);\n",
        ),
        (
            "lib.js",
            "this.a = 1;\n(() => { this.b = 2; })();\nfunction f() { return this; }\n",
        ),
        ("own.js", "var exports = {};\nthis.c = 3;\n"),
    ]);
    let r = run_build(&fs, opts(&["entry.js"]));
    let code = file(&r, "/proj/out/entry.js");
    assert!(code.contains("exports.a = 1"), "{code}");
    assert!(code.contains("exports.b = 2"), "{code}");
    assert!(code.contains("return this"), "{code}");
    if have_node() {
        assert_eq!(run_node("this_is_exports", &r, "entry.js").trim(), "1 2");
    }
    // A file with its own `exports` keeps `this`.
    let r = run_build(&fs, opts(&["own.js"]));
    assert!(file(&r, "/proj/out/own.js").contains("this.c = 3"));
}

#[test]
fn javascript_accessor_fields_are_lowered_for_a_year_target() {
    let fs = project(&[(
        "entry.js",
        "class Foo {\n  accessor one = 1;\n  static accessor #two = 2;\n  static two() { return Foo.#two; }\n}\nconsole.log(new Foo().one, Foo.two());\n",
    )]);
    let mut o = opts(&["entry.js"]);
    o.target = Some(2022);
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/entry.js");
    assert!(!code.contains("accessor "), "{code}");
    if have_node() {
        assert_eq!(run_node("js_accessor", &r, "entry.js").trim(), "1 2");
    }
    // Without a target the syntax is kept.
    let r = run_build(&fs, opts(&["entry.js"]));
    assert!(file(&r, "/proj/out/entry.js").contains("accessor one"));
}

#[test]
fn tsconfig_jsx_preserve_is_ignored() {
    // As in esbuild, the output is JavaScript: only `--jsx=preserve` keeps JSX.
    let fs = project(&[
        ("entry.tsx", "console.log(<div />);\n"),
        (
            "tsconfig.json",
            r#"{"compilerOptions": {"jsx": "preserve"}}"#,
        ),
    ]);
    let r = run_build(&fs, opts(&["entry.tsx"]));
    let code = file(&r, "/proj/out/entry.js");
    assert!(code.contains("React.createElement(\"div\""), "{code}");
    let mut o = opts(&["entry.tsx"]);
    o.jsx.mode = Some("preserve".into());
    let r = run_build(&fs, o);
    assert!(file(&r, "/proj/out/entry.js").contains("<div />"));
}

#[test]
fn external_imports_keep_evaluation_order() {
    // `foo` is imported before `./bar`, whose own external `bar` loads after it, even
    // though bar.js is placed first in the bundle.
    let fs = project(&[
        (
            "entry.js",
            "export {default as foo} from 'foo';\nexport {bar} from './bar';\n",
        ),
        ("bar.js", "export {default as bar} from 'bar';\n"),
    ]);
    let mut o = opts(&["entry.js"]);
    o.external = vec!["foo".into(), "bar".into()];
    let r = run_build(&fs, o);
    let code = file(&r, "/proj/out/entry.js");
    let foo = code.find("from \"foo\"").expect(code);
    let bar = code.find("from \"bar\"").expect(code);
    assert!(foo < bar, "{code}");
}

#[test]
fn json_namespace_imports_are_es_namespaces() {
    // `default` is the whole value, even with an `__esModule` key, and like every other
    // property of an ES namespace it is a getter. A `__proto__` key is a property.
    if !have_node() {
        return;
    }
    let fs = project(&[
        (
            "entry.js",
            "import * as ns from './a.json';\nimport b from './b.json';\n\
             console.log(Object.getOwnPropertyDescriptor(ns, 'default').get !== undefined, \
             ns.default.__esModule, Object.keys(b));\n",
        ),
        ("a.json", r#"{"__esModule": true, "x-y": 1}"#),
        ("b.json", r#"{"__proto__": {"foo": 1}}"#),
    ]);
    let r = run_build(&fs, opts(&["entry.js"]));
    assert_eq!(
        run_node("json-ns", &r, "entry.js"),
        "true true [ '__proto__' ]\n"
    );
}

#[test]
fn imported_scripts_are_commonjs_and_missing_exports_do_not_include_files() {
    // A `.js` file with no ESM syntax is CommonJS when something imports from it, so its
    // `default` is `module.exports`; an `.mjs` one is an ES module without exports. Reading
    // a missing export does not pull a `sideEffects: false` ES module into the bundle.
    if !have_node() {
        return;
    }
    let fs = project(&[
        (
            "entry.js",
            "import * as js from './foo/a.js';\nimport * as mjs from './foo/b.mjs';\n\
             import * as meta from './foo/c.js';\n\
             console.log(js.default, mjs.default, mjs.nope, meta.nope);\n",
        ),
        ("foo/package.json", r#"{"sideEffects": false}"#),
        ("foo/a.js", "console.log('js');\n"),
        ("foo/b.mjs", "console.log('mjs');\n"),
        ("foo/c.js", "console.log('meta', typeof import.meta);\n"),
    ]);
    let r = run_build(&fs, opts(&["entry.js"]));
    assert_eq!(
        run_node("cjs-scripts", &r, "entry.js"),
        "js\n{} undefined undefined undefined\n"
    );
}

#[test]
fn glob_entry_points_expand_to_matching_files() {
    let fs = project(&[
        ("src/a.js", "console.log('a');\n"),
        ("src/deep/b.js", "console.log('b');\n"),
        ("src/c.ts", "console.log('c');\n"),
    ]);
    let r = run_build(&fs, opts(&["/proj/**/*.js"]));
    let mut paths: Vec<&str> = r.outputs.iter().map(|f| f.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["/proj/out/a.js", "/proj/out/deep/b.js"]);
    let r = run_build(&fs, opts(&["src/*.js"]));
    assert_eq!(r.outputs.len(), 1);
    let ctx = BuildContext::new(opts(&["nope/*.js"]), Arc::new(fs.clone())).expect("options");
    assert!(ctx.build().has_errors());
}

#[test]
fn an_export_named_proto_is_a_namespace_property() {
    if !have_node() {
        return;
    }
    let fs = project(&[
        (
            "entry.js",
            "const lib = require('./lib.mjs');\nimport * as ns from './lib2.mjs';\n\
             console.log(lib.__proto__, Object.keys(ns));\n",
        ),
        ("lib.mjs", "export const __proto__ = 123;\n"),
        ("lib2.mjs", "const x = 1;\nexport { x as __proto__ };\n"),
    ]);
    for minify in [false, true] {
        let mut o = opts(&["entry.js"]);
        o.format = Some(Format::Cjs);
        o.minify_syntax = minify;
        o.minify_whitespace = minify;
        let r = run_build(&fs, o);
        assert_eq!(
            run_node("proto-export", &r, "entry.js"),
            "123 [ '__proto__' ]\n"
        );
    }
}

#[test]
fn exported_const_enums_keep_their_object() {
    if !have_node() {
        return;
    }
    let fs = project(&[
        (
            "foo.ts",
            "import { Foo, Baz } from './bar';\nconst enum Bar { A = 1, B = 2 }\n\
             console.log(Foo['%/*'], Foo.Y, Baz.Z, Bar.A, Bar.B);\n",
        ),
        (
            "bar.ts",
            "export const enum Foo { '%/*' = 1, Y = 'y' }\nconst enum Baz { Z = 3 }\n\
             export { Baz };\n",
        ),
    ]);
    let r = run_build(&fs, opts(&["foo.ts"]));
    assert_eq!(run_node("const-enum-export", &r, "foo.js"), "1 y 3 1 2\n");
}
