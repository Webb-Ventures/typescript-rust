//! The cross-file facts tsc's emit takes from the checker (design record 0012, "JS emit").
//!
//! The transform decides most of what it erases from the file alone (`scope.rs`): an import
//! nobody references as a value goes, a local interface in `export { I }` goes. What it
//! cannot see is what an alias stands for in another file: `export { T } from "./a"`, or
//! `import { T } from "./a"; export { T };`, where `T` is only a type in `a.ts`. A program
//! emitter (`ts_r_emit`) passes an [`EmitResolver`] backed by the checker for those; the
//! one-file tools (ts-r-run, the dev server, the bundler) use [`SyntacticResolver`], which
//! keeps today's behaviour.
//!
//! An answer of [`Answer::Unknown`] means the checker cannot tell: the transform records the
//! construct in [`crate::TransformResult::unknown`] and the emitter must not write the file
//! (TSR2002), because a wrongly kept or elided import fails at runtime.

use ts_r_ast::NodeId;
use ts_r_diagnostics::FileId;

/// A checker answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answer {
    Yes,
    No,
    Unknown,
}

/// A constant enum member value (cross-file const enum inlining).
#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Number(f64),
    String(String),
}

/// How `emitDecoratorMetadata` serializes a type reference (`design:type` and friends).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum MetadataType {
    /// The transform's own syntactic serialization (no checker).
    #[default]
    Syntactic,
    /// The type name names a value with construct signatures (a class): the name itself,
    /// written as an expression (`Foo`, `ns.Foo`, `foo_1.Foo` under CommonJS).
    Value,
    /// The name cannot be resolved to a type: a `typeof X === "function" ? X : Object`
    /// guard that reads the name at run time.
    Unknown,
    /// `void 0`: void, `undefined`, `null` and `never`.
    Void,
    /// A global constructor: `Object`, `Function`, `Boolean`, `Number`, `BigInt`,
    /// `String`, `Array`, `Symbol` or `Promise`.
    Global(&'static str),
    /// Like [`MetadataType::Global`], for a name reached through a type-only import or
    /// export: the name is not a run-time reference, so it keeps no import alive.
    TypeOnly(&'static str),
}

/// Cross-file facts for emit. Node ids are those of the file's original tree: the emitter
/// transforms a copy, and the transform asks before it rewrites a node.
pub trait EmitResolver: Sync {
    /// Whether the alias declared by `alias` (an import or export specifier, a default or
    /// namespace import) stands for something with a value meaning, so that the statement
    /// must stay in the JavaScript output. Unresolvable targets are values (tsc keeps an
    /// import of a missing module).
    fn is_value_alias(&self, file: FileId, alias: NodeId) -> Answer {
        let _ = (file, alias);
        Answer::Yes
    }

    /// The value of a const enum member accessed by `access`, including local enums whose
    /// initializers depend on another file.
    fn const_enum_value(&self, file: FileId, access: NodeId) -> Option<ConstValue> {
        let _ = (file, access);
        None
    }

    /// How decorator metadata serializes the type reference `ty` (a `TypeRef` node).
    fn metadata_type(&self, file: FileId, ty: NodeId) -> MetadataType {
        let _ = (file, ty);
        MetadataType::Syntactic
    }

    /// Whether `import x = N.y` (`decl`) must stay: it is referenced as a value and `N.y` is
    /// a value.
    fn is_referenced_alias(&self, file: FileId, decl: NodeId) -> Answer {
        let _ = (file, decl);
        Answer::Yes
    }

    /// Whether the automatic JSX runtime `file` imports implicitly (`react/jsx-runtime`)
    /// resolves to a module. When it does, JSX no longer keeps an import of the classic
    /// factory's root (`import * as React from "react"`) alive; when it does not (or the
    /// file is transpiled alone), it does, as for the classic runtime.
    fn jsx_runtime_found(&self, file: FileId) -> bool {
        let _ = file;
        false
    }
}

/// The file-local behaviour: every alias the transform keeps stays.
#[derive(Clone, Copy, Debug, Default)]
pub struct SyntacticResolver;

impl EmitResolver for SyntacticResolver {}
