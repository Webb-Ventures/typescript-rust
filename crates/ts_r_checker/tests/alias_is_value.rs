//! `Checker::alias_is_value`: whether emit keeps an import.

use ts_r_ast::Node;
use ts_r_checker::{Checker, LibStore, Loc, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

/// The answer for each default import in `/p/main.ts`, with `/p/c.ts` holding `c`.
fn default_imports(c: &str, main: &str) -> Option<Vec<Option<bool>>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/c.ts", c);
    fs.add_file("/p/main.ts", main);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"target":"esnext","module":"esnext"},"files":["c.ts","main.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let checker = Checker::new(&program);
    let fi = program.files.iter().position(|f| f.path == "/p/main.ts")?;
    let f = &program.files[fi];
    let mut out = Vec::new();
    for (i, n) in f.ast.nodes.iter().enumerate() {
        let Some(id) = ts_r_ast::NodeId::from_raw(i as u32) else {
            continue;
        };
        if matches!(n, Node::ImportDefaultSpecifier { .. }) {
            out.push(checker.alias_is_value(Loc::new(FileId(fi as u32), id), false));
        }
    }
    Some(out)
}

/// `export default F` of a local interface `F` and an exported `const F` is a value
/// (importElisionExportNonExportAndDefault: tsc keeps the import).
#[test]
fn default_export_of_local_type_and_exported_value() {
    let main = "import F from \"./c\";\nF({ msg: \"x\" });\n";
    let Some(kept) = default_imports(
        "interface F { msg: string; }\nexport const F = (f: F) => f.msg;\nexport default F;\n",
        main,
    ) else {
        return;
    };
    assert_eq!(kept, [Some(true)]);
    let Some(kept) = default_imports("interface F { msg: string; }\nexport default F;\n", main)
    else {
        return;
    };
    assert_eq!(kept, [Some(false)]);
}
