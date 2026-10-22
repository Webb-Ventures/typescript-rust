//! JSDoc in JavaScript files. The parser writes JSDoc types into the tree as TypeScript
//! annotations (`ts_r_parser::attach_jsdoc`; nodes from it satisfy `Ast::is_jsdoc`); this
//! module holds what the checker does differently for them (observed in tsc 7):
//!
//! - Type names: `String`, `Number`, `Boolean`, `BigInt`, `Void`, `Undefined` and `Null` (the
//!   global ones, without type arguments) are the primitive types, `function` is
//!   `Function`, `Object<K, V>` is `Record<K, V>`, and `Object` is `any` without
//!   `noImplicitAny` (`Object` with it).
//! - A generic class or interface with missing type arguments takes `any` for them (extra
//!   ones are ignored); only under `noImplicitAny` is that TS2314.

use ts_r_ast::Node;
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_types::TypeId;

use crate::checker::{Ck, Loc};

impl<'c, 'a> Ck<'c, 'a> {
    /// The type of a JSDoc type reference that differs from TypeScript's, if it does.
    pub(crate) fn jsdoc_type_reference(
        &mut self,
        name: Loc,
        type_args: Option<Loc>,
    ) -> Option<TypeId> {
        if let Some(r) = self.jsdoc_required_module_type(name) {
            return Some(match r {
                Ok(Some(s)) => self.module_type_reference(s, type_args),
                Ok(None) => TypeId::UNRESOLVED,
                Err(()) => TypeId::ERROR,
            });
        }
        if self.jsdoc_require_name(name) {
            return Some(TypeId::UNRESOLVED);
        }
        let ast = self.ast(name.file);
        let Node::Ident { name: atom } = *ast.node(name.node) else {
            return None;
        };
        let text = ast.atom(atom);
        let n_args = type_args.map_or(0, |ta| match *ast.node(ta.node) {
            Node::TypeArgs { args } => ast.list(args).len(),
            _ => 0,
        });
        if !matches!(
            text,
            "String"
                | "Number"
                | "Boolean"
                | "Void"
                | "Undefined"
                | "Null"
                | "BigInt"
                | "Object"
                | "array"
                | "promise"
        ) {
            return None;
        }
        // Only the global declarations, not a local of that name.
        let global = self.c.global_symbol(text, meaning::TYPE);
        let local = self.c.resolve_ident(name, meaning::TYPE);
        if local.is_some() && local != global {
            return None;
        }
        if n_args == 0 {
            return match text {
                "String" => Some(TypeId::STRING),
                "Number" => Some(TypeId::NUMBER),
                "Boolean" => Some(TypeId::BOOLEAN),
                "Void" => Some(TypeId::VOID),
                "Undefined" => Some(TypeId::UNDEFINED),
                "Null" => Some(TypeId::NULL),
                "BigInt" => Some(TypeId::BIGINT),
                "Object" if !self.c.opts.no_implicit_any => Some(TypeId::ANY),
                // (Only without `noImplicitAny`: otherwise a lookup failure, TS2552.)
                "array" | "promise" if !self.c.opts.no_implicit_any => {
                    let g = if text == "array" { "Array" } else { "Promise" };
                    let sym = self.c.global_symbol(g, meaning::TYPE)?;
                    Some(self.type_from_symbol_reference(sym, &[TypeId::ANY]))
                }
                _ => None,
            };
        }
        if text == "Object" && n_args == 2 {
            let record = self.c.global_symbol("Record", meaning::TYPE)?;
            let ta = type_args?;
            let Node::TypeArgs { args } = *self.node(ta) else {
                return None;
            };
            let nodes = self.ast(ta.file).list(args).to_vec();
            let args: Vec<TypeId> = nodes
                .iter()
                .map(|&a| self.type_from_node(Loc::new(ta.file, a)))
                .collect();
            return Some(self.type_from_symbol_reference(record, &args));
        }
        None
    }

