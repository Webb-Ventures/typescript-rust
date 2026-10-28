//! Instantiation-query names follow types shared between workers and program levels, and
//! the editor's scoped type printing.

use ts_r_ast::Node;
use ts_r_checker::{CheckRun, Checker, Ck, LibStore, Loc, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

#[test]
fn cached_instantiation_query_names() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/a.ts",
        "export declare function f<T>(x: T): T;\nexport declare const g: typeof f<string>;\n",
    );
    fs.add_file(
        "/p/b.ts",
        "import { g } from './a';\nconst n: number = g;\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022"}, "files": ["a.ts", "b.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let a = FileId(program.file_index("/p/a.ts").unwrap() as u32);
    let b = FileId(program.file_index("/p/b.ts").unwrap() as u32);
    let expected = "Type 'typeof f<string>' is not assignable to type 'number'.";
    let checker = Checker::new(&program);
    assert!(Ck::new(&checker, a).check_source_file(a).is_empty());
    // A fresh worker must print the cached type's name without checking its declaration.
    let diagnostics = Ck::new(&checker, b).check_source_file(b);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, 2322);
    assert_eq!(diagnostics[0].message, expected);

    let child = Checker::layered(&program, Some(checker.top_layer())).unwrap();
    let diagnostics = Ck::new(&child, b).check_source_file(b);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, expected);

    for threads in [1, 4] {
        let result = check_program(&program, CheckRun { threads });
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, expected);
    }
}

/// `type_to_string_at` names types with the namespaces around them, except those whose
/// body encloses the scope (tsc 7.0.2 quick info: `m1.m2.c`, `N.D2<number>`, `typeof N.c`).
#[test]
fn scoped_type_names() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/a.ts",
        "namespace m1 { export namespace m2 { export class c {} export class D2<T> { t!: T } } }\n\
         let x: m1.m2.c;\nlet y: m1.m2.D2<number>;\nlet z: typeof m1.m2.c;\n\
         namespace m1 { let w: m2.c; }\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022"}, "files": ["a.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let a = program.file_index("/p/a.ts").unwrap();
    let file = FileId(a as u32);
    let ast = &program.files[a].ast;
    let checker = Checker::new(&program);
    let mut ck = Ck::new(&checker, file);
    let mut printed = Vec::new();
    for i in 0..ast.node_count() {
        let Some(id) = ts_r_ast::NodeId::from_raw(i as u32) else {
            continue;
        };
        if let Node::VarDeclarator {
            type_ann: Some(t), ..
        } = *ast.node(id)
        {
            let ty = ck.type_from_node(Loc::new(file, t));
            printed.push(ck.type_to_string_at(ty, Loc::new(file, id)));
        }
    }
    printed.sort();
    assert_eq!(
        printed,
        ["m1.m2.D2<number>", "m1.m2.c", "m2.c", "typeof m1.m2.c"]
    );
    // Diagnostics keep tsc's bare names.
    let diagnostics = ck.check_source_file(file);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
