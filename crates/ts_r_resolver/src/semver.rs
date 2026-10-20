//! Just enough semver range matching for `typesVersions` keys and versioned `types@<range>`
//! export conditions: `||`, hyphen ranges, `<`, `<=`, `>`, `>=`, `=`, `~`, `^` and x-ranges
//! (`7`, `7.x`, `*`). Prerelease tags are ignored.

type Version = (u64, u64, u64);

/// A version with possibly missing (wildcard) parts.
#[derive(Debug, Clone, Copy)]
struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
}

fn parse_partial(s: &str) -> Option<Partial> {
    let s = s.trim().trim_start_matches(['v', '=']);
    let s = s.split(['-', '+']).next().unwrap_or("");
    if s.is_empty() {
        return Some(Partial {
            major: None,
            minor: None,
            patch: None,
        });
    }
    let mut parts = s.split('.');
    let mut next = || -> Option<Option<u64>> {
        match parts.next() {
            None | Some("x" | "X" | "*") => Some(None),
            Some(p) => p.parse().ok().map(Some),
        }
    };
    let major = next()?;
    let minor = if major.is_some() { next()? } else { None };
    let patch = if minor.is_some() { next()? } else { None };
    Some(Partial {
        major,
        minor,
        patch,
    })
}

fn lower(p: Partial) -> Version {
    (
        p.major.unwrap_or(0),
        p.minor.unwrap_or(0),
        p.patch.unwrap_or(0),
    )
}

/// Exclusive upper bound for an x-range (`7` → `<8.0.0`, `7.1` → `<7.2.0`).
fn x_upper(p: Partial) -> Option<Version> {
    match (p.major, p.minor, p.patch) {
        (None, _, _) => None,
        (Some(a), None, _) => Some((a + 1, 0, 0)),
        (Some(a), Some(b), None) => Some((a, b + 1, 0)),
        (Some(a), Some(b), Some(c)) => Some((a, b, c + 1)),
    }
}

fn comparator(c: &str, v: Version) -> bool {
    let c = c.trim();
    let (op, rest) = ["<=", ">=", "<", ">", "=", "~", "^"]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r)))
        .unwrap_or(("", c));
    let Some(p) = parse_partial(rest) else {
        return false;
    };
    let lo = lower(p);
    match op {
        ">=" => v >= lo,
        "<" => v < lo,
        ">" => match x_upper(p) {
            Some(u) => v >= u,
            None => false,
        },
        "<=" => match x_upper(p) {
            Some(u) => v < u,
            None => true,
        },
        "~" => {
            let upper = match (p.major, p.minor) {
                (Some(a), Some(b)) => (a, b + 1, 0),
                (Some(a), None) => (a + 1, 0, 0),
                _ => return true,
            };
            v >= lo && v < upper
        }
        "^" => {
            let upper = match (p.major, p.minor, p.patch) {
                (None, _, _) => return true,
                (Some(a), _, _) if a > 0 => (a + 1, 0, 0),
                (Some(0), Some(b), _) if b > 0 => (0, b + 1, 0),
                (Some(0), Some(0), Some(c)) => (0, 0, c + 1),
                (Some(0), Some(0), None) => (0, 1, 0),
                (Some(_), None, _) => (1, 0, 0),
                _ => return true,
            };
            v >= lo && v < upper
        }
        _ => match x_upper(p) {
            Some(u) => v >= lo && v < u,
            None => true,
        },
    }
}

/// True when `version` satisfies the semver `range`.
pub fn range_matches(range: &str, version: Version) -> bool {
    range.split("||").any(|alt| {
        let alt = alt.trim();
        if let Some((a, b)) = alt.split_once(" - ") {
            return comparator(&format!(">={}", a.trim()), version)
                && comparator(&format!("<={}", b.trim()), version);
        }
        // Allow `>= 4.1` (space after the operator).
        let mut tokens: Vec<String> = Vec::new();
        for t in alt.split_whitespace() {
            match tokens.last_mut() {
                Some(last) if ["<", "<=", ">", ">=", "=", "~", "^"].contains(&last.as_str()) => {
                    last.push_str(t)
                }
                _ => tokens.push(t.to_owned()),
            }
        }
        tokens.iter().all(|t| comparator(t, version))
    })
}

#[cfg(test)]
mod tests {
    use super::range_matches;
    const V: (u64, u64, u64) = (7, 0, 2);

    #[test]
    fn ranges() {
        for r in [
            "*",
            ">=4.2",
            ">=7.0",
            "7",
            "7.x",
            "7.0",
            "^7.0.0",
            "~7.0.1",
            ">=5 <8",
            "<4 || >=7",
            "6 - 7.1",
            ">= 3.1",
            "<=7",
        ] {
            assert!(range_matches(r, V), "{r}");
        }
        for r in [
            ">=9.0", "<7", ">7", "^6", "~7.1", "8.x", "<=6.9", ">=7.0.3", "5 - 6",
        ] {
            assert!(!range_matches(r, V), "{r}");
        }
    }
}
