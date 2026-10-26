//! The rule engine: one traversal per file runs every enabled rule.

use std::sync::Arc;

use ts_r_ast::{Ast, NodeId, NodeKind, ParentMap, SourceKind, Visitor, Walk, walk};
use ts_r_span::{LineIndex, Span};

use crate::diagnostic::{Diagnostic, Severity};
use crate::directives::{InlineConfig, ProblemPos, UnusedDirectiveOptions, apply_directives};
use crate::env::LintEnv;
use crate::fix::apply_fixes;
use crate::rule::{Ctx, FileCtx, Rule, RuleDef};
use crate::types::{FileKey, TypeInfo};

/// One enabled rule with its severity.
#[derive(Clone, Debug)]
pub struct ConfiguredRule {
    pub def: &'static RuleDef,
    pub severity: Severity,
    pub rule: Arc<dyn Rule>,
}

/// The enabled rules for a file, with per-node-kind dispatch tables.
#[derive(Debug, Default)]
pub struct RuleSet {
    pub rules: Vec<ConfiguredRule>,
    enter: Vec<Vec<u16>>,
    exit: Vec<Vec<u16>>,
    file_rules: Vec<u16>,
    /// The same set without type-aware rules (built on first use).
    untyped: std::sync::OnceLock<Arc<RuleSet>>,
    /// Only the type-aware rules of the set (built on first use).
    typed: std::sync::OnceLock<Arc<RuleSet>>,
}

impl RuleSet {
    pub fn new(mut rules: Vec<ConfiguredRule>) -> RuleSet {
        rules.retain(|r| r.severity != Severity::Off);
        rules.sort_by_key(|r| r.def.meta.name);
        let n = NodeKind::ALL.len();
        let mut enter = vec![Vec::new(); n];
        let mut exit = vec![Vec::new(); n];
        let mut file_rules = Vec::new();
        for (i, r) in rules.iter().enumerate() {
            for k in r.rule.kinds() {
                enter[*k as usize].push(i as u16);
            }
            for k in r.rule.exit_kinds() {
                exit[*k as usize].push(i as u16);
            }
            if r.rule.has_run_file() {
                file_rules.push(i as u16);
            }
        }
        RuleSet {
            rules,
            enter,
            exit,
            file_rules,
            untyped: std::sync::OnceLock::new(),
            typed: std::sync::OnceLock::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// True when a rule needs type information.
    pub fn has_typed(&self) -> bool {
        self.rules.iter().any(|r| r.def.meta.requires_types)
    }

    /// Names of the type-aware rules in the set.
    pub fn typed_names(&self) -> Vec<&'static str> {
        self.rules
            .iter()
            .filter(|r| r.def.meta.requires_types)
            .map(|r| r.def.meta.name)
            .collect()
    }

    /// The set without type-aware rules (for files without type information).
    pub fn untyped(&self) -> Arc<RuleSet> {
        self.untyped
            .get_or_init(|| {
                Arc::new(RuleSet::new(
                    self.rules
                        .iter()
                        .filter(|r| !r.def.meta.requires_types)
                        .cloned()
                        .collect(),
                ))
            })
            .clone()
    }

    /// Only the type-aware rules (the editor runs them on its type backend's checker and the
    /// rest without types).
    pub fn typed_only(&self) -> Arc<RuleSet> {
        self.typed
            .get_or_init(|| {
                Arc::new(RuleSet::new(
                    self.rules
                        .iter()
                        .filter(|r| r.def.meta.requires_types)
                        .cloned()
                        .collect(),
                ))
            })
            .clone()
    }

    pub fn get(&self, name: &str) -> Option<&ConfiguredRule> {
        self.rules.iter().find(|r| r.def.meta.name == name)
    }
}

/// Everything resolved from config for one file.
#[derive(Clone, Debug)]
pub struct FileConfig {
    pub rules: Arc<RuleSet>,
    pub env: LintEnv,
    /// Severity of unused `eslint-disable` reports (`linterOptions.reportUnusedDisableDirectives`).
    pub report_unused_disable_directives: Severity,
    /// `linterOptions.noInlineConfig`.
    pub no_inline_config: bool,
    /// `--quiet`: warning rules are not run, including ones set to `warn` by inline config.
    pub quiet: bool,
    /// Rules configured (not off) for this file that ts-r-lint did not run (unsupported).
    pub skipped_rules: Arc<Vec<String>>,
    /// Resolves rule names (inline `/* eslint */` comments) to definitions.
    pub registry: &'static crate::registry::Registry,
    /// The type-aware half of a split run ([`FileConfig::typed_only`]): inline config adds
    /// no untyped rules (the untyped half runs those).
    pub typed_half: bool,
}

impl FileConfig {
    /// The type-aware half of a split run: the type-aware rules only, without unused
    /// `eslint-disable` reports (the untyped half makes those).
    pub fn typed_only(&self) -> FileConfig {
        FileConfig {
            rules: self.rules.typed_only(),
            report_unused_disable_directives: Severity::Off,
            typed_half: true,
            ..self.clone()
        }
    }
}

/// The result of linting one file.
#[derive(Clone, Debug, Default)]
pub struct FileResult {
    pub diagnostics: Vec<Diagnostic>,
    /// Problems suppressed by `eslint-disable` comments.
    pub suppressed: Vec<Diagnostic>,
    /// The fixed source when `--fix` changed something.
    pub output: Option<String>,
    /// Rule names from inline config that ts-r-lint does not know.
    pub unknown_inline_rules: Vec<String>,
}

struct Runner<'r, 'f, 'a> {
    set: &'r RuleSet,
    file: &'f FileCtx<'a>,
    out: &'r mut Vec<Diagnostic>,
}

