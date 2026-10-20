//! The table of built-in rules and rule instantiation from config values.

use std::sync::{Arc, Mutex, OnceLock};

use rustc_hash::FxHashMap;

use crate::diagnostic::Severity;
use crate::engine::ConfiguredRule;
use crate::rule::{Rule, RuleDef};

/// Why a rule could not be configured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigureError {
    /// No built-in rule has this name.
    Unknown,
    /// Bad severity or options.
    Invalid(String),
}

/// All built-in rules, by ESLint name.
#[derive(Debug)]
pub struct Registry {
    defs: Vec<&'static RuleDef>,
    by_name: FxHashMap<&'static str, usize>,
    /// Instances by (rule, options JSON) so identical configs share one object.
    instances: Mutex<FxHashMap<(usize, String), Arc<dyn Rule>>>,
}

impl Registry {
    /// The process-wide registry of built-in rules.
    pub fn global() -> &'static Registry {
        static R: OnceLock<Registry> = OnceLock::new();
        R.get_or_init(|| Registry::new(crate::rules::ALL))
    }

    pub fn new(defs: &[&'static RuleDef]) -> Registry {
        let mut by_name = FxHashMap::default();
        for (i, d) in defs.iter().enumerate() {
            by_name.insert(d.meta.name, i);
        }
        Registry {
            defs: defs.to_vec(),
            by_name,
            instances: Mutex::new(FxHashMap::default()),
        }
    }

    pub fn all(&self) -> &[&'static RuleDef] {
        &self.defs
    }

    pub fn find(&self, name: &str) -> Option<&'static RuleDef> {
        self.by_name.get(name).map(|&i| self.defs[i])
    }

    /// Builds (or reuses) a rule instance for `options`.
    pub fn instantiate(
        &self,
        def: &'static RuleDef,
        options: &[serde_json::Value],
    ) -> Result<Arc<dyn Rule>, String> {
        let idx = self
            .by_name
            .get(def.meta.name)
            .copied()
            .unwrap_or(usize::MAX);
        let key = (idx, serde_json::to_string(options).unwrap_or_default());
        if let Ok(map) = self.instances.lock()
            && let Some(r) = map.get(&key)
        {
            return Ok(r.clone());
        }
        let r: Arc<dyn Rule> = Arc::from((def.create)(options)?);
        if let Ok(mut map) = self.instances.lock() {
            map.insert(key, r.clone());
        }
        Ok(r)
    }

    /// Configures a rule from an ESLint config value (`"error"`, `2`, `["warn", {...}]`).
    /// `Ok(None)` means the rule is off.
    pub fn configure(
        &self,
        name: &str,
        value: &serde_json::Value,
    ) -> Result<Option<ConfiguredRule>, ConfigureError> {
        let (severity, options) = parse_rule_value(value).map_err(ConfigureError::Invalid)?;
        let def = self.find(name).ok_or(ConfigureError::Unknown)?;
        if severity == Severity::Off {
            return Ok(None);
        }
        let rule = self
            .instantiate(def, &options)
            .map_err(ConfigureError::Invalid)?;
        Ok(Some(ConfiguredRule {
            def,
            severity,
            rule,
        }))
    }
}

/// Splits a rule config value into severity and options.
pub fn parse_rule_value(
    value: &serde_json::Value,
) -> Result<(Severity, Vec<serde_json::Value>), String> {
    match value {
        serde_json::Value::Array(items) => {
            let first = items
                .first()
                .ok_or_else(|| "empty rule configuration".to_owned())?;
            let sev = Severity::from_json(first)
                .ok_or_else(|| format!("Expected severity of \"off\", 0, \"warn\", 1, \"error\", or 2. You passed \"{first}\"."))?;
            Ok((sev, items[1..].to_vec()))
        }
        v => {
            let sev = Severity::from_json(v)
                .ok_or_else(|| format!("Expected severity of \"off\", 0, \"warn\", 1, \"error\", or 2. You passed \"{v}\"."))?;
            Ok((sev, Vec::new()))
        }
    }
}
