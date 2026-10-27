//! Entry code from stdin (esbuild's `<stdin>`): resolved from the working directory, named
//! `<stdin>` in messages.

use std::path::{Path, PathBuf};

ts_r_testing::cap_memory!();

#[test]
fn stdin_entry_resolves_from_the_working_directory() {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("build-stdin");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    std::fs::write(d.join("lib.js"), "export const x = 1;\n").expect("write");
    let run = |code: &str| {
        std::fs::write(d.join("in.txt"), code).expect("write");
        let input = std::fs::File::open(d.join("in.txt")).expect("open");
        ts_r_testing::limited(Path::new(env!("CARGO_BIN_EXE_ts-r-build")), 1024, 60)
            .args(["--bundle", "--no-cache"])
            .current_dir(&d)
            .stdin(input)
            .output()
            .expect("run ts-r-build")
    };
    let ok = run("import { x } from './lib.js'; console.log(x);");
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("var x = 1;"));
    let bad = run("import 'node_modules/fflate';");
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("<stdin>:1:7"));
    // Raw bytes reach a binary loader unchanged.
    std::fs::write(d.join("in.bin"), [0xffu8]).expect("write");
    let input = std::fs::File::open(d.join("in.bin")).expect("open");
    let b = ts_r_testing::limited(Path::new(env!("CARGO_BIN_EXE_ts-r-build")), 1024, 60)
        .args(["--loader=base64", "--no-cache"])
        .current_dir(&d)
        .stdin(input)
        .output()
        .expect("run ts-r-build");
    assert!(String::from_utf8_lossy(&b.stdout).contains("/w=="), "{b:?}");
    let _ = std::fs::remove_dir_all(&d);
}