impl Visitor for Runner<'_, '_, '_> {
    fn enter(&mut self, _ast: &Ast, id: NodeId) -> Walk {
        let k = self.file.ast.kind(id) as usize;
        for &ri in &self.set.enter[k] {
            let r = &self.set.rules[ri as usize];
            let mut cx = Ctx {
                file: self.file,
                rule: r.def.meta.name,
                severity: r.severity,
                out: self.out,
            };
            r.rule.enter(id, &mut cx);
        }
        Walk::Continue
    }
    fn leave(&mut self, _ast: &Ast, id: NodeId) {
        let k = self.file.ast.kind(id) as usize;
        for &ri in &self.set.exit[k] {
            let r = &self.set.rules[ri as usize];
            let mut cx = Ctx {
                file: self.file,
                rule: r.def.meta.name,
                severity: r.severity,
                out: self.out,
            };
            r.rule.exit(id, &mut cx);
        }
    }
}

/// Runs `set` over an already-built file context.
pub fn run_rules(file: &FileCtx<'_>, set: &RuleSet) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if set.is_empty() {
        return out;
    }
    {
        let mut runner = Runner {
            set,
            file,
            out: &mut out,
        };
        walk(file.ast, file.ast.root, &mut runner);
    }
    for &ri in &set.file_rules {
        let r = &set.rules[ri as usize];
        let mut cx = Ctx {
            file,
            rule: r.def.meta.name,
            severity: r.severity,
            out: &mut out,
        };
        r.rule.run_file(&mut cx);
    }
    out
}

/// Lints one source text once (no fixing).
pub fn lint_source(
    path: &str,
    src: &str,
    cfg: &FileConfig,
    types: &dyn TypeInfo,
    file_key: FileKey,
) -> FileResult {
    let parsed = parse_for_lint(path, src, &cfg.env);
    // tsc's checker grammar errors are no parse errors to typescript-eslint.
    lint_parsed(
        path,
        src,
        cfg,
        types,
        file_key,
        &parsed.ast,
        &parsed.syntax_errors(),
    )
}

