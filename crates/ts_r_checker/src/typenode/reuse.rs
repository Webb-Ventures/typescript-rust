//! Nodes of the enclosing file reused in built type nodes (tsc 7.0.2, observed with
//! `declarationMap`): where tsc writes part of an inferred type from the source's syntax,
//! the written text is the source's (`'x'` keeps its quotes, `Array<T>` stays) and the
//! declaration map maps it to the source.
//!
//! - **Annotations.** A parameter's annotation in a signature being written, a signature's
//!   return annotation and a property's annotation are reused when they write exactly the
//!   type being written (the same `TypeId`: an instantiated `Promise<T>` is rebuilt) and
//!   every name in them means the same at the enclosing declaration.
//! - **Initializers of object literal properties.** `x: e as T` writes `T`; a literal
//!   (`'x'`, `` `x` ``, `1`, `-1`, `1n`, not `true` or `null`) whose literal type is the
//!   property's type writes the literal; an array literal whose type is a tuple (`as const`)
//!   writes the tuple with its elements from their syntax; a nested object literal is
//!   written from its syntax.
//! - **Object literal keys.** An object literal written from its syntax keeps its keys'
//!   positions: each written name maps to the source key (`['k']` included). Only when the
//!   literal is "syntactic": no shorthand or spread member, no computed key other than a
//!   literal, no nested literal that is not, and no property value whose type is widened
//!   (`a: s` with `const s = 1` is `number`, not the value's `1`; a literal value itself is
//!   fine). Otherwise the object is written from its type and keys map nothing.
//!
//! Only nodes of the enclosing file are reused (they are already in the emitter's copy of
//! the file's tree); tsc maps nothing for nodes it copies from other files.

use ts_r_ast::visit::{Visitor, Walk, walk};
use ts_r_ast::{Node, NodeId, UnaryOp};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_span::Span;
use ts_r_types::{ObjectKind, SymId, TypeFlags, TypeId, TypeKind};

use super::{Built, TypeNodeCx};
use crate::checker::{Ck, Loc};
use crate::nodes::skip_parens;

