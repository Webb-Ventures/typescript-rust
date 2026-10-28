//! The rule API.
//!
//! A rule is a [`RuleDef`] (static metadata plus a constructor that validates options) and the
//! [`Rule`] object it builds. Rules declare the node kinds they want; the engine walks each
//! file once and calls every interested rule. Rules hold no mutable state: per-file state lives
//! in locals of [`Rule::run_file`] or is derived from the parent map / semantic model.

use std::cell::OnceCell;
use std::fmt;

use ts_r_ast::{Ast, Node, NodeId, NodeKind, ParentMap};
use ts_r_span::{LineIndex, Span};

use crate::diagnostic::{Diagnostic, Fix, Severity, Suggestion};
use crate::env::LintEnv;
use crate::semantic::Semantic;
use crate::semantic::local::{CommentGlobal, LocalSemantic};
use crate::types::{FileKey, TypeInfo};

/// Which published presets include a rule (used for `extends`-less configs, oxlint
/// categories and docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    /// `js.configs.recommended` (ESLint 10).
    EslintRecommended,
    /// `tseslint.configs.recommended` (non type-checked part).
    TsRecommended,
    /// `tseslint.configs.strict` (non type-checked part).
    TsStrict,
    /// `tseslint.configs.eslintRecommended` overrides (`no-var`, `prefer-const`, ...).
    TsEslintRecommended,
    /// The type-checked part of `tseslint.configs.recommendedTypeChecked`.
    TsRecommendedTypeChecked,
    /// The type-checked part of `tseslint.configs.strictTypeChecked`.
    TsStrictTypeChecked,
    /// The type-checked part of `tseslint.configs.stylisticTypeChecked`.
    TsStylisticTypeChecked,
}

/// oxlint-style category, used to map `.oxlintrc.json` `categories`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Correctness,
    Suspicious,
    Pedantic,
    Style,
    Restriction,
}

impl Category {
    pub fn parse(s: &str) -> Option<Category> {
        Some(match s {
            "correctness" => Category::Correctness,
            "suspicious" => Category::Suspicious,
            "pedantic" => Category::Pedantic,
            "style" => Category::Style,
            "restriction" => Category::Restriction,
            _ => return None,
        })
    }
}

/// Static rule metadata.
#[derive(Debug)]
pub struct RuleMeta {
    /// The ESLint name, used as the rule id everywhere (`no-debugger`,
    /// `@typescript-eslint/no-explicit-any`).
    pub name: &'static str,
    /// One-line description (docs, SARIF).
    pub docs: &'static str,
    /// Provides `--fix` fixes.
    pub fixable: bool,
    /// Provides editor suggestions.
    pub has_suggestions: bool,
    /// Needs type information (see [`crate::types`]): runs only on files of a tsconfig
    /// project our checker matches (see [`crate::typed`]).
    pub requires_types: bool,
    pub presets: &'static [Preset],
    pub category: Category,
}

impl RuleMeta {
    pub fn docs_url(&self) -> String {
        if let Some(n) = self.name.strip_prefix("@typescript-eslint/") {
            return format!("https://typescript-eslint.io/rules/{n}");
        }
        if let Some(n) = self.name.strip_prefix("jsdoc/") {
            return format!(
                "https://github.com/gajus/eslint-plugin-jsdoc/blob/main/docs/rules/{n}.md"
            );
        }
        format!("https://eslint.org/docs/latest/rules/{}", self.name)
    }
}

/// Builds a rule from its options.
pub type CreateFn = fn(&[serde_json::Value]) -> Result<Box<dyn Rule>, String>;

/// A rule's static definition: metadata and a constructor that validates options.
pub struct RuleDef {
    pub meta: RuleMeta,
    /// Builds the rule from its options (everything after the severity in the config array).
    pub create: CreateFn,
}

impl fmt::Debug for RuleDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuleDef")
            .field("name", &self.meta.name)
            .finish()
    }
}

/// A configured rule instance.
pub trait Rule: Send + Sync + fmt::Debug {
    /// Node kinds [`Rule::enter`] is called for.
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    /// Node kinds [`Rule::exit`] is called for (after the node's children).
    fn exit_kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn enter(&self, _node: NodeId, _cx: &mut Ctx<'_, '_>) {}
    fn exit(&self, _node: NodeId, _cx: &mut Ctx<'_, '_>) {}
    /// Called once per file, after the traversal. For rules that work on the whole file
    /// (scope-based rules, comment rules) or need their own ordered walk.
    fn run_file(&self, _cx: &mut Ctx<'_, '_>) {}
    /// True when [`Rule::run_file`] does something (saves a virtual call per file).
    fn has_run_file(&self) -> bool {
        false
    }
}

/// Everything about the file being linted. Shared by every rule.
pub struct FileCtx<'a> {
    pub path: &'a str,
    pub src: &'a str,
    pub ast: &'a Ast,
    pub parents: &'a ParentMap,
    pub lines: &'a LineIndex,
    pub env: &'a LintEnv,
    pub types: &'a dyn TypeInfo,
    pub file_key: FileKey,
    pub comment_globals: &'a [CommentGlobal],
    /// Names from `/* exported */` comments.
    pub exported: &'a [String],
    semantic: OnceCell<LocalSemantic<'a>>,
}

impl fmt::Debug for FileCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileCtx").field("path", &self.path).finish()
    }
}

