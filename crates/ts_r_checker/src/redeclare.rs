//! Redeclarations that must agree on their type or modifiers (TS2403, TS2717, TS2687).
//!
//! Rules observed from tsc 7.0.2:
//! - Every `var` declaration (and a parameter redeclared by a `var` in the body) after the
//!   first must have a type identical to the first: TS2403 at the later declaration's name,
//!   "Variable 'a' must be of type 'number', but here has type 'string'". A declaration's
//!   own type is its annotation, else its widened initializer type, else `any`; a `for-in`
//!   variable is `string` (`Extract<keyof T, string>` over a generic `T` or `this`), a
//!   `for-of` variable the iterated type. `= null` and `= undefined` initializers count as
//!   `any` (they start an evolving `any`), and so does an initializer that makes the
//!   variable's type circular (TS7022). JavaScript files too (a JSDoc `@type` is the
//!   annotation; a function initializer's type has the variable's expandos).
//! - Every property declaration merged into a symbol (interface/interface or class/interface
//!   merges) after the first must have a type identical to the first: TS2717 at the later
//!   declaration's name. Optional declarations include `undefined`.
//! - Property declarations of one instance member (class/interface merges, a parameter
//!   property beside a declared property) must agree on `private`, `protected`, `readonly`,
//!   `static`, `abstract`, `async` and `?` (`public` does not count): TS2687 at the first
//!   declaration when any other differs, and at each later one differing from the first
//!   (tsc: checkVariableLikeDeclaration and areDeclarationFlagsIdentical in checker.go).