/// TS8xxx "can only be used in TypeScript files": the program parses JavaScript files with
/// them, typescript-eslint does not treat them as parse errors.
pub fn is_ts_only_in_js_error(code: u32) -> bool {
    (8000..9000).contains(&code)
}

/// Lints an already parsed file (the checker program's own parse when types are on, so
/// type queries can use node ids). `parse_errors` are the parser's diagnostics; the first
/// error makes the file fatal. Type-aware rules run only when `types` is available.
pub fn lint_parsed(
    path: &str,
    src: &str,
    cfg: &FileConfig,
    types: &dyn TypeInfo,
    file_key: FileKey,
    ast: &Ast,
    parse_errors: &[ts_r_diagnostics::Diagnostic],
) -> FileResult {
    let lines = LineIndex::new(src);
    if let Some(first) = parse_errors
        .iter()
        .find(|d| d.is_error() && !is_ts_only_in_js_error(d.code))
    {
        return FileResult {
            diagnostics: vec![Diagnostic {
                rule: None,
                severity: Severity::Error,
                span: Span::at(first.span.start),
                message: format!("Parsing error: {}", first.message),
                message_id: None,
                fix: None,
                suggestions: Vec::new(),
                fatal: true,
                loc: None,
            }],
            ..FileResult::default()
        };
    }
    let parents = ParentMap::build(ast);
    let inline = if cfg.no_inline_config {
        InlineConfig::default()
    } else {
        InlineConfig::parse(ast, src, &lines)
    };

    // Inline `/* eslint rule: ... */` configuration changes the rule set for this file.
    let mut unknown_inline = Vec::new();
    let mut inline_problems = Vec::new();
    let custom_set;
    let untyped;
    let base: &RuleSet = if types.is_available() || !cfg.rules.has_typed() {
        &cfg.rules
    } else {
        untyped = cfg.rules.untyped();
        &untyped
    };
    let set: &RuleSet = if inline.rules.is_empty() {
        base
    } else {
        let mut rules = base.rules.clone();
        for rc in &inline.rules {
            match cfg.registry.configure(&rc.name, &rc.value) {
                Ok(Some(r)) => {
                    rules.retain(|x| x.def.meta.name != r.def.meta.name);
                    let runs_here = if r.def.meta.requires_types {
                        types.is_available()
                    } else {
                        !cfg.typed_half
                    };
                    if runs_here && !(cfg.quiet && r.severity == Severity::Warn) {
                        rules.push(r);
                    }
                }
                Ok(None) => {
                    rules.retain(|x| x.def.meta.name != rc.name);
                }
                Err(crate::registry::ConfigureError::Unknown) => {
                    unknown_inline.push(rc.name.clone());
                }
                Err(crate::registry::ConfigureError::Invalid(msg)) => {
                    inline_problems.push(Diagnostic {
                        rule: None,
                        severity: Severity::Error,
                        span: rc.span,
                        message: format!(
                            "Inline configuration for rule \"{}\" is invalid:\n\t{msg}\n",
                            rc.name
                        ),
                        message_id: None,
                        fix: None,
                        suggestions: Vec::new(),
                        fatal: false,
                        loc: None,
                    });
                }
            }
        }
        custom_set = RuleSet::new(rules);
        &custom_set
    };

    // A lib that comes from the file's tsconfig is resolved per file (cached), and so are
    // decorator options the parser options leave unset.
    let project_env;
    let env = match &cfg.env.project_lib {
        Some(p)
            if p.lib_from_tsconfig
                || p.emit_decorator_metadata.is_none()
                || p.experimental_decorators.is_none() =>
        {
            let abs;
            let file = if std::path::Path::new(path).is_absolute() {
                path
            } else {
                abs = p.root_dir.join(path).to_string_lossy().into_owned();
                &abs
            };
            project_env = LintEnv {
                lib: if p.lib_from_tsconfig {
                    Some(p.resolve(file))
                } else {
                    cfg.env.lib.clone()
                },
                decorator_metadata: cfg.env.typescript && p.decorator_metadata(file),
                ..cfg.env.clone()
            };
            &project_env
        }
        _ => &cfg.env,
    };
    let file = FileCtx::new(
        path,
        src,
        ast,
        &parents,
        &lines,
        env,
        types,
        file_key,
        &inline.globals,
        &inline.exported,
    );
    let mut diags = run_rules(&file, set);
    diags.extend(inline_problems);
    for (span, msg) in &inline.problems {
        diags.push(Diagnostic {
            rule: None,
            severity: Severity::Error,
            span: *span,
            message: msg.clone(),
            message_id: None,
            fix: None,
            suggestions: Vec::new(),
            fatal: false,
            loc: None,
        });
    }
    sort_diagnostics(&mut diags);

    // Disable directives.
    let mut result = FileResult {
        unknown_inline_rules: unknown_inline,
        ..FileResult::default()
    };
    if inline.directives.is_empty() {
        result.diagnostics = diags;
        return result;
    }
    let problems: Vec<ProblemPos> = diags
        .iter()
        .map(|d| ProblemPos {
            rule: d.rule,
            pos: d.span.start,
            line: lines.line_of(d.span.start),
        })
        .collect();
    let skipped = &cfg.skipped_rules;
    let known = |name: &str| {
        set.get(name).is_some()
            || (!skipped.iter().any(|s| s == name) && cfg.registry.find(name).is_some())
    };
    let outcome = apply_directives(
        &inline.directives,
        &problems,
        src,
        Some(UnusedDirectiveOptions {
            severity: cfg.report_unused_disable_directives,
        }),
        &known,
        skipped.is_empty(),
    );
    for (d, s) in diags.into_iter().zip(outcome.suppressed) {
        if s {
            result.suppressed.push(d);
        } else {
            result.diagnostics.push(d);
        }
    }
    result.diagnostics.extend(outcome.unused);
    sort_diagnostics(&mut result.diagnostics);
    result
}

