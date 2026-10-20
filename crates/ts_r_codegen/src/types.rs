//! TypeScript type syntax. Types are never synthesized, so they print as written (source
//! parentheses are `ParenType` nodes).

use ts_r_ast::{
    KeywordTypeKind, MappedModifier, MethodKind, Node, NodeId, NodeKind, TypeOperatorKind,
};

use crate::expr::Prec;
use crate::printer::Printer;

pub(crate) fn is_type_node(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        KeywordType
            | TypeRef
            | QualifiedName
            | ThisType
            | TemplateLitType
            | ArrayType
            | TupleType
            | NamedTupleMember
            | OptionalType
            | RestType
            | UnionType
            | IntersectionType
            | FunctionType
            | ConstructorType
            | TypeLiteral
            | PropertySignature
            | MethodSignature
            | CallSignature
            | ConstructSignature
            | TypeQuery
            | TypeOperator
            | IndexedAccessType
            | ConditionalType
            | InferType
            | MappedType
            | ParenType
            | TypePredicate
            | ImportType
            | TypeParams
            | TypeParam
            | TypeArgs
            | IndexSignature
    )
}

impl Printer<'_> {
    /// `: T`
    pub(crate) fn type_annotation(&mut self, id: NodeId) {
        self.tok(":");
        self.space();
        self.ty(id);
    }

    pub(crate) fn ty(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::KeywordType { kind } => self.tok(match kind {
                KeywordTypeKind::Any => "any",
                KeywordTypeKind::Unknown => "unknown",
                KeywordTypeKind::Number => "number",
                KeywordTypeKind::BigInt => "bigint",
                KeywordTypeKind::Boolean => "boolean",
                KeywordTypeKind::String => "string",
                KeywordTypeKind::Symbol => "symbol",
                KeywordTypeKind::Object => "object",
                KeywordTypeKind::Never => "never",
                KeywordTypeKind::Void => "void",
                KeywordTypeKind::Undefined => "undefined",
                KeywordTypeKind::Null => "null",
                KeywordTypeKind::Intrinsic => "intrinsic",
                KeywordTypeKind::Const => "const",
            }),
            Node::TypeRef { name, type_args } => {
                self.entity_name(name);
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
            }
            Node::QualifiedName { .. } => self.entity_name(id),
            Node::ThisType {} => self.tok("this"),
            Node::TemplateLitType { parts } => {
                self.raw_append_tok("`");
                for p in self.ast.list(parts).to_vec() {
                    match self.node(p) {
                        Node::TemplateElement { .. } => {
                            let raw = self.raw(p).unwrap_or("");
                            self.raw_append(raw);
                        }
                        _ => {
                            self.raw_append("${");
                            self.ty(p);
                            self.raw_append("}");
                        }
                    }
                }
                self.raw_append("`");
            }
            Node::ArrayType { elem } => {
                self.ty(elem);
                self.tok("[]");
            }
            Node::TupleType { elems } => {
                self.tok("[");
                let es = self.ast.list(elems).to_vec();
                self.comma_list(&es, |p, e| p.ty(e));
                self.tok("]");
            }
            Node::NamedTupleMember {
                rest,
                name,
                optional,
                ty,
            } => {
                if rest {
                    self.tok("...");
                }
                self.ident(name);
                if optional {
                    self.tok("?");
                }
                self.tok(":");
                self.space();
                self.ty(ty);
            }
            Node::OptionalType { ty } => {
                self.ty(ty);
                self.tok("?");
            }
            Node::RestType { ty } => {
                self.tok("...");
                self.ty(ty);
            }
            Node::UnionType { types } | Node::IntersectionType { types } => {
                let sep = if matches!(self.node(id), Node::UnionType { .. }) {
                    "|"
                } else {
                    "&"
                };
                let ts = self.ast.list(types).to_vec();
                // A leading `|`/`&` is optional, except that it is what makes a one-member
                // union a union.
                if ts.len() == 1 {
                    self.tok(sep);
                    self.space();
                }
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.space();
                        self.tok(sep);
                        self.space();
                    }
                    self.ty(*t);
                }
            }
            Node::FunctionType {
                type_params,
                params,
                return_type,
            } => {
                if let Some(tp) = type_params {
                    self.ty(tp);
                }
                self.tok("(");
                let ps = self.ast.list(params).to_vec();
                self.comma_list(&ps, |p, x| p.param(x));
                self.tok(")");
                self.space();
                self.tok("=>");
                self.space();
                self.ty(return_type);
            }
            Node::ConstructorType {
                is_abstract,
                type_params,
                params,
                return_type,
            } => {
                if is_abstract {
                    self.tok("abstract");
                    self.hard_space();
                }
                self.tok("new");
                self.space();
                if let Some(tp) = type_params {
                    self.ty(tp);
                }
                self.tok("(");
                let ps = self.ast.list(params).to_vec();
                self.comma_list(&ps, |p, x| p.param(x));
                self.tok(")");
                self.space();
                self.tok("=>");
                self.space();
                self.ty(return_type);
            }
            Node::TypeLiteral { members } => self.type_members(members, id),
            Node::PropertySignature { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::IndexSignature { .. } => self.type_member(id),
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                self.tok("typeof");
                self.hard_space();
                if self.kind(expr_name) == NodeKind::ImportType {
                    self.ty(expr_name);
                } else {
                    self.entity_name(expr_name);
                }
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
            }
            Node::TypeOperator { op, ty } => {
                self.tok(match op {
                    TypeOperatorKind::KeyOf => "keyof",
                    TypeOperatorKind::Unique => "unique",
                    TypeOperatorKind::Readonly => "readonly",
                });
                self.hard_space();
                self.ty(ty);
            }
            Node::IndexedAccessType { obj, index } => {
                self.ty(obj);
                self.tok("[");
                self.ty(index);
                self.tok("]");
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => {
                self.ty(check);
                self.hard_space();
                self.tok("extends");
                self.hard_space();
                self.ty(extends);
                self.space();
                self.tok("?");
                self.space();
                self.ty(true_ty);
                self.space();
                self.tok(":");
                self.space();
                self.ty(false_ty);
            }
            Node::InferType { type_param } => {
                self.tok("infer");
                self.hard_space();
                self.ty(type_param);
            }
            Node::MappedType {
                readonly,
                type_param,
                name_type,
                optional,
                ty,
            } => {
                self.tok("{");
                self.space();
                match readonly {
                    MappedModifier::None => {}
                    MappedModifier::Present => {
                        self.tok("readonly");
                        self.hard_space();
                    }
                    MappedModifier::Add => {
                        self.tok("+readonly");
                        self.hard_space();
                    }
                    MappedModifier::Remove => {
                        self.tok("-readonly");
                        self.hard_space();
                    }
                }
                self.tok("[");
                if let Node::TypeParam {
                    name, constraint, ..
                } = self.node(type_param)
                {
                    self.ident(name);
                    if let Some(c) = constraint {
                        self.hard_space();
                        self.tok("in");
                        self.hard_space();
                        self.ty(c);
                    }
                }
                if let Some(n) = name_type {
                    self.hard_space();
                    self.tok("as");
                    self.hard_space();
                    self.ty(n);
                }
                self.tok("]");
                match optional {
                    MappedModifier::None => {}
                    MappedModifier::Present => self.tok("?"),
                    MappedModifier::Add => self.tok("+?"),
                    MappedModifier::Remove => self.tok("-?"),
                }
                if let Some(t) = ty {
                    self.tok(":");
                    self.space();
                    self.ty(t);
                }
                self.tok(";");
                self.space();
                self.tok("}");
            }
            Node::ParenType { ty } => {
                self.tok("(");
                self.ty(ty);
                self.tok(")");
            }
            Node::TypePredicate { asserts, param, ty } => {
                if asserts {
                    self.tok("asserts");
                    self.hard_space();
                }
                if self.kind(param) == NodeKind::ThisType {
                    self.tok("this");
                } else {
                    self.ident(param);
                }
                if let Some(t) = ty {
                    self.hard_space();
                    self.tok("is");
                    self.hard_space();
                    self.ty(t);
                }
            }
            Node::ImportType {
                is_typeof,
                arg,
                attributes,
                qualifier,
                type_args,
            } => {
                if is_typeof {
                    self.tok("typeof");
                    self.hard_space();
                }
                self.tok("import(");
                self.ty_or_expr(arg);
                if let Some(a) = attributes {
                    self.tok(",");
                    self.space();
                    self.tok("{");
                    self.space();
                    let assert =
                        matches!(self.node(a), Node::ImportAttributes { assert: true, .. });
                    self.tok(if assert { "assert:" } else { "with:" });
                    self.space();
                    if let Node::ImportAttributes { entries, .. } = self.node(a) {
                        self.tok("{");
                        let es = self.ast.list(entries).to_vec();
                        if !es.is_empty() {
                            self.space();
                            self.comma_list(&es, |p, e| {
                                if let Node::ImportAttribute { key, value } = p.node(e) {
                                    p.property_key(key);
                                    p.tok(":");
                                    p.space();
                                    p.expr(value, Prec::Assign);
                                }
                            });
                            self.space();
                        }
                        self.tok("}");
                    } else {
                        self.expr(a, Prec::Assign);
                    }
                    self.space();
                    self.tok("}");
                }
                self.tok(")");
                if let Some(q) = qualifier {
                    self.tok(".");
                    self.entity_name(q);
                }
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
            }
            Node::TypeParams { params } => {
                self.tok("<");
                let ps = self.ast.list(params).to_vec();
                self.comma_list(&ps, |p, x| p.ty(x));
                self.tok(">");
            }
            Node::TypeParam {
                modifiers,
                name,
                constraint,
                default,
            } => {
                self.ts_modifiers(modifiers);
                self.ident(name);
                if let Some(c) = constraint {
                    self.hard_space();
                    self.tok("extends");
                    self.hard_space();
                    self.ty(c);
                }
                if let Some(d) = default {
                    self.space();
                    self.tok("=");
                    self.space();
                    self.ty(d);
                }
            }
            Node::TypeArgs { args } => {
                self.tok("<");
                let ps = self.ast.list(args).to_vec();
                self.comma_list(&ps, |p, x| p.ty(x));
                self.tok(">");
            }
            Node::Ident { .. } => self.ident(id),
            _ if is_type_node(self.kind(id)) => {}
            _ => self.expr(id, Prec::Unary),
        }
    }

    fn raw_append_tok(&mut self, s: &str) {
        self.tok(s);
    }

    fn ty_or_expr(&mut self, id: NodeId) {
        if is_type_node(self.kind(id)) {
            self.ty(id);
        } else {
            self.expr(id, Prec::Assign);
        }
    }

    /// `{ members }` of interfaces and type literals.
    pub(crate) fn type_members(&mut self, members: ts_r_ast::NodeList, owner: NodeId) {
        let ms = self.ast.list(members).to_vec();
        self.tok("{");
        if ms.is_empty() {
            self.tok("}");
            return;
        }
        let multiline = !self.compact
            && (self.kind(owner) == NodeKind::Interface
                || self.has_newline_between(self.span(owner).start, self.span(ms[0]).start));
        if multiline {
            self.indent();
        } else {
            self.space();
        }
        for (i, m) in ms.iter().enumerate() {
            if multiline {
                self.newline();
            } else if i > 0 {
                self.space();
            }
            self.type_member(*m);
            self.tok(";");
        }
        if multiline {
            self.dedent();
            self.newline();
        } else {
            self.space();
        }
        self.tok("}");
    }

    fn type_member(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::PropertySignature {
                modifiers,
                key,
                optional,
                type_ann,
            } => {
                self.ts_modifiers(modifiers);
                self.property_key(key);
                if optional {
                    self.tok("?");
                }
                if let Some(t) = type_ann {
                    self.type_annotation(t);
                }
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
                self.ts_modifiers(modifiers);
                match kind {
                    MethodKind::Get => {
                        self.tok("get");
                        self.hard_space();
                    }
                    MethodKind::Set => {
                        self.tok("set");
                        self.hard_space();
                    }
                    _ => {}
                }
                self.property_key(key);
                if optional {
                    self.tok("?");
                }
                self.signature(type_params, params, return_type);
            }
            Node::CallSignature {
                type_params,
                params,
                return_type,
            } => self.signature(type_params, params, return_type),
            Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => {
                self.tok("new");
                self.space();
                self.signature(type_params, params, return_type);
            }
            Node::IndexSignature { .. } => self.index_signature(id),
            _ => {}
        }
    }

    pub(crate) fn index_signature(&mut self, id: NodeId) {
        if let Node::IndexSignature {
            modifiers,
            params,
            type_ann,
        } = self.node(id)
        {
            self.ts_modifiers(modifiers);
            self.tok("[");
            let ps = self.ast.list(params).to_vec();
            self.comma_list(&ps, |p, x| p.param(x));
            self.tok("]");
            if let Some(t) = type_ann {
                self.type_annotation(t);
            }
        }
    }
}