use ts_r_ast::{Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_diagnostics::FileId;
use ts_r_types::{TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2403 for the `var` declarator `d` (only identifier names).
    pub(crate) fn check_var_redeclaration(&mut self, file: FileId, d: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let Node::VarDeclarator { name, .. } = *ast.node(d) else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let here = Loc::new(file, name);
        let Some(sym) = self.c.sym_of_decl(here) else {
            return;
        };
        let flags = self.c.sym_flags(sym);
        if !flags.contains(SymbolFlags::FUNCTION_SCOPED_VARIABLE)
            || flags.intersects(
                SymbolFlags::BLOCK_SCOPED_VARIABLE
                    | SymbolFlags::FUNCTION
                    | SymbolFlags::CLASS
                    | SymbolFlags::ENUM
                    | SymbolFlags::VALUE_MODULE
                    | SymbolFlags::ALIAS
                    | SymbolFlags::PROPERTY,
            )
        {
            return;
        }
        // The first variable-like declaration (a merged `interface Symbol` comes first in
        // the lib, `declare var Symbol` is the one compared).
        let decls: Vec<Loc> = self
            .c
            .decls(sym)
            .into_iter()
            .filter(|d| {
                let ast = self.ast(d.file);
                match *ast.node(d.node) {
                    Node::Param { .. } => true,
                    Node::Ident { .. } => self.c.parent(*d).is_some_and(|p| {
                        matches!(ast.node(p.node), Node::VarDeclarator { name, .. } if *name == d.node)
                    }),
                    _ => false,
                }
            })
            .collect();
        let Some(&first) = decls.first() else {
            return;
        };
        if decls.len() < 2 || first == here || !decls.contains(&here) {
            return;
        }
        let Some(ft) = self.own_var_type(first, true) else {
            return;
        };
        // A later declaration mentioning its own name reads the symbol's type, given by the
        // first declaration; only a parameter's annotation is taken as certain here (a
        // variable's references may be narrowed by assignments modelled approximately).
        let first_is_param = matches!(self.ast(first.file).node(first.node), Node::Param { .. });
        let Some(ht) = self.own_var_type(here, !first_is_param) else {
            return;
        };
        // An annotated type comes from syntax, not inference.
        let exact = (self.var_annotated(first), self.var_annotated(here));
        if !self.types_differ_for_redeclaration(ft, ht, exact) {
            return;
        }
        let n = self.c.sym_name(sym).to_owned();
        let fs = self.type_to_string(ft);
        let hs = self.type_to_string(ht);
        self.error(
            here,
            dg::SUBSEQUENT_VARIABLE_DECLARATIONS_MUST_HAVE_THE_SAME_TYPE_VARIABLE_0_MUST_BE_OF_T_2403,
            &[&n, &fs, &hs],
        );
    }

    /// TS2717 for the interface property signature or class property `m`.
    pub(crate) fn check_property_redeclaration(&mut self, file: FileId, m: NodeId) {
        if !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let here = Loc::new(file, m);
        let Some(sym) = self.c.sym_of_decl(here) else {
            return;
        };
        let flags = self.c.sym_flags(sym);
        if !flags.contains(SymbolFlags::PROPERTY)
            || flags.intersects(SymbolFlags::METHOD | SymbolFlags::ACCESSOR)
        {
            return;
        }
        let decls = self.c.decls(sym);
        let Some(&first) = decls.first() else {
            return;
        };
        let (ft, ht) = if decls.len() < 2 || first == here || !decls.contains(&here) {
            // A class field repeated in one body (or after an accessor) joins the earlier
            // member's symbol in tsc; this binder reports the repeat and keeps it apart.
            let Some(earlier) = self.class_member_merged_into(file, m) else {
                return;
            };
            let (Some(es), Some(hs)) = (
                self.c.sym_of_decl(Loc::new(file, earlier)),
                self.c.sym_of_decl(here),
            ) else {
                return;
            };
            (self.type_of_symbol(es), self.type_of_symbol(hs))
        } else {
            let Some(ft) = self.own_property_type(first) else {
                return;
            };
            let Some(ht) = self.own_property_type(here) else {
                return;
            };
            (ft, ht)
        };
        if !self.types_differ_for_redeclaration(ft, ht, (false, false)) {
            return;
        }
        let ast = self.ast(file);
        let key = match *ast.node(m) {
            Node::PropertySignature { key, .. } | Node::PropertyDef { key, .. } => key,
            _ => return,
        };
        // The property is named as this declaration writes it (`1.0`, `[c1]`).
        let n = self.c.text(Loc::new(file, key)).to_owned();
        let fs = self.type_to_string(ft);
        let hs = self.type_to_string(ht);
        self.error(
            Loc::new(file, key),
            dg::SUBSEQUENT_PROPERTY_DECLARATIONS_MUST_HAVE_THE_SAME_TYPE_PROPERTY_0_MUST_BE_OF_T_2717,
            &[&n, &fs, &hs],
        );
    }

    /// The earlier member of the same class whose symbol the field `m` merges into under
    /// tsc's class member excludes: a field merges with fields and accessors before it, a
    /// method only with methods, a getter only with a setter and a setter only with a getter;
    /// a member that conflicts starts a new symbol. `None` when `m` starts its own symbol.
    fn class_member_merged_into(&self, file: FileId, m: NodeId) -> Option<NodeId> {
        const FIELD: u8 = 1;
        const METHOD: u8 = 2;
        const GET: u8 = 4;
        const SET: u8 = 8;
        const ALL: u8 = FIELD | METHOD | GET | SET;
        let ast = self.ast(file);
        let Node::PropertyDef { key, modifiers, .. } = *ast.node(m) else {
            return None;
        };
        let name = crate::nodes::key_text(ast, key)?;
        let is_static = modifiers.contains(ts_r_ast::Modifiers::STATIC);
        let class = self.c.parent(Loc::new(file, m))?;
        let members = match *ast.node(class.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return None,
        };
        // (first member of the current symbol, kinds merged into it)
        let mut cur: Option<(NodeId, u8)> = None;
        for &x in ast.list(members) {
            let (k, mods, kind, excl) = match *ast.node(x) {
                Node::PropertyDef { key, modifiers, .. } => {
                    if modifiers.contains(ts_r_ast::Modifiers::ACCESSOR) {
                        (key, modifiers, FIELD, ALL)
                    } else {
                        (key, modifiers, FIELD, METHOD)
                    }
                }
                Node::MethodDef {
                    key,
                    modifiers,
                    kind,
                    ..
                } => match kind {
                    ts_r_ast::MethodKind::Method => (key, modifiers, METHOD, FIELD | GET | SET),
                    ts_r_ast::MethodKind::Get => (key, modifiers, GET, FIELD | METHOD | GET),
                    ts_r_ast::MethodKind::Set => (key, modifiers, SET, FIELD | METHOD | SET),
                    ts_r_ast::MethodKind::Constructor => continue,
                },
                _ => continue,
            };
            if mods.contains(ts_r_ast::Modifiers::STATIC) != is_static
                || crate::nodes::key_text(ast, k).as_deref() != Some(name.as_str())
            {
                continue;
            }
            cur = match cur {
                Some((first, kinds)) if kinds & excl == 0 => Some((first, kinds | kind)),
                _ => Some((x, kind)),
            };
            if x == m {
                break;
            }
        }
        match cur {
            Some((first, _)) if first != m => Some(first),
            _ => None,
        }
    }

    /// TS2687 for the property-like declaration `m` (a class property, property signature or
    /// parameter property) when its symbol has another property-like declaration whose
    /// `private`/`protected`/`readonly`/`static`/`abstract`/`async` modifiers or `?` differ:
    /// the first declaration is compared with every other one, later ones with the first
    /// (tsc: checker.go, checkVariableLikeDeclaration).
    pub(crate) fn check_identical_modifiers(&mut self, file: FileId, m: NodeId) {
        if !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let here = Loc::new(file, m);
        let decls: Vec<Loc> = self
            .merged_member_decls(here)
            .into_iter()
            .filter(|&d| self.property_like_flags(d).is_some())
            .collect();
        if decls.len() < 2 {
            return;
        }
        let Some(mine) = self.property_like_flags(here) else {
            return;
        };
        let differs = if decls[0] == here {
            decls[1..]
                .iter()
                .any(|&d| self.property_like_flags(d) != Some(mine))
        } else {
            decls.contains(&here) && self.property_like_flags(decls[0]) != Some(mine)
        };
        if !differs {
            return;
        }
        let ast = self.ast(file);
        let name = match *ast.node(m) {
            Node::PropertySignature { key, .. } | Node::PropertyDef { key, .. } => key,
            Node::Param { pattern, .. } => pattern,
            _ => return,
        };
        let shown = self.c.text(Loc::new(file, name)).to_owned();
        self.error(
            Loc::new(file, name),
            dg::ALL_DECLARATIONS_OF_0_MUST_HAVE_IDENTICAL_MODIFIERS_2687,
            &[&shown],
        );
    }

    /// Every declaration of the instance member that the property-like declaration `m`
    /// declares, across the declarations of its class or interface (each has its own member
    /// table here).
    fn merged_member_decls(&self, m: Loc) -> Vec<Loc> {
        let ast = self.ast(m.file);
        let (key, mut container) = match *ast.node(m.node) {
            Node::PropertySignature { key, .. } => (key, self.c.parent(m)),
            Node::PropertyDef { key, modifiers, .. } => {
                if modifiers.contains(ts_r_ast::Modifiers::STATIC) {
                    return Vec::new();
                }
                (key, self.c.parent(m))
            }
            // A parameter property: parameter, constructor function, constructor, class.
            Node::Param { pattern, .. } => {
                let ctor = self.c.parent(m).and_then(|f| self.c.parent(f));
                (pattern, ctor.and_then(|c| self.c.parent(c)))
            }
            _ => return Vec::new(),
        };
        let Some(name) = crate::nodes::key_text(ast, key) else {
            return Vec::new();
        };
        // Skip list wrappers between the member and its container, if any.
        for _ in 0..2 {
            match container {
                Some(c)
                    if !matches!(
                        self.ast(c.file).node(c.node),
                        Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
                    ) =>
                {
                    container = self.c.parent(c);
                }
                _ => break,
            }
        }
        let Some(c) = container.filter(|c| {
            matches!(
                self.ast(c.file).node(c.node),
                Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
            )
        }) else {
            return Vec::new();
        };
        let Some(sym) = self.c.sym_of_decl(c) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (f, t) in self.c.member_tables(sym) {
            let fl = self.c.file(f);
            if let Some(x) = fl.bound.lookup_str(&fl.ast, t, &name) {
                out.extend(fl.bound.decls(x).iter().map(|&d| Loc::new(f, d)));
            }
        }
        // A parameter property duplicating a declared property is left out of the member
        // table; it still counts as a declaration of the member.
        for d in self.c.decls(sym) {
            let dast = self.ast(d.file);
            let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) =
                *dast.node(d.node)
            else {
                continue;
            };
            for &mem in dast.list(members) {
                let Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Constructor,
                    value,
                    ..
                } = *dast.node(mem)
                else {
                    continue;
                };
                let Some(parts) = crate::nodes::fn_parts(dast, value) else {
                    continue;
                };
                for &p in dast.list(parts.params) {
                    if let Node::Param {
                        modifiers, pattern, ..
                    } = *dast.node(p)
                        && !modifiers.is_empty()
                        && crate::nodes::ident_text(dast, pattern) == Some(name.as_str())
                    {
                        out.push(Loc::new(d.file, p));
                    }
                }
            }
        }
        out.sort_unstable_by_key(|l| (l.file, l.node));
        out.dedup();
        out
    }

    /// The modifiers compared between merged property declarations, with `?`.
    fn property_like_flags(&self, d: Loc) -> Option<(ts_r_ast::Modifiers, bool)> {
        use ts_r_ast::Modifiers as M;
        let ast = self.ast(d.file);
        let (mods, optional) = match *ast.node(d.node) {
            Node::PropertySignature {
                modifiers,
                optional,
                ..
            }
            | Node::PropertyDef {
                modifiers,
                optional,
                ..
            } => (modifiers, optional),
            Node::Param {
                modifiers,
                optional,
                ..
            } if modifiers
                .intersects(M::PUBLIC | M::PRIVATE | M::PROTECTED | M::READONLY | M::OVERRIDE) =>
            {
                (modifiers, optional)
            }
            _ => return None,
        };
        let interesting =
            M::PRIVATE | M::PROTECTED | M::ASYNC | M::ABSTRACT | M::READONLY | M::STATIC;
        Some((M(mods.0 & interesting.0), optional))
    }

    /// The type a single variable declaration (`Ident` binding or parameter) gives its symbol;
    /// `check_circular` skips declarations whose annotation or initializer mention their name.
    fn own_var_type(&mut self, d: Loc, check_circular: bool) -> Option<TypeId> {
        let ast = self.ast(d.file);
        let t = match *ast.node(d.node) {
            // Unannotated parameters take contextual types (not compared), except a class
            // method's parameter defaulting to `this` (the class's `this` type).
            Node::Param {
                type_ann: None,
                init: Some(i),
                ..
            } if matches!(ast.node(crate::nodes::skip_parens(ast, i)), Node::This {})
                && self.in_class_method(d) =>
            {
                self.param_type(d)
            }
            Node::Param { type_ann: None, .. } => return None,
            Node::Param { .. } => self.param_type(d),
            Node::Ident { .. } => {
                let parent = self.c.parent(d)?;
                let Node::VarDeclarator {
                    name,
                    type_ann,
                    init,
                    ..
                } = *ast.node(parent.node)
                else {
                    return None;
                };
                if name != d.node {
                    return None;
                }
                // Circular references (`var f: Array<typeof f>`, `var x = '' && x`) make
                // the variable `any` in tsc: not compared. A later declaration's references
                // see the type the first declaration gave the symbol.
                let own = ast_ident_text(ast, name);
                if check_circular
                    && [type_ann, init]
                        .into_iter()
                        .flatten()
                        .any(|n| mentions_ident(ast, n, own))
                {
                    // An initializer whose variable type is circular through variables
                    // alone (TS7022) makes the variable `any`, compared as such.
                    return (type_ann.is_none() && self.var_type_circular(d))
                        .then_some(TypeId::ANY);
                }
                if type_ann.is_none()
                    && let Some(i) = init
                {
                    let i = crate::nodes::skip_parens(ast, i);
                    match ast.node(i) {
                        Node::NullLit {} => return Some(TypeId::ANY),
                        Node::Ident { .. } if self.c.text(Loc::new(d.file, i)) == "undefined" => {
                            return Some(TypeId::ANY);
                        }
                        _ => {}
                    }
                }
                let mut t = self.binding_identifier_type(d);
                // A JavaScript function initializer carries the variable's expandos
                // (`var x = function () {}; x.a = 1` is `{ (): void; a: number; }`).
                if type_ann.is_none()
                    && self.c.file(d.file).is_js()
                    && let Some(i) = init
                    && matches!(
                        ast.node(crate::nodes::skip_parens(ast, i)),
                        Node::FunctionExpr { .. } | Node::Arrow { .. }
                    )
                    && let Some(sym) = self.c.sym_of_decl(d)
                {
                    t = self.with_expando_props(sym, t);
                }
                // `var p = this` is the `this` type, compared like any other type.
                if type_ann.is_none()
                    && let Some(i) = init
                    && matches!(ast.node(crate::nodes::skip_parens(ast, i)), Node::This {})
                    && t != TypeId::UNRESOLVED
                    && !self.store().is_error(t)
                    && !self.store().flags(t).intersects(TypeFlags::ANY)
                {
                    return Some(t);
                }
                // An initializer's type can be a narrowed literal (`var n = s` after
                // `var s: boolean = false`) where tsc uses the declared type: not compared.
                // An initializer typed `any` may be a contextual type not modelled here.
                // An enum member (`E.a`) is never narrowed.
                if type_ann.is_none()
                    && let Some(i) = init
                    && self.is_enum_member_access(Loc::new(d.file, i))
                    && t != TypeId::UNRESOLVED
                {
                    return Some(t);
                }
                if type_ann.is_none()
                    && init.is_some()
                    && (self
                        .store()
                        .flags(t)
                        .intersects(TypeFlags::UNIT | TypeFlags::ANY)
                        || t == TypeId::UNRESOLVED
                        || self.could_contain_type_params(t) && self.in_generic_scope(d))
                {
                    return None;
                }
                t
            }
            _ => return None,
        };
        Some(if t == TypeId::AUTO { TypeId::ANY } else { t })
    }

    /// Whether `d` may see type parameters other than the ones its own initializer declares
    /// (inside a generic function, a class or an interface, or anywhere in a JavaScript
    /// file, where JSDoc templates are not looked for). Outside one, a generic initializer
    /// (`var v = <T>() => 1`) has a fixed type that is compared like any other.
    fn in_generic_scope(&self, d: Loc) -> bool {
        if self.c.file(d.file).is_js() {
            return true;
        }
        let ast = self.ast(d.file);
        let mut cur = self.c.parent(d);
        let mut guard = 0u32;
        while let Some(p) = cur {
            guard += 1;
            if guard > 10_000 {
                return true;
            }
            match *ast.node(p.node) {
                Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::Interface { .. }
                | Node::TypeAlias { .. } => return true,
                _ => {}
            }
            if matches!(
                ast.node(p.node),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            ) && crate::nodes::fn_parts(ast, p.node).is_some_and(|f| f.type_params.is_some())
            {
                return true;
            }
            cur = self.c.parent(p);
        }
        false
    }

    /// Whether the type of the variable declared at `d` is circular through the types of
    /// variables alone.
    fn var_type_circular(&mut self, d: Loc) -> bool {
        let Some(sym) = self.c.sym_of_decl(d) else {
            return false;
        };
        self.type_of_symbol(sym);
        if !self
            .circular
            .contains(&crate::checker::Resolving::Type(sym))
        {
            return false;
        }
        let path = self.type_cycles.get(&sym).cloned().unwrap_or_default();
        !path.is_empty()
            && path.iter().all(|r| {
                matches!(*r, crate::checker::Resolving::Type(s) if self.c.sym_flags(s).intersects(SymbolFlags::VARIABLE)
                    && !self.c.sym_flags(s).intersects(SymbolFlags::ACCESSOR | SymbolFlags::METHOD | SymbolFlags::FUNCTION))
            })
    }

    /// Whether `e` is `E.a` for an enum `E`.
    fn is_enum_member_access(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let Node::Member { obj, .. } = *ast.node(crate::nodes::skip_parens(ast, e.node)) else {
            return false;
        };
        if !matches!(ast.node(obj), Node::Ident { .. }) {
            return false;
        }
        self.c
            .resolve_ident(Loc::new(e.file, obj), ts_r_binder::meaning::VALUE)
            .is_some_and(|s| self.c.sym_flags(s).intersects(SymbolFlags::ENUM))
    }

    /// Whether the parameter `p` belongs to a method or accessor of a class.
    fn in_class_method(&self, p: Loc) -> bool {
        let Some(f) = self.c.parent(p) else {
            return false;
        };
        let Some(m) = self.c.parent(f) else {
            return false;
        };
        let ast = self.ast(p.file);
        if !matches!(ast.node(m.node), Node::MethodDef { .. }) {
            return false;
        }
        self.c.parent(m).is_some_and(|c| {
            matches!(
                ast.node(c.node),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            )
        })
    }

    /// The type a single property declaration gives its symbol; `None` when it comes from
    /// an initializer (not compared).
    fn own_property_type(&mut self, d: Loc) -> Option<TypeId> {
        let ast = self.ast(d.file);
        let (optional, type_ann) = match *ast.node(d.node) {
            Node::PropertySignature {
                optional, type_ann, ..
            } => (optional, type_ann),
            Node::PropertyDef {
                optional,
                type_ann,
                value,
                ..
            } => {
                if type_ann.is_none() && value.is_some() {
                    return None;
                }
                (optional, type_ann)
            }
            _ => return None,
        };
        let t = match type_ann {
            Some(ta) => self.type_from_node(Loc::new(d.file, ta)),
            None => TypeId::ANY,
        };
        Some(if optional { self.add_optionality(t) } else { t })
    }

    /// Whether two declarations' types differ (not identical). Conservative where this
    /// branch's types are approximate: unresolved types, enums and mutually assignable
    /// pairs that print an `any` (an unresolved part prints as `any`) count as the same.
    /// Two function types whose single call signatures differ in type parameter count or
    /// constraints, or in parameter count, are certainly not identical.
    fn function_shapes_differ(&mut self, a: TypeId, b: TypeId) -> bool {
        let sa = self.signatures_of(a, crate::members::SigKind::Call);
        let sb = self.signatures_of(b, crate::members::SigKind::Call);
        sa.len() == 1 && sb.len() == 1 && !self.signature_shapes_identical(sa[0], sb[0])
    }

    fn var_annotated(&self, d: Loc) -> bool {
        let ast = self.ast(d.file);
        match *ast.node(d.node) {
            Node::Param { type_ann, .. } => type_ann.is_some(),
            _ => self.c.parent(d).is_some_and(|p| {
                matches!(
                    *ast.node(p.node),
                    Node::VarDeclarator {
                        type_ann: Some(_),
                        ..
                    }
                )
            }),
        }
    }

    /// `exact`: whether each type is a written annotation, so `any` in it is meant.
    fn types_differ_for_redeclaration(
        &mut self,
        a: TypeId,
        b: TypeId,
        exact: (bool, bool),
    ) -> bool {
        if a == b {
            return false;
        }
        for t in [a, b] {
            if t == TypeId::UNRESOLVED || self.store().is_error(t) {
                return false;
            }
        }
        // Enum types are approximate here: they differ only when not mutually assignable.
        if self.mentions_enum(a) || self.mentions_enum(b) {
            return !(self.is_assignable(a, b) && self.is_assignable(b, a));
        }
        if self.is_identical(a, b) {
            return false;
        }
        let (fa, fb) = (self.store().flags(a), self.store().flags(b));
        if fa.contains(TypeFlags::ANY) && fb.contains(TypeFlags::ANY) {
            return false;
        }
        // `symbol` and `unique symbol` declarations of one property agree.
        if (fa | fb).intersects(TypeFlags::UNIQUE_ES_SYMBOL) {
            return false;
        }
        if fa.contains(TypeFlags::ANY) || fb.contains(TypeFlags::ANY) {
            return true;
        }
        // A resolved mapped type (`Required<{ a?: string }>`) is identical to the object type
        // it produces, which the identity check doesn't see, and its members may be
        // approximate (`-?` and explicit `undefined`): not compared.
        let mapped = |ck: &Self, t: TypeId| {
            matches!(
                ck.store().object_of(t).map(|o| o.kind),
                Some(ts_r_types::ObjectKind::Mapped { .. })
            )
        };
        if mapped(self, a) || mapped(self, b) {
            return false;
        }
        // Conditional, template literal and string mapping types (also as union or
        // intersection members) are not reduced the way tsc reduces them: not compared.
        let approximate = |ck: &Self, t: TypeId| {
            let f =
                TypeFlags::CONDITIONAL | TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING;
            ck.store().flags(t).intersects(f)
                || ck
                    .store()
                    .members_of_union(t)
                    .iter()
                    .any(|&m| ck.store().flags(m).intersects(f))
        };
        // A deferred generic conditional (`Extract<keyof T, string>` of a `for-in` variable)
        // is never identical to a type without type parameters.
        for (x, y) in [(a, b), (b, a)] {
            if self.store().flags(x).contains(TypeFlags::CONDITIONAL)
                && self.could_contain_type_params(x)
                && !self.could_contain_type_params(y)
                && !approximate(self, y)
            {
                return true;
            }
        }
        if approximate(self, a) || approximate(self, b) {
            return false;
        }
        if self.is_assignable(a, b) && self.is_assignable(b, a) {
            // Unions of equivalent members (inferred object literals printing alike) are not
            // deduplicated the way tsc does it.
            for t in [a, b] {
                let members = self.store().members_of_union(t).to_vec();
                if members.len() > 1 && self.store().flags(t).contains(TypeFlags::UNION) {
                    let first = self.type_to_string(members[0]);
                    if members[1..]
                        .iter()
                        .all(|&m| self.type_to_string(m) == first)
                    {
                        return false;
                    }
                }
            }
            // References to two different generic interfaces or classes compare reliably
            // for identity.
            if let (Some((ta, _)), Some((tb, _))) = (self.generic_args(a), self.generic_args(b))
                && ta != tb
            {
                return true;
            }
            // Inference may give `any` where tsc has a precise type (and other types where
            // tsc has `any`): unless both sides are written, `any` is not compared.
            let sa = self.type_to_string(a);
            let sb = self.type_to_string(b);
            if (sa.contains("any") || sb.contains("any"))
                && !(exact.0 && exact.1)
                && !self.function_shapes_differ(a, b)
            {
                return false;
            }
        }
        true
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS1165/1166/1168/1169/1170: a computed member name in a class property, method
    /// overload, interface or type literal must be a literal or an entity name (`k`,
    /// `Symbol.iterator`); `[f()]`, `[a + b]`, `[(k)]` and `[this.x]` are reported at the key.
    /// The key's type is not checked (`[s]` with `s: string` is allowed).
    pub(crate) fn check_computed_member_names(&mut self, d: Loc) {
        if !self.reporting(d.file) || self.c.file(d.file).is_js() {
            return;
        }
        let ast = self.ast(d.file);
        let (members, kind) = match *ast.node(d.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => (members, 0),
            Node::Interface { body, .. } => (body, 1),
            Node::TypeLiteral { members } => (members, 2),
            _ => return,
        };
        for &m in ast.list(members) {
            let (key, msg) = match (*ast.node(m), kind) {
                (Node::PropertyDef { key, .. }, 0) => (
                    key,
                    dg::A_COMPUTED_PROPERTY_NAME_IN_A_CLASS_PROPERTY_DECLARATION_MUST_HAVE_A_SIMPLE_LITE_1166,
                ),
                (
                    Node::MethodDef {
                        key,
                        value,
                        modifiers,
                        kind: ts_r_ast::MethodKind::Method,
                        ..
                    },
                    0,
                ) => {
                    if crate::nodes::fn_parts(ast, value).is_none_or(|p| p.body.is_some())
                        || modifiers.contains(ts_r_ast::Modifiers::ABSTRACT)
                    {
                        continue;
                    }
                    if self.is_ambient_node(Loc::new(d.file, m)) {
                        (
                            key,
                            dg::A_COMPUTED_PROPERTY_NAME_IN_AN_AMBIENT_CONTEXT_MUST_REFER_TO_AN_EXPRESSION_WHOSE_1165,
                        )
                    } else {
                        (
                            key,
                            dg::A_COMPUTED_PROPERTY_NAME_IN_A_METHOD_OVERLOAD_MUST_REFER_TO_AN_EXPRESSION_WHOSE_T_1168,
                        )
                    }
                }
                // Accessor signatures are not modelled.
                (
                    Node::MethodSignature {
                        kind: ts_r_ast::MethodKind::Get | ts_r_ast::MethodKind::Set,
                        ..
                    },
                    _,
                ) => continue,
                (Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. }, 1) => (
                    key,
                    dg::A_COMPUTED_PROPERTY_NAME_IN_AN_INTERFACE_MUST_REFER_TO_AN_EXPRESSION_WHOSE_TYPE_I_1169,
                ),
                (Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. }, 2) => (
                    key,
                    dg::A_COMPUTED_PROPERTY_NAME_IN_A_TYPE_LITERAL_MUST_REFER_TO_AN_EXPRESSION_WHOSE_TYP_1170,
                ),
                _ => continue,
            };
            let Node::ComputedKey { expr } = *ast.node(key) else {
                continue;
            };
            if crate::nodes::is_literal_computed_key(ast, key)
                || crate::nodes::is_entity_name_expr(ast, expr)
            {
                continue;
            }
            // A member reports one grammar error (a misplaced decorator wins); an `await`
            // or `yield` in its initializer (TS1308, TS1163) is not the member's error.
            let span = ast.span(m);
            if self.diags.iter().any(|g| {
                g.file == d.file
                    && (1000..2000).contains(&g.code)
                    && !matches!(g.code, 1163 | 1308)
                    && span.start <= g.span.start
                    && g.span.start < span.end
            }) {
                continue;
            }
            self.error(Loc::new(d.file, key), msg, &[]);
        }
    }

    /// TS2300 (and TS2717) for properties of one class, interface or type literal whose
    /// names collide through a computed key (`[Symbol.iterator]` twice, `[k]` beside `key`
    /// for `const k = "key"`). Every member of such a group is reported at its key; plain
    /// duplicates without a computed member are the binder's. Only groups of one member
    /// kind are reported: properties, getters or setters (two `get [Symbol.hasInstance]()`);
    /// methods (overloads) and get/set pairs are left alone.
    pub(crate) fn check_late_bound_duplicates(&mut self, d: Loc) {
        if !self.reporting(d.file) || self.c.file(d.file).is_js() {
            return;
        }
        let ast = self.ast(d.file);
        let members = match *ast.node(d.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            Node::Interface { body, .. } => body,
            Node::TypeLiteral { members } => members,
            _ => return,
        };
        let list = ast.list(members);
        let has_computed = list.iter().any(|&m| match *ast.node(m) {
            Node::PropertyDef { key, .. }
            | Node::MethodDef { key, .. }
            | Node::PropertySignature { key, .. }
            | Node::MethodSignature { key, .. } => {
                matches!(ast.node(key), Node::ComputedKey { .. })
                    && crate::nodes::key_text(ast, key).is_none()
            }
            _ => false,
        });
        if !has_computed {
            return;
        }
        // (static, name, late-bound, member kind, member, key); kinds: 0 property,
        // 1 method, 2 getter, 3 setter.
        let mut named: Vec<(bool, String, bool, u8, NodeId, NodeId)> = Vec::new();
        for &m in list {
            let (key, is_static, is_prop) = match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. } => {
                    (key, modifiers.contains(ts_r_ast::Modifiers::STATIC), 0)
                }
                Node::MethodDef {
                    key,
                    modifiers,
                    kind,
                    ..
                } => (
                    key,
                    modifiers.contains(ts_r_ast::Modifiers::STATIC),
                    accessor_kind(kind),
                ),
                Node::PropertySignature { key, .. } => (key, false, 0),
                Node::MethodSignature { key, kind, .. } => (key, false, accessor_kind(kind)),
                _ => continue,
            };
            let (name, late) = match crate::nodes::key_text(ast, key) {
                Some(t) => (t, false),
                None => {
                    let Node::ComputedKey { expr } = *ast.node(key) else {
                        continue;
                    };
                    if !crate::nodes::is_entity_name_expr(ast, expr) {
                        continue;
                    }
                    match self.late_bound_name(Loc::new(d.file, expr)) {
                        Some(n) => (n, true),
                        None => continue,
                    }
                }
            };
            named.push((is_static, name, late, is_prop, m, key));
        }
        // A static method or accessor late-bound to `prototype` collides with the class's
        // own `prototype` property (a static property of that name merges with it).
        if matches!(
            ast.node(d.node),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        ) {
            for &(is_static, ref name, late, is_prop, _, key) in &named {
                if is_static && late && is_prop != 0 && name == "prototype" {
                    let shown = self.c.text(Loc::new(d.file, key)).to_owned();
                    self.error(
                        Loc::new(d.file, key),
                        dg::DUPLICATE_IDENTIFIER_0_2300,
                        &[&shown],
                    );
                }
            }
        }
        let mut order: Vec<usize> = (0..named.len()).collect();
        order.sort_by(|&a, &b| (named[a].0, &named[a].1, a).cmp(&(named[b].0, &named[b].1, b)));
        let mut i = 0;
        while i < order.len() {
            let mut j = i + 1;
            while j < order.len()
                && named[order[j]].0 == named[order[i]].0
                && named[order[j]].1 == named[order[i]].1
            {
                j += 1;
            }
            let group = &order[i..j];
            i = j;
            if group.len() < 2
                || !group.iter().any(|&g| named[g].2)
                || named[group[0]].3 == 1
                || !group.iter().all(|&g| named[g].3 == named[group[0]].3)
            {
                continue;
            }
            let plain = group.iter().filter(|&&g| !named[g].2).count();
            // A group with a plain name shows that name; one of computed keys only shows the
            // first key as written (`[k]`, `[Symbol.iterator]`).
            let shown = if plain > 0 {
                named[group[0]].1.clone()
            } else {
                self.c.text(Loc::new(d.file, named[group[0]].5)).to_owned()
            };
            for &g in group {
                // Plain duplicates among themselves are already reported by the binder.
                if !named[g].2 && plain > 1 {
                    continue;
                }
                self.error(
                    Loc::new(d.file, named[g].5),
                    dg::DUPLICATE_IDENTIFIER_0_2300,
                    &[&shown],
                );
            }
            // Later declarations must agree with the first on their type.
            if named[group[0]].3 != 0 {
                continue;
            }
            let first = Loc::new(d.file, named[group[0]].4);
            let Some(ft) = self.own_property_type(first) else {
                continue;
            };
            for &g in &group[1..] {
                if !named[g].2 && !named[group[0]].2 {
                    continue;
                }
                let Some(ht) = self.own_property_type(Loc::new(d.file, named[g].4)) else {
                    continue;
                };
                if !self.types_differ_for_redeclaration(ft, ht, (false, false)) {
                    continue;
                }
                let fs = self.type_to_string(ft);
                let hs = self.type_to_string(ht);
                // The property is named as this declaration writes it.
                let here = if named[g].2 {
                    self.c.text(Loc::new(d.file, named[g].5)).to_owned()
                } else {
                    named[g].1.clone()
                };
                self.error(
                    Loc::new(d.file, named[g].5),
                    dg::SUBSEQUENT_PROPERTY_DECLARATIONS_MUST_HAVE_THE_SAME_TYPE_PROPERTY_0_MUST_BE_OF_T_2717,
                    &[&here, &fs, &hs],
                );
            }
        }
    }
}

