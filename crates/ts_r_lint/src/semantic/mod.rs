//! Scopes, symbols and references for rules.
//!
//! Rules see scope information only through the [`Semantic`] trait and the plain data types
//! below. The implementation in [`local`] is a small, lint-only scope analysis written for M4
//! because the binder (`ts_r_binder`, M5) is not available yet. **It is temporary**: once the
//! binder lands, an adapter implements [`Semantic`] over the binder's tables and `local` is
//! deleted. Keep rule code independent of `local`'s internals.
//!
//! The model follows what ESLint's scope managers expose (eslint-scope and typescript-eslint's
//! scope manager), because rule behaviour is defined in those terms:
//! - one [`Symbol`] per name per scope; redeclarations and TS declaration merging add more
//!   [`Decl`]s to the same symbol;
//! - every identifier that refers to a binding is a [`Reference`] with read/write/type flags;
//! - references that resolve to nothing in the file are either configured globals or
//!   unresolved ("through" references in ESLint terms).

pub mod local;

use ts_r_ast::{Atom, NodeId};

/// Index into [`Semantic::scopes`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ScopeId(pub u32);

/// Index into [`Semantic::symbols`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

/// Index into [`Semantic::references`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RefId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScopeKind {
    Global,
    Module,
    /// Function body and parameters (functions, methods, arrows, accessors).
    Function,
    /// Holds the name of a named function expression.
    FunctionExprName,
    /// Holds the inner name of a named class expression.
    ClassExprName,
    Class,
    /// A class field initializer or a static block.
    ClassFieldInit,
    StaticBlock,
    Block,
    Catch,
    For,
    Switch,
    With,
    /// `namespace X {}`, `declare module "x" {}`, `declare global {}`.
    TsModule,
    TsEnum,
    /// Type parameters and other type-level bindings (interfaces, type aliases, function
    /// types, conditional and mapped types).
    Type,
}

