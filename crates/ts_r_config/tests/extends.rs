//! `extends` graphs that are not simple chains.

use ts_r_config::{MemoryFs, OptionId, OptionValue, load_tsconfig};

// The configs are generated.
ts_r_testing::cap_memory!();

/// `c<k>` extends `c<k-1>` twice: 28 files whose naive expansion is 2^27 merges.
#[test]
fn diamond_extends_merge_each_base_once() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/c0.json",
        r#"{ "compilerOptions": { "strict": false, "declaration": true } }"#,
    );
    for k in 1..28 {
        let prev = format!("./c{}.json", k - 1);
        let own = if k == 3 {
            r#", "compilerOptions": { "strict": true }"#
        } else {
            ""
        };
        fs.add_file(
            &format!("/p/c{k}.json"),
            &format!(r#"{{ "extends": ["{prev}", "{prev}"]{own}, "files": [] }}"#),
        );
    }
    let parsed = load_tsconfig(&fs, "/p/c27.json");
    let expected: Vec<String> = (0..27).rev().map(|k| format!("/p/c{k}.json")).collect();
    assert_eq!(parsed.extended_config_paths, expected);
    // Overrides still apply in order: c3 turns `strict` back on, c0's `declaration` stays.
    assert_eq!(
        parsed.options.get(OptionId::Strict),
        Some(&OptionValue::Bool(true))
    );
    assert_eq!(
        parsed.options.get(OptionId::Declaration),
        Some(&OptionValue::Bool(true))
    );
}

/// `${configDir}` stands for `./` in the leaf config's directory (as tsc reads it), so
/// `${configDir}..` is that directory's parent, not `<dir>..`.
#[test]
fn config_dir_template_is_a_relative_path_from_the_leaf_config() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/src/tsconfig.json",
        r#"{ "extends": "../shared/base.json", "files": [] }"#,
    );
    fs.add_file(
        "/p/shared/base.json",
        r#"{ "compilerOptions": { "baseUrl": "${configDir}..", "outDir": "${configDir}/../dist",
             "paths": { "lib/*": ["${configDir}../lib/*"] } } }"#,
    );
    let parsed = load_tsconfig(&fs, "/p/src/tsconfig.json");
    assert_eq!(
        parsed.options.get(OptionId::BaseUrl),
        Some(&OptionValue::String("/p".into()))
    );
    assert_eq!(
        parsed.options.get(OptionId::OutDir),
        Some(&OptionValue::String("/p/dist".into()))
    );
    let Some(OptionValue::Paths(paths)) = parsed.options.get(OptionId::Paths) else {
        panic!("paths: {:?}", parsed.options.get(OptionId::Paths));
    };
    assert_eq!(paths.0[0].1, ["/p/lib/*"]);
}

/// TS18003 quotes `include` and `exclude` as written: `${configDir}` specs stay
/// unsubstituted, while the default `outDir` exclude is absolute (observed with tsc 7.0.2,
/// and on date-fns `pkgs/dev/config/tsconfig.json`).
#[test]
fn no_inputs_quotes_config_dir_specs_as_written() {
    let message = |base: &str, leaf: &str| {
        let mut fs = MemoryFs::new();
        fs.add_file("/p/shared/base.json", base);
        fs.add_file("/p/src/tsconfig.json", leaf);
        let parsed = load_tsconfig(&fs, "/p/src/tsconfig.json");
        let d: Vec<_> = parsed
            .diagnostics
            .iter()
            .filter(|d| d.code == 18003)
            .collect();
        assert_eq!(d.len(), 1, "{:?}", parsed.diagnostics);
        d[0].message.clone()
    };
    let head = "No inputs were found in config file '/p/src/tsconfig.json'.";
    let base = r#"{ "include": ["${configDir}/*.ts", "${configDir}/lib/**/*"],
                    "exclude": ["${configDir}/out"] }"#;
    assert_eq!(
        message(base, r#"{ "extends": "../shared/base.json" }"#),
        format!(
            r#"{head} Specified 'include' paths were '["${{configDir}}/*.ts","${{configDir}}/lib/**/*"]' and 'exclude' paths were '["${{configDir}}/out"]'."#
        )
    );
    assert_eq!(
        message(
            base,
            r#"{ "extends": "../shared/base.json", "include": ["${configDir}/x/*.ts", "y"],
                 "compilerOptions": { "outDir": "${configDir}/o" } }"#
        ),
        format!(
            r#"{head} Specified 'include' paths were '["${{configDir}}/x/*.ts","y"]' and 'exclude' paths were '["${{configDir}}/out"]'."#
        )
    );
    assert_eq!(
        message(
            r#"{ "include": ["${configDir}/*.ts"], "compilerOptions": { "outDir": "${configDir}/o" } }"#,
            r#"{ "extends": "../shared/base.json" }"#
        ),
        format!(
            r#"{head} Specified 'include' paths were '["${{configDir}}/*.ts"]' and 'exclude' paths were '["/p/src/o"]'."#
        )
    );
}

