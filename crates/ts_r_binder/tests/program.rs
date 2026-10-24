//! Program-level linking: global merging across files, module augmentation, export
//! resolution through re-exports, and the cross-file diagnostics. Expected diagnostics were
//! taken from tsc 7.0.2 on the same files.

ts_r_testing::cap_memory!();

use ts_r_ast::{Ast, SourceKind};
use ts_r_binder::program::{ModuleTarget, Program, ProgramFile};
use ts_r_binder::{BindOptions, BoundFile, ModuleRequest, bind};
use ts_r_diagnostics::FileId;
use ts_r_span::LineIndex;

struct Files {
    names: Vec<String>,
    srcs: Vec<String>,
    asts: Vec<Ast>,
    bound: Vec<BoundFile>,
}

fn build(files: &[(&str, &str)]) -> Files {
    let mut f = Files {
        names: Vec::new(),
        srcs: Vec::new(),
        asts: Vec::new(),
        bound: Vec::new(),
    };
    for &(name, src) in files {
        let p = ts_r_parser::parse(src, SourceKind::from_path(name));
        let b = bind(&p.ast, src, &BindOptions::default());
        f.names.push(name.to_owned());
        f.srcs.push(src.to_owned());
        f.asts.push(p.ast);
        f.bound.push(b);
    }
    f
}

/// Resolves `./x` against the file list (with `.ts` / `.d.ts` / `/index.ts`).
fn link(f: &Files) -> Program<'_> {
    let files: Vec<ProgramFile> = (0..f.names.len())
        .map(|i| ProgramFile {
            path: &f.names[i],
            ast: &f.asts[i],
            src: &f.srcs[i],
            bound: &f.bound[i],
        })
        .collect();
    let names = f.names.clone();
    Program::link(
        files,
        move |spec: &str, _from: &str, _req: &ModuleRequest| {
            let base = spec.strip_prefix("./")?;
            [".ts", ".d.ts", "/index.ts"]
                .iter()
                .map(|ext| format!("{base}{ext}"))
                .find(|c| names.contains(c))
        },
    )
}