    /// A JavaScript function, method or constructor none of whose parameters has a type
    /// (no typed `@param`, no `@type` for the function): all its parameters are optional.
    pub(crate) fn is_untyped_js_signature(&self, l: Loc) -> bool {
        if !self.c.file(l.file).is_js() {
            return false;
        }
        let ast = self.ast(l.file);
        let f = match *ast.node(l.node) {
            Node::MethodDef { value, .. } => value,
            _ => l.node,
        };
        let params = match *ast.node(f) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. } => params,
            _ => return false,
        };
        if ast.is_jsdoc(f) {
            return false;
        }
        let typed_tag = ast.jsdoc_tags_of(f).iter().any(|t| match t.kind {
            ts_r_ast::JsDocTagKind::Param => t.ty.is_some(),
            ts_r_ast::JsDocTagKind::Type => true,
            _ => false,
        });
        let typed_param = ast.list(params).iter().any(|&p| {
            matches!(
                ast.node(p),
                Node::Param {
                    type_ann: Some(_),
                    ..
                }
            )
        });
        !typed_tag && !typed_param
    }

    /// TS8030: the `@type` of a function declaration has no call signature, several, or one
    /// with fewer parameters than the function declares (observed tsc 7).
    pub(crate) fn check_jsdoc_function_type(&mut self, f: Loc) {
        let Some(ty) = self.jsdoc_function_type_tag(f) else {
            return;
        };
        let tl = Loc::new(f.file, ty);
        let t = self.type_from_node(tl);
        if t == TypeId::UNRESOLVED {
            return;
        }
        // (`any` and the error type have no signature either.)
        let sigs = if self.store().is_error(t) || t == TypeId::ANY {
            Vec::new()
        } else {
            self.signatures_of(t, crate::members::SigKind::Call)
        };
        let too_few = match sigs.as_slice() {
            [s] => {
                let d = self.store().sig(*s);
                let ast = self.ast(f.file);
                let declared = crate::nodes::fn_parts(ast, f.node).map_or(0, |p| {
                    ast.list(p.params)
                        .iter()
                        .filter(|&&p| !crate::nodes::is_this_param(ast, p))
                        .count()
                });
                !d.flags.contains(ts_r_types::SigFlags::HAS_REST) && declared > d.params.len()
            }
            _ => true,
        };
        if too_few {
            self.error(
                tl,
                crate::diagnostics_generated::A_JSDOC_TYPE_TAG_ON_A_FUNCTION_MUST_HAVE_A_SIGNATURE_WITH_THE_CORRECT_NUMBER_OF_A_8030,
                &[],
            );
        }
    }

    /// The signature a JSDoc `@type` declares for a JavaScript function declaration (its
    /// only call signature, not generic): it types the parameters, and its return type is
    /// the function's declared return type (tsc 7 checks `return` statements against it).
    pub(crate) fn jsdoc_declared_signature(&mut self, f: Loc) -> Option<ts_r_types::SigId> {
        if !matches!(self.ast(f.file).node(f.node), Node::FunctionDecl { .. }) {
            return None;
        }
        let ty = self.jsdoc_function_type_tag(f)?;
        let t = self.type_from_node(Loc::new(f.file, ty));
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return None;
        }
        // (A generic signature is not modelled: its type parameters would be unbound.)
        match self
            .signatures_of(t, crate::members::SigKind::Call)
            .as_slice()
        {
            [s] if self.store().sig(*s).type_params == ts_r_types::ListId::EMPTY => Some(*s),
            _ => None,
        }
    }

    /// A failed `satisfies` (`e` is the `Satisfies` node; a JSDoc `@satisfies` became one
    /// too): elaborated into the expression as an assignment is (property errors keep
    /// their own messages), else reported at the `satisfies` keyword (the tag name of
    /// `@satisfies`) with TS1360 instead of TS2322 as the head (tsc 7). Returns false when
    /// the keyword is not found (the caller reports at the expression).
    pub(crate) fn report_satisfies(
        &mut self,
        e: Loc,
        ty: ts_r_ast::NodeId,
        source: TypeId,
        target: TypeId,
    ) -> bool {
        let ast = self.ast(e.file);
        let Node::Satisfies { expr, .. } = *ast.node(e.node) else {
            return false;
        };
        let text = self.c.file(e.file).text.as_bytes();
        let name = "satisfies".len();
        let span = if ast.is_jsdoc(e.node) {
            // The `satisfies` of `@satisfies {T}`, before the type's `{`.
            let mut i = ast.span(ty).start as usize;
            let mut steps = 0;
            while i > 0 && steps < 4096 && matches!(text[i - 1], b'{' | b' ' | b'\t' | b'(') {
                i -= 1;
                steps += 1;
            }
            if i < name + 1 || &text[i - name..i] != b"satisfies" || text[i - name - 1] != b'@' {
                return false;
            }
            ts_r_span::Span::new((i - name) as u32, i as u32)
        } else {
            // The keyword: the first token after the expression.
            let end = ast.span(ty).start as usize;
            let i = crate::nodes::skip_trivia(text, ast.span(expr).end as usize, end);
            if i + name > end || &text[i..i + name] != b"satisfies" {
                return false;
            }
            ts_r_span::Span::new(i as u32, (i + name) as u32)
        };
        let xl = Loc::new(e.file, expr);
        if self.elaborate_error(xl, source, target) {
            return true;
        }
        let rel = ts_r_types::RelationKind::Assignable;
        let (mut chain, at) = self.relation_error_chain(source, target, rel, None);
        if let Some(at) = at {
            self.error_chain(at, &chain);
            return true;
        }
        if chain.code() == 2322 {
            let head = crate::relate::Head {
                info:
                    crate::diagnostics_generated::TYPE_0_DOES_NOT_SATISFY_THE_EXPECTED_TYPE_1_1360,
            };
            chain = self.relation_error_chain(source, target, rel, Some(head)).0;
        }
        self.push_chain(e.file, span, &chain);
        true
    }

    /// The type node of a JSDoc `@type` tag on a JavaScript function (declaration,
    /// expression or arrow).
    pub(crate) fn jsdoc_function_type_tag(&self, f: Loc) -> Option<ts_r_ast::NodeId> {
        let ast = self.ast(f.file);
        if ast.jsdoc_tags.is_empty()
            || !matches!(
                ast.node(f.node),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            )
        {
            return None;
        }
        ast.jsdoc_tags_of(f.node)
            .iter()
            .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
            .and_then(|t| t.ty)
    }

    /// The JSDoc `@type` of an object literal property (`/** @type {T} */ a: v` or a
    /// shorthand `a`): its declared type.
    pub(crate) fn jsdoc_property_type(&mut self, prop: Loc) -> Option<TypeId> {
        let ast = self.ast(prop.file);
        if ast.jsdoc_tags.is_empty()
            || !matches!(
                ast.node(prop.node),
                Node::Property { .. } | Node::ShorthandProp { .. }
            )
        {
            return None;
        }
        let ty = ast
            .jsdoc_tags_of(prop.node)
            .iter()
            .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
            .and_then(|t| t.ty)?;
        Some(self.type_from_node(Loc::new(prop.file, ty)))
    }

    /// The JSDoc `@type` of `export default expr` (`/** @type {T} */ export default {}`): the
    /// type `expr` must be assignable to.
    pub(crate) fn jsdoc_export_default_type(&mut self, stmt: Loc) -> Option<TypeId> {
        let ast = self.ast(stmt.file);
        if ast.jsdoc_tags.is_empty()
            || !matches!(ast.node(stmt.node), Node::ExportDefaultExpr { .. })
        {
            return None;
        }
        let ty = ast
            .jsdoc_tags_of(stmt.node)
            .iter()
            .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
            .and_then(|t| t.ty)?;
        Some(self.type_from_node(Loc::new(stmt.file, ty)))
    }

    /// The type of an object literal written in a JavaScript file (tsc's JS literal).
    pub(crate) fn is_js_literal_type(&self, t: TypeId) -> bool {
        let Some(ts_r_types::ObjectKind::Literal { sym }) =
            self.store().object_of(t).map(|o| o.kind)
        else {
            return false;
        };
        if sym == ts_r_types::SymId::NONE {
            return false;
        }
        self.c.first_decl(sym).is_some_and(|d| {
            self.c.file(d.file).is_js()
                && matches!(self.ast(d.file).node(d.node), Node::ObjectLit { .. })
        })
    }

    /// A JSDoc type name whose first identifier is a `require` binding (see
    /// [`Ck::is_require_binding`]) or, for a qualified name, the root of a dotted
    /// `@typedef`/`@callback` name ([`ts_r_ast::Ast::jsdoc_namespaces`]): not modelled.
    pub(crate) fn jsdoc_require_name(&self, name: Loc) -> bool {
        let ast = self.ast(name.file);
        if !ast.is_jsdoc(name.node) {
            return false;
        }
        let mut n = name.node;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::QualifiedName { left, .. } => n = left,
                Node::Ident { .. } => break,
                _ => return false,
            }
        }
        if let Node::Ident { name: a } = *ast.node(n)
            && n != name.node
            && ast.jsdoc_namespaces.contains(&a)
        {
            return true;
        }
        let Some(s) = self.c.resolve_ident(Loc::new(name.file, n), meaning::VALUE) else {
            return false;
        };
        // A plain name bound to a member of a module (`const C = require("m").C`) is a
        // value: TS2749 like tsc 7; so is `const {C} = require("m")` of a non-class
        // CommonJS export (`require_destructured_value`).
        if n == name.node && !self.is_direct_require_binding(s) {
            return self.is_require_binding(s) && !self.require_destructured_value(s);
        }
        self.is_require_binding(s)
    }

    /// What a whole module names as a type (`import("m")` without a qualifier, or `M` for
    /// `const M = require("m")`), observed in tsc 7: the class, interface or other type of
    /// its `export =` or `module.exports =` (`Ok(Some)`), nothing modelled (`Ok(None)`), or
    /// no type (`Err`, TS1340 or TS2749): an ECMAScript module, a value or namespace, a
    /// CommonJS module exporting anything but a class.
    pub(crate) fn module_as_type(
        &self,
        module: ts_r_types::SymId,
    ) -> Result<Option<ts_r_types::SymId>, ()> {
        let Some(d) = self.c.first_decl(module) else {
            return Ok(None);
        };
        let sf = self.c.file(d.file);
        if sf.is_json() || self.c.export_of(module, "module.exports").is_some() {
            return Ok(None);
        }
        let cjs = sf.is_js() && sf.is_commonjs_module();
        let Some(eq) = self.c.export_of(module, "export=") else {
            return if cjs || sf.bound.is_module {
                Err(())
            } else {
                Ok(None)
            };
        };
        let is_type = |s: ts_r_types::SymId| {
            let s = self.c.resolve_alias(s).unwrap_or(s);
            if self.c.sym_flags(s).intersects(SymbolFlags::TYPE) {
                Ok(Some(s))
            } else {
                Err(())
            }
        };
        if !cjs {
            return match self.c.resolve_alias(eq) {
                Some(r) => is_type(r),
                None => Ok(None),
            };
        }
        let [e] = self.c.decls(eq)[..] else {
            return Ok(None);
        };
        let east = self.ast(e.file);
        let Node::Assign { right, .. } = *east.node(e.node) else {
            return Ok(None);
        };
        let right = crate::nodes::skip_parens(east, right);
        match *east.node(right) {
            Node::ClassExpr { .. } => Ok(self.c.sym_of_decl(Loc::new(e.file, right))),
            Node::Ident { .. } => match self
                .c
                .resolve_ident(Loc::new(e.file, right), meaning::VALUE)
            {
                Some(s) => is_type(s),
                None => Ok(None),
            },
            _ => Err(()),
        }
    }

    /// `M` in JSDoc for `const M = require("m")` (and no type named `M`): what the module
    /// names as a type (`module_as_type`); `None` for another name or an unresolved module.
    pub(crate) fn jsdoc_required_module_type(
        &self,
        name: Loc,
    ) -> Option<Result<Option<ts_r_types::SymId>, ()>> {
        let ast = self.ast(name.file);
        if !ast.is_jsdoc(name.node) || !matches!(ast.node(name.node), Node::Ident { .. }) {
            return None;
        }
        if self.c.resolve_ident(name, meaning::TYPE).is_some() {
            return None;
        }
        let s = self.c.resolve_ident(name, meaning::VALUE)?;
        let d = self.c.value_decl(s)?;
        let dast = self.ast(d.file);
        if !self.c.file(d.file).is_js() {
            return None;
        }
        let decl = match *dast.node(d.node) {
            Node::VarDeclarator { .. } => d.node,
            _ => self.c.file(d.file).parents().parent(d.node)?,
        };
        let Node::VarDeclarator {
            name: n,
            init: Some(i),
            ..
        } = *dast.node(decl)
        else {
            return None;
        };
        if !matches!(dast.node(n), Node::Ident { .. }) {
            return None;
        }
        let Node::Call { callee, args, .. } = *dast.node(crate::nodes::skip_parens(dast, i)) else {
            return None;
        };
        let [arg] = dast.list(args) else {
            return None;
        };
        if dast.name_of(callee) != Some("require")
            || !matches!(dast.node(callee), Node::Ident { .. })
            || !matches!(dast.node(*arg), Node::StringLit { .. })
        {
            return None;
        }
        let module = self.required_module(d.file, *arg)?;
        Some(self.module_as_type(module))
    }

    /// The type of a module's `module_as_type` symbol: its declared type, without type
    /// arguments only when it is not generic.
    pub(crate) fn module_type_reference(
        &mut self,
        s: ts_r_types::SymId,
        type_args: Option<Loc>,
    ) -> TypeId {
        let args: Vec<TypeId> = match type_args.map(|ta| (ta, *self.node(ta))) {
            Some((ta, Node::TypeArgs { args })) => {
                let nodes = self.ast(ta.file).list(args).to_vec();
                nodes
                    .iter()
                    .map(|&a| self.type_from_node(Loc::new(ta.file, a)))
                    .collect()
            }
            _ => Vec::new(),
        };
        if args.is_empty() && !self.local_type_params(s).is_empty() {
            return TypeId::UNRESOLVED;
        }
        self.type_from_symbol_reference(s, &args)
    }

    /// For `const {K} = require("m")` (a JavaScript binding `s`): whether `K` is an export
    /// that is only a value, observed in tsc 7: a property of an object literal
    /// `module.exports = {K}`, `exports.K = v` with `v` neither a class expression nor an
    /// identifier, or an ECMAScript export without type meaning. A member-chain
    /// initializer is always a value.
    fn require_destructured_value(&self, s: ts_r_types::SymId) -> bool {
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let mut key = None;
        let mut cur = d.node;
        let mut decl = None;
        for _ in 0..8 {
            match *ast.node(cur) {
                Node::BindingProperty { key: k, .. } if key.is_none() => key = Some(k),
                Node::VarDeclarator { .. } => {
                    decl = Some(cur);
                    break;
                }
                _ => {}
            }
            let Some(p) = parents.parent(cur) else {
                return false;
            };
            cur = p;
        }
        let Some(Node::VarDeclarator {
            name,
            init: Some(i),
            ..
        }) = decl.map(|d| *ast.node(d))
        else {
            return false;
        };
        if matches!(ast.node(name), Node::Ident { .. }) {
            // `const C = require("m").C`.
            return !matches!(ast.node(i), Node::Call { .. });
        }
        let (Some(key), Node::Call { args, .. }) = (key, *ast.node(i)) else {
            return false;
        };
        let (Some(text), [arg]) = (crate::nodes::ident_text(ast, key), ast.list(args)) else {
            return false;
        };
        let Some(module) = self.required_module(d.file, *arg) else {
            return false;
        };
        if let Some(eq) = self.c.export_of(module, "export=") {
            return self.c.decls(eq).iter().all(|e| {
                let east = self.ast(e.file);
                let value = match *east.node(e.node) {
                    Node::Assign { right, .. } => right,
                    _ => return false,
                };
                matches!(
                    east.node(crate::nodes::skip_parens(east, value)),
                    Node::ObjectLit { .. }
                )
            });
        }
        let Some(x) = self.c.export_of(module, text) else {
            return false;
        };
        if !self
            .c
            .sym_facts(x)
            .intersects(ts_r_binder::SymbolFacts::EXPANDO)
        {
            // An ECMAScript export: a value unless it has a type meaning (`export function
            // C` and `export var D` are values, `export class E` is a type).
            let r = self.c.resolve_alias(x).unwrap_or(x);
            return !self
                .c
                .sym_flags(r)
                .intersects(SymbolFlags::TYPE | SymbolFlags::ALIAS);
        }
        let decls = self.c.decls(x);
        let Some(first) = decls.first() else {
            return false;
        };
        let east = self.ast(first.file);
        let Some(p) = self.c.parent(*first) else {
            return false;
        };
        match *east.node(p.node) {
            Node::Assign { left, right, .. } if left == first.node => !matches!(
                east.node(crate::nodes::skip_parens(east, right)),
                Node::ClassExpr { .. } | Node::Ident { .. }
            ),
            _ => false,
        }
    }

    /// `const m = require("m")`: a variable named by an identifier and initialized with the
    /// call itself.
    fn is_direct_require_binding(&self, s: ts_r_types::SymId) -> bool {
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        if !self.c.file(d.file).is_js() {
            return false;
        }
        let ast = self.ast(d.file);
        let decl = match *ast.node(d.node) {
            Node::VarDeclarator { .. } => d.node,
            _ => match self.c.file(d.file).parents().parent(d.node) {
                Some(p) => p,
                None => return false,
            },
        };
        match *ast.node(decl) {
            Node::VarDeclarator {
                name,
                init: Some(i),
                ..
            } => {
                matches!(ast.node(name), Node::Ident { .. })
                    && matches!(ast.node(i), Node::Call { .. })
                    && is_require_call_chain(ast, i)
            }
            _ => false,
        }
    }

    /// `Object<K, V>` in JSDoc: not TS2315.
    pub(crate) fn is_jsdoc_record(&self, r: Loc) -> bool {
        let ast = self.ast(r.file);
        if !ast.is_jsdoc(r.node) {
            return false;
        }
        let Node::TypeRef {
            name,
            type_args: Some(ta),
        } = *ast.node(r.node)
        else {
            return false;
        };
        ast.name_of(name) == Some("Object")
            && matches!(*ast.node(ta), Node::TypeArgs { args } if ast.list(args).len() == 2)
    }

    /// A JavaScript variable bound by `require` (`const x = require("m")`, `const { K } =
    /// require("m")`, `require("m").x`): an import alias in tsc. Not modelled yet, so
    /// JSDoc type names through it are left unresolved and unreported.
    pub(crate) fn is_require_binding(&self, s: ts_r_types::SymId) -> bool {
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        if !self.c.file(d.file).is_js() {
            return false;
        }
        let ast = self.ast(d.file);
        let parents = self.c.file(d.file).parents();
        let mut cur = d.node;
        for _ in 0..8 {
            match *ast.node(cur) {
                Node::VarDeclarator { init: Some(i), .. } => {
                    return is_require_call_chain(ast, i);
                }
                Node::VarDeclarator { .. } => return false,
                _ => {}
            }
            let Some(p) = parents.parent(cur) else {
                return false;
            };
            cur = p;
        }
        false
    }
}

