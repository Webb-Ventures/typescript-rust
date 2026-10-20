//! Sound-mode configuration (design record 0007, "Configuration"). It lives in one of three
//! places: the `[sound]` table of `ts-r.toml`, a `tsrconfig.json` (JSONC,
//! `{ "sound": { ... } }`) or the top-level `"ts-r": { "sound": { ... } }` key of
//! `tsconfig.json`. All three are read into one intermediate tree, so they accept exactly the
//! same keys; a project that sets it in more than one place gets TSR0020 naming every file.
//! The record's open questions are decided (2026-10-15); the optional switches the decisions
//! keep are keys of [`Policy`].

use ts_r_config::FileSystem;
use ts_r_config::glob::Pattern;
use ts_r_config::path;

/// A file's level. Ordered: each level includes the ones before it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Level {
    /// Plain TypeScript.
    #[default]
    Off,
    /// Strict flags plus S-01, S-04 and S-10.
    Strict,
    /// Every rule.
    Sound,
    /// `sound` plus runtime checks in dev builds (checking is the same as `sound`).
    Contracts,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "off" => Level::Off,
            "strict" => Level::Strict,
            "sound" => Level::Sound,
            "contracts" => Level::Contracts,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Strict => "strict",
            Level::Sound => "sound",
            Level::Contracts => "contracts",
        }
    }

    /// Whether rule `S-rr` applies at this level.
    pub fn has_rule(self, rule: u8) -> bool {
        match self {
            Level::Off => false,
            Level::Strict => matches!(rule, 0 | 1 | 4 | 10),
            Level::Sound | Level::Contracts => true,
        }
    }
}

/// How a dependency's values enter sound code (S-11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PackageMode {
    Trusted,
    Loose,
    Verify,
    Sound,
}

impl PackageMode {
    fn parse(s: &str) -> Option<PackageMode> {
        Some(match s {
            "trusted" => PackageMode::Trusted,
            "loose" => PackageMode::Loose,
            "verify" => PackageMode::Verify,
            "sound" => PackageMode::Sound,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PackageMode::Trusted => "trusted",
            PackageMode::Loose => "loose",
            PackageMode::Verify => "verify",
            PackageMode::Sound => "sound",
        }
    }
}

/// `[sound.guard]`. Extra properties are always kept and `parse` returns its input (open
/// question 11); classes are checked with `instanceof` (open question 13). Neither has a key.
#[derive(Clone, Debug)]
pub struct GuardConfig {
    /// `"full" | "first" | "none"`.
    pub explain: String,
    pub max_depth: u32,
    pub max_validator_kb: u32,
    /// `refinements = { Email = "./validators#isEmail" }` (open question 14): predicates for
    /// branded and refined types; relative specifiers are resolved against the configuration
    /// file's directory.
    pub refinements: Vec<ts_r_guard::Refinement>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        GuardConfig {
            explain: "full".into(),
            max_depth: 512,
            max_validator_kb: 8,
            refinements: Vec::new(),
        }
    }
}

impl GuardConfig {
    /// The `parse<T>` expansion options these settings select.
    pub fn options(&self) -> ts_r_guard::GuardOptions {
        ts_r_guard::GuardOptions {
            max_depth: self.max_depth,
            explain: match self.explain.as_str() {
                "first" => ts_r_guard::Explain::First,
                "none" => ts_r_guard::Explain::None,
                _ => ts_r_guard::Explain::Full,
            },
            max_validator_kb: self.max_validator_kb,
            refinements: self.refinements.clone(),
        }
    }
}

/// What [`guard_options`] found.
#[derive(Clone, Debug, Default)]
pub struct GuardSettings {
    pub options: ts_r_guard::GuardOptions,
    /// The file the settings came from (`ts-r.toml`, `tsrconfig.json` or the tsconfig), if
    /// any.
    pub source: Option<String>,
    /// Configuration problems (TSR0020).
    pub errors: Vec<ConfigError>,
}

/// `[sound.guard]` for the tools that expand `parse<T>` (`ts-r-run`, `ts-r-build`,
/// `ts-r-dev`): the configuration [`discover`] finds for the tsconfig at `tsconfig_path` (it
/// need not exist; a `ts-r.toml` above its directory still counts), else the defaults.
pub fn guard_options(fs: &dyn FileSystem, tsconfig_path: &str) -> GuardSettings {
    let (cfg, errors) = discover(fs, tsconfig_path);
    match cfg {
        Some(c) => GuardSettings {
            options: c.guard.options(),
            source: Some(c.source),
            errors,
        },
        None => GuardSettings {
            errors,
            ..GuardSettings::default()
        },
    }
}

