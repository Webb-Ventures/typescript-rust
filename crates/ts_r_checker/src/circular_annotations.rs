//! Declarations whose type annotation (TS2502) or initializer (TS7022) depends on their
//! own type, and type aliases whose body needs the alias itself (TS2456; the rule is in
//! the crate AGENTS.md, "Circular type aliases").
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
//! initializer has no function, method, class or `this` or names the variable outside them
//! (`var b = { f: () => 1, g: b.f() }`: computing the type reads `b` in any order), and
//! class properties (any key
//! but a computed non-literal one) whose initializer has no function, method or class, and
//! `for`-`in`/`of` variables whose iterated expression has none either.
//!
//! The symbol type resolution stack (`Ck::push_resolution`) records the cycles; this check
//! resolves the declaration's symbol and reports when its resolution was circular.

use rustc_hash::FxHashSet;
use ts_r_ast::{MethodKind, Node, NodeId, TypeOperatorKind};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

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
                && (!has_function(ast, init, true)
                    || self
                        .c
                        .sym_of_decl(Loc::new(file, n))
                        .or_else(|| self.c.sym_of_decl(Loc::new(file, name)))
                        .is_some_and(|s| self.names_outside_functions(file, name, init, s))) =>
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
            // A parameter typed by its default (`function f(x = x)`).
            Node::Param {
                pattern,
                type_ann: None,
                init: Some(init),
                ..
            } if self.c.opts.no_implicit_any
                && matches!(ast.node(pattern), Node::Ident { .. })
                && !has_function(ast, init, true) =>
            {
                implicit = true;
                pattern
            }
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
        // A class method's inferred return type on the cycle (`next() { return v; }` with
        // `for (var v of new It)`) is one tsc also finds; it reports both.
        if implicit
            && !path.iter().all(|r| match *r {
                Resolving::Type(s) => !self.c.sym_flags(s).intersects(
                    SymbolFlags::ACCESSOR | SymbolFlags::METHOD | SymbolFlags::FUNCTION,
                ),
                Resolving::Return(g) => self.is_class_method_sig(g),
                _ => false,
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

    /// Whether `sig` is the signature of a plain class method (not an accessor), declared
    /// with a body and without a return type annotation.
    pub(crate) fn is_class_method_sig(&self, sig: ts_r_types::SigId) -> bool {
        let Some(d) = self.store().sig(sig).decl.and_then(Loc::unpack) else {
            return false;
        };
        let ast = self.ast(d.file);
        // The declaration is the method itself or its function value.
        let method = match *ast.node(d.node) {
            Node::MethodDef { .. } => Some(d),
            _ => self.c.parent(d).filter(
                |p| matches!(*ast.node(p.node), Node::MethodDef { value, .. } if value == d.node),
            ),
        };
        let Some(m) = method else { return false };
        matches!(
            *ast.node(m.node),
            Node::MethodDef {
                kind: ts_r_ast::MethodKind::Method,
                ..
            }
        ) && self.c.parent(m).is_some_and(|c| {
            matches!(
                ast.node(c.node),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            )
        })
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
    /// Whether `s` is a variable whose type is `any` after a cycle through its initializer:
    /// declared once, unannotated, outside loops, with an initializer that has no
    /// function, method, class or `this`, or that names the variable outside them (the
    /// TS7022 declarations above).
    pub(crate) fn is_circular_variable(&self, s: SymId) -> bool {
        if !self.c.sym_flags(s).intersects(SymbolFlags::VARIABLE) {
            return false;
        }
        let decls = self.c.decls(s);
        let [d] = decls[..] else {
            return false;
        };
        let d = match self.ast(d.file).node(d.node) {
            Node::Ident { .. } => match self.c.parent(d) {
                Some(p) => p,
                None => return false,
            },
            _ => d,
        };
        if self.c.file(d.file).is_js() {
            return false;
        }
        let ast = self.ast(d.file);
        match *ast.node(d.node) {
            Node::VarDeclarator {
                name,
                type_ann: None,
                init: Some(init),
                ..
            } => {
                matches!(ast.node(name), Node::Ident { .. })
                    && !self.in_loop(d.file, d.node)
                    && (!has_function(ast, init, true)
                        || self.names_outside_functions(d.file, name, init, s))
            }
            _ => false,
        }
    }

    /// The initializer `init` of the declaration of `s` (named `name`) names `s` outside the
    /// functions and classes in it (`bar: b.foo()` in `var b = { foo: () => 1, bar: b.foo() }`):
    /// computing its type reads the variable's own type whatever the order functions are
    /// checked in, so the cycle is tsc's too.
    fn names_outside_functions(&self, file: FileId, name: NodeId, init: NodeId, s: SymId) -> bool {
        let ast = self.ast(file);
        let Some(text) = ast.name_of(name) else {
            return false;
        };
        let mut stack = vec![init];
        let mut seen = 0u32;
        while let Some(c) = stack.pop() {
            seen += 1;
            if seen > 10_000 {
                return false;
            }
            match *ast.node(c) {
                Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::ClassExpr { .. } => {}
                Node::Ident { .. } => {
                    if ast.name_of(c) == Some(text)
                        && self.c.resolve_ident(Loc::new(file, c), meaning::VALUE) == Some(s)
                    {
                        return true;
                    }
                }
                Node::Member { obj, .. } => stack.push(obj),
                Node::Property { key, value, .. } => {
                    stack.push(value);
                    if matches!(ast.node(key), Node::ComputedKey { .. }) {
                        stack.push(key);
                    }
                }
                _ => ast.for_each_child(c, |x| stack.push(x)),
            }
        }
        false
    }

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

/// Something a type alias's body resolves before the alias has a type: another type
/// alias, or a variable named by `typeof` (whose annotation is then resolved).
#[derive(Clone, Copy, PartialEq, Eq)]
enum EagerStep {
    Alias(SymId),
    Var(SymId),
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2795 at an `intrinsic` alias body unless the alias is one the compiler provides
    /// (`BuiltinIteratorReturn` with no type parameters, `Uppercase<S>` and the other string
    /// mappings or `NoInfer<T>` with one) (tsc: checker.go, checkTypeAliasDeclaration).
    pub(crate) fn check_intrinsic_type_alias(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let Node::TypeAlias {
            name,
            type_params,
            ty,
            ..
        } = *ast.node(n)
        else {
            return;
        };
        if !matches!(
            ast.node(ty),
            Node::KeywordType {
                kind: ts_r_ast::KeywordTypeKind::Intrinsic
            }
        ) {
            return;
        }
        let count = match type_params.map(|p| ast.node(p)) {
            Some(Node::TypeParams { params }) => ast.list(*params).len(),
            _ => 0,
        };
        let text = self.c.text(Loc::new(file, name));
        let valid = (count == 0 && text == "BuiltinIteratorReturn")
            || (count == 1 && crate::template::intrinsic_kind(text).is_some());
        if !valid {
            self.error(
                Loc::new(file, ty),
                dg::THE_INTRINSIC_KEYWORD_CAN_ONLY_BE_USED_TO_DECLARE_COMPILER_PROVIDED_INTRINSIC_TY_2795,
                &[],
            );
        }
    }

    /// TS2456 at the name of a type alias whose body needs the alias itself before it can
    /// be built (see the crate AGENTS.md, "Circular type aliases").
    pub(crate) fn check_circular_type_alias(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let Node::TypeAlias { name, .. } = *self.ast(file).node(n) else {
            return;
        };
        let Some(sym) = self.c.sym_of_decl(Loc::new(file, n)) else {
            return;
        };
        if !self.alias_needs_itself(sym) {
            return;
        }
        let shown = self.c.text(Loc::new(file, name)).to_owned();
        self.error(
            Loc::new(file, name),
            dg::TYPE_ALIAS_0_CIRCULARLY_REFERENCES_ITSELF_2456,
            &[&shown],
        );
    }

    /// Whether resolving alias `sym`'s body reaches `sym` again through steps that are
    /// all resolved right away.
    fn alias_needs_itself(&mut self, sym: SymId) -> bool {
        let mut seen: FxHashSet<SymId> = FxHashSet::default();
        seen.insert(sym);
        let mut work = vec![EagerStep::Alias(sym)];
        let mut found = Vec::new();
        let mut budget = 20_000u32;
        while let Some(step) = work.pop() {
            found.clear();
            match step {
                EagerStep::Alias(s) => {
                    for d in self.c.decls(s) {
                        if let Node::TypeAlias { ty, .. } = *self.ast(d.file).node(d.node) {
                            self.eager_steps(d.file, ty, true, &mut found, &mut budget);
                        }
                    }
                }
                EagerStep::Var(s) => {
                    for d in self.c.decls(s) {
                        // A variable is declared by its name.
                        let Some(p) = self.c.parent(d) else { continue };
                        if let Node::VarDeclarator {
                            name,
                            type_ann: Some(t),
                            ..
                        } = *self.ast(d.file).node(p.node)
                            && name == d.node
                        {
                            self.eager_steps(d.file, t, false, &mut found, &mut budget);
                        }
                    }
                }
            }
            for &f in &found {
                let s = match f {
                    EagerStep::Alias(s) => {
                        if s == sym {
                            return true;
                        }
                        s
                    }
                    EagerStep::Var(s) => s,
                };
                if seen.insert(s) {
                    work.push(f);
                }
            }
            if budget == 0 {
                return false;
            }
        }
        false
    }

    /// The aliases and `typeof` variables that resolving type node `root` resolves at
    /// once. `in_alias` is set for an alias's body: there an array, a tuple or a generic
    /// class or interface reference is built lazily when it is the body itself or when
    /// one of its element types or type arguments may name an alias.
    fn eager_steps(
        &mut self,
        file: FileId,
        root: NodeId,
        in_alias: bool,
        out: &mut Vec<EagerStep>,
        budget: &mut u32,
    ) {
        // (node, whether it is the alias's body itself through parentheses and `readonly`,
        // whether every construct above it up to the alias resolves its parts at once)
        let mut stack = vec![(root, in_alias, in_alias)];
        while let Some((n, whole, ctx)) = stack.pop() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let ast = self.ast(file);
            match *ast.node(n) {
                Node::ParenType { ty } => stack.push((ty, whole, ctx)),
                Node::TypeOperator { op, ty } => {
                    stack.push((ty, whole && op == TypeOperatorKind::Readonly, ctx))
                }
                Node::UnionType { types } | Node::IntersectionType { types } => {
                    stack.extend(ast.list(types).iter().map(|&t| (t, false, ctx)))
                }
                Node::IndexedAccessType { obj, index } => {
                    stack.push((obj, false, ctx));
                    stack.push((index, false, ctx));
                }
                Node::ConditionalType { check, extends, .. } => {
                    stack.push((check, false, ctx));
                    stack.push((extends, false, ctx));
                }
                Node::NamedTupleMember { ty, .. } => stack.push((ty, false, ctx)),
                Node::OptionalType { ty } | Node::RestType { ty } => stack.push((ty, false, false)),
                Node::TemplateLitType { parts } => {
                    stack.extend(ast.list(parts).iter().map(|&t| (t, false, false)))
                }
                Node::ArrayType { elem } => {
                    if !(whole || ctx && self.may_name_alias(file, elem, 0)) {
                        stack.push((elem, false, ctx));
                    }
                }
                Node::TupleType { elems } => {
                    let elems = ast.list(elems).to_vec();
                    let variadic = elems.iter().any(|&e| match *ast.node(e) {
                        Node::RestType { ty } | Node::NamedTupleMember { rest: true, ty, .. } => {
                            !crate::check::spread_array_element_node(ast, ty, 0)
                        }
                        _ => false,
                    });
                    let lazy = !variadic
                        && (whole || ctx && elems.iter().any(|&e| self.may_name_alias(file, e, 0)));
                    if !lazy {
                        stack.extend(elems.iter().map(|&e| (e, false, ctx)));
                    }
                }
                Node::TypeRef { name, type_args } => {
                    let args = match type_args.map(|a| *ast.node(a)) {
                        Some(Node::TypeArgs { args }) => ast.list(args).to_vec(),
                        _ => Vec::new(),
                    };
                    let Some(s) = self
                        .resolve_entity(Loc::new(file, name), meaning::TYPE)
                        .and_then(|s| self.c.resolve_alias(s))
                    else {
                        continue;
                    };
                    let flags = self.c.sym_flags(s);
                    if flags.intersects(SymbolFlags::TYPE_ALIAS) {
                        out.push(EagerStep::Alias(s));
                        stack.extend(args.iter().map(|&a| (a, false, ctx)));
                    } else if flags.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE)
                        && !args.is_empty()
                    {
                        let defaults = args.len() < self.local_type_params(s).len();
                        let lazy = whole
                            || ctx
                                && (defaults
                                    || args.iter().any(|&a| self.may_name_alias(file, a, 0)));
                        if !lazy {
                            stack.extend(args.iter().map(|&a| (a, false, ctx)));
                        }
                    }
                }
                Node::TypeQuery { expr_name, .. } => {
                    if matches!(ast.node(expr_name), Node::Ident { .. })
                        && let Some(s) = self
                            .c
                            .resolve_ident(Loc::new(file, expr_name), meaning::VALUE)
                            .and_then(|s| self.c.resolve_alias(s))
                        && self.c.sym_flags(s).intersects(SymbolFlags::VARIABLE)
                    {
                        out.push(EagerStep::Var(s));
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether resolving type node `n` may resolve a type alias (an alias reference or a
    /// `typeof`, through the constructs that resolve their parts at once).
    fn may_name_alias(&mut self, file: FileId, n: NodeId, depth: u32) -> bool {
        if depth > 64 {
            return true;
        }
        let ast = self.ast(file);
        match *ast.node(n) {
            Node::TypeRef { name, .. } => self
                .resolve_entity(Loc::new(file, name), meaning::TYPE)
                .and_then(|s| self.c.resolve_alias(s))
                .is_some_and(|s| self.c.sym_flags(s).intersects(SymbolFlags::TYPE_ALIAS)),
            Node::TypeQuery { .. } => true,
            Node::TypeOperator { op, ty } => {
                op != TypeOperatorKind::Unique && self.may_name_alias(file, ty, depth + 1)
            }
            Node::ParenType { ty }
            | Node::OptionalType { ty }
            | Node::NamedTupleMember { ty, .. } => self.may_name_alias(file, ty, depth + 1),
            Node::RestType { ty } => match *ast.node(ty) {
                Node::ArrayType { elem } => self.may_name_alias(file, elem, depth + 1),
                _ => true,
            },
            Node::UnionType { types } | Node::IntersectionType { types } => {
                let types = ast.list(types).to_vec();
                types
                    .into_iter()
                    .any(|t| self.may_name_alias(file, t, depth + 1))
            }
            Node::IndexedAccessType { obj, index } => {
                self.may_name_alias(file, obj, depth + 1)
                    || self.may_name_alias(file, index, depth + 1)
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => [check, extends, true_ty, false_ty]
                .into_iter()
                .any(|t| self.may_name_alias(file, t, depth + 1)),
            _ => false,
        }
    }
}
