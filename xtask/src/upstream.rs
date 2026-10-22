//! `cargo xtask upstream {check,bump,sync,diff}`: keep ts-r in step with TypeScript 7.
//!
//! Only data is ever synced from upstream: conformance/compiler tests, their expected
//! baselines, `lib.*.d.ts` files and the diagnostic message catalog. Never compiler source.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::args::Args;
use crate::util::{
    Result, Version, http_json, read_to_string, rel, root, run_capture, run_status, today,
    write_output,
};

pub const UPSTREAM_JSON: &str = "upstream/UPSTREAM.json";
pub const COMPAT_JSON: &str = "docs/state/compat.json";
/// Where `upstream sync` puts the TypeScript checkout (inside the gitignored `target/`).
pub const TS_DIR: &str = "target/upstream/typescript";

#[derive(Debug, Serialize, Deserialize)]
pub struct UpstreamFile {
    #[serde(rename = "$comment", default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(flatten)]
    pub pins: BTreeMap<String, Pin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    /// GitHub `owner/name`.
    pub repo: String,
    /// npm package name, when the upstream is released on npm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    /// Pinned npm version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Branch followed when there are no releases (test262).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Pinned git commit in `repo`.
    pub commit: String,
    /// Commit date (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// `gitHead` recorded in the npm package (may name a commit in another repo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm_git_head: Option<String>,
    /// What we use this upstream for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,
    /// Sparse-checkout patterns for `upstream sync` (data files only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync: Vec<String>,
    /// Subset of `sync` used by `upstream sync --minimal` (libs + diagnostics).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_minimal: Vec<String>,
}

pub fn load_pins() -> Result<UpstreamFile> {
    let path = root().join(UPSTREAM_JSON);
    let text = read_to_string(&path)?;
    let file: UpstreamFile =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", rel(&path)))?;
    if !file.pins.contains_key("typescript") {
        return Err(format!("{UPSTREAM_JSON} has no `typescript` pin").into());
    }
    Ok(file)
}

pub fn ts_pin(file: &UpstreamFile) -> &Pin {
    &file.pins["typescript"]
}

fn save_pins(file: &UpstreamFile) -> Result<()> {
    let text = serde_json::to_string_pretty(file)? + "\n";
    std::fs::write(root().join(UPSTREAM_JSON), text)?;
    Ok(())
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    let Some(sub) = args.positional() else {
        return Err("usage: cargo xtask upstream <check|bump|sync|diff> ...".into());
    };
    match sub.as_str() {
        "check" => check(args),
        "bump" => bump(args),
        "sync" => sync(args),
        "diff" => diff(args),
        other => Err(format!("unknown upstream subcommand `{other}`").into()),
    }
}

