//! `ecosystem/packages.toml`: the pinned package list, and `cargo xtask ecosystem refresh`,
//! which rebuilds it from npm download counts.
//!
//! Candidates come from the `npm-high-impact` package's `top-download` list (read as data from
//! its tarball, never executed). They are re-ranked by last month's downloads from
//! `api.npmjs.org`, and each winner is pinned to its current `latest` version with the
//! registry's tarball URL and `dist.integrity`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;

use serde::Deserialize;

use crate::args::Args;
use crate::util::{Result, http_json, read_to_string, root, run_capture, today};

pub const PACKAGES: &str = "ecosystem/packages.toml";

#[derive(Debug, Deserialize)]
pub struct PackageFile {
    pub meta: Meta,
    #[serde(rename = "package", default)]
    pub packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
pub struct Meta {
    /// Date the list was generated; npm installs resolve dependencies as of the next day
    /// (`npm install --before`), so runs stay reproducible.
    pub generated: String,
    pub source: String,
    /// `@types/node` version added to every install for the check stage.
    pub types_node: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    pub rank: u32,
    pub name: String,
    pub version: String,
    pub downloads: u64,
    pub integrity: String,
    pub tarball: String,
    /// Git repository URL from the package manifest (for the tests stage).
    #[serde(default)]
    pub repo: String,
}

impl Package {
    /// `@scope/name@1.2.3` → `@scope+name@1.2.3`: safe as a single path component.
    pub fn slug(&self) -> String {
        format!("{}@{}", self.name.replace('/', "+"), self.version)
    }
}

pub fn load() -> Result<PackageFile> {
    let path = root().join(PACKAGES);
    let text = read_to_string(&path)?;
    let file: PackageFile = toml::from_str(&text).map_err(|e| format!("{PACKAGES}: {e}"))?;
    validate(&file)?;
    Ok(file)
}

fn validate(file: &PackageFile) -> Result<()> {
    for (i, p) in file.packages.iter().enumerate() {
        if p.rank as usize != i + 1 {
            return Err(format!(
                "{PACKAGES}: {} has rank {}, expected {}",
                p.name,
                p.rank,
                i + 1
            )
            .into());
        }
        if crate::util::Version::parse(&p.version).is_none() {
            return Err(format!(
                "{PACKAGES}: {} has a non-pinned version {:?}",
                p.name, p.version
            )
            .into());
        }
        if !(p.integrity.starts_with("sha512-") || p.integrity.starts_with("sha1-")) {
            return Err(format!("{PACKAGES}: {} has no sha512/sha1 integrity", p.name).into());
        }
    }
    Ok(())
}

/// `cargo xtask ecosystem refresh [--count N] [--candidates N]`.
pub fn refresh(mut args: Args) -> Result<std::process::ExitCode> {
    let count: usize = args.opt("count")?.map_or(Ok(1000), |s| s.parse())?;
    let candidates: usize = args
        .opt("candidates")?
        .map_or(Ok(count + count / 2), |s| s.parse())?;
    args.finish()?;

    let (nhi_version, names) = high_impact_names()?;
    let names: Vec<String> = names.into_iter().take(candidates).collect();
    eprintln!(
        "refresh: {} candidates from npm-high-impact@{nhi_version}",
        names.len()
    );

    let downloads = download_counts(&names)?;
    let mut ranked = rank(names, &downloads);
    let missing = ranked.iter().filter(|(_, _, known)| !known).count();
    if missing > 0 {
        eprintln!(
            "refresh: {missing} candidates without a fresh count keep their npm-high-impact position"
        );
    }
    let ranked: Vec<(String, u64)> = ranked.drain(..).map(|(n, d, _)| (n, d)).collect();

    let mut out: Vec<Package> = Vec::new();
    let mut i = 0;
    while out.len() < count && i < ranked.len() {
        let batch: Vec<(String, u64)> = ranked[i..(i + 16).min(ranked.len())].to_vec();
        i += batch.len();
        let results = parallel(&batch, 4, |(name, dl)| {
            manifest(name, *dl).map_err(|e| e.to_string())
        });
        for r in results {
            match r {
                Ok(p) if out.len() < count => out.push(p),
                Ok(_) => {}
                Err(e) => eprintln!("refresh: skipped: {e}"),
            }
        }
        eprint!("\rrefresh: {}/{count} pinned", out.len());
    }
    eprintln!();
    for (i, p) in out.iter_mut().enumerate() {
        p.rank = (i + 1) as u32;
    }
    let types_node = http_json("https://registry.npmjs.org/@types%2Fnode/latest", None)?["version"]
        .as_str()
        .ok_or("@types/node: no version")?
        .to_string();
    let source = format!(
        "npm-high-impact@{nhi_version} top-download (first {candidates}), re-ranked by api.npmjs.org last-month downloads"
    );
    let text = render(&today(), &source, &types_node, &out);
    let path = root().join(PACKAGES);
    std::fs::write(&path, text)?;
    eprintln!("wrote {PACKAGES} ({} packages)", out.len());
    Ok(std::process::ExitCode::SUCCESS)
}

/// The `topDownload` names from the latest `npm-high-impact` tarball.
fn high_impact_names() -> Result<(String, Vec<String>)> {
    let meta = http_json("https://registry.npmjs.org/npm-high-impact/latest", None)?;
    let version = meta["version"]
        .as_str()
        .ok_or("npm-high-impact: no version")?
        .to_string();
    let url = meta["dist"]["tarball"]
        .as_str()
        .ok_or("npm-high-impact: no tarball")?;
    let tmp = std::env::temp_dir().join(format!("ts-r-eco-nhi-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tgz = tmp.join("nhi.tgz");
    let result = (|| -> Result<String> {
        run_capture(
            Command::new("curl")
                .args(["-fsSL", "--max-time", "120", "-o"])
                .arg(&tgz)
                .arg(url),
        )?;
        run_capture(
            Command::new("tar")
                .arg("-xzOf")
                .arg(&tgz)
                .arg("package/lib/top-download.js"),
        )
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    Ok((version, quoted_strings(&result?)))
}

/// Every single- or double-quoted string literal in `src`, in order (enough for a JS array
/// of package names; no escapes appear in npm names).
pub fn quoted_strings(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = src.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\'' || c == '"' {
            let start = i + 1;
            for (j, d) in chars.by_ref() {
                if d == c {
                    out.push(src[start..j].to_string());
                    break;
                }
            }
        }
    }
    out
}

/// Sorts candidates by downloads, most first. A candidate whose count could not be fetched
/// inherits the count of the nearest candidate before it in the (already download-ordered)
/// input, so it keeps roughly its original position. Returns (name, downloads, known).
pub fn rank(names: Vec<String>, downloads: &BTreeMap<String, u64>) -> Vec<(String, u64, bool)> {
    let mut last = u64::MAX;
    let mut out: Vec<(String, u64, bool)> = names
        .into_iter()
        .map(|n| match downloads.get(&n) {
            Some(&d) => {
                last = d;
                (n, d, true)
            }
            None => (n, last, false),
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// GET JSON, backing off on rate limits (HTTP 429): 2, 4, 8, 16, 32 s.
fn http_json_patient(url: &str) -> Result<serde_json::Value> {
    let mut wait = 2;
    loop {
        match http_json(url, None) {
            Err(e) if e.to_string().contains("429") && wait <= 32 => {
                std::thread::sleep(std::time::Duration::from_secs(wait));
                wait *= 2;
            }
            r => return r,
        }
    }
}

/// Last-month downloads. Unscoped names go 128 at a time through the bulk endpoint; scoped
/// names are not supported there and are fetched one by one.
fn download_counts(names: &[String]) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    let (scoped, plain): (Vec<&String>, Vec<&String>) =
        names.iter().partition(|n| n.starts_with('@'));
    for chunk in plain.chunks(128) {
        let list: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
        let url = format!(
            "https://api.npmjs.org/downloads/point/last-month/{}",
            list.join(",")
        );
        let v = http_json_patient(&url)?;
        for name in chunk {
            if let Some(d) = v[name.as_str()]["downloads"].as_u64() {
                out.insert((*name).clone(), d);
            }
        }
    }
    let scoped: Vec<String> = scoped.into_iter().cloned().collect();
    let results = parallel(&scoped, 2, |name| {
        let url = format!("https://api.npmjs.org/downloads/point/last-month/{name}");
        http_json_patient(&url)
            .map(|v| (name.clone(), v["downloads"].as_u64().unwrap_or(0)))
            .map_err(|e| e.to_string())
    });
    for r in results {
        match r {
            Ok((n, d)) => {
                out.insert(n, d);
            }
            Err(e) => eprintln!("refresh: downloads: {e}"),
        }
    }
    Ok(out)
}

fn manifest(name: &str, downloads: u64) -> Result<Package> {
    let url = format!(
        "https://registry.npmjs.org/{}/latest",
        name.replace('/', "%2F")
    );
    let m = http_json_patient(&url)?;
    let version = m["version"]
        .as_str()
        .ok_or_else(|| format!("{name}: no version"))?;
    let dist = &m["dist"];
    let integrity = match dist["integrity"].as_str() {
        Some(i) if i.starts_with("sha512-") => i.to_string(),
        _ => {
            let hex = dist["shasum"]
                .as_str()
                .ok_or_else(|| format!("{name}: no integrity"))?;
            format!("sha1-{}", base64(&hex_bytes(hex).ok_or("bad shasum")?))
        }
    };
    let tarball = dist["tarball"]
        .as_str()
        .ok_or_else(|| format!("{name}: no tarball"))?;
    let repo = match &m["repository"] {
        serde_json::Value::String(s) => s.clone(),
        v => v["url"].as_str().unwrap_or("").to_string(),
    };
    Ok(Package {
        rank: 0,
        name: name.to_string(),
        version: version.to_string(),
        downloads,
        integrity,
        tarball: tarball.to_string(),
        repo,
    })
}

fn render(date: &str, source: &str, types_node: &str, pkgs: &[Package]) -> String {
    let mut s = String::new();
    s.push_str(
        "# Pinned npm packages for `cargo xtask ecosystem`, most downloaded first.\n\
         # Generated by `cargo xtask ecosystem refresh`; do not edit by hand.\n\n",
    );
    let _ = writeln!(
        s,
        "[meta]\ngenerated = {}\nsource = {}\ntypes_node = {}\n",
        q(date),
        q(source),
        q(types_node)
    );
    for p in pkgs {
        let _ = writeln!(
            s,
            "[[package]]\nrank = {}\nname = {}\nversion = {}\ndownloads = {}\nintegrity = {}\ntarball = {}\nrepo = {}\n",
            p.rank,
            q(&p.name),
            q(&p.version),
            p.downloads,
            q(&p.integrity),
            q(&p.tarball),
            q(&p.repo)
        );
    }
    s
}

/// A TOML basic string.
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Runs `f` over `items` on up to `threads` threads, keeping input order.
pub fn parallel<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, 4) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= items.len() {
                        break;
                    }
                    let r = f(&items[i]);
                    if let Ok(mut slot) = slots[i].lock() {
                        *slot = Some(r);
                    }
                }
            });
        }
    });
    slots
        .into_iter()
        .filter_map(|m| m.into_inner().ok().flatten())
        .collect()
}

