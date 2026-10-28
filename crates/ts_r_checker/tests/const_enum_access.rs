//! `Checker::const_enum_access_value`: the value emit inlines for a const enum access.

use ts_r_ast::Node;
use ts_r_checker::emit::EnumConst;
use ts_r_checker::{Checker, LibStore, Loc, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

/// The value of each `Member` access in `/p/b.ts` whose text is `access`.
fn access_values(a: &str, b: &str, access: &str) -> Option<Vec<Option<EnumConst>>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", a);
    fs.add_file("/p/b.ts", b);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"target":"es2020","strict":false},"files":["a.ts","b.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let checker = Checker::new(&program);
    let fi = program.files.iter().position(|f| f.path == "/p/b.ts")?;
    let f = &program.files[fi];
    let mut out = Vec::new();
    for (i, n) in f.ast.nodes.iter().enumerate() {
        let Some(id) = ts_r_ast::NodeId::from_raw(i as u32) else {
            continue;
        };
        let span = f.ast.span(id);
        if matches!(n, Node::Member { .. })
            && f.text.get(span.start as usize..span.end as usize) == Some(access)
        {
            out.push(checker.const_enum_access_value(Loc::new(FileId(fi as u32), id)));
        }
    }
    Some(out)
}

/// An import alias in a nested namespace that merges with the target's root namespace
/// (constEnumOnlyModuleImportAliasEmit): `OnRtls` is the merged namespace from there too.
#[test]
fn alias_in_merged_nested_namespace() {
    let a =
        "namespace OnRtls.Mappa { export const enum TipoCamera { Camera2D = 1, Camera3D = 2 } }\n";
    for b in [
        "namespace OnRtls.Carrello.Home { import mappa = OnRtls.Mappa; export var x = mappa.TipoCamera.Camera3D; }\n",
        "namespace Q { import mappa = OnRtls.Mappa; export var x = mappa.TipoCamera.Camera3D; }\n",
    ] {
        let Some(v) = access_values(a, b, "mappa.TipoCamera.Camera3D") else {
            return;
        };
        assert_eq!(v, vec![Some(EnumConst::Number(2.0))], "{b}");
    }
}