// ---------------------------------------------------------------------------------------------
// check

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct TsStatus {
    pinned: String,
    latest_stable: Option<String>,
    latest_candidate: Option<String>,
    nightly: Option<String>,
    latest_tag: Option<String>,
    stable_bump: bool,
    candidate_bump: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OtherStatus {
    name: String,
    pinned: String,
    latest: String,
    behind: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckReport {
    typescript: TsStatus,
    others: Vec<OtherStatus>,
}

/// Picks the newest stable and newest RC/beta at or above `pinned`'s major version.
fn classify_versions<'a>(
    versions: impl Iterator<Item = &'a str>,
    pinned: &Version,
) -> (Option<Version>, Option<Version>) {
    let mut stable: Option<Version> = None;
    let mut candidate: Option<Version> = None;
    for v in versions.filter_map(Version::parse) {
        if v.major < pinned.major {
            continue;
        }
        if v.is_stable() {
            if stable.as_ref().is_none_or(|s| v > *s) {
                stable = Some(v);
            }
        } else if v.is_candidate() && candidate.as_ref().is_none_or(|c| v > *c) {
            candidate = Some(v);
        }
    }
    (stable, candidate)
}

fn check(mut args: Args) -> Result<ExitCode> {
    let json = args.flag("json");
    let include_rc = args.flag("include-rc");
    args.finish()?;
    let file = load_pins()?;
    let ts = ts_pin(&file);
    let pinned_str = ts
        .version
        .clone()
        .ok_or("typescript pin has no `version`")?;
    let pinned = Version::parse(&pinned_str).ok_or("typescript pin `version` is not semver")?;

    // npm: abbreviated metadata keeps the download small.
    let npm_name = ts.npm.as_deref().unwrap_or("typescript");
    let meta = http_json(
        &format!("https://registry.npmjs.org/{npm_name}"),
        Some("application/vnd.npm.install-v1+json"),
    )?;
    let versions: Vec<&str> = meta["versions"]
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let (stable, candidate) = classify_versions(versions.iter().copied(), &pinned);
    let nightly = meta["dist-tags"]["next"].as_str().map(String::from);

    // GitHub tags (newest first by API order; we take the max semver).
    let tags = http_json(
        &format!("https://api.github.com/repos/{}/tags?per_page=100", ts.repo),
        Some("application/vnd.github+json"),
    );
    let latest_tag = match &tags {
        Ok(Value::Array(items)) => items
            .iter()
            .filter_map(|t| t["name"].as_str())
            .filter(|n| n.starts_with('v'))
            .filter_map(|n| Version::parse(n).map(|v| (v, n.to_string())))
            .filter(|(v, _)| v.is_stable() || v.is_candidate())
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, n)| n),
        Ok(_) => None,
        Err(e) => {
            eprintln!("warning: could not list GitHub tags: {e}");
            None
        }
    };

    let stable_bump = stable.as_ref().is_some_and(|s| *s > pinned);
    let candidate_bump = candidate
        .as_ref()
        .is_some_and(|c| *c > pinned && stable.as_ref().is_none_or(|s| c > s));
    let ts_status = TsStatus {
        pinned: pinned_str,
        latest_stable: stable.map(|v| v.to_string()),
        latest_candidate: candidate.map(|v| v.to_string()),
        nightly,
        latest_tag,
        stable_bump,
        candidate_bump,
    };

    let mut others = Vec::new();
    for (name, pin) in &file.pins {
        if name == "typescript" {
            continue;
        }
        match check_other(pin) {
            Ok((pinned, latest)) => others.push(OtherStatus {
                name: name.clone(),
                behind: pinned != latest,
                pinned,
                latest,
            }),
            Err(e) => eprintln!("warning: could not check {name}: {e}"),
        }
    }

    let report = CheckReport {
        typescript: ts_status,
        others,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_check(&report);
    }
    let blocking =
        report.typescript.stable_bump || (include_rc && report.typescript.candidate_bump);
    Ok(if blocking {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn check_other(pin: &Pin) -> Result<(String, String)> {
    if let (Some(npm), Some(version)) = (&pin.npm, &pin.version) {
        let meta = http_json(
            &format!("https://registry.npmjs.org/{npm}"),
            Some("application/vnd.npm.install-v1+json"),
        )?;
        let latest = meta["dist-tags"]["latest"]
            .as_str()
            .ok_or("no dist-tags.latest")?;
        return Ok((version.clone(), latest.to_string()));
    }
    let branch = pin.branch.as_deref().unwrap_or("main");
    let commit = http_json(
        &format!("https://api.github.com/repos/{}/commits/{branch}", pin.repo),
        Some("application/vnd.github+json"),
    )?;
    let sha = commit["sha"].as_str().ok_or("no sha in GitHub response")?;
    let short = |s: &str| s.chars().take(12).collect::<String>();
    Ok((short(&pin.commit), short(sha)))
}

fn print_check(r: &CheckReport) {
    let ts = &r.typescript;
    let none = || "-".to_string();
    println!("TypeScript (pinned {})", ts.pinned);
    println!(
        "  latest stable:    {}{}",
        ts.latest_stable.clone().unwrap_or_else(none),
        if ts.stable_bump {
            "  <- BUMP AVAILABLE"
        } else {
            ""
        }
    );
    println!(
        "  latest RC/beta:   {}{}",
        ts.latest_candidate.clone().unwrap_or_else(none),
        if ts.candidate_bump {
            "  <- newer candidate (tracked)"
        } else {
            ""
        }
    );
    println!(
        "  nightly (next):   {}",
        ts.nightly.clone().unwrap_or_else(none)
    );
    println!(
        "  latest git tag:   {}",
        ts.latest_tag.clone().unwrap_or_else(none)
    );
    println!("Other upstreams (bumped weekly by Renovate, non-blocking)");
    for o in &r.others {
        println!(
            "  {:<18} pinned {:<14} latest {:<14} {}",
            o.name,
            o.pinned,
            o.latest,
            if o.behind { "behind" } else { "up to date" }
        );
    }
}

// ---------------------------------------------------------------------------------------------
// bump

fn bump(mut args: Args) -> Result<ExitCode> {
    let to = args.opt("to")?;
    args.finish()?;
    let mut file = load_pins()?;
    let ts = ts_pin(&file).clone();
    let npm_name = ts.npm.clone().unwrap_or_else(|| "typescript".into());
    let version = match to {
        Some(v) => Version::parse(&v).ok_or_else(|| format!("`{v}` is not a version"))?,
        None => {
            let pinned = Version::parse(ts.version.as_deref().unwrap_or("0.0.0"))
                .ok_or("bad pinned version")?;
            let meta = http_json(
                &format!("https://registry.npmjs.org/{npm_name}"),
                Some("application/vnd.npm.install-v1+json"),
            )?;
            let versions: Vec<&str> = meta["versions"]
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            classify_versions(versions.iter().copied(), &pinned)
                .0
                .ok_or("no stable version found on npm")?
        }
    };
    let v = version.to_string();
    let npm_meta = http_json(&format!("https://registry.npmjs.org/{npm_name}/{v}"), None)?;
    let tag = format!("v{v}");
    let commit = http_json(
        &format!("https://api.github.com/repos/{}/commits/{tag}", ts.repo),
        Some("application/vnd.github+json"),
    )
    .map_err(|e| format!("tag {tag} not found in {}: {e}", ts.repo))?;
    let sha = commit["sha"].as_str().ok_or("no sha")?.to_string();
    let date = commit["commit"]["committer"]["date"]
        .as_str()
        .map(|d| d.chars().take(10).collect());

    // Data files move between TypeScript layouts; re-derive the sparse patterns from the tree.
    let dir = root().join(TS_DIR);
    ensure_repo(&dir, &ts.repo)?;
    fetch_ref(&dir, &sha)?;
    let patterns = existing_patterns(&dir, &sha)?;
    if !patterns
        .iter()
        .any(|p| p.contains("iagnosticMessages.json"))
    {
        return Err(format!(
            "no diagnostic catalog found at {sha}; the upstream layout changed again - add the \
             new locations to DATA_CANDIDATES in xtask/src/upstream.rs (data files only)"
        )
        .into());
    }

    let old = ts.version.clone().unwrap_or_default();
    let pin = file.pins.get_mut("typescript").ok_or("no typescript pin")?;
    if pin.sync != patterns {
        println!("sync patterns changed: {:?} -> {:?}", pin.sync, patterns);
    }
    pin.sync_minimal = patterns.iter().filter(|p| is_minimal(p)).cloned().collect();
    pin.sync = patterns;
    pin.version = Some(v.clone());
    pin.tag = Some(tag);
    pin.commit = sha.clone();
    pin.date = date;
    pin.npm_git_head = npm_meta["gitHead"].as_str().map(String::from);
    save_pins(&file)?;

    update_compat(|compat| {
        let ts = &mut compat["typescript"];
        ts["pinned"] = Value::from(v.clone());
        ts["commit"] = Value::from(sha.clone());
        ts["supported"] = Value::from(format!("{}.{}.x", version.major, version.minor));
    })?;
    println!("typescript: {old} -> {v} ({sha})");
    Ok(ExitCode::SUCCESS)
}

fn update_compat(f: impl FnOnce(&mut Value)) -> Result<()> {
    let path = root().join(COMPAT_JSON);
    let mut compat: Value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{COMPAT_JSON}: {e}"))?,
        Err(_) => serde_json::json!({ "typescript": {} }),
    };
    if !compat["typescript"].is_object() {
        compat["typescript"] = serde_json::json!({});
    }
    f(&mut compat);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&compat)? + "\n")?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// sync

