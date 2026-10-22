//! Decorators resolved as calls (TS1238-TS1241, TS1329, TS1270, TS1271).
//!
//! Rules observed from tsc 7.0.2. A decorator `@e` is a call of `e` with arguments the
//! runtime supplies:
//! - `experimentalDecorators`: a class gets `(typeof C)`; a property `(target, key)`; a
//!   method or accessor `(target, key, TypedPropertyDescriptor<T>)`, where a signature taking
//!   only the first two also fits; a parameter `(target, key, index)`. `target` is the class
//!   for static members and constructor parameters, else its instance type; `key` is the
//!   member name's literal type (`undefined` for constructor parameters).
//! - Decorators tsc rejects where they are written (TS1206, TS1249: overloads, abstract and
//!   declared fields, legacy decorators on `#private` members and auto-accessors) are not
//!   called.
//! - Standard decorators: two arguments (the value and a context), a signature taking only
//!   the first also fits. Only the argument count is checked.
//! - A count no signature takes: when every signature can be called without arguments and
//!   the decorator is a name, TS1329 at the `@` ("did you mean to call it first"); else the
//!   kind's "Unable to resolve signature" error (TS1238 class, TS1239 parameter, TS1240
//!   property, TS1241 method) with "The runtime will invoke the decorator with N arguments,
//!   but the decorator expects M": at the decorator expression when it takes fewer, at the
//!   `@` when it needs more.
//! - An argument type that does not fit: the same error at the expression, over the
//!   argument's TS2345 chain. A decorator without call signatures that has construct
//!   signatures (a class): the same error, "This expression is not callable".
//! - The chosen signature's return type (legacy decorators): a class decorator must return
//!   something assignable to `void | typeof C`, a method decorator to `void |
//!   TypedPropertyDescriptor<T>` (else TS1270), a property decorator `void` or `any` (else
//!   TS1271), at the expression. A single signature that failed on the count is still
//!   checked, instantiated from the arguments it takes.

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId};
use ts_r_binder::meaning;
use ts_r_diagnostics::FileId;
use ts_r_types::{TypeFlags, TypeId};

use crate::call::TypedCall;
use crate::checker::{Ck, Loc};
use crate::diag::{Chain, dg};
use crate::members::SigKind;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Class,
    Property,
    Method,
    Parameter,
}

