//! Overload sets of functions, methods and constructors (TS2383-TS2394, TS2512).
//!
//! Rules observed from tsc 7.0.2, per statement list or class body. Members are named by
//! their key text, or for a computed key by the literal or unique symbol its type denotes
//! (`[Symbol.iterator]`); computed keys of wider types (`[s]` for `s: symbol`) form no group.
//! - Consecutive body-less declarations of one name form an overload group. The next
//!   declaration must be the implementation (same name, with a body):
//!   - none follows (outside ambient contexts): TS2391 at the last overload (TS2390 for
//!     constructors), unless that last overload is abstract or optional and no later
//!     declaration of the member follows in the list; when one does, an abstract last
//!     overload gets TS2516 instead (tsc: checker.go, checkFunctionOrConstructorSymbol);
//!   - a function or method with another name and a body follows: TS2389 at it ("must be
//!     'f'");
//!   - the same method name with different `static`-ness follows: TS2387/TS2388 at it.
//! - Each declaration whose `export`, `declare`, visibility, `?` or `abstract` differs from
//!   the implementation's (else the first declaration's): TS2383/2384/2385/2386/2512 at its
//!   name.
//! - Method signatures of one interface or type literal symbol (merged declarations
//!   included) take the first one's `?` as canonical: TS2386 at each deviating name
//!   (tsc: checkFunctionOrConstructorSymbol in checker.go).
//! - Each overload must be compatible with the implementation (TS2394 at the overload
//!   name): the overload's parameter types are assignable to the implementation's (optional
//!   parameters include `undefined`, rest parameters expand), the implementation does not
//!   require more arguments than the overload accepts, the overload's `this` type is
//!   assignable to the implementation's, and the return types are assignable in either
//!   direction unless the overload returns `void`. Type parameters are erased to `any`.
//! - Two implementations of one function (across merged declarations) or of one method
//!   name and `static`-ness in a class body: TS2393 at every declaration.
//! - Several constructor implementations: TS2392 at every constructor declaration.
//! - `noImplicitAny`: body-less functions, non-private methods and method signatures
//!   without a return type annotation: TS7010 at the name; construct and call signatures: TS7013 and
//!   TS7020 at the signature.

use rustc_hash::FxHashMap;
use ts_r_ast::{MethodKind, Modifiers, Node, NodeId};
use ts_r_diagnostics::FileId;
use ts_r_types::{SigId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::{fn_parts, key_text};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Method,
    Constructor,
}

