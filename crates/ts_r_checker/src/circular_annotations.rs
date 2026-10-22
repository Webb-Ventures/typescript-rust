//! Declarations whose type annotation (TS2502) or initializer (TS7022) depends on their
//! own type.
//!
//! Rules observed from tsc 7.0.2: a variable, parameter, property or accessor with a type
//! annotation whose type, when resolved, needs the type of the symbol itself
//! (`var a: typeof a`, `var d: Array<typeof d>`, `p: T["p"]`, `get foo(): typeof c.foo`,
//! or through another declaration: `var b: typeof c; var c: typeof b`) is reported at its
//! name, for each declaration on the cycle. Members of an object type literal are not
//! resolved with it (`var e: { x: typeof e }` is fine). An accessor pair's type comes from
//! the annotated getter (else the setter's parameter), which is the declaration reported.
//!
//! Under `noImplicitAny` an unannotated variable whose initializer needs its own type
//! (`var a = a`, `var b = { x: b }`, `var n = o; var o = n`) is TS7022 at its name. Our
//! function bodies, contextual types and loop flow resolve variable types in a different
//! order from tsc's, so only cycles made of variable types alone (no accessor, method or
//! function symbol on the path) are reported, for declarations outside loops whose
//! initializer has no function, method, class or `this`, and class properties (any key
//! but a computed non-literal one) whose initializer has no function, method or class, and
//! `for`-`in`/`of` variables whose iterated expression has none either.
//!
//! The symbol type resolution stack (`Ck::push_resolution`) records the cycles; this check
//! resolves the declaration's symbol and reports when its resolution was circular.