impl<'a> FileCtx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: &'a str,
        src: &'a str,
        ast: &'a Ast,
        parents: &'a ParentMap,
        lines: &'a LineIndex,
        env: &'a LintEnv,
        types: &'a dyn TypeInfo,
        file_key: FileKey,
        comment_globals: &'a [CommentGlobal],
        exported: &'a [String],
    ) -> FileCtx<'a> {
        FileCtx {
            path,
            src,
            ast,
            parents,
            lines,
            env,
            types,
            file_key,
            comment_globals,
            exported,
            semantic: OnceCell::new(),
        }
    }

    /// The scope model, built on first use.
    pub fn semantic(&self) -> &dyn Semantic {
        self.semantic.get_or_init(|| {
            LocalSemantic::build(self.ast, self.parents, self.env, self.comment_globals)
        })
    }
}

/// What a rule sees while running: the file plus a sink for diagnostics.
pub struct Ctx<'f, 'a> {
    pub file: &'f FileCtx<'a>,
    pub(crate) rule: &'static str,
    pub(crate) severity: Severity,
    pub(crate) out: &'f mut Vec<Diagnostic>,
}

impl fmt::Debug for Ctx<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctx").field("rule", &self.rule).finish()
    }
}

/// Builder returned by [`Ctx::report`].
#[derive(Debug)]
pub struct Report<'d>(&'d mut Diagnostic);

impl Report<'_> {
    pub fn id(self, message_id: &'static str) -> Self {
        self.0.message_id = Some(message_id);
        self
    }
    pub fn fix(self, fix: Fix) -> Self {
        self.0.fix = Some(fix);
        self
    }
    pub fn fix_opt(self, fix: Option<Fix>) -> Self {
        if let Some(f) = fix {
            self.0.fix = Some(f);
        }
        self
    }
    pub fn suggest(self, message_id: &'static str, desc: impl Into<String>, fix: Fix) -> Self {
        self.0.suggestions.push(Suggestion {
            message_id: Some(message_id),
            desc: desc.into(),
            fix,
        });
        self
    }
}

impl<'f, 'a> Ctx<'f, 'a> {
    #[inline]
    pub fn ast(&self) -> &'a Ast {
        self.file.ast
    }
    #[inline]
    pub fn src(&self) -> &'a str {
        self.file.src
    }
    #[inline]
    pub fn node(&self, id: NodeId) -> &'a Node {
        self.file.ast.node(id)
    }
    #[inline]
    pub fn span(&self, id: NodeId) -> Span {
        self.file.ast.span(id)
    }
    #[inline]
    pub fn text(&self, id: NodeId) -> &'a str {
        self.file.ast.span(id).text(self.file.src)
    }
    #[inline]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.file.parents.parent(id)
    }
    pub fn semantic(&self) -> &'f dyn Semantic {
        self.file.semantic()
    }
    /// True when `ident` is an identifier named `name` that refers to the global of that name
    /// (built in, configured, or declared by a `/* global */` comment), not a local binding.
    pub fn is_global_ref(&self, ident: NodeId, name: &str) -> bool {
        let ast = self.file.ast;
        let ident = crate::ast_util::skip_parens(ast, ident);
        if crate::ast_util::ident_name(ast, ident) != Some(name) {
            return false;
        }
        let sem = self.semantic();
        match sem.reference_at(ident).map(|r| sem.reference(r).resolution) {
            Some(crate::semantic::Resolution::Global) => true,
            Some(crate::semantic::Resolution::Symbol(s)) => sem
                .symbol(s)
                .flags
                .contains(crate::semantic::SymbolFlags::COMMENT_GLOBAL),
            _ => false,
        }
    }

    /// True when `name` is declared locally (not as a `/* global */`) where `at` is.
    pub fn is_shadowed(&self, at: NodeId, name: &str) -> bool {
        let sem = self.semantic();
        let scope = sem.enclosing_scope(at);
        sem.lookup(scope, name).is_some_and(|s| {
            !sem.symbol(s)
                .flags
                .contains(crate::semantic::SymbolFlags::COMMENT_GLOBAL)
        })
    }

    /// True when the global `name` exists (built in or configured) and is not shadowed at
    /// `at`.
    pub fn global_available(&self, at: NodeId, name: &str) -> bool {
        let sem = self.semantic();
        let scope = sem.enclosing_scope(at);
        match sem.lookup(scope, name) {
            Some(s) => sem
                .symbol(s)
                .flags
                .contains(crate::semantic::SymbolFlags::COMMENT_GLOBAL),
            None => sem.is_global(name),
        }
    }

    pub fn env(&self) -> &'a LintEnv {
        self.file.env
    }
    /// Type information for the file (a real checker for type-aware rules).
    #[inline]
    pub fn types(&self) -> &'a dyn TypeInfo {
        self.file.types
    }
    pub fn rule_name(&self) -> &'static str {
        self.rule
    }

    /// Reports a problem at `span`.
    pub fn report(&mut self, span: Span, message: impl Into<String>) -> Report<'_> {
        self.out.push(Diagnostic {
            rule: Some(self.rule),
            severity: self.severity,
            span,
            message: message.into(),
            message_id: None,
            fix: None,
            suggestions: Vec::new(),
            fatal: false,
        });
        let last = self.out.len() - 1;
        Report(&mut self.out[last])
    }

    /// Reports a problem on a node (parentheses excluded, like ESTree ranges).
    pub fn report_node(&mut self, id: NodeId, message: impl Into<String>) -> Report<'_> {
        let span = self.span(id);
        self.report(span, message)
    }
}
