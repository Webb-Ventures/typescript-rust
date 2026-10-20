//! Implicit helper imports remain dependencies alongside named binder imports.

use ts_r_checker::{CheckRun, Checker, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_history::incremental::History;
use ts_r_history::persist::{self, Mode};
use ts_r_history::shape::{Ref, shape_of};

ts_r_testing::cap_memory!();

fn project(helper: bool) -> MemoryFs {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"lib":["es5"],"types":[],"target":"es2015","module":"esnext","moduleResolution":"bundler","importHelpers":true},"files":["main.ts","globals.d.ts"]}"#,
    );
    fs.add_file(
        "/p/main.ts",
        "import { value } from './dep';\nexport async function f() { return value; }\n",
    );
    fs.add_file("/p/globals.d.ts", "interface Promise<T> {}\n");
    fs.add_file("/p/dep.ts", "export const value: number = 1;\n");
    fs.add_file(
        "/p/node_modules/tslib/package.json",
        r#"{"name":"tslib","types":"index.d.ts"}"#,
    );
    fs.add_file(
        "/p/node_modules/tslib/index.d.ts",
        if helper {
            "export declare function __awaiter(...args: any[]): any;\n"
        } else {
            "export {};\n"
        },
    );
    fs
}

#[test]
fn removing_an_implicit_helper_invalidates_named_importers() {
    let dir = std::env::temp_dir().join(format!("ts-r-helper-imports-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = ts_r_cache::Cache::open_in(&dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    // A minimal lib with the core global types (without them every check reports TS2318,
    // and the lazy path defers to a full load).
    std::fs::create_dir_all(dir.join("libs")).expect("create dir");
    std::fs::write(
        dir.join("libs/lib.es5.d.ts"),
        "interface Array<T> {}\ninterface Boolean {}\ninterface CallableFunction {}\n\
         interface Function {}\ninterface IArguments {}\ninterface NewableFunction {}\n\
         interface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n",
    )
    .expect("write lib");
    let libs = LibStore::new(dir.join("libs"));
    let mut history = History::default();
    for (helper, mode) in [
        (true, Mode::Cold),
        (true, Mode::Unchanged),
        (false, Mode::Incremental),
        (false, Mode::Unchanged),
        (true, Mode::Incremental),
    ] {
        let fs = project(helper);
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let main = program
            .files
            .iter()
            .find(|f| f.path == "/p/main.ts")
            .expect("main");
        let shape = shape_of(&program, main);
        assert!(
            shape
                .uses
                .contains(&Ref::All("/p/node_modules/tslib/index.d.ts".to_owned()))
        );
        assert!(
            shape
                .uses
                .contains(&Ref::Export("/p/dep.ts".to_owned(), "value".to_owned()))
        );
        assert!(!shape.uses.contains(&Ref::All("/p/dep.ts".to_owned())));

        let fresh = check_program(&program, CheckRun { threads: 1 }).diagnostics;
        assert_eq!(
            fresh.iter().map(|d| d.code).collect::<Vec<_>>(),
            if helper { vec![] } else { vec![2343] }
        );
        let (incremental, _) = history.check(&program, CheckRun { threads: 1 }, Checker::new);
        assert_eq!(incremental, fresh);
        let saved = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(saved.mode, mode);
        assert_eq!(saved.diagnostics, fresh);
        if mode == Mode::Unchanged {
            assert_eq!(saved.parsed, 0);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
