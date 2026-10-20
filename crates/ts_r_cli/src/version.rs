//! Which ts-r version's tools to run: the launcher's own, or a pinned one (rustup/Volta style).
//!
//! Precedence: `TS_R_VERSION` env var, then the nearest `.ts-r-version` file walking up from
//! the current directory, then the launcher's own version.

use std::fmt;
use std::path::{Path, PathBuf};

/// The launcher's own version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of the pin file.
pub const PIN_FILE: &str = ".ts-r-version";

/// Where the selected version came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// No pin: the launcher's own version.
    Launcher,
    /// The `TS_R_VERSION` environment variable.
    Env,
    /// A `.ts-r-version` file.
    File(PathBuf),
}

/// The version whose tools will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    /// Version string, e.g. `0.3.1`.
    pub version: String,
    /// Where it came from.
    pub origin: Origin,
}

impl Selected {
    /// Whether this is the version the launcher itself was built as (so binaries shipped next
    /// to the launcher and on PATH may be used).
    pub fn is_own(&self) -> bool {
        self.version == VERSION
    }
}

impl fmt::Display for Selected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            Origin::Launcher => write!(f, "{} (the launcher's own)", self.version),
            Origin::Env => write!(f, "{} (from TS_R_VERSION)", self.version),
            Origin::File(p) => write!(f, "{} (pinned by {})", self.version, p.display()),
        }
    }
}

/// Picks the version. `env_pin` is the value of `TS_R_VERSION`, if set.
pub fn select(cwd: Option<&Path>, env_pin: Option<&str>) -> Result<Selected, String> {
    if let Some(v) = env_pin.filter(|v| !v.trim().is_empty()) {
        let version = parse(v).map_err(|e| format!("TS_R_VERSION: {e}"))?;
        return Ok(Selected {
            version,
            origin: Origin::Env,
        });
    }
    if let Some(file) = cwd.and_then(find_pin_file) {
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("can't read {}: {e}", file.display()))?;
        let version = parse(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(Selected {
            version,
            origin: Origin::File(file),
        });
    }
    Ok(Selected {
        version: VERSION.to_owned(),
        origin: Origin::Launcher,
    })
}

/// The nearest `.ts-r-version` in `start` or its ancestors.
pub fn find_pin_file(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join(PIN_FILE))
        .find(|p| p.is_file())
}

/// Parses pin file contents: the first line that is not blank or a `#` comment. A leading
/// `v` is accepted (`v1.2.3` = `1.2.3`).
pub fn parse(text: &str) -> Result<String, String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| "no version found".to_owned())?;
    let v = line.strip_prefix('v').unwrap_or(line);
    let valid = !v.is_empty()
        && v.len() <= 64
        && v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    if valid {
        Ok(v.to_owned())
    } else {
        Err(format!("invalid version {line:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse("1.2.3\n").unwrap(), "1.2.3");
        assert_eq!(parse("  v0.4.0-beta.1+abc  ").unwrap(), "0.4.0-beta.1+abc");
        assert_eq!(parse("# pinned for CI\n\n2.0.0\nignored").unwrap(), "2.0.0");
        assert!(parse("").is_err());
        assert!(parse("# only a comment").is_err());
        assert!(parse("latest").is_err());
        assert!(parse("../../etc").is_err());
        assert!(parse("1.0 2.0").is_err());
    }

    #[test]
    fn env_wins_then_file_then_launcher() {
        let dir = std::env::temp_dir().join(format!("ts-r-version-test-{}", std::process::id()));
        let nested = dir.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            select(Some(&nested), None).unwrap().origin,
            Origin::Launcher
        );
        std::fs::write(dir.join(PIN_FILE), "9.9.9\n").unwrap();
        let s = select(Some(&nested), None).unwrap();
        assert_eq!(s.version, "9.9.9");
        assert_eq!(s.origin, Origin::File(dir.join(PIN_FILE)));
        assert!(!s.is_own());
        let s = select(Some(&nested), Some("1.0.0")).unwrap();
        assert_eq!((s.version.as_str(), s.origin), ("1.0.0", Origin::Env));
        assert!(select(Some(&nested), Some("nope")).is_err());
        std::fs::write(dir.join(PIN_FILE), "garbage!\n").unwrap();
        assert!(select(Some(&nested), None).unwrap_err().contains(PIN_FILE));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
