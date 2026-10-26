//! Node kinds. `define_nodes!` is the single source of truth: it generates the `Node` enum,
//! `NodeKind`, the source-order child walker and the dump support.
//!
//! Field order in each variant is source order; the child walker relies on it.

use crate::{Ast, Atom, NodeId, NodeList};

/// A field that may hold children. Scalars visit nothing.
pub trait Field {
    fn each_child<F: FnMut(NodeId)>(&self, ast: &Ast, f: &mut F);
    fn dump(&self, ast: &Ast) -> FieldDump;

    /// This field with every child id `c` replaced by `f(ast, c)`, in source order. A list
    /// is copied through `Ast::add_list` only if an item changes. Scalars are returned as is.
    #[inline]
    fn map_child<F: FnMut(&mut Ast, NodeId) -> NodeId>(self, _ast: &mut Ast, _f: &mut F) -> Self
    where
        Self: Sized,
    {
        self
    }
}

/// How a field is shown in `Ast::dump`.
#[derive(Debug)]
pub enum FieldDump {
    /// Default value; omitted.
    Skip,
    Scalar(String),
    Child(NodeId),
    List(NodeList),
}

impl Field for NodeId {
    #[inline]
    fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, f: &mut F) {
        f(*self)
    }
    fn dump(&self, _: &Ast) -> FieldDump {
        FieldDump::Child(*self)
    }
    #[inline]
    fn map_child<F: FnMut(&mut Ast, NodeId) -> NodeId>(self, ast: &mut Ast, f: &mut F) -> Self {
        f(ast, self)
    }
}

impl Field for Option<NodeId> {
    #[inline]
    fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, f: &mut F) {
        if let Some(id) = self {
            f(*id)
        }
    }
    fn dump(&self, _: &Ast) -> FieldDump {
        match self {
            Some(id) => FieldDump::Child(*id),
            None => FieldDump::Skip,
        }
    }
    #[inline]
    fn map_child<F: FnMut(&mut Ast, NodeId) -> NodeId>(self, ast: &mut Ast, f: &mut F) -> Self {
        self.map(|id| f(ast, id))
    }
}

impl Field for NodeList {
    #[inline]
    fn each_child<F: FnMut(NodeId)>(&self, ast: &Ast, f: &mut F) {
        for &id in ast.list(*self) {
            f(id)
        }
    }
    fn dump(&self, _: &Ast) -> FieldDump {
        if self.is_empty() {
            FieldDump::Skip
        } else {
            FieldDump::List(*self)
        }
    }
    fn map_child<F: FnMut(&mut Ast, NodeId) -> NodeId>(self, ast: &mut Ast, f: &mut F) -> Self {
        // Lists are immutable once written and `list_items` only grows, so indexing the
        // original list stays valid while `f` adds nodes and lists.
        let len = ast.list(self).len();
        let mut changed: Option<Vec<NodeId>> = None;
        for i in 0..len {
            let old = ast.list(self)[i];
            let new = f(ast, old);
            match &mut changed {
                Some(items) => items.push(new),
                None if new != old => {
                    let mut items = Vec::with_capacity(len);
                    items.extend_from_slice(&ast.list(self)[..i]);
                    items.push(new);
                    changed = Some(items);
                }
                None => {}
            }
        }
        match changed {
            Some(items) => ast.add_list(&items),
            None => self,
        }
    }
}

impl Field for Atom {
    #[inline]
    fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, _: &mut F) {}
    fn dump(&self, ast: &Ast) -> FieldDump {
        FieldDump::Scalar(format!("{:?}", ast.atom(*self)))
    }
}

impl Field for bool {
    #[inline]
    fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, _: &mut F) {}
    fn dump(&self, _: &Ast) -> FieldDump {
        if *self {
            FieldDump::Scalar(String::new())
        } else {
            FieldDump::Skip
        }
    }
}