impl ConfigError {
    /// `file(1,1): error TSR0020: Invalid sound-mode configuration: <message>.`
    pub fn render(&self) -> String {
        format!(
            "{}(1,1): error TSR0020: Invalid sound-mode configuration: {}.",
            self.file, self.message
        )
    }
}

/// The switches the record's decided open questions keep, each defaulting to the decision
/// (record 0007, "Decisions", 2026-10-15). Keys live in `[sound]`.
#[derive(Clone, Debug)]
pub struct Policy {
    /// OQ1 `loose-values = "any" | "unknown"`: values from `loose` packages stay `any`.
    pub loose_values_unknown: bool,
    /// OQ2 `code-prefix = "TSR" | "TS"`: print `TSR0201` (default) or the numeric `TS100201`.
    pub numeric_codes: bool,
    /// OQ3 `exports-declare-throws = false`: only functions that already have `@throws` tags
    /// are held to them (TSR1301); `true` also makes exported functions that throw declare
    /// it (TSR1302).
    pub exports_declare_throws: bool,
    /// OQ5 `invariant-properties = false`: mutable object properties stay covariant.
    pub invariant_properties: bool,
    /// OQ7 `strict-lib-methods = false`: methods declared in lib/`.d.ts` stay bivariant.
    pub strict_lib_methods: bool,
    /// OQ8 `constructor-escape = "all" | "before-init"`: TSR0803 is on.
    pub this_escape_after_init: bool,
    /// OQ12 `async-void-callbacks = "error" | "allow-jsx-handlers"`: TSR1003 everywhere.
    pub async_void_jsx_handlers_allowed: bool,
    /// OQ17 `guard-false-side = true`: S-07 proves the false side too.
    pub guard_false_side: bool,
    /// OQ18 `forced-flag-codes = "TSR" | "TS"`: forced-flag errors are re-coded as TSR.
    pub forced_flags_keep_tsc_codes: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            loose_values_unknown: false,
            numeric_codes: false,
            exports_declare_throws: false,
            invariant_properties: false,
            strict_lib_methods: false,
            this_escape_after_init: true,
            async_void_jsx_handlers_allowed: false,
            guard_false_side: true,
            forced_flags_keep_tsc_codes: false,
        }
    }
}

/// The resolved `[sound]` configuration of a project.
#[derive(Clone, Debug, Default)]
pub struct SoundConfig {
    /// The file it came from (`ts-r.toml` or `tsconfig.json`); empty when built in code.
    pub source: String,
    /// Directory `include`/`exclude` are relative to.
    pub base_dir: String,
    /// Level of included files.
    pub level: Level,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// S-13 (OQ3): extra error classes treated as unchecked, by name.
    pub unchecked_errors: Vec<String>,
    pub packages_default: Option<PackageMode>,
    pub packages_untyped: Option<PackageMode>,
    pub packages: Vec<(String, PackageMode)>,
    /// `[sound.sources]`: extra S-14 sources, package name to exported paths.
    pub sources: Vec<(String, Vec<String>)>,
    pub guard: GuardConfig,
    /// OQ10 `[sound.rules] "S-09" = "off"`: rules switched off (the report marks the
    /// guarantee partial).
    pub rules_off: Vec<u8>,
    pub policy: Policy,
}

/// A file level forced from the command line (`ts-r-check --sound=<level>`), applied to every
/// file the project includes.
impl SoundConfig {
    /// A configuration with every project file at `level` (`include = ["**/*"]`).
    pub fn everything_at(level: Level, base_dir: &str) -> SoundConfig {
        SoundConfig {
            base_dir: base_dir.to_owned(),
            level,
            include: vec!["**/*".into()],
            ..SoundConfig::default()
        }
    }

    /// The configured mode of package `name` (`None`: detect).
    pub fn package_mode(&self, name: &str) -> Option<PackageMode> {
        self.packages
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, m)| *m)
    }

    /// Whether rule `S-rr` is switched on.
    pub fn rule_on(&self, rule: u8) -> bool {
        !self.rules_off.contains(&rule)
    }

    /// The level `include`/`exclude` give an absolute, normalised path (pragmas aside).
    pub fn configured_level(&self, abs_path: &str, case_sensitive: bool) -> Level {
        let pat = |spec: &str, inc: bool| Pattern::new(&path::combine(&self.base_dir, spec), inc);
        if self
            .exclude
            .iter()
            .any(|e| pat(e, false).matches_self_or_ancestor(abs_path, case_sensitive))
        {
            return Level::Off;
        }
        if self
            .include
            .iter()
            .any(|i| pat(i, true).matches(abs_path, case_sensitive))
        {
            return self.level;
        }
        Level::Off
    }
}

