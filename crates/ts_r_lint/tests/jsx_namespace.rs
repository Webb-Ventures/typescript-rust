//! Classic JSX typed through the factory's namespace (`h.JSX` for `jsxFactory: "h"`) rather
//! than a global `JSX`, as tsc does. Skipped when the TypeScript lib files are missing
//! (`cargo xtask upstream sync --minimal`).

use ts_r_lint::Registry;
use ts_r_lint::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
use ts_r_lint::typed::{TypedFile, check_and_lint, load_project};

ts_r_testing::cap_memory!();

const FACTORY: &str = r#"declare function h(...a: any[]): any;
declare namespace h {
  namespace JSX {
    interface Element { readonly brand: 1 }
    interface IntrinsicElements { div: { onClick?: () => void } }
  }
}
"#;

/// The messages of `rule` on `/p/a.tsx` (`code` followed by `FACTORY`) in a project with
/// `jsx: "react"` and `options`.
fn lint(rule: &str, options: &str, code: &str) -> Option<Vec<String>> {
    let libs = ts_r_checker::LibStore::locate()?;
    let mut fs = ts_r_config::MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        &format!(r#"{{"compilerOptions":{{"strict":true,"jsx":"react","types":[]{options}}}}}"#),
    );
    fs.add_file("/p/a.tsx", &format!("{code}{FACTORY}"));
    let project = load_project(&fs, "/p/tsconfig.json", &libs, 1);
    let config = serde_json::json!([{
        "files": ["**/*.tsx"],
        "languageOptions": { "parser": "typescript-eslint/parser" },
        "rules": { rule: "error" },
    }]);
    let objs = objects_from_eslint_json(&config, Registry::global()).expect("config");
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
    let mut opts = ResolveOptions::new();
    opts.types_available = true;
    let cfg = cfg.file_config("a.tsx", &opts).expect("file config");
    let file = project.file_index("/p/a.tsx").expect("file in project");
    let run = check_and_lint(&project, &[TypedFile { file, cfg }], 1);
    assert!(run.failed.is_empty(), "the check or lint panicked");
    let r = run.results.into_iter().next().expect("lint result");
    Some(r.diagnostics.into_iter().map(|d| d.message).collect())
}

#[test]
fn factory_namespace_types_elements() {
    // `<div />` is an `h.JSX.Element`, not the error type.
    let rule = "@typescript-eslint/no-unsafe-assignment";
    let Some(got) = lint(rule, r#","jsxFactory":"h""#, "const el = <div />;\n") else {
        return;
    };
    assert_eq!(got, Vec::<String>::new());
    // The factory named by a leading `@jsx` pragma.
    let got = lint(rule, "", "/** @jsx h */\nconst el = <div />;\n");
    assert_eq!(got, Some(Vec::new()));
    // Without a namespace (no `React` here) the element has the error type.
    let got = lint(rule, "", "const el = <div />;\n");
    assert_eq!(
        got,
        Some(vec![
            "Unsafe assignment of an error typed value.".to_string()
        ])
    );
}

#[test]
fn factory_namespace_types_intrinsic_attributes() {
    let Some(got) = lint(
        "@typescript-eslint/no-misused-promises",
        r#","jsxFactory":"h""#,
        "const el = <div onClick={async () => {}} />;\n",
    ) else {
        return;
    };
    assert_eq!(
        got,
        ["Promise-returning function provided to attribute where a void return was expected."]
    );
}
