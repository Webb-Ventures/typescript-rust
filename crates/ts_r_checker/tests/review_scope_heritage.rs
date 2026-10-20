use ts_r_checker::{CheckRun, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

fn check(src: &str) -> Vec<u32> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"target":"es2022","noLib":true},"files":["a.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    // `noLib`: the missing core global types (TS2318) are not what these tests check.
    result
        .diagnostics
        .iter()
        .map(|d| d.code)
        .filter(|&c| c != 2318)
        .collect()
}

#[test]
fn nested_enum_can_read_an_enclosing_namespace_alias() {
    let out = check(
        "enum Values { One = 1 }\n\
         enum Outer { A }\n\
         namespace Outer {\n\
             export import V = Values;\n\
             export enum Inner { B = V.One }\n\
         }",
    );
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn enum_cannot_read_its_own_merged_namespace_exports() {
    let out = check(
        "namespace E { export const value = 1; }\n\
         enum E { A = value }",
    );
    assert_eq!(out, vec![2304]);
}

#[test]
fn wide_acyclic_interface_bases_do_not_report_a_cycle() {
    let mut src = String::new();
    for i in 0..257 {
        src.push_str(&format!("interface Base{i} {{}}\n"));
    }
    src.push_str("interface Wide extends ");
    for i in 0..257 {
        if i > 0 {
            src.push_str(", ");
        }
        src.push_str(&format!("Base{i}"));
    }
    src.push_str(" {}\n");
    let out = check(&src);
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn circular_interface_bases_still_report_cycles() {
    let out = check("interface A extends B {}\ninterface B extends A {}");
    assert_eq!(out, vec![2310, 2310]);
}
