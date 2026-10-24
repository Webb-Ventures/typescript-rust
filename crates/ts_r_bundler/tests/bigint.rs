//! BigInt literal checks follow the target and explicit feature overrides.

ts_r_testing::cap_memory!();

use std::sync::Arc;

use ts_r_bundler::{BuildContext, BuildOptions, BuildResult, EntryPoint, Feature};
use ts_r_config::MemoryFs;

fn build(source: &str, target: Option<u32>, supported: Vec<(Feature, bool)>) -> BuildResult {
    let mut fs = MemoryFs::new();
    fs.add_file("/proj/entry.ts", source);
    let options = BuildOptions {
        entry_points: vec![EntryPoint {
            path: "entry.ts".into(),
            name: None,
        }],
        working_dir: "/proj".into(),
        bundle: true,
        outdir: Some("out".into()),
        write: false,
        threads: 2,
        target,
        supported,
        ..BuildOptions::default()
    };
    BuildContext::new(options, Arc::new(fs))
        .expect("options")
        .build()
}

#[test]
fn unsupported_bigint_literals_are_errors() {
    for (target, supported) in [
        (Some(2019), vec![]),
        (None, vec![(Feature::Bigint, false)]),
        (Some(2020), vec![(Feature::Bigint, false)]),
    ] {
        let result = build("console.log(1n);", target, supported);
        assert!(result.has_errors(), "{result:?}");
        assert!(result.outputs.is_empty());
        assert!(result.messages.iter().any(|message| {
            message.is_error()
                && message.column == 12
                && message.text
                    == "Big integer literals are not available in the configured target environment"
        }));
    }
}

#[test]
fn supported_bigint_literals_are_preserved() {
    for (target, supported) in [
        (None, vec![]),
        (Some(2020), vec![]),
        (Some(2019), vec![(Feature::Bigint, true)]),
    ] {
        let result = build("console.log(1n);", target, supported);
        assert!(!result.has_errors(), "{:?}", result.messages);
        assert!(
            result
                .outputs
                .iter()
                .any(|output| output.text().contains("1n"))
        );
    }
}

#[test]
fn bigint_types_do_not_require_runtime_support() {
    let result = build(
        "type T = 1n; interface I { value: 2n } declare const value: 3n; console.log(1);",
        Some(2019),
        vec![(Feature::Bigint, false)],
    );
    assert!(!result.has_errors(), "{:?}", result.messages);
}