fn diags(f: &Files, p: &Program) -> Vec<String> {
    let fmt = |i: usize, start: u32, code: u32| {
        let lc = LineIndex::new(&f.srcs[i]).line_col(start);
        format!("{}({},{}) TS{}", f.names[i], lc.line + 1, lc.col + 1, code)
    };
    let mut out: Vec<String> = p
        .diagnostics
        .iter()
        .map(|d| fmt(d.file.0 as usize, d.span.start, d.code))
        .collect();
    for (i, b) in f.bound.iter().enumerate() {
        out.extend(b.diagnostics.iter().map(|d| fmt(i, d.span.start, d.code)));
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn script_globals_merge_and_conflict_across_files() {
    let f = build(&[
        (
            "a.ts",
            "interface Shared { a: number }\nvar v = 1;\nlet l = 1;\nenum E { A }\n",
        ),
        (
            "b.ts",
            "interface Shared { b: number }\nvar v = 2;\nvar l = 2;\nclass E {}\n",
        ),
    ]);
    let p = link(&f);
    let shared = p.global("Shared").expect("merged interface");
    assert_eq!(shared.symbols.len(), 2);
    assert_eq!(p.global("v").expect("var").symbols.len(), 2);
    assert_eq!(
        diags(&f, &p),
        [
            "a.ts(3,5) TS2451",
            "a.ts(4,6) TS2567",
            "b.ts(3,5) TS2451",
            "b.ts(4,7) TS2567",
        ]
    );
}

/// tsc 7 (`noSymbolForMergeCrash`): a global an interface and a non-instantiated namespace
/// declare, then a type alias in a later file, is TS2649 at the alias only; the other order
/// is an ordinary duplicate.
#[test]
fn conflict_with_a_namespace_only_global_is_ts2649() {
    let f = build(&[
        ("initial.ts", "interface A { }\nnamespace A {}\n"),
        ("final.ts", "type A = {}\n"),
    ]);
    let p = link(&f);
    assert_eq!(diags(&f, &p), ["final.ts(1,6) TS2649"]);
    assert_eq!(p.global("A").expect("merged").symbols.len(), 1);
    let f = build(&[
        ("first.ts", "type A = {}\n"),
        ("second.ts", "interface A { }\nnamespace A {}\n"),
    ]);
    let p = link(&f);
    assert_eq!(
        diags(&f, &p),
        [
            "first.ts(1,6) TS2300",
            "second.ts(1,11) TS2300",
            "second.ts(2,11) TS2300"
        ]
    );
}

#[test]
fn modules_do_not_contribute_globals_but_declare_global_does() {
    let f = build(&[
        (
            "a.ts",
            "export const x = 1;\ndeclare global { var injected: number; }\n",
        ),
        ("b.ts", "var injected = 2;\nvar x = 3;\n"),
    ]);
    let p = link(&f);
    assert!(p.global("x").is_some_and(|m| m.symbols.len() == 1));
    assert_eq!(p.global("injected").expect("augmented").symbols.len(), 2);
    assert!(diags(&f, &p).is_empty());
}

#[test]
fn exports_resolve_through_reexports_and_stars() {
    let f = build(&[
        (
            "main.ts",
            "import { a, b, c } from \"./mid\";\nexport { a, b, c };\n",
        ),
        (
            "mid.ts",
            "export * from \"./leaf\";\nexport { y as b } from \"./leaf\";\nexport const c = 1;\n",
        ),
        (
            "leaf.ts",
            "export const a = 1;\nexport const y = 2;\nexport default 3;\n",
        ),
    ]);
    let p = link(&f);
    let mid = p.module_target(FileId(0), 0);
    assert_eq!(mid, ModuleTarget::File(FileId(1)));
    let leaf = FileId(2);
    let a = p.resolve_export(mid, "a").expect("a via export *");
    assert_eq!(p.resolve_alias(a).unwrap_or(a).file, leaf);
    let b = p.resolve_export(mid, "b").expect("b via rename");
    assert_eq!(p.resolve_alias(b).unwrap_or(b).file, leaf);
    assert!(
        p.resolve_export(mid, "y").is_some(),
        "export * re-exports y"
    );
    assert!(
        p.resolve_export(mid, "default").is_none(),
        "export * skips default"
    );
    let mut names: Vec<String> = p
        .export_names(mid)
        .into_iter()
        .map(|(n, _)| n.into())
        .collect();
    names.sort();
    assert_eq!(names, ["a", "b", "c", "y"]);
}

#[test]
fn export_star_cycles_terminate() {
    let f = build(&[
        ("a.ts", "export * from \"./b\";\nexport const a = 1;\n"),
        ("b.ts", "export * from \"./a\";\nexport const b = 1;\n"),
    ]);
    let p = link(&f);
    let t = ModuleTarget::File(FileId(0));
    assert!(p.resolve_export(t, "b").is_some());
    assert!(p.resolve_export(t, "missing").is_none());
}

/// A name re-exported only through `export type *` is type-only; one plain `export *` of the
/// same name (even from the same module) makes it a value.
#[test]
fn type_only_export_stars() {
    let f = build(&[
        ("a.ts", "export class A {}\nexport class B {}\n"),
        (
            "b.ts",
            "export type * from \"./a\";\nexport { B } from \"./a\";\n",
        ),
        ("c.ts", "export * from \"./b\";\n"),
        (
            "d.ts",
            "export type * from \"./a\";\nexport * from \"./a\";\n",
        ),
    ]);
    let p = link(&f);
    let (b, c, d) = (
        ModuleTarget::File(FileId(1)),
        ModuleTarget::File(FileId(2)),
        ModuleTarget::File(FileId(3)),
    );
    assert!(p.star_export_is_type_only(b, "A"));
    assert!(!p.star_export_is_type_only(b, "B"));
    assert!(p.star_export_is_type_only(c, "A"));
    assert!(!p.star_export_is_type_only(d, "A"));
    assert!(!p.star_export_is_type_only(b, "missing"));
}

/// A chain of `export *` far deeper than any real program stops at a depth limit instead of
/// overflowing the stack, while a wide barrel still re-exports every module it reaches.
#[test]
fn deep_export_star_chains_stop_and_wide_barrels_resolve() {
    let n = 20_000;
    let chain: String = (0..n)
        .map(|i| {
            format!(
                "declare module \"m{i}\" {{ export * from \"m{}\"; }}\n",
                i + 1
            )
        })
        .collect();
    let wide = 300;
    let barrel = format!(
        "declare module \"barrel\" {{ {} }}\n{}",
        (0..wide)
            .map(|i| format!("export * from \"w{i}\";"))
            .collect::<String>(),
        (0..wide)
            .map(|i| format!("declare module \"w{i}\" {{ export const v{i}: number; }}\n"))
            .collect::<String>()
    );
    // Linking looks an augmented name up through the chain.
    let aug = "export {};\ndeclare module \"m0\" { export const deep: number; }\n";
    let f = build(&[
        ("chain.d.ts", &chain),
        ("barrel.d.ts", &barrel),
        ("aug.ts", aug),
    ]);
    let p = link(&f);
    let target = |spec: &str| {
        p.ambient_modules
            .iter()
            .position(|(name, _)| &**name == spec)
            .map(|i| ModuleTarget::Ambient(i as u32))
            .expect(spec)
    };
    let m0 = target("m0");
    assert!(p.resolve_export(m0, "missing").is_none());
    let names = p.export_names(m0);
    assert_eq!(names.len(), 1);
    assert_eq!(&*names[0].0, "deep");
    let b = target("barrel");
    assert!(p.resolve_export(b, &format!("v{}", wide - 1)).is_some());
    assert_eq!(p.export_names(b).len(), wide);
}

/// Aliases whose target goes back through themselves (directly, through an entity name, or
/// through the module's own `export =`) resolve to `None` instead of recursing forever.
#[test]
fn circular_aliases_terminate() {
    let f = build(&[
        ("a.ts", "export import A = A.x;\n"),
        ("b.ts", "import { foo } from \"./b\";\nexport = foo;\n"),
    ]);
    let p = link(&f);
    let a = p
        .resolve_export(ModuleTarget::File(FileId(0)), "A")
        .expect("A is exported");
    assert_eq!(p.resolve_alias(a), None);
    assert_eq!(p.resolve_export(ModuleTarget::File(FileId(1)), "foo"), None);
}

/// A 200k-segment `export default` entity name resolves without recursing per segment.
#[test]
fn long_entity_alias_chain_terminates() {
    let src = format!(
        "const x = {{}};\nexport default x{};\n",
        ".a".repeat(200_000)
    );
    let f = build(&[("a.ts", &src)]);
    let p = link(&f);
    let d = p
        .resolve_export(ModuleTarget::File(FileId(0)), "default")
        .expect("default");
    assert_eq!(p.resolve_alias(d), None);
}

#[test]
fn module_augmentation_merges_and_unresolved_is_reported() {
    let f = build(&[
        (
            "base.ts",
            "export interface Base { a: number }\nexport class K {}\n",
        ),
        (
            "aug.ts",
            "import { Base } from \"./base\";\ndeclare module \"./base\" {\n  interface Base { b: number }\n  let K: number;\n}\ndeclare module \"./nowhere\" {}\nexport {};\n",
        ),
    ]);
    let p = link(&f);
    assert!(
        p.augmentations
            .iter()
            .any(|&(_, t)| t == ModuleTarget::File(FileId(0)))
    );
    // `let K` vs the class K: TS2451 on both; the missing module: TS2664.
    assert_eq!(
        diags(&f, &p),
        [
            "aug.ts(4,7) TS2451",
            "aug.ts(6,16) TS2664",
            "base.ts(2,14) TS2451"
        ]
    );
}

#[test]
fn value_augmentation_of_a_variable_with_a_type_only_namespace_is_ts2649() {
    // tsc 7: augmentExportEquals7. A type-only augmentation of the same entity is fine.
    let lib = "declare var lib: () => void;\ndeclare namespace lib {}\nexport = lib;\n";
    let f = build(&[
        ("lib.d.ts", lib),
        (
            "ext.ts",
            "import * as lib from \"./lib\";\ndeclare module \"./lib\" {\n    export function fn(): void;\n}\n",
        ),
    ]);
    let p = link(&f);
    assert_eq!(diags(&f, &p), ["ext.ts(2,16) TS2649"]);
    let f = build(&[
        ("lib.d.ts", lib),
        (
            "ext.ts",
            "import * as lib from \"./lib\";\ndeclare module \"./lib\" {\n    export interface I {}\n}\n",
        ),
    ]);
    let p = link(&f);
    assert!(diags(&f, &p).is_empty());
}

#[test]
fn augmenting_a_module_found_outside_the_program_is_not_ts2664() {
    // tsc 7: untypedModuleImport_withAugmentation (the checker reports TS2665 instead).
    let f = build(&[(
        "a.ts",
        "declare module \"foo\" {\n    export const x: number;\n}\nimport { x } from \"foo\";\n",
    )]);
    let files: Vec<ProgramFile> = vec![ProgramFile {
        path: &f.names[0],
        ast: &f.asts[0],
        src: &f.srcs[0],
        bound: &f.bound[0],
    }];
    let outside = Program::link(files, |spec: &str, _: &str, _: &ModuleRequest| {
        (spec == "foo").then(|| "/node_modules/foo/index.js".to_owned())
    });
    assert!(diags(&f, &outside).is_empty());
    assert_eq!(diags(&f, &link(&f)), ["a.ts(1,16) TS2664"]);
}

#[test]
fn augmenting_an_export_assigned_expression_is_ts2671() {
    // Observed with tsc 7: `export =` of an object literal, a number or a class or function
    // expression (and JavaScript's `module.exports = {...}`) cannot be augmented.
    let f = build(&[
        ("lit.ts", "class A {}\nexport = { A };\n"),
        ("num.ts", "export = 42;\n"),
        (
            "ns.ts",
            "namespace N { export const a = 1; }\nexport = N;\n",
        ),
        (
            "i.ts",
            "import q = require(\"./lit\");\nimport n = require(\"./num\");\nimport s = require(\"./ns\");\ndeclare module \"./lit\" {\n  interface A { b: string }\n}\ndeclare module \"./num\" {\n  interface Q {}\n}\ndeclare module \"./ns\" {\n  interface Q {}\n}\n",
        ),
    ]);
    let p = link(&f);
    assert_eq!(diags(&f, &p), ["i.ts(4,16) TS2671", "i.ts(7,16) TS2671"]);
}

#[test]
fn ambient_modules_merge_and_patterns_match() {
    let f = build(&[
        (
            "a.d.ts",
            "declare module \"lib\" { export const x: number; }\ndeclare module \"img/*\" { const v: string; export default v; }\n",
        ),
        (
            "b.d.ts",
            "declare module \"lib\" { export const y: number; }\n",
        ),
        (
            "main.ts",
            "import { x, y } from \"lib\";\nimport pic from \"img/cat\";\nexport { x, y, pic };\n",
        ),
    ]);
    let p = link(&f);
    let lib = p.module_target(FileId(2), 0);
    assert!(matches!(lib, ModuleTarget::Ambient(_)));
    assert!(p.resolve_export(lib, "x").is_some());
    assert!(p.resolve_export(lib, "y").is_some());
    assert!(matches!(
        p.module_target(FileId(2), 1),
        ModuleTarget::Ambient(_)
    ));
    assert!(diags(&f, &p).is_empty());
}

#[test]
fn builtin_global_names() {
    let f = build(&[
        ("a.ts", "var globalThis = 1;\nfunction undefined() {}\n"),
        (
            "m.ts",
            "export {};\ndeclare global { var globalThis: unknown; }\n",
        ),
    ]);
    let p = link(&f);
    assert_eq!(diags(&f, &p), ["a.ts(1,5) TS2397", "a.ts(2,10) TS2397"]);
}

#[test]
fn umd_global_merges_with_script_namespace() {
    let f = build(&[
        (
            "lib.d.ts",
            "export declare const v: number;\nexport as namespace Lib;\n",
        ),
        ("s.ts", "declare namespace Lib { const w: number; }\n"),
    ]);
    let p = link(&f);
    let lib = p.global("Lib").expect("UMD global");
    assert_eq!(lib.symbols.len(), 2);
    assert!(diags(&f, &p).is_empty());
}