pub fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(T[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_hex() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(hex_bytes("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(hex_bytes("0"), None);
    }

    #[test]
    fn ranking_keeps_unknown_counts_in_place() {
        let names: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let mut d = BTreeMap::new();
        d.insert("a".to_string(), 100);
        d.insert("c".to_string(), 50);
        d.insert("d".to_string(), 200);
        let r: Vec<String> = rank(names, &d).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(r, vec!["d", "a", "b", "c"]);
    }

    #[test]
    fn quoted_names() {
        let src = "export const topDownload = [\n  'semver',\n  \"@babel/core\",\n]\n";
        assert_eq!(quoted_strings(src), vec!["semver", "@babel/core"]);
    }

    #[test]
    fn render_round_trips() {
        let p = Package {
            rank: 1,
            name: "@a/b".into(),
            version: "1.2.3".into(),
            downloads: 5,
            integrity: "sha512-abc".into(),
            tarball: "https://x/y.tgz".into(),
            repo: "git+https://github.com/a/b.git".into(),
        };
        let text = render("2026-10-15", "test", "24.0.0", &[p]);
        let file: PackageFile = toml::from_str(&text).unwrap();
        validate(&file).unwrap();
        assert_eq!(file.packages[0].slug(), "@a+b@1.2.3");
        assert_eq!(file.meta.types_node, "24.0.0");
    }

    #[test]
    fn checked_in_list_is_valid() {
        if root().join(PACKAGES).is_file() {
            let file = load().unwrap();
            assert!(!file.packages.is_empty());
        }
    }
}
