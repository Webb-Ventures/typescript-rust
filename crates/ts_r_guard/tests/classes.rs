//! User classes (`instanceof`, record 0007 open question 13) and refinements of branded types
//! (`[sound.guard] refinements`, open question 14) in expanded code and shared modules.
//! Node runs them end to end in `ts_r_run/tests/guard.rs` and `ts_r_build/tests/guard.rs`
//! (fixture `tests/fixtures/app`).

use std::sync::Arc;

use ts_r_config::{FileSystem, MemoryFs};
use ts_r_guard::vfs::GuardFs;
use ts_r_guard::{ExpandOptions, Expander, GuardOptions, Layout, Refinement};

ts_r_testing::cap_memory!();

const MODELS: &str = "export class Point { constructor(public x: number, public y: number) {} }\n\
export abstract class Shape {}\n\
export class Box<T> { constructor(public v: T) {} }\n\
declare class Ghost {}\n\
export { Ghost };\n\
export default class Anon {}\n\
export type Email = string & { readonly __brand: \"Email\" };\n\
export type Tag = string & { readonly __tag: true };\n\
export type Port = number;\n";

const MAIN: &str = "import { parse, is } from \"@ts-r.dev/guard\";\n\
import { Point, Shape, Box, Ghost, type Email, type Tag, type Port } from \"./models\";\n\
import type { Point as TypePoint } from \"./models\";\n\
import Anon from \"./models\";\n\
class Hidden {}\n\
export class Here { n = 1 }\n\
interface Holder { at: Point; here: Here; email: Email }\n\
export const a = (x: unknown) => parse<Point>(x);\n\
export const b = (x: unknown) => parse<Holder>(x);\n\
export const c = (x: unknown) => is<Here | null>(x);\n\
export const d = (x: unknown) => parse<Anon>(x);\n\
export const e = (x: unknown) => parse<Email>(x);\n\
export const f = (x: unknown) => parse<Port>(x);\n\
export const g1 = (x: unknown) => parse<Shape>(x);\n\
export const g2 = (x: unknown) => parse<Box<number>>(x);\n\
export const g3 = (x: unknown) => parse<TypePoint>(x);\n\
export const g4 = (x: unknown) => parse<Ghost>(x);\n\
export const g5 = (x: unknown) => parse<Hidden>(x);\n\
export const g6 = (x: unknown) => parse<Tag>(x);\n\
export const g7 = (x: unknown) => parse<string & { b: 1 }>(x);\n";

fn fs() -> Arc<MemoryFs> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#);
    fs.add_file("/p/models.ts", MODELS);
    fs.add_file("/p/src/main.ts", MAIN);
    Arc::new(fs)
}

fn guard() -> GuardOptions {
    GuardOptions {
        refinements: vec![
            Refinement {
                name: "Email".into(),
                module: "/p/lib/validators".into(),
                export: "isEmail".into(),
            },
            Refinement {
                name: "Port".into(),
                module: "port-checks".into(),
                export: "default".into(),
            },
        ],
        ..GuardOptions::default()
    }
}

fn main_src() -> String {
    MAIN.replace("\"./models\"", "\"../models\"")
}

#[test]
fn classes_and_refinements_expand_inline() {
    let ex = Expander::new(
        fs(),
        ExpandOptions {
            guard: guard(),
            layout: Layout::Inline,
        },
    );
    let src = main_src();
    let x = ex.expand("/p/src/main.ts", &src).expect("expanded");
    let head = x.code.lines().next().unwrap_or("");
    // Imported classes keep the user's specifier, re-based onto the file; a class declared in
    // the file is used by its own name.
    assert!(
        head.starts_with("import { Point as $c_Point } from \"../models\";"),
        "{head}"
    );
    assert!(head.contains("import { default as $c_Anon } from \"../models\";"));
    assert!(head.contains("v instanceof $c_Point"), "{head}");
    assert!(head.contains("instanceof Here"), "{head}");
    assert!(!head.contains("$c_Here"), "{head}");
    assert!(head.contains("[$C, \"Point\", $c_Point]"), "{head}");
    // Refinements: the base type, then the predicate; paths relative to the file.
    assert!(
        head.contains("import { isEmail as $r_Email } from \"../lib/validators\";"),
        "{head}"
    );
    assert!(head.contains("import { default as $r_Port } from \"port-checks\";"));
    assert!(
        head.contains("typeof v === \"string\" && $r_Email(v)"),
        "{head}"
    );
    assert!(
        head.contains("[$F, \"string\", $r_Email, \"Email\"]"),
        "{head}"
    );
    assert!(head.contains("const $F = "), "the runtime part is shipped");
    let got: Vec<String> = x.errors.iter().map(|e| e.render("main.ts")).collect();
    assert_eq!(
        got,
        [
            "main.ts(14,41): error TSR1501: Type 'Shape' cannot be checked at runtime: 'Shape' is an abstract class; validate one of its concrete subclasses (or a union of them) instead.",
            "main.ts(15,41): error TSR1501: Type 'Box<number>' cannot be checked at runtime: 'Box' is a generic class; 'instanceof' can't check its type arguments.",
            "main.ts(16,41): error TSR1501: Type 'TypePoint' cannot be checked at runtime: class 'Point' is only imported as a type ('import type' or 'export type'), so the validator has no value to check 'instanceof' against; import it as a value.",
            "main.ts(17,41): error TSR1501: Type 'Ghost' cannot be checked at runtime: class 'Ghost' is declared with 'declare class', which has no value at runtime.",
            "main.ts(18,41): error TSR1501: Type 'Hidden' cannot be checked at runtime: class 'Hidden' is not exported from its module; export it so the validator can import it.",
            "main.ts(19,41): error TSR1501: Type 'Tag' cannot be checked at runtime: 'Tag' is a branded type; register a predicate for it in '[sound.guard] refinements' (for example Tag = \"./validators#isTag\").",
            "main.ts(20,41): error TSR1501: Type 'string & { b: 1 }' cannot be checked at runtime: a primitive intersected with an object type is a branded type; name it with a type alias and register a predicate for it in '[sound.guard] refinements'.",
        ]
    );
    // Lines are kept and the output still parses.
    assert_eq!(x.code.lines().count(), src.lines().count());
    let parsed = ts_r_parser::parse(&x.code, ts_r_ast::SourceKind::Ts);
    assert!(
        parsed.diagnostics.iter().all(|d| !d.is_error()),
        "{:?}",
        parsed.diagnostics
    );
}

