//! Pathological types give a reason, never a stack overflow or a hang.

use ts_r_ast::Node;
use ts_r_checker::load_program;
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_guard::{Generator, GuardOptions};

ts_r_testing::cap_memory!();

/// Generates for the type of `export type T = ...;` in a one-file program.
fn generate(src: &str) -> Result<String, String> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true},"include":["*.ts"]}"#,
    );
    fs.add_file("/p/a.ts", src);
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let p = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let file = p
        .files
        .iter()
        .position(|f| f.path == "/p/a.ts")
        .ok_or("no file")?;
    let ast = &p.files[file].ast;
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return Err("no program".into());
    };
    let ty = ast
        .list(stmts)
        .iter()
        .find_map(|&s| match *ast.node(s) {
            Node::TypeAlias { ty, .. } => Some(ty),
            _ => None,
        })
        .ok_or("no alias")?;
    let mut g = Generator::new(&p, GuardOptions::default());
    g.add(file, ty).map_err(|u| u.reason)?;
    Ok(g.module())
}

#[test]
fn deeply_nested_type_literal() {
    let depth = 400;
    let src = format!(
        "export type T = {}number{};",
        "{ a: ".repeat(depth),
        " }".repeat(depth)
    );
    let r = generate(&src);
    assert!(r.is_err(), "expected a reason");
}

#[test]
fn self_referential_aliases() {
    // `A` only through itself: evaluates (a recursive validator), no loop.
    assert!(generate("export type T = { next: T | null };").is_ok());
    // Partial of itself while being evaluated: a reason.
    let r = generate("export type T = { a: Partial<T> };");
    assert!(r.is_err());
}

#[test]
fn wide_union() {
    let members: Vec<String> = (0..2000).map(|i| format!("\"m{i}\"")).collect();
    let src = format!("export type T = {};", members.join(" | "));
    let code = generate(&src).expect("supported");
    assert!(code.contains("\"m1999\""));
}

#[test]
fn utility_types_of_an_alias_of_itself() {
    // `A`'s body is a reference to `A`: expanding or resolving it never reaches a type.
    for t in [
        "Partial<A>",
        "Exclude<A, \"x\">",
        "Pick<A, \"a\">",
        "keyof A",
    ] {
        let r = generate(&format!("export type T = {t};\ntype A = A;"));
        assert!(r.is_err(), "{t}: expected a reason");
    }
}

#[test]
fn derived_types_with_no_members_are_never() {
    // An empty union would emit `return ();`: rejected like a written `never`.
    for t in [
        "Extract<\"a\", \"b\">",
        "Exclude<\"a\", \"a\">",
        "keyof {}",
        "NonNullable<null | undefined>",
        "{ a: Exclude<\"a\", \"a\"> }",
    ] {
        let r = generate(&format!("export type T = {t};"));
        assert_eq!(r.err().as_deref(), Some("no value has type 'never'"), "{t}");
    }
}

#[test]
fn non_nullable_unknown_is_unsupported() {
    // Its predicate would be `true`, accepting `null` and `undefined`.
    for t in ["NonNullable<unknown>", "NonNullable<unknown | null>"] {
        assert!(generate(&format!("export type T = {t};")).is_err(), "{t}");
    }
    assert!(generate("export type T = NonNullable<string | null>;").is_ok());
}

#[test]
fn declarations_named_like_entries() {
    // Entry `e0` and a declaration `e0` must not both define `$is_e0` / `$d_e0`.
    let code =
        generate("export type T = { a: e0; b: e1 };\ntype e0 = string[];\ntype e1 = { c: e0 };")
            .expect("supported");
    for f in ["const $is_e0 =", "const $d_e0 ="] {
        assert_eq!(code.matches(f).count(), 1, "{f} in\n{code}");
    }
}
