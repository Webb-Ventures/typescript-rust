//! Compares our tsconfig loading with stored `tsc --showConfig` output.
//!
//! Every directory under `tests/fixtures/` is a project. Each `tsconfig*.json` at its root is
//! loaded and compared with:
//!
//! - `expected/<name>.showconfig.txt`: `tsc --showConfig -p <name>` output,
//! - `expected/<name>.diagnostics.txt`: config diagnostics from
//!   `tsc -p <name> --listFilesOnly --pretty false`.
//!
//! The fixture's absolute path is stored as `${FIXTURE}`. Expected files were generated with
//! the TypeScript version in `tests/fixtures/TSC_VERSION.txt`. Regenerate with
//! `cargo test -p ts_r_config --test oracle -- --ignored regenerate` (uses `$TS_R_TSC` or
//! `npx -y -p typescript@7.0.2 tsc`).

use std::path::{Path, PathBuf};
use std::process::Command;

use ts_r_config::{ConfigDiagnostic, RealFs, load_tsconfig, path, show_config};

// External projects are real-world input.
ts_r_testing::cap_memory!();

const PLACEHOLDER: &str = "${FIXTURE}";

/// Codes of diagnostics that come from config handling (others are program errors).
fn is_config_code(code: u32, located_in_json: bool) -> bool {
    matches!(code, 1002 | 1005 | 1010 | 1012 | 1136 | 1327 | 1328)
        || ((5000..6000).contains(&code) && code != 5011)
        || matches!(
            code,
            6046 | 6266 | 6304 | 6379 | 17010 | 18000 | 18002 | 18003 | 18035 | 18051
        )
        || (code == 6053 && located_in_json)
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_projects() -> Vec<(PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let mut configs: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("tsconfig") && n.ends_with(".json"))
            .collect();
        configs.sort();
        out.push((d, configs));
    }
    out
}

fn norm_dir(d: &Path) -> String {
    path::normalize(&d.to_string_lossy())
}

fn our_diagnostics(fixture: &str, diags: &[ConfigDiagnostic]) -> Vec<String> {
    let mut lines: Vec<String> = diags
        .iter()
        .filter(|d| {
            let in_json = d.file.as_deref().is_some_and(|f| f.ends_with(".json"));
            is_config_code(d.code, in_json)
        })
        .map(|d| {
            d.render(
                |f| path::relative(fixture, f, false),
                |f| std::fs::read_to_string(f).ok(),
            )
        })
        .map(|s| s.replace(fixture, PLACEHOLDER))
        .collect();
    lines.sort();
    lines
}