/// Paths we are allowed to copy from upstream: tests, baselines, libs, diagnostics, notices.
pub fn is_data_path(pattern: &str) -> bool {
    let p = pattern.trim_start_matches('/');
    const DATA_PREFIXES: &[&str] = &[
        "tests/cases/",
        "tests/baselines/reference/",
        "tests/lib/",
        "tsc/testdata/",
        "src/lib/",
        "harness/", // test262
        "test/",    // test262 / other test suites
    ];
    DATA_PREFIXES.iter().any(|d| p.starts_with(d))
        || (p.starts_with("tsc/internal/bundled/libs/") && p.ends_with(".d.ts"))
        || p.ends_with("diagnosticMessages.json")
        || p.ends_with("DiagnosticMessages.json")
        || matches!(
            p,
            "LICENSE" | "LICENSE.txt" | "ThirdPartyNoticeText.txt" | "NOTICE.txt"
        )
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd
}

fn ensure_repo(dir: &Path, repo: &str) -> Result<()> {
    if dir.join(".git").exists() {
        // A partial restore (e.g. a CI cache of `target/`) can leave a `.git` git doesn't accept,
        // and then `git -C dir` silently runs against the enclosing repo. Start over in that case.
        let top = run_capture(git(dir).args(["rev-parse", "--show-toplevel"])).ok();
        let is_top = top
            .and_then(|t| std::fs::canonicalize(t.trim()).ok())
            .zip(std::fs::canonicalize(dir).ok())
            .is_some_and(|(t, d)| t == d);
        if is_top {
            return Ok(());
        }
        eprintln!(
            "{}: not a usable git checkout, re-creating it",
            dir.display()
        );
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    run_capture(git(dir).args(["init", "--quiet"]))?;
    run_capture(git(dir).args([
        "remote",
        "add",
        "origin",
        &format!("https://github.com/{repo}.git"),
    ]))?;
    Ok(())
}

/// Shallow, blob-less fetch of one ref; returns the commit SHA.
fn fetch_ref(dir: &Path, reference: &str) -> Result<String> {
    if let Ok(sha) = run_capture(git(dir).args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{reference}^{{commit}}"),
    ])) {
        let sha = sha.trim();
        if sha.len() == 40 && sha.starts_with(reference) {
            return Ok(sha.to_string());
        }
    }
    eprintln!("fetching {reference} ...");
    run_status(git(dir).args([
        "fetch",
        "--quiet",
        "--depth",
        "1",
        "--filter=blob:none",
        "--no-tags",
        "origin",
        reference,
    ]))?;
    let sha = run_capture(git(dir).args(["rev-parse", "FETCH_HEAD^{commit}"]))?;
    Ok(sha.trim().to_string())
}