use ts_r_ast::{MethodKind, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc, Resolving};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the annotated declaration `n` for a circular type annotation (TS2502).
    pub(crate) fn check_circular_annotation(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        // Unannotated declarations with an initializer are TS7022 under `noImplicitAny`.
        let mut implicit = false;
        let for_of = self
            .c
            .parent(Loc::new(file, n))
            .and_then(|list| self.c.parent(list))
            .and_then(|h| match *ast.node(h.node) {
                Node::ForOf { right, .. } | Node::ForIn { right, .. } => Some(right),
                _ => None,
            });
        let name = match *ast.node(n) {
            Node::VarDeclarator {
                name,
                type_ann: Some(_),
                ..
            } => name,
            Node::VarDeclarator {
                name,
                type_ann: None,
                init: Some(init),
                ..
            } if self.c.opts.no_implicit_any
                && !self.in_loop(file, n)
                && !has_function(ast, init, true) =>
            {
                implicit = true;
                name
            }
            // A `for`-`of` variable is typed by its iterable; a `for`-`in` one checks its
            // object.
            Node::VarDeclarator {
                name,
                type_ann: None,
                init: None,
                ..
            } if self.c.opts.no_implicit_any
                && for_of.is_some_and(|right| !has_function(ast, right, true)) =>
            {
                implicit = true;
                name
            }
            Node::Param {
                pattern,
                type_ann: Some(_),
                ..
            } => pattern,
            // A class property's initializer reads its siblings through `this`.
            Node::PropertyDef {
                key,
                type_ann: None,
                value: Some(init),
                ..
            } if self.c.opts.no_implicit_any && !has_function(ast, init, false) => {
                implicit = true;
                key
            }
            Node::PropertyDef {
                key,
                type_ann: Some(_),
                ..
            }
            | Node::PropertySignature {
                key,
                type_ann: Some(_),
                ..
            } => key,
            Node::MethodDef {
                kind: kind @ (MethodKind::Get | MethodKind::Set),
                key,
                ..
            }
            | Node::MethodSignature {
                kind: kind @ (MethodKind::Get | MethodKind::Set),
                key,
                ..
            } => {
                if !self.accessor_has_annotation(file, n, kind) {
                    return;
                }
                key
            }
            _ => return,
        };
        let property = implicit && matches!(ast.node(n), Node::PropertyDef { .. });
        let named = match ast.node(name) {
            Node::Ident { .. } | Node::StringLit { .. } => true,
            Node::PrivateName { .. } | Node::ComputedKey { .. } => property,
            _ => false,
        };
        if !named {
            return;
        }
        let Some(sym) = self
            .c
            .sym_of_decl(Loc::new(file, n))
            .or_else(|| self.c.sym_of_decl(Loc::new(file, name)))
        else {
            return;
        };
        if let Node::MethodDef { kind, .. } | Node::MethodSignature { kind, .. } = *ast.node(n) {
            if !self.is_type_determining_accessor(sym, Loc::new(file, n), kind) {
                return;
            }
        } else if self
            .c
            .value_decl(sym)
            .is_some_and(|d| d != Loc::new(file, n) && d != Loc::new(file, name))
        {
            // A later declaration of a variable reads the type the first one gave it.
            return;
        }
        self.type_of_symbol(sym);
        if !self.circular.contains(&Resolving::Type(sym)) {
            return;
        }
        let path = self.type_cycles.get(&sym).cloned().unwrap_or_default();
        if !implicit && path.iter().any(|&r| self.is_excused_cycle_step(r)) {
            return;
        }
        // Only cycles of variable types: those through return types, contextual types and
        // checked nodes depend on how eagerly function expressions are checked, which
        // differs from tsc.
        // An accessor, method or function on the cycle is reported by tsc as its implicit
        // return type (TS7023).
        if implicit
            && !path.iter().all(|r| {
                matches!(r, Resolving::Type(s) if !self.c.sym_flags(*s).intersects(
                    SymbolFlags::ACCESSOR | SymbolFlags::METHOD | SymbolFlags::FUNCTION
                ))
            })
        {
            return;
        }
        let shown = match *ast.node(name) {
            Node::StringLit { value } => format!("\"{}\"", ast.atom(value)),
            _ => self.c.text(Loc::new(file, name)).to_owned(),
        };
        let info = if implicit {
            dg::_0_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_IT_DOES_NOT_HAVE_A_TYPE_ANNOTATION_AND_IS_REFE_7022
        } else {
            dg::_0_IS_REFERENCED_DIRECTLY_OR_INDIRECTLY_IN_ITS_OWN_TYPE_ANNOTATION_2502
        };
        self.error(Loc::new(file, name), info, &[&shown]);
    }

    /// A step of a resolution cycle that tsc does not take: an inferred return type (its
    /// cycles are TS7023/TS7024), or a type alias whose body (through unions,
    /// intersections, parentheses and type operators) has an array, a tuple or a reference
    /// with type arguments, whose element types and type arguments tsc resolves lazily
    /// (`type A = K<B>; type B = typeof a; var a: A` is fine).
    fn is_excused_cycle_step(&self, r: Resolving) -> bool {
        let s = match r {
            Resolving::Return(_) => return true,
            Resolving::DeclaredType(s) => s,
            _ => return false,
        };
        if !self.c.sym_flags(s).intersects(SymbolFlags::TYPE_ALIAS) {
            return false;
        }
        self.c.decls(s).into_iter().any(|d| {
            let ast = self.ast(d.file);
            let Node::TypeAlias { ty, .. } = *ast.node(d.node) else {
                return false;
            };
            let mut stack = vec![ty];
            let mut guard = 0u32;
            while let Some(t) = stack.pop() {
                guard += 1;
                if guard > 10_000 {
                    return true;
                }
                match *ast.node(t) {
                    Node::ArrayType { .. }
                    | Node::TupleType { .. }
                    | Node::TypeRef {
                        type_args: Some(_), ..
                    } => return true,
                    Node::ParenType { ty } | Node::TypeOperator { ty, .. } => stack.push(ty),
                    Node::UnionType { types } | Node::IntersectionType { types } => {
                        stack.extend(ast.list(types).iter().copied())
                    }
                    _ => {}
                }
            }
            false
        })
    }

    /// `n` is inside a loop of its function (flow analysis of loops resolves variable
    /// types in an order tsc does not).
    fn in_loop(&self, file: FileId, n: NodeId) -> bool {
        let ast = self.ast(file);
        let parents = self.c.file(file).parents();
        let mut cur = parents.parent(n);
        for _ in 0..10_000 {
            let Some(p) = cur else { return false };
            match ast.node(p) {
                Node::For { .. }
                | Node::ForIn { .. }
                | Node::ForOf { .. }
                | Node::While { .. }
                | Node::DoWhile { .. } => return true,
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => return false,
                _ => {}
            }
            cur = parents.parent(p);
        }
        false
    }

    /// The getter `n` has a return type annotation, or the setter `n` a parameter type.
    fn accessor_has_annotation(&self, file: FileId, n: NodeId, kind: MethodKind) -> bool {
        let ast = self.ast(file);
        let (params, ret) = match *ast.node(n) {
            Node::MethodDef { value, .. } => match *ast.node(value) {
                Node::FunctionExpr {
                    params,
                    return_type,
                    ..
                } => (params, return_type),
                _ => return false,
            },
            Node::MethodSignature {
                params,
                return_type,
                ..
            } => (params, return_type),
            _ => return false,
        };
        match kind {
            MethodKind::Get => ret.is_some(),
            _ => ast.list(params).first().is_some_and(|&p| {
                matches!(
                    ast.node(p),
                    Node::Param {
                        type_ann: Some(_),
                        ..
                    }
                )
            }),
        }
    }

    /// An annotated getter, or an annotated setter without an annotated getter.
    fn is_type_determining_accessor(
        &self,
        sym: ts_r_types::SymId,
        l: Loc,
        kind: MethodKind,
    ) -> bool {
        if kind == MethodKind::Get {
            return true;
        }
        !self.c.decls(sym).into_iter().any(|d| {
            d != l
                && matches!(
                    *self.ast(d.file).node(d.node),
                    Node::MethodDef {
                        kind: MethodKind::Get,
                        ..
                    } | Node::MethodSignature {
                        kind: MethodKind::Get,
                        ..
                    }
                )
                && self.accessor_has_annotation(d.file, d.node, MethodKind::Get)
        })
    }
}

/// `n` contains a function, arrow, method, accessor or class (their bodies and flow are
/// checked in an order that differs from tsc's).
fn has_function(ast: &ts_r_ast::Ast, n: NodeId, this_counts: bool) -> bool {
    let mut stack = vec![n];
    let mut seen = 0u32;
    while let Some(c) = stack.pop() {
        seen += 1;
        if seen > 10_000 {
            return true;
        }
        match ast.node(c) {
            Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodDef { .. }
            | Node::ClassExpr { .. } => return true,
            // `this` is the enclosing class or object literal, whose accessors' return
            // types close the cycle (`var _this = this` in a getter returning `_this.x`).
            Node::This {} if this_counts => return true,
            _ => ast.for_each_child(c, |x| stack.push(x)),
        }
    }
    false
}
