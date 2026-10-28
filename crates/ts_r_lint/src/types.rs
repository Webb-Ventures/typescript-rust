//! Type information for type-aware rules.
//!
//! Rules never talk to a checker directly. They ask the file's [`TypeInfo`] about nodes of
//! the file being linted and about the types, signatures and symbols it hands out (opaque
//! ids of the shared checker's store). [`NoTypeInfo`] answers nothing; the engine enables
//! rules whose [`crate::RuleMeta::requires_types`] is set only when the file has a real
//! provider: [`crate::typed::CkTypes`], a `ts_r_checker` worker that has already checked the
//! file, so `ts-r-lint`, `ts-r-check` and `ts-r-lsp` read the same type graph and no second
//! TypeScript program is built. See `docs/design/0006-lint.md`, "Type-aware rules".
//!
//! Node arguments are nodes of the linted file's AST, which is the program's own parse of
//! the file (the engine lints the checker's `SourceFile` AST when types are on).

use ts_r_ast::{Ast, NodeId};

pub use ts_r_types::{ObjectFlags, SigId as Sig, SymId as Sym, TypeFlags, TypeId as Ty};

/// Identifies a file within one lint run (index into the run's file list).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileKey(pub u32);

/// A declaration of a symbol, possibly in another file of the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decl {
    /// Program file index (opaque).
    pub file: u32,
    pub node: NodeId,
    /// Declared in a default library file (`lib.*.d.ts`).
    pub in_lib: bool,
    /// Declared in the file being linted (`node` is a node of the linted AST).
    pub in_this_file: bool,
    /// Declared in a `.d.ts` file.
    pub in_declaration_file: bool,
    /// Declared in a file found through `node_modules`.
    pub in_external: bool,
}

/// A signature's type predicate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Predicate {
    /// `asserts x` / `asserts x is T` / `asserts this`.
    pub asserts: bool,
    /// Narrows `this` rather than a parameter.
    pub this: bool,
    /// Index of the narrowed parameter.
    pub param_index: usize,
    /// The asserted type (`None` for a bare `asserts x`).
    pub ty: Option<Ty>,
}

/// Binder facts of a symbol (subset of `ts_r_binder::SymbolFlags`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SymFacts {
    pub method: bool,
    pub property: bool,
    pub function: bool,
    pub class: bool,
    pub accessor: bool,
    pub enum_member: bool,
    pub optional: bool,
    pub readonly: bool,
    pub is_static: bool,
    /// An `import` alias (before resolution).
    pub alias: bool,
    pub variable: bool,
    pub type_parameter: bool,
    pub interface: bool,
    pub type_alias: bool,
    pub enum_: bool,
    pub module: bool,
}

/// Type queries available to rules. Every method has an "unknown" default so the trait can
/// grow; only [`TypeInfo::is_available`] providers are ever asked.
#[allow(unused_variables)]
pub trait TypeInfo {
    /// True when this provider can answer queries. Rules that require types are only enabled
    /// when this is true.
    fn is_available(&self) -> bool {
        false
    }

    // ---- nodes of the linted file -------------------------------------------------------

    /// The type at a node, as TypeScript's `getTypeAtLocation`: an expression's (narrowed)
    /// type, a declaration name's declared type, a type node's type.
    fn type_at(&self, node: NodeId) -> Option<Ty> {
        None
    }
    /// The contextual (expected) type of an expression.
    fn contextual_type(&self, node: NodeId) -> Option<Ty> {
        None
    }
    /// The signature a call, `new`, tagged template or decorator resolved to.
    fn resolved_signature(&self, call: NodeId) -> Option<Sig> {
        None
    }
    /// The symbol a node names: an identifier reference, a declaration name, the property of
    /// a member access (`a.b` or the `b` of it).
    fn symbol_at(&self, node: NodeId) -> Option<Sym> {
        None
    }
    /// The type of a type node.
    fn type_of_type_node(&self, node: NodeId) -> Option<Ty> {
        None
    }
    /// The declared (not narrowed) type of the variable, parameter or property an
    /// identifier or member access refers to.
    fn declared_type_at(&self, node: NodeId) -> Option<Ty> {
        None
    }

    // ---- types --------------------------------------------------------------------------

