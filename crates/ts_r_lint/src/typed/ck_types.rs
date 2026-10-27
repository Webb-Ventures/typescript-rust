//! [`CkTypes`]: [`TypeInfo`] over a `ts_r_checker` worker that has checked the linted file.

use std::cell::RefCell;

use ts_r_ast::{Ast, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::checker::{Ck, Loc};
use ts_r_checker::members::SigKind;
use ts_r_diagnostics::FileId;
use ts_r_types::{ObjectFlags, ObjectKind, SigId, TypeFlags, TypeId, TypeKind};

use crate::types::{Decl, Predicate, SymFacts, TypeInfo};

/// Type information for one file, answered by a checker worker. The worker must have run
/// `check_source_file` on the file first, so expression types are the ones the check saw
/// (contextual types, narrowing).
pub struct CkTypes<'r, 'c, 'a> {
    ck: RefCell<&'r mut Ck<'c, 'a>>,
    file: FileId,
}

impl std::fmt::Debug for CkTypes<'_, '_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CkTypes").field("file", &self.file).finish()
    }
}

impl<'r, 'c, 'a> CkTypes<'r, 'c, 'a> {
    pub fn new(ck: &'r mut Ck<'c, 'a>, file: FileId) -> Self {
        CkTypes {
            ck: RefCell::new(ck),
            file,
        }
    }

    fn loc(&self, n: NodeId) -> Loc {
        Loc::new(self.file, n)
    }

    fn with<R>(&self, f: impl FnOnce(&mut Ck<'c, 'a>) -> R) -> R {
        let mut ck = self.ck.borrow_mut();
        ck.quietly(f)
    }

    fn ast(&self) -> &'a Ast {
        let ck = self.ck.borrow();
        ck.c.ast(self.file)
    }

    fn decl_of(&self, l: Loc) -> Decl {
        let ck = self.ck.borrow();
        let f = ck.c.file(l.file);
        Decl {
            file: l.file.0,
            node: l.node,
            in_lib: f.is_lib,
            in_this_file: l.file == self.file,
            in_declaration_file: f.is_declaration(),
            in_external: f.is_external,
        }
    }
}