/// Whether `ty` is the `const` of `as const` / `<const>`.
fn is_const_type(ast: &ts_r_ast::Ast, ty: NodeId) -> bool {
    match *ast.node(ty) {
        Node::KeywordType {
            kind: ts_r_ast::KeywordTypeKind::Const,
        } => true,
        Node::TypeRef {
            name,
            type_args: None,
        } => crate::nodes::ident_text(ast, name) == Some("const"),
        _ => false,
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// `ann`, a type node, when it can stand for `t` at `cx.enclosing` (see the module
    /// docs).
    pub(super) fn reuse_type_node(
        &mut self,
        ann: Loc,
        t: TypeId,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        let n = self.reuse_type_node_inner(ann, t, cx)?;
        // A reused annotation counts towards the serialized length like a written one
        // (observed: TS7056 for a spread of 66,750 `p000000: number` members).
        cx.approx_len += super::subtree_len(cx.out, n);
        Some(n)
    }

    fn reuse_type_node_inner(
        &mut self,
        ann: Loc,
        t: TypeId,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        if t == TypeId::UNRESOLVED {
            return None;
        }
        if self.type_from_node(ann) != t {
            return None;
        }
        if super::has_assert_import_type(self.ast(ann.file), ann.node) {
            return None;
        }
        if ann.file != cx.enclosing.file {
            // Another file's annotation is copied, its names written as they resolve here
            // (observed: text kept, `X` written `import("./a").X`, nothing mapped).
            let refs = cx.refs.len();
            let r = self.copy_foreign(ann.file, ann.node, cx, &mut Vec::new(), 0);
            if r.is_none() {
                cx.refs.truncate(refs);
            }
            return r;
        }
        self.names_mean_the_same(ann, cx).then_some(ann.node)
    }

    /// Type node `n` of `file` (not the enclosing file) copied into `cx.out`: names are
    /// written as they can be from the enclosing declaration, literals keep their text.
    /// `None` for what is not copied (`this`, `import()` types, template literal types,
    /// computed keys, names that cannot be written). `local` holds the type parameters
    /// declared inside the copy.
    fn copy_foreign(
        &mut self,
        file: ts_r_diagnostics::FileId,
        n: NodeId,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        depth: u32,
    ) -> Option<NodeId> {
        if depth > super::MAX_NODE_DEPTH {
            return None;
        }
        let d = depth + 1;
        let ast = self.ast(file);
        let node = *ast.node(n);
        Some(match node {
            Node::KeywordType { kind } => cx.add(Node::KeywordType { kind }),
            Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::Unary { .. }
            | Node::TemplateLit { .. } => {
                if let Node::TemplateLit { parts } = node
                    && ast.list(parts).len() != 1
                {
                    return None;
                }
                // The literal as written (`'x'` keeps its quotes): a name node holding its
                // text, which printers write as it is.
                let text = self.foreign_text(file, n)?;
                if text.contains('\n') {
                    return None;
                }
                let name = cx.ident(&text);
                cx.add(Node::TypeRef {
                    name,
                    type_args: None,
                })
            }
            Node::TypeRef { name, type_args } => {
                let name = self.foreign_name(file, name, cx, local)?;
                let type_args = self.copy_type_args(file, type_args, cx, local, d)?;
                cx.add(Node::TypeRef { name, type_args })
            }
            // `import("m").Name<A>`: the name as it is written from the enclosing file.
            Node::ImportType {
                is_typeof: false,
                arg,
                attributes: None,
                qualifier: Some(q),
                type_args,
            } => {
                let name = crate::nodes::ident_text(ast, q)?.to_owned();
                let target = self.import_type_export(file, arg, &name)?;
                let name = self.entity_of(target, meaning::TYPE, cx)?;
                let type_args = self.copy_type_args(file, type_args, cx, local, d)?;
                cx.add(Node::TypeRef { name, type_args })
            }
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                if matches!(ast.node(expr_name), Node::ImportType { .. }) {
                    return None;
                }
                let s = self.resolve_entity(Loc::new(file, expr_name), meaning::VALUE)?;
                let target = if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                    self.c.resolve_alias(s)?
                } else {
                    s
                };
                let expr_name = self.entity_of(target, meaning::VALUE, cx)?;
                let type_args = self.copy_type_args(file, type_args, cx, local, d)?;
                cx.add(Node::TypeQuery {
                    expr_name,
                    type_args,
                })
            }
            Node::ArrayType { elem } => {
                let elem = self.copy_foreign(file, elem, cx, local, d)?;
                cx.add(Node::ArrayType { elem })
            }
            Node::ParenType { ty } => {
                let ty = self.copy_foreign(file, ty, cx, local, d)?;
                cx.add(Node::ParenType { ty })
            }
            Node::OptionalType { ty } => {
                let ty = self.copy_foreign(file, ty, cx, local, d)?;
                cx.add(Node::OptionalType { ty })
            }
            Node::RestType { ty } => {
                let ty = self.copy_foreign(file, ty, cx, local, d)?;
                cx.add(Node::RestType { ty })
            }
            Node::TypeOperator { op, ty } => {
                let ty = self.copy_foreign(file, ty, cx, local, d)?;
                cx.add(Node::TypeOperator { op, ty })
            }
            Node::IndexedAccessType { obj, index } => {
                let obj = self.copy_foreign(file, obj, cx, local, d)?;
                let index = self.copy_foreign(file, index, cx, local, d)?;
                cx.add(Node::IndexedAccessType { obj, index })
            }
            Node::UnionType { types } => {
                let types = self.copy_list(file, types, cx, local, d)?;
                cx.add(Node::UnionType { types })
            }
            Node::IntersectionType { types } => {
                let types = self.copy_list(file, types, cx, local, d)?;
                cx.add(Node::IntersectionType { types })
            }
            Node::TupleType { elems } => {
                let elems = self.copy_list(file, elems, cx, local, d)?;
                cx.add(Node::TupleType { elems })
            }
            Node::NamedTupleMember {
                rest,
                name,
                optional,
                ty,
            } => {
                let name = self.copy_ident(file, name, cx)?;
                let ty = self.copy_foreign(file, ty, cx, local, d)?;
                cx.add(Node::NamedTupleMember {
                    rest,
                    name,
                    optional,
                    ty,
                })
            }
            Node::TypeLiteral { members } => {
                let members = self.copy_list(file, members, cx, local, d)?;
                cx.add(Node::TypeLiteral { members })
            }
            Node::PropertySignature {
                modifiers,
                key,
                optional,
                type_ann,
            } => {
                let key = self.copy_key(file, key, cx)?;
                let type_ann = self.copy_opt(file, type_ann, cx, local, d)?;
                cx.add(Node::PropertySignature {
                    modifiers,
                    key,
                    optional,
                    type_ann,
                })
            }
            Node::IndexSignature {
                modifiers,
                params,
                type_ann,
            } => {
                let params = self.copy_params(file, params, cx, local, d)?;
                let type_ann = self.copy_opt(file, type_ann, cx, local, d)?;
                cx.add(Node::IndexSignature {
                    modifiers,
                    params,
                    type_ann,
                })
            }
            Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::MethodSignature {
                kind: ts_r_ast::MethodKind::Method,
                ..
            }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. } => {
                let mark = local.len();
                let r = self.copy_signature(file, node, cx, local, d);
                local.truncate(mark);
                r?
            }
            _ => return None,
        })
    }

    /// A signature-like node of another file (its type parameters are in scope in it).
    fn copy_signature(
        &mut self,
        file: ts_r_diagnostics::FileId,
        node: Node,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        d: u32,
    ) -> Option<NodeId> {
        Some(match node {
            Node::FunctionType {
                type_params,
                params,
                return_type,
            } => {
                let type_params = self.copy_type_params(file, type_params, cx, local, d)?;
                let params = self.copy_params(file, params, cx, local, d)?;
                let return_type = self.copy_foreign(file, return_type, cx, local, d)?;
                cx.add(Node::FunctionType {
                    type_params,
                    params,
                    return_type,
                })
            }
            Node::ConstructorType {
                is_abstract,
                type_params,
                params,
                return_type,
            } => {
                let type_params = self.copy_type_params(file, type_params, cx, local, d)?;
                let params = self.copy_params(file, params, cx, local, d)?;
                let return_type = self.copy_foreign(file, return_type, cx, local, d)?;
                cx.add(Node::ConstructorType {
                    is_abstract,
                    type_params,
                    params,
                    return_type,
                })
            }
            Node::MethodSignature {
                modifiers,
                kind,
                key,
                optional,
                type_params,
                params,
                return_type,
            } => {
                let key = self.copy_key(file, key, cx)?;
                let type_params = self.copy_type_params(file, type_params, cx, local, d)?;
                let params = self.copy_params(file, params, cx, local, d)?;
                let return_type = self.copy_opt(file, return_type, cx, local, d)?;
                cx.add(Node::MethodSignature {
                    modifiers,
                    kind,
                    key,
                    optional,
                    type_params,
                    params,
                    return_type,
                })
            }
            Node::CallSignature {
                type_params,
                params,
                return_type,
            } => {
                let type_params = self.copy_type_params(file, type_params, cx, local, d)?;
                let params = self.copy_params(file, params, cx, local, d)?;
                let return_type = self.copy_opt(file, return_type, cx, local, d)?;
                cx.add(Node::CallSignature {
                    type_params,
                    params,
                    return_type,
                })
            }
            Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => {
                let type_params = self.copy_type_params(file, type_params, cx, local, d)?;
                let params = self.copy_params(file, params, cx, local, d)?;
                let return_type = self.copy_opt(file, return_type, cx, local, d)?;
                cx.add(Node::ConstructSignature {
                    type_params,
                    params,
                    return_type,
                })
            }
            _ => return None,
        })
    }

    /// What module `import(arg)` of `file` exports as `name` (aliases resolved).
    fn import_type_export(
        &mut self,
        file: ts_r_diagnostics::FileId,
        arg: NodeId,
        name: &str,
    ) -> Option<SymId> {
        let lit = match *self.ast(file).node(arg) {
            Node::TypeRef { name, .. } => name,
            _ => arg,
        };
        let req = self
            .c
            .bound(file)
            .module_requests
            .iter()
            .position(|r| r.node == lit || r.node == arg)?;
        let target = self.c.link.module_target(file, req as u32);
        let module = self.c.module_target_symbol(target)?;
        let (_, e) = self
            .c
            .exports_of(module)
            .into_iter()
            .find(|(n, _)| n == name)?;
        if self.c.sym_flags(e).intersects(SymbolFlags::ALIAS) {
            self.c.resolve_alias(e)
        } else {
            Some(e)
        }
    }

    /// The source text of node `n` of `file`.
    fn foreign_text(&self, file: ts_r_diagnostics::FileId, n: NodeId) -> Option<String> {
        let sp = self.ast(file).span(n);
        self.c
            .file(file)
            .text
            .get(sp.start as usize..sp.end as usize)
            .map(str::to_owned)
    }

    fn copy_opt(
        &mut self,
        file: ts_r_diagnostics::FileId,
        n: Option<NodeId>,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        depth: u32,
    ) -> Option<Option<NodeId>> {
        match n {
            Some(n) => Some(Some(self.copy_foreign(file, n, cx, local, depth)?)),
            None => Some(None),
        }
    }

    fn copy_list(
        &mut self,
        file: ts_r_diagnostics::FileId,
        l: ts_r_ast::NodeList,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        depth: u32,
    ) -> Option<ts_r_ast::NodeList> {
        let items = self.ast(file).list(l).to_vec();
        let mut out = Vec::with_capacity(items.len());
        for i in items {
            out.push(self.copy_foreign(file, i, cx, local, depth)?);
        }
        Some(cx.out.add_list(&out))
    }

    fn copy_type_args(
        &mut self,
        file: ts_r_diagnostics::FileId,
        args: Option<NodeId>,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        depth: u32,
    ) -> Option<Option<NodeId>> {
        let Some(a) = args else {
            return Some(None);
        };
        let Node::TypeArgs { args } = *self.ast(file).node(a) else {
            return None;
        };
        let args = self.copy_list(file, args, cx, local, depth)?;
        Some(Some(cx.add(Node::TypeArgs { args })))
    }

    /// Type parameters declared by a copied signature: their names are kept, and are in
    /// scope in the rest of the copy unless they would capture a generated name.
    fn copy_type_params(
        &mut self,
        file: ts_r_diagnostics::FileId,
        tps: Option<NodeId>,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        depth: u32,
    ) -> Option<Option<NodeId>> {
        let Some(tp) = tps else {
            return Some(None);
        };
        let ast = self.ast(file);
        let Node::TypeParams { params } = *ast.node(tp) else {
            return None;
        };
        let items = ast.list(params).to_vec();
        for &i in &items {
            if let Node::TypeParam { name, .. } = *ast.node(i) {
                let name = crate::nodes::ident_text(ast, name)?;
                if cx
                    .tp_names
                    .iter()
                    .chain(cx.infer_names.iter())
                    .any(|(_, n)| n == name)
                {
                    return None;
                }
                local.push(name.to_owned());
            }
        }
        let mut out = Vec::with_capacity(items.len());
        for i in items {
            let Node::TypeParam {
                modifiers,
                name,
                constraint,
                default,
            } = *self.ast(file).node(i)
            else {
                return None;
            };
            let name = self.copy_ident(file, name, cx)?;
            let constraint = self.copy_opt(file, constraint, cx, local, depth)?;
            let default = self.copy_opt(file, default, cx, local, depth)?;
            out.push(cx.add(Node::TypeParam {
                modifiers,
                name,
                constraint,
                default,
            }));
        }
        let params = cx.out.add_list(&out);
        Some(Some(cx.add(Node::TypeParams { params })))
    }

    fn copy_params(
        &mut self,
        file: ts_r_diagnostics::FileId,
        params: ts_r_ast::NodeList,
        cx: &mut TypeNodeCx,
        local: &mut Vec<String>,
        depth: u32,
    ) -> Option<ts_r_ast::NodeList> {
        let items = self.ast(file).list(params).to_vec();
        let mut out = Vec::with_capacity(items.len());
        for i in items {
            let Node::Param {
                modifiers,
                rest,
                pattern,
                optional,
                type_ann,
                init: None,
                ..
            } = *self.ast(file).node(i)
            else {
                return None;
            };
            let pattern = self.copy_ident(file, pattern, cx)?;
            let type_ann = self.copy_opt(file, type_ann, cx, local, depth)?;
            let decorators = cx.out.add_list(&[]);
            out.push(cx.add(Node::Param {
                decorators,
                modifiers,
                rest,
                pattern,
                optional,
                type_ann,
                init: None,
            }));
        }
        Some(cx.out.add_list(&out))
    }

    /// An identifier of another file, by its text.
    fn copy_ident(
        &mut self,
        file: ts_r_diagnostics::FileId,
        n: NodeId,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        let text = crate::nodes::ident_text(self.ast(file), n)?.to_owned();
        Some(cx.ident(&text))
    }

    /// A member key of another file: a name, or a string or number as written.
    fn copy_key(
        &mut self,
        file: ts_r_diagnostics::FileId,
        n: NodeId,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        match *self.ast(file).node(n) {
            Node::Ident { .. } => self.copy_ident(file, n, cx),
            Node::StringLit { .. } | Node::NumberLit { .. } => {
                let text = self.foreign_text(file, n)?;
                Some(cx.ident(&text))
            }
            _ => None,
        }
    }

    /// The name of a type reference of another file, written as it can be from the
    /// enclosing declaration.
    fn foreign_name(
        &mut self,
        file: ts_r_diagnostics::FileId,
        name: NodeId,
        cx: &mut TypeNodeCx,
        local: &[String],
    ) -> Option<NodeId> {
        if let Some(text) = crate::nodes::ident_text(self.ast(file), name)
            && local.iter().any(|l| l == text)
        {
            let text = text.to_owned();
            return Some(cx.ident(&text));
        }
        let s = self.resolve_entity(Loc::new(file, name), meaning::TYPE)?;
        if self.c.sym_flags(s).intersects(SymbolFlags::TYPE_PARAMETER) {
            let store = self.store();
            let written = cx
                .tp_names
                .iter()
                .rev()
                .find(|(tp, _)| store.type_param_data(*tp).is_some_and(|d| d.sym == s))
                .map(|(_, n)| n.clone())?;
            return Some(cx.ident(&written));
        }
        let target = if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
            self.c.resolve_alias(s)?
        } else {
            s
        };
        self.entity_of(target, meaning::TYPE, cx)
    }

    /// Whether every name in type node `n` resolves at `cx.enclosing` to what it resolves
    /// to where it is written (names declared inside `n`, type parameters of the
    /// signatures being written and of the enclosing declarations excepted). `this` types
    /// are never reused.
    fn names_mean_the_same(&mut self, n: Loc, cx: &mut TypeNodeCx) -> bool {
        struct Names {
            refs: Vec<(NodeId, u8)>,
            type_params: Vec<NodeId>,
            this: bool,
        }
        fn leftmost(ast: &ts_r_ast::Ast, mut e: NodeId) -> NodeId {
            for _ in 0..1000 {
                match *ast.node(e) {
                    Node::QualifiedName { left, .. } => e = left,
                    Node::Member { obj, .. } => e = obj,
                    _ => break,
                }
            }
            e
        }
        impl Visitor for Names {
            fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
                match *ast.node(id) {
                    Node::TypeRef { name, .. } => {
                        let m = if matches!(ast.node(name), Node::QualifiedName { .. }) {
                            meaning::NAMESPACE
                        } else {
                            meaning::TYPE
                        };
                        self.refs.push((leftmost(ast, name), m));
                    }
                    Node::TypeQuery { expr_name, .. } => {
                        if !matches!(ast.node(expr_name), Node::ImportType { .. }) {
                            self.refs.push((leftmost(ast, expr_name), meaning::VALUE));
                        }
                    }
                    Node::ComputedKey { expr } => {
                        let e = leftmost(ast, skip_parens(ast, expr));
                        if matches!(ast.node(e), Node::Ident { .. }) {
                            self.refs.push((e, meaning::VALUE));
                        }
                    }
                    Node::ThisType {} | Node::This {} => {
                        self.this = true;
                        return Walk::Stop;
                    }
                    Node::ExprWithTypeArgs { .. } => {
                        self.this = true;
                        return Walk::Stop;
                    }
                    Node::TypeParam { .. } => self.type_params.push(id),
                    _ => {}
                }
                Walk::Continue
            }
        }
        let ast = self.ast(n.file);
        let mut v = Names {
            refs: Vec::new(),
            type_params: Vec::new(),
            this: false,
        };
        walk(ast, n.node, &mut v);
        if v.this {
            return false;
        }
        // A type parameter declared inside (`infer T`) that the written type names otherwise
        // (`T_1`), or whose name another `infer` of the written type already has.
        for &tp in &v.type_params {
            let Node::TypeParam { name, .. } = *ast.node(tp) else {
                continue;
            };
            let (Some(text), Some(s)) = (
                crate::nodes::ident_text(ast, name),
                self.c.sym_of_decl(Loc::new(n.file, tp)),
            ) else {
                return false;
            };
            let store = self.store();
            let is_s = |t: &TypeId| store.type_param_data(*t).is_some_and(|d| d.sym == s);
            if cx
                .tp_names
                .iter()
                .chain(cx.infer_names.iter())
                .any(|(t, nm)| is_s(t) && nm != text)
                || cx.infer_names.iter().any(|(t, nm)| !is_s(t) && nm == text)
            {
                return false;
            }
        }
        let span = ast.span(n.node);
        let mut refs = Vec::new();
        for (id, m) in v.refs {
            let Some(name) = crate::nodes::ident_text(ast, id) else {
                return false;
            };
            let Some(s) = self.resolve_name_at(Loc::new(n.file, id), name, m) else {
                return false;
            };
            let decl = self.c.first_decl(s);
            if decl.is_some_and(|d| {
                d.file == n.file && {
                    let ds = self.ast(d.file).span(d.node);
                    span.start <= ds.start && ds.end <= span.end
                }
            }) {
                continue;
            }
            if self.c.sym_flags(s).intersects(SymbolFlags::TYPE_PARAMETER)
                && self.type_param_in_scope(s, name, cx)
            {
                continue;
            }
            if self.resolve_name_at(cx.enclosing, name, m) != Some(s) {
                return false;
            }
            refs.push(s);
        }
        cx.refs.extend(refs);
        true
    }

    /// Whether type parameter `s` is written as `name` at `cx.enclosing`: a type parameter of
    /// a signature being written (not renamed), or declared by the enclosing declaration or
    /// one around it.
    fn type_param_in_scope(&self, s: SymId, name: &str, cx: &TypeNodeCx) -> bool {
        let store = self.store();
        if cx
            .tp_names
            .iter()
            .any(|(tp, n)| store.type_param_data(*tp).is_some_and(|d| d.sym == s) && n == name)
        {
            return true;
        }
        let Some(d) = self.c.first_decl(s) else {
            return false;
        };
        if d.file != cx.enclosing.file {
            return false;
        }
        let parents = self.c.file(d.file).parents();
        let mut owner = parents.parent(d.node);
        // `TypeParam` -> `TypeParams` -> the declaring node.
        for _ in 0..2 {
            match owner {
                Some(o) if matches!(self.ast(d.file).node(o), Node::TypeParams { .. }) => {
                    owner = parents.parent(o);
                }
                _ => break,
            }
        }
        let Some(owner) = owner else { return false };
        let ast = self.ast(d.file);
        let (o, e) = (ast.span(owner), ast.span(cx.enclosing.node));
        o.start <= e.start && e.end <= o.end
    }

    /// The type node for expression `e` of the enclosing file whose type is `t`: from the
    /// expression's syntax where tsc writes it so (module docs), else from `t`.
    pub(super) fn expr_tn(&mut self, e: Loc, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        if cx.depth > super::MAX_NODE_DEPTH {
            return Err(super::NotNameable::TooDeep);
        }
        cx.depth += 1;
        let r = self.expr_tn_inner(e, t, cx);
        cx.depth -= 1;
        r
    }

    fn expr_tn_inner(&mut self, e: Loc, t: TypeId, cx: &mut TypeNodeCx) -> Built {
        let foreign = e.file != cx.enclosing.file;
        let ast = self.ast(e.file);
        let n = skip_parens(ast, e.node);
        match *ast.node(n) {
            Node::As { ty, expr } | Node::TypeAssertion { ty, expr } => {
                if is_const_type(ast, ty) {
                    return self.expr_tn(Loc::new(e.file, expr), t, cx);
                }
                match self.reuse_type_node(Loc::new(e.file, ty), t, cx) {
                    Some(r) => Ok(r),
                    None => self.tn(t, cx),
                }
            }
            Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::TemplateLit { .. }
            | Node::Unary { .. } => match self.reuse_literal(Loc::new(e.file, n), t) {
                // Another file's literal is copied as written (no mapping).
                Some(r) if foreign => match self.foreign_text(e.file, r) {
                    Some(text) if !text.contains('\n') => {
                        let name = cx.ident(&text);
                        Ok(cx.add(Node::TypeRef {
                            name,
                            type_args: None,
                        }))
                    }
                    _ => self.tn(t, cx),
                },
                Some(r) => Ok(r),
                None => {
                    let r = self.tn(t, cx)?;
                    // A number spelled otherwise (`0x10`) is written from its value, mapped
                    // to the literal (observed).
                    if !foreign && self.number_literal_of(Loc::new(e.file, n), t) {
                        cx.origins.push((r, ast.span(n)));
                    }
                    Ok(r)
                }
            },
            Node::ObjectLit { .. } => {
                let lit = Loc::new(e.file, n);
                if self.is_literal_type_of(t, lit) && self.syntactic_object(lit, t) {
                    cx.literal_keys = Some(n);
                    let r = self.tn(t, cx);
                    cx.literal_keys = None;
                    r
                } else {
                    self.tn(t, cx)
                }
            }
            // Another file's `as const` tuple is written from its type (observed).
            Node::ArrayLit { elements } if !foreign => {
                let elems = ast.list(elements).to_vec();
                match self.const_tuple(e.file, &elems, t, cx) {
                    Some(r) => r,
                    None => self.tn(t, cx),
                }
            }
            _ => self.tn(t, cx),
        }
    }

    /// Literal expression `e` (a string, no-substitution template, number or bigint,
    /// possibly negated) when its literal type is `t`.
    fn reuse_literal(&mut self, e: Loc, t: TypeId) -> Option<NodeId> {
        let ast = self.ast(e.file);
        let (lit, negated) = match *ast.node(e.node) {
            Node::Unary {
                op: UnaryOp::Minus,
                arg,
            } => (arg, true),
            Node::Unary { .. } => return None,
            _ => (e.node, false),
        };
        let s = self.store();
        let wanted = match (ast.node(lit), s.kind(t)) {
            (Node::StringLit { value }, TypeKind::StringLiteral(x)) if !negated => {
                ast.atom(*value) == s.str(x)
            }
            (Node::TemplateLit { parts }, TypeKind::StringLiteral(x)) if !negated => {
                match ast.list(*parts) {
                    [only] => match ast.node(*only) {
                        Node::TemplateElement {
                            cooked,
                            invalid_escape: false,
                        } => ast.atom(*cooked) == s.str(x),
                        _ => false,
                    },
                    _ => false,
                }
            }
            (Node::NumberLit { value }, TypeKind::NumberLiteral(..)) => {
                let v = if negated { -value.get() } else { value.get() };
                let sp = ast.span(lit);
                let raw = self
                    .c
                    .file(e.file)
                    .text
                    .get(sp.start as usize..sp.end as usize);
                // Other spellings (`0x10`) are written from the value (observed).
                // `-0` is written as written; its type is `0`.
                s.number_value(t).is_some_and(|tv| tv == v)
                    && raw == Some(ts_r_binder::js_number_to_string(value.get()).as_str())
            }
            (Node::BigIntLit { value }, TypeKind::BigIntLiteral(x)) => {
                let digits = ast.atom(*value);
                let want = s.str(x);
                if negated {
                    want.strip_prefix('-') == Some(digits)
                } else {
                    want == digits
                }
            }
            _ => false,
        };
        wanted.then_some(e.node)
    }

    /// Whether `e`, a number literal (possibly negated), has the value of literal type `t`.
    fn number_literal_of(&self, e: Loc, t: TypeId) -> bool {
        let ast = self.ast(e.file);
        let (lit, negated) = match *ast.node(e.node) {
            Node::Unary {
                op: UnaryOp::Minus,
                arg,
            } => (arg, true),
            _ => (e.node, false),
        };
        let Node::NumberLit { value } = *ast.node(lit) else {
            return false;
        };
        let v = if negated { -value.get() } else { value.get() };
        self.store().number_value(t).is_some_and(|tv| tv == v)
    }

    /// Whether `t` is the type of object literal `lit` (or its widened copy).
    fn is_literal_type_of(&self, t: TypeId, lit: Loc) -> bool {
        match self.store().object_of(t).map(|o| o.kind) {
            Some(ObjectKind::Literal { sym }) if !sym.is_none() && self.c.sym_exists(sym) => {
                self.c.first_decl(sym) == Some(lit)
            }
            _ => false,
        }
    }

    /// Whether object literal `lit` of type `t` is written from its syntax (module docs).
    pub(super) fn syntactic_object(&mut self, lit: Loc, t: TypeId) -> bool {
        let ast = self.ast(lit.file);
        let Node::ObjectLit { props } = *ast.node(lit.node) else {
            return false;
        };
        let key_ok = |key: NodeId| match *ast.node(key) {
            Node::ComputedKey { expr } => matches!(
                ast.node(skip_parens(ast, expr)),
                Node::StringLit { .. } | Node::NumberLit { .. }
            ),
            _ => true,
        };
        let mut members = None;
        for &m in ast.list(props).to_vec().iter() {
            match *ast.node(m) {
                Node::Property { key, value, .. } => {
                    if !key_ok(key) {
                        return false;
                    }
                    let v = skip_parens(ast, value);
                    let simple = match *ast.node(v) {
                        Node::StringLit { .. }
                        | Node::NumberLit { .. }
                        | Node::BigIntLit { .. }
                        | Node::BoolLit { .. }
                        | Node::NullLit {}
                        | Node::As { .. }
                        | Node::TypeAssertion { .. }
                        | Node::Arrow { .. }
                        | Node::FunctionExpr { .. }
                        | Node::ClassExpr { .. }
                        | Node::ArrayLit { .. } => v == value,
                        Node::TemplateLit { parts } => v == value && ast.list(parts).len() == 1,
                        Node::Unary {
                            op: UnaryOp::Minus,
                            arg,
                        } => {
                            v == value
                                && matches!(
                                    ast.node(arg),
                                    Node::NumberLit { .. } | Node::BigIntLit { .. }
                                )
                        }
                        _ => false,
                    };
                    if simple {
                        if self.infers_predicate(Loc::new(lit.file, v)) {
                            return false;
                        }
                        continue;
                    }
                    let members = members.get_or_insert_with(|| {
                        let mut members = rustc_hash::FxHashMap::default();
                        for &p in self.resolved_members(t).props.iter() {
                            if let Some(d) = self.c.first_decl(p) {
                                members.entry(d).or_insert(p);
                            }
                        }
                        members
                    });
                    let Some(&p) = members.get(&Loc::new(lit.file, m)) else {
                        return false;
                    };
                    let pt = self.type_of_symbol(p);
                    if let Node::ObjectLit { .. } = ast.node(v) {
                        if !self.syntactic_object(Loc::new(lit.file, v), pt) {
                            return false;
                        }
                        continue;
                    }
                    // `!x` of an operand that is always truthy or always falsy is a
                    // `true`/`false` literal for tsc, widened in the object (observed:
                    // `{ a: !s }` with `const s = 1`).
                    if let Node::Unary {
                        op: UnaryOp::Not,
                        arg,
                    } = *ast.node(v)
                    {
                        let at = self.check_expr(Loc::new(lit.file, arg));
                        let f = self.store().flags(at);
                        if f.intersects(
                            TypeFlags::LITERAL
                                | TypeFlags::NULLABLE
                                | TypeFlags::VOID
                                | TypeFlags::OBJECT
                                | TypeFlags::NON_PRIMITIVE,
                        ) && !f.contains(TypeFlags::UNION)
                        {
                            return false;
                        }
                    }
                    // A value whose type is widened in the object's type is not written
                    // from syntax (observed: `{ a: s }` with `const s = 1`).
                    if self.check_expr(Loc::new(lit.file, value)) != pt {
                        return false;
                    }
                }
                Node::MethodDef { key, value, .. } => {
                    if !key_ok(key) || self.infers_predicate(Loc::new(lit.file, value)) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }

    /// Whether `f`, a function or arrow without a return annotation, has an inferred type
    /// predicate (`(p: unknown) => isS(p)`): tsc does not write an object holding one from
    /// its syntax (observed).
    fn infers_predicate(&mut self, f: Loc) -> bool {
        let ast = self.ast(f.file);
        if !matches!(
            ast.node(f.node),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        ) {
            return false;
        }
        if crate::nodes::fn_parts(ast, f.node).is_none_or(|p| p.return_type.is_some()) {
            return false;
        }
        let sig = self.signature_from_decl(f);
        self.predicate_of(sig).is_some()
    }

    /// An array literal whose type `t` is a tuple of as many elements (`as const`): the
    /// tuple with each element written from its syntax.
    fn const_tuple(
        &mut self,
        file: ts_r_diagnostics::FileId,
        elems: &[NodeId],
        t: TypeId,
        cx: &mut TypeNodeCx,
    ) -> Option<Built> {
        let s = self.store();
        let o = s.object_of(t)?;
        let ObjectKind::Reference { target, args } = o.kind else {
            return None;
        };
        let shape = s.tuple_shape_of(target)?.clone();
        let args = s.list_items(args).to_vec();
        let ast = self.ast(file);
        if shape.elements.len() != elems.len()
            || args.len() < elems.len()
            || shape
                .elements
                .iter()
                .any(|e| e.kind != ts_r_types::TupleElement::REQUIRED || e.label.is_some())
            || elems
                .iter()
                .any(|&e| matches!(ast.node(e), Node::Spread { .. } | Node::Hole {}))
        {
            return None;
        }
        let mut out = Vec::with_capacity(elems.len());
        for (i, &e) in elems.iter().enumerate() {
            let n = match self.expr_tn(Loc::new(file, e), args[i], cx) {
                Ok(n) => n,
                Err(err) => return Some(Err(err)),
            };
            out.push(if super::needs_parens(cx.out, n, false) {
                cx.paren(n)
            } else {
                n
            });
        }
        let l = cx.out.add_list(&out);
        let tuple = cx.add(Node::TupleType { elems: l });
        Some(Ok(if shape.readonly {
            cx.add(Node::TypeOperator {
                op: ts_r_ast::TypeOperatorKind::Readonly,
                ty: tuple,
            })
        } else {
            tuple
        }))
    }

    /// The value of object literal property `p` when it is declared by `key: value`.
    pub(super) fn literal_prop_value(&self, p: SymId) -> Option<Loc> {
        let d = *self.c.decls(p).first()?;
        match *self.ast(d.file).node(d.node) {
            Node::Property { value, .. } => Some(Loc::new(d.file, value)),
            _ => None,
        }
    }

    /// The annotation of property `p`'s declaration (`a: T` in an interface, type literal
    /// or class).
    pub(super) fn prop_annotation(&self, p: SymId) -> Option<Loc> {
        // A copied property (a spread's) has the declarations of the one it copies.
        let mut p = p;
        for _ in 0..8 {
            match self.c.synth_sym(p) {
                Some(x) if x.decl.is_none() && x.origins.len() == 1 => p = x.origins[0],
                _ => break,
            }
        }
        let d = *self.c.decls(p).first()?;
        match *self.ast(d.file).node(d.node) {
            Node::PropertySignature {
                type_ann: Some(t), ..
            }
            | Node::PropertyDef {
                type_ann: Some(t), ..
            } => Some(Loc::new(d.file, t)),
            _ => None,
        }
    }

    /// Where the key of property `p` is written in object literal `lit`, when `p` is one of
    /// its members.
    pub(super) fn literal_key_span(&self, p: SymId, lit: NodeId, cx: &TypeNodeCx) -> Option<Span> {
        let d = *self.c.decls(p).first()?;
        self.member_key_span(d, lit, cx)
    }

    /// Where the key of object literal member `d` is written, when `d` is a member of `lit`.
    pub(super) fn member_key_span(&self, d: Loc, lit: NodeId, cx: &TypeNodeCx) -> Option<Span> {
        if d.file != cx.enclosing.file {
            return None;
        }
        let parent = self.c.file(d.file).parents().parent(d.node)?;
        if parent != lit {
            return None;
        }
        let ast = self.ast(d.file);
        let key = match *ast.node(d.node) {
            Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
            _ => return None,
        };
        Some(ast.span(key))
    }

    /// The declaration of signature `sig` (a method's function).
    fn sig_decl_parts(&self, sig: ts_r_types::SigId) -> Option<(Loc, crate::nodes::FnParts)> {
        let (f, n) = self.store().sig(sig).decl?;
        let file = ts_r_diagnostics::FileId(f);
        let node = NodeId::from_raw(n)?;
        let parts = crate::nodes::fn_parts(self.ast(file), node)?;
        Some((Loc::new(file, node), parts))
    }

    /// The return annotation of signature `sig`'s declaration.
    fn sig_return_annotation(&self, sig: ts_r_types::SigId) -> Option<Loc> {
        let (d, parts) = self.sig_decl_parts(sig)?;
        Some(Loc::new(d.file, parts.return_type?))
    }

    /// The annotation of signature `sig`'s `this` parameter.
    pub(super) fn sig_this_annotation(&self, sig: ts_r_types::SigId) -> Option<Loc> {
        let (d, parts) = self.sig_decl_parts(sig)?;
        let ast = self.ast(d.file);
        let &first = ast.list(parts.params).first()?;
        match *ast.node(first) {
            Node::Param {
                pattern,
                type_ann: Some(t),
                ..
            } if crate::nodes::ident_text(ast, pattern) == Some("this") => {
                Some(Loc::new(d.file, t))
            }
            _ => None,
        }
    }

    /// The annotation of parameter `p`'s declaration.
    pub(super) fn param_annotation(&self, p: SymId) -> Option<Loc> {
        if p == SymId::NONE {
            return None;
        }
        let d = self.c.first_decl(p)?;
        match *self.ast(d.file).node(d.node) {
            Node::Param {
                type_ann: Some(t), ..
            } => Some(Loc::new(d.file, t)),
            _ => None,
        }
    }

    /// The return annotation of the getter declaring accessor `p`.
    pub(super) fn getter_annotation(&self, p: SymId) -> Option<Loc> {
        self.c.decls(p).iter().find_map(|d| {
            let ast = self.ast(d.file);
            match *ast.node(d.node) {
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Get,
                    value,
                    ..
                } => crate::nodes::fn_parts(ast, value)
                    .and_then(|f| f.return_type)
                    .map(|r| Loc::new(d.file, r)),
                _ => None,
            }
        })
    }

    /// The parameter annotation of the setter declaring accessor `p`.
    pub(super) fn setter_annotation(&self, p: SymId) -> Option<Loc> {
        self.c.decls(p).iter().find_map(|d| {
            let ast = self.ast(d.file);
            match *ast.node(d.node) {
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Set,
                    value,
                    ..
                } => {
                    let f = crate::nodes::fn_parts(ast, value)?;
                    let &first = ast.list(f.params).first()?;
                    match *ast.node(first) {
                        Node::Param {
                            type_ann: Some(t), ..
                        } => Some(Loc::new(d.file, t)),
                        _ => None,
                    }
                }
                _ => None,
            }
        })
    }

    /// The return type node of signature `sig` from its declaration's annotation, when it
    /// can be reused: a plain type writing `r`, or a type predicate whose type is the
    /// predicate's.
    pub(super) fn reuse_return_annotation(
        &mut self,
        sig: ts_r_types::SigId,
        r: TypeId,
        pred: Option<ts_r_types::TypePredicate>,
        cx: &mut TypeNodeCx,
    ) -> Option<NodeId> {
        let ann = self.sig_return_annotation(sig)?;
        let node = *self.ast(ann.file).node(ann.node);
        match (node, pred) {
            (Node::TypePredicate { ty, .. }, Some(p)) => {
                if ann.file != cx.enclosing.file {
                    return None;
                }
                match (ty, p.ty) {
                    (Some(tn), Some(pt)) => {
                        self.reuse_type_node(Loc::new(ann.file, tn), pt, cx)?;
                    }
                    (None, None) => {}
                    _ => return None,
                }
                Some(ann.node)
            }
            (Node::TypePredicate { .. }, None) | (_, Some(_)) => None,
            _ => {
                if self.store().flags(r).contains(TypeFlags::NEVER)
                    && !matches!(node, Node::KeywordType { .. })
                {
                    return None;
                }
                self.reuse_type_node(ann, r, cx)
            }
        }
    }
}
