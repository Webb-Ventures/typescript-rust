//! `parse<T>` call-site expansion and the bundler's virtual validator modules, on an
//! in-memory project. End-to-end runs in Node are in `ts_r_run/tests/guard.rs` and
//! `ts_r_build/tests/guard.rs` (fixture `tests/fixtures/app`).

use std::sync::Arc;

use ts_r_config::{FileSystem, MemoryFs};
use ts_r_guard::vfs::GuardFs;
use ts_r_guard::{ExpandOptions, Expander, Explain, GuardOptions, Layout};

ts_r_testing::cap_memory!();

const TYPES: &str = "export interface User { id: number; name: string; tags: string[]; manager: User | null }\n\
export type Role = \"admin\" | \"member\";\n\
export interface Page<T> { items: T[]; next: string | null }\n\
declare module \"ext\" { interface Ext { a: number } }\n";

const MAIN: &str = "#!/usr/bin/env node\n\
\"use strict\";\n\
import { parse, is as isA, ParseError, expect } from \"@ts-r.dev/guard\";\n\
import * as g from \"@ts-r.dev/guard\";\n\
import type { User, Page, Role } from \"./types\";\n\
export const a = (x: unknown) => parse<User>(x);\n\
export const b = (x: unknown) => isA<Page<User>>(x) && isA<Page<User>>(x);\n\
export const c = [1].map(parse<Role>);\n\
export const d = (x: unknown) => g.parse<{ n: number }>(x);\n\
export const e = (x: unknown) => x instanceof ParseError || x instanceof g.ParseError;\n\
export const f = (x: string | undefined) => expect(x, \"x\");\n";

fn fs() -> Arc<MemoryFs> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#);
    fs.add_file("/p/types.ts", TYPES);
    fs.add_file("/p/main.ts", MAIN);
    fs.add_file("/p/plain.ts", "export const x = 1;\n");
    Arc::new(fs)
}

fn expander(layout: Layout) -> Expander {
    Expander::new(
        fs(),
        ExpandOptions {
            guard: GuardOptions::default(),
            layout,
        },
    )
}

#[test]
fn inline_expansion_keeps_lines_and_rewrites_call_sites() {
    let x = expander(Layout::Inline)
        .expand("/p/main.ts", MAIN)
        .expect("expanded");
    assert!(x.errors.is_empty(), "{:?}", x.errors);
    assert_eq!(x.code.lines().count(), MAIN.lines().count());
    let lines: Vec<&str> = x.code.lines().collect();
    // The shebang stays first; the generated code follows the directive on its line.
    assert_eq!(lines[0], "#!/usr/bin/env node");
    assert!(lines[1].starts_with("\"use strict\"; "), "{}", lines[1]);
    assert!(lines[1].contains("function $parse("), "{}", lines[1]);
    assert!(lines[1].contains("const ParseError = $ParseError();"));
    assert!(!lines[1].contains("const isA"));
    assert!(lines[1].contains("const g = { ParseError: $ParseError"));
    assert!(lines[1].contains("const expect = $expect;"));
    // The imports are gone (their lines are blank).
    assert_eq!(lines[2], "");
    assert_eq!(lines[3], "");
    assert!(!x.code.contains("@ts-r.dev/guard"));
    // Call sites, instantiation expressions and namespace calls.
    assert!(lines[5].ends_with("=> $f$parse$e0(x);"), "{}", lines[5]);
    assert!(
        lines[6].contains("$f$is$e1(x) && $f$is$e1(x)"),
        "{}",
        lines[6]
    );
    assert!(lines[7].ends_with("[1].map($f$parse$e2);"), "{}", lines[7]);
    assert!(lines[8].ends_with("=> $f$parse$e3(x);"), "{}", lines[8]);
    // One validator per type: `User` is a plain reference, `Page<User>` an entry.
    assert_eq!(x.types, ["User", "Page<User>", "Role", "{ n: number }"]);
    assert!(
        lines[1].contains("const $f$parse$e0 = (v) => $parse(v, $is_User, $d_User, \"User\");")
    );
    assert!(lines[1].contains("const $f$is$e1 = (v) => $is$e1(v, 0);"));
    assert!(lines[1].contains("const $is$e1 = (v, d) =>"));
    assert_eq!(x.deps, ["/p/types.ts"]);
    // Still valid TypeScript.
    let parsed = ts_r_parser::parse(&x.code, ts_r_ast::SourceKind::Ts);
    assert!(
        parsed.diagnostics.iter().all(|d| !d.is_error()),
        "{:?}",
        parsed.diagnostics
    );
    // Deterministic.
    let y = expander(Layout::Inline)
        .expand("/p/main.ts", MAIN)
        .expect("expanded");
    assert_eq!(x.code, y.code);
}