struct FnDecl {
    node: NodeId,
    name_node: NodeId,
    name: String,
    kind: Kind,
    has_body: bool,
    is_static: bool,
    modifiers: Modifiers,
    optional: bool,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks the overload groups of the statement list or class body `container`.
    pub(crate) fn check_overloads(&mut self, file: FileId, container: NodeId) {
        if !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        let list = match *ast.node(container) {
            Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return,
        };
        let items: Vec<Option<FnDecl>> = ast
            .list(list)
            .iter()
            .map(|&n| self.fn_decl_info(file, n))
            .collect();
        if items.iter().all(Option::is_none) {
            return;
        }
        let ambient_container = self.is_ambient_node(Loc::new(file, container));
        self.check_duplicate_implementations(file, &items);
        // TS2392 at every constructor declaration when several have a body.
        let ctor_impls = items
            .iter()
            .flatten()
            .filter(|d| d.kind == Kind::Constructor && d.has_body)
            .count();
        let multiple_ctors = ctor_impls > 1;
        if multiple_ctors {
            for d in items
                .iter()
                .flatten()
                .filter(|d| d.kind == Kind::Constructor)
            {
                let span = self.decl_name_span(file, d);
                self.error_span(
                    file,
                    span,
                    dg::MULTIPLE_CONSTRUCTOR_IMPLEMENTATIONS_ARE_NOT_ALLOWED_2392,
                    &[],
                );
            }
        }
        let mut i = 0;
        while i < items.len() {
            let Some(a) = &items[i] else {
                i += 1;
                continue;
            };
            if a.has_body || (multiple_ctors && a.kind == Kind::Constructor) {
                i += 1;
                continue;
            }
            // Overload group.
            let mut group: Vec<&FnDecl> = vec![a];
            let mut j = i + 1;
            while let Some(Some(b)) = items.get(j) {
                if b.has_body || b.name != a.name || b.kind != a.kind || b.is_static != a.is_static
                {
                    break;
                }
                group.push(b);
                j += 1;
            }
            let next = items.get(j).and_then(Option::as_ref);
            let ambient = ambient_container
                || group
                    .iter()
                    .any(|d| d.modifiers.contains(Modifiers::DECLARE));
            // Abstract and optional (`m?(): T;`) methods need no implementation; what
            // counts is the group's last declaration. A later, non-adjacent declaration of
            // the same member still makes the group's end an error (TS2516 when abstract).
            let last = group[group.len() - 1];
            let last_abstract = last.modifiers.contains(Modifiers::ABSTRACT);
            let continued_later = !ambient
                && items[j..]
                    .iter()
                    .flatten()
                    .any(|d| d.name == a.name && d.kind == a.kind && d.is_static == a.is_static);
            let abstract_group = (last_abstract || last.optional) && !continued_later;
            let mut implementation: Option<&FnDecl> = None;
            match next {
                Some(b)
                    if b.name == a.name
                        && b.kind == a.kind
                        && b.is_static != a.is_static
                        && !ambient =>
                {
                    let info = if a.is_static {
                        dg::FUNCTION_OVERLOAD_MUST_BE_STATIC_2387
                    } else {
                        dg::FUNCTION_OVERLOAD_MUST_NOT_BE_STATIC_2388
                    };
                    self.error(Loc::new(file, b.name_node), info, &[]);
                }
                Some(b) if b.name == a.name && b.kind == a.kind && b.has_body => {
                    implementation = Some(b);
                    j += 1;
                }
                Some(b)
                    if b.has_body
                        && b.kind == a.kind
                        && a.kind != Kind::Constructor
                        && !ambient
                        && !abstract_group =>
                {
                    // The name as written (`'["B"]'`).
                    let written = self.c.text(Loc::new(file, a.name_node)).to_owned();
                    self.error(
                        Loc::new(file, b.name_node),
                        dg::FUNCTION_IMPLEMENTATION_NAME_MUST_BE_0_2389,
                        &[&written],
                    );
                }
                _ => {
                    if !ambient && !abstract_group {
                        if a.kind == Kind::Constructor {
                            let span = self.decl_name_span(file, last);
                            self.error_span(
                                file,
                                span,
                                dg::CONSTRUCTOR_IMPLEMENTATION_IS_MISSING_2390,
                                &[],
                            );
                        } else if last_abstract {
                            self.error(
                                Loc::new(file, last.name_node),
                                dg::ALL_DECLARATIONS_OF_AN_ABSTRACT_METHOD_MUST_BE_CONSECUTIVE_2516,
                                &[],
                            );
                        } else {
                            self.error(
                                Loc::new(file, last.name_node),
                                dg::FUNCTION_IMPLEMENTATION_IS_MISSING_OR_NOT_IMMEDIATELY_FOLLOWING_THE_DECLARATION_2391,
                                &[],
                            );
                        }
                    }
                }
            }
            self.check_overload_modifiers(file, &group, implementation, ambient_container);
            if let Some(imp) = implementation
                && !ambient
            {
                // Only the first incompatible overload is reported.
                for o in &group {
                    if !self.check_overload_compatible(file, o, imp) {
                        break;
                    }
                }
            }
            i = j;
        }
    }

