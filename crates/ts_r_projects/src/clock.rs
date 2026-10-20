//! Local clock times for build and watch messages (`05:05:25 PM`, as tsc 7 prints them),
//! without platform calls: the UTC offset comes from the TZif file of `$TZ` (an IANA name or
//! a path) or `/etc/localtime`; anything unreadable means UTC. Clock times only appear in
//! terminal output, never in anything stored (record 0012, "Determinism").

use std::time::{SystemTime, UNIX_EPOCH};

/// `HH:MM:SS AM` for `t` in local time (12-hour clock, zero-padded hour).
pub fn time_of_day(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let local = secs + local_offset(secs);
    let day = local.rem_euclid(86_400);
    let (h, m, s) = (day / 3600, (day % 3600) / 60, day % 60);
    let (h12, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12:02}:{m:02}:{s:02} {half}")
}

/// The local UTC offset in seconds at Unix time `t`.
pub fn local_offset(t: i64) -> i64 {
    let data = match std::env::var("TZ") {
        Ok(tz) if !tz.is_empty() => {
            let name = tz.trim_start_matches(':');
            if name.eq_ignore_ascii_case("UTC") || name.eq_ignore_ascii_case("GMT") {
                return 0;
            }
            let path = if name.starts_with('/') {
                name.to_owned()
            } else {
                format!("/usr/share/zoneinfo/{name}")
            };
            std::fs::read(path).ok()
        }
        _ => std::fs::read("/etc/localtime").ok(),
    };
    data.and_then(|d| tzif_offset(&d, t)).unwrap_or(0)
}

fn be32(b: &[u8], at: usize) -> Option<i64> {
    let s = b.get(at..at + 4)?;
    Some(i32::from_be_bytes([s[0], s[1], s[2], s[3]]) as i64)
}

fn be64(b: &[u8], at: usize) -> Option<i64> {
    let s = b.get(at..at + 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(i64::from_be_bytes(a))
}

/// The offset a TZif file (RFC 8536) gives for `t`: the type of the last transition at or
/// before `t` (the first standard type before every transition). Uses the 64-bit block of
/// version 2+ files.
fn tzif_offset(b: &[u8], t: i64) -> Option<i64> {
    if b.get(0..4)? != b"TZif" {
        return None;
    }
    let counts = |at: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (i, slot) in c.iter_mut().enumerate() {
            *slot = usize::try_from(be32(b, at + 20 + i * 4)?).ok()?;
        }
        Some(c)
    };
    // [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt]
    let c1 = counts(0)?;
    let v1_len = c1[3] * 5 + c1[4] * 6 + c1[5] + c1[2] * 8 + c1[1] + c1[0];
    let (base, c, time_size) = if b[4] >= b'2' {
        let at = 44 + v1_len;
        (at, counts(at)?, 8)
    } else {
        (0, c1, 4)
    };
    let (timecnt, typecnt) = (c[3], c[4]);
    let times = base + 44;
    let idxs = times + timecnt * time_size;
    let types = idxs + timecnt;
    let offset_of = |ty: usize| -> Option<i64> {
        if ty >= typecnt {
            return None;
        }
        be32(b, types + ty * 6)
    };
    let mut current: Option<usize> = None;
    for i in 0..timecnt {
        let at = times + i * time_size;
        let when = if time_size == 8 {
            be64(b, at)?
        } else {
            be32(b, at)?
        };
        if when > t {
            break;
        }
        current = Some(*b.get(idxs + i)? as usize);
    }
    match current {
        Some(ty) => offset_of(ty),
        None => {
            // Before the first transition: the first non-DST type, else type 0.
            let first_std = (0..typecnt).find(|&ty| b.get(types + ty * 6 + 4) == Some(&0));
            offset_of(first_std.unwrap_or(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_hour_clock() {
        let at = |s: u64| UNIX_EPOCH + std::time::Duration::from_secs(s);
        // Format only (the offset is whatever the machine has): HH:MM:SS AM|PM.
        let t = time_of_day(at(1_700_000_000));
        assert_eq!(t.len(), 11, "{t}");
        assert!(t.ends_with(" AM") || t.ends_with(" PM"), "{t}");
    }

    #[test]
    fn garbage_is_utc() {
        assert_eq!(tzif_offset(b"nope", 0), None);
        assert_eq!(tzif_offset(b"TZif2", 0), None);
    }
}
