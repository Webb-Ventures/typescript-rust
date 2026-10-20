//! TypeScript types: unions/intersections, conditional, function and constructor types,
//! mapped types, tuples, template literal types, type members, type parameters/arguments.

use ts_r_ast::{
    JsDocSyntax, JsDocSyntaxKind, KeywordTypeKind, MappedModifier, MethodKind, Modifiers, Node,
    NodeId, NodeList, Span, TypeOperatorKind, UnaryOp,
};
use ts_r_diagnostics::messages;
use ts_r_lexer::TokenKind as T;

use crate::modifiers::{ModCtx, is_export_param_modifier_follow, is_param_modifier};
use crate::{Ctx, Parser};

impl<'a> Parser<'a> {
    pub(crate) fn parse_type(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.with_ctx(Ctx::TYPE, 0, |p| p.parse_type_inner());
        self.leave();
        r
    }

    fn parse_type_inner(&mut self) -> NodeId {
        let start = self.start();
        if self.is_start_of_function_type() {
            return self.parse_function_type(start);
        }
        if self.at(T::New) || (self.at(T::Abstract) && self.peek_kind() == T::New) {
            return self.parse_constructor_type(start);
        }
        let check = self.parse_union_type();
        if !self.ctx.has(Ctx::DISALLOW_CONDITIONAL_TYPES)
            && !self.newline_before()
            && self.at(T::Extends)
        {
            self.bump();
            let extends = self.with_ctx(Ctx::DISALLOW_CONDITIONAL_TYPES, 0, |p| {
                p.parse_type_nested()
            });
            self.expect(T::Question);
            let true_ty = self.with_ctx(0, Ctx::DISALLOW_CONDITIONAL_TYPES, |p| {
                p.parse_type_nested()
            });
            self.expect(T::Colon);
            let false_ty = self.with_ctx(0, Ctx::DISALLOW_CONDITIONAL_TYPES, |p| {
                p.parse_type_nested()
            });
            return self.finish_node(
                start,
                Node::ConditionalType {
                    check,
                    extends,
                    true_ty,
                    false_ty,
                },
            );
        }
        check
    }

    /// `parse_type` that keeps the current conditional-type context.
    fn parse_type_nested(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.parse_type_inner();
        self.leave();
        r
    }

    /// A type inside brackets/parens, where conditional types are allowed again.
    fn parse_type_in_brackets(&mut self) -> NodeId {
        self.with_ctx(0, Ctx::DISALLOW_CONDITIONAL_TYPES, |p| p.parse_type())
    }

    fn is_start_of_function_type(&mut self) -> bool {
        match self.kind() {
            T::Lt => true,
            T::LParen => self.lookahead(|p| {
                p.bump();
                match p.kind() {
                    T::RParen | T::DotDotDot => true,
                    T::LBracket | T::LBrace => {
                        if !p.skip_balanced() {
                            return false;
                        }
                        p.after_param_start()
                    }
                    T::This => {
                        p.bump();
                        p.after_param_start()
                    }
                    // `(readonly string[])` is a parenthesized type operator; `(readonly x: T)`
                    // and `(readonly [a]) =>` are parameters: decide after the next name or
                    // pattern, like any parameter.
                    T::Readonly => {
                        p.bump();
                        match p.kind() {
                            T::DotDotDot => true,
                            T::LBracket | T::LBrace => p.skip_balanced() && p.after_param_start(),
                            k if k.is_identifier_like() => {
                                p.bump();
                                p.after_param_start()
                            }
                            _ => p.after_param_start(),
                        }
                    }
                    k if k.is_identifier_like() => {
                        p.bump();
                        if is_param_modifier(k)
                            && (p.kind().is_identifier_like()
                                || matches!(p.kind(), T::LBrace | T::LBracket | T::DotDotDot))
                        {
                            return true;
                        }
                        p.after_param_start()
                    }
                    // `(export e) => T`: a misplaced parameter modifier (request R7).
                    T::Export => {
                        p.bump();
                        is_export_param_modifier_follow(p.kind())
                    }
                    _ => false,
                }
            }),
            _ => false,
        }
    }

    /// After a parameter name in a possible function type: `:`, `,`, `?`, `=`, or `) =>`.
    fn after_param_start(&mut self) -> bool {
        match self.kind() {
            T::Colon | T::Comma | T::Question | T::Eq => true,
            T::RParen => {
                self.bump();
                self.at(T::Arrow)
            }
            _ => false,
        }
    }

