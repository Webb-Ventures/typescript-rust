//! The ts-r type checker. Design: `docs/design/0004-types-and-checker.md`.

#[allow(dead_code, clippy::all, missing_docs)]
pub mod diagnostics_generated;

pub mod abstract_access;
pub mod abstracts;
pub mod assign_targets;
pub mod call;
pub mod check;
pub mod checker;
pub mod circular_annotations;
pub mod classes;
pub mod conditional;
pub mod decl_names;
pub mod declared;
pub mod decls;
pub mod decorator_calls;
pub mod decorators;
pub mod diag;
pub mod driver;
pub mod emit;
pub mod emit_helpers;
pub mod export_locals;
pub mod export_merge;
pub mod export_redeclare;
pub mod expr;
pub mod flow;
pub mod func;
pub mod heritage;
pub mod implicit_members;
pub mod import_conflicts;
pub mod index_keys;
pub mod indexes;
pub mod infer;
pub mod init_order;
pub mod instantiate;
pub mod iterable;
pub mod jsdoc;
pub mod jsx;
pub mod jsx_check;
pub mod jsx_runtime;
pub mod layers;
pub mod let_names;
pub(crate) mod lib_hint;
pub mod mapped;
pub mod members;
pub mod merge_order;
pub mod modifiers;
pub mod namespace_names;
pub mod nodes;
pub mod object_dups;
pub mod overloads;
pub mod overrides;
pub mod param_refs;
pub mod print;
pub mod program;
pub mod query_sound;
pub mod redeclare;
pub(crate) mod references;
pub mod relate;
pub mod shapes;
pub mod static_refs;
pub mod super_calls;
pub mod symbols;
pub mod symtype;
pub mod template;
pub mod type_param_merge;
pub mod typenode;
pub mod uncalled;
pub mod unions;
pub mod unused;
pub mod variance;
pub mod yield_any;

pub use checker::{CheckOptions, Checker, Ck, LayerError, Loc};
pub use driver::{
    CheckResult, CheckRun, check_files, check_program, check_program_through_syntax_errors,
    check_program_with, should_check, sort_and_dedup,
};
pub use layers::Layer;
pub use program::{
    LibStore, ModeResolutions, ProgramFiles, SourceCache, SourceFile, load_program,
    load_program_cached, load_program_with_references,
};
pub use references::reference_diagnostics;
