//! Entry expansion and deduplication before graph construction.

ts_r_testing::cap_memory!();

use std::sync::Arc;

use ts_r_bundler::{BuildContext, BuildOptions, EntryPoint};
use ts_r_config::MemoryFs;

fn context(fs: MemoryFs, entries: &[&str]) -> BuildContext {
    BuildContext::new(
        BuildOptions {
            working_dir: "/proj".into(),
            entry_points: entries
                .iter()
                .map(|p| EntryPoint {
                    path: (*p).into(),
                    name: None,
                })
                .collect(),
            outdir: Some("out".into()),
            write: false,
            threads: 2,
            ..BuildOptions::default()
        },
        Arc::new(fs),
    )
    .expect("options")
}

#[test]
fn glob_size_limit_reports_an_error_instead_of_partial_entries() {
    let mut fs = MemoryFs::new();
    for i in 0..100_001 {
        fs.add_file(&format!("/proj/entry{i:06}.js"), "");
    }
    let r = context(fs, &["*.js"]).build();
    assert!(r.has_errors());
    assert!(r.outputs.is_empty());
    assert!(r.errors().any(|m| m.text.contains("traversal limit")));
}

#[test]
fn glob_depth_limit_reports_an_error_instead_of_partial_entries() {
    let mut fs = MemoryFs::new();
    fs.add_file("/proj/entry.js", "console.log(1);");
    fs.add_file(&format!("/proj/{}entry.js", "deep/".repeat(65)), "");
    let r = context(fs, &["**/*.js"]).build();
    assert!(r.has_errors());
    assert!(r.outputs.is_empty());
    assert!(r.errors().any(|m| m.text.contains("depth limit")));
}

#[test]
fn glob_skips_directories_that_cannot_match() {
    let mut fs = MemoryFs::new();
    fs.add_file("/proj/good/entry.js", "console.log(1);");
    fs.add_file(&format!("/proj/bad/{}entry.js", "deep/".repeat(65)), "");
    fs.add_file("/proj/good/deep/extra.js", "");
    let r = context(fs, &["g*/*.js"]).build();
    assert!(!r.has_errors(), "{:?}", r.messages);
    assert_eq!(r.outputs.len(), 1);
    assert_eq!(r.outputs[0].path, "/proj/out/entry.js");
}

#[test]
fn overlapping_entries_keep_their_order_and_distinct_output_names() {
    let mut fs = MemoryFs::new();
    fs.add_file("/proj/a.js", "console.log('a');");
    fs.add_file("/proj/b.js", "console.log('b');");
    let mut opts = context(fs.clone(), &["b.js", "*.js", "a.js", "b.js"])
        .options()
        .clone();
    opts.entry_points.push(EntryPoint {
        path: "b.js".into(),
        name: Some("copy".into()),
    });
    let ctx = BuildContext::new(opts, Arc::new(fs)).expect("options");
    for _ in 0..2 {
        let r = ctx.build();
        assert!(!r.has_errors(), "{:?}", r.messages);
        let paths: Vec<_> = r.outputs.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(
            paths,
            ["/proj/out/b.js", "/proj/out/a.js", "/proj/out/copy.js"]
        );
    }
}
