//! `cargo xtask ecosystem repro [<file>...]`: runs minimized repros
//! (`ecosystem/repros/<stage>/*`) through their stage worker, exactly like a package, and
//! reports whether each one still fails with the cause in its header.
//!
//! A repro is one file, or several files in `// @filename: <path>` sections. For parse and
//! transform the files form a package directory. For bundle and check they form a package
//! named `repro` in `node_modules/` (paths starting with `node_modules/` are other packages);
//! without its own `package.json`, the first file is the package's `main` (and the first
//! `.d.ts` its `types`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::report::{STAGES, StageResult, Status};
use super::{Worker, build_binaries, run_worker};
use crate::args::Args;
use crate::util::{Result, read_to_string, rel, root};

pub fn run(mut args: Args) -> Result<ExitCode> {
    let no_build = args.flag("no-build");
    let bin_dir = args
        .opt("bin-dir")?
        .unwrap_or_else(|| "target/release".into());
    let mut files: Vec<PathBuf> = args.rest().into_iter().map(PathBuf::from).collect();
    args.finish()?;
    if files.is_empty() {
        files = all_repros();
    }
    if files.is_empty() {
        return Err("no repros under ecosystem/repros".into());
    }
    let stages: Vec<String> = STAGES.iter().map(|s| s.to_string()).collect();
    let exe = build_binaries(no_build, &stages)?;
    let worker = Worker {
        exe: &exe,
        bin_dir: &bin_dir,
    };
    let tmp = std::env::temp_dir().join(format!("ts-r-ecosystem-repro-{}", std::process::id()));
    let mut fixed = 0;
    let mut manual = 0;
    for (i, file) in files.iter().enumerate() {
        let file = if file.is_absolute() {
            file.clone()
        } else {
            std::env::current_dir()?.join(file)
        };
        let stage = file
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !STAGES.contains(&stage.as_str()) {
            println!("{}: not under repros/<stage>/", rel(&file));
            continue;
        }
        let dir = tmp.join(i.to_string());
        let text = read_to_string(&file)?;
        if let Some(why) = header(&text, "Manual:") {
            println!("{:<70} manual: {why}", rel(&file));
            manual += 1;
            continue;
        }
        let expected = header(&text, "Cause:").unwrap_or_default();
        let (pkg, install) = materialize(&text, &file, &stage, &dir)?;
        let r = run_worker(
            worker,
            "repro",
            &stage,
            &pkg,
            install.as_deref(),
            &dir.join("work"),
        );
        let (state, got) = describe(&r);
        if state == "passes" {
            fixed += 1;
        }
        let matches = if r.failures.iter().any(|f| f.cause == expected) {
            "same cause"
        } else if state == "passes" {
            "fixed? delete the repro once the owner's fixture test covers it"
        } else {
            "different cause"
        };
        println!("{:<70} {state}: {got} ({matches})", rel(&file));
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    println!(
        "{} repros: {} still failing, {fixed} passing, {manual} manual",
        files.len(),
        files.len() - fixed - manual
    );
    Ok(ExitCode::SUCCESS)
}

fn describe(r: &StageResult) -> (&'static str, String) {
    match r.status {
        Status::Pass => ("passes", String::new()),
        Status::Skip => ("skipped", r.note.clone()),
        Status::Fail | Status::Error => (
            "fails",
            r.failures
                .first()
                .map(|f| f.cause.clone())
                .unwrap_or_else(|| r.note.clone()),
        ),
    }
}

fn all_repros() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for stage in STAGES {
        let dir = root().join("ecosystem/repros").join(stage);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e != "md"))
            .collect();
        files.sort();
        out.extend(files);
    }
    out
}

/// The value of a `// Key: value` header line.
pub fn header(text: &str, key: &str) -> Option<String> {
    text.lines().take(30).find_map(|l| {
        let rest = l.trim().strip_prefix("//")?.trim();
        Some(rest.strip_prefix(key)?.trim().to_string())
    })
}