#[test]
fn files_without_guard_imports_are_untouched() {
    let e = expander(Layout::Inline);
    assert!(e.expand("/p/plain.ts", "export const x = 1;\n").is_none());
    let only_types = "import type { Guard } from \"@ts-r.dev/guard\";\nexport let g: Guard<number> | undefined;\n";
    assert!(e.expand("/p/plain.ts", only_types).is_none());
    assert!(e.expand("/p/types.d.ts", MAIN).is_none());
    assert!(e.expand("/p/main.js", MAIN).is_none());
}

#[test]
fn request_and_command_line_forms_expand() {
    let src = "import { body, query, args } from \"@ts-r.dev/guard\";\n\
import type { User } from \"./types\";\n\
export const a = (req: { body: unknown }) => body<User>(req);\n\
export const b = (req: { url: string }) => query<{ q: string; n?: number }>(req);\n\
export const c = () => args<{ port: number; verbose: boolean }>();\n\
// Property keys and labels named like a form are not uses of it.\n\
export const d = { body: 1, query() {}, args: 2 };\n\
body: for (;;) break body;\n";
    let x = expander(Layout::Inline)
        .expand("/p/req.ts", src)
        .expect("expanded");
    assert!(x.errors.is_empty(), "{:?}", x.errors);
    let head = x.code.lines().next().unwrap_or("");
    for want in [
        "(r) => $f$parse$e0(r.body)",
        "(r) => $f$parse$e1($env($query(r), $d$e1()))",
        "(a) => $f$parse$e2($args($d$e2(), a))",
        "const $query = ",
        "const $args = ",
    ] {
        assert!(head.contains(want), "{want} in {head}");
    }
    assert!(
        x.code
            .contains("export const d = { body: 1, query() {}, args: 2 };")
    );
}

fn expand_with(guard: GuardOptions, src: &str) -> ts_r_guard::Expanded {
    Expander::new(
        fs(),
        ExpandOptions {
            guard,
            layout: Layout::Inline,
        },
    )
    .expand("/p/opts.ts", src)
    .expect("expanded")
}

#[test]
fn explain_and_max_depth_options_shape_the_output() {
    let src = "import { parse, env } from \"@ts-r.dev/guard\";\n\
import type { User } from \"./types\";\n\
export const a = (x: unknown) => parse<User>(x);\n";
    let full = expand_with(GuardOptions::default(), src).code;
    assert!(
        full.contains("$parse(v, $is_User, $d_User, \"User\")"),
        "{full}"
    );
    assert!(full.contains("const $w = "), "{full}");
    let first = expand_with(
        GuardOptions {
            explain: Explain::First,
            max_depth: 64,
            ..GuardOptions::default()
        },
        src,
    )
    .code;
    assert!(
        first.contains("$parse(v, $is_User, $d_User, \"User\", 1)"),
        "{first}"
    );
    assert!(first.contains("=> d < 64 &&") && first.contains("const $MAX_DEPTH = 64;"));
    // "none": no descriptors and no walker, only the predicates.
    let none_opts = GuardOptions {
        explain: Explain::None,
        ..GuardOptions::default()
    };
    let none = expand_with(none_opts.clone(), src).code;
    assert!(none.contains("$parse0(v, $is_User, \"User\")"), "{none}");
    for absent in ["$d_User", "const $w = ", "const $O = ", "const $bad = "] {
        assert!(!none.contains(absent), "{absent} in {none}");
    }
    assert!(
        none.len() * 3 < full.len(),
        "{} vs {}",
        none.len(),
        full.len()
    );
    // A converting form still needs its descriptor.
    let with_env = format!("{src}export const e = () => env<{{ PORT: number }}>();\n");
    let none_env = expand_with(none_opts, &with_env).code;
    assert!(
        none_env.contains("$env(") && none_env.contains("const $d$e1 = "),
        "{none_env}"
    );
}

#[test]
fn validators_over_the_budget_are_tsr1504() {
    let props: String = (0..80)
        .map(|i| format!("p{i}: string | number; "))
        .collect();
    let src = format!(
        "import {{ parse }} from \"@ts-r.dev/guard\";\n\
interface Big {{ {props}}}\n\
interface Wrap {{ big: Big; small: number }}\n\
export const a = (x: unknown) => parse<Wrap>(x);\n\
export const b = (x: unknown) => parse<{{ n: number }}>(x);\n"
    );
    let x = expand_with(
        GuardOptions {
            max_validator_kb: 1,
            ..GuardOptions::default()
        },
        &src,
    );
    let got: Vec<String> = x.errors.iter().map(|e| e.render("opts.ts")).collect();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].starts_with("opts.ts(4,40): error TSR1504: The validator for 'Wrap' is ")
            && got[0].ends_with(
                " KB, over the 1 KB budget. Validate a smaller type or raise '[sound.guard] max-validator-kb'."
            ),
        "{got:?}"
    );
    // Default budget (8 KB) and 0 (off): no error.
    assert!(expand_with(GuardOptions::default(), &src).errors.is_empty());
    let off = GuardOptions {
        max_validator_kb: 0,
        ..GuardOptions::default()
    };
    assert!(expand_with(off, &src).errors.is_empty());
}

