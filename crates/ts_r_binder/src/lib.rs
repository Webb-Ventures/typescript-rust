//! Binder: scopes, symbols, declaration merging, the control-flow graph, bind-time
//! diagnostics, and program-level linking (global merging, module exports).
//!
//! The design is `docs/design/0003-binder.md`. In short: [`bind`] turns one parsed file into a
//! [`BoundFile`] of flat arrays (`u32` ids, no pointers, deterministic order) without looking at
//! any other file; [`program::Program`] links bound files through `ts_r_resolver`.
//!
//! ```
//! use ts_r_ast::SourceKind;
//! let src = "let a = 1; let a = 2; export {};";
//! let parsed = ts_r_parser::parse(src, SourceKind::Ts);
//! let bound = ts_r_binder::bind(&parsed.ast, src, &ts_r_binder::BindOptions::default());
//! assert_eq!(bound.diagnostics.len(), 2); // TS2451 on both declarations
//! assert!(bound.is_module);
//! ```

mod binder;
mod dump;
pub mod flow;
pub mod messages;
pub mod names;
pub mod program;
mod resolve;
pub mod symbols;

use ts_r_ast::{Ast, Node, NodeId, Span};
use ts_r_diagnostics::Diagnostic;

pub use flow::{FlowGraph, FlowId, FlowKind, FlowNode, FunctionFlow};
pub use names::{Name, Names, js_number_to_string};
pub use resolve::is_body_local;
pub use symbols::{Symbol, SymbolFacts, SymbolFlags, SymbolId, TableId, TableRange};

/// Bump when the layout or meaning of anything in [`BoundFile`] changes (cache keys).
pub const BINDER_VERSION: u32 = 1;

/// Options that change binding. Everything else about a file comes from its AST.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BindOptions {
    /// Treat the file as a module even without `import`/`export` (`moduleDetection: force`,
    /// `.mts`/`.cts`, or a `.ts`/`.js` file under a `"type": "module"` package with node16+).
    pub force_module: bool,
    /// `alwaysStrict`; `None` means the TS7 default (true). With `Some(false)`, scripts are
    /// strict only under a `"use strict"` prologue (modules and classes always are).
    pub always_strict: Option<bool>,
    /// `allowUnreachableCode`; only `Some(false)` produces TS7027 diagnostics.
    pub allow_unreachable_code: Option<bool>,
    /// `allowUnusedLabels`; only `Some(false)` produces TS7028 diagnostics.
    pub allow_unused_labels: Option<bool>,
    /// `preserveConstEnums` (or `isolatedModules`): const enums emit code, so they and
    /// namespaces holding only const enums count as reachable-code statements.
    pub preserve_const_enums: bool,
    /// The file's imports resolve in ECMAScript-module mode (an `es20xx`/`esnext`/`preserve`
    /// module, or an ESM-format file under `node16`+). Not used by binding; recorded for the
    /// checker's module diagnostics.
    pub esm_format: bool,
    /// The file's own format (its extension, else the nearest `package.json` `type`) is
    /// ECMAScript module, under any `module` setting. Not used by binding; the checker uses
    /// it for synthetic default imports.
    pub package_esm: bool,
    /// Under `node16`/`node18`, the nearest `package.json` deciding a `.ts`/`.tsx`/`.js`/`.jsx`
    /// file's format: how many directories above the file's own it is, and whether it has a
    /// `type` field. Not used by binding; the checker's TS1479 hints use it.
    pub package_scope: Option<(u16, bool)>,
}

/// Index into [`BoundFile::scopes`]. The file scope is `ScopeId(0)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct ScopeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScopeKind {
    File,
    /// Namespace body, ambient module body or `declare global` block.
    Module,
    /// Function-like: parameters plus the top-level declarations of the body.
    Function,
    /// Class body: class expression name and nothing else (members are not in scope).
    Class,
    Block,
    Catch,
    /// `for`/`for-in`/`for-of` head with `let`/`const`.
    For,
    Switch,
    TypeParams,
    StaticBlock,
    /// Enum body: members are visible by bare name in initializers.
    Enum,
    /// Name of a named function or class expression.
    ExprName,
}