/// An `f64` stored as two `u32` halves so nodes stay 4-byte aligned.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct F64Bits {
    pub lo: u32,
    pub hi: u32,
}

impl F64Bits {
    pub fn new(v: f64) -> F64Bits {
        let b = v.to_bits();
        F64Bits {
            lo: b as u32,
            hi: (b >> 32) as u32,
        }
    }
    pub fn get(self) -> f64 {
        f64::from_bits((self.hi as u64) << 32 | self.lo as u64)
    }
}

impl std::fmt::Debug for F64Bits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.get())
    }
}

impl Field for F64Bits {
    #[inline]
    fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, _: &mut F) {}
    fn dump(&self, _: &Ast) -> FieldDump {
        FieldDump::Scalar(format!("{}", self.get()))
    }
}

/// Fieldless `u8` enums used in nodes. The first variant is the default and is omitted from
/// dumps.
macro_rules! small_enums {
    ($( $(#[$m:meta])* pub enum $name:ident { $($(#[$vm:meta])* $v:ident),* $(,)? } )*) => {$(
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
        #[repr(u8)]
        pub enum $name { #[default] $($(#[$vm])* $v),* }

        impl Field for $name {
            #[inline]
            fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, _: &mut F) {}
            fn dump(&self, _: &Ast) -> FieldDump {
                if *self == $name::default() { FieldDump::Skip } else { FieldDump::Scalar(format!("{:?}", self)) }
            }
        }
    )*};
}

small_enums! {
    pub enum VarKind { Var, Let, Const, Using, AwaitUsing }

    pub enum BinaryOp {
        /// `==`
        Eq, NotEq, StrictEq, StrictNotEq, Lt, LtEq, Gt, GtEq,
        Shl, Shr, UShr, Add, Sub, Mul, Div, Mod, Exp,
        BitOr, BitXor, BitAnd, In, InstanceOf,
        LogicalOr, LogicalAnd, Coalesce,
    }

    pub enum AssignOp {
        Assign, AddAssign, SubAssign, MulAssign, DivAssign, ModAssign, ExpAssign,
        ShlAssign, ShrAssign, UShrAssign, BitOrAssign, BitXorAssign, BitAndAssign,
        OrAssign, AndAssign, CoalesceAssign,
    }

    pub enum UnaryOp { Minus, Plus, Not, BitNot, TypeOf, Void, Delete }

    pub enum UpdateOp { Increment, Decrement }

    pub enum MethodKind { Method, Get, Set, Constructor }

    pub enum ModuleKind {
        /// `namespace X {}`
        Namespace,
        /// `module X {}` or `declare module "x" {}`
        Module,
        /// `declare global {}`
        Global,
    }

    pub enum HeritageKind { Extends, Implements }

    pub enum KeywordTypeKind {
        Any, Unknown, Number, BigInt, Boolean, String, Symbol, Object, Never, Void,
        Undefined, Null, Intrinsic,
        /// The `const` in `x as const` / `<const>x`.
        Const,
    }

    pub enum TypeOperatorKind { KeyOf, Unique, Readonly }

    pub enum MappedModifier {
        None,
        /// `readonly` / `?` without a sign.
        Present,
        /// `+readonly` / `+?`
        Add,
        /// `-readonly` / `-?`
        Remove,
    }

    pub enum MetaPropKind { NewTarget, ImportMeta }

    pub enum ImportPhase { Evaluation, Defer, Source }
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Eq => "==",
            NotEq => "!=",
            StrictEq => "===",
            StrictNotEq => "!==",
            Lt => "<",
            LtEq => "<=",
            Gt => ">",
            GtEq => ">=",
            Shl => "<<",
            Shr => ">>",
            UShr => ">>>",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod => "%",
            Exp => "**",
            BitOr => "|",
            BitXor => "^",
            BitAnd => "&",
            In => "in",
            InstanceOf => "instanceof",
            LogicalOr => "||",
            LogicalAnd => "&&",
            Coalesce => "??",
        }
    }
}