/// A package `extends` with no `node_modules` copy resolves through Yarn PnP's
/// `.pnp.data.json` and the package's `exports` (esbuild's `TsconfigPackageJsonExportsYarnPnP`
/// and `TsconfigStackOverflowYarnPnP`, whose package depends on itself).
#[test]
fn package_extends_through_yarn_pnp() {
    let tsconfigs = |fs: &mut MemoryFs| {
        fs.add_file(
            "/u/p/packages/tsconfigs/package.json",
            r#"{ "exports": { "./config": "./configs/tsconfig.json" } }"#,
        );
        fs.add_file(
            "/u/p/packages/tsconfigs/configs/tsconfig.json",
            r#"{ "compilerOptions": { "jsxFactory": "success" } }"#,
        );
    };
    let factory = |fs: &MemoryFs, config: &str| {
        let parsed = load_tsconfig(fs, config);
        assert!(
            parsed
                .diagnostics
                .iter()
                .all(|d| d.code != 6053 && d.code != 5083),
            "{:?}",
            parsed.diagnostics
        );
        assert_eq!(
            parsed.extended_config_paths,
            ["/u/p/packages/tsconfigs/configs/tsconfig.json"]
        );
        parsed.options.get(OptionId::JsxFactory).cloned()
    };
    let success = Some(OptionValue::String("success".into()));

    let mut fs = MemoryFs::new();
    tsconfigs(&mut fs);
    fs.add_file("/u/p/packages/app/index.tsx", "console.log(<div/>)");
    fs.add_file(
        "/u/p/packages/app/tsconfig.json",
        r#"{ "extends": "tsconfigs/config" }"#,
    );
    fs.add_file(
        "/u/p/.pnp.data.json",
        r#"{ "packageRegistryData": [
          ["app", [["workspace:packages/app", { "packageLocation": "./packages/app/",
            "packageDependencies": [["tsconfigs", "workspace:packages/tsconfigs"]], "linkType": "SOFT" }]]],
          ["tsconfigs", [["workspace:packages/tsconfigs", { "packageLocation": "./packages/tsconfigs/",
            "packageDependencies": [], "linkType": "SOFT" }]]]
        ] }"#,
    );
    assert_eq!(factory(&fs, "/u/p/packages/app/tsconfig.json"), success);

    let mut fs = MemoryFs::new();
    tsconfigs(&mut fs);
    fs.add_file("/u/p/entry.jsx", "console.log(<div />)");
    fs.add_file("/u/p/tsconfig.json", r#"{ "extends": "tsconfigs/config" }"#);
    fs.add_file(
        "/u/p/.pnp.data.json",
        r#"{ "packageRegistryData": [
          [null, [[null, { "packageLocation": "./",
            "packageDependencies": [["tsconfigs", "virtual:some-path"]], "linkType": "SOFT" }]]],
          ["tsconfigs", [
            ["virtual:some-path", { "packageLocation": "./packages/tsconfigs/",
              "packageDependencies": [["tsconfigs", "virtual:some-path"]], "packagePeers": [], "linkType": "SOFT" }],
            ["workspace:packages/tsconfigs", { "packageLocation": "./packages/tsconfigs/",
              "packageDependencies": [["tsconfigs", "workspace:packages/tsconfigs"]], "linkType": "SOFT" }]
          ]]
        ] }"#,
    );
    assert_eq!(factory(&fs, "/u/p/tsconfig.json"), success);

    // The manifest that placed the package is a configuration input, for watchers.
    let parsed = load_tsconfig(&fs, "/u/p/tsconfig.json");
    assert_eq!(parsed.pnp_manifest_paths, ["/u/p/.pnp.data.json"]);

    // A name the manifest does not list stays TS6053; the manifest was still read, and a
    // change to it can make the name resolve.
    fs.add_file("/u/p/tsconfig.json", r#"{ "extends": "other/config" }"#);
    let parsed = load_tsconfig(&fs, "/u/p/tsconfig.json");
    assert!(parsed.diagnostics.iter().any(|d| d.code == 6053));
    assert_eq!(parsed.pnp_manifest_paths, ["/u/p/.pnp.data.json"]);
}

/// A manifest consulted by a base config is an input of every config that extends it; one
/// that resolves through `node_modules` consults none.
#[test]
fn pnp_manifests_are_inherited_inputs() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/u/p/packages/tsconfigs/package.json",
        r#"{ "exports": { "./config": "./configs/tsconfig.json" } }"#,
    );
    fs.add_file(
        "/u/p/packages/tsconfigs/configs/tsconfig.json",
        r#"{ "compilerOptions": { "strict": true } }"#,
    );
    fs.add_file(
        "/u/p/.pnp.data.json",
        r#"{ "packageRegistryData": [
          [null, [[null, { "packageLocation": "./",
            "packageDependencies": [["tsconfigs", "workspace:packages/tsconfigs"]] }]]],
          ["tsconfigs", [["workspace:packages/tsconfigs", { "packageLocation": "./packages/tsconfigs/",
            "packageDependencies": [] }]]]
        ] }"#,
    );
    fs.add_file("/u/p/base.json", r#"{ "extends": "tsconfigs/config" }"#);
    fs.add_file("/u/p/a/tsconfig.json", r#"{ "extends": "../base.json" }"#);
    fs.add_file("/u/p/a/x.ts", "");
    let parsed = load_tsconfig(&fs, "/u/p/a/tsconfig.json");
    assert_eq!(parsed.pnp_manifest_paths, ["/u/p/.pnp.data.json"]);

    fs.add_file(
        "/u/p/node_modules/tsconfigs/package.json",
        r#"{ "exports": { "./config": "./c.json" } }"#,
    );
    fs.add_file("/u/p/node_modules/tsconfigs/c.json", "{}");
    let parsed = load_tsconfig(&fs, "/u/p/a/tsconfig.json");
    assert!(
        parsed.pnp_manifest_paths.is_empty(),
        "{:?}",
        parsed.pnp_manifest_paths
    );
}
