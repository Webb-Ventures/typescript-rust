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