/// Splits `// @filename: <path>` sections. Without markers, the whole text is `default`.
pub fn sections(text: &str, default: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if let Some(name) = t
            .strip_prefix("// @filename:")
            .or_else(|| t.strip_prefix("//@filename:"))
        {
            out.push((name.trim().to_string(), String::new()));
        } else if let Some(last) = out.last_mut() {
            last.1.push_str(line);
        }
    }
    if out.is_empty() {
        out.push((default.to_string(), text.to_string()));
    }
    out
}

/// `foo.d.ts` → `index.d.ts`, `foo.mjs` → `index.mjs`.
fn default_name(file: &Path) -> String {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("index.js");
    for ext in [".d.ts", ".d.mts", ".d.cts"] {
        if name.ends_with(ext) {
            return format!("index{ext}");
        }
    }
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("js");
    format!("index.{ext}")
}

/// Writes the repro's files; returns (package dir, install dir for bundle/check).
fn materialize(
    text: &str,
    file: &Path,
    stage: &str,
    dir: &Path,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let secs = sections(text, &default_name(file));
    let in_node_modules = matches!(stage, "bundle" | "check");
    let (base, pkg) = if in_node_modules {
        let install = dir.join("install");
        (install.clone(), install.join("node_modules/repro"))
    } else {
        (dir.join("pkg"), dir.join("pkg"))
    };
    for (name, body) in &secs {
        if name.contains("..") || name.starts_with('/') {
            return Err(format!("{}: bad @filename {name}", file.display()).into());
        }
        let target = if in_node_modules && !name.starts_with("node_modules/") {
            pkg.join(name)
        } else {
            base.join(name)
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, body)?;
    }
    std::fs::create_dir_all(&pkg)?;
    if !pkg.join("package.json").is_file() {
        let own: Vec<&String> = secs
            .iter()
            .map(|(n, _)| n)
            .filter(|n| !n.starts_with("node_modules/"))
            .collect();
        let main = own
            .iter()
            .find(|n| !n.contains(".d."))
            .map(|s| s.as_str())
            .unwrap_or("index.js");
        let mut pj = serde_json::json!({ "name": "repro", "version": "0.0.0", "main": main });
        if let Some(t) = own.iter().find(|n| n.contains(".d.")) {
            pj["types"] = serde_json::Value::String((*t).clone());
        }
        std::fs::write(pkg.join("package.json"), pj.to_string())?;
    }
    Ok((pkg, in_node_modules.then_some(base)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_and_headers() {
        let text = "// Cause: x y\n// Owner: ts_r_bundler\n// @filename: a.js\nexport {};\n// @filename: node_modules/p/index.js\nmodule.exports = 1;\n";
        assert_eq!(header(text, "Cause:").as_deref(), Some("x y"));
        let s = sections(text, "index.js");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], ("a.js".to_string(), "export {};\n".to_string()));
        assert_eq!(
            sections("let a;\n", "index.ts"),
            vec![("index.ts".to_string(), "let a;\n".to_string())]
        );
        assert_eq!(default_name(Path::new("x/y.d.ts")), "index.d.ts");
        assert_eq!(default_name(Path::new("x/y.mjs")), "index.mjs");
    }

    #[test]
    fn materializes_bundle_packages() {
        let dir = std::env::temp_dir().join(format!("ts-r-eco-repro-test-{}", std::process::id()));
        let text = "// @filename: index.mjs\nexport const a = 1;\n// @filename: node_modules/dep/index.js\nmodule.exports = 2;\n";
        let (pkg, install) = materialize(text, Path::new("b/x.mjs"), "bundle", &dir).unwrap();
        assert!(pkg.join("index.mjs").is_file());
        assert!(install.unwrap().join("node_modules/dep/index.js").is_file());
        let pj = std::fs::read_to_string(pkg.join("package.json")).unwrap();
        assert!(pj.contains("\"main\":\"index.mjs\""), "{pj}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
