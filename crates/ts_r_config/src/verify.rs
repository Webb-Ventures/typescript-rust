//! Program-level option checks: options TS 7 removed (TS5102/TS5108) and invalid option
//! combinations (TS5052, TS5053, TS5069, ...). tsc reports these when it builds a program,
//! not under `--showConfig`.

use crate::diagnostic::ConfigDiagnostic;
use crate::enums::{JsxEmit, ModuleKind, ModuleResolution, ScriptTarget};
use crate::options::{CompilerOptions, OptionId, OptionValue};
use crate::path;

/// How an option changed between TypeScript 6 and 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// The option itself was removed (TS5102).
    Option,
    /// One value of the option was removed (TS5108).
    Value(&'static str),
}

/// Options and values TS 6 deprecated and TS 7 removed but still parses (so it can
/// report them). Options removed entirely are in
/// [`crate::options::REMOVED_UNKNOWN_OPTIONS`].
pub const REMOVED_IN_TS7: &[(&str, Removal)] = &[
    ("baseUrl", Removal::Option),
    ("outFile", Removal::Option),
    ("downlevelIteration", Removal::Option),
    ("target", Removal::Value("ES5")),
    ("module", Removal::Value("AMD")),
    ("module", Removal::Value("UMD")),
    ("module", Removal::Value("System")),
    ("moduleResolution", Removal::Value("node10")),
    ("moduleResolution", Removal::Value("Classic")),
    ("esModuleInterop", Removal::Value("false")),
    ("allowSyntheticDefaultImports", Removal::Value("false")),
    ("alwaysStrict", Removal::Value("false")),
];

struct Ctx<'a> {
    o: &'a CompilerOptions,
    /// The leaf config file. Errors about options set in other files (bases) are anchored
    /// like errors about options not set at all, as tsc 7 does.
    leaf: Option<String>,
    out: Vec<ConfigDiagnostic>,
    /// Location used for errors about options that were not set in the leaf config: its
    /// `compilerOptions` key, when it has one (else the error has no location).
    fallback: Option<(String, (u32, u32))>,
}

impl Ctx<'_> {
    fn local_origin(&self, id: OptionId) -> Option<&crate::options::Origin> {
        let o = self.o.origin(id)?;
        match &self.leaf {
            Some(l) if **l != *o.file => None,
            _ => Some(o),
        }
    }

    /// Like [`Self::key`] but anchored at whichever of `a`/`b` comes first in the leaf
    /// config (tsc's behaviour for TS5052/TS5053).
    fn first_key(&mut self, a: OptionId, b: OptionId, code: u32, msg: String) {
        let pick = match (self.local_origin(a), self.local_origin(b)) {
            (Some(x), Some(y)) if y.key_span.0 < x.key_span.0 => b,
            (None, Some(_)) => b,
            _ => a,
        };
        self.key(pick, code, msg);
    }

    fn key(&mut self, id: OptionId, code: u32, msg: String) -> Option<&mut ConfigDiagnostic> {
        let d = match self.local_origin(id) {
            Some(o) => ConfigDiagnostic::at(code, msg, &o.file, o.key_span),
            None => match &self.fallback {
                Some((f, s)) => ConfigDiagnostic::at(code, msg, f, *s),
                None => ConfigDiagnostic::global(code, msg),
            },
        };
        self.out.push(d);
        self.out.last_mut()
    }

    fn value(&mut self, id: OptionId, code: u32, msg: String) {
        let d = match self.local_origin(id) {
            Some(o) => ConfigDiagnostic::at(code, msg, &o.file, o.value_span),
            None => match &self.fallback {
                Some((f, s)) => ConfigDiagnostic::at(code, msg, f, *s),
                None => ConfigDiagnostic::global(code, msg),
            },
        };
        self.out.push(d);
    }

    fn set(&self, id: OptionId) -> bool {
        self.o.is_set(id)
    }

    fn truthy(&self, id: OptionId) -> bool {
        matches!(self.o.get(id), Some(OptionValue::Bool(true)))
            || matches!(
                self.o.get(id),
                Some(OptionValue::String(_) | OptionValue::List(_) | OptionValue::Paths(_))
            )
    }
}