fn is_type_node(n: &Node) -> bool {
    matches!(
        n,
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::ThisType {}
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::TypeLiteral { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::InferType { .. }
            | Node::MappedType { .. }
            | Node::ParenType { .. }
            | Node::ImportType { .. }
            | Node::TypePredicate { .. }
    )
}

impl<'a> TypeInfo for CkTypes<'_, '_, 'a> {
    fn is_available(&self) -> bool {
        true
    }

    fn type_at(&self, node: NodeId) -> Option<Ty> {
        let ast = self.ast();
        let l = self.loc(node);
        let n = ast.node(node);
        if is_type_node(n) {
            return Some(self.with(|ck| ck.type_from_node(l)));
        }
        let parent = {
            let ck = self.ck.borrow();
            ck.c.parent(l)
        };
        match *n {
            Node::Ident { .. } | Node::PrivateName { .. } => {
                if let Some(p) = parent {
                    match *ast.node(p.node) {
                        Node::Member { prop, .. } if prop == node => {
                            return Some(self.with(|ck| ck.check_expr(p)));
                        }
                        Node::VarDeclarator { name, .. } if name == node => {
                            return Some(self.with(|ck| ck.binding_identifier_type(l)));
                        }
                        Node::Param { pattern, .. } if pattern == node => {
                            return Some(self.with(|ck| ck.param_type(p)));
                        }
                        // A default value (`p = x`, `{ a = x }`) is an expression.
                        Node::Param { .. } => {}
                        Node::BindingProperty { init, .. } if init != Some(node) => {
                            return Some(self.with(|ck| ck.binding_identifier_type(l)));
                        }
                        // The name of a type parameter declaration: the type parameter.
                        Node::TypeParam { name, .. } if name == node => {
                            let s = {
                                let ck = self.ck.borrow();
                                ck.c.sym_of_decl(p).or_else(|| ck.c.sym_of_decl(l))
                            }?;
                            return Some(self.with(|ck| ck.declared_type_of_symbol(s)));
                        }
                        _ => {}
                    }
                }
                let decl_sym = {
                    let ck = self.ck.borrow();
                    ck.c.sym_of_decl(l)
                };
                if let Some(s) = decl_sym {
                    return Some(self.with(|ck| ck.type_of_symbol(s)));
                }
                Some(self.with(|ck| ck.check_expr(l)))
            }
            Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::MethodDef { .. }
            | Node::PropertyDef { .. }
            | Node::MethodSignature { .. }
            | Node::PropertySignature { .. }
            | Node::Enum { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. } => {
                let s = {
                    let ck = self.ck.borrow();
                    ck.c.sym_of_decl(l)
                }?;
                Some(self.with(|ck| ck.type_of_symbol(s)))
            }
            Node::VarDeclarator { name, .. } => self.type_at(name),
            Node::Param { .. } => Some(self.with(|ck| ck.param_type(l))),
            _ => Some(self.with(|ck| ck.check_expr(l))),
        }
    }

    fn contextual_type(&self, node: NodeId) -> Option<Ty> {
        let l = self.loc(node);
        self.with(|ck| ck.contextual_type(l))
    }

    fn resolved_signature(&self, call: NodeId) -> Option<Sig> {
        let l = self.loc(call);
        let mut ck = self.ck.borrow_mut();
        ck.resolved_signature(l)
    }

    fn symbol_at(&self, node: NodeId) -> Option<Sym> {
        let ast = self.ast();
        let l = self.loc(node);
        let node = ts_r_checker::nodes::skip_parens(ast, node);
        let l = Loc::new(l.file, node);
        match *ast.node(node) {
            Node::Member { .. } | Node::Index { .. } => {
                self.with(|ck| ck.property_symbol_of_access(l))
            }
            Node::Ident { .. } | Node::PrivateName { .. } => {
                let (decl, parent) = {
                    let ck = self.ck.borrow();
                    (ck.c.sym_of_decl(l), ck.c.parent(l))
                };
                if decl.is_some() {
                    return decl;
                }
                if let Some(p) = parent
                    && let Node::Member { prop, .. } = *ast.node(p.node)
                    && prop == node
                {
                    return self.with(|ck| ck.property_symbol_of_access(p));
                }
                let ck = self.ck.borrow();
                ck.c.resolve_ident(l, ts_r_binder::meaning::VALUE)
                    .or_else(|| ck.c.resolve_ident(l, ts_r_binder::meaning::TYPE))
                    .or_else(|| ck.c.resolve_ident(l, ts_r_binder::meaning::NAMESPACE))
            }
            Node::This {} => None,
            // A module specifier (`from "./m"`): the module's symbol.
            Node::StringLit { .. } => {
                let ck = self.ck.borrow();
                let c = &ck.c;
                let request = c
                    .bound(l.file)
                    .module_requests
                    .iter()
                    .position(|r| r.node == node);
                match request {
                    Some(i) => c.module_target_symbol(c.link.module_target(l.file, i as u32)),
                    None => c.sym_of_decl(l),
                }
            }
            _ => {
                let ck = self.ck.borrow();
                ck.c.sym_of_decl(l)
            }
        }
    }

    fn type_of_type_node(&self, node: NodeId) -> Option<Ty> {
        let l = self.loc(node);
        Some(self.with(|ck| ck.type_from_node(l)))
    }

    fn declared_type_at(&self, node: NodeId) -> Option<Ty> {
        let s = self.symbol_at(node)?;
        let s = self.resolve_alias(s).unwrap_or(s);
        Some(self.with(|ck| ck.type_of_symbol(s)))
    }

    fn flags(&self, t: Ty) -> TypeFlags {
        self.ck.borrow().store().flags(t)
    }

    fn is_error(&self, t: Ty) -> bool {
        self.ck.borrow().store().is_error(t)
    }

    fn object_flags(&self, t: Ty) -> ObjectFlags {
        self.ck.borrow().object_flags(t)
    }

    fn union_members(&self, t: Ty) -> Vec<Ty> {
        self.ck.borrow().union_members(t)
    }

    fn intersection_members(&self, t: Ty) -> Vec<Ty> {
        let ck = self.ck.borrow();
        let s = ck.store();
        match s.kind(t) {
            TypeKind::Intersection { list, .. } => s.list_items(list).to_vec(),
            _ => vec![t],
        }
    }

    fn type_to_string(&self, t: Ty) -> String {
        self.with(|ck| ck.type_to_string(t))
    }

    fn apparent_type(&self, t: Ty) -> Ty {
        self.with(|ck| ck.apparent_type(t))
    }

    fn contains_type_params(&self, t: Ty) -> bool {
        self.ck.borrow().could_contain_type_params(t)
    }

    fn base_constraint(&self, t: Ty) -> Ty {
        self.with(|ck| ck.base_constraint(t))
    }

    fn property_of_type(&self, t: Ty, name: &str) -> Option<Sym> {
        self.with(|ck| ck.property_of_type(t, name))
    }

    fn properties_of(&self, t: Ty) -> Vec<Sym> {
        self.with(|ck| ck.properties_of(t).into_iter().map(|(_, s)| s).collect())
    }

    fn type_of_symbol(&self, s: Sym) -> Ty {
        self.with(|ck| ck.type_of_symbol(s))
    }

    fn declared_type_of_symbol(&self, s: Sym) -> Option<Ty> {
        Some(self.with(|ck| ck.declared_type_of_symbol(s)))
    }

    fn signatures(&self, t: Ty, construct: bool) -> Vec<Sig> {
        let kind = if construct {
            SigKind::Construct
        } else {
            SigKind::Call
        };
        self.with(|ck| ck.signatures_of(t, kind))
    }

    fn return_type(&self, sig: Sig) -> Ty {
        self.with(|ck| ck.return_type_of(sig))
    }

    fn param_type(&self, sig: Sig, i: usize) -> Option<Ty> {
        self.with(|ck| ck.param_type_at(sig, i))
    }

    fn param_count(&self, sig: Sig) -> (usize, bool) {
        self.ck.borrow().param_count(sig)
    }

    fn min_arg_count(&self, sig: Sig) -> Option<usize> {
        Some(self.ck.borrow().store().sig(sig).min_args as usize)
    }

    fn rest_param_type(&self, sig: Sig) -> Option<Ty> {
        let ck = self.ck.borrow();
        let d = ck.store().sig(sig);
        if d.has_rest() {
            d.params.last().map(|p| p.ty)
        } else {
            None
        }
    }

    fn this_param_type(&self, sig: Sig) -> Option<Ty> {
        self.ck.borrow().store().sig(sig).this_type
    }

    fn signature_decl(&self, sig: Sig) -> Option<Decl> {
        let (f, n) = {
            let ck = self.ck.borrow();
            let mut s: SigId = sig;
            let mut found = None;
            for _ in 0..16 {
                let d = ck.store().sig(s);
                if let Some(x) = d.decl {
                    found = Some(x);
                    break;
                }
                match d.target {
                    Some((t, _)) => s = t,
                    None => break,
                }
            }
            found?
        };
        if f as usize >= self.ck.borrow().c.file_count() {
            return None;
        }
        let node = NodeId::from_raw(n)?;
        Some(self.decl_of(Loc::new(FileId(f), node)))
    }

    fn signature_type_params(&self, sig: Sig) -> Vec<Ty> {
        let ck = self.ck.borrow();
        let s = ck.store();
        s.list_items(s.sig(sig).type_params).to_vec()
    }

    fn type_predicate(&self, sig: Sig) -> Option<Predicate> {
        use ts_r_types::PredicateKind as K;
        let p = self.with(|ck| ck.predicate_of(sig))?;
        Some(Predicate {
            asserts: matches!(p.kind, K::AssertsIdentifier | K::AssertsThis),
            this: matches!(p.kind, K::This | K::AssertsThis),
            param_index: p.param_index as usize,
            ty: p.ty,
        })
    }

    fn is_assignable(&self, source: Ty, target: Ty) -> bool {
        self.with(|ck| ck.is_assignable(source, target))
    }

    fn awaited_type(&self, t: Ty) -> Ty {
        self.with(|ck| ck.awaited_type(t))
    }

    fn iterated_type(&self, t: Ty, is_await: bool) -> Option<Ty> {
        self.with(|ck| ck.iterated_type(t, is_await))
    }

    fn well_known_symbol(&self, name: &str) -> Option<String> {
        self.with(|ck| ck.well_known_symbol_name(name))
    }

    fn type_args(&self, t: Ty) -> Vec<Ty> {
        let ck = self.ck.borrow();
        if ck.c.is_tuple_type(t) {
            return ck.c.type_args_of_tuple(t).to_vec();
        }
        ck.c.type_args_of(t).to_vec()
    }

    fn is_array(&self, t: Ty) -> bool {
        self.ck.borrow().c.is_array_type(t)
    }

    fn is_tuple(&self, t: Ty) -> bool {
        self.ck.borrow().c.is_tuple_type(t)
    }

    fn tuple_rest_index(&self, t: Ty) -> Option<usize> {
        let ck = self.ck.borrow();
        let shape = ck.store().tuple_shape_of(t)?;
        shape
            .elements
            .iter()
            .position(|e| e.kind == ts_r_types::TupleElement::REST)
    }

    fn array_element(&self, t: Ty) -> Option<Ty> {
        let ck = self.ck.borrow();
        if !ck.c.is_array_type(t) {
            return None;
        }
        ck.c.type_args_of(t).first().copied()
    }

    fn literal_text(&self, t: Ty) -> Option<String> {
        self.ck.borrow().literal_text(t)
    }

    fn type_symbol(&self, t: Ty) -> Option<Sym> {
        let ck = self.ck.borrow();
        let s = ck.store();
        let sym = match s.kind(t) {
            TypeKind::Object(o) => match s.object(o).kind {
                ObjectKind::Declared { sym, .. } => Some(sym),
                ObjectKind::Reference { target, .. } => match s.object_of(target).map(|o| o.kind) {
                    Some(ObjectKind::Declared { sym, .. }) => Some(sym),
                    _ => None,
                },
                ObjectKind::Anonymous { decl, .. } | ObjectKind::Mapped { decl, .. } => {
                    // `typeof C` of a class, function or namespace is keyed by its symbol.
                    if decl.0 == u32::MAX {
                        return Some(ts_r_types::SymId(decl.1)).filter(|s| !s.is_none());
                    }
                    if decl.0 as usize >= ck.c.file_count() {
                        return None;
                    }
                    let node = NodeId::from_raw(decl.1)?;
                    ck.c.sym_of_decl(Loc::new(FileId(decl.0), node))
                }
                ObjectKind::Literal { sym } => Some(sym),
                ObjectKind::TupleTarget { .. } => None,
            },
            TypeKind::EnumLiteral { member, .. } => Some(member),
            TypeKind::Enum(sym) | TypeKind::UniqueSymbol(sym) => Some(sym),
            TypeKind::TypeParameter(_) => s.type_param_data(t).map(|d| d.sym),
            _ => None,
        };
        sym.filter(|s| !s.is_none())
    }

    fn alias_symbol(&self, t: Ty) -> Option<Sym> {
        let ck = self.ck.borrow();
        ck.store().alias_of(t).map(|a| a.sym)
    }

    fn alias_type_args(&self, t: Ty) -> Vec<Ty> {
        let ck = self.ck.borrow();
        match ck.store().alias_of(t) {
            Some(a) => ck.store().list_items(a.args).to_vec(),
            None => Vec::new(),
        }
    }

    fn index_type(&self, t: Ty, number: bool) -> Option<Ty> {
        let key = if number {
            TypeId::NUMBER
        } else {
            TypeId::STRING
        };
        self.with(|ck| ck.index_info_of(t, key)).map(|i| i.value)
    }

    fn name_index_type(&self, t: Ty, name: &str) -> Option<Ty> {
        self.with(|ck| ck.name_pattern_index_info(t, name))
            .map(|i| i.value)
    }

    fn global_type(&self, name: &str) -> Option<Ty> {
        let s = {
            let ck = self.ck.borrow();
            ck.c.global_symbol(name, ts_r_binder::meaning::TYPE)
        }?;
        Some(self.with(|ck| ck.declared_type_of_symbol(s)))
    }

    fn reference_target(&self, t: Ty) -> Option<Ty> {
        let ck = self.ck.borrow();
        match ck.store().object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, .. }) => Some(target),
            _ => None,
        }
    }

    fn base_types(&self, t: Ty) -> Vec<Ty> {
        let Some(sym) = self.type_symbol(t) else {
            return Vec::new();
        };
        self.with(|ck| ck.base_types(sym).to_vec())
    }

    fn non_nullable(&self, t: Ty) -> Ty {
        self.with(|ck| ck.non_nullable(t))
    }

    fn base_of_literal(&self, t: Ty) -> Ty {
        self.with(|ck| ck.base_of_literal(t))
    }

    fn union_of(&self, types: &[Ty]) -> Ty {
        self.with(|ck| ck.union(types))
    }

    fn strict_null_checks(&self) -> bool {
        self.ck.borrow().c.opts.strict_null_checks
    }

    fn option(&self, name: &str) -> Option<bool> {
        let ck = self.ck.borrow();
        let o = &ck.c.opts;
        Some(match name {
            "strictNullChecks" => o.strict_null_checks,
            "noUncheckedIndexedAccess" => o.no_unchecked_indexed_access,
            "exactOptionalPropertyTypes" => o.exact_optional_property_types,
            "noImplicitThis" => o.no_implicit_this,
            "noPropertyAccessFromIndexSignature" => o.no_property_access_from_index_signature,
            "strictFunctionTypes" => o.strict_function_types,
            "useUnknownInCatchVariables" => o.use_unknown_in_catch_variables,
            _ => return None,
        })
    }

    fn symbol_name(&self, s: Sym) -> String {
        self.ck.borrow().c.sym_name(s).to_owned()
    }

    fn symbol_facts(&self, s: Sym) -> SymFacts {
        let ck = self.ck.borrow();
        let f = ck.c.sym_flags(s);
        let facts = ck.c.sym_facts(s);
        SymFacts {
            method: f.intersects(SymbolFlags::METHOD),
            property: f.intersects(SymbolFlags::PROPERTY),
            function: f.intersects(SymbolFlags::FUNCTION),
            class: f.intersects(SymbolFlags::CLASS),
            accessor: f.intersects(SymbolFlags::ACCESSOR),
            enum_member: f.intersects(SymbolFlags::ENUM_MEMBER),
            optional: f.intersects(SymbolFlags::OPTIONAL),
            readonly: facts.contains(ts_r_binder::SymbolFacts::READONLY),
            is_static: facts.contains(ts_r_binder::SymbolFacts::STATIC),
            alias: f.intersects(SymbolFlags::ALIAS),
            variable: f.intersects(SymbolFlags::VARIABLE),
            type_parameter: f.intersects(SymbolFlags::TYPE_PARAMETER),
            interface: f.intersects(SymbolFlags::INTERFACE),
            type_alias: f.intersects(SymbolFlags::TYPE_ALIAS),
            enum_: f.intersects(SymbolFlags::ENUM),
            module: f.intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE),
        }
    }

    fn symbol_decls(&self, s: Sym) -> Vec<Decl> {
        let locs = self.ck.borrow().c.decls(s);
        locs.into_iter().map(|l| self.decl_of(l)).collect()
    }

    fn resolve_alias(&self, s: Sym) -> Option<Sym> {
        self.ck.borrow().c.resolve_alias(s)
    }

    fn symbol_parent(&self, s: Sym) -> Option<Sym> {
        self.ck.borrow().c.sym_parent(s)
    }

    fn non_optional_type_at(&self, node: NodeId) -> Option<Ty> {
        let l = self.loc(node);
        let chained = self.ck.borrow().in_optional_chain(l);
        if chained {
            Some(self.with(|ck| ck.check_chain(l).0))
        } else {
            self.type_at(node)
        }
    }

    fn type_parts(&self, t: Ty) -> Vec<Ty> {
        self.with(|ck| match ck.store().kind(t) {
            TypeKind::Index(x) => vec![x],
            TypeKind::IndexedAccess { obj, index } => vec![obj, index],
            TypeKind::Substitution { base, .. } => vec![base],
            TypeKind::StringMapping { ty, .. } => vec![ty],
            TypeKind::TemplateLiteral(_) => ck
                .store()
                .template_parts(t)
                .map(|(_, types)| types.to_vec())
                .unwrap_or_default(),
            TypeKind::Conditional(_) => {
                let s = ck.store();
                let Some(d) = s.conditional_data(t) else {
                    return Vec::new();
                };
                let root = *s.conditional_root_data(d.root);
                let check = ck.instantiate(root.check, d.mapper);
                let extends = ck.instantiate(root.extends, d.mapper);
                let mut v = vec![check, extends];
                v.extend(ck.conditional_branches_union(t));
                v
            }
            TypeKind::Object(_) => {
                let Some(info) = ck.mapped_info(t) else {
                    return Vec::new();
                };
                let mut v = vec![ck.mapped_constraint(&info)];
                for n in [info.name_node, info.template_node].into_iter().flatten() {
                    let raw = ck.type_from_node(Loc::new(info.decl.file, n));
                    v.push(ck.instantiate(raw, info.mapper));
                }
                v
            }
            _ => Vec::new(),
        })
    }

    fn mapped_key_type(&self, s: Sym) -> Option<Ty> {
        let ck = self.ck.borrow();
        ck.c.synth_sym(s).and_then(|x| x.mapped).map(|(_, key)| key)
    }

    fn decl_file(&self, d: &Decl) -> Option<(&Ast, &str)> {
        let ck = self.ck.borrow();
        let c: &ts_r_checker::Checker<'a> = ck.c;
        let f: &'a ts_r_checker::SourceFile = c.files.get(d.file as usize)?;
        Some((&f.ast, f.text.as_str()))
    }

    fn decl_path(&self, d: &Decl) -> Option<String> {
        let ck = self.ck.borrow();
        ck.c.files.get(d.file as usize).map(|f| f.path.clone())
    }

    fn export_of(&self, s: Sym, name: &str) -> Option<Sym> {
        self.ck.borrow().c.export_of(s, name)
    }

    fn file_export(&self, name: &str) -> Option<Sym> {
        let ck = self.ck.borrow();
        let file_symbol = ck.c.sym(self.file, ck.c.bound(self.file).file_symbol);
        ck.c.export_of(file_symbol, name)
    }

    fn global_symbol(&self, name: &str) -> Option<Sym> {
        self.ck
            .borrow()
            .c
            .global_symbol(name, ts_r_binder::meaning::ALL)
    }

    fn jsx_namespace(&self, at: NodeId) -> Option<Sym> {
        self.ck.borrow().c.jsx_namespace(self.loc(at))
    }
}

use crate::types::{Sig, Sym, Ty};