    /// Skips a balanced `[...]` or `{...}` (lookahead only).
    fn skip_balanced(&mut self) -> bool {
        let mut depth = 0u32;
        loop {
            match self.kind() {
                T::LBracket | T::LBrace | T::LParen => depth += 1,
                T::RBracket | T::RBrace | T::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.bump();
                        return true;
                    }
                }
                T::Eof => return false,
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_function_type(&mut self, start: u32) -> NodeId {
        let type_params = self.parse_type_params_opt();
        let params = self.parse_params();
        self.expect(T::Arrow);
        let return_type = self.parse_return_type_after_arrow();
        self.finish_node(
            start,
            Node::FunctionType {
                type_params,
                params,
                return_type,
            },
        )
    }

    fn parse_constructor_type(&mut self, start: u32) -> NodeId {
        let is_abstract = self.eat(T::Abstract);
        self.expect(T::New);
        let type_params = self.parse_type_params_opt();
        let params = self.parse_params();
        self.expect(T::Arrow);
        let return_type = self.parse_return_type_after_arrow();
        self.finish_node(
            start,
            Node::ConstructorType {
                is_abstract,
                type_params,
                params,
                return_type,
            },
        )
    }

    fn parse_return_type_after_arrow(&mut self) -> NodeId {
        self.with_ctx(Ctx::TYPE, Ctx::DISALLOW_CONDITIONAL_TYPES, |p| {
            p.parse_type_or_predicate()
        })
    }

    /// `: T` (or `: x is T`, `: asserts x`) if the current token is `colon`.
    pub(crate) fn parse_return_type_opt(&mut self, colon: T) -> Option<NodeId> {
        if !self.ts || !self.at(colon) {
            return None;
        }
        self.bump();
        Some(self.with_ctx(Ctx::TYPE, 0, |p| p.parse_type_or_predicate()))
    }

    /// The return type of a call, construct or method signature. `=> T` there is
    /// `':' expected` at the `=>`, and `T` is still the return type, as in tsc
    /// (parseErrorIncorrectReturnToken).
    fn parse_signature_return_type(&mut self) -> Option<NodeId> {
        if self.ts && self.at(T::Arrow) {
            self.error_expected(":");
            self.bump();
            return Some(self.with_ctx(Ctx::TYPE, 0, |p| p.parse_type_or_predicate()));
        }
        self.parse_return_type_opt(T::Colon)
    }

    pub(crate) fn parse_type_or_predicate(&mut self) -> NodeId {
        let start = self.start();
        if self.at(T::Asserts) {
            let next = self.peek();
            if !next.newline_before()
                && (next.kind == T::This || (next.kind.is_identifier_like() && next.kind != T::Is))
            {
                self.bump();
                let param = self.parse_predicate_param();
                let ty = if self.at(T::Is) && !self.newline_before() {
                    self.bump();
                    Some(self.parse_type())
                } else {
                    None
                };
                return self.finish_node(
                    start,
                    Node::TypePredicate {
                        asserts: true,
                        param,
                        ty,
                    },
                );
            }
        }
        if self.kind().is_identifier_like() || self.at(T::This) {
            let next = self.peek();
            if next.kind == T::Is && !next.newline_before() {
                let param = self.parse_predicate_param();
                self.bump();
                let ty = Some(self.parse_type());
                return self.finish_node(
                    start,
                    Node::TypePredicate {
                        asserts: false,
                        param,
                        ty,
                    },
                );
            }
        }
        self.parse_type()
    }

    fn parse_predicate_param(&mut self) -> NodeId {
        if self.at(T::This) {
            let s = self.start();
            self.bump();
            self.finish_node(s, Node::ThisType {})
        } else {
            self.parse_identifier_name()
        }
    }

    fn parse_union_type(&mut self) -> NodeId {
        let start = self.start();
        let leading = self.eat(T::Pipe);
        let first = self.parse_intersection_type();
        if !self.at(T::Pipe) {
            if leading {
                let mark = self.scratch.len();
                self.scratch.push(first);
                let types = self.finish_list(mark);
                return self.finish_node(start, Node::UnionType { types });
            }
            return first;
        }
        let mark = self.scratch.len();
        self.scratch.push(first);
        while self.eat(T::Pipe) {
            if self.aborting() {
                break;
            }
            let t = self.parse_intersection_type();
            self.scratch.push(t);
        }
        let types = self.finish_list(mark);
        self.finish_node(start, Node::UnionType { types })
    }

    fn parse_intersection_type(&mut self) -> NodeId {
        let start = self.start();
        let leading = self.eat(T::Amp);
        let first = self.parse_type_operator_or_function();
        if !self.at(T::Amp) {
            if leading {
                let mark = self.scratch.len();
                self.scratch.push(first);
                let types = self.finish_list(mark);
                return self.finish_node(start, Node::IntersectionType { types });
            }
            return first;
        }
        let mark = self.scratch.len();
        self.scratch.push(first);
        while self.eat(T::Amp) {
            if self.aborting() {
                break;
            }
            let t = self.parse_type_operator_or_function();
            self.scratch.push(t);
        }
        let types = self.finish_list(mark);
        self.finish_node(start, Node::IntersectionType { types })
    }

    /// Union/intersection members may be (unparenthesized, erroneous) function types.
    fn parse_type_operator_or_function(&mut self) -> NodeId {
        let start = self.start();
        if self.is_start_of_function_type() {
            return self.parse_function_type(start);
        }
        if self.at(T::New) || (self.at(T::Abstract) && self.peek_kind() == T::New) {
            return self.parse_constructor_type(start);
        }
        self.parse_type_operator()
    }

    fn parse_type_operator(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.parse_type_operator_inner();
        self.leave();
        r
    }

    fn parse_type_operator_inner(&mut self) -> NodeId {
        let start = self.start();
        let op = match self.kind() {
            T::KeyOf => Some(TypeOperatorKind::KeyOf),
            T::Unique => Some(TypeOperatorKind::Unique),
            T::Readonly => Some(TypeOperatorKind::Readonly),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let ty = self.parse_type_operator();
            return self.finish_node(start, Node::TypeOperator { op, ty });
        }
        if self.at(T::Infer) {
            self.bump();
            let pstart = self.start();
            let name = self.parse_identifier();
            let constraint = if self.at(T::Extends) {
                self.try_parse(|p| {
                    p.bump();
                    let c = p.with_ctx(Ctx::DISALLOW_CONDITIONAL_TYPES, 0, |p| {
                        p.parse_type_nested()
                    });
                    if p.ctx.has(Ctx::DISALLOW_CONDITIONAL_TYPES) || !p.at(T::Question) {
                        Some(c)
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            let tp = self.finish_node(
                pstart,
                Node::TypeParam {
                    modifiers: Modifiers::NONE,
                    name,
                    constraint,
                    default: None,
                },
            );
            return self.finish_node(start, Node::InferType { type_param: tp });
        }
        self.parse_postfix_type()
    }

    fn parse_postfix_type(&mut self) -> NodeId {
        let start = self.start();
        let mut ty = self.parse_primary_type();
        loop {
            if self.jsdoc_types() && !self.aborting() {
                // JSDoc `T!` (non-null: `T`) and `T?` (nullable: `T | null`), when the `?`
                // cannot start a conditional type's branch.
                if self.at(T::Bang) {
                    self.bump();
                    self.note_jsdoc_syntax(start, JsDocSyntaxKind::PostfixNonNull, Some(ty));
                    continue;
                }
                if self.at(T::Question) && self.jsdoc_postfix_question() {
                    let q = self.tok().span();
                    self.bump();
                    self.note_jsdoc_syntax(start, JsDocSyntaxKind::PostfixNullable, Some(ty));
                    ty = self.jsdoc_nullable(start, ty, q);
                    continue;
                }
            }
            if self.newline_before() || !self.at(T::LBracket) || self.aborting() {
                break;
            }
            let open = self.start();
            self.bump();
            if self.eat(T::RBracket) {
                ty = self.finish_node(start, Node::ArrayType { elem: ty });
            } else {
                let index = self.parse_type_in_brackets();
                self.expect_closing(T::RBracket, T::LBracket, open);
                ty = self.finish_node(start, Node::IndexedAccessType { obj: ty, index });
            }
        }
        ty
    }

    /// Records JSDoc type syntax from `start` to the previous token in a TypeScript file.
    fn note_jsdoc_syntax(&mut self, start: u32, kind: JsDocSyntaxKind, ty: Option<NodeId>) {
        if self.ts && !self.jsdoc {
            self.ast.jsdoc_syntax.push(JsDocSyntax {
                span: Span::new(start, self.prev_end.max(start)),
                kind,
                ty,
            });
        }
    }

    /// Forgets the record of a postfix `T?` that ends at `ty` (it is a tuple element's `?`).
    fn unnote_postfix_nullable(&mut self, ty: NodeId) {
        let end = self.ast.span(ty).end;
        if self
            .ast
            .jsdoc_syntax
            .last()
            .is_some_and(|j| j.kind == JsDocSyntaxKind::PostfixNullable && j.span.end == end)
        {
            self.ast.jsdoc_syntax.pop();
        }
    }

    /// Whether JSDoc type syntax (`*`, `?T`, `T?`, `!T`, `T!`, `Array.<T>`) is parsed: in
    /// JSDoc comments, and in TypeScript files, where tsc 7 accepts it silently
    /// (`jsdocDisallowedInTypescript`).
    fn jsdoc_types(&self) -> bool {
        self.jsdoc || self.ts
    }

    /// In a JSDoc type, whether the `?` at the current token is postfix nullability: the
    /// token after it ends the type. `|` and `&` start a type, so `S extends T ? | A : B`
    /// is a conditional type with a leading-bar union (tsc 7; `@types/node` writes it).
    fn jsdoc_postfix_question(&mut self) -> bool {
        matches!(
            self.peek_kind(),
            T::RBrace
                | T::Comma
                | T::Semi
                | T::RParen
                | T::RBracket
                | T::Gt
                | T::Eq
                | T::Eof
                | T::Star
        )
    }

    /// `T` of a JSDoc postfix `T?` built by [`Parser::jsdoc_nullable`].
    fn jsdoc_postfix_nullable_inner(&self, ty: NodeId) -> Option<NodeId> {
        if !self.jsdoc_types() {
            return None;
        }
        let Node::UnionType { types } = *self.ast.node(ty) else {
            return None;
        };
        let &[inner, null] = self.ast.list(types) else {
            return None;
        };
        let q = self.ast.span(null);
        let is_q = matches!(
            self.ast.node(null),
            Node::KeywordType {
                kind: KeywordTypeKind::Null
            }
        ) && q.end == q.start + 1
            && q.end == self.ast.span(ty).end
            && self.src.as_bytes().get(q.start as usize) == Some(&b'?');
        is_q.then_some(inner)
    }

    /// `T | null` for JSDoc `?T` / `T?`; the `null` takes the span of the `?`.
    fn jsdoc_nullable(&mut self, start: u32, ty: NodeId, q: Span) -> NodeId {
        let null = self.add_node(
            q,
            Node::KeywordType {
                kind: KeywordTypeKind::Null,
            },
        );
        let mark = self.scratch.len();
        self.scratch.push(ty);
        self.scratch.push(null);
        let types = self.finish_list(mark);
        self.finish_node(start, Node::UnionType { types })
    }

    fn keyword_type(&self) -> Option<KeywordTypeKind> {
        use KeywordTypeKind as K;
        Some(match self.kind() {
            T::Any => K::Any,
            T::UnknownKw => K::Unknown,
            T::NumberKw => K::Number,
            T::BigIntKw => K::BigInt,
            T::Boolean => K::Boolean,
            T::StringKw => K::String,
            T::SymbolKw => K::Symbol,
            T::ObjectKw => K::Object,
            T::Never => K::Never,
            T::Undefined => K::Undefined,
            _ => return None,
        })
    }

    fn parse_primary_type(&mut self) -> NodeId {
        let start = self.start();
        if self.jsdoc_types() {
            match self.kind() {
                // `*`: any type.
                T::Star => {
                    self.bump();
                    self.note_jsdoc_syntax(start, JsDocSyntaxKind::All, None);
                    return self.finish_node(
                        start,
                        Node::KeywordType {
                            kind: KeywordTypeKind::Any,
                        },
                    );
                }
                // `?T`: `T | null` (`??T` too: the lexer's `??` is two prefixes).
                T::Question | T::QuestionQuestion => {
                    let q = self.tok().span();
                    self.bump();
                    let ty = self.parse_type_operator();
                    self.note_jsdoc_syntax(start, JsDocSyntaxKind::PrefixNullable, Some(ty));
                    return self.jsdoc_nullable(start, ty, q);
                }
                // `!T`: `T`.
                T::Bang => {
                    self.bump();
                    let ty = self.parse_type_operator();
                    self.note_jsdoc_syntax(start, JsDocSyntaxKind::PrefixNonNull, Some(ty));
                    return ty;
                }
                _ => {}
            }
        }
        if let Some(kind) = self.keyword_type()
            && self.peek_kind() != T::Dot
        {
            self.bump();
            return self.finish_node(start, Node::KeywordType { kind });
        }
        match self.kind() {
            T::Void => {
                self.bump();
                self.finish_node(
                    start,
                    Node::KeywordType {
                        kind: KeywordTypeKind::Void,
                    },
                )
            }
            T::Null => {
                self.bump();
                self.finish_node(
                    start,
                    Node::KeywordType {
                        kind: KeywordTypeKind::Null,
                    },
                )
            }
            T::This => {
                self.bump();
                self.finish_node(start, Node::ThisType {})
            }
            T::TypeOf => {
                self.bump();
                if self.at(T::Import) {
                    return self.parse_import_type(start, true);
                }
                let expr_name = self.parse_entity_name(true);
                let type_args = if self.at(T::Lt) && !self.newline_before() {
                    Some(self.parse_type_args())
                } else {
                    None
                };
                self.finish_node(
                    start,
                    Node::TypeQuery {
                        expr_name,
                        type_args,
                    },
                )
            }
            T::Import => self.parse_import_type(start, false),
            T::LBrace => {
                if self.is_mapped_type_start() {
                    self.parse_mapped_type()
                } else {
                    let members = self.parse_object_type_members(false);
                    self.finish_node(start, Node::TypeLiteral { members })
                }
            }
            T::LBracket => self.parse_tuple_type(),
            T::LParen => {
                let open = self.start();
                self.bump();
                let ty = self.parse_type_in_brackets();
                self.expect_closing(T::RParen, T::LParen, open);
                self.finish_node(start, Node::ParenType { ty })
            }
            T::String => self.parse_string_literal(),
            T::Number => self.parse_numeric_literal(),
            T::BigInt => self.parse_bigint_literal(),
            T::True | T::False => {
                let value = self.at(T::True);
                self.bump();
                self.finish_node(start, Node::BoolLit { value })
            }
            T::Minus if matches!(self.peek_kind(), T::Number | T::BigInt) => {
                self.bump();
                let arg = if self.at(T::Number) {
                    self.parse_numeric_literal()
                } else {
                    self.parse_bigint_literal()
                };
                self.finish_node(
                    start,
                    Node::Unary {
                        op: UnaryOp::Minus,
                        arg,
                    },
                )
            }
            T::NoSubstTemplate => {
                // A template literal type without substitutions is a string literal type.
                self.parse_template_literal_type()
            }
            T::TemplateHead => self.parse_template_literal_type(),
            T::Lt => self.parse_function_type(start),
            T::Asserts | T::Infer => {
                // As a plain type name.
                let name = self.parse_entity_name(false);
                self.finish_type_ref(start, name)
            }
            // `await`/`yield` are type names even where they are operators
            // (asyncArrowFunction10_es2017: `var v: await;` in an async function).
            T::Await | T::Yield => {
                let name = self.parse_entity_name(true);
                self.finish_type_ref(start, name)
            }
            k if k.is_identifier_like() => {
                let name = self.parse_entity_name(false);
                self.finish_type_ref(start, name)
            }
            // A reserved word is a type name too (`x: break`, `a: super()`), as in tsc: no
            // syntax error, and the checker reports TS2304.
            k if k.is_identifier_or_keyword() => {
                let name = self.parse_entity_name(true);
                self.finish_type_ref(start, name)
            }
            _ => {
                self.error(messages::TYPE_EXPECTED);
                self.missing()
            }
        }
    }

    fn finish_type_ref(&mut self, start: u32, name: NodeId) -> NodeId {
        // JSDoc `Array.<T>`.
        if self.jsdoc_types() && self.at(T::Dot) && self.peek_kind() == T::Lt {
            self.bump();
        }
        let type_args = if self.at(T::Lt) && !self.newline_before() {
            Some(self.parse_type_args())
        } else {
            None
        };
        self.finish_node(start, Node::TypeRef { name, type_args })
    }

    /// `A.B.C`. With `allow_keywords`, the first name may be a keyword (e.g. `typeof this`).
    pub(crate) fn parse_entity_name(&mut self, allow_keywords: bool) -> NodeId {
        let start = self.start();
        let mut name = if allow_keywords && self.kind().is_identifier_or_keyword() {
            let s = self.start();
            let a = self.intern_token();
            self.bump();
            self.finish_node(s, Node::Ident { name: a })
        } else {
            self.parse_identifier()
        };
        while self.at(T::Dot) {
            // JSDoc `Array.<T>`: the `.` belongs to the type arguments.
            if self.jsdoc_types() && self.peek_kind() == T::Lt {
                break;
            }
            // `A.` followed by `<` is not part of a name.
            self.bump();
            let right = if let Some(missing) = self.missing_name_after_dot() {
                missing
            } else if self.at(T::PrivateName) {
                self.parse_private_name()
            } else {
                self.parse_identifier_name()
            };
            name = self.finish_node(start, Node::QualifiedName { left: name, right });
        }
        name
    }

    fn parse_import_type(&mut self, start: u32, is_typeof: bool) -> NodeId {
        self.expect(T::Import);
        let open = self.start();
        self.expect(T::LParen);
        let arg = self.parse_type_in_brackets();
        if !matches!(
            self.ast.node(arg),
            Node::StringLit { .. } | Node::Missing {}
        ) {
            // `import(T)`: tsc's checker wants a string literal (importTypeGeneric).
            let span = self.ast.span(arg);
            self.deferred_grammar_error_at(messages::STRING_LITERAL_EXPECTED, span, &[]);
        }
        let mut attributes = None;
        if self.eat(T::Comma) && !self.at(T::RParen) {
            let astart = self.start();
            let obrace = self.start();
            if self.expect(T::LBrace) {
                let assert = self.at(T::Assert);
                if self.at(T::With) || self.at(T::Assert) {
                    self.bump();
                    self.expect(T::Colon);
                    attributes = Some(self.parse_import_attributes_body(astart, assert));
                } else {
                    self.error_expected("with");
                }
                self.eat(T::Comma);
                self.expect_closing(T::RBrace, T::LBrace, obrace);
            }
            // Malformed attributes (`{"resolution-mode": "x"}`, `[...]`, a name) recover as
            // empty `with` attributes, as tsc's tree does (its declaration emit writes
            // `import("pkg", { with: {} })`).
            if attributes.is_none() {
                let mark = self.scratch.len();
                let entries = self.finish_list(mark);
                attributes = Some(self.finish_node(
                    astart,
                    Node::ImportAttributes {
                        assert: false,
                        entries,
                    },
                ));
            }
            self.eat(T::Comma);
        }
        self.expect_closing(T::RParen, T::LParen, open);
        let qualifier = if self.eat(T::Dot) {
            Some(self.parse_entity_name(true))
        } else {
            None
        };
        let type_args = if self.at(T::Lt) && !self.newline_before() {
            Some(self.parse_type_args())
        } else {
            None
        };
        self.finish_node(
            start,
            Node::ImportType {
                is_typeof,
                arg,
                attributes,
                qualifier,
                type_args,
            },
        )
    }

    fn is_mapped_type_start(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if matches!(p.kind(), T::Plus | T::Minus) {
                p.bump();
                return p.at(T::Readonly);
            }
            if p.at(T::Readonly) {
                p.bump();
            }
            if !p.at(T::LBracket) {
                return false;
            }
            p.bump();
            if !p.kind().is_identifier_like() {
                return false;
            }
            p.bump();
            p.at(T::In)
        })
    }

    fn parse_mapped_type(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let readonly = match self.kind() {
            T::Plus => {
                self.bump();
                self.expect(T::Readonly);
                MappedModifier::Add
            }
            T::Minus => {
                self.bump();
                self.expect(T::Readonly);
                MappedModifier::Remove
            }
            T::Readonly => {
                self.bump();
                MappedModifier::Present
            }
            _ => MappedModifier::None,
        };
        let bopen = self.start();
        self.expect(T::LBracket);
        let pstart = self.start();
        let name = self.parse_identifier();
        self.expect(T::In);
        let constraint = Some(self.parse_type_in_brackets());
        let type_param = self.finish_node(
            pstart,
            Node::TypeParam {
                modifiers: Modifiers::NONE,
                name,
                constraint,
                default: None,
            },
        );
        let name_type = if self.eat(T::As) {
            Some(self.parse_type_in_brackets())
        } else {
            None
        };
        self.expect_closing(T::RBracket, T::LBracket, bopen);
        let optional = match self.kind() {
            T::Plus => {
                self.bump();
                self.expect(T::Question);
                MappedModifier::Add
            }
            T::Minus => {
                self.bump();
                self.expect(T::Question);
                MappedModifier::Remove
            }
            T::Question => {
                self.bump();
                MappedModifier::Present
            }
            _ => MappedModifier::None,
        };
        let ty = if self.eat(T::Colon) {
            Some(self.parse_type_in_brackets())
        } else {
            None
        };
        if !self.eat(T::Semi) {
            self.eat(T::Comma);
        }
        // Members after the mapping are an error; skip them.
        while !matches!(self.kind(), T::RBrace | T::Eof) && !self.aborting() {
            self.error(messages::UNEXPECTED_TOKEN);
            self.bump();
        }
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(
            start,
            Node::MappedType {
                readonly,
                type_param,
                name_type,
                optional,
                ty,
            },
        )
    }

    fn parse_tuple_type(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let mark = self.scratch.len();
        self.with_ctx(0, Ctx::DISALLOW_CONDITIONAL_TYPES, |p| {
            while !matches!(p.kind(), T::RBracket | T::Eof) && !p.aborting() {
                let s = p.start();
                let rest = p.eat(T::DotDotDot);
                let named = p.kind().is_identifier_or_keyword()
                    && p.lookahead(|p| {
                        p.bump();
                        if p.at(T::Question) {
                            p.bump();
                        }
                        p.at(T::Colon)
                    });
                let el = if named {
                    let name = p.parse_identifier_name();
                    let optional = p.eat(T::Question);
                    p.expect(T::Colon);
                    let ty = p.parse_type();
                    // `[name: T?]` is TS5086's, not a JSDoc nullable (tsc 7).
                    p.unnote_postfix_nullable(ty);
                    p.finish_node(
                        s,
                        Node::NamedTupleMember {
                            rest,
                            name,
                            optional,
                            ty,
                        },
                    )
                } else {
                    let ty = p.parse_type();
                    let ty = if let Some(inner) = p.jsdoc_postfix_nullable_inner(ty) {
                        // JSDoc `[T?]`: an optional element, not `T | null`; valid
                        // TypeScript unless under `...` (tsc 7 reports `[...T?]`).
                        if !rest {
                            p.unnote_postfix_nullable(ty);
                        }
                        p.finish_node(s, Node::OptionalType { ty: inner })
                    } else if p.at(T::Question) {
                        p.bump();
                        p.finish_node(s, Node::OptionalType { ty })
                    } else {
                        ty
                    };
                    if rest {
                        p.finish_node(s, Node::RestType { ty })
                    } else {
                        ty
                    }
                };
                p.scratch.push(el);
                if !p.eat(T::Comma) {
                    if !p.at(T::RBracket) {
                        p.error_expected(",");
                    }
                    break;
                }
            }
        });
        let elems = self.finish_list(mark);
        self.expect_closing(T::RBracket, T::LBracket, open);
        self.finish_node(start, Node::TupleType { elems })
    }

    fn parse_template_literal_type(&mut self) -> NodeId {
        let start = self.start();
        let mark = self.scratch.len();
        let first = self.template_element(false);
        self.scratch.push(first);
        if self.at(T::TemplateHead) {
            self.bump();
            loop {
                if self.aborting() {
                    break;
                }
                let t = self.parse_type_in_brackets();
                self.scratch.push(t);
                if !self.at(T::RBrace) {
                    self.error_expected("}");
                    break;
                }
                self.lx.rescan_template_continuation();
                let el = self.template_element(false);
                self.scratch.push(el);
                let tail = self.at(T::TemplateTail);
                self.bump();
                if tail {
                    break;
                }
            }
        } else {
            self.bump();
        }
        let parts = self.finish_list(mark);
        self.finish_node(start, Node::TemplateLitType { parts })
    }

    // ---- Object type members (interfaces and type literals) ----

    /// `{ members }` of an interface (`interface`) or a type literal.
    pub(crate) fn parse_object_type_members(&mut self, interface: bool) -> NodeList {
        let open = self.start();
        if !self.expect(T::LBrace) {
            return NodeList::EMPTY;
        }
        let mark = self.scratch.len();
        self.with_ctx(
            Ctx::TYPE,
            Ctx::DISALLOW_CONDITIONAL_TYPES | Ctx::DISALLOW_IN,
            |p| {
                while !matches!(p.kind(), T::RBrace | T::Eof) && !p.aborting() {
                    let before = p.tok().start;
                    if let Some(m) = p.parse_type_member(interface) {
                        p.scratch.push(m);
                    }
                    if p.tok().start == before && !p.at(T::RBrace) && !p.at(T::Eof) {
                        p.error(messages::PROPERTY_OR_SIGNATURE_EXPECTED);
                        p.bump();
                    }
                }
            },
        );
        let members = self.finish_list(mark);
        self.expect_closing(T::RBrace, T::LBrace, open);
        members
    }

    fn parse_type_member(&mut self, interface: bool) -> Option<NodeId> {
        let start = self.start();
        if self.at(T::LParen) || self.at(T::Lt) {
            let type_params = self.parse_type_params_opt();
            let params = self.parse_params();
            let return_type = self.parse_signature_return_type();
            self.type_member_separator();
            return Some(self.finish_node(
                start,
                Node::CallSignature {
                    type_params,
                    params,
                    return_type,
                },
            ));
        }
        if self.at(T::New) && matches!(self.peek_kind(), T::LParen | T::Lt) {
            self.bump();
            let type_params = self.parse_type_params_opt();
            let params = self.parse_params();
            let return_type = self.parse_signature_return_type();
            self.type_member_separator();
            return Some(self.finish_node(
                start,
                Node::ConstructSignature {
                    type_params,
                    params,
                    return_type,
                },
            ));
        }
        // `readonly`, and misplaced modifiers the checker reports (`public x: T`).
        let modifiers = self.parse_modifiers(ModCtx::TypeMember);
        if self.at(T::LBracket) && self.is_index_signature() {
            return Some(self.parse_index_signature(start, modifiers));
        }
        let mut kind = MethodKind::Method;
        if matches!(self.kind(), T::Get | T::Set) {
            let next = self.peek();
            if matches!(
                next.kind,
                T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName
            ) || next.kind.is_identifier_or_keyword()
            {
                kind = if self.at(T::Get) {
                    MethodKind::Get
                } else {
                    MethodKind::Set
                };
                self.bump();
            }
        }
        let is_key_start = matches!(
            self.kind(),
            T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName
        ) || self.kind().is_identifier_or_keyword();
        if !is_key_start {
            return None;
        }
        let key = self.parse_property_key();
        let optional = self.eat(T::Question);
        if kind != MethodKind::Method || matches!(self.kind(), T::LParen | T::Lt) {
            let type_params = self.parse_type_params_opt();
            let params = self.parse_params();
            let return_type = self.parse_signature_return_type();
            if kind != MethodKind::Method && self.at(T::LBrace) {
                // `get foo() { return 0 }` in a type: the body is parsed and dropped, with
                // TS1183 (accessorBodyInTypeContext).
                let body = self.with_ctx(
                    Ctx::IN_FUNCTION,
                    Ctx::TYPE
                        | Ctx::DISALLOW_CONDITIONAL_TYPES
                        | Ctx::DISALLOW_IN
                        | Ctx::AWAIT
                        | Ctx::YIELD,
                    |p| p.parse_function_body_block(),
                );
                let span = self.ast.span(body);
                self.deferred_grammar_error_at(messages::AMBIENT_IMPLEMENTATION, span, &[]);
            }
            self.type_member_separator();
            return Some(self.finish_node(
                start,
                Node::MethodSignature {
                    modifiers,
                    kind,
                    key,
                    optional,
                    type_params,
                    params,
                    return_type,
                },
            ));
        }
        let type_ann = if self.eat(T::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        if self.eat(T::Eq) {
            // Initializers are not allowed in types: a checker grammar error at the
            // initializer, which is dropped (errorOnInitializerIn*Property).
            let init = self.with_ctx(0, Ctx::TYPE, |p| p.parse_assignment());
            let msg = if interface {
                messages::INTERFACE_PROPERTY_INITIALIZER
            } else {
                messages::TYPE_LITERAL_PROPERTY_INITIALIZER
            };
            let span = self.ast.span(init);
            self.deferred_grammar_error_at(msg, span, &[]);
        }
        self.type_member_separator();
        Some(self.finish_node(
            start,
            Node::PropertySignature {
                modifiers,
                key,
                optional,
                type_ann,
            },
        ))
    }

    /// `;` or `,` between type members, or a line break / `}`.
    pub(crate) fn type_member_separator(&mut self) {
        if self.eat(T::Semi) || self.eat(T::Comma) {
            return;
        }
        if self.at(T::RBrace) || self.newline_before() || self.at(T::Eof) {
            return;
        }
        self.error_expected(";");
    }

    // ---- Type parameters and arguments ----

    pub(crate) fn parse_type_params_opt(&mut self) -> Option<NodeId> {
        if !self.ts || !self.at(T::Lt) {
            return None;
        }
        let start = self.start();
        self.bump();
        let mark = self.scratch.len();
        while !matches!(self.kind(), T::Gt | T::Eof) && !self.aborting() {
            let pstart = self.start();
            // `const`/`in`/`out`, and misplaced modifiers (`<public T>`).
            let modifiers = self.parse_modifiers(ModCtx::TypeParam);
            if !self.kind().is_identifier_like() {
                self.error(messages::TYPE_PARAMETER_DECLARATION_EXPECTED);
                if !matches!(self.kind(), T::Comma) {
                    break;
                }
            }
            let name = self.parse_identifier();
            let constraint = if self.eat(T::Extends) {
                Some(self.parse_type_in_brackets())
            } else {
                None
            };
            let default = if self.eat(T::Eq) {
                Some(self.parse_type_in_brackets())
            } else {
                None
            };
            let tp = self.finish_node(
                pstart,
                Node::TypeParam {
                    modifiers,
                    name,
                    constraint,
                    default,
                },
            );
            self.scratch.push(tp);
            if !self.eat(T::Comma) {
                break;
            }
        }
        let params = self.finish_list(mark);
        self.expect(T::Gt);
        if params.is_empty() {
            // `class C<> {}`: a grammar error at the `<` (classWithEmptyTypeParameter).
            self.deferred_grammar_error_at(
                messages::TYPE_PARAMETER_LIST_EMPTY,
                Span::new(start, self.prev_end),
                &[],
            );
        }
        Some(self.finish_node(start, Node::TypeParams { params }))
    }

    /// `<T, U>` at the current `<` (or `<<`, which is split).
    pub(crate) fn parse_type_args(&mut self) -> NodeId {
        let start = self.start();
        let was_failed = self.spec_failed;
        let r = self.parse_type_args_inner(start);
        if self.speculating > 0 && !was_failed && self.spec_failed {
            self.failed_type_args.insert(start);
        }
        r
    }

    /// Whether `<` here was already found not to start a type-argument list.
    pub(crate) fn type_args_known_to_fail(&self) -> bool {
        self.failed_type_args.contains(&self.tok().start)
    }

    fn parse_type_args_inner(&mut self, start: u32) -> NodeId {
        self.lx.split_less_than();
        self.expect(T::Lt);
        let mark = self.scratch.len();
        self.with_ctx(
            Ctx::TYPE,
            Ctx::DISALLOW_CONDITIONAL_TYPES | Ctx::DISALLOW_IN,
            |p| {
                while !matches!(p.kind(), T::Gt | T::Eof) && !p.aborting() {
                    let t = p.parse_type();
                    p.scratch.push(t);
                    if !p.eat(T::Comma) {
                        break;
                    }
                }
            },
        );
        let args = self.finish_list(mark);
        if self.at(T::Gt) {
            self.bump();
        } else {
            self.error_expected(">");
        }
        if args.is_empty() {
            // `f<>()`: a grammar error at the `<` (emptyTypeArgumentList).
            self.deferred_grammar_error_at(
                messages::TYPE_ARGUMENT_LIST_EMPTY,
                Span::new(start, self.prev_end),
                &[],
            );
        }
        self.finish_node(start, Node::TypeArgs { args })
    }
}