    fn flags(&self, t: Ty) -> TypeFlags {
        TypeFlags::NONE
    }
    /// The error type: [`Ty::ERROR`], or the checker's error type of an unresolved type
    /// reference, which prints by its written name (`Problem`).
    fn is_error(&self, t: Ty) -> bool {
        t == Ty::ERROR
    }
    fn object_flags(&self, t: Ty) -> ObjectFlags {
        ObjectFlags::NONE
    }
    /// A union's members; `[t]` for anything else.
    fn union_members(&self, t: Ty) -> Vec<Ty> {
        vec![t]
    }
    /// An intersection's members; `[t]` for anything else.
    fn intersection_members(&self, t: Ty) -> Vec<Ty> {
        vec![t]
    }
    /// The type as TypeScript prints it in messages.
    fn type_to_string(&self, t: Ty) -> String {
        String::new()
    }
    /// The apparent type (`String` for `string`, constraints of type parameters).
    fn apparent_type(&self, t: Ty) -> Ty {
        t
    }
    /// The type mentions type parameters anywhere (a generic, deferred type). Unknown
    /// answers `true`.
    fn contains_type_params(&self, t: Ty) -> bool {
        true
    }
    /// The base constraint of a type parameter or other instantiable type; `t` otherwise.
    fn base_constraint(&self, t: Ty) -> Ty {
        t
    }
    fn property_of_type(&self, t: Ty, name: &str) -> Option<Sym> {
        None
    }
    fn properties_of(&self, t: Ty) -> Vec<Sym> {
        Vec::new()
    }
    fn type_of_symbol(&self, s: Sym) -> Ty {
        Ty::UNRESOLVED
    }
    /// The declared type of a class, interface, alias or enum symbol.
    fn declared_type_of_symbol(&self, s: Sym) -> Option<Ty> {
        None
    }
    /// Call (or construct) signatures of a type.
    fn signatures(&self, t: Ty, construct: bool) -> Vec<Sig> {
        Vec::new()
    }
    fn return_type(&self, sig: Sig) -> Ty {
        Ty::UNRESOLVED
    }
    /// The type of argument position `i` (rest parameters spread).
    fn param_type(&self, sig: Sig, i: usize) -> Option<Ty> {
        None
    }
    /// Number of non-rest parameters and whether there is a rest parameter.
    fn param_count(&self, sig: Sig) -> (usize, bool) {
        (0, false)
    }
    /// The number of required parameters (`None`: unknown).
    fn min_arg_count(&self, sig: Sig) -> Option<usize> {
        None
    }
    /// The declared type of the rest parameter (`T[]`, a tuple, `any`, ...), not spread.
    fn rest_param_type(&self, sig: Sig) -> Option<Ty> {
        None
    }
    /// The declared type of the `this` parameter.
    fn this_param_type(&self, sig: Sig) -> Option<Ty> {
        None
    }
    /// The declaration of a signature (`None` for synthetic ones).
    fn signature_decl(&self, sig: Sig) -> Option<Decl> {
        None
    }
    /// Type parameters of a signature.
    fn signature_type_params(&self, sig: Sig) -> Vec<Ty> {
        Vec::new()
    }
    /// The type predicate of a signature (`x is T`, `asserts x`, `this is T`, ...).
    fn type_predicate(&self, sig: Sig) -> Option<Predicate> {
        None
    }
    fn is_assignable(&self, source: Ty, target: Ty) -> bool {
        false
    }
    /// `Awaited<T>` as `await` computes it.
    fn awaited_type(&self, t: Ty) -> Ty {
        t
    }
    /// The element type `for (.. of)` (or `for await`) produces for a value of type `t`.
    fn iterated_type(&self, _t: Ty, _is_await: bool) -> Option<Ty> {
        None
    }
    /// The property name of the well-known symbol `Symbol.<name>` (`asyncIterator`), as
    /// [`TypeInfo::property_of_type`] expects it.
    fn well_known_symbol(&self, _name: &str) -> Option<String> {
        None
    }
    /// Type arguments of a generic type reference (tuple element types for tuples).
    fn type_args(&self, t: Ty) -> Vec<Ty> {
        Vec::new()
    }
    fn is_array(&self, t: Ty) -> bool {
        false
    }
    fn is_tuple(&self, t: Ty) -> bool {
        false
    }
    /// Index of a tuple type's rest element (`...T[]`); its type argument is the element type.
    fn tuple_rest_index(&self, t: Ty) -> Option<usize> {
        None
    }
    /// Element type of an array or readonly array (not tuples).
    fn array_element(&self, t: Ty) -> Option<Ty> {
        None
    }
    /// The value of a string, number or bigint literal type, as source text would show it
    /// (strings without quotes).
    fn literal_text(&self, t: Ty) -> Option<String> {
        None
    }
    /// The symbol of a class/interface instance type, enum, alias target, ...
    fn type_symbol(&self, t: Ty) -> Option<Sym> {
        None
    }
    /// The alias symbol a type was written through (`type A = ...; let a: A`).
    fn alias_symbol(&self, t: Ty) -> Option<Sym> {
        None
    }
    /// The type arguments of the alias reference `t` came from (`Readonly<Error>` -> `[Error]`).
    fn alias_type_args(&self, t: Ty) -> Vec<Ty> {
        Vec::new()
    }
    /// The index signature type of `t` for `string` (`number: false`) or `number` keys.
    fn index_type(&self, t: Ty, number: bool) -> Option<Ty> {
        None
    }
    /// The index signature type a property name reads through a pattern key
    /// (`[k: Lowercase<string>]` for `t.a`), when no `string` signature covers it.
    fn name_index_type(&self, t: Ty, name: &str) -> Option<Ty> {
        None
    }
    /// The declared global type `name` (`Promise`, `Error`, ...), as a non-generic reference
    /// target.
    fn global_type(&self, name: &str) -> Option<Ty> {
        None
    }
    /// For a reference to a generic type (`Promise<string>`), its target (`Promise<T>`).
    fn reference_target(&self, t: Ty) -> Option<Ty> {
        None
    }
    /// Base types of a class or interface type.
    fn base_types(&self, t: Ty) -> Vec<Ty> {
        Vec::new()
    }
    /// Non-nullable form (`NonNullable<T>`).
    fn non_nullable(&self, t: Ty) -> Ty {
        t
    }
    /// The widened, regular form of a literal (`"a"` -> `string`).
    fn base_of_literal(&self, t: Ty) -> Ty {
        t
    }
    /// Union of `types` (subtype-reduced like TypeScript's `getUnionType`).
    fn union_of(&self, types: &[Ty]) -> Ty {
        types.first().copied().unwrap_or(Ty::NEVER)
    }
    /// Compiler option facts.
    fn strict_null_checks(&self) -> bool {
        true
    }
    fn option(&self, name: &str) -> Option<bool> {
        None
    }