fn head(dir: &Path) -> Option<String> {
    run_capture(git(dir).args(["rev-parse", "--verify", "--quiet", "HEAD"]))
        .ok()
        .map(|s| s.trim().to_string())
}

fn sync(mut args: Args) -> Result<ExitCode> {
    let minimal = args.flag("minimal");
    let force = args.flag("force");
    let commit_override = args.opt("commit")?;
    let dest = args.opt("dest")?;
    args.finish()?;

    let file = load_pins()?;
    let ts = ts_pin(&file);
    let patterns = if minimal { &ts.sync_minimal } else { &ts.sync };
    if patterns.is_empty() {
        return Err("typescript pin has no sync patterns".into());
    }
    if let Some(bad) = patterns.iter().find(|p| !is_data_path(p)) {
        return Err(format!(
            "refusing to sync `{bad}`: only tests, baselines, lib files and diagnostics may be \
             copied from upstream (the from-scratch rule)"
        )
        .into());
    }
    let commit = commit_override.unwrap_or_else(|| ts.commit.clone());
    let dir = match dest {
        Some(d) => PathBuf::from(d),
        None => root().join(TS_DIR),
    };

    ensure_repo(&dir, &ts.repo)?;
    let mut set = git(&dir);
    set.args(["sparse-checkout", "set", "--no-cone"])
        .args(patterns);
    run_capture(&mut set)?;

    let current = head(&dir);
    if current.as_deref() == Some(commit.as_str()) && !force {
        println!("{} already at {commit} (patterns re-applied)", rel(&dir));
    } else {
        let sha = fetch_ref(&dir, &commit)?;
        eprintln!(
            "checking out {sha} (sparse: {} patterns) ...",
            patterns.len()
        );
        run_status(git(&dir).args([
            "-c",
            "advice.detachedHead=false",
            "-c",
            "core.longpaths=true",
            "checkout",
            "--quiet",
            "--force",
            "--detach",
            &sha,
        ]))?;
    }

    // `-t` tags entries; `H` = checked out, `S` = outside the sparse patterns (not on disk).
    let listed = run_capture(git(&dir).args(["ls-files", "-t"]))?;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for line in listed.lines().filter_map(|l| l.strip_prefix("H ")) {
        let area = patterns
            .iter()
            .map(|p| p.trim_start_matches('/').split('*').next().unwrap_or(""))
            .find(|p| line.starts_with(p))
            .unwrap_or("other");
        *counts.entry(area).or_default() += 1;
    }
    println!("synced microsoft/TypeScript@{commit} into {}", rel(&dir));
    for (area, n) in &counts {
        println!("  {n:>7}  {area}");
    }

    if dest_is_default(&dir) {
        let synced = commit.clone();
        update_compat(|compat| {
            let last = &compat["lastSync"];
            if last["commit"].as_str() != Some(synced.as_str()) {
                compat["lastSync"] = serde_json::json!({ "date": today(), "commit": synced });
            }
        })?;
    }
    Ok(ExitCode::SUCCESS)
}

fn dest_is_default(dir: &Path) -> bool {
    dir == root().join(TS_DIR)
}

// ---------------------------------------------------------------------------------------------
// diff

/// Every place a TypeScript data file has lived (the 7.0 tree and the later `tsc/`-only tree).
/// `bump` picks the ones that exist at the new commit; `diff` looks at all of them so files
/// that moved between layouts show up as moves, not as mass deletions.
pub const DATA_CANDIDATES: &[&str] = &[
    "/LICENSE.txt",
    "/NOTICE.txt",
    "/ThirdPartyNoticeText.txt",
    "/src/lib/",
    "/tsc/internal/bundled/libs/*.d.ts",
    "/src/compiler/diagnosticMessages.json",
    "/tsc/internal/diagnostics/diagnosticMessages.json",
    "/tsc/internal/diagnostics/extraDiagnosticMessages.json",
    "/tests/lib/",
    "/tests/cases/",
    "/tests/baselines/reference/",
    "/tsc/testdata/tests/",
    "/tsc/testdata/baselines/reference/",
    "/tsc/testdata/submoduleAccepted.txt",
];