    /// TS7010/7013/7020 for the body-less declaration `n` without a return type.
    pub(crate) fn check_implicit_any_return(&mut self, file: FileId, n: NodeId) {
        if !self.c.opts.no_implicit_any || !self.reporting(file) || self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        let name = match *ast.node(n) {
            // A body the parser recovered from nothing (`function f() => 4`) is none.
            Node::FunctionDecl {
                name: Some(name),
                body,
                return_type: None,
                ..
            } if body.is_none_or(|b| ast.span(b).is_empty()) => name,
            // Private methods are exempt (their types are not part of declarations).
            Node::MethodDef {
                kind: MethodKind::Method,
                key,
                value,
                modifiers,
                ..
            } if !modifiers.contains(Modifiers::PRIVATE)
                && !matches!(ast.node(key), Node::PrivateName { .. })
                && fn_parts(ast, value)
                    .is_some_and(|p| p.body.is_none() && p.return_type.is_none()) =>
            {
                key
            }
            Node::MethodSignature {
                kind: MethodKind::Method,
                key,
                return_type: None,
                ..
            } => key,
            Node::ConstructSignature {
                return_type: None, ..
            } => {
                self.error(
                    Loc::new(file, n),
                    dg::CONSTRUCT_SIGNATURE_WHICH_LACKS_RETURN_TYPE_ANNOTATION_IMPLICITLY_HAS_AN_ANY_RET_7013,
                    &[],
                );
                return;
            }
            Node::CallSignature {
                return_type: None, ..
            } => {
                self.error(
                    Loc::new(file, n),
                    dg::CALL_SIGNATURE_WHICH_LACKS_RETURN_TYPE_ANNOTATION_IMPLICITLY_HAS_AN_ANY_RETURN_T_7020,
                    &[],
                );
                return;
            }
            _ => return,
        };
        let mut text = self.c.text(Loc::new(file, name)).to_owned();
        // A name the parser recovered as missing prints as tsc's `(Missing)`.
        if text.is_empty() {
            text = "(Missing)".to_owned();
        }
        self.error(
            Loc::new(file, name),
            dg::_0_WHICH_LACKS_RETURN_TYPE_ANNOTATION_IMPLICITLY_HAS_AN_1_RETURN_TYPE_7010,
            &[&text, "any"],
        );
    }

    fn fn_decl_info(&mut self, file: FileId, n: NodeId) -> Option<FnDecl> {
        let ast = self.ast(file);
        match *ast.node(n) {
            Node::FunctionDecl {
                modifiers,
                name: Some(name),
                body,
                ..
            } => Some(FnDecl {
                node: n,
                name_node: name,
                name: crate::nodes::ident_text(ast, name)?.to_owned(),
                kind: Kind::Function,
                has_body: body.is_some(),
                is_static: false,
                modifiers,
                optional: false,
            }),
            Node::MethodDef {
                modifiers,
                kind,
                key,
                optional,
                value,
                ..
            } => {
                let k = match kind {
                    // `constructor<>() {}` parses as a method; tsc treats it as a constructor.
                    MethodKind::Method
                        if matches!(ast.node(key), Node::Ident { .. })
                            && key_text(ast, key).as_deref() == Some("constructor") =>
                    {
                        Kind::Constructor
                    }
                    MethodKind::Method => Kind::Method,
                    MethodKind::Constructor => Kind::Constructor,
                    _ => return None,
                };
                let name = if k == Kind::Constructor {
                    "constructor".to_owned()
                } else {
                    match key_text(ast, key) {
                        Some(t) => t,
                        // A computed name of a literal or unique symbol type (`[Symbol.iterator]`).
                        None => match *ast.node(key) {
                            Node::ComputedKey { expr } => {
                                self.late_bound_name(Loc::new(file, expr))?
                            }
                            _ => return None,
                        },
                    }
                };
                Some(FnDecl {
                    node: n,
                    name_node: key,
                    name,
                    kind: k,
                    has_body: fn_parts(ast, value).is_some_and(|p| p.body.is_some()),
                    is_static: modifiers.contains(Modifiers::STATIC),
                    modifiers,
                    optional,
                })
            }
            _ => None,
        }
    }

    /// TS2393 for names implemented more than once in one list.
    fn check_duplicate_implementations(&mut self, file: FileId, items: &[Option<FnDecl>]) {
        // Methods: implementations per (name, static) in this class body.
        let mut method_impls: FxHashMap<(&str, bool), u32> = FxHashMap::default();
        for d in items.iter().flatten() {
            if d.has_body && d.kind == Kind::Method {
                *method_impls
                    .entry((d.name.as_str(), d.is_static))
                    .or_default() += 1;
            }
        }
        for d in items.iter().flatten() {
            let dup = match d.kind {
                Kind::Constructor => false,
                Kind::Method => method_impls
                    .get(&(d.name.as_str(), d.is_static))
                    .is_some_and(|&n| n > 1),
                // Functions: implementations across every declaration of the symbol
                // (merged namespaces included).
                Kind::Function => self
                    .c
                    .sym_of_decl(Loc::new(file, d.node))
                    .is_some_and(|sym| {
                        self.c
                            .decls(sym)
                            .into_iter()
                            .filter(|l| {
                                !self.c.file(l.file).is_js()
                                    && matches!(
                                        self.node(*l),
                                        Node::FunctionDecl { body: Some(_), .. }
                                    )
                            })
                            .count()
                            > 1
                    }),
            };
            if dup {
                self.error(
                    Loc::new(file, d.name_node),
                    dg::DUPLICATE_FUNCTION_IMPLEMENTATION_2393,
                    &[],
                );
            }
        }
    }