#[test]
fn errors_have_codes_and_positions() {
    let src = "import { parse, is } from \"@ts-r.dev/guard\";\n\
const a = (x: unknown) => parse<() => void>(x);\n\
const b = (x: unknown) => parse(x);\n\
const c = [1].map(is);\n\
const d = (is: (x: number) => boolean) => [1].map(is);\n";
    let x = expander(Layout::Inline)
        .expand("/p/bad.ts", src)
        .expect("expanded");
    let got: Vec<String> = x.errors.iter().map(|e| e.render("bad.ts")).collect();
    assert_eq!(
        got,
        [
            "bad.ts(2,33): error TSR1501: Type '() => void' cannot be checked at runtime: function types have no runtime shape.",
            "bad.ts(3,27): error TSR1402: 'parse' needs an explicit type argument in sound mode, e.g. 'parse<User>(...)'.",
        ],
        "a parameter named `is` shadows the import, so its uses are not errors"
    );
}

#[test]
fn shared_layout_imports_virtual_modules() {
    let fs = fs();
    let gfs = GuardFs::new(fs.clone(), "/p", GuardOptions::default(), true);
    let code = gfs.read_file("/p/main.ts").expect("main");
    assert!(gfs.errors_in(&["/p/main.ts".to_owned()]).is_empty());
    assert!(code.contains("from \"/p/.ts-r-guard/rt.js\";"), "{code}");
    assert!(
        code.contains(
            "import { $is as $is_User, $d as $d_User } from \"/p/.ts-r-guard/types.ts/User.js\";"
        ),
        "{code}"
    );
    assert!(code.contains("/p/.ts-r-guard/types.ts/Page.js"));
    assert!(
        !code.contains("function $parse("),
        "the runtime is not inlined"
    );
    // The virtual tree: listed completely, without knowing who asks.
    assert!(gfs.directory_exists("/p/.ts-r-guard"));
    let names = |p: &str| -> Vec<String> { gfs.read_dir(p).into_iter().map(|e| e.name).collect() };
    assert!(names("/p").contains(&".ts-r-guard".to_owned()));
    assert_eq!(
        names("/p/.ts-r-guard/types.ts"),
        ["Page.js", "Role.js", "User.js", "~mod~ext"]
    );
    assert_eq!(names("/p/.ts-r-guard/types.ts/~mod~ext"), ["Ext.js"]);
    assert!(gfs.file_exists("/p/.ts-r-guard/types.ts/User.js"));
    assert!(!gfs.file_exists("/p/.ts-r-guard/types.ts/Nope.js"));
    assert!(gfs.file_exists("/p/.ts-r-guard/rt.js"));
    // A declaration's module: its own functions, imports for what it references.
    let user = gfs
        .read_file("/p/.ts-r-guard/types.ts/User.js")
        .expect("User.js");
    assert!(user.contains("const $is_User = (v, d) =>"), "{user}");
    assert!(user.contains("export { $is_User as $is, $d_User as $d };"));
    assert_eq!(
        user,
        gfs.read_file("/p/.ts-r-guard/types.ts/User.js")
            .unwrap_or_default()
    );
    let page = gfs
        .read_file("/p/.ts-r-guard/types.ts/Page.js")
        .expect("Page.js");
    assert!(page.contains("const $is_Page = (v, d, t0) =>"), "{page}");
    let ext = gfs
        .read_file("/p/.ts-r-guard/types.ts/~mod~ext/Ext.js")
        .expect("Ext.js");
    assert!(ext.contains("const $is_Ext = (v, d) =>"), "{ext}");
    assert!(gfs.touched().contains(&"/p/types.ts".to_owned()));
}

