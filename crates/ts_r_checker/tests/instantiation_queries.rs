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

/// A program of `files` (path, text) under `/p`, strict, all files listed.
fn editor_program(files: &[(&str, &str)]) -> Option<ts_r_checker::ProgramFiles> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    let mut names = Vec::new();
    for (path, text) in files {
        fs.add_file(&format!("/p/{path}"), text);
        names.push(format!("\"{path}\""));
    }
    fs.add_file(
        "/p/tsconfig.json",
        &format!(
            r#"{{"compilerOptions": {{"strict": true, "target": "es2022", "module": "commonjs"}}, "files": [{}]}}"#,
            names.join(", ")
        ),
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    Some(load_program(
        &fs,
        &cfg.file_names,
        &cfg.options,
        Some(&libs),
        1,
    ))
}

/// Each annotated variable declaration of `file` (in source order) with its annotation.
fn annotated_vars(ast: &ts_r_ast::Ast) -> Vec<(ts_r_ast::NodeId, ts_r_ast::NodeId)> {
    let mut out = Vec::new();
    for i in 0..ast.node_count() {
        let Some(id) = ts_r_ast::NodeId::from_raw(i as u32) else {
            continue;
        };
        if let Node::VarDeclarator {
            type_ann: Some(t), ..
        } = *ast.node(id)
        {
            out.push((id, t));
        }
    }
    out.sort_by_key(|&(id, _)| ast.span(id).start);
    out
}

/// `signature_to_string_at` qualifies names as `type_to_string_at` does (tsc 7.0.2 quick
/// info: `iterator?: Underscore.Iterator<number, boolean>`); `signature_to_string` and
/// `print_signature` keep the bare names diagnostics use.
#[test]
fn scoped_signature_names() {
    let Some(program) = editor_program(&[(
        "a.ts",
        "namespace Underscore { export interface Iterator<T, U> {} \
         export interface Static { all<T>(list: T[], iterator?: Iterator<T, boolean>): T; } }\n\
         declare const all: Underscore.Static[\"all\"];\n",
    )]) else {
        return;
    };
    let a = program.file_index("/p/a.ts").unwrap();
    let file = FileId(a as u32);
    let ast = &program.files[a].ast;
    let checker = Checker::new(&program);
    let mut ck = Ck::new(&checker, file);
    let (decl, ann) = annotated_vars(ast)[0];
    let ty = ck.type_from_node(Loc::new(file, ann));
    let sig = ck.signatures_of(ty, ts_r_checker::members::SigKind::Call)[0];
    let kind = ts_r_checker::members::SigKind::Call;
    assert_eq!(
        ck.signature_to_string_at(sig, kind, false, Loc::new(file, decl)),
        "<T>(list: T[], iterator?: Underscore.Iterator<T, boolean> | undefined): T"
    );
    assert_eq!(
        ck.signature_to_string(sig),
        "<T>(list: T[], iterator?: Iterator<T, boolean> | undefined): T"
    );
    assert_eq!(
        ck.signature_to_string_with(
            sig,
            kind,
            true,
            ts_r_checker::PrintMode::quick_info(Loc::new(file, decl))
        ),
        "<T>(list: T[], iterator?: Underscore.Iterator<T, boolean>) => T"
    );
}

/// `PrintMode::omit_optional_undefined` prints `?` parameters and properties without the
/// `undefined` their optionality adds (tsc 7.0.2 quick info: `c?: () => string`), and
/// keeps one the annotation spells out. The default keeps it, as diagnostics do.
#[test]
fn quick_info_optional_without_undefined() {
    let Some(program) = editor_program(&[(
        "a.ts",
        "declare const f: (c?: () => string, d?: number | undefined) => void;\n\
         declare const o: { a?: number; b?: string | undefined; m?(): void };\n",
    )]) else {
        return;
    };
    let a = program.file_index("/p/a.ts").unwrap();
    let file = FileId(a as u32);
    let ast = &program.files[a].ast;
    let checker = Checker::new(&program);
    let mut ck = Ck::new(&checker, file);
    let mode = ts_r_checker::PrintMode {
        omit_optional_undefined: true,
        ..Default::default()
    };
    let mut quick = Vec::new();
    let mut plain = Vec::new();
    for (_, ann) in annotated_vars(ast) {
        let ty = ck.type_from_node(Loc::new(file, ann));
        quick.push(ck.type_to_string_with(ty, mode));
        plain.push(ck.type_to_string(ty));
    }
    assert_eq!(
        quick,
        [
            "(c?: () => string, d?: number | undefined) => void",
            "{ a?: number; b?: string | undefined; m?(): void; }",
        ]
    );
    assert_eq!(
        plain,
        [
            "(c?: (() => string) | undefined, d?: number | undefined) => void",
            "{ a?: number | undefined; b?: string | undefined; m?(): void; }",
        ]
    );
}

/// `PrintMode::via_aliases` names a type that is not in scope by its own name through
/// the import alias that reaches it or its namespace (tsc 7.0.2 quick info:
/// `a<a<string>>` for `import a = require('./a')` of `export = Foo`, `d.C<number>` for
/// `import d = M.N`). `type_to_string_at` is unchanged.
#[test]
fn quick_info_names_through_import_aliases() {
    let Some(program) = editor_program(&[
        ("a.ts", "interface Foo<T> { a: string }\nexport = Foo;\n"),
        (
            "b.ts",
            "import a = require('./a');\nexport var x: a<a<string>>;\n\
             namespace M { export namespace N { export class C<T> { t!: T } export class D {} } }\n\
             import d = M.N;\nvar aa: d.C<number>;\nvar bb: d.D;\nvar cc: M.N.D;\n",
        ),
    ]) else {
        return;
    };
    let b = program.file_index("/p/b.ts").unwrap();
    let file = FileId(b as u32);
    let ast = &program.files[b].ast;
    let checker = Checker::new(&program);
    let mut ck = Ck::new(&checker, file);
    let mut quick = Vec::new();
    let mut at = Vec::new();
    for (decl, ann) in annotated_vars(ast) {
        let ty = ck.type_from_node(Loc::new(file, ann));
        let scope = Loc::new(file, decl);
        quick.push(ck.type_to_string_with(ty, ts_r_checker::PrintMode::quick_info(scope)));
        at.push(ck.type_to_string_at(ty, scope));
    }
    assert_eq!(quick, ["a<a<string>>", "d.C<number>", "d.D", "d.D"]);
    assert_eq!(at, ["Foo<Foo<string>>", "M.N.C<number>", "M.N.D", "M.N.D"]);
}
