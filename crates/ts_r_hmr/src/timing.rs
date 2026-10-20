//! Save → browser-updated latency, broken down like the plan's hot-reload budget:
//!
//! | step | budget |
//! |---|---|
//! | file watcher event (mtime → native event) | 1–5 ms |
//! | parse + transform the changed file | < 2 ms |
//! | re-prepare modules whose `parse<T>` validators read a changed file | < 1 ms |
//! | find modules to update (graph propagation) | < 1 ms |
//! | push over WebSocket + apply in browser | 5–15 ms |
//!
//! We also report the debounce wait and the file read separately, because they are ours to
//! tune. The browser step is measured by the client (message received → callbacks done) and
//! sent back as a `ts-r:applied` event; `total` is mtime → that acknowledgement arriving.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// One hot update's timings. `None` = not measured.
#[derive(Clone, Debug, Default)]
pub struct UpdateTiming {
    pub id: u64,
    /// URLs of the changed modules.
    pub files: Vec<String>,
    /// File mtime → native event received.
    pub watch: Option<Duration>,
    /// Native event → batch flushed by the debouncer.
    pub debounce: Duration,
    pub read: Duration,
    pub transform: Duration,
    pub validators: Duration,
    pub graph: Duration,
    /// Message built and written to every socket.
    pub push: Duration,
    /// Reported by the browser: message received → update applied.
    pub browser: Option<Duration>,
    /// mtime (or first event) → browser acknowledgement received.
    pub total: Option<Duration>,
    /// What the update was (`js-update`, `css-update`, `full-reload`, `error`).
    pub outcome: String,
    /// When the change happened (mtime, falling back to the first event), for `total`.
    pub(crate) origin: Option<SystemTime>,
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

impl UpdateTiming {
    /// One log line, e.g.
    /// `hmr /src/a.js js-update · watch 1.2 · debounce 3.1 · read 0.0 · transform 0.1 · validators 0.0 · graph 0.0 · push 0.1 · browser 4.0 · total 8.9 ms`
    pub fn log_line(&self) -> String {
        let mut s = format!("hmr {} {}", self.files.join(", "), self.outcome);
        let opt = |d: Option<Duration>| d.map_or_else(|| "-".to_owned(), ms);
        let _ = write!(
            s,
            " · watch {} · debounce {} · read {} · transform {} · validators {} · graph {} · push {} · browser {} · total {} ms",
            opt(self.watch),
            ms(self.debounce),
            ms(self.read),
            ms(self.transform),
            ms(self.validators),
            ms(self.graph),
            ms(self.push),
            opt(self.browser),
            opt(self.total),
        );
        s
    }

    pub fn to_json(&self) -> serde_json::Value {
        let f = |d: Duration| d.as_secs_f64() * 1000.0;
        serde_json::json!({
            "id": self.id,
            "files": self.files,
            "outcome": self.outcome,
            "watch": self.watch.map(f),
            "debounce": f(self.debounce),
            "read": f(self.read),
            "transform": f(self.transform),
            "validators": f(self.validators),
            "graph": f(self.graph),
            "push": f(self.push),
            "browser": self.browser.map(f),
            "total": self.total.map(f),
        })
    }
}

/// Recent updates and their acknowledgements.
#[derive(Debug, Default)]
pub struct Timings {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Sent, waiting for a browser acknowledgement.
    waiting: HashMap<u64, UpdateTiming>,
    /// Finished (acknowledged, or not expecting one), newest last.
    done: VecDeque<UpdateTiming>,
}

const KEEP: usize = 2000;

impl Timings {
    /// Records a sent update that expects a browser acknowledgement.
    pub fn sent(&self, t: UpdateTiming) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Forget acknowledgements that never came (no browser connected, or it reloaded).
        if g.waiting.len() > 64 {
            let oldest = g.waiting.keys().min().copied();
            if let Some(k) = oldest {
                g.waiting.remove(&k);
            }
        }
        g.waiting.insert(t.id, t);
    }

    /// Records an update that won't be acknowledged (full reload, error, nothing to do).
    pub fn finished(&self, t: UpdateTiming) -> UpdateTiming {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        push_done(&mut g.done, t.clone());
        t
    }

    /// The browser applied update `id` in `browser_ms`. Returns the completed timing the
    /// first time an update is acknowledged (several tabs may acknowledge).
    pub fn acknowledged(&self, id: u64, browser_ms: Option<f64>) -> Option<UpdateTiming> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut t = g.waiting.remove(&id)?;
        t.browser = browser_ms
            .filter(|m| m.is_finite() && *m >= 0.0)
            .map(|m| Duration::from_secs_f64(m / 1000.0));
        t.total = t
            .origin
            .and_then(|o| SystemTime::now().duration_since(o).ok());
        push_done(&mut g.done, t.clone());
        Some(t)
    }

    /// Acknowledged js/css updates only (what the latency budget is about).
    pub fn completed(&self) -> Vec<UpdateTiming> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.done
            .iter()
            .filter(|t| t.total.is_some())
            .cloned()
            .collect()
    }

    /// JSON for `/@ts-r/stats`: count, p50/p95 per step and the last 50 updates.
    pub fn stats_json(&self) -> serde_json::Value {
        let done = self.completed();
        let pick = |f: &dyn Fn(&UpdateTiming) -> Option<Duration>| -> serde_json::Value {
            let mut v: Vec<f64> = done
                .iter()
                .filter_map(f)
                .map(|d| d.as_secs_f64() * 1000.0)
                .collect();
            v.sort_by(f64::total_cmp);
            serde_json::json!({ "p50": percentile(&v, 50.0), "p95": percentile(&v, 95.0) })
        };
        let recent: Vec<_> = done
            .iter()
            .rev()
            .take(50)
            .map(UpdateTiming::to_json)
            .collect();
        serde_json::json!({
            "count": done.len(),
            "watch": pick(&|t| t.watch),
            "debounce": pick(&|t| Some(t.debounce)),
            "read": pick(&|t| Some(t.read)),
            "transform": pick(&|t| Some(t.transform)),
            "validators": pick(&|t| Some(t.validators)),
            "graph": pick(&|t| Some(t.graph)),
            "push": pick(&|t| Some(t.push)),
            "browser": pick(&|t| t.browser),
            "total": pick(&|t| t.total),
            "recent": recent,
        })
    }
}

fn push_done(done: &mut VecDeque<UpdateTiming>, t: UpdateTiming) {
    if done.len() == KEEP {
        done.pop_front();
    }
    done.push_back(t);
}

/// Nearest-rank percentile of sorted values (`None` if empty).
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(50.0));
        assert_eq!(percentile(&v, 95.0), Some(95.0));
        assert_eq!(percentile(&[3.0], 95.0), Some(3.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn acknowledgement_completes_once() {
        let t = Timings::default();
        t.sent(UpdateTiming {
            id: 7,
            files: vec!["/src/a.js".into()],
            outcome: "js-update".into(),
            origin: Some(SystemTime::now()),
            ..Default::default()
        });
        let done = t.acknowledged(7, Some(4.5)).unwrap();
        assert_eq!(done.browser, Some(Duration::from_secs_f64(0.0045)));
        assert!(done.total.is_some());
        assert!(t.acknowledged(7, Some(1.0)).is_none());
        assert_eq!(t.completed().len(), 1);
        let line = done.log_line();
        assert!(
            line.contains("/src/a.js js-update") && line.contains("browser 4.5"),
            "{line}"
        );
        assert_eq!(t.stats_json()["count"], 1);
    }
}
