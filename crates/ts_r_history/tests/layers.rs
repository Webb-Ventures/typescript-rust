//! Layered checks equal flat checks: the fixture projects are checked with `check_program`
//! and with shared lower levels (every hot-file choice), and the diagnostics must match.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use ts_r_checker::{CheckRun, Checker, LibStore, ProgramFiles, check_program, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_diagnostics::Diagnostic;
use ts_r_history::levels;

ts_r_testing::cap_memory!();

/// The shared levels live in one registry per process: a test that builds the fixture's
/// levels while another compares `Arc`s of the same levels races it, so these run one at a
/// time.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .join("tsconfig.json");
    p.to_string_lossy().replace('\\', "/")
}

fn load(libs: &LibStore, config: &str) -> ProgramFiles {
    let fs = RealFs::new();
    let cfg = load_tsconfig(&fs, config);
    load_program(&fs, &cfg.file_names, &cfg.options, Some(libs), 1)
}

/// Diagnostics as comparable lines (file path instead of file id).
fn lines(program: &ProgramFiles, diags: &[Diagnostic]) -> Vec<String> {
    let path = |f: ts_r_diagnostics::FileId| {
        program
            .files
            .get(f.0 as usize)
            .map_or("<global>", |f| f.path.as_str())
            .to_owned()
    };
    let mut v: Vec<String> = diags
        .iter()
        .map(|d| {
            let rel: Vec<String> = d
                .related
                .iter()
                .map(|r| {
                    format!(
                        "{}@{}:{} {}",
                        path(r.file),
                        r.span.start,
                        r.span.end,
                        r.message
                    )
                })
                .collect();
            format!(
                "{}@{}:{} TS{} {} [{}]",
                path(d.file),
                d.span.start,
                d.span.end,
                d.code,
                d.message,
                rel.join("; ")
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn layered_equals_flat() {
    let _serial = serial();
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let program = load(&libs, &fixture("app"));
    let flat = check_program(&program, CheckRun { threads: 1 });
    let want = lines(&program, &flat.diagnostics);
    assert!(want.len() >= 8, "fixture should have errors: {want:#?}");

    let project: Vec<usize> = (0..program.files.len())
        .filter(|&i| levels::base_level(&program.files[i]) == levels::PROJECT)
        .collect();
    let mut hots: Vec<Vec<usize>> = vec![Vec::new()];
    hots.extend(project.iter().map(|&i| vec![i]));
    for hot in hots {
        let (arranged, run) = ts_r_history::check_layered(&program, &hot, CheckRun { threads: 2 });
        let got = lines(&arranged, &run.diagnostics);
        assert_eq!(got, want, "hot = {hot:?}");
    }
}

#[test]
fn plan_hoists_global_contributors() {
    let _serial = serial();
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let program = load(&libs, &fixture("app"));
    let graph = levels::Graph::build(&program);
    let lv = levels::plan(&program, &graph, &[]);
    let level_of = |suffix: &str| {
        let i = program
            .files
            .iter()
            .position(|f| f.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("{suffix} not loaded"));
        lv[i]
    };
    // `declare global { interface Array<T> }` merges with the lib: it must be checked with it.
    assert_eq!(level_of("src/globals.d.ts"), levels::LIB);
    assert_eq!(level_of("tiny-lib/index.d.ts"), levels::DEPS);
    assert_eq!(level_of("src/main.ts"), levels::PROJECT);
    // The module augmentation and its target share a level.
    assert_eq!(level_of("src/augment.ts"), level_of("src/shapes.ts"));

    // Editing math.ts makes it and everything importing it hot; env.d.ts stays stable.
    let math = program
        .files
        .iter()
        .position(|f| f.path.ends_with("src/math.ts"))
        .unwrap_or(0);
    let lv = levels::plan(&program, &graph, &[math]);
    assert_eq!(lv[math], levels::HOT);
    let main = program
        .files
        .iter()
        .position(|f| f.path.ends_with("src/main.ts"))
        .unwrap_or(0);
    assert_eq!(lv[main], levels::HOT);
}

#[test]
fn lower_levels_are_shared() {
    let _serial = serial();
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let a = load(&libs, &fixture("app"));
    let b = load(&libs, &fixture("app"));
    let mut pa = levels::with_files(&a, a.files.clone());
    let ra = ts_r_history::arrange(&mut pa, &[]);
    let top_a = Checker::layered(&pa, ra.parent.as_ref());
    assert!(top_a.is_ok());
    // Another project loading the same files (other `Arc`s) finds the levels by content.
    let mut pb = levels::with_files(&b, b.files.clone());
    let rb = ts_r_history::arrange(&mut pb, &[]);
    assert_eq!(rb.built, 0, "{rb:?}");
    assert!(rb.reused >= 1);
    let (Some(x), Some(y)) = (&ra.parent, &rb.parent) else {
        panic!("no shared level");
    };
    assert!(std::sync::Arc::ptr_eq(x, y));
    // The second program now holds the first one's lower-level files.
    let end = x.file_end() as usize;
    for i in 0..end {
        assert!(std::sync::Arc::ptr_eq(&pa.files[i], &pb.files[i]));
    }
    // Options that only say where files go do not split levels; checking options do.
    let mut c = load(&libs, &fixture("app"));
    c.options.set(
        ts_r_config::OptionId::OutDir,
        Some(ts_r_config::OptionValue::String("/elsewhere".to_owned())),
        None,
    );
    let rc = ts_r_history::arrange(&mut c, &[]);
    assert_eq!(rc.built, 0, "{rc:?}");
    let mut d = load(&libs, &fixture("app"));
    d.options.set(
        ts_r_config::OptionId::ExactOptionalPropertyTypes,
        Some(ts_r_config::OptionValue::Bool(true)),
        None,
    );
    let rd = ts_r_history::arrange(&mut d, &[]);
    assert!(rd.built >= 1, "{rd:?}");
}

/// A module augmentation of a re-exported name (`declare module "./index" { interface Meta }`
/// where `./index` re-exports `Meta`) in a dependency: the redirect is wholly in a lower level,
/// so building the project's level must neither touch it nor panic (vitest's types did, on
/// trpc and date-fns: an index underflow in the symbol table).
#[test]
fn lower_level_augmentation_redirect() {
    let _serial = serial();
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let program = load(&libs, &fixture("aug-redirect"));
    let flat = check_program(&program, CheckRun { threads: 1 });
    let want = lines(&program, &flat.diagnostics);
    // The augmentation merged: `b` exists and is a string.
    assert_eq!(want.len(), 1, "{want:#?}");
    assert!(want[0].contains("TS2322"), "{want:#?}");
    let mut arranged = levels::with_files(&program, program.files.clone());
    let a = ts_r_history::arrange(&mut arranged, &[]);
    assert!(
        a.parent.is_some(),
        "the dependency level should be shared: {a:?}"
    );
    assert!(Checker::layered(&arranged, a.parent.as_ref()).is_ok());
    let (arranged, run) = ts_r_history::check_layered(&program, &[], CheckRun { threads: 2 });
    assert_eq!(lines(&arranged, &run.diagnostics), want);
}
