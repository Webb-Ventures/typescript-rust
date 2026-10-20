use ts_r_checker::{CheckRun, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

fn check(src: &str) -> Vec<(u32, String)> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.cts", src);
    fs.add_file("/p/data.json", r#"{"x":1}"#);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"module":"node16","resolveJsonModule":true,"noEmit":true,"noLib":true},"files":["a.cts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    result
        .diagnostics
        .into_iter()
        // `noLib`: the missing core global types (TS2318) are not what these tests check.
        .filter(|d| d.code != 2318)
        .map(|d| (d.code, d.message))
        .collect()
}

#[test]
fn import_equals_uses_json_value() {
    let out = check(
        "import data = require('./data.json');\nexport const n = data.x;\nconst s: string = n;\n",
    );
    assert_eq!(
        out,
        [(
            2322,
            "Type 'number' is not assignable to type 'string'.".to_owned()
        )]
    );
}

#[test]
fn type_only_json_namespace_is_not_a_value() {
    let out = check("import type * as data from './data.json';\nexport const n = data.x;\n");
    assert_eq!(
        out,
        [(
            1361,
            "'data' cannot be used as a value because it was imported using 'import type'."
                .to_owned()
        )]
    );
}
