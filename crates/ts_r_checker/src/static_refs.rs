//! Class type parameters referenced from static members (TS2302).
//!
//! Rule observed from tsc 7.0.2: a type reference that names a type parameter of a class,
//! anywhere inside a static property, method, accessor, index signature or static block
//! of that class (nested functions, classes and object literals included), is reported at
//! the reference.
//!
//! A type parameter of a class or interface is not in scope inside a computed name of
//! one of that type's own members (`class C<T> { [foo<T>()]() {} }`): TS2467 at the
//! reference, and the reference is the error type (no TS2304). tsc:
//! binder/nameresolver.go (the computed property name case of the scope walk).
//!
//! An `infer` type parameter is not in scope in the `extends` clause that declares it
//! (`T extends { a: infer U, b: infer V extends U }` is TS2304 for `U`), only in the true
//! branch; `infer_in_own_extends` tells the type-name resolver.

use ts_r_ast::{Modifiers, Node};
use ts_r_types::SymId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Parent steps the walk from a reference to its class follows.
const MAX_STEPS: u32 = 4096;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks type reference `r` that resolved to type parameter `sym`.
    pub(crate) fn check_static_type_parameter_use(&mut self, r: Loc, sym: SymId) {
        if self.reporting(r.file) && self.is_static_type_parameter_use(r, sym) {
            self.error(
                r,
                dg::STATIC_MEMBERS_CANNOT_REFERENCE_CLASS_TYPE_PARAMETERS_2302,
                &[],
            );
        }
        if self.reporting(r.file) && self.is_computed_name_type_parameter_use(r, sym) {
            self.error(
                r,
                dg::A_COMPUTED_PROPERTY_NAME_CANNOT_REFERENCE_A_TYPE_PARAMETER_FROM_ITS_CONTAINING_T_2467,
                &[],
            );
        }
    }

    /// `true` when type reference `r` names type parameter `sym` of a class or interface
    /// from inside the computed name of one of that type's members.
    pub(crate) fn is_computed_name_type_parameter_use(&self, r: Loc, sym: SymId) -> bool {
        let Some(owner) = self.type_param_owner(sym, true) else {
            return false;
        };
        if owner.file != r.file {
            return false;
        }
        let ast = self.ast(r.file);
        let parents = self.c.file(r.file).parents();
        let mut cur = parents.parent(r.node);
        for _ in 0..MAX_STEPS {
            let Some(n) = cur else {
                return false;
            };
            if n == owner.node {
                return false;
            }
            if matches!(ast.node(n), Node::ComputedKey { .. })
                && parents.parent(n).and_then(|m| parents.parent(m)) == Some(owner.node)
            {
                return true;
            }
            cur = parents.parent(n);
        }
        false
    }

    /// `true` when type reference `r` names type parameter `sym` of a class from inside a
    /// static member of that class. Such a reference is tsc's error type (`any`), so
    /// `static m(): List<T> { return new List<T>(null) }` reports only TS2302.
    pub(crate) fn is_static_type_parameter_use(&self, r: Loc, sym: SymId) -> bool {
        let Some(class) = self.type_param_class(sym) else {
            return false;
        };
        if class.file != r.file {
            return false;
        }
        let ast = self.ast(r.file);
        let parents = self.c.file(r.file).parents();
        let mut child = r.node;
        for _ in 0..MAX_STEPS {
            let Some(p) = parents.parent(child) else {
                return false;
            };
            if p == class.node {
                return is_static_member(ast.node(child));
            }
            child = p;
        }
        false
    }

    /// The class declaring type parameter `sym`.
    /// `true` when the type name `l` resolved to `sym`, an `infer` type parameter, from
    /// inside the `extends` clause of the conditional type that declares it.
    pub(crate) fn infer_in_own_extends(&self, l: Loc, sym: SymId) -> bool {
        if !self
            .c
            .sym_flags(sym)
            .intersects(ts_r_binder::SymbolFlags::TYPE_PARAMETER)
        {
            return false;
        }
        let Some(d) = self
            .c
            .decls(sym)
            .first()
            .copied()
            .filter(|d| d.file == l.file)
        else {
            return false;
        };
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        if !matches!(
            parents.parent(d.node).map(|p| ast.node(p)),
            Some(Node::InferType { .. })
        ) {
            return false;
        }
        let mut cur = parents.parent(d.node);
        for _ in 0..MAX_STEPS {
            let Some(n) = cur else {
                return false;
            };
            if let Node::ConditionalType { extends, .. } = *ast.node(n) {
                let es = ast.span(extends);
                let ds = self.c.span(d);
                if es.start <= ds.start && ds.end <= es.end {
                    let ls = ast.span(l.node);
                    return es.start <= ls.start && ls.end <= es.end;
                }
            }
            cur = parents.parent(n);
        }
        false
    }

    fn type_param_class(&self, sym: SymId) -> Option<Loc> {
        self.type_param_owner(sym, false)
    }

    /// The class (or, with `interfaces`, the interface) declaring type parameter `sym`.
    fn type_param_owner(&self, sym: SymId, interfaces: bool) -> Option<Loc> {
        let d = *self.c.decls(sym).first()?;
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let list = parents.parent(d.node)?;
        if !matches!(ast.node(list), Node::TypeParams { .. }) {
            return None;
        }
        let owner = parents.parent(list)?;
        match ast.node(owner) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => Some(Loc::new(d.file, owner)),
            Node::Interface { .. } if interfaces => Some(Loc::new(d.file, owner)),
            _ => None,
        }
    }
}

fn is_static_member(n: &Node) -> bool {
    match *n {
        Node::PropertyDef { modifiers, .. }
        | Node::MethodDef { modifiers, .. }
        | Node::IndexSignature { modifiers, .. } => modifiers.contains(Modifiers::STATIC),
        Node::StaticBlock { .. } => true,
        _ => false,
    }
}
