//! `Checker::enum_member_value`: the evaluated value of any enum member, for emit.

use ts_r_ast::Node;
use ts_r_checker::emit::EnumConst;
use ts_r_checker::{Checker, LibStore, Loc, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

fn values(isolated: bool) -> Option<Vec<(String, Option<EnumConst>)>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/bar.ts", "export const BAR = 'bar';\n");
    fs.add_file(
        "/p/foo.ts",
        "import { BAR } from './bar';\nconst enum C { X = 1 }\nexport enum Foo { A = `${BAR}`, F = BAR, G = 2 + BAR, H = A, I = H + BAR, J = C.X + 1, K = Math.random() }\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        &format!(
            r#"{{"compilerOptions":{{"target":"es2020","isolatedModules":{isolated},"noEmit":true}},"files":["bar.ts","foo.ts"]}}"#
        ),
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let checker = Checker::new(&program);
    let fi = program.files.iter().position(|f| f.path == "/p/foo.ts")?;
    let file = FileId(fi as u32);
    let ast = &program.files[fi].ast;
    let mut out = Vec::new();
    for (i, n) in ast.nodes.iter().enumerate() {
        if let Node::EnumMember { name, .. } = *n {
            let text = ast.name_of(name).unwrap_or_default().to_owned();
            let Some(id) = ts_r_ast::NodeId::from_raw(i as u32) else {
                continue;
            };
            let loc = Loc::new(file, id);
            out.push((text, checker.enum_member_value(loc)));
        }
    }
    Some(out)
}

#[test]
fn enum_member_values_reach_other_files() {
    for isolated in [false, true] {
        let Some(v) = values(isolated) else { return };
        let s = |x: &str| Some(EnumConst::String(x.to_owned()));
        assert_eq!(
            v,
            vec![
                ("X".to_owned(), Some(EnumConst::Number(1.0))),
                ("A".to_owned(), s("bar")),
                ("F".to_owned(), s("bar")),
                ("G".to_owned(), s("2bar")),
                ("H".to_owned(), s("bar")),
                ("I".to_owned(), s("barbar")),
                ("J".to_owned(), Some(EnumConst::Number(2.0))),
                ("K".to_owned(), None),
            ],
            "isolatedModules: {isolated}"
        );
    }
}
