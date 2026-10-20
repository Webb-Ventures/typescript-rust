//! The sound-mode case harness over `docs/design/0007-sound-mode/cases/` (semantics in its
//! README): one program from every `.ts` file there, `tsconfig.json` options and
//! `_shared/sound.toml`, `// @expect` markers blanked before checking. Every reject file must
//! report exactly its marked `(line, code)` multiset (TS and TSR codes), every accept file and
//! `_shared/` file nothing. Prints a per-rule table (`--nocapture` to see it) and fails when
//! fewer cases pass than pinned, or when 1 and 4 threads disagree. A second run turns on the
//! opt-in `exports-declare-throws` and checks the S-13 `// @opt-in` markers.
//!
//! Skipped when the lib files are missing (`cargo xtask upstream sync --minimal`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::code_label;
use ts_r_sound::{SoundRun, check_sound, config};

ts_r_testing::cap_memory!();

/// Cases passing today; raise it when more pass (never lower it without a reason).
const PINNED_PASSING: usize = 234;

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/0007-sound-mode/cases")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Replaces every `// @expect ...` suffix with spaces (offsets don't move).
fn blank_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        match line.find("// @expect") {
            Some(i) => {
                out.push_str(&line[..i]);
                let rest = &line[i..];
                let body = rest.trim_end_matches(['\n', '\r']);
                out.extend(std::iter::repeat_n(' ', body.len()));
                out.push_str(&rest[body.len()..]);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// `(line, code)` `@expect` markers of a file, one-based lines.
fn expectations(text: &str) -> Vec<(u32, String)> {
    markers(text, "// @expect")
}

/// `(line, code)` markers introduced by `tag`, one-based lines, sorted.
fn markers(text: &str, tag: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if let Some(j) = line.find(tag) {
            for code in line[j + tag.len()..].split_whitespace() {
                out.push((i as u32 + 1, code.to_owned()));
            }
        }
    }
    out.sort();
    out
}

struct Outcome {
    /// Case path relative to the cases dir -> actual `(line, code)` multiset.
    actual: BTreeMap<String, Vec<(u32, String)>>,
    /// Every rendered diagnostic (for the determinism check).
    rendered: Vec<String>,
}

/// `opt_in`: also turn on `exports-declare-throws` (the `// @opt-in` markers).
fn run(threads: usize, opt_in: bool) -> Option<(Outcome, BTreeMap<String, String>)> {
    let libs = LibStore::locate()?;
    let root = ts_r_testing::canonical(cases_dir()).ok()?;
    let root_s = root.to_string_lossy().replace('\\', "/");
    let mut files = Vec::new();
    collect(&root, &mut files);
    let mut fs = MemoryFs::new();
    fs.set_cwd(&root_s);
    let mut originals = BTreeMap::new();
    for p in &files {
        let ps = p.to_string_lossy().replace('\\', "/");
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        let rel = ps[root_s.len() + 1..].to_owned();
        if ps.ends_with(".ts") {
            fs.add_file(&ps, &blank_markers(&text));
            originals.insert(rel, text);
        } else {
            fs.add_file(&ps, &text);
        }
    }
    let cfg = load_tsconfig(&fs, &format!("{root_s}/tsconfig.json"));
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), threads);
    let base = check_program(&program, CheckRun { threads });
    let toml_text = std::fs::read_to_string(root.join("_shared/sound.toml")).ok()?;
    let (sound_cfg, errors) = config::parse_toml(&toml_text, &format!("{root_s}/ts-r.toml"));
    assert!(errors.is_empty(), "{errors:?}");
    let mut sound_cfg = sound_cfg?;
    sound_cfg.policy.exports_declare_throws = opt_in;
    let sound = check_sound(&SoundRun {
        program: &program,
        config: &sound_cfg,
        base: &base.diagnostics,
        threads,
        case_sensitive: true,
    });
    let mut actual: BTreeMap<String, Vec<(u32, String)>> = BTreeMap::new();
    let mut rendered = Vec::new();
    for d in base.diagnostics.iter().chain(&sound.diagnostics) {
        let Some(f) = program.files.get(d.file.0 as usize) else {
            rendered.push(format!("global {}: {}", code_label(d.code), d.message));
            continue;
        };
        if f.is_lib {
            continue;
        }
        let line = f.lines().line_of(d.span.start) + 1;
        let rel = f
            .path
            .strip_prefix(&format!("{root_s}/"))
            .unwrap_or(&f.path)
            .to_owned();
        rendered.push(format!(
            "{rel}({line}): {} {}",
            code_label(d.code),
            d.message.lines().next().unwrap_or("")
        ));
        actual
            .entry(rel)
            .or_default()
            .push((line, code_label(d.code)));
    }
    for v in actual.values_mut() {
        v.sort();
    }
    Some((Outcome { actual, rendered }, originals))
}