/// A configuration problem (TSR0020), reported on the config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub file: String,
    pub message: String,
}

/// Intermediate value tree shared by the TOML and JSON readers.
#[derive(Clone, Debug)]
enum Val {
    Str(String),
    Bool(bool),
    Int(i64),
    Arr(Vec<Val>),
    Table(Vec<(String, Val)>),
    Other,
}

impl Val {
    fn kind(&self) -> &'static str {
        match self {
            Val::Str(_) => "a string",
            Val::Bool(_) => "a boolean",
            Val::Int(_) => "a number",
            Val::Arr(_) => "an array",
            Val::Table(_) => "a table",
            Val::Other => "a value of another type",
        }
    }
}

fn from_toml(v: &toml::Value) -> Val {
    match v {
        toml::Value::String(s) => Val::Str(s.clone()),
        toml::Value::Boolean(b) => Val::Bool(*b),
        toml::Value::Integer(i) => Val::Int(*i),
        toml::Value::Array(a) => Val::Arr(a.iter().map(from_toml).collect()),
        toml::Value::Table(t) => {
            Val::Table(t.iter().map(|(k, v)| (k.clone(), from_toml(v))).collect())
        }
        _ => Val::Other,
    }
}

fn from_json(v: &ts_r_config::jsonc::Value) -> Val {
    if let Some(s) = v.as_str() {
        return Val::Str(s.to_owned());
    }
    if let Some(b) = v.as_bool() {
        return Val::Bool(b);
    }
    if let Some(a) = v.as_array() {
        return Val::Arr(a.iter().map(from_json).collect());
    }
    if let Some(o) = v.as_object() {
        return Val::Table(
            o.iter()
                .map(|m| (m.key.clone(), from_json(&m.value)))
                .collect(),
        );
    }
    let json = v.to_json();
    match json.parse::<f64>() {
        Ok(n) if n.fract() == 0.0 && n.abs() < 1e15 => Val::Int(n as i64),
        _ => Val::Other,
    }
}

struct Reader<'a> {
    file: &'a str,
    errors: Vec<ConfigError>,
}

