//! `parse<T>` end to end through `ts-r-run` (record 0007): the fixture in
//! `crates/ts_r_guard/tests/fixtures/app` runs in Node (under `scripts/limit.sh`) and prints
//! one line per valid or invalid input, including the `ParseError` shape; the output must
//! equal `expected.txt` (which `ts_r_build/tests/guard.rs` checks for the bundle too).
//! Skipped when Node is missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

ts_r_testing::cap_memory!();

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn app() -> PathBuf {
    repo().join("crates/ts_r_guard/tests/fixtures/app")
}

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("guard-run-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

/// `ts-r-run <args>` in `dir`, under `scripts/limit.sh`, with the fixture's env vars.
fn ts_r_run(dir: &Path, cache: &Path, args: &[&str]) -> Output {
    ts_r_testing::limited(env!("CARGO_BIN_EXE_ts-r-run"), 1024, 120)
        .args(args)
        .current_dir(dir)
        .env("TS_R_CACHE_DIR", cache)
        .env("GUARD_PORT", "8080")
        .env("GUARD_DEBUG", "1")
        .env("GUARD_MODE", "dev")
        .output()
        .expect("run ts-r-run")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn parse_fixture_runs_in_node() {
    if !have_node() {
        eprintln!("skipped: node not found");
        return;
    }
    let cache = scratch("cache");
    let out = ts_r_run(&app(), &cache, &["--no-cache", "main.ts"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let expected = std::fs::read_to_string(app().join("expected.txt")).expect("expected.txt");
    let got = text(&out.stdout);
    for (i, (g, e)) in got.lines().zip(expected.lines()).enumerate() {
        assert_eq!(g, e, "line {}", i + 1);
    }
    assert_eq!(got, expected);
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn guard_errors_stop_the_run() {
    let cache = scratch("errors-cache");
    let out = ts_r_run(&app(), &cache, &["--no-cache", "errors/bad.ts"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    let codes: Vec<&str> = err
        .lines()
        .filter_map(|l| l.split("error ").nth(1))
        .filter_map(|l| l.split(':').next())
        .collect();
    assert_eq!(codes, ["TSR1501", "TSR1402", "TSR1503"], "{err}");
    assert!(
        err.contains("bad.ts(5,41): error TSR1501: Type '() => void' cannot be checked at runtime"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&cache);
}

/// Editing a type changes the validators of files that parse it, through the cache and the
/// prebuild stamp (the declaration files are inputs of the run).
#[test]
fn editing_a_type_reaches_cached_runs() {
    if !have_node() {
        eprintln!("skipped: node not found");
        return;
    }
    let dir = scratch("edit");
    let cache = scratch("edit-cache");
    for f in [
        "main.ts",
        "report.ts",
        "types.ts",
        "extra.ts",
        "models.ts",
        "validators.ts",
        "package.json",
        "tsconfig.json",
        "tsrconfig.json",
    ] {
        std::fs::copy(app().join(f), dir.join(f)).expect("copy fixture");
    }
    let first = |o: &Output| text(&o.stdout).lines().next().unwrap_or("").to_owned();
    let a = ts_r_run(&dir, &cache, &["main.ts"]);
    assert!(a.status.success(), "{}", text(&a.stderr));
    assert!(
        first(&a).starts_with("parse User valid: ok"),
        "{}",
        first(&a)
    );
    let warm = ts_r_run(&dir, &cache, &["main.ts"]);
    assert_eq!(first(&warm), first(&a));
    let types = std::fs::read_to_string(dir.join("types.ts")).expect("types");
    std::fs::write(
        dir.join("types.ts"),
        types.replacen("id: number;", "id: boolean;", 1),
    )
    .expect("edit");
    let b = ts_r_run(&dir, &cache, &["main.ts"]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    assert!(
        first(&b).starts_with("parse User valid: ParseError type=User issues=[{\"path\":[\"id\"],\"expected\":\"boolean\""),
        "{}",
        first(&b)
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&cache);
}

/// `[sound.guard]` from `ts-r.toml`, `tsrconfig.json` and the tsconfig's `"ts-r"` key (record
/// 0007, "Configuration"): `explain` changes the error, an edit reaches cached runs, and more
/// than one place is TSR0020 naming every file (the tsconfig key wins).
#[test]
fn sound_guard_options_come_from_ts_r_toml_and_tsconfig() {
    if !have_node() {
        eprintln!("skipped: node not found");
        return;
    }
    let dir = scratch("config");
    let cache = scratch("config-cache");
    for f in ["types.ts", "package.json", "tsconfig.json"] {
        std::fs::copy(app().join(f), dir.join(f)).expect("copy fixture");
    }
    std::fs::write(
        dir.join("cfg.ts"),
        "import { safeParse } from \"@ts-r.dev/guard\";\n\
import type { User } from \"./types\";\n\
const r = safeParse<User>({ id: \"x\", name: 1 });\n\
console.log(r.ok ? \"ok\" : `${r.error.issues.length} ${r.error.message}`);\n",
    )
    .expect("cfg.ts");
    let run_with = |args: &[&str]| {
        let o = ts_r_run(&dir, &cache, args);
        assert!(o.status.success(), "{}", text(&o.stderr));
        (text(&o.stdout).trim().to_owned(), text(&o.stderr))
    };
    let run = || run_with(&["cfg.ts"]);
    // Creating a config file where none was is not seen by a warm run's stamp (as for a new
    // tsconfig.json); editing one is.
    let (full, _) = run_with(&["--no-cache", "cfg.ts"]);
    assert_eq!(
        full,
        "5 Invalid User: $.id: expected number, received string (+4 more)"
    );
    let toml = |explain: &str| {
        std::fs::write(
            dir.join("ts-r.toml"),
            format!("[sound]\nlevel = \"sound\"\n\n[sound.guard]\nexplain = \"{explain}\"\n"),
        )
        .expect("ts-r.toml")
    };
    toml("first");
    assert_eq!(
        run().0,
        "1 Invalid User: $.id: expected number, received string"
    );
    toml("none");
    assert_eq!(run().0, "0 Invalid User");
    // Both places: TSR0020, and the tsconfig's key is used.
    let tsconfig = std::fs::read_to_string(dir.join("tsconfig.json")).expect("tsconfig");
    std::fs::write(
        dir.join("tsconfig.json"),
        tsconfig.replacen(
            '{',
            "{\n  \"ts-r\": { \"sound\": { \"guard\": { \"explain\": \"full\" } } },",
            1,
        ),
    )
    .expect("tsconfig");
    let (out, err) = run();
    assert_eq!(out, full);
    assert!(
        err.contains("error TSR0020: Invalid sound-mode configuration: sound mode is configured in more than one place: the \"ts-r\" key of '")
            && err.contains("ts-r.toml'; keep one."),
        "{err}"
    );
    // A tsrconfig.json as well: the error names all three files.
    std::fs::write(
        dir.join("tsrconfig.json"),
        "{ \"sound\": { \"guard\": { \"explain\": \"first\" } } }\n",
    )
    .expect("tsrconfig.json");
    let (out, err) = run_with(&["--no-cache", "cfg.ts"]); // a new config file: not in the stamp
    assert_eq!(out, full);
    assert!(
        err.contains("tsconfig.json', '") && err.contains("tsrconfig.json' and '"),
        "{err}"
    );
    // Only the tsrconfig.json: it is read.
    std::fs::remove_file(dir.join("ts-r.toml")).expect("rm ts-r.toml");
    std::fs::write(dir.join("tsconfig.json"), &tsconfig).expect("tsconfig");
    let (out, err) = run_with(&["--no-cache", "cfg.ts"]);
    assert_eq!(
        out,
        "1 Invalid User: $.id: expected number, received string"
    );
    assert!(!err.contains("TSR0020"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn env_reads_the_keys_of_an_alias_of_an_interface() {
    if !have_node() {
        eprintln!("skipped: node not found");
        return;
    }
    let dir = scratch("env-alias");
    let cache = scratch("env-alias-cache");
    for f in ["package.json", "tsconfig.json"] {
        std::fs::copy(app().join(f), dir.join(f)).expect("copy fixture");
    }
    // `Env`'s descriptor is a reference to `Base`'s: its keys are still read from the
    // environment.
    std::fs::write(
        dir.join("env.ts"),
        "import { env } from \"@ts-r.dev/guard\";\ninterface Base { GUARD_PORT: number; GUARD_MODE: \"dev\" | \"prod\" }\ntype Env = Base;\nconsole.log(JSON.stringify(env<Env>()));\n",
    )
    .expect("env.ts");
    let out = ts_r_run(&dir, &cache, &["env.ts"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "{\"GUARD_PORT\":8080,\"GUARD_MODE\":\"dev\"}\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&cache);
}