#[test]
fn sound_cases() {
    let Some((out, originals)) = run(4, false) else {
        eprintln!("skipped: lib files or cases not found");
        return;
    };
    // rule dir -> (accept pass, accept total, reject pass, reject total)
    let mut table: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut passing = 0;
    for (rel, text) in &originals {
        let parts: Vec<&str> = rel.split('/').collect();
        let expected = expectations(text);
        let actual = out.actual.get(rel).cloned().unwrap_or_default();
        if parts[0] == "_shared" {
            if !actual.is_empty() {
                failures.push(format!("{rel}: expected nothing, got {actual:?}"));
            }
            continue;
        }
        if parts.len() < 3 {
            continue;
        }
        let row = table.entry(parts[0].to_owned()).or_default();
        let (pass_i, total_i) = if parts[1] == "accept" { (0, 1) } else { (2, 3) };
        row[total_i] += 1;
        if expected == actual {
            row[pass_i] += 1;
            passing += 1;
        } else {
            failures.push(format!("{rel}: expected {expected:?}, got {actual:?}"));
        }
    }
    eprintln!("{:<32} {:>9} {:>9}", "rule", "accept", "reject");
    let mut tot = [0usize; 4];
    for (rule, r) in &table {
        eprintln!("{rule:<32} {:>4}/{:<4} {:>4}/{:<4}", r[0], r[1], r[2], r[3]);
        for i in 0..4 {
            tot[i] += r[i];
        }
    }
    eprintln!(
        "{:<32} {:>4}/{:<4} {:>4}/{:<4}  ({passing} of {} cases pass)",
        "total",
        tot[0],
        tot[1],
        tot[2],
        tot[3],
        tot[1] + tot[3]
    );
    for f in &failures {
        eprintln!("FAIL {f}");
    }
    let (single, _) = run(1, false).expect("second run");
    assert_eq!(out.rendered, single.rendered, "1 and 4 threads disagree");
    assert!(
        !failures.iter().any(|f| f.starts_with("_shared/")),
        "a _shared file has diagnostics"
    );
    assert!(
        passing >= PINNED_PASSING,
        "{passing} cases pass, fewer than the pinned {PINNED_PASSING}"
    );
}

/// `exports-declare-throws = true` (open question 3, off by default): every S-13 case reports
/// its `@expect` markers plus its `// @opt-in` markers, and nothing else.
#[test]
fn exports_declare_throws_opt_in() {
    let Some((out, originals)) = run(4, true) else {
        eprintln!("skipped: lib files or cases not found");
        return;
    };
    let mut failures = Vec::new();
    let mut opt_in_marks = 0;
    for (rel, text) in &originals {
        if !rel.starts_with("s13-checked-exceptions/") {
            continue;
        }
        let mut expected = expectations(text);
        let extra = markers(text, "// @opt-in");
        opt_in_marks += extra.len();
        expected.extend(extra);
        expected.sort();
        let actual = out.actual.get(rel).cloned().unwrap_or_default();
        if expected != actual {
            failures.push(format!("{rel}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(opt_in_marks >= 3, "only {opt_in_marks} '@opt-in' markers");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
