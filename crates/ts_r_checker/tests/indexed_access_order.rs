//! Indexed-access aliases with shared operands sort independently of worker order.

use ts_r_checker::{CheckRun, Checker, Ck, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

#[test]
fn indexed_access_alias_order_ignores_creation_order() {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/lib.d.ts",
        "interface Array<T> {}\ninterface Boolean {}\ninterface Function {}\n\
         interface CallableFunction {}\ninterface NewableFunction {}\n\
         interface IArguments {}\ninterface Number {}\ninterface Object {}\n\
         interface RegExp {}\ninterface String {}\n",
    );
    fs.add_file(
        "/p/shared.d.ts",
        "export type A<T> = T[keyof T];\n\
         export type B<T> = T[keyof T];\n\
         export interface Shared<T> { a: A<T>; b: B<T>; both: A<T> | B<T> }\n",
    );
    fs.add_file(
        "/p/a.ts",
        "import { Shared } from './shared';\n\
         export function a<T>(x: Shared<T>) { return x.a; }\n",
    );
    fs.add_file(
        "/p/b.ts",
        "import { Shared } from './shared';\n\
         export function b<T>(x: Shared<T>) { return x.b; }\n",
    );
    fs.add_file(
        "/p/c.ts",
        "import { Shared } from './shared';\n\
         export function c<T>(x: Shared<T>): number { return x.both; }\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "skipLibCheck": true, "target": "es2022", "noLib": true}, "files": ["lib.d.ts", "a.ts", "b.ts", "c.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let a = FileId(program.file_index("/p/a.ts").unwrap() as u32);
    let b = FileId(program.file_index("/p/b.ts").unwrap() as u32);
    let c = FileId(program.file_index("/p/c.ts").unwrap() as u32);
    // tsc 7.0.2, --strict --skipLibCheck --target es2022 --lib es2022.
    let expected = concat!(
        "Type 'A<T> | B<T>' is not assignable to type 'number'.\n",
        "  Type 'A<T>' is not assignable to type 'number'.\n",
        "    Type 'T[string] | T[number] | T[symbol]' is not assignable to type 'number'.\n",
        "      Type 'T[string]' is not assignable to type 'number'.",
    );
    for order in [[a, b], [b, a]] {
        let checker = Checker::new(&program);
        for file in order {
            assert!(Ck::new(&checker, file).check_source_file(file).is_empty());
        }
        let diagnostics = Ck::new(&checker, c).check_source_file(c);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, 2322);
        assert_eq!(diagnostics[0].message, expected, "order: {order:?}");
    }
    for threads in [1, 4] {
        let result = check_program(&program, CheckRun { threads });
        assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
        assert_eq!(result.diagnostics[0].code, 2322);
        assert_eq!(result.diagnostics[0].message, expected);
    }
}