/// Runs tsc 7's program-level option checks. `compiler_options_key` is the location of the
/// leaf config's `compilerOptions` key (tsc anchors errors about implied values there).
pub fn verify_options(
    o: &CompilerOptions,
    compiler_options_key: Option<(&str, (u32, u32))>,
) -> Vec<ConfigDiagnostic> {
    let mut c = Ctx {
        o,
        leaf: o.config_file_path.clone(),
        out: Vec::new(),
        fallback: compiler_options_key.map(|(f, s)| (f.to_owned(), s)),
    };
    let e = o.effective();

    // --- Removed in TS 7 -------------------------------------------------------------
    for id in [
        OptionId::BaseUrl,
        OptionId::OutFile,
        OptionId::DownlevelIteration,
    ] {
        if c.set(id) {
            let msg = format!(
                "Option '{}' has been removed. Please remove it from your configuration.",
                id.name()
            );
            let hint = if id == OptionId::BaseUrl {
                // Relative to the leaf config, not to the config that set `baseUrl`, and
                // `./`-prefixed unless it starts with `../` (`..` is `./../*`; observed with
                // tsc 7.0.2 on rxjs `tools/marbles` and playwright `tests`).
                o.base_url().map(|b| {
                    let dir = c
                        .leaf
                        .as_deref()
                        .or(o.origin(id).map(|or| &*or.file))
                        .map(|f| path::directory_of(f).to_owned())
                        .unwrap_or_default();
                    let rel = path::relative(&dir, b, true);
                    let star = if rel.is_empty() {
                        "./*".to_owned()
                    } else if path::is_rooted(&rel) || rel.starts_with("../") {
                        format!("{rel}/*")
                    } else {
                        format!("./{rel}/*")
                    };
                    format!("Use '\"paths\": {{\"*\": [\"{star}\"]}}' instead.")
                })
            } else {
                None
            };
            if let Some(d) = c.key(id, 5102, msg)
                && let Some(h) = hint
            {
                d.related.push(h);
            }
        }
    }
    let removed_value = |name: &str, val: &str| {
        format!("Option '{name}={val}' has been removed. Please remove it from your configuration.")
    };
    if o.target() == Some(ScriptTarget::ES5) {
        c.value(OptionId::Target, 5108, removed_value("target", "ES5"));
    }
    if let Some(m) = o.module().and_then(|m| m.removed_display()) {
        c.value(OptionId::Module, 5108, removed_value("module", m));
    }
    match o.module_resolution() {
        Some(ModuleResolution::Node10) => c.value(
            OptionId::ModuleResolution,
            5108,
            removed_value("moduleResolution", "node10"),
        ),
        Some(ModuleResolution::Classic) => c.value(
            OptionId::ModuleResolution,
            5108,
            removed_value("moduleResolution", "Classic"),
        ),
        _ => {}
    }
    for id in [
        OptionId::EsModuleInterop,
        OptionId::AllowSyntheticDefaultImports,
        OptionId::AlwaysStrict,
    ] {
        if matches!(o.get(id), Some(OptionValue::Bool(false))) {
            c.value(id, 5108, removed_value(id.name(), "false"));
        }
    }

    // --- Module / resolution compatibility ------------------------------------------
    let module = e.module();
    let resolution = e.resolution_algorithm();
    if resolution == ModuleResolution::Bundler
        && matches!(
            module,
            ModuleKind::AMD | ModuleKind::UMD | ModuleKind::System
        )
    {
        let msg = "Option 'bundler' can only be used when 'module' is set to 'preserve', 'commonjs', or 'es2015' or later.".to_owned();
        c.value(OptionId::ModuleResolution, 5095, msg);
    }
    // `bundler` under a Node module kind: TS5095 and TS5109, both at the resolution value.
    if module.is_node() && e.module_resolution() == ModuleResolution::Bundler {
        let msg = "Option 'bundler' can only be used when 'module' is set to 'preserve', 'commonjs', or 'es2015' or later.".to_owned();
        c.value(OptionId::ModuleResolution, 5095, msg);
        let (want, shown) = match module {
            ModuleKind::Node16 => ("Node16", "Node16"),
            ModuleKind::Node18 => ("Node16", "Node18"),
            ModuleKind::Node20 => ("Node16", "Node20"),
            _ => ("NodeNext", "NodeNext"),
        };
        c.value(
            OptionId::ModuleResolution,
            5109,
            format!("Option 'moduleResolution' must be set to '{want}' (or left unspecified) when option 'module' is set to '{shown}'."),
        );
    }
    if matches!(
        e.module_resolution(),
        ModuleResolution::Node16 | ModuleResolution::NodeNext
    ) && !module.is_node()
        && c.set(OptionId::ModuleResolution)
    {
        let name = if e.module_resolution() == ModuleResolution::Node16 {
            "Node16"
        } else {
            "NodeNext"
        };
        c.value(
            OptionId::Module,
            5110,
            format!("Option 'module' must be set to '{name}' when option 'moduleResolution' is set to '{name}'."),
        );
    }

    // --- Option combinations ----------------------------------------------------------
    let without = |c: &mut Ctx<'_>, id: OptionId, needed: OptionId| {
        let msg = format!(
            "Option '{}' cannot be specified without specifying option '{}'.",
            id.name(),
            needed.name()
        );
        c.first_key(id, needed, 5052, msg);
    };
    let with = |c: &mut Ctx<'_>, id: OptionId, other: OptionId| {
        let msg = format!(
            "Option '{}' cannot be specified with option '{}'.",
            id.name(),
            other.name()
        );
        c.first_key(id, other, 5053, msg);
    };

    let explicit_false =
        |c: &Ctx<'_>, id: OptionId| matches!(c.o.get(id), Some(OptionValue::Bool(false)));
    if c.truthy(OptionId::Composite) {
        if explicit_false(&c, OptionId::Declaration) {
            c.key(
                OptionId::Declaration,
                6304,
                "Composite projects may not disable declaration emit.".into(),
            );
        }
        // tsc anchors this one at the `declaration` key when there is one, else at
        // `compilerOptions`.
        if explicit_false(&c, OptionId::Incremental) {
            c.key(
                OptionId::Declaration,
                6379,
                "Composite projects may not disable incremental compilation.".into(),
            );
        }
    }
    // An explicit `incremental` (not one implied by `composite`) needs somewhere to put
    // its build info: a config file, or `tsBuildInfoFile`.
    if c.truthy(OptionId::Incremental)
        && !c.set(OptionId::TsBuildInfoFile)
        && o.config_file_path.as_deref().is_none_or(str::is_empty)
    {
        c.out.push(ConfigDiagnostic::global(
            5074,
            "Option '--incremental' is only valid with a known configuration file (like \
             'tsconfig.json') or when '--tsBuildInfoFile' is explicitly provided.",
        ));
    }
    if explicit_false(&c, OptionId::PreserveConstEnums) {
        let by = if c.truthy(OptionId::VerbatimModuleSyntax) {
            Some(OptionId::VerbatimModuleSyntax)
        } else if c.truthy(OptionId::IsolatedModules) {
            Some(OptionId::IsolatedModules)
        } else {
            None
        };
        if let Some(by) = by {
            let msg = format!(
                "Option 'preserveConstEnums' cannot be disabled when '{}' is enabled.",
                by.name()
            );
            c.first_key(OptionId::PreserveConstEnums, by, 5091, msg);
        }
    }
    if c.truthy(OptionId::StrictPropertyInitialization)
        && !e.strict_flag(crate::options::StrictFlag::StrictNullChecks)
    {
        without(
            &mut c,
            OptionId::StrictPropertyInitialization,
            OptionId::StrictNullChecks,
        );
    }
    // `checkJs` implies `allowJs`, so only an explicit `allowJs: false` conflicts.
    if c.truthy(OptionId::CheckJs) && explicit_false(&c, OptionId::AllowJs) {
        without(&mut c, OptionId::CheckJs, OptionId::AllowJs);
    }
    if c.truthy(OptionId::IsolatedDeclarations) && c.truthy(OptionId::AllowJs) {
        with(&mut c, OptionId::AllowJs, OptionId::IsolatedDeclarations);
    }
    if c.truthy(OptionId::ExactOptionalPropertyTypes)
        && !e.strict_flag(crate::options::StrictFlag::StrictNullChecks)
    {
        without(
            &mut c,
            OptionId::ExactOptionalPropertyTypes,
            OptionId::StrictNullChecks,
        );
    }
    if c.truthy(OptionId::InlineSourceMap) {
        if c.truthy(OptionId::SourceMap) {
            with(&mut c, OptionId::SourceMap, OptionId::InlineSourceMap);
        }
        if c.truthy(OptionId::MapRoot) {
            with(&mut c, OptionId::MapRoot, OptionId::InlineSourceMap);
        }
    }
    if c.truthy(OptionId::Lib) && c.truthy(OptionId::NoLib) {
        with(&mut c, OptionId::Lib, OptionId::NoLib);
    }
    if c.truthy(OptionId::EmitDecoratorMetadata) && !c.truthy(OptionId::ExperimentalDecorators) {
        without(
            &mut c,
            OptionId::EmitDecoratorMetadata,
            OptionId::ExperimentalDecorators,
        );
    }
    if c.set(OptionId::JsxFactory) {
        if c.set(OptionId::ReactNamespace) {
            with(&mut c, OptionId::ReactNamespace, OptionId::JsxFactory);
        }
        if let Some(f) = o.jsx_factory()
            && !is_entity_name(f)
        {
            c.value(
                    OptionId::JsxFactory,
                    5067,
                    format!("Invalid value for 'jsxFactory'. '{f}' is not a valid identifier or qualified-name."),
                );
        }
    } else if let Some(ns) = o.react_namespace()
        && !is_identifier(ns)
    {
        c.value(
            OptionId::ReactNamespace,
            5059,
            format!("Invalid value for '--reactNamespace'. '{ns}' is not a valid identifier."),
        );
    }
    if let Some(f) = o.jsx_fragment_factory() {
        if !c.set(OptionId::JsxFactory) {
            without(&mut c, OptionId::JsxFragmentFactory, OptionId::JsxFactory);
        }
        if !is_entity_name(f) {
            c.value(
                OptionId::JsxFragmentFactory,
                18035,
                format!("Invalid value for 'jsxFragmentFactory'. '{f}' is not a valid identifier or qualified-name."),
            );
        }
    }
    let automatic = match o.jsx() {
        Some(JsxEmit::ReactJsx) => Some("react-jsx"),
        Some(JsxEmit::ReactJsxDev) => Some("react-jsxdev"),
        _ => None,
    };
    if let Some(mode) = automatic {
        for id in [
            OptionId::JsxFactory,
            OptionId::ReactNamespace,
            OptionId::JsxFragmentFactory,
        ] {
            if c.set(id) {
                c.key(
                    id,
                    5089,
                    format!(
                        "Option '{}' cannot be specified when option 'jsx' is '{mode}'.",
                        id.name()
                    ),
                );
            }
        }
    }
    if c.set(OptionId::JsxImportSource) && o.jsx() == Some(JsxEmit::React) {
        c.key(
            OptionId::JsxImportSource,
            5089,
            "Option 'jsxImportSource' cannot be specified when option 'jsx' is 'react'.".into(),
        );
    }
    if c.set(OptionId::DeclarationDir) && !e.declaration() {
        c.key(
            OptionId::DeclarationDir,
            5069,
            "Option 'declarationDir' cannot be specified without specifying option 'declaration' or option 'composite'.".into(),
        );
    }
    if c.set(OptionId::MapRoot)
        && !c.truthy(OptionId::SourceMap)
        && !c.truthy(OptionId::DeclarationMap)
    {
        c.key(
            OptionId::MapRoot,
            5069,
            "Option 'mapRoot' cannot be specified without specifying option 'sourceMap' or option 'declarationMap'.".into(),
        );
    }
    if c.set(OptionId::SourceRoot)
        && !c.truthy(OptionId::SourceMap)
        && !c.truthy(OptionId::InlineSourceMap)
    {
        c.key(
            OptionId::SourceRoot,
            5051,
            "Option 'sourceRoot can only be used when either option '--inlineSourceMap' or option '--sourceMap' is provided.".into(),
        );
    }
    if c.truthy(OptionId::InlineSources)
        && !c.truthy(OptionId::SourceMap)
        && !c.truthy(OptionId::InlineSourceMap)
    {
        c.key(
            OptionId::InlineSources,
            5051,
            "Option 'inlineSources can only be used when either option '--inlineSourceMap' or option '--sourceMap' is provided.".into(),
        );
    }
    if c.truthy(OptionId::EmitDeclarationOnly) && !e.declaration() {
        c.key(
            OptionId::EmitDeclarationOnly,
            5069,
            "Option 'emitDeclarationOnly' cannot be specified without specifying option 'declaration' or option 'composite'.".into(),
        );
    }
    if c.truthy(OptionId::IsolatedDeclarations) && !e.declaration() {
        c.key(
            OptionId::IsolatedDeclarations,
            5069,
            "Option 'isolatedDeclarations' cannot be specified without specifying option 'declaration' or option 'composite'.".into(),
        );
    }
    if c.truthy(OptionId::DeclarationMap) && !e.declaration() {
        c.key(
            OptionId::DeclarationMap,
            5069,
            "Option 'declarationMap' cannot be specified without specifying option 'declaration' or option 'composite'.".into(),
        );
    }
    if e.allow_importing_ts_extensions()
        && !e.no_emit()
        && !c.truthy(OptionId::EmitDeclarationOnly)
        && !c.truthy(OptionId::RewriteRelativeImportExtensions)
    {
        c.value(
            OptionId::AllowImportingTsExtensions,
            5096,
            "Option 'allowImportingTsExtensions' can only be used when one of 'noEmit', 'emitDeclarationOnly', or 'rewriteRelativeImportExtensions' is set.".into(),
        );
    }
    if c.truthy(OptionId::ResolvePackageJsonExports)
        || c.truthy(OptionId::ResolvePackageJsonImports)
    {
        // Always valid in TS 7 (node10/classic are gone).
    }
    c.out
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

