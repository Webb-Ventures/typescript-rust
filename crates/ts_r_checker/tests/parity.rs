//! Fixture tests for differences found on real projects (editor parity, ecosystem check).
//!
//! Each `tests/parity/<name>.ts` is a small project; `<name>.txt` holds tsc 7.0.2's
//! `--pretty false` output for it (empty when tsc reports nothing). A fixture may split into
//! files with `// @filename: <path>` lines (TypeScript's test format; the text before the
//! first one is `a.ts`). A `tsconfig.json` section replaces the default config, which is the
//! private-app base: strict, `noUncheckedIndexedAccess`, ES2023, bundler resolution,
//! `moduleDetection: force`. Files are checked under `/p` with every non-config file listed.
//!
//! To add one: put the reproduction in a directory with the config, run
//! `npx -y -p typescript@7.0.2 tsc -p . --pretty false > <name>.txt`, copy both here.
//! Skipped when the lib files are not available (`cargo xtask upstream sync --minimal`).

use std::path::Path;

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::{FileId, Files, format_plain};
use ts_r_span::SourceText;

ts_r_testing::cap_memory!();

const DEFAULT_OPTIONS: &str = r#""strict": true, "noUncheckedIndexedAccess": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true"#;

struct Shown(Vec<Option<SourceText>>);

impl Files for Shown {
    fn source(&self, file: FileId) -> Option<&SourceText> {
        self.0.get(file.0 as usize).and_then(|s| s.as_ref())
    }
}

fn split_files(text: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = vec![("a.ts".to_owned(), String::new())];
    for line in text.split_inclusive('\n') {
        if let Some(name) = line.trim().strip_prefix("// @filename:") {
            files.push((name.trim().to_owned(), String::new()));
        } else if let Some(last) = files.last_mut() {
            last.1.push_str(line);
        }
    }
    files.retain(|(n, t)| !(n == "a.ts" && t.trim().is_empty()));
    files
}

fn run(fixture: &Path, libs: &LibStore) -> String {
    let text = std::fs::read_to_string(fixture).unwrap_or_default();
    let files = split_files(&text);
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    let mut names = Vec::new();
    let mut has_config = false;
    for (name, body) in &files {
        fs.add_file(&format!("/p/{name}"), body);
        if name == "tsconfig.json" {
            has_config = true;
        } else if !name.starts_with("node_modules/") {
            names.push(format!("\"{name}\""));
        }
    }
    if !has_config {
        fs.add_file(
            "/p/tsconfig.json",
            &format!(
                "{{\"compilerOptions\": {{{DEFAULT_OPTIONS}}}, \"files\": [{}]}}",
                names.join(", ")
            ),
        );
    }
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(libs), 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    let shown = Shown(
        program
            .files
            .iter()
            .map(|f| {
                (!f.is_lib).then(|| {
                    let name = f.path.strip_prefix("/p/").unwrap_or(&f.path).to_owned();
                    SourceText::new(name, f.text.clone())
                })
            })
            .collect(),
    );
    let mut out = String::new();
    for d in &result.diagnostics {
        out.push_str(&format_plain(d, &shown));
        out.push('\n');
    }
    out
}

#[test]
fn parity_fixtures() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    let mut fixtures: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/parity")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .collect();
    fixtures.sort();
    assert!(!fixtures.is_empty());
    let mut failures = Vec::new();
    for f in &fixtures {
        let expected = std::fs::read_to_string(f.with_extension("txt")).unwrap_or_default();
        let actual = run(f, &libs);
        if actual.trim_end() != expected.trim_end() {
            failures.push(format!(
                "{}\n--- expected (tsc 7)\n{}--- actual\n{}",
                f.display(),
                expected,
                actual
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