#[derive(Clone, Copy, Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub node: NodeId,
    pub parent: Option<ScopeId>,
    pub locals: TableId,
    /// Namespace/module/enum symbol whose exports are also in scope (`Module`, `File`, `Enum`).
    pub symbol: Option<SymbolId>,
}

/// Meaning bits for references.
pub mod meaning {
    pub const VALUE: u8 = 1;
    pub const TYPE: u8 = 2;
    pub const NAMESPACE: u8 = 4;
    pub const ALL: u8 = 7;
}

/// An identifier reference that is not declared in this file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeRef {
    pub node: NodeId,
    pub name: Name,
    pub meaning: u8,
}

/// Name references found in the file.
#[derive(Clone, Debug, Default)]
pub struct Refs {
    /// Identifier → local symbol, sorted by node.
    pub resolved: Vec<(NodeId, SymbolId)>,
    /// References to names this file does not declare (globals or errors), in source order.
    pub free: Vec<FreeRef>,
}

/// How a module specifier is used.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RequestKind {
    Import,
    ExportFrom,
    ImportEquals,
    DynamicImport,
    ImportType,
    Augmentation,
}

/// A module specifier in the file, in source order.
#[derive(Clone, Copy, Debug)]
pub struct ModuleRequest {
    /// Cooked specifier text.
    pub specifier: ts_r_ast::Atom,
    /// The string literal node.
    pub node: NodeId,
    pub kind: RequestKind,
    pub type_only: bool,
    /// Inside `declare module "x" { }` in a script file: a relative specifier there is never
    /// resolved (TS2439).
    pub in_ambient_module: bool,
    /// An import or export statement inside `declare module "x" { }` or `declare global { }`
    /// in a module file (an augmentation): a relative specifier there resolves only when the
    /// file also imports it outside augmentations (observed from tsc; TS2307 otherwise).
    /// `import("x")` types are not affected.
    pub in_augmentation: bool,
    /// A valid `resolution-mode` attribute (`"import"`/`"require"`) of an `import type`,
    /// `export type ... from` or `import()` type: the mode the specifier resolves in,
    /// instead of the file's. Other imports ignore the attribute.
    pub resolution_mode: Option<ts_r_resolver::ResolutionMode>,
}

/// What an alias imports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportName {
    Default,
    Named(Name),
    /// `import * as ns`, `export * as ns`.
    Namespace,
    /// `import x = require("m")`.
    ExportEquals,
}

/// Target of an alias symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AliasTarget {
    /// From another module: index into [`BoundFile::module_requests`].
    Module { request: u32, import: ImportName },
    /// A local entity name or expression: `export { a }`, `export default a`, `export = a`,
    /// `import x = A.B`. The node is the `Ident`/`QualifiedName`/`Member` expression.
    Entity(NodeId),
}

#[derive(Clone, Copy, Debug)]
pub struct AliasInfo {
    pub symbol: SymbolId,
    pub decl: NodeId,
    pub target: AliasTarget,
}

/// `export * from "m"`.
#[derive(Clone, Copy, Debug)]
pub struct ExportStar {
    pub request: u32,
    pub node: NodeId,
    /// The module or namespace symbol that re-exports (the file symbol or an ambient module).
    pub container: SymbolId,
}