#[test]
fn errors_last_until_the_file_is_read_again() {
    // A watch rebuild can reuse a module without reading it: its errors must still be there.
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("guard-errors-kept");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let root = ts_r_config::path::normalize_slashes(&dir.to_string_lossy());
    let bad = format!("{root}/bad.ts");
    let helper = format!("{root}/helper.ts");
    std::fs::write(
        &bad,
        "import { parse } from \"@ts-r.dev/guard\";\nexport const v = parse(1);\n",
    )
    .expect("bad");
    std::fs::write(&helper, "export const h = 1;\n").expect("helper");
    let gfs = GuardFs::new(
        Arc::new(ts_r_config::RealFs::new()),
        &root,
        GuardOptions::default(),
        true,
    );
    let bad = gfs.realpath(&bad);
    let helper = gfs.realpath(&helper);
    let files = [bad.clone(), helper.clone()];
    let codes = |gfs: &GuardFs| -> Vec<String> {
        gfs.errors_in(&files)
            .iter()
            .flat_map(|f| f.errors.iter().map(|e| e.code_label()))
            .collect()
    };
    gfs.read_file(&bad).expect("bad");
    gfs.read_file(&helper).expect("helper");
    assert_eq!(codes(&gfs), ["TSR1402"]);
    // Reported again on the next build, which only re-reads the helper.
    gfs.read_file(&helper).expect("helper");
    assert_eq!(codes(&gfs), ["TSR1402"]);
    // Only for the build's files.
    assert!(gfs.errors_in(std::slice::from_ref(&helper)).is_empty());
    // Replaced when the file is read again.
    std::fs::write(&bad, "export const v = 1;\n").expect("fixed");
    gfs.read_file(&bad).expect("bad");
    assert!(codes(&gfs).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directive_without_semicolon_is_terminated() {
    let src = "\"use strict\"\nimport { parse } from \"@ts-r.dev/guard\";\nexport const a = (x: unknown) => parse<number>(x);\n";
    for layout in [
        Layout::Inline,
        Layout::Shared {
            project_root: "/p".into(),
            vroot: "/p/.ts-r-guard".into(),
        },
    ] {
        let x = expander(layout)
            .expand("/p/strict.ts", src)
            .expect("expanded");
        assert!(x.code.starts_with("\"use strict\"; "), "{}", x.code);
        let parsed = ts_r_parser::parse(&x.code, ts_r_ast::SourceKind::Ts);
        assert!(parsed.diagnostics.is_empty(), "{}", x.code);
    }
}

#[test]
fn runtime_only_files_do_not_load_their_imports() {
    let ex = expander(Layout::Inline);
    let helpers = "import { expect } from \"@ts-r.dev/guard\";\nimport { x } from \"./plain\";\nexport const y = expect(x, \"x\");\n";
    let x = ex.expand("/p/helpers.ts", helpers).expect("expanded");
    assert!(x.errors.is_empty(), "{:?}", x.errors);
    assert!(x.code.contains("const expect = $expect;"), "{}", x.code);
    assert!(x.deps.is_empty(), "{:?}", x.deps);
    // A typed call still loads the declarations it needs.
    let x = ex.expand("/p/main.ts", MAIN).expect("expanded");
    assert!(x.deps.contains(&"/p/types.ts".to_owned()), "{:?}", x.deps);
}

#[test]
fn call_sites_share_validators_only_for_the_same_text() {
    // Whitespace inside a literal type is part of the type.
    let src = "import { is } from \"@ts-r.dev/guard\";\nexport const a = (x: unknown) => is<\"a  b\">(x);\nexport const b = (x: unknown) => is<\"a b\">(x);\n";
    let x = expander(Layout::Inline)
        .expand("/p/lits.ts", src)
        .expect("expanded");
    assert!(
        x.code.contains("\"a  b\"") && x.code.contains("\"a b\""),
        "{}",
        x.code
    );
    assert_ne!(
        x.code.lines().nth(1).and_then(|l| l.split("=> ").nth(1)),
        x.code.lines().nth(2).and_then(|l| l.split("=> ").nth(1)),
        "{}",
        x.code
    );
}

#[test]
fn changed_tsconfig_options_are_reloaded() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("guard-config-reload");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let root = ts_r_config::path::normalize_slashes(&dir.to_string_lossy());
    let config = |target: &str| {
        let paths =
            format!(r#"{{"compilerOptions":{{"baseUrl":".","paths":{{"@s":["{target}"]}}}}}}"#);
        std::fs::write(format!("{root}/tsconfig.json"), paths).expect("tsconfig");
    };
    std::fs::write(
        format!("{root}/a.ts"),
        "export interface S { value: string }\n",
    )
    .expect("a");
    std::fs::write(
        format!("{root}/b.ts"),
        "export interface S { value: number }\n",
    )
    .expect("b");
    let main = format!("{root}/main.ts");
    std::fs::write(&main, "import { is } from \"@ts-r.dev/guard\";\nimport type { S } from \"@s\";\nexport const f = (x: unknown) => is<S>(x);\n").expect("main");
    config("./a.ts");
    let gfs = GuardFs::new(
        Arc::new(ts_r_config::RealFs::new()),
        &root,
        GuardOptions::default(),
        false,
    );
    let main = gfs.realpath(&main);
    let first = gfs.read_file(&main).expect("main");
    assert!(first.contains("typeof v.value === \"string\""), "{first}");
    config("./b.ts");
    gfs.forget_configs();
    let second = gfs.read_file(&main).expect("main");
    assert!(second.contains("typeof v.value === \"number\""), "{second}");
    let _ = std::fs::remove_dir_all(&dir);
}