/// Candidates used by `sync --minimal`: notices, libs and diagnostics.
pub fn is_minimal(pattern: &str) -> bool {
    pattern.contains("LICENSE")
        || pattern.contains("NOTICE")
        || pattern.contains("ThirdPartyNotice")
        || pattern.contains("iagnosticMessages.json")
        || pattern == "/src/lib/"
        || pattern.starts_with("/tsc/internal/bundled/libs/")
}

/// Diagnostic catalogs in merge order; later files override earlier ones for the same code.
pub const CATALOG_PATHS: &[&str] = &[
    "src/compiler/diagnosticMessages.json",
    "tsc/internal/diagnostics/diagnosticMessages.json",
    "tsc/internal/diagnostics/extraDiagnosticMessages.json",
];

/// Turns a sparse pattern into a git pathspec.
fn pathspec(pattern: &str) -> String {
    let p = pattern.trim_start_matches('/');
    if p.contains('*') {
        format!(":(glob){p}")
    } else {
        p.to_string()
    }
}

/// Which candidate patterns exist at `sha` (reads trees only; no blobs are downloaded).
fn existing_patterns(dir: &Path, sha: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for pattern in DATA_CANDIDATES {
        let p = pattern.trim_start_matches('/');
        let probe = match p.split_once('*') {
            Some((dir_part, _)) => dir_part.trim_end_matches('/'),
            None => p.trim_end_matches('/'),
        };
        let listed = run_capture(git(dir).args(["ls-tree", sha, "--", probe]))?;
        if !listed.trim().is_empty() {
            out.push((*pattern).to_string());
        }
    }
    Ok(out)
}

#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
    /// `(from, to)` with identical content.
    pub moved: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawChange {
    pub status: char,
    pub old_oid: String,
    pub new_oid: String,
    pub path: String,
}

/// Parses `git diff --raw --no-renames --no-abbrev` output.
pub fn parse_raw(text: &str) -> Vec<RawChange> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.trim_start_matches(':').split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        out.push(RawChange {
            status: fields[4].chars().next().unwrap_or('M'),
            old_oid: fields[2].to_string(),
            new_oid: fields[3].to_string(),
            path: path.to_string(),
        });
    }
    out
}

/// Layout-independent identity of a data file, so moves between layouts can be paired.
pub fn layout_key(path: &str) -> String {
    for prefix in ["tsc/testdata/tests/cases/", "tests/cases/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return format!("cases/{rest}");
        }
    }
    for prefix in ["tsc/testdata/tests/lib/", "tests/lib/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return format!("testlib/{rest}");
        }
    }
    for prefix in ["tsc/internal/bundled/libs/", "src/lib/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return format!("lib/{}", rest.strip_prefix("lib.").unwrap_or(rest));
        }
    }
    if let Some(rest) = path.strip_prefix("tsc/testdata/baselines/reference/") {
        return format!(
            "baseline/{}",
            rest.strip_prefix("submodule/").unwrap_or(rest)
        );
    }
    path.to_string()
}