fn is_entity_name(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(json: &str) -> Vec<u32> {
        let mut fs = crate::fs::MemoryFs::new();
        fs.add_file("/p/tsconfig.json", json)
            .add_file("/p/a.ts", "");
        let cfg = crate::tsconfig::load_tsconfig(&fs, "/p/tsconfig.json");
        let mut v: Vec<u32> = verify_options(&cfg.options, None)
            .iter()
            .map(|d| d.code)
            .collect();
        v.sort();
        v
    }

    #[test]
    fn incremental_needs_a_config_or_build_info_path() {
        let json = |o: &str| format!(r#"{{"compilerOptions":{o}}}"#);
        assert!(codes(&json(r#"{"incremental":true}"#)).is_empty());
        let without_config = |o: &str| {
            let mut fs = crate::fs::MemoryFs::new();
            fs.add_file("/p/tsconfig.json", &json(o));
            let mut cfg = crate::tsconfig::load_tsconfig(&fs, "/p/tsconfig.json");
            cfg.options.config_file_path = None;
            verify_options(&cfg.options, None)
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>()
        };
        assert_eq!(without_config(r#"{"incremental":true}"#), [5074]);
        assert!(
            without_config(r#"{"incremental":true,"tsBuildInfoFile":"x.tsbuildinfo"}"#).is_empty()
        );
        assert!(without_config(r#"{"composite":true}"#).is_empty());
    }

    /// The `baseUrl` hint is relative to the leaf config, `./`-prefixed unless it starts
    /// with `../` (observed with tsc 7.0.2).
    #[test]
    fn base_url_hint_is_relative_to_the_leaf_config() {
        let hint = |base: &str, base_url: &str| {
            let mut fs = crate::fs::MemoryFs::new();
            fs.add_file(
                base,
                &format!(r#"{{"compilerOptions":{{"baseUrl":"{base_url}"}}}}"#),
            );
            if base != "/r/a/b/c/tsconfig.json" {
                fs.add_file(
                    "/r/a/b/c/tsconfig.json",
                    &format!(r#"{{"extends":"{base}","files":[]}}"#),
                );
            }
            let cfg = crate::tsconfig::load_tsconfig(&fs, "/r/a/b/c/tsconfig.json");
            let d = verify_options(&cfg.options, None);
            let d = d.iter().find(|d| d.code == 5102).expect("TS5102");
            d.related.join("|")
        };
        let leaf = "/r/a/b/c/tsconfig.json";
        for (base_url, star) in [
            ("..", "./../*"),
            ("../", "./../*"),
            ("./..", "./../*"),
            ("../..", "../../*"),
            ("../src", "../src/*"),
            (".", "./*"),
            ("./", "./*"),
            ("src", "./src/*"),
            ("src/", "./src/*"),
            ("/r", "../../../*"),
        ] {
            assert_eq!(
                hint(leaf, base_url),
                format!(r#"Use '"paths": {{"*": ["{star}"]}}' instead."#),
                "{base_url}"
            );
        }
        // Set in a base one level up: still relative to the leaf.
        assert_eq!(
            hint("/r/a/b/base.json", "src"),
            r#"Use '"paths": {"*": ["../src/*"]}' instead."#
        );
        assert_eq!(
            hint("/r/a/b/base.json", "c"),
            r#"Use '"paths": {"*": ["./*"]}' instead."#
        );
    }

    #[test]
    fn removed_options() {
        assert_eq!(codes(r#"{"compilerOptions":{"baseUrl":"."}}"#), [5102]);
        assert_eq!(codes(r#"{"compilerOptions":{"target":"es5"}}"#), [5108]);
        assert_eq!(
            codes(r#"{"compilerOptions":{"esModuleInterop":false}}"#),
            [5108]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"esModuleInterop":true}}"#),
            Vec::<u32>::new()
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"module":"amd"}}"#),
            [5095, 5108]
        );
    }

    #[test]
    fn combinations() {
        assert_eq!(
            codes(r#"{"compilerOptions":{"emitDecoratorMetadata":true}}"#),
            [5052]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"checkJs":true,"allowJs":false}}"#),
            [5052]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"checkJs":true}}"#),
            Vec::<u32>::new()
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"declarationDir":"d"}}"#),
            [5069]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"jsx":"react","jsxImportSource":"preact"}}"#),
            [5089]
        );
        assert_eq!(codes(r#"{"compilerOptions":{"jsxFactory":"1a"}}"#), [5067]);
        assert_eq!(
            codes(r#"{"compilerOptions":{"allowImportingTsExtensions":true}}"#),
            [5096]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"allowImportingTsExtensions":true,"noEmit":true}}"#),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn combinations_tsc7() {
        let none = Vec::<u32>::new();
        assert_eq!(
            codes(
                r#"{"compilerOptions":{"composite":true,"declaration":false,"incremental":false}}"#
            ),
            [6304, 6379]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"composite":false,"incremental":false}}"#),
            none
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"isolatedModules":true,"preserveConstEnums":false}}"#),
            [5091]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"preserveConstEnums":false}}"#),
            none
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"jsx":"react-jsxdev","jsxFactory":"h"}}"#),
            [5089]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"jsx":"react-native","jsxFactory":"h"}}"#),
            none
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"module":"node18","moduleResolution":"bundler"}}"#),
            [5095, 5109]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"module":"node18","moduleResolution":"nodenext"}}"#),
            none
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"inlineSources":true}}"#),
            [5051]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"inlineSources":true,"inlineSourceMap":true}}"#),
            none
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"strictPropertyInitialization":true,"strict":false}}"#),
            [5052]
        );
        assert_eq!(
            codes(r#"{"compilerOptions":{"strictPropertyInitialization":true}}"#),
            none
        );
    }

    #[test]
    fn identifiers() {
        assert!(is_entity_name("React.createElement"));
        assert!(!is_entity_name("x-y"));
        assert!(!is_identifier("a.b"));
    }
}
