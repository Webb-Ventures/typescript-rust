//! Export shapes decide which files an edit can affect.

use std::path::PathBuf;
use std::sync::Arc;

use ts_r_checker::program::bind_options_for;
use ts_r_checker::{LibStore, ProgramFiles, SourceFile, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_history::shape::ProgramShapes;

ts_r_testing::cap_memory!();

fn load(libs: &LibStore) -> ProgramFiles {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/app/tsconfig.json");
    let config = p.to_string_lossy().replace('\\', "/");
    let fs = RealFs::new();
    let cfg = load_tsconfig(&fs, &config);
    load_program(&fs, &cfg.file_names, &cfg.options, Some(libs), 1)
}

/// Replaces the text of the file whose path ends with `suffix`; returns its path.
fn edit(program: &mut ProgramFiles, suffix: &str, from: &str, to: &str) -> String {
    let i = program
        .files
        .iter()
        .position(|f| f.path.ends_with(suffix))
        .expect("file");
    let old = &program.files[i];
    assert!(old.text.contains(from), "{suffix} has no {from:?}");
    let text = old.text.replacen(from, to, 1);
    let opts = bind_options_for::<RealFs>(&old.path, &program.options, None);
    let f = SourceFile::new(old.path.clone(), text, false, &opts);
    let path = f.path.clone();
    program.files[i] = Arc::new(f);
    path
}

fn names(paths: &[String]) -> Vec<String> {
    let mut v: Vec<String> = paths
        .iter()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn body_edits_affect_nobody() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let mut program = load(&libs);
    let mut shapes = ProgramShapes::build(&program, 2);
    let all: Vec<String> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.clone())
        .collect();
    let p = edit(&mut program, "src/math.ts", "a.x + b.x", "b.x + a.x");
    assert!(shapes.update(&program, &[p], &all).is_empty());
}

#[test]
fn export_edits_affect_users_only() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let mut program = load(&libs);
    let mut shapes = ProgramShapes::build(&program, 2);
    let all: Vec<String> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.clone())
        .collect();
    // `Point` is used by shapes.ts (and through `Box`/`Shape` by their users), not by util.ts.
    let p = edit(
        &mut program,
        "src/math.ts",
        "y: number;\n}",
        "y: number;\n  z?: number;\n}",
    );
    let hit = names(&shapes.update(&program, &[p], &all));
    assert!(hit.contains(&"shapes.ts".to_owned()), "{hit:?}");
    assert!(hit.contains(&"main.ts".to_owned()), "{hit:?}");
    assert!(!hit.contains(&"util.ts".to_owned()), "{hit:?}");
    assert!(!hit.contains(&"env.d.ts".to_owned()), "{hit:?}");
    // A global contributes to every user of the name.
    let p = edit(
        &mut program,
        "src/globals.d.ts",
        "var APP_NAME: string",
        "var APP_NAME: number",
    );
    let hit = names(&shapes.update(&program, &[p], &all));
    assert!(hit.contains(&"util.ts".to_owned()), "{hit:?}");
    assert!(!hit.contains(&"math.ts".to_owned()), "{hit:?}");
}

#[test]
fn an_export_edit_spares_users_of_other_exports() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let mut program = load(&libs);
    let mut shapes = ProgramShapes::build(&program, 2);
    let all: Vec<String> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.clone())
        .collect();
    // `sum` is imported by shapes.ts only; main.ts imports `pick` from math.ts and types of
    // shapes.ts whose signatures do not mention `sum`.
    let p = edit(
        &mut program,
        "src/math.ts",
        "sum(values: number[]): number",
        "sum(values: number[]): string",
    );
    let hit = names(&shapes.update(&program, &[p], &all));
    assert_eq!(hit, ["shapes.ts"], "{hit:?}");
    // A new export affects only broken.ts, whose missing import's error suggests names.
    let p = edit(
        &mut program,
        "src/math.ts",
        "export const ORIGIN",
        "export const EXTRA = 1;\nexport const ORIGIN",
    );
    let hit = names(&shapes.update(&program, &[p], &all));
    assert_eq!(hit, ["broken.ts"], "{hit:?}");
    // `ORIGIN: Point` depends on its sibling `Point`.
    let p = edit(
        &mut program,
        "src/math.ts",
        "  y: number;\n}",
        "  y: string;\n}",
    );
    let hit = names(&shapes.update(&program, &[p], &all));
    assert!(hit.contains(&"shapes.ts".to_owned()), "{hit:?}");
    assert!(hit.contains(&"main.ts".to_owned()), "{hit:?}");
}

/// An import of a name the target declares but does not export is TS2459; once the local is
/// renamed it is TS2305. The importer must be re-checked although the exports are unchanged
/// (differential seeds 2, 4, 8, ... before the shape hashed module locals).
#[test]
fn renaming_an_unexported_local_reaches_its_importers() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let mut program = load(&libs);
    let all: Vec<String> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.path.clone())
        .collect();
    edit(
        &mut program,
        "src/util.ts",
        "export const banner",
        "const banner",
    );
    let mut shapes = ProgramShapes::build(&program, 2);
    let p = edit(&mut program, "src/util.ts", "const banner", "const sum");
    let affected = shapes.update(&program, &[p], &all);
    assert_eq!(names(&affected), ["main.ts"]);
    // A body edit of the same local still affects nobody.
    let mut shapes = ProgramShapes::build(&program, 2);
    let q = edit(
        &mut program,
        "src/util.ts",
        "${APP_NAME} ${APP_VERSION}",
        "${APP_VERSION}",
    );
    assert!(shapes.update(&program, &[q], &all).is_empty());
}
