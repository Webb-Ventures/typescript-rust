//! User settings (`ts-r.*`) and the daemon's timing knobs.
//!
//! Settings arrive from the editor (LSP `initializationOptions`, `workspace/didChangeConfiguration`,
//! or the tsserver stand-in's hello) as JSON. Keys may be flat (`memoryBudgetMB`), prefixed
//! (`ts-r.memoryBudgetMB`) or nested (`{"ts-r": {"memoryBudgetMB": ..}}`). Unknown keys and
//! wrong types are ignored, never errors.
//!
//! Most settings are daemon-wide. `ts-r.lint.enable` is per client ([`ClientLint`]): one
//! window may run the ESLint extension and another not.
//!
//! Timing knobs that tests need to shrink are read from the environment
//! (`TS_R_LSP_SAMPLE_MS`, `TS_R_LSP_RESTART_IDLE_MS`, `TS_R_LSP_LINGER_MS`,
//! `TS_R_LSP_IDLE_SHUTDOWN_MS`, `TS_R_LSP_LINT_DEBOUNCE_MS`).

use serde_json::{Map, Value, json};
use std::time::Duration;

/// Which engine answers type questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeBackend {
    /// The project's own TypeScript (TS 7 native server, or tsserver for TS 5/6).
    TypeScript,
    /// Our checker (L3, the default), with the per-project parity check that falls back to
    /// the project's TypeScript for diagnostics.
    TsR,
}

/// `ts-r.lint.enable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LintEnable {
    /// Lint files governed by an ESLint, oxlint or Biome config. Editor extensions resolve
    /// `auto` themselves (VS Code: off while the ESLint extension is enabled) and send `on`
    /// or `off`; a plain LSP client that says nothing gets `auto`, which the server treats
    /// as `on`.
    Auto,
    /// Lint.
    On,
    /// Keep using ESLint: no lint diagnostics or code actions.
    Off,
}

impl LintEnable {
    /// Parses `"auto" | "on" | "off"`, booleans, and the L1 values of `ts-r.lint`
    /// (`"syntax"`, `"full"` mean on).
    pub fn parse(v: &Value) -> Option<LintEnable> {
        match v {
            Value::Bool(true) => Some(LintEnable::On),
            Value::Bool(false) => Some(LintEnable::Off),
            Value::String(s) => match s.as_str() {
                "auto" => Some(LintEnable::Auto),
                "on" | "syntax" | "full" => Some(LintEnable::On),
                "off" => Some(LintEnable::Off),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether lint runs.
    pub fn is_on(self) -> bool {
        self != LintEnable::Off
    }

    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            LintEnable::Auto => "auto",
            LintEnable::On => "on",
            LintEnable::Off => "off",
        }
    }
}

/// Per-client lint settings found in one settings object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientLint {
    /// `ts-r.lint.enable` (or the L1 string form of `ts-r.lint`).
    pub enable: Option<LintEnable>,
    /// `lintOnly`: the client only wants lint (the VS Code extension's lint connection; type
    /// answers come through its tsserver stand-in). No type backend is started for it.
    pub lint_only: Option<bool>,
}

impl ClientLint {
    /// Reads the per-client lint keys from flat, prefixed or nested settings.
    pub fn from_json(v: &Value) -> ClientLint {
        let mut out = ClientLint::default();
        out.read(v);
        out
    }