impl Reader<'_> {
    fn err(&mut self, message: String) {
        self.errors.push(ConfigError {
            file: self.file.to_owned(),
            message,
        });
    }

    fn string(&mut self, key: &str, v: &Val) -> Option<String> {
        match v {
            Val::Str(s) => Some(s.clone()),
            other => {
                self.err(format!("'{key}' must be a string, not {}", other.kind()));
                None
            }
        }
    }

    fn strings(&mut self, key: &str, v: &Val) -> Vec<String> {
        match v {
            Val::Arr(items) => items.iter().filter_map(|i| self.string(key, i)).collect(),
            other => {
                self.err(format!(
                    "'{key}' must be an array of strings, not {}",
                    other.kind()
                ));
                Vec::new()
            }
        }
    }

    fn boolean(&mut self, key: &str, v: &Val) -> Option<bool> {
        match v {
            Val::Bool(b) => Some(*b),
            other => {
                self.err(format!("'{key}' must be a boolean, not {}", other.kind()));
                None
            }
        }
    }

    fn uint(&mut self, key: &str, v: &Val) -> Option<u32> {
        match v {
            Val::Int(i) if *i >= 0 && *i <= u32::MAX as i64 => Some(*i as u32),
            other => {
                self.err(format!(
                    "'{key}' must be a non-negative integer, not {}",
                    other.kind()
                ));
                None
            }
        }
    }

    fn choice(&mut self, key: &str, v: &Val, allowed: &[&str]) -> Option<String> {
        let s = self.string(key, v)?;
        if allowed.contains(&s.as_str()) {
            Some(s)
        } else {
            let list = allowed
                .iter()
                .map(|a| format!("'{a}'"))
                .collect::<Vec<_>>()
                .join(", ");
            self.err(format!("'{key}' must be one of {list}, not '{s}'"));
            None
        }
    }

    fn mode(&mut self, key: &str, v: &Val) -> Option<PackageMode> {
        let s = self.choice(key, v, &["trusted", "loose", "verify", "sound"])?;
        PackageMode::parse(&s)
    }

    /// `refinements = { Email = "./validators#isEmail" }`: type name to `<module>#<export>`.
    fn refinements(&mut self, cfg: &mut SoundConfig, v: &Val) {
        let Val::Table(t) = v else {
            self.err(format!(
                "'guard.refinements' must be a table of type names to \"<module>#<export>\", not {}",
                v.kind()
            ));
            return;
        };
        for (name, target) in t {
            let key = format!("guard.refinements.{name}");
            if !is_identifier(name) {
                self.err(format!("'{key}': '{name}' is not a type name"));
                continue;
            }
            let Some(s) = self.string(&key, target) else {
                continue;
            };
            let Some((module, export)) = s.rsplit_once('#') else {
                self.err(format!(
                    "'{key}' must be \"<module>#<export>\", for example \"./validators#is{name}\", not '{s}'"
                ));
                continue;
            };
            if module.is_empty() || !(is_identifier(export) || export == "default") {
                self.err(format!(
                    "'{key}' must be \"<module>#<export>\" with an exported function name, not '{s}'"
                ));
                continue;
            }
            let module = if path::is_explicitly_relative(module) {
                path::resolve(path::directory_of(self.file), module)
            } else if path::is_rooted(module) {
                path::normalize(module)
            } else {
                module.to_owned()
            };
            cfg.guard.refinements.retain(|r| r.name != *name);
            cfg.guard.refinements.push(ts_r_guard::Refinement {
                name: name.clone(),
                module,
                export: export.to_owned(),
            });
        }
    }

    fn read_sound(&mut self, cfg: &mut SoundConfig, table: &[(String, Val)]) {
        for (k, v) in table {
            match k.as_str() {
                "level" => {
                    if let Some(s) =
                        self.choice("level", v, &["off", "strict", "sound", "contracts"])
                    {
                        cfg.level = Level::parse(&s).unwrap_or_default();
                    }
                }
                "include" => cfg.include = self.strings("include", v),
                "exclude" => cfg.exclude = self.strings("exclude", v),
                "unchecked-errors" => cfg.unchecked_errors = self.strings("unchecked-errors", v),
                "loose-values" => {
                    if let Some(s) = self.choice(k, v, &["any", "unknown"]) {
                        cfg.policy.loose_values_unknown = s == "unknown";
                    }
                }
                "code-prefix" => {
                    if let Some(s) = self.choice(k, v, &["TSR", "TS"]) {
                        cfg.policy.numeric_codes = s == "TS";
                    }
                }
                "exports-declare-throws" => {
                    if let Some(b) = self.boolean(k, v) {
                        cfg.policy.exports_declare_throws = b;
                    }
                }
                "invariant-properties" => {
                    if let Some(b) = self.boolean(k, v) {
                        cfg.policy.invariant_properties = b;
                    }
                }
                "strict-lib-methods" => {
                    if let Some(b) = self.boolean(k, v) {
                        cfg.policy.strict_lib_methods = b;
                    }
                }
                "constructor-escape" => {
                    if let Some(s) = self.choice(k, v, &["all", "before-init"]) {
                        cfg.policy.this_escape_after_init = s == "all";
                    }
                }
                "async-void-callbacks" => {
                    if let Some(s) = self.choice(k, v, &["error", "allow-jsx-handlers"]) {
                        cfg.policy.async_void_jsx_handlers_allowed = s == "allow-jsx-handlers";
                    }
                }
                "guard-false-side" => {
                    if let Some(b) = self.boolean(k, v) {
                        cfg.policy.guard_false_side = b;
                    }
                }
                "forced-flag-codes" => {
                    if let Some(s) = self.choice(k, v, &["TSR", "TS"]) {
                        cfg.policy.forced_flags_keep_tsc_codes = s == "TS";
                    }
                }
                "packages" => match v {
                    Val::Table(t) => {
                        for (name, m) in t {
                            let mode = self.mode(name, m);
                            match (name.as_str(), mode) {
                                ("default", Some(m)) => cfg.packages_default = Some(m),
                                ("untyped", Some(m)) => cfg.packages_untyped = Some(m),
                                (_, Some(m)) => cfg.packages.push((name.clone(), m)),
                                _ => {}
                            }
                        }
                    }
                    other => self.err(format!("'packages' must be a table, not {}", other.kind())),
                },
                "sources" => match v {
                    Val::Table(t) => {
                        for (name, paths) in t {
                            let p = self.strings(name, paths);
                            cfg.sources.push((name.clone(), p));
                        }
                    }
                    other => self.err(format!("'sources' must be a table, not {}", other.kind())),
                },
                "rules" => match v {
                    Val::Table(t) => {
                        for (name, on) in t {
                            let rule = parse_rule_id(name);
                            let Some(rule) = rule else {
                                self.err(format!("unknown rule '{name}' in 'rules'"));
                                continue;
                            };
                            if let Some(s) = self.choice(name, on, &["on", "off"])
                                && s == "off"
                            {
                                cfg.rules_off.push(rule);
                            }
                        }
                    }
                    other => self.err(format!("'rules' must be a table, not {}", other.kind())),
                },
                "guard" => match v {
                    Val::Table(t) => {
                        for (gk, gv) in t {
                            match gk.as_str() {
                                "explain" => {
                                    if let Some(s) = self.choice(gk, gv, &["full", "first", "none"])
                                    {
                                        cfg.guard.explain = s;
                                    }
                                }
                                "max-depth" => {
                                    if let Some(n) = self.uint(gk, gv) {
                                        cfg.guard.max_depth = n;
                                    }
                                }
                                "max-validator-kb" => {
                                    if let Some(n) = self.uint(gk, gv) {
                                        cfg.guard.max_validator_kb = n;
                                    }
                                }
                                "refinements" => self.refinements(cfg, gv),
                                other => self.err(format!("unknown key 'guard.{other}'")),
                            }
                        }
                    }
                    other => self.err(format!("'guard' must be a table, not {}", other.kind())),
                },
                other => self.err(format!("unknown key '{other}'")),
            }
        }
    }
}