impl ScopeKind {
    /// Scopes that receive `var` declarations.
    pub fn is_var_scope(self) -> bool {
        matches!(
            self,
            ScopeKind::Global
                | ScopeKind::Module
                | ScopeKind::Function
                | ScopeKind::StaticBlock
                | ScopeKind::ClassFieldInit
                | ScopeKind::TsModule
        )
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    /// The node that creates the scope (`Program` for the global and module scopes).
    pub node: NodeId,
    pub parent: Option<ScopeId>,
    /// Symbols declared directly in this scope, in declaration order.
    pub symbols: Vec<SymbolId>,
    /// Inside a `with` statement (lookups are dynamic).
    pub dynamic: bool,
}

macro_rules! flags32 {
    ($(#[$m:meta])* pub struct $name:ident { $( $(#[$fm:meta])* const $flag:ident = $val:expr; )* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
        pub struct $name(pub u32);
        impl $name {
            $( $(#[$fm])* pub const $flag: $name = $name($val); )*
            pub const NONE: $name = $name(0);
            #[inline] pub const fn contains(self, o: $name) -> bool { self.0 & o.0 == o.0 }
            #[inline] pub const fn intersects(self, o: $name) -> bool { self.0 & o.0 != 0 }
            #[inline] pub const fn union(self, o: $name) -> $name { $name(self.0 | o.0) }
            #[inline] pub fn insert(&mut self, o: $name) { self.0 |= o.0 }
            #[inline] pub fn remove(&mut self, o: $name) { self.0 &= !o.0 }
            #[inline] pub const fn is_empty(self) -> bool { self.0 == 0 }
        }
        impl std::ops::BitOr for $name {
            type Output = $name;
            fn bitor(self, o: $name) -> $name { $name(self.0 | o.0) }
        }
    };
}

flags32! {
    /// What kinds of declarations a symbol has (merged over all its declarations).
    pub struct SymbolFlags {
        const VAR = 1 << 0;
        const LET = 1 << 1;
        const CONST = 1 << 2;
        /// `using` / `await using`.
        const USING = 1 << 3;
        const FUNCTION = 1 << 4;
        const CLASS = 1 << 5;
        const PARAM = 1 << 6;
        const CATCH_PARAM = 1 << 7;
        const IMPORT = 1 << 8;
        /// `import type`, `import { type X }`.
        const TYPE_IMPORT = 1 << 9;
        const TYPE_ALIAS = 1 << 10;
        const INTERFACE = 1 << 11;
        const ENUM = 1 << 12;
        const NAMESPACE = 1 << 13;
        const TYPE_PARAM = 1 << 14;
        const ENUM_MEMBER = 1 << 15;
        /// The implicit `arguments` of a non-arrow function.
        const ARGUMENTS = 1 << 16;
        /// Declared by a `/* global x */` comment.
        const COMMENT_GLOBAL = 1 << 17;
        /// The name of a named function expression (inside its own scope).
        const FN_EXPR_NAME = 1 << 18;
        /// The name of a named class expression (inside its own scope).
        const CLASS_EXPR_NAME = 1 << 19;
        /// A `declare` / `.d.ts` declaration.
        const AMBIENT = 1 << 20;
        /// Exported from the module (`export const`, `export default function`, ...).
        const EXPORTED = 1 << 21;
        /// A parameter of a function without a body (overload, `declare`, abstract method,
        /// function type): never "unused".
        const SIGNATURE_PARAM = 1 << 22;
        /// A TS parameter property (`constructor(private x)`).
        const PARAM_PROPERTY = 1 << 23;
        /// `import x = require()` / `import x = A.B`.
        const IMPORT_EQUALS = 1 << 24;
        /// Rest sibling in an object pattern exists (`{ a, ...rest }`).
        const HAS_REST_SIBLING = 1 << 25;
        /// Declared inside an array destructuring pattern.
        const ARRAY_DESTRUCTURE = 1 << 26;
        /// Declared in `declare global {}` or another ambient module augmentation.
        const GLOBAL_AUGMENT = 1 << 27;
        /// `infer X`.
        const INFER = 1 << 28;
    }
}

impl SymbolFlags {
    /// Declarations that create a value binding.
    pub const VALUE: SymbolFlags = SymbolFlags(
        Self::VAR.0
            | Self::LET.0
            | Self::CONST.0
            | Self::USING.0
            | Self::FUNCTION.0
            | Self::CLASS.0
            | Self::PARAM.0
            | Self::CATCH_PARAM.0
            | Self::IMPORT.0
            | Self::ENUM.0
            | Self::NAMESPACE.0
            | Self::ENUM_MEMBER.0
            | Self::ARGUMENTS.0
            | Self::COMMENT_GLOBAL.0
            | Self::FN_EXPR_NAME.0
            | Self::CLASS_EXPR_NAME.0
            | Self::IMPORT_EQUALS.0,
    );
    /// Declarations that create a type binding.
    pub const TYPE: SymbolFlags = SymbolFlags(
        Self::CLASS.0
            | Self::INTERFACE.0
            | Self::TYPE_ALIAS.0
            | Self::ENUM.0
            | Self::NAMESPACE.0
            | Self::TYPE_PARAM.0
            | Self::IMPORT.0
            | Self::CLASS_EXPR_NAME.0
            | Self::IMPORT_EQUALS.0,
    );
}

/// The syntactic form of one declaration.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DeclKind {
    Var,
    Let,
    Const,
    Using,
    Function,
    Class,
    Param,
    CatchParam,
    ImportDefault,
    ImportNamespace,
    ImportNamed,
    ImportEquals,
    TypeAlias,
    Interface,
    Enum,
    EnumMember,
    Namespace,
    TypeParam,
    Arguments,
    CommentGlobal,
    FunctionExprName,
    ClassExprName,
}

/// One declaration of a symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decl {
    pub kind: DeclKind,
    /// The identifier that names the binding (the comment start for `/* global */`).
    pub name: NodeId,
    /// The declaring node: `VarDeclarator`, `FunctionDecl`, `Param`, `ImportSpecifier`,
    /// `ClassDecl`, `CatchClause`, `TypeParam`, ...
    pub node: NodeId,
    /// The enclosing statement or parent (`VarDecl` for declarators, `ImportDecl` for
    /// specifiers); equal to `node` otherwise.
    pub parent: NodeId,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: Atom,
    pub scope: ScopeId,
    pub flags: SymbolFlags,
    pub decls: Vec<Decl>,
    /// References resolved to this symbol, in source order.
    pub refs: Vec<RefId>,
    /// For `/* global x */` comment globals: the comment span.
    pub comment_span: Option<ts_r_span::Span>,
}

impl Symbol {
    pub fn is_value(&self) -> bool {
        self.flags.intersects(SymbolFlags::VALUE)
    }
    pub fn is_type(&self) -> bool {
        self.flags.intersects(SymbolFlags::TYPE)
    }
}

flags32! {
    pub struct RefFlags {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        /// The reference is in a type position (`let x: Foo`, `implements Foo`).
        const TYPE = 1 << 2;
        /// A write that initialises a declaration (`let a = 1`, `for (const a of b)`).
        const INIT = 1 << 3;
        /// `typeof x` inside a type.
        const TYPEOF_TYPE = 1 << 4;
        /// `export { x }`, `export default x`, `export = x`.
        const EXPORT = 1 << 5;
        /// A JSX tag name or the implicit JSX factory (`React`).
        const JSX = 1 << 6;
        /// Operand of the `typeof` operator in an expression.
        const TYPEOF_EXPR = 1 << 7;
        /// The write is a compound/update (`a += 1`, `a++`): it also reads.
        const COMPOUND = 1 << 8;
        /// The reference is the target of a destructuring assignment.
        const DESTRUCTURE = 1 << 9;
        /// An implicit reference that does not correspond to an identifier node
        /// (the JSX factory).
        const IMPLICIT = 1 << 10;
    }
}

/// What a reference resolved to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Symbol(SymbolId),
    /// A configured or built-in global (not declared in the file).
    Global,
    Unresolved,
}

#[derive(Clone, Copy, Debug)]
pub struct Reference {
    /// The `Ident` (or JSX name) node.
    pub node: NodeId,
    pub name: Atom,
    /// The scope the reference occurs in.
    pub scope: ScopeId,
    pub flags: RefFlags,
    pub resolution: Resolution,
    /// For writes: the assigned expression (`right` of `=`, the declarator init).
    pub write_value: Option<NodeId>,
}

impl Reference {
    pub fn is_read(&self) -> bool {
        self.flags.contains(RefFlags::READ)
    }
    pub fn is_write(&self) -> bool {
        self.flags.contains(RefFlags::WRITE)
    }
    pub fn is_type(&self) -> bool {
        self.flags.contains(RefFlags::TYPE)
    }
    pub fn symbol(&self) -> Option<SymbolId> {
        match self.resolution {
            Resolution::Symbol(s) => Some(s),
            _ => None,
        }
    }
}

/// Scope queries used by rules. Implemented by [`local::LocalSemantic`] today and by an
/// adapter over `ts_r_binder` later.
pub trait Semantic {
    fn scopes(&self) -> &[Scope];
    fn symbols(&self) -> &[Symbol];
    fn references(&self) -> &[Reference];

