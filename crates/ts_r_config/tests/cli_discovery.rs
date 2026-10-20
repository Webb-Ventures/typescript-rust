use ts_r_config::cli::parse_command_line;
use ts_r_config::{ConfigLoader, MemoryFs};

ts_r_testing::cap_memory!();

#[test]
fn overrides_apply_before_discovery_and_empty_input_diagnostics() {
    let mut fs = MemoryFs::new();
    fs.add_file("/p/tsconfig.json", "{}")
        .add_file("/p/bad.js", "const x: number = 'oops';");
    let mut loader = ConfigLoader::new(&fs);
    assert!(loader.load("/p/tsconfig.json").file_names.is_empty());
    for args in [["--allowJs"], ["--checkJs"]] {
        let args = args.map(str::to_owned);
        let cl = parse_command_line(&args, "/p", &fs);
        let parsed = loader.load_with_overrides("/p/tsconfig.json", |o| cl.apply_to(o));
        assert_eq!(parsed.file_names, ["/p/bad.js"]);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }
}

#[test]
fn overrides_recompute_inherited_default_exclusions() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/base.json",
        r#"{ "compilerOptions": { "allowJs": true, "outDir": "old" } }"#,
    )
    .add_file("/p/tsconfig.json", r#"{ "extends": "./base.json" }"#)
    .add_file("/p/a.ts", "export {};")
    .add_file("/p/b.js", "export {};")
    .add_file("/p/old/c.ts", "export {};")
    .add_file("/p/dist/d.ts", "export {};")
    .add_file("/p/types/e.ts", "export {};");
    let args = [
        "--allowJs",
        "false",
        "--outDir",
        "dist",
        "--declarationDir",
        "types",
    ]
    .map(str::to_owned);
    let cl = parse_command_line(&args, "/p", &fs);
    let mut loader = ConfigLoader::new(&fs);
    let parsed = loader.load_with_overrides("/p/tsconfig.json", |o| cl.apply_to(o));
    assert_eq!(parsed.file_names, ["/p/a.ts", "/p/old/c.ts"]);
    assert_eq!(parsed.default_excludes, ["/p/dist", "/p/types"]);
    let args = ["--outDir", "null"].map(str::to_owned);
    let cl = parse_command_line(&args, "/p", &fs);
    let parsed = loader.load_with_overrides("/p/tsconfig.json", |o| cl.apply_to(o));
    assert!(parsed.default_excludes.is_empty());
    assert!(parsed.file_names.iter().any(|f| f == "/p/old/c.ts"));
    // Overrides do not leak into subsequent loads through the loader's caches.
    assert_eq!(loader.load("/p/tsconfig.json").default_excludes, ["/p/old"]);
}
