//! TypeScript syntax in a JavaScript file (`TsInJs::Report`): tsc's TS8xxx errors.
//!
//! The file is parsed with the TypeScript grammar (minus expression type arguments, see
//! `Parser::js_ts`); this pass then walks the finished tree once and reports each
//! TypeScript-only construct. Rules observed by running tsc 7.0.2 (`--allowJs --checkJs`) on
//! small `.js` files:
//!
//! - Declarations: `interface`, `enum` (also `const enum`), `namespace`/`module` (TS8006, at
//!   the name; at each segment of `A.B.C`), type aliases (TS8008, at the name). Interfaces,
//!   enums and type aliases are not looked into; namespaces are. `declare global` and
//!   `declare module "m"` report nothing.
//! - `import x = ...` (TS8002) and `export =` (TS8003) at the statement; `import type` and
//!   `export type` (TS8006) at the statement, an inline `{ type A }` at the specifier.
//! - Modifiers `declare`, `abstract`, `public`, `private`, `protected`, `readonly`, `override`
//!   (TS8009 at the keyword), except `declare` on a namespace; any modifier on a parameter
//!   (TS8012, once, at the first); `?` on parameters, properties and methods (TS8009 '?').
//!   A definite `!` is not reported.
//! - Type parameters (TS8004, at the first parameter), heritage type arguments (TS8011, at
//!   the first argument), `implements` (TS8005, at the keyword), type annotations and
//!   return types (TS8010, at the type).
//! - A function or method without a body and an index signature (TS8017, at the
//!   declaration); the return type of a signature is not reported.
//! - `x!` (TS8013, at the expression), `x as T` (TS8016) and `x satisfies T` (TS8037) at
//!   the type.
//!
//! The walk uses an explicit stack, so deep trees cannot overflow the native stack.

use ts_r_ast::{Ast, HeritageKind, Modifiers, ModuleKind, Node, NodeId, NodeKind, NodeList, Span};
use ts_r_diagnostics::{Diagnostic, DiagnosticMessage, messages as m};

use crate::Parser;

/// Modifiers only TypeScript has. `export`, `default`, `static`, `async`, `accessor` and
/// `const` are JavaScript.
const TS_ONLY: Modifiers = Modifiers::DECLARE
    .union(Modifiers::ABSTRACT)
    .union(Modifiers::PUBLIC)
    .union(Modifiers::PRIVATE)
    .union(Modifiers::PROTECTED)
    .union(Modifiers::READONLY)
    .union(Modifiers::OVERRIDE);

impl Parser<'_> {
    /// Reports every TypeScript-only construct in the parsed file (see the module docs).
    pub(crate) fn report_ts_in_js(&mut self) {
        let mut w = Walk {
            ast: &self.ast,
            src: self.src,
            out: Vec::new(),
            stack: vec![self.ast.root],
        };
        while let Some(id) = w.stack.pop() {
            w.visit(id);
        }
        let out = w.out;
        self.diags.extend(out);
    }
}

struct Walk<'a> {
    ast: &'a Ast,
    src: &'a str,
    out: Vec<Diagnostic>,
    stack: Vec<NodeId>,
}