/// Parses tsc's non-pretty output into one string per diagnostic (continuation lines
/// joined with `\n`), keeping config diagnostics only.
fn parse_tsc_diagnostics(output: &str) -> Vec<String> {
    let mut diags: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if line.starts_with("  ") {
            if let Some(c) = &mut current {
                c.push('\n');
                c.push_str(line);
            }
            continue;
        }
        if let Some(c) = current.take() {
            diags.push(c);
        }
        if line.contains("error TS") {
            current = Some(line.to_owned());
        }
    }
    if let Some(c) = current {
        diags.push(c);
    }
    let mut kept: Vec<String> = diags
        .into_iter()
        .filter(|d| {
            let first = d.lines().next().unwrap_or("");
            let code: u32 = first
                .split("error TS")
                .nth(1)
                .and_then(|r| r.split(':').next())
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let in_json = first
                .split('(')
                .next()
                .is_some_and(|f| f.ends_with(".json"));
            is_config_code(code, in_json)
        })
        // Drop "The file is in the program because" trailers (program-level context).
        .map(|d| {
            d.split("\n  The file is in the program because:")
                .next()
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    kept.sort();
    kept
}

#[test]
fn fixtures_match_tsc_show_config() {
    let fs = RealFs::new();
    let mut failures = Vec::new();
    let mut compared = 0;
    for (dir, configs) in fixture_projects() {
        let fixture = norm_dir(&dir);
        for cfg_name in configs {
            let expected_show = dir
                .join("expected")
                .join(format!("{cfg_name}.showconfig.txt"));
            let expected_diag = dir
                .join("expected")
                .join(format!("{cfg_name}.diagnostics.txt"));
            let Ok(want_show) = std::fs::read_to_string(&expected_show) else {
                failures.push(format!(
                    "{fixture}/{cfg_name}: missing {}",
                    expected_show.display()
                ));
                continue;
            };
            // An expected output that climbs four directories up depends on where the
            // checkout is (an absolute path made relative), and on Windows, where `/x` has
            // no drive: such an input can't be a fixture.
            if want_show.contains("../../../../") {
                failures.push(format!(
                    "{fixture}/{cfg_name}: the expected showConfig depends on the checkout's location"
                ));
                continue;
            }
            let parsed = load_tsconfig(&fs, &path::combine(&fixture, &cfg_name));
            let got_show = show_config(&parsed).replace(&fixture, PLACEHOLDER);
            if got_show.trim_end() != want_show.trim_end() {
                failures.push(format!(
                    "{}/{cfg_name}: showConfig differs\n--- expected\n{}\n--- got\n{}",
                    dir.file_name().unwrap().to_string_lossy(),
                    want_show.trim_end(),
                    got_show
                ));
            }
            let want_diag: Vec<String> = std::fs::read_to_string(&expected_diag)
                .unwrap_or_default()
                .split("\n\n")
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim_end().to_owned())
                .collect();
            let got_diag = our_diagnostics(&fixture, &parsed.all_diagnostics());
            if got_diag != want_diag {
                failures.push(format!(
                    "{}/{cfg_name}: diagnostics differ\n--- expected\n{}\n--- got\n{}",
                    dir.file_name().unwrap().to_string_lossy(),
                    want_diag.join("\n"),
                    got_diag.join("\n")
                ));
            }
            compared += 1;
        }
    }
    assert!(compared > 0, "no fixtures found");
    assert!(
        failures.is_empty(),
        "{} of {compared} configs differ:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// tsc under `scripts/limit.sh`, which the root AGENTS.md requires for oracle runs (limit.sh
/// is bash: Windows runs tsc directly).
fn tsc_command() -> Command {
    let (program, args): (String, &[&str]) = match std::env::var("TS_R_TSC") {
        Ok(p) => (p, &[]),
        Err(_) => ("npx".into(), &["-y", "-p", "typescript@7.0.2", "tsc"]),
    };
    let mut c = if cfg!(windows) {
        Command::new(program)
    } else {
        let limit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/limit.sh");
        let mut c = Command::new(limit);
        c.arg("--").arg(program);
        c
    };
    c.args(args);
    c
}

#[test]
#[ignore = "runs the real tsc; use to regenerate expected outputs"]
fn regenerate() {
    let version = tsc_command().arg("--version").output().expect("run tsc");
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    std::fs::write(
        fixtures_dir().join("TSC_VERSION.txt"),
        format!("{version}\n"),
    )
    .unwrap();
    for (dir, configs) in fixture_projects() {
        let fixture = norm_dir(&dir);
        std::fs::create_dir_all(dir.join("expected")).unwrap();
        for cfg_name in configs {
            let show = tsc_command()
                .current_dir(&dir)
                .args(["--showConfig", "-p", &cfg_name])
                .output()
                .expect("run tsc");
            let text = String::from_utf8_lossy(&show.stdout).replace(&fixture, PLACEHOLDER);
            std::fs::write(
                dir.join("expected")
                    .join(format!("{cfg_name}.showconfig.txt")),
                format!("{}\n", text.trim_end()),
            )
            .unwrap();
            let run = tsc_command()
                .current_dir(&dir)
                .args(["-p", &cfg_name, "--listFilesOnly", "--pretty", "false"])
                .output()
                .expect("run tsc");
            let out = format!(
                "{}{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            )
            .replace(&fixture, PLACEHOLDER);
            let diags = parse_tsc_diagnostics(&out);
            let body = if diags.is_empty() {
                String::new()
            } else {
                format!("{}\n", diags.join("\n\n"))
            };
            std::fs::write(
                dir.join("expected")
                    .join(format!("{cfg_name}.diagnostics.txt")),
                body,
            )
            .unwrap();
        }
    }
}

fn find_tsconfigs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if name != "node_modules" && !name.starts_with('.') {
                find_tsconfigs(&e.path(), out);
            }
        } else if name.starts_with("tsconfig") && name.ends_with(".json") {
            out.push(e.path());
        }
    }
}

/// Live comparison against real projects: every `tsconfig*.json` under the directories in
/// `$TS_R_EXTERNAL_PROJECTS` (`:`-separated).
/// `TS_R_EXTERNAL_PROJECTS=/path/to/repo cargo test -p ts_r_config --test oracle -- --ignored external`
#[test]
#[ignore = "needs external projects and the real tsc"]
fn external_projects_match_tsc() {
    let roots = std::env::var("TS_R_EXTERNAL_PROJECTS").unwrap_or_default();
    let fs = RealFs::new();
    let mut failures = Vec::new();
    let mut compared = 0;
    for root in roots.split(':').filter(|r| !r.is_empty()) {
        let mut configs = Vec::new();
        find_tsconfigs(Path::new(root), &mut configs);
        for cfg in configs {
            let dir = cfg.parent().unwrap();
            let name = cfg.file_name().unwrap().to_string_lossy().into_owned();
            let show = tsc_command()
                .current_dir(dir)
                .args(["--showConfig", "-p", &name])
                .output()
                .expect("run tsc");
            let want = String::from_utf8_lossy(&show.stdout).trim_end().to_owned();
            let parsed = load_tsconfig(&fs, &path::normalize(&cfg.to_string_lossy()));
            let got = show_config(&parsed);
            compared += 1;
            if got != want {
                failures.push(format!(
                    "{}\n--- tsc\n{want}\n--- ts-r\n{got}",
                    cfg.display()
                ));
                continue;
            }
            let run = tsc_command()
                .current_dir(dir)
                .args(["-p", &name, "--listFilesOnly", "--pretty", "false"])
                .output()
                .expect("run tsc");
            let out = format!(
                "{}{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            let want_diag = parse_tsc_diagnostics(&out);
            let fixture = path::normalize(&dir.to_string_lossy());
            let got_diag: Vec<String> = our_diagnostics(&fixture, &parsed.all_diagnostics())
                .into_iter()
                .map(|d| d.replace(PLACEHOLDER, &fixture))
                .collect();
            if got_diag != want_diag {
                failures.push(format!(
                    "{} diagnostics\n--- tsc\n{}\n--- ts-r\n{}",
                    cfg.display(),
                    want_diag.join("\n"),
                    got_diag.join("\n")
                ));
            }
        }
    }
    eprintln!("compared {compared} configs, {} mismatches", failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