/// An ASCII JavaScript identifier (type names and export names in `refinements`).
fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// `S-01`..`S-14` (also `s-1`, `S1`) to the rule number.
pub fn parse_rule_id(s: &str) -> Option<u8> {
    let rest = s.strip_prefix('S').or_else(|| s.strip_prefix('s'))?;
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    let n: u8 = rest.parse().ok()?;
    (1..=14).contains(&n).then_some(n)
}

/// Parses the text of a `ts-r.toml`. `None` when it has no `[sound]` table.
pub fn parse_toml(text: &str, file: &str) -> (Option<SoundConfig>, Vec<ConfigError>) {
    let mut r = Reader {
        file,
        errors: Vec::new(),
    };
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            let first = msg.lines().next().unwrap_or("").trim().to_owned();
            r.err(format!("cannot parse TOML ({first})"));
            return (None, r.errors);
        }
    };
    let Some(sound) = table.get("sound") else {
        return (None, r.errors);
    };
    let Val::Table(t) = from_toml(sound) else {
        r.err("'sound' must be a table".into());
        return (None, r.errors);
    };
    let mut cfg = SoundConfig {
        source: file.to_owned(),
        base_dir: path::directory_of(file).to_owned(),
        level: Level::Sound,
        ..SoundConfig::default()
    };
    r.read_sound(&mut cfg, &t);
    (Some(cfg), r.errors)
}

/// The name of the JSON configuration file.
pub const TSRCONFIG: &str = "tsrconfig.json";

/// The name of the TOML configuration file.
pub const TS_R_TOML: &str = "ts-r.toml";

/// Reads the `sound` member of a JSON object (`what` names the object in messages).
fn json_sound(
    obj: &ts_r_config::jsonc::Value,
    what: &str,
    file: &str,
    r: &mut Reader<'_>,
) -> Option<SoundConfig> {
    let sound = obj.get("sound")?;
    let Val::Table(t) = from_json(sound) else {
        r.err(format!("'{what}' must be an object"));
        return None;
    };
    let mut cfg = SoundConfig {
        source: file.to_owned(),
        base_dir: path::directory_of(file).to_owned(),
        level: Level::Sound,
        ..SoundConfig::default()
    };
    r.read_sound(&mut cfg, &t);
    Some(cfg)
}

/// Reads the `"ts-r": { "sound": ... }` key of a tsconfig.json text. Syntax errors are the
/// tsconfig's own (reported by the regular config load), so they are not repeated here.
pub fn parse_tsconfig_key(text: &str, file: &str) -> (Option<SoundConfig>, Vec<ConfigError>) {
    let mut r = Reader {
        file,
        errors: Vec::new(),
    };
    let parsed = ts_r_config::jsonc::parse(text, file);
    let Some(root) = parsed.root else {
        return (None, r.errors);
    };
    let Some(tsr) = root.get("ts-r") else {
        return (None, r.errors);
    };
    let cfg = json_sound(tsr, "ts-r.sound", file, &mut r);
    (cfg, r.errors)
}

/// Reads a `tsrconfig.json` text: JSONC with the shape of the tsconfig's `"ts-r"` key,
/// `{ "sound": { ... } }`. Other top-level keys are left for other tools. `None` when it has
/// no `sound` object.
pub fn parse_tsrconfig(text: &str, file: &str) -> (Option<SoundConfig>, Vec<ConfigError>) {
    let mut r = Reader {
        file,
        errors: Vec::new(),
    };
    let parsed = ts_r_config::jsonc::parse(text, file);
    if let Some(d) = parsed.diagnostics.first() {
        r.err(format!(
            "cannot parse JSON ({})",
            d.message.trim_end_matches('.')
        ));
        return (None, r.errors);
    }
    let Some(root) = parsed.root else {
        return (None, r.errors);
    };
    if root.as_object().is_none() {
        r.err(format!("'{TSRCONFIG}' must contain an object"));
        return (None, r.errors);
    }
    let cfg = json_sound(&root, "sound", file, &mut r);
    (cfg, r.errors)
}