/// ESLint's maximum number of fix passes.
pub const MAX_FIX_PASSES: usize = 10;

/// Lints and applies fixes until nothing changes (at most [`MAX_FIX_PASSES`] passes). The
/// returned diagnostics describe the final text.
pub fn lint_and_fix(
    path: &str,
    src: &str,
    cfg: &FileConfig,
    types: &dyn TypeInfo,
    file_key: FileKey,
) -> FileResult {
    let mut text = src.to_owned();
    let mut changed = false;
    for _ in 0..MAX_FIX_PASSES {
        let r = lint_source(path, &text, cfg, types, file_key);
        if r.diagnostics.iter().any(|d| d.fatal) {
            break;
        }
        let fixes: Vec<&crate::diagnostic::Fix> = r
            .diagnostics
            .iter()
            .filter_map(|d| d.fix.as_ref())
            .collect();
        if fixes.is_empty() {
            break;
        }
        let (next, applied) = apply_fixes(&text, &fixes);
        if applied == 0 || next == text {
            break;
        }
        text = next;
        changed = true;
    }
    let mut r = lint_source(path, &text, cfg, types, file_key);
    if changed {
        r.output = Some(text);
    }
    r
}

/// Sorts by position, then rule name (ESLint orders by line and column).
pub fn sort_diagnostics(d: &mut [Diagnostic]) {
    d.sort_by(|a, b| {
        a.span
            .start
            .cmp(&b.span.start)
            .then_with(|| a.rule.cmp(&b.rule))
    });
}