    /// TS2383/2384/2385/2386/2512: modifiers that must agree across an overload set.
    /// TS2386 at the method signature `n` (interface or type literal) when its `?` differs
    /// from the first method signature of the same symbol, merged declarations included.
    pub(crate) fn check_method_signature_optionality(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let Node::MethodSignature {
            kind: MethodKind::Method,
            key,
            optional,
            ..
        } = *self.ast(file).node(n)
        else {
            return;
        };
        let here = Loc::new(file, n);
        let Some(sym) = self.c.sym_of_decl(here) else {
            return;
        };
        let first = self.c.decls(sym).into_iter().find(|d| {
            matches!(
                self.c.file(d.file).ast.node(d.node),
                Node::MethodSignature {
                    kind: MethodKind::Method,
                    ..
                }
            )
        });
        let Some(first) = first else {
            return;
        };
        let Node::MethodSignature {
            optional: canonical,
            ..
        } = *self.c.file(first.file).ast.node(first.node)
        else {
            return;
        };
        if first != here && optional != canonical {
            self.error(
                Loc::new(file, key),
                dg::OVERLOAD_SIGNATURES_MUST_ALL_BE_OPTIONAL_OR_REQUIRED_2386,
                &[],
            );
        }
    }

    fn check_overload_modifiers(
        &mut self,
        file: FileId,
        group: &[&FnDecl],
        implementation: Option<&FnDecl>,
        ambient_container: bool,
    ) {
        let all: Vec<&FnDecl> = group.iter().copied().chain(implementation).collect();
        if all.len() < 2 {
            return;
        }
        let canon = implementation.unwrap_or(group[0]);
        let vis = |d: &FnDecl| {
            if d.modifiers.contains(Modifiers::PRIVATE) {
                2
            } else if d.modifiers.contains(Modifiers::PROTECTED) {
                1
            } else {
                0
            }
        };
        for d in all {
            let span = self.decl_name_span(file, d);
            // Members of ambient namespaces are exported anyway.
            if d.kind == Kind::Function
                && !ambient_container
                && d.modifiers.contains(Modifiers::EXPORT)
                    != canon.modifiers.contains(Modifiers::EXPORT)
            {
                self.error_span(
                    file,
                    span,
                    dg::OVERLOAD_SIGNATURES_MUST_ALL_BE_EXPORTED_OR_NON_EXPORTED_2383,
                    &[],
                );
            }
            if d.modifiers.contains(Modifiers::DECLARE)
                != canon.modifiers.contains(Modifiers::DECLARE)
            {
                self.error_span(
                    file,
                    span,
                    dg::OVERLOAD_SIGNATURES_MUST_ALL_BE_AMBIENT_OR_NON_AMBIENT_2384,
                    &[],
                );
            }
            if d.kind != Kind::Function && vis(d) != vis(canon) {
                self.error_span(
                    file,
                    span,
                    dg::OVERLOAD_SIGNATURES_MUST_ALL_BE_PUBLIC_PRIVATE_OR_PROTECTED_2385,
                    &[],
                );
            }
            if d.kind == Kind::Method && d.optional != canon.optional {
                self.error_span(
                    file,
                    span,
                    dg::OVERLOAD_SIGNATURES_MUST_ALL_BE_OPTIONAL_OR_REQUIRED_2386,
                    &[],
                );
            }
            if d.kind == Kind::Method
                && d.modifiers.contains(Modifiers::ABSTRACT)
                    != canon.modifiers.contains(Modifiers::ABSTRACT)
            {
                self.error_span(
                    file,
                    span,
                    dg::OVERLOAD_SIGNATURES_MUST_ALL_BE_ABSTRACT_OR_NON_ABSTRACT_2512,
                    &[],
                );
            }
        }
    }