impl Walk<'_> {
    fn report(&mut self, msg: DiagnosticMessage, span: Span, args: &[&str]) {
        self.out.push(Diagnostic::new(msg, span, args));
    }

    fn push(&mut self, id: Option<NodeId>) {
        if let Some(id) = id {
            self.stack.push(id);
        }
    }

    fn push_list(&mut self, list: NodeList) {
        self.stack.extend_from_slice(self.ast.list(list));
    }

    fn visit(&mut self, id: NodeId) {
        let ast = self.ast;
        let span = ast.span(id);
        match *ast.node(id) {
            Node::Interface { name, .. } => {
                self.report(m::JS_DECLARATION, ast.span(name), &["interface"]);
            }
            Node::Enum { name, .. } => self.report(m::JS_DECLARATION, ast.span(name), &["enum"]),
            Node::TypeAlias { name, .. } => self.report(m::JS_TYPE_ALIAS, ast.span(name), &[]),
            Node::Module {
                kind, name, body, ..
            } => {
                if kind != ModuleKind::Global {
                    let word = if kind == ModuleKind::Namespace {
                        "namespace"
                    } else {
                        "module"
                    };
                    self.name_segments(name, word);
                }
                self.push(body);
            }
            Node::ImportEquals { .. } => self.report(m::JS_IMPORT_EQUALS, span, &[]),
            Node::ExportAssignment { .. } => self.report(m::JS_EXPORT_ASSIGNMENT, span, &[]),
            Node::ImportDecl {
                type_only: true, ..
            } => self.report(m::JS_DECLARATION, span, &["import type"]),
            Node::ExportNamed {
                type_only: true, ..
            }
            | Node::ExportAll {
                type_only: true, ..
            } => self.report(m::JS_DECLARATION, span, &["export type"]),
            Node::ImportSpecifier {
                type_only: true, ..
            } => self.report(m::JS_DECLARATION, span, &["import...type"]),
            Node::ExportSpecifier {
                type_only: true, ..
            } => self.report(m::JS_DECLARATION, span, &["export...type"]),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => {
                self.modifiers(id);
                self.function(id, span.start);
            }
            Node::Arrow {
                type_params,
                params,
                return_type,
                body,
                ..
            } => {
                self.type_params(type_params);
                self.push_list(params);
                self.annotation(return_type);
                self.stack.push(body);
            }
            Node::MethodDef {
                decorators,
                key,
                optional,
                value,
                ..
            } => {
                self.modifiers(id);
                self.push_list(decorators);
                self.stack.push(key);
                if optional {
                    self.question_after(ast.span(key).end);
                }
                self.function(value, span.start);
            }
            Node::PropertyDef {
                decorators,
                key,
                optional,
                type_ann,
                value,
                ..
            } => {
                self.modifiers(id);
                self.push_list(decorators);
                self.stack.push(key);
                if optional {
                    self.question_after(ast.span(key).end);
                }
                self.annotation(type_ann);
                self.push(value);
            }
            Node::IndexSignature { .. } => self.report(m::JS_SIGNATURE, span, &[]),
            Node::Param {
                decorators,
                pattern,
                optional,
                type_ann,
                init,
                ..
            } => {
                if let Some(first) = ast.modifier_tokens_of(id).first() {
                    self.report(m::JS_PARAMETER_MODIFIERS, first.span, &[]);
                }
                self.push_list(decorators);
                self.stack.push(pattern);
                if optional {
                    self.question_after(ast.span(pattern).end);
                }
                self.annotation(type_ann);
                self.push(init);
            }
            Node::ClassDecl {
                decorators,
                name,
                type_params,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                decorators,
                name,
                type_params,
                heritage,
                members,
                ..
            } => {
                self.modifiers(id);
                self.push_list(decorators);
                self.push(name);
                self.type_params(type_params);
                for &h in ast.list(heritage) {
                    self.heritage(h);
                }
                self.push_list(members);
            }
            Node::VarDecl { decls, .. } => {
                self.modifiers(id);
                self.push_list(decls);
            }
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } => {
                self.stack.push(name);
                self.annotation(type_ann);
                self.push(init);
            }
            Node::CatchClause {
                param,
                type_ann,
                body,
            } => {
                self.push(param);
                self.annotation(type_ann);
                self.stack.push(body);
            }
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                self.report(m::JS_TYPE_ASSERTION, ast.span(ty), &[]);
                self.stack.push(expr);
            }
            Node::Satisfies { expr, ty } => {
                self.report(m::JS_SATISFIES, ast.span(ty), &[]);
                self.stack.push(expr);
            }
            Node::NonNull { expr } => {
                self.report(m::JS_NON_NULL, span, &[]);
                self.stack.push(expr);
            }
            // Expressions take no type arguments in this mode; these only appear if a caller
            // hands over a tree built otherwise.
            Node::Call {
                callee,
                type_args,
                args,
                ..
            }
            | Node::New {
                callee,
                type_args,
                args,
                ..
            } => {
                self.type_args(type_args);
                self.stack.push(callee);
                self.push_list(args);
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                self.type_args(type_args);
                self.stack.push(tag);
                self.stack.push(quasi);
            }
            Node::Instantiation { expr, type_args } => {
                self.type_args(Some(type_args));
                self.stack.push(expr);
            }
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                ..
            } => {
                self.type_args(type_args);
                self.stack.push(name);
                self.push_list(attrs);
            }
            _ if is_type(ast.kind(id)) => {}
            _ => {
                let stack = &mut self.stack;
                ast.for_each_child(id, |c| stack.push(c));
            }
        }
    }

    /// TS8006 at each segment of a namespace name (`A.B.C`); nothing for `module "m"`.
    fn name_segments(&mut self, name: NodeId, word: &str) {
        let mut n = name;
        loop {
            match *self.ast.node(n) {
                Node::QualifiedName { left, right } => {
                    self.report(m::JS_DECLARATION, self.ast.span(right), &[word]);
                    n = left;
                }
                Node::Ident { .. } => {
                    self.report(m::JS_DECLARATION, self.ast.span(n), &[word]);
                    return;
                }
                _ => return,
            }
        }
    }

    /// TS8009 at each TypeScript-only modifier keyword written on `id`.
    fn modifiers(&mut self, id: NodeId) {
        for t in self.ast.modifier_tokens_of(id) {
            if TS_ONLY.intersects(t.flag) {
                let text = self
                    .src
                    .get(t.span.start as usize..t.span.end as usize)
                    .unwrap_or("");
                self.out
                    .push(Diagnostic::new(m::JS_MODIFIER, t.span, &[text]));
            }
        }
    }

    /// The parts of a function shared by declarations, expressions and methods. A missing
    /// body makes it a signature (TS8017 from `sig_start`); its return type is not reported.
    fn function(&mut self, id: NodeId, sig_start: u32) {
        let (Node::FunctionDecl {
            name,
            type_params,
            params,
            return_type,
            body,
            ..
        }
        | Node::FunctionExpr {
            name,
            type_params,
            params,
            return_type,
            body,
            ..
        }) = *self.ast.node(id)
        else {
            self.stack.push(id);
            return;
        };
        self.push(name);
        self.type_params(type_params);
        self.push_list(params);
        match body {
            Some(b) => {
                self.annotation(return_type);
                self.stack.push(b);
            }
            None => {
                let end = self.ast.span(id).end;
                self.report(
                    m::JS_SIGNATURE,
                    Span::new(sig_start, end.max(sig_start)),
                    &[],
                );
            }
        }
    }

    fn heritage(&mut self, h: NodeId) {
        let ast = self.ast;
        let Node::HeritageClause { kind, types } = *ast.node(h) else {
            return;
        };
        if kind == HeritageKind::Implements {
            let s = ast.span(h).start;
            self.report(
                m::JS_IMPLEMENTS,
                Span::new(s, s + "implements".len() as u32),
                &[],
            );
            return;
        }
        for &t in ast.list(types) {
            match *ast.node(t) {
                Node::ExprWithTypeArgs { expr, type_args } => {
                    self.type_args(type_args);
                    self.stack.push(expr);
                }
                _ => self.stack.push(t),
            }
        }
    }

    /// TS8004 at the first type parameter.
    fn type_params(&mut self, tp: Option<NodeId>) {
        let Some(tp) = tp else { return };
        let span = match *self.ast.node(tp) {
            Node::TypeParams { params } => self.ast.list(params).first().map(|&p| self.ast.span(p)),
            _ => None,
        };
        self.report(
            m::JS_TYPE_PARAMETERS,
            span.unwrap_or(self.ast.span(tp)),
            &[],
        );
    }

    /// TS8011 at the first type argument.
    fn type_args(&mut self, ta: Option<NodeId>) {
        let Some(ta) = ta else { return };
        let span = match *self.ast.node(ta) {
            Node::TypeArgs { args } => self.ast.list(args).first().map(|&a| self.ast.span(a)),
            _ => None,
        };
        self.report(m::JS_TYPE_ARGUMENTS, span.unwrap_or(self.ast.span(ta)), &[]);
    }

    /// TS8010 at a type annotation or return type.
    fn annotation(&mut self, ty: Option<NodeId>) {
        if let Some(ty) = ty {
            self.report(m::JS_TYPE_ANNOTATION, self.ast.span(ty), &[]);
        }
    }

    /// TS8009 '?' at the `?` after `pos` (skipping whitespace and comments).
    fn question_after(&mut self, pos: u32) {
        let b = self.src.as_bytes();
        let mut i = pos as usize;
        while i < b.len() {
            match b[i] {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => i += 1,
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i = self.src[i + 2..]
                        .find("*/")
                        .map_or(b.len(), |e| i + 2 + e + 2);
                }
                b'?' => {
                    let s = i as u32;
                    self.report(m::JS_MODIFIER, Span::new(s, s + 1), &["?"]);
                    return;
                }
                _ => return,
            }
        }
    }
}

/// Type nodes, which the walk never enters (their parents report them).
fn is_type(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        KeywordType
            | TypeRef
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
    )
}
