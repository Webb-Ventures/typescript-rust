//! ts-r-lint engine. See AGENTS.md and docs/design/0006-lint.md.

pub mod ast_util;
pub mod cfg;
pub mod config;
pub mod diagnostic;
pub mod directives;
pub mod engine;
pub mod env;
pub mod fix;
pub mod globals;
pub mod jsdoc;
pub mod lib_globals;
#[rustfmt::skip]
pub mod lib_globals_generated;
pub mod output;
pub mod project;
pub mod regex;
pub mod registry;
pub mod rule;
pub mod rules;
pub mod semantic;
pub mod type_util;
pub mod typed;
pub mod types;

pub use diagnostic::{Diagnostic, Edit, Fix, Severity, Suggestion};
pub use engine::{FileConfig, FileResult, lint_and_fix, lint_source};
pub use registry::Registry;
pub use rule::{Ctx, Rule, RuleDef, RuleMeta};
pub use types::{NoTypeInfo, TypeInfo};