/// Reads one configuration file by its kind: `.toml` is `ts-r.toml`; JSON with a top-level
/// `"ts-r"` key is a tsconfig; other JSON is a `tsrconfig.json` (`ts-r-check --sound-config`).
pub fn parse_file(text: &str, file: &str) -> (Option<SoundConfig>, Vec<ConfigError>) {
    if file.ends_with(".toml") {
        return parse_toml(text, file);
    }
    let is_tsconfig = ts_r_config::jsonc::parse(text, file)
        .root
        .is_some_and(|r| r.get("ts-r").is_some());
    if is_tsconfig {
        parse_tsconfig_key(text, file)
    } else {
        parse_tsrconfig(text, file)
    }
}

/// The nearest file named `name` in `dir` or an ancestor (at most 64 levels).
fn nearest(fs: &dyn FileSystem, dir: &str, name: &str) -> Option<String> {
    let mut dir = dir.to_owned();
    for _ in 0..64 {
        let candidate = path::combine(&dir, name);
        if fs.file_exists(&candidate) {
            return Some(candidate);
        }
        let parent = path::directory_of(&dir).to_owned();
        if parent == dir || parent.is_empty() {
            return None;
        }
        dir = parent;
    }
    None
}

/// `'a'`, `'a' and 'b'`, `'a', 'b' and 'c'`.
fn list_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Finds the configuration for the project whose tsconfig is `tsconfig_path`. Three places
/// can hold it (open question 16, decided 2026-10-15): the `"ts-r"` key of that tsconfig,
/// the nearest `tsrconfig.json` and the nearest `ts-r.toml` in its directory or an ancestor.
/// Setting it in more than one is TSR0020, one error naming every file involved, reported
/// on the file that is still used: the tsconfig key, else `tsrconfig.json`.
pub fn discover(
    fs: &dyn FileSystem,
    tsconfig_path: &str,
) -> (Option<SoundConfig>, Vec<ConfigError>) {
    let mut errors = Vec::new();
    // (configuration, how the error message names it), in precedence order.
    let mut found: Vec<(SoundConfig, String)> = Vec::new();
    if let Some(text) = fs.read_file(tsconfig_path) {
        let (c, e) = parse_tsconfig_key(&text, tsconfig_path);
        errors.extend(e);
        if let Some(c) = c {
            found.push((c, format!("the \"ts-r\" key of '{tsconfig_path}'")));
        }
    }
    type Parse = fn(&str, &str) -> (Option<SoundConfig>, Vec<ConfigError>);
    let dir = path::directory_of(tsconfig_path);
    let files: [(&str, Parse); 2] = [(TSRCONFIG, parse_tsrconfig), (TS_R_TOML, parse_toml)];
    for (name, parse) in files {
        let Some(file) = nearest(fs, dir, name) else {
            continue;
        };
        let Some(text) = fs.read_file(&file) else {
            continue;
        };
        let (c, e) = parse(&text, &file);
        errors.extend(e);
        if let Some(c) = c {
            found.push((c, format!("'{file}'")));
        }
    }
    if found.len() > 1 {
        let names: Vec<String> = found.iter().map(|(_, n)| n.clone()).collect();
        errors.push(ConfigError {
            file: found[0].0.source.clone(),
            message: format!(
                "sound mode is configured in more than one place: {}; keep one",
                list_and(&names)
            ),
        });
    }
    (found.into_iter().next().map(|(c, _)| c), errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_options_for_the_tools() {
        let mut fs = ts_r_config::MemoryFs::new();
        fs.add_file(
            "/p/ts-r.toml",
            "[sound.guard]\nexplain = \"none\"\nmax-depth = 64\nmax-validator-kb = 2\n",
        );
        let s = guard_options(&fs, "/p/app/tsconfig.json");
        assert_eq!(
            s.options,
            ts_r_guard::GuardOptions {
                explain: ts_r_guard::Explain::None,
                max_depth: 64,
                max_validator_kb: 2,
                refinements: Vec::new(),
            }
        );
        assert_eq!(s.source.as_deref(), Some("/p/ts-r.toml"));
        assert!(s.errors.is_empty());
        let none = guard_options(&ts_r_config::MemoryFs::new(), "/q/tsconfig.json");
        assert_eq!(none.options, ts_r_guard::GuardOptions::default());
        assert_eq!(none.source, None);
        fs.add_file(
            "/p/app/tsconfig.json",
            r#"{ "ts-r": { "sound": { "guard": { "explain": "first", "max-depth": "x" } } } }"#,
        );
        let both = guard_options(&fs, "/p/app/tsconfig.json");
        assert_eq!(both.options.explain, ts_r_guard::Explain::First);
        assert_eq!(both.options.max_depth, 512);
        let msgs: Vec<String> = both.errors.iter().map(ConfigError::render).collect();
        assert_eq!(msgs.len(), 2, "{msgs:?}");
        assert!(
            msgs.iter().any(|m| m.starts_with("/p/app/tsconfig.json(1,1)")
                && m.contains("configured in more than one place: the \"ts-r\" key of '/p/app/tsconfig.json' and '/p/ts-r.toml'; keep one")),
            "{msgs:?}"
        );
    }

    #[test]
    fn three_places_and_conflicts_name_every_file() {
        let mut fs = ts_r_config::MemoryFs::new();
        fs.add_file(
            "/p/app/tsrconfig.json",
            "// JSONC\n{ \"sound\": { \"level\": \"strict\", \"include\": [\"src/**\"], }, \"lint\": {} }",
        );
        let (cfg, errors) = discover(&fs, "/p/app/tsconfig.json");
        assert!(errors.is_empty(), "{errors:?}");
        let cfg = cfg.unwrap();
        assert_eq!(cfg.source, "/p/app/tsrconfig.json");
        assert_eq!(cfg.level, Level::Strict);
        assert_eq!(cfg.base_dir, "/p/app");

        // A tsrconfig.json without "sound" doesn't configure sound mode.
        let mut quiet = ts_r_config::MemoryFs::new();
        quiet.add_file("/q/tsrconfig.json", "{ \"lint\": {} }");
        quiet.add_file("/q/ts-r.toml", "[sound]\nlevel = \"sound\"\n");
        let (cfg, errors) = discover(&quiet, "/q/tsconfig.json");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.unwrap().source, "/q/ts-r.toml");

        fs.add_file("/p/ts-r.toml", "[sound]\nlevel = \"sound\"\n");
        let (cfg, errors) = discover(&fs, "/p/app/tsconfig.json");
        assert_eq!(cfg.unwrap().source, "/p/app/tsrconfig.json");
        let msgs: Vec<String> = errors.iter().map(ConfigError::render).collect();
        assert_eq!(
            msgs,
            vec![
                "/p/app/tsrconfig.json(1,1): error TSR0020: Invalid sound-mode configuration: sound mode is configured in more than one place: '/p/app/tsrconfig.json' and '/p/ts-r.toml'; keep one."
            ]
        );

        fs.add_file(
            "/p/app/tsconfig.json",
            r#"{ "ts-r": { "sound": { "level": "contracts" } } }"#,
        );
        let (cfg, errors) = discover(&fs, "/p/app/tsconfig.json");
        assert_eq!(cfg.unwrap().level, Level::Contracts);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "/p/app/tsconfig.json");
        assert!(
            errors[0].message.contains(
                "the \"ts-r\" key of '/p/app/tsconfig.json', '/p/app/tsrconfig.json' and '/p/ts-r.toml'"
            ),
            "{errors:?}"
        );

        let (_, errors) = parse_tsrconfig("{ \"sound\": ", "/r/tsrconfig.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].message.starts_with("cannot parse JSON"),
            "{errors:?}"
        );
        let (_, errors) = parse_tsrconfig("[1]", "/r/tsrconfig.json");
        assert_eq!(errors.len(), 1, "{errors:?}");

        // `ts-r-check --sound-config` reads any of the three kinds.
        let (c, _) = parse_file("[sound]\nlevel = \"strict\"\n", "/r/x.toml");
        assert_eq!(c.unwrap().level, Level::Strict);
        let (c, _) = parse_file(r#"{ "sound": { "level": "strict" } }"#, "/r/tsrconfig.json");
        assert_eq!(c.unwrap().level, Level::Strict);
        let (c, _) = parse_file(
            r#"{ "ts-r": { "sound": { "level": "strict" } } }"#,
            "/r/tsconfig.json",
        );
        assert_eq!(c.unwrap().level, Level::Strict);
    }

    #[test]
    fn reads_refinements() {
        let text = r#"
[sound.guard]
refinements = { Email = "./src/validators#isEmail", Int = "@acme/checks#isInt", Slug = "../shared/slug.ts#default" }
"#;
        let (cfg, errors) = parse_toml(text, "/p/app/ts-r.toml");
        assert!(errors.is_empty(), "{errors:?}");
        let r = cfg.unwrap().guard.options().refinements;
        let got: Vec<(&str, &str, &str, bool)> = r
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.module.as_str(),
                    r.export.as_str(),
                    r.is_path(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("Email", "/p/app/src/validators", "isEmail", true),
                ("Int", "@acme/checks", "isInt", false),
                ("Slug", "/p/shared/slug.ts", "default", true),
            ]
        );
        let bad = r#"
[sound.guard.refinements]
Email = "./validators"
"bad name" = "./v#isX"
Int = "./v#not an export"
Id = 3
"#;
        let (cfg, errors) = parse_toml(bad, "/p/ts-r.toml");
        assert!(cfg.unwrap().guard.refinements.is_empty());
        let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(msgs.len(), 4, "{msgs:?}");
        assert!(msgs[0].contains("\"./validators#isEmail\""), "{msgs:?}");
        let json = r#"{ "ts-r": { "sound": { "guard": { "refinements": { "Email": "./v#isEmail" } } } } }"#;
        let (cfg, errors) = parse_tsconfig_key(json, "/p/tsconfig.json");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.unwrap().guard.refinements[0].module, "/p/v");
    }

    #[test]
    fn retired_keys_are_unknown() {
        for key in [
            "overload-implementations = \"allow\"",
            "guard.classes = \"reject\"",
            "guard.extra-properties = \"reject\"",
        ] {
            let (_, errors) = parse_toml(&format!("[sound]\n{key}\n"), "/p/ts-r.toml");
            assert_eq!(errors.len(), 1, "{key}: {errors:?}");
            assert!(errors[0].message.contains("unknown key"), "{errors:?}");
        }
        assert!(!Policy::default().exports_declare_throws);
    }

    #[test]
    fn reads_the_record_example() {
        let text = r#"
[sound]
level = "sound"
include = ["src/**"]
exclude = ["src/legacy/**", "**/*.test.ts"]
unchecked-errors = []

[sound.packages]
default = "trusted"
untyped = "loose"
"lodash" = "verify"
"@acme/core" = "sound"

[sound.sources]
"@acme/rpc" = ["Client.call"]

[sound.guard]
explain = "full"
max-depth = 512
max-validator-kb = 8
"#;
        let (cfg, errors) = parse_toml(text, "/p/ts-r.toml");
        assert!(errors.is_empty(), "{errors:?}");
        let cfg = cfg.unwrap();
        assert_eq!(cfg.level, Level::Sound);
        assert_eq!(cfg.base_dir, "/p");
        assert_eq!(cfg.package_mode("lodash"), Some(PackageMode::Verify));
        assert_eq!(cfg.packages_untyped, Some(PackageMode::Loose));
        assert_eq!(cfg.sources[0].1, vec!["Client.call".to_owned()]);
        assert_eq!(cfg.configured_level("/p/src/a.ts", true), Level::Sound);
        assert_eq!(cfg.configured_level("/p/src/legacy/a.ts", true), Level::Off);
        assert_eq!(cfg.configured_level("/p/src/a.test.ts", true), Level::Off);
        assert_eq!(cfg.configured_level("/p/other/a.ts", true), Level::Off);
    }

    #[test]
    fn reports_bad_values() {
        let (cfg, errors) = parse_toml("[sound]\nlevel = \"loud\"\nfoo = 1\n", "/p/ts-r.toml");
        assert!(cfg.is_some());
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("'level' must be one of"))
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("unknown key 'foo'"))
        );
        let (_, errors) = parse_toml("[sound\n", "/p/ts-r.toml");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn reads_the_tsconfig_key_and_rules() {
        let text = r#"{ "compilerOptions": {}, "ts-r": { "sound": { "level": "strict",
            "include": ["src/**"], "rules": { "S-09": "off" }, "loose-values": "unknown" } } }"#;
        let (cfg, errors) = parse_tsconfig_key(text, "/p/tsconfig.json");
        assert!(errors.is_empty(), "{errors:?}");
        let cfg = cfg.unwrap();
        assert_eq!(cfg.level, Level::Strict);
        assert!(!cfg.rule_on(9));
        assert!(cfg.policy.loose_values_unknown);
    }

    #[test]
    fn rule_ids() {
        assert_eq!(parse_rule_id("S-01"), Some(1));
        assert_eq!(parse_rule_id("S-14"), Some(14));
        assert_eq!(parse_rule_id("S-99"), None);
        assert_eq!(parse_rule_id("X-01"), None);
    }
}