    /// TS2394 when overload `o` is not compatible with implementation `imp`; `false` when
    /// reported.
    fn check_overload_compatible(&mut self, file: FileId, o: &FnDecl, imp: &FnDecl) -> bool {
        let os = self.erased_signature(Loc::new(file, o.node));
        let is = self.erased_signature(Loc::new(file, imp.node));
        // Parameters are bivariant for methods and constructors, and for functions without
        // strictFunctionTypes.
        let bivariant = o.kind != Kind::Function || !self.c.opts.strict_function_types;
        if self.overload_compatible(os, is, o.kind == Kind::Constructor, bivariant) {
            return true;
        }
        let span = self.decl_name_span(file, o);
        self.error_span(
            file,
            span,
            dg::THIS_OVERLOAD_SIGNATURE_IS_NOT_COMPATIBLE_WITH_ITS_IMPLEMENTATION_SIGNATURE_2394,
            &[],
        );
        false
    }

    /// Where overload diagnostics go: the name, or for constructors the modifiers and the
    /// `constructor` keyword.
    fn decl_name_span(&self, file: FileId, d: &FnDecl) -> ts_r_span::Span {
        if d.kind == Kind::Constructor {
            let ast = self.ast(file);
            ts_r_span::Span::new(ast.span(d.node).start, ast.span(d.name_node).end)
        } else {
            self.error_span_of(Loc::new(file, d.name_node))
        }
    }

    /// The declaration's signature with its type parameters replaced by `any`.
    fn erased_signature(&mut self, l: Loc) -> SigId {
        let sig = self.signature_from_decl(l);
        let tps = self
            .store()
            .list_items(self.store().sig(sig).type_params)
            .to_vec();
        if tps.is_empty() {
            return sig;
        }
        let anys = vec![TypeId::ANY; tps.len()];
        let m = self.store().mapper(&tps, &anys);
        self.instantiate_signature_full(sig, m)
    }

    fn overload_compatible(&mut self, os: SigId, is: SigId, ctor: bool, bivariant: bool) -> bool {
        // Types this layer may approximate are not compared.
        let skip = |ck: &Self, t: TypeId| {
            t == TypeId::UNRESOLVED
                || ck.store().is_error(t)
                || ck.could_contain_type_params(t)
                || ck.mentions_enum(t)
        };
        // Returns, in either direction (`void` overloads accept any implementation).
        if !ctor {
            let or = self.return_type_of(os);
            let ir = self.return_type_of(is);
            if skip(self, or) || skip(self, ir) {
                return true;
            }
            let f = self.store().flags(or) | self.store().flags(ir);
            if !(or == TypeId::VOID
                || f.intersects(TypeFlags::ANY)
                || self.is_assignable(ir, or)
                || self.is_assignable(or, ir))
            {
                return false;
            }
        }
        // `this` types.
        let ot = self.store().sig(os).this_type;
        let it = self.store().sig(is).this_type;
        if let (Some(ot), Some(it)) = (ot, it)
            && !skip(self, ot)
            && !skip(self, it)
            && !self.is_assignable(ot, it)
        {
            return false;
        }
        // Arity.
        let (o_count, o_rest) = self.param_count(os);
        let i_min = self.store().sig(is).min_args as usize;
        if !o_rest && i_min > o_count {
            return false;
        }
        // Parameters: overload to implementation.
        let n = o_count.max(self.param_count(is).0) + usize::from(o_rest);
        for k in 0..n {
            let Some(mut op) = self.param_type_at(os, k) else {
                break;
            };
            let Some(ip) = self.param_type_at(is, k) else {
                break;
            };
            if self
                .store()
                .sig(os)
                .params
                .get(k)
                .is_some_and(|p| p.optional && !(o_rest && k >= o_count))
            {
                op = self.add_optionality(op);
            }
            if skip(self, op) || skip(self, ip) {
                continue;
            }
            let related = self.is_assignable(op, ip) || (bivariant && self.is_assignable(ip, op));
            if !related {
                return false;
            }
        }
        true
    }
}