    fn read(&mut self, v: &Value) {
        let Some(obj) = v.as_object() else { return };
        if let Some(nested) = obj.get("ts-r") {
            self.read(nested);
        }
        for (k, val) in obj {
            match k.strip_prefix("ts-r.").unwrap_or(k) {
                "lint" => match val {
                    Value::Object(o) => {
                        if let Some(e) = o.get("enable").and_then(LintEnable::parse) {
                            self.enable = Some(e);
                        }
                    }
                    v => {
                        if let Some(e) = LintEnable::parse(v) {
                            self.enable = Some(e);
                        }
                    }
                },
                "lint.enable" => {
                    if let Some(e) = LintEnable::parse(val) {
                        self.enable = Some(e);
                    }
                }
                "lintOnly" => {
                    if let Some(b) = val.as_bool() {
                        self.lint_only = Some(b);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Effective settings of a daemon.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// `ts-r.typeBackend`.
    pub type_backend: TypeBackend,
    /// `ts-r.memoryBudgetMB` (default 1500). Backend RSS above this triggers an idle restart.
    pub memory_budget_mb: u64,
    /// `ts-r.idleShutdownMinutes` (default 10). No client activity for this long stops the
    /// backends; 0 disables idle shutdown.
    pub idle_shutdown: Duration,
    /// `ts-r.lint.debounceMs` (default 10): quiet time after an edit before the file is
    /// linted again. Folds the burst of didChanges one editor action sends (multi-cursor,
    /// format, undo) and adds little to typing latency: a lint takes about a millisecond, so a
    /// longer wait would dominate it. Open and save lint at once.
    pub lint_debounce: Duration,
    /// How long clients must be quiet before an over-budget backend is restarted.
    pub restart_idle: Duration,
    /// How often backend RSS is sampled.
    pub sample_interval: Duration,
    /// How long the daemon stays up after the last client disconnects.
    pub linger: Duration,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            type_backend: TypeBackend::TsR,
            memory_budget_mb: 1500,
            idle_shutdown: Duration::from_secs(10 * 60),
            lint_debounce: Duration::from_millis(10),
            restart_idle: Duration::from_secs(30),
            sample_interval: Duration::from_secs(10),
            linger: Duration::from_secs(10),
        }
    }
}

fn env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

impl Settings {
    /// Defaults with environment overrides for the timing knobs.
    pub fn from_env() -> Settings {
        let mut s = Settings::default();
        if let Some(d) = env_ms("TS_R_LSP_SAMPLE_MS") {
            s.sample_interval = d;
        }
        if let Some(d) = env_ms("TS_R_LSP_RESTART_IDLE_MS") {
            s.restart_idle = d;
        }
        if let Some(d) = env_ms("TS_R_LSP_LINGER_MS") {
            s.linger = d;
        }
        if let Some(d) = env_ms("TS_R_LSP_IDLE_SHUTDOWN_MS") {
            s.idle_shutdown = d;
        }
        if let Some(d) = env_ms("TS_R_LSP_LINT_DEBOUNCE_MS") {
            s.lint_debounce = d;
        }
        // Tests that drive fake TypeScript backends pin the default.
        match std::env::var("TS_R_LSP_TYPE_BACKEND").as_deref() {
            Ok("typescript") => s.type_backend = TypeBackend::TypeScript,
            Ok("ts-r") => s.type_backend = TypeBackend::TsR,
            _ => {}
        }
        s
    }

    /// Applies whatever `ts-r.*` keys `v` contains. Returns warnings for values it ignored.
    /// Per-client keys (`lint.enable`, `lintOnly`) are read by [`ClientLint`] and skipped here.
    pub fn apply_json(&mut self, v: &Value) -> Vec<String> {
        let mut warnings = Vec::new();
        let Some(obj) = v.as_object() else {
            return warnings;
        };
        if let Some(nested) = obj.get("ts-r") {
            warnings.extend(self.apply_json(nested));
        }
        for (k, val) in obj {
            let key = k.strip_prefix("ts-r.").unwrap_or(k);
            match key {
                "typeBackend" => match val.as_str() {
                    Some("typescript") => self.type_backend = TypeBackend::TypeScript,
                    Some("ts-r") => self.type_backend = TypeBackend::TsR,
                    _ => warnings.push(format!("ignored ts-r.typeBackend = {val}")),
                },
                "memoryBudgetMB" => match val.as_u64() {
                    Some(mb) if mb >= 64 => self.memory_budget_mb = mb,
                    _ => warnings.push(format!("ignored ts-r.memoryBudgetMB = {val} (min 64)")),
                },
                "idleShutdownMinutes" => match val.as_f64() {
                    // Negative, NaN and too large for a `Duration` (1e308) are rejected.
                    Some(m) if Duration::try_from_secs_f64(m * 60.0).is_ok() => {
                        self.idle_shutdown = Duration::from_secs_f64(m * 60.0)
                    }
                    _ => warnings.push(format!("ignored ts-r.idleShutdownMinutes = {val}")),
                },
                "lint" => match val {
                    Value::Object(o) => self.apply_lint(o, &mut warnings),
                    v if LintEnable::parse(v).is_some() => {}
                    _ => warnings.push(format!("ignored ts-r.lint = {val}")),
                },
                "lint.enable" => {
                    if LintEnable::parse(val).is_none() {
                        warnings.push(format!("ignored ts-r.lint.enable = {val}"));
                    }
                }
                "lint.debounceMs" => {
                    let mut m = Map::new();
                    m.insert("debounceMs".into(), val.clone());
                    self.apply_lint(&m, &mut warnings);
                }
                _ => {}
            }
        }
        warnings
    }

    fn apply_lint(&mut self, o: &Map<String, Value>, warnings: &mut Vec<String>) {
        if let Some(v) = o.get("debounceMs") {
            match v.as_u64() {
                Some(ms) if ms <= 5000 => self.lint_debounce = Duration::from_millis(ms),
                _ => warnings.push(format!("ignored ts-r.lint.debounceMs = {v} (0..5000)")),
            }
        }
        if let Some(v) = o.get("enable")
            && LintEnable::parse(v).is_none()
        {
            warnings.push(format!("ignored ts-r.lint.enable = {v}"));
        }
    }

    /// Notes the user should see about settings not implemented yet.
    pub fn l1_notes(&self) -> Vec<&'static str> {
        let mut notes = Vec::new();
        if self.type_backend == TypeBackend::TsR && crate::tsr::libs().is_none() {
            notes.push(
                "ts-r.typeBackend \"ts-r\": the TypeScript lib files were not found next to ts-r-lsp (set TS_R_LIB_DIR); using your project's TypeScript",
            );
        }
        notes
    }

    /// JSON view (for `status`).
    pub fn to_json(&self) -> Value {
        json!({
            "typeBackend": match self.type_backend { TypeBackend::TypeScript => "typescript", TypeBackend::TsR => "ts-r" },
            "memoryBudgetMB": self.memory_budget_mb,
            "idleShutdownSeconds": self.idle_shutdown.as_secs_f64(),
            "lintDebounceMs": self.lint_debounce.as_millis() as u64,
            "restartIdleSeconds": self.restart_idle.as_secs_f64(),
            "sampleIntervalSeconds": self.sample_interval.as_secs_f64(),
            "lingerSeconds": self.linger.as_secs_f64(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_plan() {
        let s = Settings::default();
        assert_eq!(s.memory_budget_mb, 1500);
        assert_eq!(s.idle_shutdown, Duration::from_secs(600));
        assert_eq!(s.type_backend, TypeBackend::TsR);
        assert_eq!(s.lint_debounce, Duration::from_millis(10));
    }

    #[test]
    fn idle_shutdown_too_large_for_a_duration_is_ignored() {
        let mut s = Settings::default();
        for m in [1e308, 1e18, f64::MAX] {
            let w = s.apply_json(&json!({ "idleShutdownMinutes": m }));
            assert_eq!(w.len(), 1, "{m}");
        }
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn flat_prefixed_and_nested_keys() {
        let mut s = Settings::default();
        assert!(s.apply_json(&json!({"memoryBudgetMB": 800})).is_empty());
        assert_eq!(s.memory_budget_mb, 800);
        s.apply_json(&json!({"ts-r.idleShutdownMinutes": 0.5}));
        assert_eq!(s.idle_shutdown, Duration::from_secs(30));
        let w = s.apply_json(
            &json!({"ts-r": {"typeBackend": "ts-r", "lint": {"enable": "on", "debounceMs": 10}}}),
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s.type_backend, TypeBackend::TsR);
        assert_eq!(s.lint_debounce, Duration::from_millis(10));
        s.apply_json(&json!({"ts-r.lint.debounceMs": 0}));
        assert_eq!(s.lint_debounce, Duration::ZERO);
        assert_eq!(
            s.l1_notes().len(),
            usize::from(crate::tsr::libs().is_none())
        );
    }

    #[test]
    fn bad_values_are_ignored_with_warnings() {
        let mut s = Settings::default();
        let w = s.apply_json(
            &json!({"memoryBudgetMB": "lots", "lint": 3, "idleShutdownMinutes": -1, "other": 1}),
        );
        assert_eq!(w.len(), 3);
        assert_eq!(s, Settings::default());
        assert!(s.apply_json(&json!(null)).is_empty());
        let w = s.apply_json(&json!({"lint": {"enable": "sometimes", "debounceMs": -4}}));
        assert_eq!(w.len(), 2);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn client_lint_keys() {
        let c = ClientLint::from_json(&json!({"ts-r": {"lint": {"enable": "off"}}}));
        assert_eq!(c.enable, Some(LintEnable::Off));
        let c = ClientLint::from_json(&json!({"ts-r.lint.enable": "on", "lintOnly": true}));
        assert_eq!(c.enable, Some(LintEnable::On));
        assert_eq!(c.lint_only, Some(true));
        // L1's string form of ts-r.lint still works.
        assert_eq!(
            ClientLint::from_json(&json!({"lint": "full"})).enable,
            Some(LintEnable::On)
        );
        assert_eq!(
            ClientLint::from_json(&json!({"lint": "auto"})).enable,
            Some(LintEnable::Auto)
        );
        assert_eq!(
            ClientLint::from_json(&json!({"lint": 1})),
            ClientLint::default()
        );
        assert!(LintEnable::Auto.is_on() && !LintEnable::Off.is_on());
    }
}
