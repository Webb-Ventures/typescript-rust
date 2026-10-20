//! Resident memory of process trees.
//!
//! Unix: one `ps -A -o pid=,ppid=,rss=` snapshot, then the RSS of a pid and all its
//! descendants is summed (tsserver may have children; the typings installer is disabled).
//! Windows: `tasklist` for the single process. Sampling runs on the daemon's sampler thread,
//! never on the event loop.

use std::collections::BTreeMap;
use std::process::Command;

/// One `ps` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    /// Parent pid.
    pub ppid: u32,
    /// Resident set size in KiB.
    pub rss_kib: u64,
}

/// A snapshot of every process's parent and RSS.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    procs: BTreeMap<u32, ProcInfo>,
}

impl Snapshot {
    /// Takes a snapshot. Empty if `ps` is unavailable.
    pub fn take() -> Snapshot {
        #[cfg(unix)]
        {
            let out = Command::new("ps")
                .args(["-A", "-o", "pid=,ppid=,rss="])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    Snapshot::parse_ps(&String::from_utf8_lossy(&o.stdout))
                }
                _ => Snapshot::default(),
            }
        }
        #[cfg(windows)]
        {
            Snapshot::default()
        }
    }

    /// Parses `pid ppid rss` lines.
    pub fn parse_ps(text: &str) -> Snapshot {
        let mut procs = BTreeMap::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(pid), Some(ppid), Some(rss)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            if let (Ok(pid), Ok(ppid), Ok(rss)) = (pid.parse(), ppid.parse(), rss.parse()) {
                procs.insert(pid, ProcInfo { ppid, rss_kib: rss });
            }
        }
        Snapshot { procs }
    }

    /// Whether a pid is in the snapshot.
    pub fn contains(&self, pid: u32) -> bool {
        self.procs.contains_key(&pid)
    }

    /// `pid` and all its descendants.
    pub fn tree(&self, pid: u32) -> Vec<u32> {
        let mut out = vec![];
        if !self.procs.contains_key(&pid) {
            return out;
        }
        let mut stack = vec![pid];
        while let Some(p) = stack.pop() {
            out.push(p);
            for (&child, info) in &self.procs {
                if info.ppid == p && child != p {
                    stack.push(child);
                }
            }
        }
        out
    }

    /// Summed RSS in bytes of `pid` and its descendants. `None` if `pid` is unknown.
    pub fn tree_rss_bytes(&self, pid: u32) -> Option<u64> {
        if !self.procs.contains_key(&pid) {
            return None;
        }
        Some(
            self.tree(pid)
                .iter()
                .map(|p| self.procs[p].rss_kib * 1024)
                .sum(),
        )
    }

    /// RSS in bytes of one process.
    pub fn rss_bytes(&self, pid: u32) -> Option<u64> {
        self.procs.get(&pid).map(|p| p.rss_kib * 1024)
    }
}

/// RSS of `pid` and its descendants, in bytes.
pub fn tree_rss_bytes(pid: u32) -> Option<u64> {
    #[cfg(unix)]
    {
        Snapshot::take().tree_rss_bytes(pid)
    }
    #[cfg(windows)]
    {
        windows_rss_bytes(pid)
    }
}

#[cfg(windows)]
fn windows_rss_bytes(pid: u32) -> Option<u64> {
    // "Image","PID","Session","Session#","Mem Usage" -> "12,345 K"
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains(&format!("\"{pid}\"")))?;
    let mem = line.rsplit("\",\"").next()?;
    let digits: String = mem.chars().filter(char::is_ascii_digit).collect();
    digits.parse::<u64>().ok().map(|k| k * 1024)
}

/// Bytes -> MiB, rounded.
pub fn mib(bytes: u64) -> u64 {
    (bytes + 512 * 1024) / (1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_descendants() {
        let s =
            Snapshot::parse_ps("  1 0 100\n 10 1 1000\n 11 10 200\n 12 11 50\n 13 1 7\nbad line\n");
        assert_eq!(s.tree_rss_bytes(10), Some(1250 * 1024));
        assert_eq!(s.tree_rss_bytes(13), Some(7 * 1024));
        assert_eq!(s.tree_rss_bytes(99), None);
        let mut t = s.tree(10);
        t.sort();
        assert_eq!(t, vec![10, 11, 12]);
    }

    #[cfg(unix)]
    #[test]
    fn own_process_has_rss() {
        let rss = tree_rss_bytes(std::process::id()).expect("ps works");
        assert!(rss > 1024 * 1024);
        assert_eq!(mib(3 * 1024 * 1024 + 1), 3);
    }
}