impl AssignOp {
    pub fn as_str(self) -> &'static str {
        use AssignOp::*;
        match self {
            Assign => "=",
            AddAssign => "+=",
            SubAssign => "-=",
            MulAssign => "*=",
            DivAssign => "/=",
            ModAssign => "%=",
            ExpAssign => "**=",
            ShlAssign => "<<=",
            ShrAssign => ">>=",
            UShrAssign => ">>>=",
            BitOrAssign => "|=",
            BitXorAssign => "^=",
            BitAndAssign => "&=",
            OrAssign => "||=",
            AndAssign => "&&=",
            CoalesceAssign => "??=",
        }
    }
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        use UnaryOp::*;
        match self {
            Minus => "-",
            Plus => "+",
            Not => "!",
            BitNot => "~",
            TypeOf => "typeof",
            Void => "void",
            Delete => "delete",
        }
    }
}

impl UpdateOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateOp::Increment => "++",
            UpdateOp::Decrement => "--",
        }
    }
}

macro_rules! bitflags16 {
    ($(#[$m:meta])* pub struct $name:ident: $ty:ty { $( $(#[$fm:meta])* const $flag:ident = $val:expr; )* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub $ty);

        impl $name {
            $( $(#[$fm])* pub const $flag: $name = $name($val); )*
            pub const NONE: $name = $name(0);
            const NAMES: &'static [(&'static str, $ty)] = &[$( (stringify!($flag), $val) ),*];

            #[inline] pub const fn contains(self, other: $name) -> bool { self.0 & other.0 == other.0 }
            #[inline] pub const fn intersects(self, other: $name) -> bool { self.0 & other.0 != 0 }
            #[inline] pub const fn is_empty(self) -> bool { self.0 == 0 }
            #[inline] pub fn insert(&mut self, other: $name) { self.0 |= other.0 }
            #[inline] pub fn remove(&mut self, other: $name) { self.0 &= !other.0 }
            #[inline] pub const fn union(self, other: $name) -> $name { $name(self.0 | other.0) }
        }

        impl std::ops::BitOr for $name {
            type Output = $name;
            fn bitor(self, rhs: $name) -> $name { $name(self.0 | rhs.0) }
        }

        impl std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: $name) { self.0 |= rhs.0 }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let mut first = true;
                write!(f, "[")?;
                for (name, bit) in Self::NAMES {
                    if self.0 & bit != 0 {
                        if !first { write!(f, " ")?; }
                        first = false;
                        write!(f, "{}", name.to_ascii_lowercase())?;
                    }
                }
                write!(f, "]")
            }
        }

        impl Field for $name {
            #[inline]
            fn each_child<F: FnMut(NodeId)>(&self, _: &Ast, _: &mut F) {}
            fn dump(&self, _: &Ast) -> FieldDump {
                if self.is_empty() { FieldDump::Skip } else { FieldDump::Scalar(format!("{:?}", self)) }
            }
        }
    };
}

bitflags16! {
    /// Declaration modifiers, as written (validity is checked later).
    pub struct Modifiers: u16 {
        const EXPORT = 1 << 0;
        const DEFAULT = 1 << 1;
        const DECLARE = 1 << 2;
        const ABSTRACT = 1 << 3;
        const PUBLIC = 1 << 4;
        const PRIVATE = 1 << 5;
        const PROTECTED = 1 << 6;
        const READONLY = 1 << 7;
        const STATIC = 1 << 8;
        const OVERRIDE = 1 << 9;
        const ACCESSOR = 1 << 10;
        const CONST = 1 << 11;
        const IN = 1 << 12;
        const OUT = 1 << 13;
        const ASYNC = 1 << 14;
    }
}

bitflags16! {
    /// Function flags.
    pub struct FnFlags: u8 {
        const ASYNC = 1 << 0;
        const GENERATOR = 1 << 1;
        /// Arrow function whose body is an expression (no braces).
        const EXPRESSION_BODY = 1 << 2;
    }
}