/// Groups raw changes, pairing deletions and additions with the same [`layout_key`] as moves
/// (identical content) or modifications (listed under the new path).
pub fn classify_changes(raw: &[RawChange]) -> Changes {
    let mut c = Changes::default();
    let mut added: BTreeMap<String, &RawChange> = BTreeMap::new();
    for r in raw.iter().filter(|r| r.status == 'A') {
        added.insert(layout_key(&r.path), r);
    }
    let mut paired = std::collections::BTreeSet::new();
    for r in raw {
        match r.status {
            'A' => {}
            'D' => match added.get(&layout_key(&r.path)) {
                Some(a) => {
                    paired.insert(a.path.clone());
                    if a.new_oid == r.old_oid {
                        c.moved.push((r.path.clone(), a.path.clone()));
                    } else {
                        c.modified.push(a.path.clone());
                    }
                }
                None => c.removed.push(r.path.clone()),
            },
            _ => c.modified.push(r.path.clone()),
        }
    }
    for r in raw.iter().filter(|r| r.status == 'A') {
        if !paired.contains(&r.path) {
            c.added.push(r.path.clone());
        }
    }
    c.modified.sort();
    c
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagEntry {
    pub code: u32,
    pub category: String,
    pub message: String,
    /// Editors render the span as unused/faded (`reportsUnnecessary`).
    pub unnecessary: bool,
    /// Editors render the span as deprecated/struck through (`reportsDeprecated`).
    pub deprecated: bool,
}

/// Parses a `diagnosticMessages.json` catalog (message -> {category, code}) into code order.
pub fn parse_catalog(text: &str) -> Result<BTreeMap<u32, DiagEntry>> {
    let map: serde_json::Map<String, Value> = serde_json::from_str(text)?;
    let mut out = BTreeMap::new();
    for (message, info) in map {
        let Some(code) = info["code"].as_u64() else {
            continue;
        };
        let code = u32::try_from(code).map_err(|_| format!("code {code} out of range"))?;
        let category = info["category"].as_str().unwrap_or("Error").to_string();
        let flag = |k: &str| info[k].as_bool().unwrap_or(false);
        out.insert(
            code,
            DiagEntry {
                code,
                category,
                message,
                unnecessary: flag("reportsUnnecessary"),
                deprecated: flag("reportsDeprecated"),
            },
        );
    }
    Ok(out)
}

/// Merges every catalog in [`CATALOG_PATHS`] that `read` can produce (later files win).
pub fn merged_catalog(
    mut read: impl FnMut(&str) -> Option<String>,
) -> Result<(BTreeMap<u32, DiagEntry>, Vec<&'static str>)> {
    let mut merged = BTreeMap::new();
    let mut used = Vec::new();
    for path in CATALOG_PATHS {
        if let Some(text) = read(path) {
            merged.extend(parse_catalog(&text).map_err(|e| format!("{path}: {e}"))?);
            used.push(*path);
        }
    }
    Ok((merged, used))
}

#[derive(Debug, Default, PartialEq)]
pub struct CatalogDiff {
    pub added: Vec<DiagEntry>,
    pub removed: Vec<DiagEntry>,
    pub changed: Vec<(DiagEntry, DiagEntry)>,
}

pub fn diff_catalogs(
    old: &BTreeMap<u32, DiagEntry>,
    new: &BTreeMap<u32, DiagEntry>,
) -> CatalogDiff {
    let mut d = CatalogDiff::default();
    for (code, n) in new {
        match old.get(code) {
            None => d.added.push(n.clone()),
            Some(o) if o != n => d.changed.push((o.clone(), n.clone())),
            Some(_) => {}
        }
    }
    for (code, o) in old {
        if !new.contains_key(code) {
            d.removed.push(o.clone());
        }
    }
    d
}

const TEST_PREFIXES: &[&str] = &["tests/cases/", "tsc/testdata/tests/cases/"];
const BASELINE_PREFIXES: &[&str] = &["tests/baselines/reference/", "tsc/testdata/baselines/"];
const LIB_PREFIXES: &[&str] = &[
    "src/lib/",
    "tests/lib/",
    "tsc/testdata/tests/lib/",
    "tsc/internal/bundled/libs/",
];

fn resolve_ref(r: &str, pin: &Pin) -> String {
    if r == "pinned" {
        return pin.commit.clone();
    }
    if let Some(v) = Version::parse(r)
        && !r.starts_with('v')
    {
        return format!("v{v}");
    }
    r.to_string()
}

fn diff(mut args: Args) -> Result<ExitCode> {
    let out = args.opt("out")?;
    let limit: usize = args
        .opt("limit")?
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--limit must be a number")?
        .unwrap_or(200);
    let (Some(old), Some(new)) = (args.positional(), args.positional()) else {
        return Err(
            "usage: cargo xtask upstream diff <old> <new> [--out file.md] [--limit N]\n  \
             refs: a version (7.0.2), a tag (v7.0.2), a branch, a commit SHA, or `pinned`"
                .into(),
        );
    };
    args.finish()?;
    let file = load_pins()?;
    let ts = ts_pin(&file);
    let dir = root().join(TS_DIR);
    ensure_repo(&dir, &ts.repo)?;
    let old_sha = fetch_ref(&dir, &resolve_ref(&old, ts))?;
    let new_sha = fetch_ref(&dir, &resolve_ref(&new, ts))?;

    let mut cmd = git(&dir);
    cmd.args([
        "diff",
        "--raw",
        "--no-renames",
        "--no-abbrev",
        &old_sha,
        &new_sha,
        "--",
    ])
    .args(DATA_CANDIDATES.iter().map(|p| pathspec(p)));
    let raw = parse_raw(&run_capture(&mut cmd)?);
    let pick = |prefixes: &[&str]| {
        let subset: Vec<RawChange> = raw
            .iter()
            .filter(|r| prefixes.iter().any(|p| r.path.starts_with(p)))
            .cloned()
            .collect();
        classify_changes(&subset)
    };
    let tests = pick(TEST_PREFIXES);
    let baselines = pick(BASELINE_PREFIXES);
    let libs = pick(LIB_PREFIXES);

    let show = |sha: &str, path: &str| {
        run_capture(git(&dir).args(["show", &format!("{sha}:{path}")])).ok()
    };
    let (old_cat, _) = merged_catalog(|p| show(&old_sha, p))?;
    let (new_cat, _) = merged_catalog(|p| show(&new_sha, p))?;
    let catalog = diff_catalogs(&old_cat, &new_cat);

    let old_patterns = existing_patterns(&dir, &old_sha)?;
    let new_patterns = existing_patterns(&dir, &new_sha)?;

    let shas = (old_sha.as_str(), new_sha.as_str());
    let mut md = render_diff(
        &ts.repo,
        (&old, &new),
        shas,
        &tests,
        &baselines,
        &libs,
        &catalog,
        limit,
    );
    if old_patterns != new_patterns {
        let _ = writeln!(
            md,
            "## Layout change\n\nData paths moved between these commits. `cargo xtask upstream bump` \
             rewrites `sync`/`syncMinimal` in UPSTREAM.json to:\n"
        );
        for p in &new_patterns {
            let _ = writeln!(md, "- `{p}`");
        }
        md.push('\n');
    }
    write_output(out.as_deref(), &md)?;
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn render_diff(
    repo: &str,
    (old, new): (&str, &str),
    (old_sha, new_sha): (&str, &str),
    tests: &Changes,
    baselines: &Changes,
    libs: &Changes,
    catalog: &CatalogDiff,
    limit: usize,
) -> String {
    let short = |s: &str| s.chars().take(10).collect::<String>();
    let mut md = String::new();
    let _ = writeln!(md, "# TypeScript upstream diff: `{old}` -> `{new}`\n");
    let _ = writeln!(
        md,
        "Commits `{}` -> `{}` · [compare on GitHub](https://github.com/{repo}/compare/{old_sha}...{new_sha})\n",
        short(old_sha),
        short(new_sha)
    );
    let _ = writeln!(
        md,
        "The merged PRs' descriptions and tests are the spec. Read them, and the compiler source to understand them; never copy or translate it.\n"
    );
    let _ = writeln!(md, "## Summary\n");
    let _ = writeln!(md, "| Area | Added | Removed | Changed | Moved |");
    let _ = writeln!(md, "|---|---:|---:|---:|---:|");
    for (name, c) in [
        ("Tests", tests),
        ("Baselines", baselines),
        ("Lib files", libs),
    ] {
        let _ = writeln!(
            md,
            "| {name} | {} | {} | {} | {} |",
            c.added.len(),
            c.removed.len(),
            c.modified.len(),
            c.moved.len()
        );
    }
    let _ = writeln!(
        md,
        "| Diagnostic codes | {} | {} | {} | |\n",
        catalog.added.len(),
        catalog.removed.len(),
        catalog.changed.len()
    );

    let list = |md: &mut String, title: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        let _ = writeln!(md, "### {title} ({})\n", items.len());
        for p in items.iter().take(limit) {
            let _ = writeln!(md, "- `{p}`");
        }
        if items.len() > limit {
            let _ = writeln!(md, "- ... and {} more", items.len() - limit);
        }
        md.push('\n');
    };
    for (heading, c) in [
        ("Tests", tests),
        ("Baselines", baselines),
        ("Lib files", libs),
    ] {
        let _ = writeln!(md, "## {heading}\n");
        let lower = heading.to_lowercase();
        list(&mut md, &format!("New {lower}"), &c.added);
        list(&mut md, &format!("Removed {lower}"), &c.removed);
        list(&mut md, &format!("Changed {lower}"), &c.modified);
        if !c.moved.is_empty() {
            let _ = writeln!(
                md,
                "{} {lower} moved without content changes (e.g. `{}` -> `{}`).\n",
                c.moved.len(),
                c.moved[0].0,
                c.moved[0].1
            );
        }
    }

    let _ = writeln!(md, "## Diagnostics\n");
    let entries = |md: &mut String, title: &str, items: &[DiagEntry]| {
        if items.is_empty() {
            return;
        }
        let _ = writeln!(md, "### {title} ({})\n", items.len());
        for d in items {
            let _ = writeln!(md, "- TS{} ({}): {}", d.code, d.category, d.message);
        }
        md.push('\n');
    };
    entries(&mut md, "New diagnostics", &catalog.added);
    entries(&mut md, "Removed diagnostics", &catalog.removed);
    if !catalog.changed.is_empty() {
        let _ = writeln!(md, "### Changed diagnostics ({})\n", catalog.changed.len());
        for (o, n) in &catalog.changed {
            let _ = writeln!(
                md,
                "- TS{}: {} ({}) -> {} ({})",
                n.code, o.message, o.category, n.message, n.category
            );
        }
        md.push('\n');
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_changes_and_moves() {
        let z = "0000000000000000000000000000000000000000";
        let text = format!(
            ":000000 100644 {z} aaa A\ttsc/testdata/tests/cases/compiler/a.ts\n\
             :100644 000000 aaa {z} D\ttests/cases/compiler/a.ts\n\
             :100644 000000 bbb {z} D\tsrc/lib/es5.d.ts\n\
             :000000 100644 {z} ccc A\ttsc/internal/bundled/libs/lib.es5.d.ts\n\
             :100644 000000 ddd {z} D\ttests/cases/compiler/gone.ts\n\
             :000000 100644 {z} eee A\ttsc/testdata/tests/cases/compiler/new.ts\n\
             :100644 100644 fff 111 M\ttests/cases/compiler/edit.ts\n"
        );
        let raw = parse_raw(&text);
        assert_eq!(raw.len(), 7);
        assert_eq!(raw[0].status, 'A');
        let c = classify_changes(&raw);
        assert_eq!(
            c.moved,
            vec![(
                "tests/cases/compiler/a.ts".to_string(),
                "tsc/testdata/tests/cases/compiler/a.ts".to_string()
            )]
        );
        assert_eq!(
            c.modified,
            vec![
                "tests/cases/compiler/edit.ts",
                "tsc/internal/bundled/libs/lib.es5.d.ts"
            ]
        );
        assert_eq!(c.removed, vec!["tests/cases/compiler/gone.ts"]);
        assert_eq!(c.added, vec!["tsc/testdata/tests/cases/compiler/new.ts"]);
        assert_eq!(
            layout_key("tsc/testdata/baselines/reference/submodule/compiler/x.errors.txt"),
            "baseline/compiler/x.errors.txt"
        );
    }

    #[test]
    fn candidates_are_data() {
        assert!(DATA_CANDIDATES.iter().all(|p| is_data_path(p)));
        assert_eq!(
            pathspec("/tsc/internal/bundled/libs/*.d.ts"),
            ":(glob)tsc/internal/bundled/libs/*.d.ts"
        );
        assert!(is_minimal("/src/compiler/diagnosticMessages.json"));
        assert!(!is_minimal("/tests/cases/"));
    }

    #[test]
    fn catalogs() {
        let old = parse_catalog(
            r#"{"Unterminated string literal.": {"category": "Error", "code": 1002},
                "Old one.": {"category": "Error", "code": 1003},
                "Gone.": {"category": "Message", "code": 6000}}"#,
        )
        .unwrap();
        let new = parse_catalog(
            r#"{"Unterminated string literal.": {"category": "Error", "code": 1002},
                "New wording.": {"category": "Error", "code": 1003},
                "Brand new.": {"category": "Error", "code": 2999}}"#,
        )
        .unwrap();
        let d = diff_catalogs(&old, &new);
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].code, 2999);
        assert_eq!(d.removed[0].code, 6000);
        assert_eq!(d.changed[0].1.message, "New wording.");
    }

    #[test]
    fn data_paths_only() {
        assert!(is_data_path("/tests/cases/"));
        assert!(is_data_path("/src/lib/"));
        assert!(is_data_path("/src/compiler/diagnosticMessages.json"));
        assert!(is_data_path(
            "/tsc/internal/diagnostics/extraDiagnosticMessages.json"
        ));
        assert!(!is_data_path("/src/compiler/"));
        assert!(!is_data_path("/src/compiler/checker.ts"));
        assert!(!is_data_path("/tsc/internal/checker/"));
        assert!(is_data_path("/tsc/internal/bundled/libs/*.d.ts"));
        assert!(!is_data_path("/tsc/internal/bundled/libs/"));
    }

    #[test]
    fn version_classification() {
        let pinned = Version::parse("7.0.2").unwrap();
        let vs = [
            "6.0.3",
            "7.0.1-rc",
            "7.0.2",
            "7.0.3",
            "7.1.0-rc",
            "7.2.0-dev.20261014.1",
        ];
        let (s, c) = classify_versions(vs.into_iter(), &pinned);
        assert_eq!(s.unwrap().to_string(), "7.0.3");
        assert_eq!(c.unwrap().to_string(), "7.1.0-rc");
    }

    #[test]
    fn refs() {
        let pin = Pin {
            repo: "microsoft/TypeScript".into(),
            npm: None,
            version: None,
            tag: None,
            branch: None,
            commit: "abc".into(),
            date: None,
            npm_git_head: None,
            uses: None,
            sync: vec![],
            sync_minimal: vec![],
        };
        assert_eq!(resolve_ref("7.0.2", &pin), "v7.0.2");
        assert_eq!(resolve_ref("v7.0.2", &pin), "v7.0.2");
        assert_eq!(resolve_ref("pinned", &pin), "abc");
        assert_eq!(resolve_ref("1e4744d6", &pin), "1e4744d6");
    }

    #[test]
    fn pins_file_parses() {
        let text = std::fs::read_to_string(root().join(UPSTREAM_JSON)).unwrap();
        let file: UpstreamFile = serde_json::from_str(&text).unwrap();
        let ts = ts_pin(&file);
        assert!(ts.sync.iter().all(|p| is_data_path(p)));
        assert!(ts.sync_minimal.iter().all(|p| ts.sync.contains(p)));
        assert_eq!(ts.commit.len(), 40);
    }
}
