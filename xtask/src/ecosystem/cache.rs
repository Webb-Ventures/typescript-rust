//! The tarball cache in `~/.cache/ts-r/ecosystem` (or `$TS_R_CACHE_DIR/ecosystem`):
//! `tarballs/<slug>.tgz` (verified against `dist.integrity`), `src/<slug>/` (unpacked
//! package), `npm/` (npm's own cache for dependency installs). Capped at
//! [`CACHE_CAP_BYTES`]; the least recently used entries go first.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use super::packages::{Package, base64, hex_bytes};
use crate::util::{Result, run_capture};

pub const CACHE_CAP_BYTES: u64 = 5 * 1024 * 1024 * 1024;

pub fn dir() -> PathBuf {
    if let Ok(d) = std::env::var("TS_R_CACHE_DIR")
        && !d.is_empty()
    {
        return PathBuf::from(d).join("ecosystem");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ts-r/ecosystem")
}

pub fn tarball_path(p: &Package) -> PathBuf {
    dir().join("tarballs").join(format!("{}.tgz", p.slug()))
}

pub fn src_path(p: &Package) -> PathBuf {
    dir().join("src").join(p.slug())
}

pub fn npm_cache() -> PathBuf {
    dir().join("npm")
}

/// What [`ensure`] had to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Cached,
    Downloaded,
}

/// Makes sure the package is downloaded, verified and unpacked. Returns the package root
/// (the directory holding its `package.json`). `allow_download` false: fail instead of
/// downloading (the per-run download cap is used up).
pub fn ensure(p: &Package, allow_download: bool) -> Result<(PathBuf, Fetched)> {
    let tgz = tarball_path(p);
    let src = src_path(p);
    let mut fetched = Fetched::Cached;
    if !tgz.is_file() {
        if !allow_download {
            return Err("download cap reached".into());
        }
        std::fs::create_dir_all(tgz.parent().unwrap_or(Path::new(".")))?;
        let part = tgz.with_extension("tgz.part");
        run_capture(
            Command::new("curl")
                .args([
                    "-fsSL",
                    "--retry",
                    "2",
                    "--max-time",
                    "300",
                    "--max-filesize",
                    "268435456",
                    "-o",
                ])
                .arg(&part)
                .arg(&p.tarball),
        )?;
        if let Err(e) = verify(&part, &p.integrity) {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, &tgz)?;
        let _ = std::fs::remove_dir_all(&src);
        fetched = Fetched::Downloaded;
    } else if !src.is_dir() {
        // Re-verify before unpacking a tarball that may have been tampered with.
        verify(&tgz, &p.integrity)?;
    }
    if !src.is_dir() {
        unpack(&tgz, &src)?;
    }
    touch(&src);
    Ok((package_root(&src)?, fetched))
}

/// Checks `file` against an npm `sha512-<base64>` / `sha1-<base64>` integrity string.
pub fn verify(file: &Path, integrity: &str) -> Result<()> {
    let (algo, want) = integrity.split_once('-').ok_or("malformed integrity")?;
    let flag = match algo {
        "sha512" => "-sha512",
        "sha1" => "-sha1",
        other => return Err(format!("unsupported integrity algorithm {other}").into()),
    };
    let out = run_capture(Command::new("openssl").args(["dgst", flag, "-r"]).arg(file))?;
    let hex = out.split_whitespace().next().unwrap_or("");
    let got = base64(&hex_bytes(hex).ok_or("openssl printed no digest")?);
    if got != want {
        return Err(format!(
            "integrity mismatch for {}: expected {integrity}, got {algo}-{got}",
            file.display()
        )
        .into());
    }
    Ok(())
}

fn unpack(tgz: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("unpacking");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    // bsdtar and GNU tar both refuse absolute paths and `..` members by default.
    let r = run_capture(
        Command::new("tar")
            .arg("-xzf")
            .arg(tgz)
            .arg("-C")
            .arg(&tmp)
            .arg("--no-same-owner"),
    );
    if let Err(e) = r {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    // Some tarballs ship directories without the execute bit.
    let _ = Command::new("chmod")
        .args(["-R", "u+rwX"])
        .arg(&tmp)
        .status();
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// The single top-level directory of an unpacked tarball (`package/` for most packages).
fn package_root(src: &Path) -> Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(src)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    if let Some(d) = dirs.iter().find(|d| d.join("package.json").is_file()) {
        return Ok(d.clone());
    }
    Err(format!("{}: no package.json in the tarball", src.display()).into())
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

pub fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| size_of(&e.path()))
        .sum()
}

/// Deletes least recently used entries until the cache is under `cap` bytes. Returns the
/// final size.
pub fn enforce_cap(cap: u64) -> u64 {
    let root = dir();
    let mut total = size_of(&root);
    if total <= cap {
        return total;
    }
    // Candidates: unpacked sources, tarballs, then npm's cache as a whole.
    let mut entries: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
    for sub in ["src", "tarballs", "repos"] {
        if let Ok(rd) = std::fs::read_dir(root.join(sub)) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let size = size_of(&p);
                entries.push((t, p, size));
            }
        }
    }
    let npm = npm_cache();
    let npm_size = size_of(&npm);
    if npm_size > 0 {
        entries.push((SystemTime::UNIX_EPOCH, npm, npm_size));
    }
    entries.sort();
    for (_, p, size) in entries {
        if total <= cap {
            break;
        }
        let _ = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        total = total.saturating_sub(size);
    }
    total
}

pub fn clean() -> Result<()> {
    let d = dir();
    if d.exists() {
        std::fs::remove_dir_all(&d)?;
    }
    eprintln!("removed {}", d.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_checks_digest() {
        let tmp = std::env::temp_dir().join(format!("ts-r-eco-verify-{}", std::process::id()));
        std::fs::write(&tmp, b"hello").unwrap();
        // sha1("hello") = aaf4c61d dcc5e8a2 dabede0f 3b482cd9 aea9434d
        let good = format!(
            "sha1-{}",
            base64(&hex_bytes("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d").unwrap())
        );
        if crate::util::has_program("openssl")
            || Command::new("openssl").arg("version").output().is_ok()
        {
            verify(&tmp, &good).unwrap();
            assert!(verify(&tmp, "sha1-AAAA").is_err());
        }
        assert!(verify(&tmp, "md5-xyz").is_err());
        let _ = std::fs::remove_file(&tmp);
    }
}
