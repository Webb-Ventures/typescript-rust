//! Types of value symbols: variables, parameters, properties, functions, classes,
//! namespaces, enum members, accessors and aliases; destructuring and iteration.

use rustc_hash::FxHashMap;
use ts_r_ast::{HeritageKind, MethodKind, Modifiers, Node, NodeId};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_types::{
    IndexInfo, MapperId, ObjectFlags, ObjectKind, ResolvedMembers, SymId, TupleElement, TypeFlags,
    TypeId, TypeTraits,
};

use crate::checker::{Ck, GlobalType, Loc, Resolving};
use crate::declared::anon;
use crate::nodes::{fn_parts, key_text, skip_parens};
use crate::symbols::{SynthSym, check_flags};

impl<'c, 'a> Ck<'c, 'a> {
    /// The (declared or inferred) type of a value symbol.
    pub fn type_of_symbol(&mut self, s: SymId) -> TypeId {
        if s.is_none() {
            return TypeId::ERROR;
        }
        if let Some(x) = self.c.synth_sym(s) {
            if let Some(t) = x.known_type() {
                return t;
            }
            let origins = x.origins.to_vec();
            let mapper = x.mapper;
            let check = x.check;
            let t = if let Some((mt, key)) = x.mapped {
                self.mapped_property_type(s, mt, key)
            } else if mapper != MapperId::IDENTITY && origins.len() == 1 {
                // An instantiation (of a member merged across declarations too: a method
                // overloaded in two global augmentations).
                let base = self.type_of_symbol(origins[0]);
                self.instantiate(base, mapper)
            } else if check & check_flags::MERGED_MEMBER != 0 {
                let t = self.symbol_anonymous(s, anon::SYMBOL_FUNCTION);
                if self.c.sym_flags(s).intersects(SymbolFlags::OPTIONAL) {
                    self.add_optionality(t)
                } else {
                    t
                }
            } else if let Some(&o) = origins.first() {
                self.type_of_symbol(o)
            } else {
                TypeId::ERROR
            };
            if let Some(x) = self.c.synth_sym(s) {
                let _ = x.ty.set(t);
                return x.known_type().unwrap_or(t);
            }
            return t;
        }
        if let Some(l) = self.c.sym_links(s)
            && let Some(&t) = l.ty.get()
        {
            return t;
        }
        // A type that depends on speculative contextual parameter types (or on ones taken
        // under loop assumptions) is kept only for the check of the function whose
        // parameters it depends on.
        let owner =
            if self.speculative > 0 || !self.context_overrides.is_empty() || self.flow_loop > 0 {
                self.speculation_owner(s)
            } else {
                None
            };
        if let Some(f) = owner
            && let Some(t) = self.spec_frame(f).and_then(|m| m.get(&s).copied())
        {
            return t;
        }
        if !self.push_resolution(Resolving::Type(s)) {
            return TypeId::ANY;
        }
        let t = self.compute_type_of_symbol(s);
        if !self.pop_resolution() {
            return TypeId::ANY;
        }
        if let Some(f) = owner {
            if let Some(i) = self.spec_frames.iter().rposition(|x| x.0 == f) {
                self.spec_frames[i].1.insert(s, t);
            }
            return t;
        }
        match self.c.sym_links(s) {
            Some(l) => {
                let _ = l.ty.set(t);
                *l.ty.get().unwrap_or(&t)
            }
            None => t,
        }
    }

    fn node_module_kind(&self) -> bool {
        matches!(
            self.c.opts.module,
            ts_r_config::ModuleKind::Node16
                | ts_r_config::ModuleKind::Node18
                | ts_r_config::ModuleKind::Node20
                | ts_r_config::ModuleKind::NodeNext
        )
    }

    /// The type of the JSON file module `s`: its value's widened type; under the Node
    /// module kinds (the ES module view) `{ default: value }` (observed).
    fn json_module_type(&mut self, s: SymId) -> Option<TypeId> {
        let v = self.json_value_type(s)?;
        if !self.node_module_kind() {
            return Some(v);
        }
        Some(self.json_with_default(v, &[]))
    }