/// The result of binding one file. Plain data; see the design record.
#[derive(Clone, Debug, Default)]
pub struct BoundFile {
    /// The file has top-level `import`/`export` (or `force_module`).
    pub is_module: bool,
    /// A JavaScript CommonJS module: a module without ECMAScript module syntax (its exports
    /// are `exports.x = v` / `module.exports = v` assignments).
    pub commonjs: bool,
    /// The file is a declaration file.
    pub is_declaration: bool,
    /// Symbol 0: the file itself (`MODULE_FILE`); its `exports` are the module's exports.
    pub file_symbol: SymbolId,
    pub symbols: Vec<Symbol>,
    pub decl_nodes: Vec<NodeId>,
    /// `[0]` is the empty table.
    pub tables: Vec<TableRange>,
    /// Per table, sorted by name.
    pub table_entries: Vec<(Name, SymbolId)>,
    pub scopes: Vec<Scope>,
    /// Scope-creating nodes, sorted by node.
    pub scope_of_node: Vec<(NodeId, ScopeId)>,
    /// Declaration node → symbol, sorted by node.
    pub symbol_of_decl: Vec<(NodeId, SymbolId)>,
    /// Class and interface declaration → its member table, sorted by node. Each class
    /// declaration has its own table; the interface declarations of a symbol share one.
    /// `Symbol::members` is the first declaration's table.
    pub decl_members: Vec<(NodeId, TableId)>,
    pub flow: FlowGraph,
    pub refs: Refs,
    /// Sorted by symbol.
    pub aliases: Vec<AliasInfo>,
    pub module_requests: Vec<ModuleRequest>,
    pub export_stars: Vec<ExportStar>,
    /// `declare module "x"` in a script or declaration file: name → module symbol.
    pub ambient_modules: TableId,
    /// `declare module "x"` in a module: augmentations, name → module symbol.
    pub module_augmentations: TableId,
    /// Declarations inside `declare global { }`.
    pub global_augmentations: TableId,
    /// `export as namespace X`.
    pub umd_name: Option<(Name, NodeId)>,
    pub names: Names,
    /// Bind-time diagnostics, sorted by position.
    pub diagnostics: Vec<Diagnostic>,
    /// Runs of unreachable statements (reported only with `allowUnreachableCode: false`).
    pub unreachable: Vec<Span>,
    /// Label identifiers that are never targeted.
    pub unused_labels: Vec<NodeId>,
    /// Late-bound expando assignments `f[k] = v` (`k` a name whose type the checker turns
    /// into the property name), as (target function symbol, assignment target), sorted.
    pub late_expandos: Vec<(SymbolId, NodeId)>,
}

impl BoundFile {
    #[inline]
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.index()]
    }

    /// Declarations of a symbol in binding order.
    pub fn decls(&self, id: SymbolId) -> &[NodeId] {
        let s = &self.symbols[id.index()];
        &self.decl_nodes[s.decl_start as usize..(s.decl_start + s.decl_len) as usize]
    }

    /// Entries of a table, sorted by name.
    pub fn table(&self, table: TableId) -> &[(Name, SymbolId)] {
        match self.tables.get(table.0 as usize) {
            Some(r) => &self.table_entries[r.start as usize..(r.start + r.len) as usize],
            None => &[],
        }
    }

    /// Looks a name up in a table.
    pub fn lookup(&self, table: TableId, name: Name) -> Option<SymbolId> {
        let entries = self.table(table);
        entries
            .binary_search_by_key(&name, |&(n, _)| n)
            .ok()
            .map(|i| entries[i].1)
    }

    /// Looks a string up in a table.
    pub fn lookup_str(&self, ast: &Ast, table: TableId, name: &str) -> Option<SymbolId> {
        let n = self.names.find(ast, name)?;
        self.lookup(table, n)
    }

    /// The module's exports (empty for scripts).
    pub fn exports(&self) -> &[(Name, SymbolId)] {
        self.table(self.symbols[self.file_symbol.index()].exports)
    }

    /// Top-level declarations of a script (its global contribution); empty for modules.
    pub fn script_globals(&self) -> &[(Name, SymbolId)] {
        if self.is_module {
            &[]
        } else {
            self.table(self.scopes[0].locals)
        }
    }

    pub fn name_text<'a>(&'a self, ast: &'a Ast, name: Name) -> &'a str {
        self.names.text(ast, name)
    }

    /// The symbol declared by a declaration node.
    pub fn symbol_of_decl(&self, node: NodeId) -> Option<SymbolId> {
        self.symbol_of_decl
            .binary_search_by_key(&node, |&(n, _)| n)
            .ok()
            .map(|i| self.symbol_of_decl[i].1)
    }

    /// Member tables of every declaration of a class/interface symbol, in binding order.
    pub fn member_tables(&self, id: SymbolId) -> Vec<TableId> {
        let mut v: Vec<TableId> = self
            .decls(id)
            .iter()
            .filter_map(|&d| {
                self.decl_members
                    .binary_search_by_key(&d, |&(n, _)| n)
                    .ok()
                    .map(|i| self.decl_members[i].1)
            })
            .collect();
        v.dedup();
        if v.is_empty() && !self.symbol(id).members.is_none() {
            v.push(self.symbol(id).members);
        }
        v
    }

    /// The local symbol an identifier reference resolved to.
    pub fn resolved(&self, node: NodeId) -> Option<SymbolId> {
        self.refs
            .resolved
            .binary_search_by_key(&node, |&(n, _)| n)
            .ok()
            .map(|i| self.refs.resolved[i].1)
    }

    /// The scope created by a node (function, block, class, ...).
    pub fn scope_of(&self, node: NodeId) -> Option<ScopeId> {
        self.scope_of_node
            .binary_search_by_key(&node, |&(n, _)| n)
            .ok()
            .map(|i| self.scope_of_node[i].1)
    }

    pub fn alias(&self, symbol: SymbolId) -> Option<&AliasInfo> {
        let i = self.aliases.partition_point(|a| a.symbol < symbol);
        self.aliases.get(i).filter(|a| a.symbol == symbol)
    }

    /// All alias declarations of a symbol (several only for merged/duplicate aliases).
    pub fn aliases_of(&self, symbol: SymbolId) -> &[AliasInfo] {
        let lo = self.aliases.partition_point(|a| a.symbol < symbol);
        let hi = self.aliases.partition_point(|a| a.symbol <= symbol);
        &self.aliases[lo..hi]
    }

    /// Text dump of symbols, scopes and diagnostics for snapshot tests.
    pub fn dump(&self, ast: &Ast, src: &str) -> String {
        dump::dump(self, ast, src)
    }

    /// Text dump of the control-flow graph.
    pub fn dump_flow(&self, ast: &Ast, src: &str) -> String {
        dump::dump_flow(self, ast, src)
    }
}