/// The JSDoc `@type` types of functions and assignment targets (they fill no field of the
/// tree), in tag order.
pub(crate) fn detached_jsdoc_types(ast: &ts_r_ast::Ast) -> Vec<ts_r_ast::NodeId> {
    let mut out = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    for t in &ast.jsdoc_tags {
        let Some(ty) = t.ty else { continue };
        if t.kind != ts_r_ast::JsDocTagKind::Type
            || !matches!(
                ast.node(t.host),
                Node::FunctionDecl { .. }
                    | Node::FunctionExpr { .. }
                    | Node::Arrow { .. }
                    | Node::Member { .. }
                    | Node::Index { .. }
                    | Node::Property { .. }
                    | Node::ShorthandProp { .. }
                    | Node::ExportDefaultExpr { .. }
            )
        {
            continue;
        }
        let mut child = false;
        ast.for_each_child(t.host, |c| child |= c == ty);
        if !child && seen.insert(ty) {
            out.push(ty);
        }
    }
    out
}

/// `require("m")`, possibly followed by `.x` accesses.
pub(crate) fn is_require_call_chain(ast: &ts_r_ast::Ast, mut e: ts_r_ast::NodeId) -> bool {
    for _ in 0..64 {
        match *ast.node(e) {
            Node::Paren { expr } => e = expr,
            Node::Member { obj, .. } => e = obj,
            Node::Call { callee, args, .. } => {
                return ast.name_of(callee) == Some("require")
                    && matches!(*ast.node(callee), Node::Ident { .. })
                    && ast.list(args).len() == 1
                    && matches!(ast.node(ast.list(args)[0]), Node::StringLit { .. });
            }
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::{Ast, JsDocTag, JsDocTagKind, KeywordTypeKind, Modifiers, SourceKind};
    use ts_r_span::Span;

    ts_r_testing::cap_memory!();

    #[test]
    fn many_detached_annotations_keep_tag_order_without_duplicates() {
        let mut ast = Ast::new(SourceKind::Js, 0);
        let value = ast.add(Node::Dummy {}, Span::EMPTY);
        let mut expected = Vec::new();
        for _ in 0..100_000 {
            let host = ast.add(
                Node::Property {
                    modifiers: Modifiers::default(),
                    key: value,
                    value,
                },
                Span::EMPTY,
            );
            let ty = ast.add(
                Node::KeywordType {
                    kind: KeywordTypeKind::Number,
                },
                Span::EMPTY,
            );
            let tag = JsDocTag {
                host,
                kind: JsDocTagKind::Type,
                ty: Some(ty),
                span: Span::EMPTY,
            };
            ast.jsdoc_tags.extend([tag, tag]);
            expected.push(ty);
        }
        ast.jsdoc_tags.reverse();
        expected.reverse();
        assert_eq!(detached_jsdoc_types(&ast), expected);
    }
}