    // ---- symbols ------------------------------------------------------------------------

    fn symbol_name(&self, s: Sym) -> String {
        String::new()
    }
    fn symbol_facts(&self, s: Sym) -> SymFacts {
        SymFacts::default()
    }
    fn symbol_decls(&self, s: Sym) -> Vec<Decl> {
        Vec::new()
    }
    /// The symbol an import alias refers to.
    fn resolve_alias(&self, s: Sym) -> Option<Sym> {
        None
    }
    /// The symbol's parent (the class, interface or namespace declaring it).
    fn symbol_parent(&self, s: Sym) -> Option<Sym> {
        None
    }
    /// The type of `node` without the `undefined` an enclosing optional chain's
    /// short-circuit adds (`a?.b` is the type of `b` read from a non-nullish `a`). Other
    /// nodes: [`TypeInfo::type_at`].
    fn non_optional_type_at(&self, node: NodeId) -> Option<Ty> {
        self.type_at(node)
    }
    /// The types a non-object type is built from (`keyof X` -> `X`, `O[K]` -> `O`, `K`,
    /// conditional check, extends and branch types, template literal spans, ...), and for a
    /// mapped type its constraint, `as` type and template. Empty for other types (walk
    /// unions, references, members and signatures with the other methods).
    fn type_parts(&self, t: Ty) -> Vec<Ty> {
        Vec::new()
    }
    /// For a property of a mapped type (`{ [K in E]: T }`, `Record<E, T>`), the key type it
    /// was created for (`E.A`).
    fn mapped_key_type(&self, s: Sym) -> Option<Ty> {
        None
    }
    /// The AST and source text of the file declaring `d`.
    fn decl_file(&self, d: &Decl) -> Option<(&Ast, &str)> {
        None
    }
    /// The absolute path of the file declaring `d`.
    fn decl_path(&self, d: &Decl) -> Option<String> {
        None
    }
    /// The export named `name` of a namespace, enum, class or module symbol.
    fn export_of(&self, s: Sym, name: &str) -> Option<Sym> {
        None
    }
    /// The symbol the linted file exports as `name`, re-exports followed.
    fn file_export(&self, name: &str) -> Option<Sym> {
        None
    }
    /// The global symbol named `name` (any meaning).
    fn global_symbol(&self, name: &str) -> Option<Sym> {
        None
    }
    /// The `JSX` namespace JSX at `at` is typed with: the JSX factory's (`h.JSX`,
    /// `React.JSX`), else the global one.
    fn jsx_namespace(&self, at: NodeId) -> Option<Sym> {
        None
    }
}

/// The provider for files without type information: every answer is unknown.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTypeInfo;

impl TypeInfo for NoTypeInfo {}