    /// The symbol declared by the binding identifier `ident`.
    fn symbol_of_decl(&self, ident: NodeId) -> Option<SymbolId>;
    /// The reference made by identifier `ident`.
    fn reference_at(&self, ident: NodeId) -> Option<RefId>;
    /// The scope created by `node` (functions, blocks, classes, ...).
    fn scope_of_node(&self, node: NodeId) -> Option<ScopeId>;
    /// The innermost scope containing `node`.
    fn enclosing_scope(&self, node: NodeId) -> ScopeId;
    /// Finds `name` from `scope` outwards.
    fn lookup(&self, scope: ScopeId, name: &str) -> Option<SymbolId>;
    /// True when `name` is a configured or built-in global.
    fn is_global(&self, name: &str) -> bool;
    /// The name of a symbol.
    fn name(&self, sym: SymbolId) -> &str;

    fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes()[id.0 as usize]
    }
    fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols()[id.0 as usize]
    }
    fn reference(&self, id: RefId) -> &Reference {
        &self.references()[id.0 as usize]
    }
    /// The symbol an identifier refers to or declares.
    fn symbol_of_ident(&self, ident: NodeId) -> Option<SymbolId> {
        self.symbol_of_decl(ident).or_else(|| {
            self.reference_at(ident)
                .and_then(|r| self.reference(r).symbol())
        })
    }
    fn global_scope(&self) -> ScopeId {
        ScopeId(0)
    }
    /// True when the reference at `ident` resolves to a global (built-in, configured or
    /// unresolved), i.e. it is not shadowed by a local declaration.
    fn refers_to_global(&self, ident: NodeId) -> bool {
        match self.reference_at(ident) {
            Some(r) => match self.reference(r).resolution {
                Resolution::Symbol(s) => self.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
                _ => true,
            },
            None => false,
        }
    }
}