impl Kind {
    fn info(self) -> dg::DiagnosticInfo {
        match self {
            Kind::Class => {
                dg::UNABLE_TO_RESOLVE_SIGNATURE_OF_CLASS_DECORATOR_WHEN_CALLED_AS_AN_EXPRESSION_1238
            }
            Kind::Parameter => {
                dg::UNABLE_TO_RESOLVE_SIGNATURE_OF_PARAMETER_DECORATOR_WHEN_CALLED_AS_AN_EXPRESSION_1239
            }
            Kind::Property => {
                dg::UNABLE_TO_RESOLVE_SIGNATURE_OF_PROPERTY_DECORATOR_WHEN_CALLED_AS_AN_EXPRESSION_1240
            }
            Kind::Method => {
                dg::UNABLE_TO_RESOLVE_SIGNATURE_OF_METHOD_DECORATOR_WHEN_CALLED_AS_AN_EXPRESSION_1241
            }
        }
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks decorator `d` (a `Decorator` node) of `owner` as a call; `t` is the type of
    /// its expression.
    pub(crate) fn check_decorator_call(
        &mut self,
        file: FileId,
        d: NodeId,
        owner: NodeId,
        t: TypeId,
    ) {
        let ast = self.ast(file);
        let Node::Decorator { expr } = *ast.node(d) else {
            return;
        };
        let s = self.store();
        if t == TypeId::UNRESOLVED || s.is_error(t) || s.flags(t).intersects(TypeFlags::ANY) {
            return;
        }
        let legacy = self.c.opts.experimental_decorators;
        let Some((kind, args, value_type)) = self.decorator_arguments(file, owner, legacy) else {
            return;
        };
        let el = Loc::new(file, expr);
        let dl = Loc::new(file, d);
        let apparent = self.apparent_type(t);
        let sigs = self.signatures_of(apparent, SigKind::Call);
        if sigs.is_empty() {
            if !self.signatures_of(apparent, SigKind::Construct).is_empty() {
                let mut chain = Chain::new();
                chain.push_back(kind.info(), &[]);
                chain.push_back(dg::THIS_EXPRESSION_IS_NOT_CALLABLE_2349, &[]);
                self.no_signatures_chain(&mut chain, t, SigKind::Call);
                self.error_chain(el, &chain);
            }
            return;
        }
        let optional_last = !legacy || kind == Kind::Method;
        let chosen = match self.resolve_typed_call(dl, &sigs, &args, optional_last) {
            TypedCall::Ok(sig) => Some(sig),
            TypedCall::Arg {
                sig,
                arg_type,
                param,
            } => {
                let (mut chain, _) = self.relation_error_chain(
                    arg_type,
                    param,
                    ts_r_types::RelationKind::Assignable,
                    Some(crate::relate::Head {
                        info: dg::ARGUMENT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_PARAMETER_OF_TYPE_1_2345,
                    }),
                );
                chain.push_front(kind.info(), &[]);
                self.error_chain(el, &chain);
                Some(sig)
            }
            TypedCall::Arity => {
                self.report_decorator_arity(file, d, expr, kind, &sigs, &args, optional_last)
            }
        };
        if legacy && let Some(sig) = chosen {
            self.check_decorator_return(el, kind, sig, value_type);
        }
    }

    /// TS1329 or the arity form of TS1238-TS1241; returns the single signature instantiated
    /// from the arguments it takes, for the return type check.
    #[allow(clippy::too_many_arguments)]
    fn report_decorator_arity(
        &mut self,
        file: FileId,
        d: NodeId,
        expr: NodeId,
        kind: Kind,
        sigs: &[ts_r_types::SigId],
        args: &[TypeId],
        optional_last: bool,
    ) -> Option<ts_r_types::SigId> {
        let ast = self.ast(file);
        let arities: Vec<(usize, usize, bool)> = sigs.iter().map(|&s| self.sig_arity(s)).collect();
        let n = args.len();
        let fewest = if optional_last {
            n.saturating_sub(1)
        } else {
            n
        };
        let is_name = matches!(ast.node(expr), Node::Ident { .. } | Node::Member { .. });
        if is_name && arities.iter().all(|a| a.0 == 0) {
            let name = self.c.text(Loc::new(file, expr)).to_owned();
            let sp = ast.span(d);
            self.error_span(
                file,
                ts_r_span::Span::new(sp.start, ast.span(expr).end),
                dg::_0_ACCEPTS_TOO_FEW_ARGUMENTS_TO_BE_USED_AS_A_DECORATOR_HERE_DID_YOU_MEAN_TO_CALL_I_1329,
                &[&name],
            );
            return None;
        }
        let min = arities.iter().map(|a| a.0).min().unwrap_or(0);
        let max = arities.iter().map(|a| a.1).max().unwrap_or(0);
        let rest = arities.iter().any(|a| a.2);
        let mut chain = Chain::new();
        chain.push_back(kind.info(), &[]);
        let count = n.to_string();
        let at = if !rest && max < fewest {
            chain.push_back(
                dg::THE_RUNTIME_WILL_INVOKE_THE_DECORATOR_WITH_1_ARGUMENTS_BUT_THE_DECORATOR_EXPECTS_1278,
                &[&max.to_string(), &count],
            );
            Loc::new(file, expr)
        } else {
            if rest || max > min {
                chain.push_back(
                    dg::THE_RUNTIME_WILL_INVOKE_THE_DECORATOR_WITH_1_ARGUMENTS_BUT_THE_DECORATOR_EXPECTS_1279,
                    &[&min.to_string(), &count],
                );
            } else {
                chain.push_back(
                    dg::THE_RUNTIME_WILL_INVOKE_THE_DECORATOR_WITH_1_ARGUMENTS_BUT_THE_DECORATOR_EXPECTS_1278,
                    &[&min.to_string(), &count],
                );
            }
            Loc::new(file, d)
        };
        let sp = if at.node == d {
            let s = ast.span(d);
            ts_r_span::Span::new(s.start, ast.span(expr).end)
        } else {
            self.error_span_of(at)
        };
        self.push_nested_chain(file, sp, &chain);
        match sigs {
            [s] => {
                let taken = &args[..args.len().min(arities[0].1)];
                Some(self.instantiate_typed_call(Loc::new(file, d), *s, taken))
            }
            _ => None,
        }
    }

    /// TS1270/TS1271 for the return type of the decorator signature `sig`.
    fn check_decorator_return(
        &mut self,
        el: Loc,
        kind: Kind,
        sig: ts_r_types::SigId,
        value: TypeId,
    ) {
        let r = self.return_type_of(sig);
        let s = self.store();
        if r == TypeId::UNRESOLVED
            || s.is_error(r)
            || s.flags(r).intersects(TypeFlags::ANY | TypeFlags::VOID)
        {
            return;
        }
        match kind {
            Kind::Property => {
                let rs = self.type_to_string(r);
                self.error(
                    el,
                    dg::DECORATOR_FUNCTION_RETURN_TYPE_IS_0_BUT_IS_EXPECTED_TO_BE_VOID_OR_ANY_1271,
                    &[&rs],
                );
            }
            Kind::Class | Kind::Method => {
                if value == TypeId::UNRESOLVED {
                    return;
                }
                let expected = self.union(&[TypeId::VOID, value]);
                if self.is_assignable(r, expected) {
                    return;
                }
                let rs = self.type_to_string(r);
                let es = self.type_to_string(expected);
                self.error(
                    el,
                    dg::DECORATOR_FUNCTION_RETURN_TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_1270,
                    &[&rs, &es],
                );
            }
            Kind::Parameter => {}
        }
    }

    /// The decorator kind of `owner`, the argument types the runtime passes, and the type a
    /// class or method decorator may return besides `void` (`UNRESOLVED` when unknown).
    fn decorator_arguments(
        &mut self,
        file: FileId,
        owner: NodeId,
        legacy: bool,
    ) -> Option<(Kind, Vec<TypeId>, TypeId)> {
        let ast = self.ast(file);
        let kind = match *ast.node(owner) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => Kind::Class,
            Node::PropertyDef { modifiers, .. } => {
                if modifiers.intersects(Modifiers::ABSTRACT | Modifiers::DECLARE) {
                    return None;
                }
                if modifiers.contains(Modifiers::ACCESSOR) && legacy {
                    return None;
                }
                Kind::Property
            }
            Node::MethodDef { kind, value, .. } => {
                if kind == MethodKind::Constructor
                    || crate::nodes::fn_parts(ast, value).is_none_or(|p| p.body.is_none())
                {
                    return None;
                }
                Kind::Method
            }
            Node::Param { .. } => Kind::Parameter,
            _ => return None,
        };
        if !legacy {
            return Some((kind, vec![TypeId::ANY, TypeId::ANY], TypeId::UNRESOLVED));
        }
        // The class, the member and (for parameters) the function.
        let (class, member) = match kind {
            Kind::Class => (owner, None),
            Kind::Parameter => {
                let func = self.c.parent(Loc::new(file, owner))?;
                let m = self.c.parent(func)?;
                if !matches!(ast.node(m.node), Node::MethodDef { .. }) {
                    return None;
                }
                (self.c.parent(m)?.node, Some(m.node))
            }
            _ => (self.c.parent(Loc::new(file, owner))?.node, Some(owner)),
        };
        if !matches!(
            ast.node(class),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        ) {
            return None;
        }
        let class_sym = self.c.sym_of_decl(Loc::new(file, class))?;
        let ctor = self.type_of_symbol(class_sym);
        let Some(m) = member else {
            return Some((kind, vec![ctor], ctor));
        };
        let (modifiers, key, mkind) = match *ast.node(m) {
            Node::PropertyDef { modifiers, key, .. } => (modifiers, key, None),
            Node::MethodDef {
                modifiers,
                key,
                kind,
                ..
            } => (modifiers, key, Some(kind)),
            _ => return None,
        };
        // Invalid there (TS1206).
        if matches!(ast.node(key), Node::PrivateName { .. }) {
            return None;
        }
        let target =
            if modifiers.contains(Modifiers::STATIC) || mkind == Some(MethodKind::Constructor) {
                ctor
            } else {
                self.declared_type_of_symbol(class_sym)
            };
        let key_type = if mkind == Some(MethodKind::Constructor) {
            TypeId::UNDEFINED
        } else {
            self.decorator_key_type(file, key)
        };
        match kind {
            Kind::Property => Some((kind, vec![target, key_type], TypeId::UNRESOLVED)),
            Kind::Method => {
                let value = self
                    .c
                    .sym_of_decl(Loc::new(file, m))
                    .map(|s| self.type_of_symbol(s))
                    .unwrap_or(TypeId::UNRESOLVED);
                let desc = self
                    .c
                    .global_symbol("TypedPropertyDescriptor", meaning::TYPE)
                    .map(|s| self.declared_type_of_symbol(s))
                    .filter(|&d| d != TypeId::UNRESOLVED && !self.store().is_error(d))
                    .map(|d| self.make_reference(d, &[value]));
                // tsc's descriptor type lookup reports a missing global (TS2318).
                if desc.is_none() && self.reporting(file) {
                    self.report_missing_global_type("TypedPropertyDescriptor");
                }
                let desc = match desc {
                    Some(d) if value != TypeId::UNRESOLVED => d,
                    _ => {
                        return Some((
                            kind,
                            vec![target, key_type, TypeId::ANY],
                            TypeId::UNRESOLVED,
                        ));
                    }
                };
                Some((kind, vec![target, key_type, desc], desc))
            }
            Kind::Parameter => {
                let func = self.c.parent(Loc::new(file, owner))?;
                let params = match *ast.node(func.node) {
                    Node::FunctionExpr { params, .. } => params,
                    _ => return None,
                };
                let index = ast
                    .list(params)
                    .iter()
                    .filter(|&&p| !crate::nodes::is_this_param(ast, p))
                    .position(|&p| p == owner)?;
                let it = self.store().number_literal(index as f64, false);
                Some((kind, vec![target, key_type, it], TypeId::UNRESOLVED))
            }
            Kind::Class => None,
        }
    }

    /// The literal type of a member name (`any` when it has none).
    fn decorator_key_type(&mut self, file: FileId, key: NodeId) -> TypeId {
        let ast = self.ast(file);
        let name = match *ast.node(key) {
            Node::ComputedKey { expr } => self.late_bound_name(Loc::new(file, expr)),
            Node::NumberLit { .. } => {
                let text = crate::nodes::key_text(ast, key);
                return match text.and_then(|t| t.parse::<f64>().ok()) {
                    Some(v) => self.store().number_literal(v, false),
                    None => TypeId::ANY,
                };
            }
            Node::PrivateName { .. } => None,
            _ => crate::nodes::key_text(ast, key),
        };
        match name {
            Some(n) if !n.starts_with("__@") => self.store().string_literal(&n, false),
            _ => TypeId::ANY,
        }
    }
}