#[test]
fn without_a_refinement_a_brand_is_tsr1501() {
    let ex = Expander::new(
        fs(),
        ExpandOptions {
            guard: GuardOptions::default(),
            layout: Layout::Inline,
        },
    );
    let x = ex.expand("/p/src/main.ts", &main_src()).expect("expanded");
    assert!(
        x.errors.iter().any(|e| e.line == 12
            && e.message
                .contains("'Email' is a branded type; register a predicate")),
        "{:?}",
        x.errors
    );
    // An alias without an intersection is just its type.
    assert!(!x.errors.iter().any(|e| e.line == 13), "{:?}", x.errors);
}

#[test]
fn shared_modules_import_classes_and_predicates_by_absolute_path() {
    let mut mem = MemoryFs::new();
    mem.set_cwd("/p");
    mem.add_file("/p/tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#);
    mem.add_file("/p/models.ts", MODELS);
    let main = "import { parse } from \"@ts-r.dev/guard\";\n\
import { Point, type Email } from \"./models\";\n\
export class Here { n = 1 }\n\
export interface Holder { at: Point; here: Here; email: Email }\n\
export const a = (x: unknown) => parse<Holder>(x);\n\
export const b = (x: unknown) => parse<Point | Here>(x);\n";
    mem.add_file("/p/main.ts", main);
    let gfs = GuardFs::new(Arc::new(mem), "/p", guard(), true);
    let code = gfs.read_file("/p/main.ts").expect("main");
    assert!(gfs.errors_in(&gfs.touched()).is_empty());
    // The file's own entry: the imported class by absolute path, its own class by name.
    assert!(
        code.contains("import { Point as $c_Point } from \"/p/models.ts\";"),
        "{code}"
    );
    assert!(code.contains("instanceof Here"), "{code}");
    // A declaration module imports what its body uses.
    let holder = gfs
        .read_file("/p/.ts-r-guard/main.ts/Holder.js")
        .expect("Holder.js");
    assert!(
        holder.contains("import { Point as $c_Point } from \"/p/models.ts\";"),
        "{holder}"
    );
    assert!(
        holder.contains("import { Here as $c_Here } from \"/p/main.ts\";"),
        "{holder}"
    );
    let email = gfs
        .read_file("/p/.ts-r-guard/models.ts/Email.js")
        .expect("Email.js");
    assert!(
        email.contains("import { isEmail as $r_Email } from \"/p/lib/validators\";"),
        "{email}"
    );
    assert!(email.contains("$F"), "{email}");
}

#[test]
fn a_refined_declaration_imports_what_its_base_uses() {
    let mut mem = MemoryFs::new();
    mem.set_cwd("/p");
    mem.add_file("/p/tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#);
    mem.add_file(
        "/p/models.ts",
        "export type Base = { x: number };\nexport type Refined = Base;\n",
    );
    mem.add_file(
        "/p/main.ts",
        "import { parse } from \"@ts-r.dev/guard\";\n\
import type { Refined } from \"./models\";\n\
export const a = (x: unknown) => parse<Refined>(x);\n",
    );
    let opts = GuardOptions {
        refinements: vec![Refinement {
            name: "Refined".into(),
            module: "/p/lib/validators".into(),
            export: "isRefined".into(),
        }],
        ..GuardOptions::default()
    };
    let gfs = GuardFs::new(Arc::new(mem), "/p", opts, true);
    gfs.read_file("/p/main.ts").expect("main");
    assert!(gfs.errors_in(&gfs.touched()).is_empty());
    let refined = gfs
        .read_file("/p/.ts-r-guard/models.ts/Refined.js")
        .expect("Refined.js");
    // The refinement's base is a declaration of its own: imported like any other.
    assert!(
        refined.contains(
            "import { $is as $is_Base, $d as $d_Base } from \"/p/.ts-r-guard/models.ts/Base.js\";"
        ),
        "{refined}"
    );
}