/// The member kind code of a method-like member: 1 method, 2 getter, 3 setter.
fn accessor_kind(kind: ts_r_ast::MethodKind) -> u8 {
    match kind {
        ts_r_ast::MethodKind::Get => 2,
        ts_r_ast::MethodKind::Set => 3,
        _ => 1,
    }
}

fn ast_ident_text(ast: &ts_r_ast::Ast, n: NodeId) -> &str {
    crate::nodes::ident_text(ast, n).unwrap_or("")
}

/// Whether the subtree at `root` contains an identifier spelled `name` (bounded walk).
fn mentions_ident(ast: &ts_r_ast::Ast, root: NodeId, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut stack = vec![root];
    let mut budget = 10_000u32;
    while let Some(n) = stack.pop() {
        budget = budget.saturating_sub(1);
        if budget == 0 {
            return true;
        }
        if crate::nodes::ident_text(ast, n) == Some(name) {
            return true;
        }
        // Parameter and property names declare, they don't refer (`var x: (x: T) => T`).
        let skip = match *ast.node(n) {
            Node::Param { pattern, .. } => Some(pattern),
            Node::PropertySignature { key, .. } => Some(key),
            _ => None,
        }
        .filter(|&k| matches!(ast.node(k), Node::Ident { .. }));
        ast.for_each_child(n, |c| {
            if Some(c) != skip {
                stack.push(c);
            }
        });
    }
    false
}
