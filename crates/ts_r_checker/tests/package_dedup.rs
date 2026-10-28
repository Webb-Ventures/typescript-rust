//! Package deduplication (tsc's `deduplicatePackages`): identical copies of `x@1.2.3` under
//! `a` and `b` load once. A `/// <reference path>` to the copy that was folded into the other
//! still names a file of the program (no TS6053), and libraries only the folded copy's
//! dependencies reference are not loaded. No lib files needed (`noLib`).

use ts_r_checker::{CheckRun, ProgramFiles, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

const LIB: &str = "interface Array<T> { length: number; [n: number]: T; }\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n";
const X: &str = "export declare class X {\n    private p;\n}\n";
const X_PKG: &str = r#"{ "name": "x", "version": "1.2.3", "types": "index.d.ts" }"#;

fn packages(fs: &mut MemoryFs) {
    fs.add_file(
        "/p/node_modules/a/package.json",
        r#"{ "name": "a", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/a/index.d.ts",
        "import { X } from \"x\";\nexport declare function a(x: X): void;\n",
    );
    fs.add_file("/p/node_modules/a/node_modules/x/package.json", X_PKG);
    fs.add_file("/p/node_modules/a/node_modules/x/index.d.ts", X);
    fs.add_file(
        "/p/node_modules/b/package.json",
        r#"{ "name": "b", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/b/index.d.ts",
        "import { X } from \"x\";\nexport declare function b(): X;\n",
    );
    fs.add_file("/p/node_modules/b/node_modules/x/package.json", X_PKG);
    fs.add_file("/p/node_modules/b/node_modules/x/index.d.ts", X);
}

fn load(fs: &MemoryFs) -> ProgramFiles {
    let cfg = load_tsconfig(fs, "/p/tsconfig.json");
    load_program(fs, &cfg.file_names, &cfg.options, None, 1)
}

fn lines(program: &ProgramFiles) -> Vec<String> {
    check_program(program, CheckRun { threads: 1 })
        .diagnostics
        .iter()
        .map(|d| {
            let f = &program.files[d.file.0 as usize];
            let lc = f.lines().line_col_utf16(&f.text, d.span.start);
            format!("{}:{}:{} TS{}", f.path, lc.line + 1, lc.col + 1, d.code)
        })
        .collect()
}

/// tsc 7.0.2 (`--listFiles`): one `x` (a's), no diagnostics.
#[test]
fn a_reference_to_a_folded_copy_is_found() {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    packages(&mut fs);
    fs.add_file("/p/lib.d.ts", LIB);
    fs.add_file("/p/one.ts", "import { a } from \"a\";\nexport {};\n");
    fs.add_file(
        "/p/two.ts",
        "/// <reference path=\"node_modules/b/node_modules/x/index.d.ts\" />\nimport { b } from \"b\";\nexport {};\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"noLib": true, "strict": true, "module": "esnext", "moduleResolution": "bundler", "types": []}, "files": ["lib.d.ts", "one.ts", "two.ts"]}"#,
    );
    let program = load(&fs);
    // One copy of `x` in the program.
    let xs: Vec<&str> = program
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.ends_with("/x/index.d.ts"))
        .collect();
    assert_eq!(xs, ["/p/node_modules/a/node_modules/x/index.d.ts"]);
    assert_eq!(lines(&program), Vec::<String>::new());
}

/// A library only the folded copy's own dependency references (`/// <reference lib="dom" />`
/// in the `dep@2` that `b`'s copy of `x` resolves) is not in the program: tsc 7.0.2 lists no
/// DOM lib and reports `main.ts(4,18): error TS2584: Cannot find name 'document'. Do you
/// need to change your target library? ...`. Needs the lib files.
#[test]
fn libraries_of_a_folded_copys_dependencies_are_not_loaded() {
    let Some(libs) = ts_r_checker::LibStore::locate() else {
        eprintln!("skipped: no lib files");
        return;
    };
    let x = "import { D } from \"dep\";\nexport declare class X {\n    private p;\n    d: D;\n}\n";
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/node_modules/a/package.json",
        r#"{ "name": "a", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/a/index.d.ts",
        "import { X } from \"x\";\nexport declare function a(x: X): void;\n",
    );
    fs.add_file("/p/node_modules/a/node_modules/x/package.json", X_PKG);
    fs.add_file("/p/node_modules/a/node_modules/x/index.d.ts", x);
    fs.add_file(
        "/p/node_modules/b/package.json",
        r#"{ "name": "b", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/b/index.d.ts",
        "import { X } from \"x\";\nexport declare function b(): X;\n",
    );
    fs.add_file("/p/node_modules/b/node_modules/x/package.json", X_PKG);
    fs.add_file("/p/node_modules/b/node_modules/x/index.d.ts", x);
    fs.add_file(
        "/p/node_modules/dep/package.json",
        r#"{ "name": "dep", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file("/p/node_modules/dep/index.d.ts", "export interface D {}\n");
    fs.add_file(
        "/p/node_modules/b/node_modules/x/node_modules/dep/package.json",
        r#"{ "name": "dep", "version": "2.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/b/node_modules/x/node_modules/dep/index.d.ts",
        "/// <reference lib=\"dom\" />\nexport interface D {}\n",
    );
    fs.add_file(
        "/p/main.ts",
        "import { a } from \"a\";\nimport { b } from \"b\";\na(b());\nexport const t = document.title;\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "lib": ["es2015"], "module": "esnext", "moduleResolution": "bundler", "types": [], "noEmit": true}, "files": ["main.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let names: Vec<&str> = program
        .files
        .iter()
        .map(|f| f.path.rsplit('/').next().unwrap_or(&f.path))
        .filter(|n| n.starts_with("lib.dom"))
        .collect();
    assert!(names.is_empty(), "{names:?}");
    let non_lib: Vec<&str> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        non_lib,
        [
            "/p/node_modules/dep/index.d.ts",
            "/p/node_modules/a/node_modules/x/index.d.ts",
            "/p/node_modules/a/index.d.ts",
            "/p/node_modules/b/index.d.ts",
            "/p/main.ts"
        ]
    );
    assert_eq!(lines(&program), ["/p/main.ts:4:18 TS2584"]);
}