macro_rules! define_nodes {
    ($( $(#[$m:meta])* $name:ident { $( $field:ident : $ty:ty ),* $(,)? } )*) => {
        /// One AST node. See `docs/design/0001-ast.md`.
        #[derive(Clone, Copy, PartialEq, Debug)]
        pub enum Node {
            $( $(#[$m])* $name { $( $field: $ty ),* } ),*
        }

        /// The kind of a [`Node`], without its fields.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum NodeKind { $( $name ),* }

        impl NodeKind {
            pub const ALL: &'static [NodeKind] = &[$( NodeKind::$name ),*];

            pub fn name(self) -> &'static str {
                match self { $( NodeKind::$name => stringify!($name) ),* }
            }
        }

        impl Node {
            #[inline]
            pub fn kind(&self) -> NodeKind {
                match self { $( Node::$name { .. } => NodeKind::$name ),* }
            }

            /// Calls `f` for each direct child, in source order.
            #[allow(unused_variables)]
            #[inline]
            pub fn for_each_child<F: FnMut(NodeId)>(&self, ast: &Ast, f: &mut F) {
                match self {
                    $( Node::$name { $( $field ),* } => { $( Field::each_child($field, ast, f); )* } ),*
                }
            }

            /// This node with every child id `c` replaced by `f(ast, c)`. Children are visited
            /// in source order (as by `for_each_child`); a `NodeList` that changes is copied
            /// with `Ast::add_list`, one that does not is kept. The result is not added to
            /// the arena. `f` gets the arena so it can allocate, e.g. a deep clone:
            /// `ast.map_children(id, &mut |ast, c| deep_clone(ast, c))`.
            #[allow(unused_variables)]
            #[inline]
            pub fn map_children<F: FnMut(&mut Ast, NodeId) -> NodeId>(
                &self,
                ast: &mut Ast,
                f: &mut F,
            ) -> Node {
                match *self {
                    $( Node::$name { $( $field ),* } => Node::$name {
                        $( $field: Field::map_child($field, ast, f) ),*
                    } ),*
                }
            }

            /// Field names and dump forms, for `Ast::dump`.
            #[allow(unused_variables)]
            pub fn dump_fields(&self, ast: &Ast) -> Vec<(&'static str, FieldDump)> {
                match self {
                    $( Node::$name { $( $field ),* } => {
                        vec![$( (stringify!($field), Field::dump($field, ast)) ),*]
                    } ),*
                }
            }
        }
    };
}

define_nodes! {
    /// Reserved slot 0. Never referenced.
    Dummy {}
    /// Placeholder for a node that was expected but missing (error recovery).
    Missing {}

    // ---- Program and statements ----
    Program { stmts: NodeList }
    /// `"use strict";` prologue entry. `value` is the cooked string.
    Directive { value: Atom }
    Block { stmts: NodeList }
    Empty {}
    ExprStmt { expr: NodeId }
    If { test: NodeId, cons: NodeId, alt: Option<NodeId> }
    For { init: Option<NodeId>, test: Option<NodeId>, update: Option<NodeId>, body: NodeId }
    ForIn { left: NodeId, right: NodeId, body: NodeId }
    ForOf { is_await: bool, left: NodeId, right: NodeId, body: NodeId }
    While { test: NodeId, body: NodeId }
    DoWhile { body: NodeId, test: NodeId }
    Return { arg: Option<NodeId> }
    Break { label: Option<NodeId> }
    Continue { label: Option<NodeId> }
    Throw { arg: NodeId }
    Labeled { label: NodeId, body: NodeId }
    Switch { disc: NodeId, cases: NodeList }
    /// `test` is `None` for `default:`.
    SwitchCase { test: Option<NodeId>, body: NodeList }
    Try { block: NodeId, handler: Option<NodeId>, finalizer: Option<NodeId> }
    CatchClause { param: Option<NodeId>, type_ann: Option<NodeId>, body: NodeId }
    Debugger {}
    With { obj: NodeId, body: NodeId }
    /// `var`/`let`/`const`/`using`/`await using`; also the `init` of `for` loops.
    VarDecl { modifiers: Modifiers, kind: VarKind, decls: NodeList }
    VarDeclarator { name: NodeId, definite: bool, type_ann: Option<NodeId>, init: Option<NodeId> }

    // ---- Functions and classes ----
    FunctionDecl {
        modifiers: Modifiers, flags: FnFlags, name: Option<NodeId>, type_params: Option<NodeId>,
        params: NodeList, return_type: Option<NodeId>, body: Option<NodeId>,
    }
    /// Function expressions and the value of every `MethodDef`.
    FunctionExpr {
        modifiers: Modifiers, flags: FnFlags, name: Option<NodeId>, type_params: Option<NodeId>,
        params: NodeList, return_type: Option<NodeId>, body: Option<NodeId>,
    }
    /// `body` is a `Block` or, with `FnFlags::EXPRESSION_BODY`, an expression.
    Arrow {
        flags: FnFlags, type_params: Option<NodeId>, params: NodeList,
        return_type: Option<NodeId>, body: NodeId,
    }
    /// A parameter. `pattern` is an `Ident` (possibly `this`), `ObjectPattern` or
    /// `ArrayPattern`. Accessibility/readonly/override modifiers make it a parameter property.
    Param {
        decorators: NodeList, modifiers: Modifiers, rest: bool, pattern: NodeId, optional: bool,
        type_ann: Option<NodeId>, init: Option<NodeId>,
    }
    Decorator { expr: NodeId }
    ClassDecl {
        decorators: NodeList, modifiers: Modifiers, name: Option<NodeId>,
        type_params: Option<NodeId>, heritage: NodeList, members: NodeList,
    }
    ClassExpr {
        decorators: NodeList, modifiers: Modifiers, name: Option<NodeId>,
        type_params: Option<NodeId>, heritage: NodeList, members: NodeList,
    }
    /// `extends A<T>` or `implements A, B`.
    HeritageClause { kind: HeritageKind, types: NodeList }
    ExprWithTypeArgs { expr: NodeId, type_args: Option<NodeId> }
    /// Class field, including `accessor` fields (`Modifiers::ACCESSOR`).
    PropertyDef {
        decorators: NodeList, modifiers: Modifiers, key: NodeId, optional: bool, definite: bool,
        type_ann: Option<NodeId>, value: Option<NodeId>,
    }
    /// Method, getter, setter or constructor in a class or object literal. `value` is a
    /// `FunctionExpr` holding params, body and async/generator flags.
    MethodDef {
        decorators: NodeList, modifiers: Modifiers, kind: MethodKind, key: NodeId,
        optional: bool, value: NodeId,
    }
    StaticBlock { body: NodeList }
    /// `[key: string]: T` in classes, interfaces and type literals.
    IndexSignature { modifiers: Modifiers, params: NodeList, type_ann: Option<NodeId> }
    /// `[expr]` as a property name.
    ComputedKey { expr: NodeId }

    // ---- TS declarations ----
    Interface {
        modifiers: Modifiers, name: NodeId, type_params: Option<NodeId>, extends: NodeList,
        body: NodeList,
    }
    TypeAlias { modifiers: Modifiers, name: NodeId, type_params: Option<NodeId>, ty: NodeId }
    Enum { modifiers: Modifiers, name: NodeId, members: NodeList }
    EnumMember { name: NodeId, init: Option<NodeId> }
    /// `namespace A.B {}`, `module "x" {}`, `declare global {}`. `name` is an `Ident`,
    /// `QualifiedName` or `StringLit`; `body` is a `ModuleBlock` (absent for `declare module "x";`).
    Module { modifiers: Modifiers, kind: ModuleKind, name: NodeId, body: Option<NodeId> }
    ModuleBlock { stmts: NodeList }

    // ---- Modules ----
    /// `has_clause` is false for side-effect imports (`import "x"`).
    ImportDecl {
        type_only: bool, phase: ImportPhase, has_clause: bool, specifiers: NodeList,
        source: NodeId, attributes: Option<NodeId>,
    }
    ImportDefaultSpecifier { local: NodeId }
    ImportNamespaceSpecifier { local: NodeId }
    /// `imported` is present only with `as` (`{ a as b }`); it may be a `StringLit`.
    ImportSpecifier { type_only: bool, imported: Option<NodeId>, local: NodeId }
    /// `with { ... }`, or legacy `assert { ... }` when `assert` is true.
    ImportAttributes { assert: bool, entries: NodeList }
    ImportAttribute { key: NodeId, value: NodeId }
    /// `import x = require("y")` / `import x = A.B`.
    ImportEquals { modifiers: Modifiers, type_only: bool, name: NodeId, module_ref: NodeId }
    ExternalModuleRef { expr: NodeId }
    ExportNamed {
        type_only: bool, specifiers: NodeList, source: Option<NodeId>, attributes: Option<NodeId>,
    }
    ExportSpecifier { type_only: bool, local: NodeId, exported: Option<NodeId> }
    /// `export * from "x"` / `export * as ns from "x"`.
    ExportAll {
        type_only: bool, exported: Option<NodeId>, source: NodeId, attributes: Option<NodeId>,
    }
    /// `export default <expr>;` (declarations use `Modifiers::DEFAULT` instead).
    ExportDefaultExpr { expr: NodeId }
    /// `export = expr;`
    ExportAssignment { expr: NodeId }
    /// `export as namespace X;`
    NamespaceExport { name: NodeId }

    // ---- Expressions ----
    Ident { name: Atom }
    /// `#name`; `name` excludes the `#`.
    PrivateName { name: Atom }
    This {}
    Super {}
    NullLit {}
    BoolLit { value: bool }
    NumberLit { value: F64Bits }
    /// `value` is the literal text without the `n` suffix.
    BigIntLit { value: Atom }
    /// `value` is the cooked string; the raw text is the span.
    StringLit { value: Atom }
    RegexLit { pattern: Atom, flags: Atom }
    /// `parts` alternate `TemplateElement`, expression, `TemplateElement`, ...
    TemplateLit { parts: NodeList }
    /// Raw text is the span (excluding the delimiters).
    TemplateElement { cooked: Atom, invalid_escape: bool }
    TaggedTemplate { tag: NodeId, type_args: Option<NodeId>, quasi: NodeId }
    ArrayLit { elements: NodeList }
    /// Elision in an array literal or pattern.
    Hole {}
    ObjectLit { props: NodeList }
    /// `key: value` in an object literal. `modifiers` are misplaced ones (`{ public a: 1 }`),
    /// kept for the checker's grammar errors.
    Property { modifiers: Modifiers, key: NodeId, value: NodeId }
    /// `{ a }` or, in assignment patterns, `{ a = 1 }`. `modifiers` as for `Property`.
    ShorthandProp { modifiers: Modifiers, name: NodeId, init: Option<NodeId> }
    /// `...x` in arrays, objects and arguments.
    Spread { arg: NodeId }
    Paren { expr: NodeId }
    Seq { exprs: NodeList }
    Unary { op: UnaryOp, arg: NodeId }
    Update { op: UpdateOp, prefix: bool, arg: NodeId }
    Await { arg: NodeId }
    Yield { delegate: bool, arg: Option<NodeId> }
    Binary { left: NodeId, op: BinaryOp, right: NodeId }
    Assign { left: NodeId, op: AssignOp, right: NodeId }
    Conditional { test: NodeId, cons: NodeId, alt: NodeId }
    /// `optional` marks `f?.()`.
    Call { callee: NodeId, optional: bool, type_args: Option<NodeId>, args: NodeList }
    /// `has_args` is false for `new Foo` without parentheses.
    New { callee: NodeId, type_args: Option<NodeId>, has_args: bool, args: NodeList }
    /// `obj.prop` / `obj?.prop`; `prop` is an `Ident` or `PrivateName`.
    Member { obj: NodeId, optional: bool, prop: NodeId }
    /// `obj[index]` / `obj?.[index]`.
    Index { obj: NodeId, optional: bool, index: NodeId }
    MetaProperty { kind: MetaPropKind }
    /// `import(x)`, `import(x, opts)`, `import.defer(x)`, `import.source(x)`. `type_args` is
    /// the `TypeArgs` of `import<T>(x)` / `import<T>` (an error, TS1326, but the checker
    /// still resolves the names in them); `None` otherwise and for an empty `import<>`.
    ImportCall { phase: ImportPhase, type_args: Option<NodeId>, arg: NodeId, options: Option<NodeId> }
    As { expr: NodeId, ty: NodeId }
    Satisfies { expr: NodeId, ty: NodeId }
    /// `<T>expr`
    TypeAssertion { ty: NodeId, expr: NodeId }
    NonNull { expr: NodeId }
    /// `f<T>` without a call.
    Instantiation { expr: NodeId, type_args: NodeId }

    // ---- Binding patterns ----
    ObjectPattern { props: NodeList }
    /// `{ key: value = init }`; shorthand `{ key = init }` has no `value`.
    BindingProperty { key: NodeId, value: Option<NodeId>, init: Option<NodeId> }
    ArrayPattern { elements: NodeList }
    /// `pattern = default` inside an array pattern.
    AssignPattern { left: NodeId, right: NodeId }
    /// `...pattern` inside object and array patterns.
    Rest { arg: NodeId }

    // ---- Types ----
    KeywordType { kind: KeywordTypeKind }
    /// `name` is an `Ident` or `QualifiedName`.
    TypeRef { name: NodeId, type_args: Option<NodeId> }
    QualifiedName { left: NodeId, right: NodeId }
    ThisType {}
    /// `parts` alternate `TemplateElement`, type, `TemplateElement`, ...
    TemplateLitType { parts: NodeList }
    ArrayType { elem: NodeId }
    TupleType { elems: NodeList }
    NamedTupleMember { rest: bool, name: NodeId, optional: bool, ty: NodeId }
    OptionalType { ty: NodeId }
    RestType { ty: NodeId }
    UnionType { types: NodeList }
    IntersectionType { types: NodeList }
    FunctionType { type_params: Option<NodeId>, params: NodeList, return_type: NodeId }
    ConstructorType {
        is_abstract: bool, type_params: Option<NodeId>, params: NodeList, return_type: NodeId,
    }
    TypeLiteral { members: NodeList }
    PropertySignature { modifiers: Modifiers, key: NodeId, optional: bool, type_ann: Option<NodeId> }
    /// `modifiers` are only ever misplaced ones (`interface I { public m(): void }`).
    MethodSignature {
        modifiers: Modifiers, kind: MethodKind, key: NodeId, optional: bool, type_params: Option<NodeId>,
        params: NodeList, return_type: Option<NodeId>,
    }
    CallSignature { type_params: Option<NodeId>, params: NodeList, return_type: Option<NodeId> }
    ConstructSignature {
        type_params: Option<NodeId>, params: NodeList, return_type: Option<NodeId>,
    }
    /// `typeof x.y<T>`; `expr_name` is an `Ident`, `QualifiedName` or `ImportType`.
    TypeQuery { expr_name: NodeId, type_args: Option<NodeId> }
    TypeOperator { op: TypeOperatorKind, ty: NodeId }
    IndexedAccessType { obj: NodeId, index: NodeId }
    ConditionalType { check: NodeId, extends: NodeId, true_ty: NodeId, false_ty: NodeId }
    /// `infer T` / `infer T extends U`; `type_param` is a `TypeParam`.
    InferType { type_param: NodeId }
    /// `{ readonly [K in T as N]?: V }`; `type_param` is a `TypeParam` whose constraint is
    /// the `in` type.
    MappedType {
        readonly: MappedModifier, type_param: NodeId, name_type: Option<NodeId>,
        optional: MappedModifier, ty: Option<NodeId>,
    }
    ParenType { ty: NodeId }
    /// `x is T`, `asserts x`, `asserts x is T`; `param` is an `Ident` or `ThisType`.
    TypePredicate { asserts: bool, param: NodeId, ty: Option<NodeId> }
    /// `import("x").A.B<T>`, optionally `typeof import("x")`.
    ImportType {
        is_typeof: bool, arg: NodeId, attributes: Option<NodeId>, qualifier: Option<NodeId>,
        type_args: Option<NodeId>,
    }
    TypeParams { params: NodeList }
    /// `const`/`in`/`out` are `modifiers`.
    TypeParam {
        modifiers: Modifiers, name: NodeId, constraint: Option<NodeId>, default: Option<NodeId>,
    }
    TypeArgs { args: NodeList }

    // ---- JSX ----
    JsxElement { opening: NodeId, children: NodeList, closing: Option<NodeId> }
    JsxOpening { name: NodeId, type_args: Option<NodeId>, attrs: NodeList, self_closing: bool }
    JsxClosing { name: NodeId }
    JsxFragment { children: NodeList }
    JsxAttr { name: NodeId, value: Option<NodeId> }
    JsxSpreadAttr { arg: NodeId }
    /// `{expr}`; `expr` is `None` for `{}` or `{/* comment */}`.
    JsxExprContainer { expr: Option<NodeId> }
    JsxSpreadChild { expr: NodeId }
    /// Raw text between tags; the text is the span.
    JsxText {}
    JsxNamespacedName { ns: NodeId, name: NodeId }
    JsxMemberExpr { obj: NodeId, prop: NodeId }
}

impl Node {
    /// True for nodes that are statements or declarations in a statement list.
    pub fn is_statement(&self) -> bool {
        use NodeKind::*;
        matches!(
            self.kind(),
            Directive
                | Block
                | Empty
                | ExprStmt
                | If
                | For
                | ForIn
                | ForOf
                | While
                | DoWhile
                | Return
                | Break
                | Continue
                | Throw
                | Labeled
                | Switch
                | Try
                | Debugger
                | With
                | VarDecl
                | FunctionDecl
                | ClassDecl
                | Interface
                | TypeAlias
                | Enum
                | Module
                | ImportDecl
                | ImportEquals
                | ExportNamed
                | ExportAll
                | ExportDefaultExpr
                | ExportAssignment
                | NamespaceExport
        )
    }

    /// The declaration modifiers of nodes that carry them.
    pub fn modifiers(&self) -> Modifiers {
        match *self {
            Node::VarDecl { modifiers, .. }
            | Node::FunctionDecl { modifiers, .. }
            | Node::FunctionExpr { modifiers, .. }
            | Node::Param { modifiers, .. }
            | Node::ClassDecl { modifiers, .. }
            | Node::ClassExpr { modifiers, .. }
            | Node::PropertyDef { modifiers, .. }
            | Node::MethodDef { modifiers, .. }
            | Node::IndexSignature { modifiers, .. }
            | Node::Interface { modifiers, .. }
            | Node::TypeAlias { modifiers, .. }
            | Node::Enum { modifiers, .. }
            | Node::Module { modifiers, .. }
            | Node::ImportEquals { modifiers, .. }
            | Node::PropertySignature { modifiers, .. }
            | Node::MethodSignature { modifiers, .. }
            | Node::Property { modifiers, .. }
            | Node::ShorthandProp { modifiers, .. }
            | Node::TypeParam { modifiers, .. } => modifiers,
            _ => Modifiers::NONE,
        }
    }
}