    /// `{ ...props, default: v }`: a JSON module's view as an object with a `default`.
    fn json_with_default(&mut self, v: TypeId, props: &[(ts_r_types::StrId, SymId)]) -> TypeId {
        let s = self.store();
        let n = s.intern_str("default");
        let p = self
            .c
            .new_synth(SynthSym::new(n, SymbolFlags::PROPERTY, 0).with_type(v));
        let mut members: Vec<_> = props.iter().copied().filter(|&(k, _)| k != n).collect();
        members.push((n, p));
        s.literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ts_r_types::ResolvedMembers::new(members, Vec::new(), Vec::new(), Vec::new()),
            ts_r_types::TypeTraits::NONE,
        )
    }

    /// Under the Node module kinds, the type of alias `s` of JSON module `t` when it is
    /// not the module's own: a default import or import-equals is the value, and so is a
    /// CommonJS file's `import * as ns`, except that a value with its own `default`
    /// property gets the value's properties and `default: value` (tsc adds the synthetic
    /// default to a value with signatures or a `default` property, `resolveESModuleSymbol`;
    /// nodeModulesJson, nodeModulesResolveJsonModule).
    pub(crate) fn json_import_type(&mut self, s: SymId, t: SymId) -> Option<TypeId> {
        if !self.node_module_kind() {
            return None;
        }
        let d = self.c.first_decl(s)?;
        let v = self.json_value_type(t)?;
        let ast = self.ast(d.file);
        let parent = self.c.file(d.file).parents().parent(d.node);
        let kind = [Some(d.node), parent]
            .into_iter()
            .flatten()
            .map(|n| ast.node(n))
            .find(|n| {
                matches!(
                    n,
                    Node::ImportDefaultSpecifier { .. }
                        | Node::ImportNamespaceSpecifier { .. }
                        | Node::ImportEquals { .. }
                )
            });
        match kind {
            Some(Node::ImportDefaultSpecifier { .. } | Node::ImportEquals { .. }) => Some(v),
            Some(Node::ImportNamespaceSpecifier { .. }) if !self.c.file(d.file).esm_format => {
                if self.property_of_type(v, "default").is_none() {
                    return Some(v);
                }
                let props = self.properties_of(v);
                Some(self.json_with_default(v, &props))
            }
            _ => None,
        }
    }

    /// The type of JSON file module `s`'s value (widened; `any` for an empty file).
    pub(crate) fn json_value_type(&mut self, s: SymId) -> Option<TypeId> {
        let d = self.c.first_decl(s)?;
        if !self.c.file(d.file).is_json() {
            return None;
        }
        let ast = self.ast(d.file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return None;
        };
        let Some(&first) = ast.list(stmts).first() else {
            return Some(TypeId::ANY);
        };
        let Node::ExprStmt { expr } = *ast.node(first) else {
            return Some(TypeId::ANY);
        };
        let t = self.check_expr(Loc::new(d.file, expr));
        let t = self.widen_literal(t);
        Some(self.widened_type(t))
    }

    fn compute_type_of_symbol(&mut self, s: SymId) -> TypeId {
        let flags = self.c.sym_flags(s);
        // An alias merged with a local value (`import { A }` and `const A = 0`, or
        // `import * as Lib` and `namespace Lib { .. }`, TS2440): the local value
        // (tsc: `getTypeOfSymbol` reads values before aliases).
        let local_value = SymbolFlags::VARIABLE
            | SymbolFlags::FUNCTION
            | SymbolFlags::CLASS
            | SymbolFlags::REGULAR_ENUM
            | SymbolFlags::VALUE_MODULE;
        let merged_local = flags.intersects(SymbolFlags::ALIAS)
            && flags.intersects(local_value)
            && self.c.resolve_alias(s).is_some_and(|t| t != s);
        if flags.intersects(SymbolFlags::ALIAS) && !merged_local {
            let Some(t) = self.c.resolve_alias(s) else {
                // `import d from "m"` of a module without a default export (synthetic
                // default): the module itself (a JSON module's value).
                return match self.c.synthetic_default_module(s) {
                    Some(m) => match self.json_value_type(m) {
                        Some(v) => v,
                        None => self.type_of_symbol(m),
                    },
                    // `import { resolve } from "path"` where `path` is `export = p` with
                    // `const p: PlatformPath`: the property of the `export =` value.
                    None => self.export_equals_member_type(s).unwrap_or(TypeId::ERROR),
                };
            };
            if t == s {
                return TypeId::ERROR;
            }
            if let Some(v) = self.json_import_type(s, t) {
                return v;
            }
            if let Some(v) = self.namespace_import_module_type(s, t) {
                return v;
            }
            if !self.c.has_meaning(t, meaning::VALUE) {
                if let Some(v) = self.alias_merged_value(s) {
                    return self.type_of_symbol(v);
                }
                return self.export_equals_member_type(s).unwrap_or(TypeId::ERROR);
            }
            return self.type_of_symbol(t);
        }
        if flags.intersects(SymbolFlags::MODULE_FILE | SymbolFlags::VALUE_MODULE)
            && self.c.is_shorthand_ambient_module(s)
        {
            return TypeId::ANY;
        }
        if flags.intersects(SymbolFlags::MODULE_FILE) {
            // A JSON module (`resolveJsonModule`) is its value, widened like a `let`
            // initializer.
            if let Some(t) = self.json_module_type(s) {
                return t;
            }
            // `export =` modules are their assigned value.
            if let Some(eq) = self.c.export_of(s, "export=") {
                let eq = self.c.resolve_alias(eq).unwrap_or(eq);
                // A type or uninstantiated namespace has no value (the use reports it).
                if !self.c.has_meaning(eq, meaning::VALUE) {
                    return TypeId::ERROR;
                }
                return self.type_of_symbol(eq);
            }
            return self.symbol_anonymous(s, anon::NAMESPACE);
        }
        if flags.intersects(SymbolFlags::CLASS) {
            let st = self.symbol_anonymous(s, anon::CLASS_STATIC);
            // A mixin class (`class C extends base` with `base: T`) is also the type
            // variable it extends: `typeof C & T`.
            if let Some(tv) = self.base_type_variable(s) {
                return self.intersection(&[st, tv]);
            }
            return st;
        }
        if flags.intersects(SymbolFlags::FUNCTION) {
            return self.symbol_anonymous(s, anon::SYMBOL_FUNCTION);
        }
        if flags.intersects(SymbolFlags::METHOD) {
            let t = self.symbol_anonymous(s, anon::SYMBOL_FUNCTION);
            if flags.intersects(SymbolFlags::OPTIONAL) {
                return self.add_optionality(t);
            }
            return t;
        }
        if flags.intersects(SymbolFlags::ENUM_MEMBER) {
            return self.enum_member_type(s, true);
        }
        if flags.intersects(SymbolFlags::ACCESSOR) {
            return self.accessor_type(s);
        }
        if flags.intersects(SymbolFlags::VARIABLE | SymbolFlags::PROPERTY) {
            let t = self.variable_like_type(s);
            return t;
        }
        if flags.intersects(SymbolFlags::ENUM | SymbolFlags::VALUE_MODULE) {
            return self.symbol_anonymous(s, anon::NAMESPACE);
        }
        if flags.intersects(SymbolFlags::EXPORT_EXPRESSION) {
            if let Some(d) = self.c.first_decl(s) {
                let ast = self.ast(d.file);
                if let Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } =
                    *ast.node(d.node)
                {
                    // Widened like a declaration's initializer: an exported object literal
                    // is no longer fresh where it is imported.
                    let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, expr)));
                    return self.widened_type(t);
                }
                // JavaScript `module.exports = v`: the assigned value (`1`, as `export = 1`).
                // tsc unions several assignments (with its own property order); not
                // modelled: the first one.
                if let Node::Assign { left, right, .. } = *ast.node(d.node) {
                    // `/** @type {T} */ module.exports = v`: `T`.
                    if let Some(ty) = ast
                        .jsdoc_tags_of(left)
                        .iter()
                        .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
                        .and_then(|t| t.ty)
                    {
                        return self.type_from_node(Loc::new(d.file, ty));
                    }
                    return self.quietly(|ck| ck.check_expr(Loc::new(d.file, right)));
                }
            }
            return TypeId::ERROR;
        }
        TypeId::ERROR
    }

    /// The value of a named import whose export is only a type, taken from the property of
    /// the module's `export =` value of that name (see `Checker::export_equals_member`).
    /// `import * as ns from "m"` where `m` is `export =` a callable or constructable value
    /// (or one with a `default` property), or an ES module importing a CommonJS one under
    /// the Node module kinds: the namespace object is the value's properties without its
    /// signatures, plus a synthetic `default` (the value itself) when `m` can have a
    /// synthetic default (tsc: checker.go resolveESModuleSymbol,
    /// getTypeWithSyntheticDefaultImportType, cloneTypeAsModuleType). `None`: the value's
    /// own type.
    fn namespace_import_module_type(&mut self, s: SymId, t: SymId) -> Option<TypeId> {
        let (file, request) = self.namespace_import_request(s)?;
        let m = self
            .c
            .module_target_symbol(self.c.link.module_target(file, request))?;
        let esm_cjs = self.c.esm_imports_cjs(file, m);
        if self.c.export_of(m, "export=").is_none() && !esm_cjs {
            return None;
        }
        if !self.c.has_meaning(t, meaning::VALUE) {
            return None;
        }
        let ty = self.type_of_symbol(t);
        if !self
            .store()
            .flags(ty)
            .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION | TypeFlags::UNION)
            && !esm_cjs
        {
            return None;
        }
        let has_sigs = !self
            .signatures_of(ty, crate::members::SigKind::Call)
            .is_empty()
            || !self
                .signatures_of(ty, crate::members::SigKind::Construct)
                .is_empty();
        if !has_sigs && !esm_cjs && self.property_of_type(ty, "default").is_none() {
            return None;
        }
        let mut props: Vec<(ts_r_types::StrId, SymId)> = Vec::new();
        let mut index = Vec::new();
        if self
            .store()
            .flags(ty)
            .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            props = self.properties_of(ty);
            index = self.resolved_members(ty).index.to_vec();
        }
        if self.namespace_import_synthetic_default(file, m, ty) {
            let name = self.store().intern_str("default");
            props.retain(|&(n, _)| n != name);
            let dp = self
                .c
                .new_synth(SynthSym::new(name, SymbolFlags::PROPERTY, 0).with_type(ty));
            props.push((name, dp));
        }
        // Printed `typeof C` for a class, enum or namespace (tsc keeps the symbol; a
        // function's view prints its structure).
        // The `export =` entity names the view (`t` may be the module).
        let t = match self.c.export_of(m, "export=") {
            Some(eq) => self.c.resolve_alias(eq).unwrap_or(eq),
            None => t,
        };
        let named = self.c.sym_flags(t).intersects(
            SymbolFlags::CLASS
                | SymbolFlags::ENUM
                | SymbolFlags::VALUE_MODULE
                | SymbolFlags::MODULE_FILE,
        );
        Some(self.store().literal_object(
            t,
            if named {
                ObjectFlags::MODULE
            } else {
                ObjectFlags::NONE
            },
            ResolvedMembers::new(props, Vec::new(), Vec::new(), index),
            TypeTraits::NONE,
        ))
    }

    /// Whether `import * as ns` in `importer` of module `m` (`ty`: its value's type) gets a
    /// synthetic `default` (tsc: checker.go canHaveSyntheticDefault, which namespace
    /// imports consult without the `allowSyntheticDefaultImports` gate): an ES module
    /// importing a CommonJS one under the Node module kinds always does, an ES module
    /// importing an ES module never; TypeScript sources only with `export =`; JavaScript
    /// files when they are CommonJS without `__esModule`; declaration files and ambient
    /// modules unless their `default` (a property of the `export =` value's type when
    /// there is one) is declared as a default export, or they export `__esModule`.
    fn namespace_import_synthetic_default(
        &mut self,
        importer: ts_r_diagnostics::FileId,
        m: SymId,
        ty: TypeId,
    ) -> bool {
        if self.c.esm_imports_cjs(importer, m) {
            return true;
        }
        if self.c.esm_imports_esm(importer, m) {
            return false;
        }
        if let crate::symbols::SymKind::Binder(f, l) = self.c.sym_kind(m)
            && self.c.bound(f).file_symbol == l
            && !self.c.file(f).is_declaration()
        {
            if !self.c.file(f).is_js() {
                return self.c.export_of(m, "export=").is_some();
            }
            return self.c.bound(f).commonjs && self.c.export_of(m, "__esModule").is_none();
        }
        let eq = self.c.export_of(m, "export=").is_some();
        let export = |ck: &mut Self, name: &str| {
            if eq {
                ck.property_of_type(ty, name)
            } else {
                ck.c.export_of(m, name)
            }
        };
        if let Some(d) = export(self, "default") {
            let syntactic = self.c.decls(d).into_iter().any(|l| {
                let n = self.ast(l.file).node(l.node);
                matches!(
                    n,
                    Node::ExportDefaultExpr { .. } | Node::ExportSpecifier { .. }
                ) || n.modifiers().contains(Modifiers::DEFAULT)
            });
            if syntactic {
                return false;
            }
        }
        export(self, "__esModule").is_none()
    }

    /// Whether alias `s` is an `import * as ns` (not `export * as ns`).
    pub(crate) fn is_namespace_import(&self, s: SymId) -> bool {
        self.namespace_import_request(s).is_some()
    }

    fn namespace_import_request(&self, s: SymId) -> Option<(ts_r_diagnostics::FileId, u32)> {
        let crate::symbols::SymKind::Binder(file, local) = self.c.sym_kind(s) else {
            return None;
        };
        let info = self.c.bound(file).alias(local)?;
        let ts_r_binder::AliasTarget::Module {
            request,
            import: ts_r_binder::ImportName::Namespace,
        } = info.target
        else {
            return None;
        };
        let d = self.c.first_decl(s)?;
        matches!(
            self.ast(d.file).node(d.node),
            Node::ImportNamespaceSpecifier { .. }
        )
        .then_some((file, request))
    }

    pub fn export_equals_member_type(&mut self, s: SymId) -> Option<TypeId> {
        let (eq, name) = self.c.export_equals_member(s)?;
        let et = self.type_of_symbol(eq);
        let p = self.property_of_type(et, &name)?;
        Some(self.type_of_symbol(p))
    }

    /// The annotated parameter type of an accessor symbol's setter (class member or
    /// signature), the type writes must satisfy; `None` without one. An instantiated member
    /// gets its setter type instantiated (through every level of an instantiation of an
    /// instantiated member); union and intersection properties (several origins) are not
    /// modelled.
    pub(crate) fn setter_type(&mut self, s: SymId) -> Option<TypeId> {
        // A union property with a setter in some member is written as the union of the
        // members' write types (observed: `(One | Two).prop = v` checks `v` against each
        // member's setter or property type, joined).
        // An intersection property is written as the intersection of the members' write
        // types the same way (`(One & Two).prop = v`).
        if let Some(syn) = self.c.synth_sym(s)
            && syn.check & (check_flags::SYNTHETIC_UNION | check_flags::SYNTHETIC_INTERSECTION) != 0
            && syn.origins.len() > 1
        {
            let is_union = syn.check & check_flags::SYNTHETIC_UNION != 0;
            let origins = syn.origins.clone();
            let writes: Vec<Option<TypeId>> =
                origins.iter().map(|&o| self.setter_type(o)).collect();
            if writes.iter().all(Option::is_none) {
                return None;
            }
            let parts: Vec<TypeId> = origins
                .iter()
                .zip(writes)
                .map(|(&o, w)| w.unwrap_or_else(|| self.type_of_symbol(o)))
                .collect();
            return Some(if is_union {
                self.union(&parts)
            } else {
                self.intersection(&parts)
            });
        }
        if self.c.synth_sym(s).is_some() {
            let mut mappers = Vec::new();
            let mut cur = s;
            while let Some(syn) = self.c.synth_sym(cur) {
                let [origin] = syn.origins[..] else {
                    return None;
                };
                // A cycle, or a chain no real program builds.
                if origin == cur || mappers.len() >= 64 {
                    return None;
                }
                mappers.push(syn.mapper);
                cur = origin;
            }
            let mut w = self.setter_type(cur)?;
            for &mapper in mappers.iter().rev() {
                if mapper != MapperId::IDENTITY {
                    w = self.instantiate(w, mapper);
                }
            }
            return Some(w);
        }
        if !self.c.sym_flags(s).intersects(SymbolFlags::SET_ACCESSOR) {
            return None;
        }
        for d in self.c.decls(s) {
            let ast = self.ast(d.file);
            let params = match *ast.node(d.node) {
                Node::MethodSignature {
                    kind: ts_r_ast::MethodKind::Set,
                    params,
                    ..
                } => params,
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Set,
                    value,
                    ..
                } => fn_parts(ast, value)?.params,
                _ => continue,
            };
            let param = *ast
                .list(params)
                .iter()
                .find(|&&x| !crate::nodes::is_this_param(ast, x))?;
            if let Node::Param {
                type_ann: Some(t),
                optional,
                ..
            } = *ast.node(param)
            {
                let t = self.type_from_node(Loc::new(d.file, t));
                return Some(self.js_optional_setter_param(d.file, optional, t));
            }
            return None;
        }
        None
    }

    /// An optional JavaScript setter parameter (`@param {T=} v`, TS1051) accepts
    /// `undefined`; a TypeScript `v?: T` does not.
    fn js_optional_setter_param(
        &mut self,
        file: ts_r_diagnostics::FileId,
        optional: bool,
        t: TypeId,
    ) -> TypeId {
        if optional && self.c.file(file).is_js() {
            self.add_optionality(t)
        } else {
            t
        }
    }

    fn accessor_type(&mut self, s: SymId) -> TypeId {
        let decls = self.c.decls(s);
        // Accessor signatures (interfaces, type literals): the getter's annotation, else the
        // setter parameter's (`accessor_signatures`).
        for kind in [ts_r_ast::MethodKind::Get, ts_r_ast::MethodKind::Set] {
            for &d in &decls {
                let ast = self.ast(d.file);
                let Node::MethodSignature {
                    kind: k,
                    params,
                    return_type,
                    ..
                } = *ast.node(d.node)
                else {
                    continue;
                };
                if k != kind {
                    continue;
                }
                let ann = if kind == ts_r_ast::MethodKind::Get {
                    return_type
                } else {
                    ast.list(params)
                        .iter()
                        .find(|&&x| !crate::nodes::is_this_param(ast, x))
                        .and_then(|&x| match *ast.node(x) {
                            Node::Param { type_ann, .. } => type_ann,
                            _ => None,
                        })
                };
                if let Some(t) = ann {
                    return self.type_from_node(Loc::new(d.file, t));
                }
            }
        }
        // Getter first: its annotation (a JSDoc `@type` in JavaScript), else a setter
        // annotation, else the getter body.
        for &d in &decls {
            if let Node::MethodDef {
                kind: ts_r_ast::MethodKind::Get,
                value,
                ..
            } = *self.node(d)
            {
                if let Some(p) = fn_parts(self.ast(d.file), value)
                    && let Some(rt) = p.return_type
                {
                    return self.type_from_node(Loc::new(d.file, rt));
                }
                let ast = self.ast(d.file);
                if self.c.file(d.file).is_js()
                    && let Some(ty) = [d.node, value]
                        .iter()
                        .flat_map(|&n| ast.jsdoc_tags_of(n).iter())
                        .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
                        .and_then(|t| t.ty)
                    && !matches!(ast.node(ty), Node::FunctionType { .. })
                {
                    return self.type_from_node(Loc::new(d.file, ty));
                }
            }
        }
        for &d in &decls {
            if let Node::MethodDef {
                kind: ts_r_ast::MethodKind::Set,
                value,
                ..
            } = *self.node(d)
                && let Some(p) = fn_parts(self.ast(d.file), value)
            {
                let ast = self.ast(d.file);
                if let Some(&param) = ast
                    .list(p.params)
                    .iter()
                    .find(|&&x| !crate::nodes::is_this_param(ast, x))
                    && let Node::Param {
                        type_ann: Some(t),
                        optional,
                        ..
                    } = *ast.node(param)
                {
                    let t = self.type_from_node(Loc::new(d.file, t));
                    return self.js_optional_setter_param(d.file, optional, t);
                }
            }
        }
        for &d in &decls {
            if let Node::MethodDef {
                kind: ts_r_ast::MethodKind::Get,
                value,
                ..
            } = *self.node(d)
            {
                let sig = self.signature_from_decl(Loc::new(d.file, value));
                return self.return_type_of(sig);
            }
        }
        TypeId::ANY
    }

    /// Variables, parameters and properties.
    fn variable_like_type(&mut self, s: SymId) -> TypeId {
        let Some(d) = self.c.value_decl(s).or_else(|| self.c.first_decl(s)) else {
            return TypeId::ERROR;
        };
        let optional = self.c.sym_flags(s).intersects(SymbolFlags::OPTIONAL);
        let ast = self.ast(d.file);
        let t = match *ast.node(d.node) {
            Node::Param { .. } => return self.param_type(d),
            Node::PropertyDef {
                modifiers,
                type_ann,
                value,
                ..
            } => {
                if let Some(ta) = type_ann {
                    self.type_from_node(Loc::new(d.file, ta))
                } else if let Some(v) = value {
                    let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, v)));
                    if modifiers.contains(Modifiers::READONLY) {
                        t
                    } else {
                        let w = self.widen_literal(t);
                        self.widened_type(w)
                    }
                } else {
                    self.property_type_from_constructor(d, modifiers)
                        .unwrap_or(TypeId::IMPLICIT_ANY)
                }
            }
            Node::PropertySignature { type_ann, .. } => match type_ann {
                Some(ta) => self.type_from_node(Loc::new(d.file, ta)),
                None => TypeId::IMPLICIT_ANY,
            },
            Node::Property { value, .. } => {
                let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, value)));
                let w = self.widen_literal(t);
                self.widened_type(w)
            }
            Node::ShorthandProp { name, .. } => {
                let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, name)));
                let w = self.widen_literal(t);
                self.widened_type(w)
            }
            Node::Ident { .. } => {
                let t = self.binding_identifier_type(d);
                // An annotated variable keeps its annotation; its expando properties type
                // only the function initializer.
                let annotated = self.c.parent(d).is_some_and(|p| {
                    matches!(
                        self.node(p),
                        Node::VarDeclarator {
                            type_ann: Some(_),
                            ..
                        }
                    )
                });
                if annotated {
                    return t;
                }
                return self.with_expando_props(s, t);
            }
            Node::Member { .. } | Node::Index { .. } | Node::Call { .. } => self.expando_type(s),
            Node::EnumMember { .. } => self.enum_member_type(s, true),
            _ => TypeId::ANY,
        };
        if optional { self.add_optionality(t) } else { t }
    }

    /// An unannotated, uninitialised instance property of a TS class under `noImplicitAny`
    /// takes the type `this.<name>` has where the constructor returns (`readonly args;` then
    /// `this.args = [a, b] as const`).
    fn property_type_from_constructor(&mut self, d: Loc, modifiers: Modifiers) -> Option<TypeId> {
        if !self.c.opts.no_implicit_any
            || self.c.file(d.file).is_js()
            || modifiers.intersects(Modifiers::DECLARE)
        {
            return None;
        }
        let ast = self.ast(d.file);
        let Node::PropertyDef { key, .. } = *ast.node(d.node) else {
            return None;
        };
        let name = match *ast.node(key) {
            Node::Ident { .. }
            | Node::PrivateName { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. } => crate::nodes::key_text(ast, key)?,
            _ => return None,
        };
        let class = self.c.parent(d)?;
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) =
            *ast.node(class.node)
        else {
            return None;
        };
        // A static property: the first static block that assigns it a non-nullish value.
        if modifiers.contains(Modifiers::STATIC) {
            let blocks: Vec<NodeId> = ast
                .list(members)
                .iter()
                .copied()
                .filter(|&m| matches!(ast.node(m), Node::StaticBlock { .. }))
                .collect();
            return blocks
                .into_iter()
                .find_map(|b| self.property_type_in_constructor(d.file, b, &name));
        }
        let ctor = ast.list(members).iter().find_map(|&m| match *ast.node(m) {
            Node::MethodDef {
                kind: ts_r_ast::MethodKind::Constructor,
                value,
                ..
            } if crate::nodes::fn_parts(ast, value).is_some_and(|p| p.body.is_some()) => {
                Some(value)
            }
            _ => None,
        })?;
        self.property_type_in_constructor(d.file, ctor, &name)
    }

    /// Whether `left` (`this.p` or `this["p"]` in a constructor or static block) assigns a
    /// property whose type is inferred from those assignments: the assigned value has no
    /// contextual type.
    pub(crate) fn assigns_inferred_this_property(&self, left: Loc) -> bool {
        if !self.c.opts.no_implicit_any || self.c.file(left.file).is_js() {
            return false;
        }
        let ast = self.ast(left.file);
        let n = skip_parens(ast, left.node);
        let (obj, name) = match *ast.node(n) {
            Node::Member { obj, prop, .. } => (obj, crate::nodes::key_text(ast, prop)),
            Node::Index { obj, index, .. } => {
                let i = skip_parens(ast, index);
                match ast.node(i) {
                    Node::StringLit { .. } | Node::NumberLit { .. } => {
                        (obj, crate::nodes::key_text(ast, i))
                    }
                    _ => return false,
                }
            }
            _ => return false,
        };
        let Some(name) = name else { return false };
        if !matches!(ast.node(skip_parens(ast, obj)), Node::This {}) {
            return false;
        }
        let mut cur = Loc::new(left.file, n);
        let mut is_static = false;
        let class = loop {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::Arrow { .. } => {}
                Node::FunctionExpr { .. } => {
                    let Some(m) = self.c.parent(p) else {
                        return false;
                    };
                    if !matches!(
                        ast.node(m.node),
                        Node::MethodDef {
                            kind: ts_r_ast::MethodKind::Constructor,
                            ..
                        }
                    ) {
                        return false;
                    }
                    break self.c.parent(m);
                }
                Node::StaticBlock { .. } => {
                    is_static = true;
                    break self.c.parent(p);
                }
                Node::FunctionDecl { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::MethodDef { .. } => return false,
                _ => {}
            }
            cur = p;
        };
        let Some(class) = class else { return false };
        let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) =
            *ast.node(class.node)
        else {
            return false;
        };
        ast.list(members).iter().any(|&m| match *ast.node(m) {
            Node::PropertyDef {
                modifiers,
                key,
                type_ann: None,
                value: None,
                ..
            } => {
                modifiers.contains(Modifiers::STATIC) == is_static
                    && !modifiers.contains(Modifiers::DECLARE)
                    && crate::nodes::key_text(ast, key).as_deref() == Some(name.as_str())
            }
            _ => false,
        })
    }

    /// A variable holding a function with expando properties (`const f = () => 1;
    /// f.x = 1`) has the function's signatures and those properties.
    pub(crate) fn with_expando_props(&mut self, s: SymId, t: TypeId) -> TypeId {
        let props = self.expando_props(s);
        if props.is_empty() {
            return t;
        }
        let Some(ObjectKind::Literal { sym }) = self.store().object_of(t).map(|o| o.kind) else {
            return t;
        };
        let m = self.resolved_members(t);
        let (call, construct) = (m.call.to_vec(), m.construct.to_vec());
        let traits = self.store().traits(t) & TypeTraits::CONTAINS_TYPE_PARAMETER;
        // A JavaScript `{}` container (`var o = {}; o.x = 1`) stays an object literal.
        let flags = if call.is_empty() && construct.is_empty() && self.is_js_literal_type(t) {
            ObjectFlags::OBJECT_LITERAL
        } else {
            ObjectFlags::ANONYMOUS | ObjectFlags::FUNCTION_LIKE
        };
        self.store().literal_object(
            sym,
            flags,
            ResolvedMembers::new(props, call, construct, Vec::new()),
            traits,
        )
    }

    /// An expando property (`f.x = v`): the union of the widened types of its assigned
    /// values.
    fn expando_type(&mut self, s: SymId) -> TypeId {
        if self.annotated_expando_type(s).is_some() {
            let decls = self.c.decls(s).to_vec();
            return self.annotated_expando_values(&decls);
        }
        if let Some(t) = self.jsdoc_expando_type(s) {
            return t;
        }
        let mut types = Vec::new();
        let mut object_literals = 0;
        // A CommonJS export (`exports.x = {}`) is no container.
        let commonjs_export = self
            .c
            .sym_parent(s)
            .is_some_and(|p| self.c.sym_flags(p).intersects(SymbolFlags::MODULE_FILE));
        for d in self.c.decls(s) {
            // `Object.defineProperty(exports, "x", d)`.
            if let Node::Call { args, .. } = *self.node(d) {
                types.push(self.define_property_type(d.file, args));
                continue;
            }
            let Some(p) = self.c.parent(d) else {
                continue;
            };
            if let Node::Assign { left, right, .. } = *self.node(p)
                && left == d.node
            {
                let ast = self.ast(d.file);
                if let Node::ObjectLit { props } = *ast.node(skip_parens(ast, right)) {
                    // JavaScript containers (`f.ns = {}` then `f.ns.x = 1`) are not
                    // modelled.
                    if ast.list(props).is_empty() && self.c.file(d.file).is_js() && !commonjs_export
                    {
                        return TypeId::UNRESOLVED;
                    }
                    object_literals += 1;
                }
                let t = self.quietly(|ck| ck.check_expr(Loc::new(d.file, right)));
                let w = self.widen_literal(t);
                types.push(self.widened_type(w));
            }
        }
        if types.is_empty() {
            return TypeId::ANY;
        }
        // `x.f = undefined` (or `void 0`) declares nothing: `any` alone, else the other
        // assignments' types (observed).
        if types.iter().all(|&t| t == TypeId::UNDEFINED) {
            return TypeId::ANY;
        }
        types.retain(|&t| t != TypeId::UNDEFINED);
        let t = self.union(&types);
        // A union of object literals is normalized by tsc (missing properties become
        // optional `undefined`); not modelled.
        if object_literals > 1 && self.store().flags(t).contains(TypeFlags::UNION) {
            return TypeId::UNRESOLVED;
        }
        // A CommonJS export assigned values of different types keeps their literal types
        // (and sometimes `undefined`) in tsc; not modelled.
        if commonjs_export && self.store().flags(t).contains(TypeFlags::UNION) {
            return TypeId::UNRESOLVED;
        }
        // `this.x = v` only in a class's methods, never in its constructor, may be unset:
        // `| undefined` under strictNullChecks (tsc 7).
        if self.c.opts.strict_null_checks && self.this_property_only_in_methods(s) {
            return self.union(&[t, TypeId::UNDEFINED]);
        }
        t
    }

    /// Every declaration of `s` is a `this.x` assignment target in a class method or
    /// accessor (arrows see through), none in a constructor.
    fn this_property_only_in_methods(&self, s: SymId) -> bool {
        let mut any = false;
        for d in self.c.decls(s) {
            let ast = self.ast(d.file);
            let Node::Member { obj, .. } = *ast.node(d.node) else {
                return false;
            };
            if !matches!(ast.node(obj), Node::This {}) {
                return false;
            }
            let parents = self.c.file(d.file).parents();
            let mut cur = parents.parent(d.node);
            let mut in_method = false;
            while let Some(n) = cur {
                match ast.node(n) {
                    Node::Arrow { .. } => {}
                    Node::FunctionExpr { .. } => {
                        in_method = parents.parent(n).is_some_and(|m| {
                            matches!(
                                ast.node(m),
                                Node::MethodDef { kind, .. } if *kind != MethodKind::Constructor
                            ) && parents.parent(m).is_some_and(|c| match *ast.node(c) {
                                // A derived class may inherit the property (tsc then uses the
                                // base's type); not modelled, so no `undefined` there.
                                Node::ClassDecl { heritage, .. }
                                | Node::ClassExpr { heritage, .. } => {
                                    !ast.list(heritage).iter().any(|&h| {
                                        matches!(
                                            ast.node(h),
                                            Node::HeritageClause {
                                                kind: HeritageKind::Extends,
                                                ..
                                            }
                                        )
                                    })
                                }
                                _ => false,
                            })
                        });
                        break;
                    }
                    Node::FunctionDecl { .. }
                    | Node::MethodDef { .. }
                    | Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
                    | Node::StaticBlock { .. }
                    | Node::PropertyDef { .. } => break,
                    _ => {}
                }
                cur = parents.parent(n);
            }
            if !in_method {
                return false;
            }
            any = true;
        }
        any
    }

    /// `/** @type {T} */ f.x = v` (JavaScript): the type of the first declaration's tag.
    pub(crate) fn jsdoc_expando_type(&mut self, s: SymId) -> Option<TypeId> {
        for d in self.c.decls(s) {
            let tag = self
                .ast(d.file)
                .jsdoc_tags_of(d.node)
                .iter()
                .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
                .and_then(|t| t.ty);
            if let Some(ty) = tag {
                return Some(self.type_from_node(Loc::new(d.file, ty)));
            }
        }
        None
    }

    /// The type of a property defined by `Object.defineProperty(o, k, d)`: the descriptor's
    /// `value` (widened), its `get` method's return type, or its `set` method's parameter
    /// type; `any` otherwise.
    fn define_property_type(
        &mut self,
        file: ts_r_diagnostics::FileId,
        args: ts_r_ast::NodeList,
    ) -> TypeId {
        let ast = self.ast(file);
        let Some(&d) = ast.list(args).get(2) else {
            return TypeId::ANY;
        };
        let dt = self.quietly(|ck| ck.check_expr(Loc::new(file, d)));
        if let Some(p) = self.property_of_type(dt, "value") {
            let t = self.type_of_symbol(p);
            let w = self.widen_literal(t);
            return self.widened_type(w);
        }
        if let Some(p) = self.property_of_type(dt, "get") {
            let t = self.type_of_symbol(p);
            if let Some(&sig) = self.signatures_of(t, crate::members::SigKind::Call).first() {
                return self.return_type_of(sig);
            }
            return TypeId::ANY;
        }
        if let Some(p) = self.property_of_type(dt, "set") {
            let t = self.type_of_symbol(p);
            if let Some(&sig) = self.signatures_of(t, crate::members::SigKind::Call).first() {
                return self.param_type_at(sig, 0).unwrap_or(TypeId::ANY);
            }
        }
        TypeId::ANY
    }

    /// An expando property of an annotated variable (`const C: Comp = () => null;
    /// C.defaultProps = {...}`) that the annotation declares has the annotation's type.
    pub(crate) fn annotated_expando_type(&mut self, s: SymId) -> Option<TypeId> {
        let parent = self.c.sym_parent(s)?;
        let name = self.c.sym_name(s);
        self.annotated_expando_member(parent, name)
    }

    /// The type of an expando property the host's annotation declares: the union of its
    /// assigned values, each checked against the declared type as its contextual type and
    /// widened as in a mutable location (`f.done = true` under `done: true` stays `true`,
    /// `f.done = false` is `false` and fails the annotation; tsc:
    /// `getAssignmentDeclarationInitializerType` in `checker.go`).
    pub(crate) fn annotated_expando_values(&mut self, lefts: &[Loc]) -> TypeId {
        let mut types = Vec::new();
        for &left in lefts {
            let Some(p) = self.c.parent(left) else {
                continue;
            };
            let Node::Assign { left: l, right, .. } = *self.node(p) else {
                continue;
            };
            if l != left.node {
                continue;
            }
            if !self.push_resolution(crate::checker::Resolving::Node(left)) {
                types.push(TypeId::ANY);
                continue;
            }
            let t = self.quietly(|ck| ck.check_expr_mutable(Loc::new(left.file, right)));
            self.pop_resolution();
            types.push(self.widened_type(t));
        }
        if types.is_empty() {
            TypeId::ANY
        } else {
            self.union(&types)
        }
    }

    /// The type the annotation of the variable `host` (`const f: F = () => {}`) gives
    /// its expando property `name`, when the annotation declares it. Late-bound
    /// expandos (`f[k] = v`) use it too.
    pub(crate) fn annotated_expando_member(&mut self, host: SymId, name: &str) -> Option<TypeId> {
        let d = self.c.value_decl(host)?;
        let p = self.c.parent(d)?;
        let Node::VarDeclarator {
            type_ann: Some(ta), ..
        } = *self.node(p)
        else {
            return None;
        };
        let ann = self.type_from_node(Loc::new(p.file, ta));
        let prop = self.property_of_type(ann, name)?;
        let t = self.type_of_symbol(prop);
        Some(self.remove_missing(t))
    }

    /// The type of a declared binding identifier (variable, destructured element, catch
    /// variable, `for-in`/`for-of` variable).
    pub fn binding_identifier_type(&mut self, id: Loc) -> TypeId {
        let Some(parent) = self.c.parent(id) else {
            return TypeId::ANY;
        };
        let ast = self.ast(id.file);
        match *ast.node(parent.node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } if name == id.node => {
                if let Some(ta) = type_ann {
                    return self.type_from_node(Loc::new(id.file, ta));
                }
                if let Some(t) = self.for_variable_type(parent) {
                    // Widened like an initializer (`for (let x of [])` is `any` without
                    // `strictNullChecks`); destructuring patterns are not.
                    return self.widened_type(t);
                }
                let is_const = self.is_const_declaration(id);
                match init {
                    Some(i)
                        if !self.is_ambient_node(id)
                            && matches!(
                                ast.node(crate::nodes::skip_parens(ast, i)),
                                Node::ArrayLit { elements } if ast.list(*elements).is_empty()
                            ) =>
                    {
                        // `let xs = []` is an evolving array in tsc (its element type
                        // follows the pushes); not modelled yet, so stay silent.
                        self.create_array(TypeId::UNRESOLVED, false)
                    }
                    // `let x = null` / `= undefined` evolves like `let x;` under
                    // noImplicitAny (not `const`, ambient or exported declarations).
                    Some(i)
                        if !is_const
                            && (self.c.opts.no_implicit_any || self.c.file(id.file).is_js())
                            && !self.is_ambient_node(id)
                            && !self.is_exported_var(parent)
                            && is_null_or_undefined(ast, crate::nodes::skip_parens(ast, i)) =>
                    {
                        TypeId::AUTO
                    }
                    // `const A = Symbol()` declares a fresh `unique symbol` (`typeof A`).
                    Some(i)
                        if is_const
                            && self.c.sym_of_decl(id).is_some()
                            && self.is_global_symbol_call(Loc::new(id.file, i)) =>
                    {
                        self.quietly(|ck| ck.check_expr(Loc::new(id.file, i)));
                        match self.c.sym_of_decl(id) {
                            Some(s) => self.store().unique_symbol(s),
                            None => TypeId::SYMBOL,
                        }
                    }
                    Some(i) => {
                        let t = match self.quick_call_type(Loc::new(id.file, i)) {
                            Some(t) => t,
                            None => self.quietly(|ck| ck.check_expr(Loc::new(id.file, i))),
                        };
                        // JavaScript `var m = require("m")` is an alias of the module: its
                        // type is not widened (`module.exports = 1` is `1`).
                        if self.c.file(id.file).is_js()
                            && let Some(r) = self.commonjs_require_type(Loc::new(
                                id.file,
                                crate::nodes::skip_parens(ast, i),
                            ))
                        {
                            return r;
                        }
                        // A mutable variable widens literals; every variable widens a
                        // unique symbol initializer (`let s = A` and `const B = A` are
                        // `symbol`s), but only at the top level: `A | string` and
                        // `Awaited<T> | typeof A` keep it (observed in tsc 7).
                        let t = if is_const { t } else { self.widen_literal(t) };
                        // A mutable variable with a literal type assigned compound-like
                        // (`x = x + 1`, `x += 1`) has the literal's base type.
                        let t = if !is_const
                            && self.some_member(t, |m| {
                                self.store().flags(m).intersects(TypeFlags::LITERAL)
                            })
                            && self
                                .c
                                .sym_of_decl(id)
                                .is_some_and(|s| self.compound_assigned_vars(id.file).contains(&s))
                        {
                            self.base_of_literal(t)
                        } else {
                            t
                        };
                        let t = if self.store().flags(t).contains(TypeFlags::UNIQUE_ES_SYMBOL) {
                            TypeId::SYMBOL
                        } else {
                            t
                        };
                        self.widened_type(t)
                    }
                    None => {
                        if self.is_ambient_node(id) || is_const {
                            TypeId::ANY
                        } else {
                            TypeId::AUTO
                        }
                    }
                }
            }
            Node::CatchClause { type_ann, .. } => self.catch_variable_type(id.file, type_ann),
            Node::BindingProperty { .. }
            | Node::ArrayPattern { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. } => self.binding_element_type(id),
            _ => TypeId::ANY,
        }
    }

    /// The type of an initializer that calls (or awaits a call of) a function with one
    /// non-generic call signature (and no properties, index or construct signatures): its
    /// declared (awaited) return type, read without checking the arguments. A variable's type does not depend on the
    /// call's arguments then (`const s = link({ next() { const n = run(); return n } })`
    /// inside `run` is no cycle), as in tsc.
    fn quick_call_type(&mut self, init: Loc) -> Option<TypeId> {
        if self.c.file(init.file).is_js() {
            return None;
        }
        let ast = self.ast(init.file);
        let e = crate::nodes::skip_parens(ast, init.node);
        // `await f()`: the awaited return type.
        if let Node::Await { arg } = *ast.node(e) {
            let t = self.quick_call_type(Loc::new(init.file, arg))?;
            return Some(self.quietly(|ck| ck.awaited_type(t)));
        }
        let Node::Call {
            callee,
            optional: false,
            ..
        } = *ast.node(e)
        else {
            return None;
        };
        // Optional chains, `super(...)` and `import(...)` take the full check.
        let mut cur = callee;
        let mut steps = 0;
        loop {
            steps += 1;
            if steps > 10_000 {
                return None;
            }
            match *ast.node(cur) {
                Node::Member { optional: true, .. }
                | Node::Index { optional: true, .. }
                | Node::Call { optional: true, .. }
                | Node::Super {} => return None,
                Node::Member { obj, .. }
                | Node::Index { obj, .. }
                | Node::Call { callee: obj, .. } => {
                    cur = obj;
                }
                Node::Paren { expr } | Node::NonNull { expr } => cur = expr,
                _ => break,
            }
        }
        let ct = self.quietly(|ck| ck.check_expr(Loc::new(init.file, callee)));
        let ct = self.non_nullable(ct);
        if !self.store().flags(ct).contains(TypeFlags::OBJECT) {
            return None;
        }
        let m = self.resolved_members(ct);
        if m.call.len() != 1
            || !m.construct.is_empty()
            || !m.props.is_empty()
            || !m.index.is_empty()
        {
            return None;
        }
        let sig = m.call[0];
        if self.store().sig(sig).type_params != ts_r_types::ListId::EMPTY {
            return None;
        }
        Some(self.return_type_of(sig))
    }

    /// The variables of `file` assigned compound-like: `x op= y` or `x = a op b` with an
    /// arithmetic, shift or bitwise operator.
    pub(crate) fn compound_assigned_vars(
        &mut self,
        file: ts_r_diagnostics::FileId,
    ) -> std::rc::Rc<rustc_hash::FxHashSet<ts_r_types::SymId>> {
        if let Some(s) = self.compound_assigned.get(&file) {
            return s.clone();
        }
        use ts_r_ast::{AssignOp, BinaryOp};
        let ast = self.ast(file);
        let mut out = rustc_hash::FxHashSet::default();
        for n in (1..=ast.node_count() as u32).filter_map(ts_r_ast::NodeId::from_raw) {
            let Node::Assign { left, op, right } = *ast.node(n) else {
                continue;
            };
            let compound = match op {
                AssignOp::Assign => matches!(
                    *ast.node(crate::nodes::skip_parens(ast, right)),
                    Node::Binary {
                        op: BinaryOp::Add
                            | BinaryOp::Sub
                            | BinaryOp::Mul
                            | BinaryOp::Div
                            | BinaryOp::Mod
                            | BinaryOp::Exp
                            | BinaryOp::Shl
                            | BinaryOp::Shr
                            | BinaryOp::UShr,
                        ..
                    }
                ),
                AssignOp::OrAssign | AssignOp::AndAssign | AssignOp::CoalesceAssign => false,
                _ => true,
            };
            let target = crate::nodes::skip_parens(ast, left);
            if compound
                && matches!(ast.node(target), Node::Ident { .. })
                && let Some(s) = self.c.resolve_ident(Loc::new(file, target), meaning::VALUE)
            {
                out.insert(s);
            }
        }
        let rc = std::rc::Rc::new(out);
        self.compound_assigned.insert(file, rc.clone());
        rc
    }

    /// Whether `e` is a call of the global `Symbol` (`Symbol()`, `Symbol("desc")`,
    /// `Symbol.for("key")`).
    pub(crate) fn is_global_symbol_call(&mut self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let Node::Call {
            callee, type_args, ..
        } = *ast.node(crate::nodes::skip_parens(ast, e.node))
        else {
            return false;
        };
        if type_args.is_some() {
            return false;
        }
        // `Symbol.for("key")` declares a unique symbol too.
        let callee = match *ast.node(callee) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } if crate::nodes::ident_text(ast, prop) == Some("for") => obj,
            _ => callee,
        };
        let Node::Ident { name } = *ast.node(callee) else {
            return false;
        };
        if ast.atom(name) != "Symbol" {
            return false;
        }
        let global = self.c.global_symbol("Symbol", meaning::VALUE);
        global.is_some()
            && self
                .c
                .resolve_ident(Loc::new(e.file, callee), meaning::VALUE)
                == global
    }

    /// Whether the declarator `d` is in an `export var`/`let` statement.
    pub(crate) fn is_exported_var(&self, d: Loc) -> bool {
        self.c.parent(d).is_some_and(|l| {
            matches!(*self.node(l), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT))
        })
    }

    /// `for (const x of xs)` / `for (const k in o)`: the variable's type.
    pub(crate) fn for_variable_type(&mut self, declarator: Loc) -> Option<TypeId> {
        let decl = self.c.parent(declarator)?;
        let stmt = self.c.parent(decl)?;
        let ast = self.ast(stmt.file);
        match *ast.node(stmt.node) {
            Node::ForOf {
                left,
                right,
                is_await,
                ..
            } if left == decl.node => {
                let t = self.quietly(|ck| ck.check_expr(Loc::new(stmt.file, right)));
                Some(
                    self.iterated_type(t, is_await)
                        .unwrap_or(TypeId::UNRESOLVED),
                )
            }
            Node::ForIn { left, right, .. } if left == decl.node => {
                let t = self.quietly(|ck| ck.check_expr(Loc::new(stmt.file, right)));
                Some(self.for_in_key_type(t))
            }
            _ => None,
        }
    }

    /// The type of a `for-in` variable over a value of type `t`: `Extract<keyof T, string>`
    /// when `keyof T` stays generic (a type parameter, `this`), else `string`.
    fn for_in_key_type(&mut self, t: TypeId) -> TypeId {
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return TypeId::STRING;
        }
        let t = if self.store().flags(t).contains(TypeFlags::UNION) {
            self.non_nullable(t)
        } else {
            t
        };
        // `keyof (T & object)` is `keyof T | keyof object`, that is `keyof T`.
        let k = if self.store().flags(t).contains(TypeFlags::INTERSECTION) {
            let ms = self.store().members_of_union(t).to_vec();
            let keys: Vec<TypeId> = ms.into_iter().map(|m| self.keyof_type(m)).collect();
            self.union(&keys)
        } else {
            self.keyof_type(t)
        };
        if !self
            .store()
            .flags(k)
            .intersects(TypeFlags::INDEX | TypeFlags::TYPE_PARAMETER)
        {
            return TypeId::STRING;
        }
        if let Some(ex) = self.c.global_symbol("Extract", meaning::TYPE)
            && self.c.sym_flags(ex).intersects(SymbolFlags::TYPE_ALIAS)
        {
            let local = self.local_type_params(ex);
            if local.len() == 2 {
                let d = self.declared_type_of_symbol(ex);
                let m = self.store().mapper(&local, &[k, TypeId::STRING]);
                let r = self.instantiate(d, m);
                if !self.store().is_error(r) && r != TypeId::UNRESOLVED {
                    return r;
                }
            }
        }
        self.intersection(&[k, TypeId::STRING])
    }

    /// `t` with intersections whose required unit-typed properties conflict
    /// (`{ a: "foo" } & { a: "bar" }`) reduced to `never`, also as union members.
    pub(crate) fn reduce_conflicting_intersections(&mut self, t: TypeId) -> TypeId {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            if !ms
                .iter()
                .any(|&m| self.store().flags(m).contains(TypeFlags::INTERSECTION))
            {
                return t;
            }
            let kept: Vec<TypeId> = ms
                .iter()
                .copied()
                .filter(|&m| !self.is_conflicting_intersection(m))
                .collect();
            if kept.len() == ms.len() {
                return t;
            }
            return self.union(&kept);
        }
        if self.is_conflicting_intersection(t) {
            TypeId::NEVER
        } else {
            t
        }
    }

    fn is_conflicting_intersection(&mut self, t: TypeId) -> bool {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::INTERSECTION) {
            return false;
        }
        let ms = s.members_of_union(t).to_vec();
        let mut seen: Vec<(ts_r_types::StrId, TypeId)> = Vec::new();
        for m in ms {
            if !self.store().flags(m).contains(TypeFlags::OBJECT) {
                continue;
            }
            for (name, p) in self.properties_of(m) {
                if self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL) {
                    continue;
                }
                let pt = self.type_of_symbol(p);
                let f = self.store().flags(pt);
                if !f.intersects(TypeFlags::UNIT) || f.contains(TypeFlags::UNION) {
                    continue;
                }
                match seen.iter().find(|(n, _)| *n == name) {
                    Some(&(_, other)) if other != pt => return true,
                    Some(_) => {}
                    None => seen.push((name, pt)),
                }
            }
        }
        false
    }

    /// The element type produced by iterating a value of type `t`.
    pub fn iterated_type(&mut self, t: TypeId, is_await: bool) -> Option<TypeId> {
        let mut notes = IterNotes::default();
        self.iterated_type_noting(t, is_await, &mut notes)
    }

    /// [`Self::iterated_type`], recording in `notes` the sync iterator problems that still
    /// iterate (see [`IterNotes`]).
    pub(crate) fn iterated_type_noting(
        &mut self,
        t: TypeId,
        is_await: bool,
        notes: &mut IterNotes,
    ) -> Option<TypeId> {
        let t = self.reduce_conflicting_intersections(t);
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::ANY) {
            return Some(t);
        }
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            let mut parts = Vec::new();
            for m in ms {
                parts.push(self.iterated_type_noting(m, is_await, notes)?);
            }
            return Some(self.union(&parts));
        }
        if f.intersects(TypeFlags::STRING_LIKE) {
            return Some(TypeId::STRING);
        }
        if let Some(e) = self.element_type_of_array_like(t) {
            if is_await {
                return Some(self.awaited_type(e));
            }
            return Some(e);
        }
        // The iterator protocol: `[Symbol.iterator]().next()`'s `value` when not done.
        // `for await` also accepts sync iterables (their values are awaited).
        if is_await {
            if let Some(v) = self.iterated_by_protocol(t, "asyncIterator", true, notes) {
                return Some(v);
            }
            let v = self.iterated_by_protocol(t, "iterator", false, notes)?;
            return Some(self.awaited_type(v));
        }
        self.iterated_by_protocol(t, "iterator", false, notes)
    }

    /// `t[Symbol.<method>]().next()`'s non-done `value` type; `any` anywhere along the
    /// chain makes the element `any`.
    fn iterated_by_protocol(
        &mut self,
        t: TypeId,
        method: &str,
        is_await: bool,
        notes: &mut IterNotes,
    ) -> Option<TypeId> {
        let it_name = self.well_known_symbol_name(method)?;
        let p = self.property_of_type(t, &it_name)?;
        let pt = self.type_of_symbol(p);
        if self.store().flags(pt).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        // The method is called without arguments (observed: `[Symbol.iterator](_: number)`
        // does not make an iterable).
        let sigs = self.signatures_of(pt, crate::members::SigKind::Call);
        let sig = *sigs.iter().find(|&&s| self.store().sig(s).min_args == 0)?;
        let iter = self.return_type_of(sig);
        if self.store().flags(iter).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        // `return` and `throw` are optional, but when present they must be methods.
        if !is_await && notes.not_method.is_none() {
            for name in ["return", "throw"] {
                let Some(m) = self.quietly(|ck| ck.property_of_type(iter, name)) else {
                    continue;
                };
                let mt = self.type_of_symbol(m);
                let mt = self.non_nullable(mt);
                if !self
                    .store()
                    .flags(mt)
                    .intersects(TypeFlags::ANY | TypeFlags::NEVER)
                    && self
                        .signatures_of(mt, crate::members::SigKind::Call)
                        .is_empty()
                {
                    notes.not_method = Some(name);
                    break;
                }
            }
        }
        let next = self.property_of_type(iter, "next")?;
        let nt = self.type_of_symbol(next);
        if self.store().flags(nt).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        let nsig = *self
            .signatures_of(nt, crate::members::SigKind::Call)
            .first()?;
        let mut res = self.return_type_of(nsig);
        if is_await {
            let raw = res;
            res = self.awaited_type(res);
            // A `next()` thenable that can't be awaited (`{ then() {} }`) is tsc's TS1320,
            // not a missing async iterator; it iterates nothing.
            if res == TypeId::NEVER
                || (res == raw
                    && self
                        .quietly(|ck| ck.property_of_type(raw, "then"))
                        .is_some())
            {
                return Some(TypeId::NEVER);
            }
        }
        if self.store().flags(res).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        // `IteratorResult<T>` = `IteratorYieldResult<T> | IteratorReturnResult<R>`.
        let yields: Vec<TypeId> = self
            .union_members(res)
            .into_iter()
            .filter(|&m| {
                let done =
                    self.quietly(|ck| ck.property_of_type(m, "done").map(|d| ck.type_of_symbol(d)));
                !matches!(done, Some(TypeId::TRUE))
            })
            .collect();
        let mut vals = Vec::new();
        for y in yields {
            if let Some(v) = self.property_of_type(y, "value") {
                vals.push(self.type_of_symbol(v));
            }
        }
        if vals.is_empty() {
            // A sync iterator whose `next()` result has no `value` still iterates (as
            // `any`); the caller reports it.
            if !is_await {
                notes.no_value = true;
                return Some(TypeId::ANY);
            }
            return None;
        }
        let v = self.union(&vals);
        if is_await {
            return Some(self.awaited_type(v));
        }
        Some(v)
    }

    /// The value a `yield*` of a value of type `t` produces: the `value` of the
    /// `done: true` results of its iterator's `next()` (awaited in an async generator,
    /// which also delegates to sync iterables). `None` when no result says `done: true`.
    pub(crate) fn iterated_return_type(&mut self, t: TypeId, is_await: bool) -> Option<TypeId> {
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::ANY) {
            return Some(t);
        }
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            let mut parts = Vec::new();
            for m in ms {
                parts.push(self.iterated_return_type(m, is_await)?);
            }
            return Some(self.union(&parts));
        }
        let res = if is_await {
            match self.iterator_next_result(t, "asyncIterator") {
                Some(r) => r,
                None => self.iterator_next_result(t, "iterator")?,
            }
        } else {
            self.iterator_next_result(t, "iterator")?
        };
        let res = if is_await {
            self.awaited_type(res)
        } else {
            res
        };
        if self.store().flags(res).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        let mut vals = Vec::new();
        for m in self.union_members(res) {
            let done =
                self.quietly(|ck| ck.property_of_type(m, "done").map(|d| ck.type_of_symbol(d)));
            if done == Some(TypeId::TRUE)
                && let Some(v) = self.quietly(|ck| ck.property_of_type(m, "value"))
            {
                vals.push(self.type_of_symbol(v));
            }
        }
        if vals.is_empty() {
            return None;
        }
        let v = self.union(&vals);
        Some(if is_await { self.awaited_type(v) } else { v })
    }

    /// The return type of `t[Symbol.<method>]().next()`, quietly; `any` along the way is
    /// `any`.
    fn iterator_next_result(&mut self, t: TypeId, method: &str) -> Option<TypeId> {
        let it_name = self.well_known_symbol_name(method)?;
        let p = self.quietly(|ck| ck.property_of_type(t, &it_name))?;
        let pt = self.type_of_symbol(p);
        if self.store().flags(pt).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        let sigs = self.signatures_of(pt, crate::members::SigKind::Call);
        let sig = *sigs.iter().find(|&&s| self.store().sig(s).min_args == 0)?;
        let iter = self.return_type_of(sig);
        if self.store().flags(iter).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        let next = self.quietly(|ck| ck.property_of_type(iter, "next"))?;
        let nt = self.type_of_symbol(next);
        if self.store().flags(nt).intersects(TypeFlags::ANY) {
            return Some(TypeId::ANY);
        }
        let nsig = *self
            .signatures_of(nt, crate::members::SigKind::Call)
            .first()?;
        Some(self.return_type_of(nsig))
    }

    /// The property name of `Symbol.<name>`.
    pub fn well_known_symbol_name(&mut self, name: &str) -> Option<String> {
        let sc = self.c.global_symbol("SymbolConstructor", meaning::TYPE)?;
        let st = self.declared_type_of_symbol(sc);
        let p = self.property_of_type(st, name)?;
        let t = self.type_of_symbol(p);
        self.property_name_of_type(t)
    }

    /// `Awaited<T>` approximation: unwraps `PromiseLike` (`then`'s callback parameter).
    pub fn awaited_type(&mut self, t: TypeId) -> TypeId {
        self.awaited_type_in(t, &mut Vec::new())
    }

    /// [`Self::awaited_type`] given the thenables being unwrapped on this path, across unions:
    /// meeting one again is circular (tsc's TS1062), and at most 10 are unwrapped.
    fn awaited_type_in(&mut self, t: TypeId, path: &mut Vec<TypeId>) -> TypeId {
        let mut cur = t;
        loop {
            let s = self.store();
            let f = s.flags(cur);
            if f.intersects(TypeFlags::ANY | TypeFlags::UNKNOWN | TypeFlags::NEVER)
                || f.intersects(TypeFlags::PRIMITIVE) && !f.contains(TypeFlags::UNION)
            {
                return cur;
            }
            if f.contains(TypeFlags::UNION) {
                return self.map_type(cur, |ck, m| {
                    let n = path.len();
                    let r = ck.awaited_type_in(m, path);
                    path.truncate(n);
                    r
                });
            }
            if f.intersects(TypeFlags::INSTANTIABLE) {
                return cur;
            }
            if path.len() >= 10 || path.contains(&cur) {
                return TypeId::UNRESOLVED;
            }
            path.push(cur);
            let Some(then) = self.quietly(|ck| ck.property_of_type(cur, "then")) else {
                return cur;
            };
            let tt = self.type_of_symbol(then);
            let sigs = self.signatures_of(tt, crate::members::SigKind::Call);
            let Some(&sig) = sigs.first() else {
                return cur;
            };
            // A `then` without an `onfulfilled` parameter: a thenable that is no valid
            // promise, with no awaited type (tsc: getPromisedTypeOfPromise, TS1058/TS1320).
            let Some(cb) = self.store().sig(sig).params.first().map(|p| p.ty) else {
                return TypeId::UNRESOLVED;
            };
            let cb = self.non_nullable(cb);
            let cbsigs = self.signatures_of(cb, crate::members::SigKind::Call);
            let Some(&cbs) = cbsigs.first() else {
                return TypeId::UNRESOLVED;
            };
            let Some(v) = self.store().sig(cbs).params.first().map(|p| p.ty) else {
                return TypeId::UNRESOLVED;
            };
            cur = v;
        }
    }

    /// Whether `t` (or a member of the union `t`) is a thenable that is no valid promise:
    /// its `then` is callable, but no signature of it takes an `onfulfilled` callback
    /// (tsc: getAwaitedTypeNoAlias, isThenableType).
    pub(crate) fn is_invalid_thenable(&mut self, t: TypeId) -> bool {
        let members = if self.store().flags(t).contains(TypeFlags::UNION) {
            self.store().members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        members.into_iter().any(|m| {
            let f = self.store().flags(m);
            if !f.contains(TypeFlags::OBJECT) || f.intersects(TypeFlags::INSTANTIABLE) {
                return false;
            }
            let Some(then) = self.quietly(|ck| ck.property_of_type(m, "then")) else {
                return false;
            };
            let tt = self.type_of_symbol(then);
            let tt = self.non_nullable(tt);
            let sigs = self.signatures_of(tt, crate::members::SigKind::Call);
            !sigs.is_empty()
                && sigs.iter().all(|&s| {
                    let Some(cb) = self.store().sig(s).params.first().map(|p| p.ty) else {
                        return true;
                    };
                    let cb = self.non_nullable(cb);
                    !self.store().flags(cb).intersects(TypeFlags::ANY)
                        && self
                            .signatures_of(cb, crate::members::SigKind::Call)
                            .is_empty()
                })
        })
    }

    /// Promise<T> (or unresolved without lib).
    pub fn promise_of(&mut self, t: TypeId) -> TypeId {
        self.global_reference(GlobalType::Promise, &[t])
            .unwrap_or(TypeId::UNRESOLVED)
    }

    /// Whether a node is in an ambient context (`declare`, `.d.ts`).
    pub fn is_ambient_node(&self, l: Loc) -> bool {
        if self.c.file(l.file).is_declaration() {
            return true;
        }
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut cur = Some(l.node);
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            if ast.node(n).modifiers().contains(Modifiers::DECLARE) {
                return true;
            }
            cur = parents.parent(n);
        }
        false
    }

    /// A catch variable's type: `unknown` or `any` by `useUnknownInCatchVariables`, or its
    /// annotation, which must be one of those (any other is TS1196 and an error type).
    fn catch_variable_type(
        &mut self,
        file: ts_r_diagnostics::FileId,
        type_ann: Option<NodeId>,
    ) -> TypeId {
        let Some(ta) = type_ann else {
            return if self.c.opts.use_unknown_in_catch_variables {
                TypeId::UNKNOWN
            } else {
                TypeId::ANY
            };
        };
        let t = self.type_from_node(Loc::new(file, ta));
        if self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
            t
        } else {
            TypeId::ERROR
        }
    }

    /// The type of the pattern (or declaration) a binding element belongs to.
    pub fn pattern_parent_type(&mut self, pattern: Loc) -> TypeId {
        let Some(p) = self.c.parent(pattern) else {
            return TypeId::ANY;
        };
        let ast = self.ast(p.file);
        match *ast.node(p.node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } if name == pattern.node => {
                if let Some(ta) = type_ann {
                    return self.type_from_node(Loc::new(p.file, ta));
                }
                if let Some(t) = self.for_variable_type(p) {
                    return t;
                }
                match init {
                    Some(i) => {
                        let t = self.quietly(|ck| ck.check_expr(Loc::new(p.file, i)));
                        let t = self.widen_literal(t);
                        self.widened_type(t)
                    }
                    None => TypeId::ANY,
                }
            }
            Node::Param { pattern: pat, .. } if pat == pattern.node => self.param_type(p),
            Node::CatchClause { type_ann, .. } => self.catch_variable_type(p.file, type_ann),
            Node::BindingProperty { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. }
            | Node::ArrayPattern { .. } => self.element_type_in_pattern(pattern),
            _ => TypeId::ANY,
        }
    }

    /// The contextual type of a default in a binding pattern (`{ cb = (x) => {} }` or
    /// `[g = (z) => z]`): what the element receives from the destructured value.
    pub(crate) fn binding_default_context(&mut self, p: Loc) -> Option<TypeId> {
        let ast = self.ast(p.file);
        match *ast.node(p.node) {
            Node::BindingProperty { key, .. } => {
                let obj_pat = self.c.parent(p)?;
                let parent_t = self.pattern_parent_type(obj_pat);
                let name = key_text(ast, key)?;
                Some(self.destructured_property_type(parent_t, &name))
            }
            Node::AssignPattern { .. } => Some(self.element_type_in_pattern(p)),
            _ => None,
        }
    }

    /// `n` is inside a declaration's binding pattern (a variable or parameter), not an
    /// assignment target.
    pub(crate) fn in_binding_pattern(&self, n: Loc) -> bool {
        let mut cur = n;
        for _ in 0..256 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match *self.ast(p.file).node(p.node) {
                Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => cur = p,
                Node::VarDeclarator { name, .. } => return name == cur.node,
                Node::Param { pattern, .. } => return pattern == cur.node,
                _ => return false,
            }
        }
        false
    }

    /// Type of a binding identifier inside a pattern.
    fn binding_element_type(&mut self, id: Loc) -> TypeId {
        self.element_type_in_pattern(id)
    }

    /// The type the node at `n` (a binding identifier or nested pattern) receives.
    pub(crate) fn element_type_in_pattern(&mut self, n: Loc) -> TypeId {
        if self.depth > 200 {
            return TypeId::UNRESOLVED;
        }
        self.depth += 1;
        let r = self.element_type_in_pattern_inner(n);
        self.depth -= 1;
        r
    }

    /// The type of a rest element at `idx` of an array pattern over `parent_t`: the tuple's
    /// remaining elements, or an array of the element type.
    pub(crate) fn array_rest_type(&mut self, parent_t: TypeId, idx: usize) -> TypeId {
        // The rest of an `any` is `any` (observed: `rest.map((e) => ..)` is
        // then an untyped call whose callback parameters are implicitly any).
        if self.store().flags(parent_t).intersects(TypeFlags::ANY) {
            return parent_t;
        }
        if let Some(shape) = self.store().tuple_shape_of(parent_t) {
            let shape = shape.clone();
            let args = self.c.type_args_of_tuple(parent_t).to_vec();
            let mut els: Vec<crate::declared::TupleEl> = Vec::new();
            for (e, &ty) in shape.elements.iter().zip(args.iter()).skip(idx) {
                // A rest element takes its array type; the rest of
                // `[number, ...string[]]` is `string[]`.
                let ty = if e.kind == TupleElement::REST {
                    self.create_array(ty, false)
                } else {
                    ty
                };
                els.push(crate::declared::TupleEl {
                    ty,
                    kind: e.kind,
                    label: e.label,
                });
            }
            if let [only] = els.as_slice()
                && only.kind == TupleElement::REST
            {
                return only.ty;
            }
            return self.create_tuple(&els, false);
        }
        // The rest of a union of tuples and arrays is the union of their rests.
        if self.store().flags(parent_t).contains(TypeFlags::UNION) {
            let members = self.union_members(parent_t);
            if members
                .iter()
                .all(|&m| self.store().tuple_shape_of(m).is_some() || self.c.is_array_type(m))
            {
                let parts: Vec<TypeId> = members
                    .into_iter()
                    .map(|m| self.array_rest_type(m, idx))
                    .collect();
                return self.union(&parts);
            }
        }
        // The rest of an iterable is an array of what it iterates.
        let e = match self.element_type_of_array_like(parent_t) {
            Some(e) => e,
            None if self
                .store()
                .flags(parent_t)
                .intersects(TypeFlags::STRING_LIKE) =>
            {
                TypeId::UNRESOLVED
            }
            None => self
                .iterated_type(parent_t, false)
                .unwrap_or(TypeId::UNRESOLVED),
        };
        self.create_array(e, false)
    }

    fn element_type_in_pattern_inner(&mut self, n: Loc) -> TypeId {
        let Some(p) = self.c.parent(n) else {
            return TypeId::ANY;
        };
        let ast = self.ast(n.file);
        match *ast.node(p.node) {
            Node::BindingProperty { key, value, init } => {
                // `{ key: value = init }` / `{ key = init }`.
                let Some(obj_pat) = self.c.parent(p) else {
                    return TypeId::ANY;
                };
                let parent_t = self.pattern_parent_type(obj_pat);
                if value.is_some() && value != Some(n.node) && key == n.node {
                    return TypeId::ANY;
                }
                let name = match key_text(ast, key) {
                    Some(k) => Some(k),
                    None => match *ast.node(key) {
                        Node::ComputedKey { expr } => self.late_bound_name(Loc::new(n.file, expr)),
                        _ => None,
                    },
                };
                let mut t = match name {
                    Some(name) => self.destructured_property_type(parent_t, &name),
                    None => TypeId::UNRESOLVED,
                };
                t = self.destructured_flow_type(p, t);
                // `var { e = 1 } = {}`: the object literal initializer is typed by the
                // pattern, which gives it an optional `e`; the default supplies the type.
                if t == TypeId::ERROR
                    && init.is_some()
                    && let Some(d) = self.c.parent(obj_pat)
                    && let Node::VarDeclarator {
                        name: dn,
                        type_ann: None,
                        init: Some(di),
                        ..
                    } = *ast.node(d.node)
                    && dn == obj_pat.node
                    && matches!(ast.node(skip_parens(ast, di)), Node::ObjectLit { .. })
                {
                    t = TypeId::UNDEFINED;
                }
                if let Some(i) = init {
                    t = self.with_default(t, Loc::new(n.file, i));
                }
                t
            }
            Node::AssignPattern { left, right } if left == n.node => {
                let base = self.element_type_in_pattern(p);
                self.with_default(base, Loc::new(n.file, right))
            }
            Node::ArrayPattern { elements } => {
                let parent_t = self.pattern_parent_type(p);
                let idx = ast
                    .list(elements)
                    .iter()
                    .position(|&e| e == n.node)
                    .unwrap_or(0);
                self.array_element_at(parent_t, idx)
            }
            Node::Rest { .. } => {
                let Some(pp) = self.c.parent(p) else {
                    return TypeId::ANY;
                };
                match *ast.node(pp.node) {
                    Node::ArrayPattern { elements } => {
                        let parent_t = self.pattern_parent_type(pp);
                        let idx = ast
                            .list(elements)
                            .iter()
                            .position(|&e| e == p.node)
                            .unwrap_or(0);
                        self.array_rest_type(parent_t, idx)
                    }
                    Node::ObjectPattern { props } => {
                        let parent_t = self.pattern_parent_type(pp);
                        let mut omit = Vec::new();
                        for &e in ast.list(props) {
                            let Node::BindingProperty { key, .. } = *ast.node(e) else {
                                continue;
                            };
                            let name = match key_text(ast, key) {
                                Some(k) => Some(k),
                                None => match *ast.node(key) {
                                    Node::ComputedKey { expr } => {
                                        self.late_bound_name(Loc::new(n.file, expr))
                                    }
                                    _ => None,
                                },
                            };
                            match name {
                                Some(k) => omit.push(k),
                                None => return TypeId::UNRESOLVED,
                            }
                        }
                        self.object_rest_type(parent_t, &omit)
                    }
                    _ => TypeId::UNRESOLVED,
                }
            }
            _ => self.pattern_parent_type(n),
        }
    }

    /// The rest of a generic `parent` (tsc: `getRestType` in
    /// `tsc/internal/checker/checker.go`): `Omit<parent, K>` where `K` holds the named keys
    /// and the keys of the apparent type's properties a rest cannot take (methods and
    /// accessors of classes; private and protected ones add nothing). No such keys leave
    /// `parent` itself.
    fn generic_object_rest_type(&mut self, parent: TypeId, omit: &[String]) -> TypeId {
        let Some(alias) = self
            .c
            .global_symbol("Omit", ts_r_binder::meaning::TYPE)
            .filter(|&a| self.c.sym_flags(a).intersects(SymbolFlags::TYPE_ALIAS))
        else {
            return TypeId::UNRESOLVED;
        };
        let local = self.local_type_params(alias);
        if local.len() != 2 {
            return TypeId::UNRESOLVED;
        }
        let mut keys: Vec<TypeId> = omit.iter().map(|k| self.property_key_type(k)).collect();
        let apparent = self.apparent_type(parent);
        let class_instance = self.is_class_instance_type(apparent);
        for (n, p) in self.properties_of(apparent) {
            let name = self.store().str(n).to_owned();
            if omit.contains(&name) {
                continue;
            }
            if name.starts_with('#') || self.is_non_public_property(p) {
                continue;
            }
            if class_instance
                && self
                    .c
                    .sym_flags(p)
                    .intersects(SymbolFlags::METHOD | SymbolFlags::ACCESSOR)
            {
                keys.push(self.property_key_type(&name));
            }
        }
        if keys.is_empty() {
            return parent;
        }
        let key = self.union(&keys);
        let d = self.declared_type_of_symbol(alias);
        let m = self.store().mapper(&local, &[parent, key]);
        self.instantiate(d, m)
    }

    /// The type of `...rest` in an object binding pattern destructuring `parent`, the other
    /// elements binding `omit` (observed): the remaining public properties without
    /// `readonly`, the index signatures, and no signatures; class instances also lose their
    /// methods and accessors. Unions distribute. Generic parents give `Omit<T, K>`
    /// (`generic_object_rest_type`); arrays, tuples and primitives are not modelled.
    pub(crate) fn object_rest_type(&mut self, parent: TypeId, omit: &[String]) -> TypeId {
        let f = self.store().flags(parent);
        if f.intersects(TypeFlags::ANY) {
            return parent;
        }
        if f.contains(TypeFlags::UNION) {
            let members: Vec<TypeId> = self
                .store()
                .members_of_union(parent)
                .iter()
                .copied()
                .filter(|&m| !self.store().flags(m).intersects(TypeFlags::NULLABLE))
                .collect();
            let mut out = Vec::with_capacity(members.len());
            for m in members {
                let r = self.object_rest_type(m, omit);
                if r == TypeId::UNRESOLVED {
                    return r;
                }
                out.push(r);
            }
            return self.union(&out);
        }
        if self.is_generic_type(parent) {
            return self.generic_object_rest_type(parent, omit);
        }
        if !f.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
            || self.c.is_array_type(parent)
            || self.c.is_tuple_type(parent)
        {
            return TypeId::UNRESOLVED;
        }
        if f.contains(TypeFlags::INTERSECTION)
            && self
                .store()
                .members_of_union(parent)
                .iter()
                .any(|&m| !self.store().flags(m).contains(TypeFlags::OBJECT))
        {
            return TypeId::UNRESOLVED;
        }
        let class_instance = self.is_class_instance_type(parent);
        let mut props = Vec::new();
        let mut contains_tp = false;
        for (n, p) in self.properties_of(parent) {
            if omit.iter().any(|k| k == self.store().str(n)) {
                continue;
            }
            let pf = self.c.sym_flags(p);
            if class_instance && pf.intersects(SymbolFlags::METHOD | SymbolFlags::ACCESSOR) {
                continue;
            }
            let decl = match self.c.synth_sym(p) {
                Some(x) if x.origins.len() == 1 => self
                    .c
                    .value_decl(x.origins[0])
                    .or_else(|| self.c.first_decl(x.origins[0])),
                Some(x) => x.decl,
                None => self.c.value_decl(p).or_else(|| self.c.first_decl(p)),
            };
            if self.c.sym_check_flags(p) & (check_flags::PRIVATE | check_flags::PROTECTED) != 0
                || self.store().str(n).starts_with('#')
                || decl.is_some_and(|d| {
                    self.node(d)
                        .modifiers()
                        .intersects(Modifiers::PRIVATE | Modifiers::PROTECTED)
                })
            {
                continue;
            }
            let t = self.type_of_symbol(p);
            contains_tp |= self
                .store()
                .traits(t)
                .contains(TypeTraits::CONTAINS_TYPE_PARAMETER);
            let mut flags = if pf.intersects(SymbolFlags::METHOD) {
                SymbolFlags::METHOD
            } else {
                SymbolFlags::PROPERTY
            };
            flags |= pf & SymbolFlags::OPTIONAL;
            let np = self.c.new_synth(
                SynthSym::new(n, flags, check_flags::OBJECT_LITERAL)
                    .with_type(t)
                    .with_decl(decl),
            );
            props.push((n, np));
        }
        let mut index = Vec::new();
        if f.contains(TypeFlags::INTERSECTION) {
            for key in [TypeId::STRING, TypeId::NUMBER] {
                if let Some(i) = self.index_info_of(parent, key)
                    && i.key == key
                {
                    index.push(IndexInfo {
                        readonly: false,
                        ..i
                    });
                }
            }
        } else {
            for i in self.resolved_members(parent).index.iter() {
                index.push(IndexInfo {
                    readonly: false,
                    ..*i
                });
            }
        }
        for i in &index {
            contains_tp |= self
                .store()
                .traits(i.value)
                .contains(TypeTraits::CONTAINS_TYPE_PARAMETER);
        }
        let mut traits = TypeTraits::CONTAINS_OBJECT_LITERAL;
        if contains_tp {
            traits |= TypeTraits::CONTAINS_TYPE_PARAMETER;
        }
        self.store().literal_object(
            SymId::NONE,
            ObjectFlags::OBJECT_LITERAL,
            ResolvedMembers::new(props, Vec::new(), Vec::new(), index),
            traits,
        )
    }

    fn with_default(&mut self, t: TypeId, init: Loc) -> TypeId {
        let it = self.quietly(|ck| ck.check_expr(init));
        // The declared type is the default's contextual type: a literal it admits stays
        // literal (`{ v = 'a' }: { v?: 'a' | 'b' }` gives `'a' | 'b'`).
        let keep = !self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN)
            && t != TypeId::UNRESOLVED
            && self.is_literal_of_contextual_type(it, t);
        let it = if keep {
            self.regular_of_literal(it)
        } else if self.is_const_declaration(init) {
            it
        } else {
            self.widen_literal(it)
        };
        let base = if self.c.opts.strict_null_checks {
            self.filter_type(t, |ck, m| {
                !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
            })
        } else {
            t
        };
        // Under an annotated declaration the default only removes `undefined` (a default
        // that may be `undefined` keeps it); it is checked against the annotation instead.
        if self.pattern_root_annotated(init) && t != TypeId::UNRESOLVED {
            return if self.c.opts.strict_null_checks
                && !self.contains_undefined(it)
                && !self.store().flags(it).intersects(TypeFlags::ANY_OR_UNKNOWN)
            {
                base
            } else {
                t
            };
        }
        if self.store().flags(base).contains(TypeFlags::NEVER) {
            return it;
        }
        // An empty array default adds nothing to an array type (`{ xs = [] }: { xs?: T[] }`
        // gives `T[]`, not `T[] | never[]`).
        let empty_array = self
            .element_type_of_array_like(it)
            .is_some_and(|e| e == TypeId::NEVER || e == TypeId::IMPLICIT_NEVER);
        if empty_array && self.c.is_array_like_type(base) && self.is_assignable(it, base) {
            return base;
        }
        // Subtype reduction: `{ m = {} }` of `{ m?: { a?: 1 } }` is `{ a?: 1 }`.
        self.union_subtype(&[base, it])
    }

    /// Whether the declaration whose binding pattern contains `n` has a type annotation.
    fn pattern_root_annotated(&self, n: Loc) -> bool {
        let ast = self.ast(n.file);
        let mut cur = n;
        for _ in 0..256 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::VarDeclarator { type_ann, .. } => return type_ann.is_some(),
                Node::Param { type_ann, .. } => return type_ann.is_some(),
                Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => cur = p,
                _ => return false,
            }
        }
        false
    }

    pub(crate) fn destructured_property_type(&mut self, parent: TypeId, name: &str) -> TypeId {
        let s = self.store();
        if s.flags(parent).intersects(TypeFlags::ANY) {
            return parent;
        }
        match self.quietly(|ck| ck.property_of_type(parent, name)) {
            Some(p) => self.type_of_symbol(p),
            None => {
                let key = if name.parse::<f64>().is_ok() {
                    TypeId::NUMBER
                } else {
                    TypeId::STRING
                };
                match self.index_info_of(parent, key) {
                    Some(i) => {
                        if self.c.opts.no_unchecked_indexed_access {
                            self.add_undefined(i.value)
                        } else {
                            i.value
                        }
                    }
                    // A property of `unknown` or of an object type without it is the error
                    // type (TS2339 is reported at the key; `destructure_missing_property`).
                    None if parent == TypeId::UNKNOWN
                        || (self.store().flags(parent).contains(TypeFlags::OBJECT)
                            && !self
                                .store()
                                .flags(parent)
                                .intersects(TypeFlags::UNION | TypeFlags::INTERSECTION)) =>
                    {
                        TypeId::ERROR
                    }
                    None => TypeId::UNRESOLVED,
                }
            }
        }
    }

    /// The element at `idx` of tuple `t` with a rest element when `idx` is at or past the
    /// rest: the union of the rest element and every element after it (`[number,
    /// ...string[], boolean]` at 1 is `string | boolean`), under noUncheckedIndexedAccess
    /// with `undefined` from the count of non-rest elements on (observed: at 2 it is
    /// `string | boolean | undefined`, and `[...number[], number]` at 0 is `number`).
    /// `None` before the rest, for tuples without one and for variadic elements.
    pub(crate) fn tuple_rest_region_element(&mut self, t: TypeId, idx: usize) -> Option<TypeId> {
        let s = self.store();
        let shape = s.tuple_shape_of(t)?;
        if !shape.has_rest()
            || shape
                .elements
                .iter()
                .any(|e| e.kind == TupleElement::VARIADIC)
        {
            return None;
        }
        let lead = shape
            .elements
            .iter()
            .position(|e| e.kind == TupleElement::REST)?;
        let fixed = shape
            .elements
            .iter()
            .filter(|e| e.kind != TupleElement::REST)
            .count();
        let n = shape.elements.len();
        let args = self.c.type_args_of_tuple(t).to_vec();
        if idx < lead || args.len() != n {
            return None;
        }
        let mut parts = args[lead..].to_vec();
        if idx >= fixed && self.c.opts.no_unchecked_indexed_access {
            parts.push(TypeId::UNDEFINED);
        }
        Some(self.union(&parts))
    }

    pub(crate) fn array_element_at(&mut self, t: TypeId, idx: usize) -> TypeId {
        let s = self.store();
        if s.flags(t).intersects(TypeFlags::ANY) {
            return t;
        }
        if let Some(shape) = s.tuple_shape_of(t) {
            let shape = shape.clone();
            if let Some(r) = self.tuple_rest_region_element(t, idx) {
                return r;
            }
            let args = self.c.type_args_of_tuple(t).to_vec();
            if let Some(el) = shape.elements.get(idx) {
                let a = args.get(idx).copied().unwrap_or(TypeId::UNRESOLVED);
                return match el.kind {
                    TupleElement::OPTIONAL => self.add_optionality(a),
                    TupleElement::VARIADIC => TypeId::UNRESOLVED,
                    _ => a,
                };
            }
            if shape.has_rest() {
                let i = shape
                    .elements
                    .iter()
                    .position(|e| e.kind == TupleElement::REST)
                    .unwrap_or(0);
                return args.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
            }
            return TypeId::UNDEFINED;
        }
        // A union of tuples (and arrays) is indexed member by member: `const [a] = t` with
        // `t: readonly ['x', 'y'] | readonly ['z', 'w']` gives `'x' | 'z'`.
        if s.flags(t).contains(TypeFlags::UNION) {
            let members = self.union_members(t);
            if members
                .iter()
                .any(|&m| self.store().tuple_shape_of(m).is_some())
                && members
                    .iter()
                    .all(|&m| self.store().tuple_shape_of(m).is_some() || self.c.is_array_type(m))
            {
                let parts: Vec<TypeId> = members
                    .into_iter()
                    .map(|m| self.array_element_at(m, idx))
                    .collect();
                return self.union(&parts);
            }
        }
        if let Some(e) = self.element_type_of_array_like(t) {
            if self.c.opts.no_unchecked_indexed_access {
                return self.add_undefined(e);
            }
            return e;
        }
        if s.flags(t).intersects(TypeFlags::STRING_LIKE) {
            return TypeId::STRING;
        }
        match self.iterated_type(t, false) {
            // An element read from an iterable may be missing too (observed:
            // `const [x] = iterable` is `number | undefined` under noUncheckedIndexedAccess).
            Some(e) if self.c.opts.no_unchecked_indexed_access => self.add_undefined(e),
            Some(e) => e,
            None => TypeId::UNRESOLVED,
        }
    }

    /// The declared type of a node that declares a symbol (helper for flow analysis).
    pub fn declared_type_of_node(&mut self, l: Loc) -> TypeId {
        match self.c.sym_of_decl(l) {
            Some(s) => self.type_of_symbol(s),
            None => TypeId::ANY,
        }
    }

    /// Whether a symbol's declaration has no type annotation (its type is inferred).
    pub fn is_declared_without_annotation(&self, s: SymId) -> bool {
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        let ast = self.ast(d.file);
        match *ast.node(d.node) {
            Node::Ident { .. } => {
                // A binding of a destructuring pattern (`const [v] = args`) inherits from
                // the declarator or parameter that holds the pattern.
                let mut cur = d;
                for _ in 0..64 {
                    let Some(p) = self.c.parent(cur) else {
                        return false;
                    };
                    match *ast.node(p.node) {
                        Node::VarDeclarator { type_ann, .. } => return type_ann.is_none(),
                        Node::Param { type_ann, .. } => return cur != d && type_ann.is_none(),
                        Node::ArrayPattern { .. }
                        | Node::ObjectPattern { .. }
                        | Node::BindingProperty { .. }
                        | Node::AssignPattern { .. }
                        | Node::Rest { .. } => cur = p,
                        _ => return false,
                    }
                }
                false
            }
            Node::Param { type_ann, .. } => type_ann.is_none(),
            _ => false,
        }
    }

    /// Whether a symbol's type may depend on speculative contextual parameter types, and on
    /// which function's: it is declared without annotation inside a function expression, or
    /// inside an arrow with a contextually typed (unannotated) parameter; the innermost such
    /// function is returned. An arrow without one, such as a test body
    /// `async () => { ... }`, has nothing a contextual type changes in its locals, so they
    /// are cached as usual; otherwise every speculative check of the arrow would re-derive
    /// each local from its initializer, and chains of locals grow exponentially.
    fn speculation_owner(&self, s: SymId) -> Option<Loc> {
        if !self.is_declared_without_annotation(s) {
            return None;
        }
        let d = self.c.value_decl(s)?;
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let mut cur = parents.parent(d.node);
        while let Some(n) = cur {
            let l = Loc::new(d.file, n);
            match ast.node(n) {
                Node::FunctionExpr { .. } => return Some(l),
                Node::Arrow { .. } if self.is_context_sensitive_function(l) => return Some(l),
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    /// The symbol types kept for the innermost speculative check of function `f` (see
    /// `Ck::spec_frames`).
    fn spec_frame(&self, f: Loc) -> Option<&FxHashMap<SymId, TypeId>> {
        self.spec_frames
            .iter()
            .rev()
            .find(|x| x.0 == f)
            .map(|x| &x.1)
    }

    /// The `VarDeclarator` of a variable's binding identifier.
    pub fn declarator_of(&self, id: Loc) -> Option<Loc> {
        let p = self.c.parent(id)?;
        match *self.node(p) {
            Node::VarDeclarator { name, .. } if name == id.node => Some(p),
            _ => None,
        }
    }

    /// Skips parentheses of an expression location.
    pub fn skip_parens_loc(&self, l: Loc) -> Loc {
        Loc::new(l.file, skip_parens(self.ast(l.file), l.node))
    }

    pub fn is_class_instance_type(&self, t: TypeId) -> bool {
        let s = self.store();
        match s.object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Declared { sym, .. }) => {
                self.c.sym_flags(sym).intersects(SymbolFlags::CLASS)
            }
            Some(ObjectKind::Reference { target, .. }) => match s.object_of(target).map(|o| o.kind)
            {
                Some(ObjectKind::Declared { sym, .. }) => {
                    self.c.sym_flags(sym).intersects(SymbolFlags::CLASS)
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub fn node_loc(&self, file: ts_r_diagnostics::FileId, n: NodeId) -> Loc {
        Loc::new(file, n)
    }
}

/// `null` or the identifier `undefined`.
fn is_null_or_undefined(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    match *ast.node(n) {
        Node::NullLit {} => true,
        Node::Ident { name } => ast.atom(name) == "undefined",
        _ => false,
    }
}

/// Problems with a sync iterator that tsc reports but that still let it iterate.
#[derive(Default)]
pub(crate) struct IterNotes {
    /// `next()`'s result has no `value` (TS2490); the element is `any`.
    pub no_value: bool,
    /// A `return` or `throw` member that is not a method (TS2767).
    pub not_method: Option<&'static str>,
}