/// Binds one parsed file. `src` is the text the AST was parsed from (used for diagnostic
/// arguments that quote declaration names).
pub fn bind(ast: &Ast, src: &str, opts: &BindOptions) -> BoundFile {
    binder::Binder::new(ast, src, opts).run()
}

/// The node naming a declaration (the node diagnostics about the declaration point at), if
/// it has one.
pub fn decl_name_node(ast: &Ast, decl: NodeId) -> Option<NodeId> {
    Some(match *ast.node(decl) {
        Node::Ident { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::PrivateName { .. }
        | Node::ComputedKey { .. } => decl,
        Node::Param { pattern, .. } => pattern,
        Node::FunctionDecl { name, .. }
        | Node::FunctionExpr { name, .. }
        | Node::ClassDecl { name, .. }
        | Node::ClassExpr { name, .. } => name?,
        Node::Interface { name, .. }
        | Node::TypeAlias { name, .. }
        | Node::Enum { name, .. }
        | Node::TypeParam { name, .. }
        | Node::ImportEquals { name, .. }
        | Node::EnumMember { name, .. }
        | Node::NamespaceExport { name } => name,
        Node::Module { name, .. } => match *ast.node(name) {
            Node::QualifiedName { right, .. } => right,
            _ => name,
        },
        Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. }
        | Node::Property { key, .. } => key,
        Node::ShorthandProp { name, .. } => name,
        Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => {
            let e = {
                let mut e = expr;
                while let Node::Paren { expr } = *ast.node(e) {
                    e = expr;
                }
                e
            };
            if matches!(ast.node(e), Node::Ident { .. }) {
                e
            } else {
                return None;
            }
        }
        Node::ExportSpecifier {
            local, exported, ..
        } => exported.unwrap_or(local),
        Node::ImportDefaultSpecifier { local }
        | Node::ImportNamespaceSpecifier { local }
        | Node::ImportSpecifier { local, .. } => local,
        Node::ExportAll { exported, .. } => exported?,
        _ => return None,
    })
}