/// Parses a file for linting. With the TypeScript parser configured, a `.js`/`.jsx` file
/// takes TypeScript syntax the way typescript-eslint does: tsc's grammar for JavaScript files
/// (JSX on, no type arguments on expressions), without tsc's TS8xxx "can only be used in
/// TypeScript files" errors, which typescript-eslint does not treat as parse errors.
pub fn parse_for_lint(path: &str, src: &str, env: &LintEnv) -> ts_r_parser::ParseResult {
    let ts_in_js = if env.typescript {
        ts_r_parser::TsInJs::Allow
    } else {
        ts_r_parser::TsInJs::Reject
    };
    ts_r_parser::parse_with(
        src,
        SourceKind::from_path(path),
        ts_r_parser::ParseOptions {
            ts_in_js,
            // espree and typescript-estree parse `sourceType: "module"` with the module goal
            // (top-level `await` is the operator), and typescript-estree keeps parsing
            // binary operators after `as T` / `satisfies T` as TypeScript 5 does.
            module: env.source_type == crate::env::SourceType::Module,
            assertion_continues: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::types::NoTypeInfo;

    /// The rule names reported for `src` with `cfg`, without type information.
    fn reported(cfg: &super::FileConfig, src: &str) -> Vec<&'static str> {
        let r = crate::lint_source(
            "/p/a.ts",
            src,
            cfg,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        );
        r.diagnostics.iter().filter_map(|d| d.rule).collect()
    }

    #[test]
    fn inline_config_keeps_the_typed_half_typed() {
        let reg = crate::Registry::global();
        let json: serde_json::Value = serde_json::from_str(
            r#"[{"files": ["**/*.ts"], "rules": {"@typescript-eslint/no-floating-promises": "error"}}]"#,
        )
        .unwrap();
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let mut opts = ResolveOptions::new();
        opts.types_available = true;
        let fc = file.file_config("a.ts", &opts).unwrap();
        assert!(fc.rules.has_typed());
        let src = "/* eslint no-debugger: \"error\" */\ndebugger;\n";
        // The untyped half (no types) runs the inline rule; the typed half leaves it to it.
        assert_eq!(reported(&fc, src), ["no-debugger"]);
        assert!(reported(&fc.typed_only(), src).is_empty());
    }

    #[test]
    fn quiet_skips_rules_inline_config_sets_to_warn() {
        let v = serde_json::json!([{ "files": ["**/*.js"] }]);
        let objs = objects_from_eslint_json(&v, crate::Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let quiet = ResolveOptions {
            quiet: true,
            ..ResolveOptions::new()
        };
        let fc = cfg.file_config("a.js", &quiet).unwrap();
        let src = "/* eslint no-var: \"warn\" */\nvar x = 1; console.log(x);\n";
        // ESLint 9.39 with `--quiet --fix` leaves this file unchanged.
        let r = lint_and_fix("/p/a.js", src, &fc, &NoTypeInfo, FileKey(0));
        assert_eq!(r.output, None);
        assert!(r.diagnostics.is_empty());
        let src = "/* eslint no-var: \"error\" */\nvar x = 1; console.log(x);\n";
        let r = lint_and_fix("/p/a.js", src, &fc, &NoTypeInfo, FileKey(0));
        assert!(r.output.is_some_and(|o| o.contains("let x")));
    }

    /// `sourceType: "module"` parses with the module goal: top-level `await` followed by a
    /// line break is the operator, as espree and typescript-estree read it.
    #[test]
    fn parse_for_lint_module_goal() {
        let env = LintEnv::default();
        let parsed = parse_for_lint("/p/a.js", "await\nPromise.all([x]);\n", &env);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert!(
            (1..parsed.ast.nodes.len() as u32)
                .filter_map(ts_r_ast::NodeId::from_raw)
                .any(|n| parsed.ast.kind(n) == ts_r_ast::NodeKind::Await)
        );
    }

    /// Binary operators keep binding after `as T` (`x < 0 as any ** 2`), as TypeScript 5 and
    /// typescript-estree parse it.
    #[test]
    fn parse_for_lint_assertion_continues() {
        let env = LintEnv::default();
        let parsed = parse_for_lint("/p/a.ts", "x < 0 as any ** 2;\n", &env);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }
}
