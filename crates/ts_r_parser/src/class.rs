//! Classes, class members, parameters and decorators.

use ts_r_ast::{FnFlags, HeritageKind, MethodKind, Modifiers, Node, NodeId, NodeList, Span};
use ts_r_diagnostics::messages;
use ts_r_lexer::TokenKind as T;

use crate::grammar;
use crate::lists::List;
use crate::modifiers::ModCtx;
use crate::stmt::BodyRule;
use crate::{Ctx, Parser};

impl<'a> Parser<'a> {
    /// `class Name<T> extends B implements C { ... }` starting at the `class` keyword.
    pub(crate) fn parse_class(
        &mut self,
        start: u32,
        decorators: NodeList,
        modifiers: Modifiers,
        is_expr: bool,
    ) -> NodeId {
        self.expect(T::Class);
        let name = if self.at_identifier() && !self.at(T::Implements) {
            Some(self.parse_identifier())
        } else {
            if !is_expr && !modifiers.contains(Modifiers::DEFAULT) {
                // `class {}`, `class;`: no syntax error in tsc; its checker reports TS1211
                // at the declaration (exportClassWithoutName). `class void {}` is then
                // `'{' expected` at the `void` (classWithPredefinedTypesAsNames2).
                self.deferred_grammar_error_at(
                    messages::CLASS_DECLARATION_WITHOUT_NAME,
                    Span::new(start, start + 1),
                    &[],
                );
            }
            None
        };
        let type_params = self.parse_type_params_opt();
        let mark = self.scratch.len();
        let mut heritage_check = grammar::HeritageCheck::default();
        loop {
            let hstart = self.start();
            let kind = match self.kind() {
                T::Extends => HeritageKind::Extends,
                T::Implements => HeritageKind::Implements,
                _ => break,
            };
            let keyword_span = self.tok().span();
            let keyword = if kind == HeritageKind::Extends {
                "extends"
            } else {
                "implements"
            };
            self.bump();
            let types = self.parse_heritage_types(keyword);
            self.check_class_heritage(&mut heritage_check, kind, keyword_span, types);
            let h = self.finish_node(hstart, Node::HeritageClause { kind, types });
            self.scratch.push(h);
        }
        let heritage = self.finish_list(mark);
        let members = self.parse_class_body();
        let node = if is_expr {
            Node::ClassExpr {
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            }
        } else {
            Node::ClassDecl {
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            }
        };
        self.finish_node(start, node)
    }

    /// The expression after `extends` (a left-hand-side expression).
    pub(crate) fn parse_heritage_expression(&mut self) -> NodeId {
        self.with_ctx(0, Ctx::DISALLOW_IN | Ctx::IN_CONDITIONAL_TRUE, |p| {
            p.parse_lhs()
        })
    }

    fn parse_class_body(&mut self) -> NodeList {
        let open = self.start();
        if !self.expect(T::LBrace) {
            return NodeList::EMPTY;
        }
        let mark = self.scratch.len();
        self.with_ctx(
            Ctx::IN_CLASS,
            Ctx::DISALLOW_IN | Ctx::IN_CONDITIONAL_TRUE | Ctx::DECORATOR,
            |p| {
                p.in_list(List::ClassMembers, |p| {
                    loop {
                        if p.aborting() || p.is_list_terminator(List::ClassMembers) {
                            break;
                        }
                        if !p.is_list_element(List::ClassMembers, false) {
                            if p.abort_list_or_skip(List::ClassMembers) {
                                break;
                            }
                            continue;
                        }
                        let before = p.tok().start;
                        let before_kind = p.kind();
                        if let Some(m) = p.parse_class_member() {
                            p.scratch.push(m);
                        }
                        if p.tok().start == before
                            && p.kind() == before_kind
                            && !p.at(T::Eof)
                            && p.abort_list_or_skip(List::ClassMembers)
                        {
                            break;
                        }
                    }
                })
            },
        );
        let members = self.finish_list(mark);
        self.expect_closing(T::RBrace, T::LBrace, open);
        members
    }

    fn parse_class_member(&mut self) -> Option<NodeId> {
        let start = self.start();
        if self.eat(T::Semi) {
            return None;
        }
        let mut decorators = if self.at(T::At) {
            self.parse_decorators()
        } else {
            NodeList::EMPTY
        };
        // Includes `async`, and modifiers the checker rejects (`const x = 1`, `in x`,
        // `public static {}`). Stops before the `static` of a static block.
        let mut modifiers = self.parse_modifiers(ModCtx::ClassMember);
        // `public @dec m() {}`: TS1436 at the `@`; the decorators and further modifiers
        // still belong to the member (decoratorOnClassMethod3).
        if !modifiers.is_empty() && self.at(T::At) {
            self.error(messages::DECORATORS_MUST_PRECEDE_NAME_AND_KEYWORDS);
            let late = self.parse_decorators();
            if decorators.is_empty() {
                decorators = late;
            }
            let more = self.parse_modifiers(ModCtx::ClassMember);
            modifiers.insert(more);
        }
        // `public var x`: TS1440 at `var`, which is skipped (ClassDeclaration26).
        if !modifiers.is_empty() && self.at(T::Var) {
            self.error(messages::VARIABLE_DECLARATION_NOT_ALLOWED_HERE);
            self.bump();
            let more = self.parse_modifiers(ModCtx::ClassMember);
            modifiers.insert(more);
        }
        if self.at(T::Static) && self.peek_kind() == T::LBrace {
            self.bump();
            let open = self.start();
            self.bump();
            let body = self.with_ctx(Ctx::IN_FUNCTION, Ctx::AWAIT | Ctx::YIELD, |p| {
                p.with_jump(crate::grammar::Jump::StaticBlock, |p| {
                    p.parse_statements(false, false)
                })
            });
            self.check_statement_list(body, grammar::Container::Block);
            self.expect_closing(T::RBrace, T::LBrace, open);
            return Some(self.finish_node(start, Node::StaticBlock { body }));
        }
        if self.at(T::LBracket) && self.is_index_signature() {
            return Some(self.parse_index_signature(start, modifiers));
        }
        let mut flags = FnFlags::NONE;
        let mut kind = MethodKind::Method;
        let mut is_method = false;
        let star = self.at(T::Star).then(|| self.tok().span());
        if self.eat(T::Star) {
            flags |= FnFlags::GENERATOR;
            is_method = true;
        }
        if !is_method
            && matches!(self.kind(), T::Get | T::Set)
            && self.next_can_follow_member_modifier(false)
        {
            kind = if self.at(T::Get) {
                MethodKind::Get
            } else {
                MethodKind::Set
            };
            self.bump();
            is_method = true;
        }
        if !self.at_property_key_start() {
            if decorators.is_empty() && modifiers.is_empty() && !is_method {
                return None;
            }
            if !modifiers.is_empty() && !is_method && !matches!(self.kind(), T::LParen | T::Lt) {
                // `public {};`: TS1146 just after the modifiers, then a nameless property
                // whose `;` is missing (classMemberWithMissingIdentifier).
                self.error_at(messages::DECLARATION_EXPECTED, Span::at(self.prev_end), &[]);
                let key = self.missing();
                self.property_semicolon(key, false, false);
                return Some(self.finish_node(
                    start,
                    Node::PropertyDef {
                        decorators,
                        modifiers,
                        key,
                        optional: false,
                        definite: false,
                        type_ann: None,
                        value: None,
                    },
                ));
            }
            self.error(messages::IDENTIFIER_EXPECTED);
        }
        let key = self.parse_property_key();
        if modifiers.contains(Modifiers::CONST) {
            let span = self.ast.span(key);
            self.deferred_grammar_error_at(
                messages::CLASS_MEMBER_CANNOT_HAVE_KEYWORD,
                span,
                &["const"],
            );
        }
        // `async constructor()` is still a constructor (the checker rejects `async`).
        if kind == MethodKind::Method
            && flags.is_empty()
            && self.is_constructor_key(key)
            && matches!(self.kind(), T::LParen | T::Lt)
        {
            kind = MethodKind::Constructor;
        }
        let optional = self.eat(T::Question);
        let bang = self.tok().span();
        let definite = !optional && self.at(T::Bang) && !self.newline_before() && {
            self.bump();
            true
        };
        if is_method || matches!(self.kind(), T::LParen | T::Lt) {
            if modifiers.contains(Modifiers::ASYNC) {
                modifiers.remove(Modifiers::ASYNC);
                flags |= FnFlags::ASYNC;
            }
            let fstart = self.start();
            // Accessors need a body outside ambient contexts unless they are abstract.
            let accessor = matches!(kind, MethodKind::Get | MethodKind::Set);
            let rule = if !accessor
                || self.ctx.has(Ctx::AMBIENT)
                || modifiers.intersects(Modifiers::ABSTRACT | Modifiers::DECLARE)
            {
                BodyRule::Optional
            } else {
                BodyRule::Accessor
            };
            let (type_params, params, return_type, body) = self.parse_function_rest(flags, rule);
            self.method_grammar(kind, key, type_params, params, return_type);
            self.check_ambient_function(star, body);
            if matches!(kind, MethodKind::Get | MethodKind::Set)
                && modifiers.contains(Modifiers::ABSTRACT)
                && body.is_some()
            {
                let span = self.ast.span(key);
                self.deferred_grammar_error_at(messages::ABSTRACT_ACCESSOR_BODY, span, &[]);
            }
            let value = self.finish_node(
                fstart,
                Node::FunctionExpr {
                    modifiers: Modifiers::NONE,
                    flags,
                    name: None,
                    type_params,
                    params,
                    return_type,
                    body,
                },
            );
            return Some(self.finish_node(
                start,
                Node::MethodDef {
                    decorators,
                    modifiers,
                    kind,
                    key,
                    optional,
                    value,
                },
            ));
        }
        let type_ann = if self.ts && self.eat(T::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let value = if self.eat(T::Eq) {
            Some(
                self.with_ctx(Ctx::IN_FUNCTION, Ctx::AWAIT | Ctx::YIELD, |p| {
                    p.parse_assignment()
                }),
            )
        } else {
            None
        };
        // Decorators on a `declare` field are an error of their own (TS1206) that tsc reports
        // instead.
        if let Some(v) = value
            && (self.ctx.has(Ctx::AMBIENT)
                || (modifiers.contains(Modifiers::DECLARE) && decorators.is_empty()))
        {
            let const_like = modifiers.contains(Modifiers::READONLY) && type_ann.is_none();
            self.check_ambient_initializer(v, const_like);
        }
        if definite {
            let not_permitted = self.ctx.has(Ctx::AMBIENT)
                || modifiers
                    .intersects(Modifiers::DECLARE | Modifiers::STATIC | Modifiers::ABSTRACT);
            self.check_definite(bang, not_permitted, type_ann.is_some(), value.is_some());
        }
        let bare = !optional && !definite && type_ann.is_none() && value.is_none();
        self.property_semicolon(key, bare, type_ann.is_some() && value.is_none());
        Some(self.finish_node(
            start,
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                optional,
                definite,
                type_ann,
                value,
            },
        ))
    }

    fn at_property_key_start(&self) -> bool {
        matches!(
            self.kind(),
            T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName
        ) || self.kind().is_identifier_or_keyword()
    }

    fn is_constructor_key(&self, key: NodeId) -> bool {
        match *self.ast.node(key) {
            Node::Ident { name } | Node::StringLit { value: name } => {
                self.ast.atom(name) == "constructor"
            }
            _ => false,
        }
    }

    /// The grammar errors tsc's checker reports for accessor and constructor signatures
    /// (deferred: only in files without syntax errors), the first that applies.
    pub(crate) fn method_grammar(
        &mut self,
        kind: MethodKind,
        key: NodeId,
        type_params: Option<NodeId>,
        params: NodeList,
        return_type: Option<NodeId>,
    ) {
        let at_key = self.ast.span(key);
        if kind == MethodKind::Constructor {
            self.check_constructor_params(params);
        }
        let error = match kind {
            MethodKind::Constructor => match (type_params, return_type) {
                // At the list's position, just after the `<`.
                (Some(tp), _) => Some((
                    messages::CONSTRUCTOR_TYPE_PARAMETERS,
                    Span::at(self.ast.span(tp).start + 1),
                )),
                (None, Some(t)) => Some((messages::CONSTRUCTOR_TYPE_ANNOTATION, self.ast.span(t))),
                (None, None) => None,
            },
            MethodKind::Get | MethodKind::Set if type_params.is_some() => {
                Some((messages::ACCESSOR_TYPE_PARAMETERS, at_key))
            }
            MethodKind::Get => {
                let params = self.ast.list(params);
                let this = params.first().is_some_and(|&p| self.is_this_param(p));
                (params.len() > usize::from(this))
                    .then_some((messages::GET_ACCESSOR_PARAMETERS, at_key))
            }
            MethodKind::Set if return_type.is_some() => {
                Some((messages::SET_ACCESSOR_RETURN_TYPE, at_key))
            }
            MethodKind::Set => {
                let params = self.ast.list(params);
                let this = params.first().is_some_and(|&p| self.is_this_param(p));
                let params = &params[usize::from(this)..];
                match params {
                    [p] => match *self.ast.node(*p) {
                        Node::Param { rest: true, .. } => Some((
                            messages::SET_ACCESSOR_REST_PARAMETER,
                            Span::at(self.ast.span(*p).start),
                        )),
                        // At the `?` after the name.
                        Node::Param {
                            optional: true,
                            pattern,
                            ..
                        } => Some((
                            messages::SET_ACCESSOR_OPTIONAL_PARAMETER,
                            Span::at(self.ast.span(pattern).end),
                        )),
                        Node::Param { init: Some(_), .. } => {
                            Some((messages::SET_ACCESSOR_PARAMETER_INITIALIZER, at_key))
                        }
                        _ => None,
                    },
                    _ => Some((messages::SET_ACCESSOR_ONE_PARAMETER, at_key)),
                }
            }
            MethodKind::Method => None,
        };
        if let Some((msg, span)) = error {
            self.deferred_grammar_error_at(msg, span, &[]);
        }
    }

    fn is_this_param(&self, p: NodeId) -> bool {
        match *self.ast.node(p) {
            Node::Param { pattern, .. } => {
                matches!(*self.ast.node(pattern), Node::Ident { name } if self.ast.atom(name) == "this")
            }
            _ => false,
        }
    }

    /// Can the token after the current modifier keyword follow it (so the keyword is a
    /// modifier, not a member name)?
    fn next_can_follow_member_modifier(&mut self, same_line: bool) -> bool {
        let next = self.peek();
        if same_line && next.newline_before() {
            return false;
        }
        match next.kind {
            T::String
            | T::Number
            | T::BigInt
            | T::LBracket
            | T::LBrace
            | T::PrivateName
            | T::Star
            | T::At => true,
            k => k.is_identifier_or_keyword(),
        }
    }

    /// `[` starts an index signature: `[key: T]`, `[a, b]`-style errors, `[...x: T]` or `[]`
    /// (TS1096, parserIndexSignature9).
    pub(crate) fn is_index_signature(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if p.at(T::DotDotDot) || p.at(T::RBracket) {
                return true;
            }
            if matches!(
                p.kind(),
                T::Public | T::Private | T::Protected | T::Readonly
            ) {
                p.bump();
                return p.kind().is_identifier_like();
            }
            if !p.kind().is_identifier_like() {
                return false;
            }
            p.bump();
            matches!(p.kind(), T::Colon | T::Comma)
                || (p.at(T::Question) && {
                    p.bump();
                    p.at(T::Colon)
                })
        })
    }

    pub(crate) fn parse_index_signature(
        &mut self,
        start: u32,
        modifiers: ts_r_ast::Modifiers,
    ) -> NodeId {
        let open = self.start();
        self.expect(T::LBracket);
        let mark = self.scratch.len();
        while !matches!(self.kind(), T::RBracket | T::Eof) && !self.aborting() {
            let p = self.parse_param();
            self.scratch.push(p);
            let comma = self.tok().span();
            if !self.eat(T::Comma) {
                break;
            }
            if self.at(T::RBracket) {
                self.deferred_grammar_error_at(
                    messages::INDEX_SIGNATURE_TRAILING_COMMA,
                    comma,
                    &[],
                );
            }
        }
        let params = self.finish_list(mark);
        self.expect_closing(T::RBracket, T::LBracket, open);
        let type_ann = if self.eat(T::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        self.type_member_separator();
        let sig = self.finish_node(
            start,
            Node::IndexSignature {
                modifiers,
                params,
                type_ann,
            },
        );
        self.check_index_signature(sig, params, type_ann);
        sig
    }

    // ---- Parameters ----

    /// `( params )`.
    pub(crate) fn parse_params(&mut self) -> NodeList {
        let open = self.start();
        if !self.expect(T::LParen) {
            return NodeList::EMPTY;
        }
        let mark = self.scratch.len();
        self.with_ctx(
            0,
            Ctx::DISALLOW_IN
                | Ctx::IN_CONDITIONAL_TRUE
                | Ctx::DECORATOR
                | Ctx::TYPE
                | Ctx::DISALLOW_CONDITIONAL_TYPES,
            |p| {
                while !matches!(p.kind(), T::RParen | T::Eof) && !p.aborting() {
                    let before = p.tok().start;
                    if p.at_skipped_in_params() {
                        p.error(messages::PARAMETER_DECLARATION_EXPECTED);
                        p.bump();
                        continue;
                    }
                    if reserved_word_ends_params(p.kind()) {
                        // `function f(enum) {}`: the list ends; `enum` starts a statement.
                        p.error(messages::PARAMETER_DECLARATION_EXPECTED);
                        break;
                    }
                    let nameless = p.at_nameless_param_start();
                    let param = p.parse_param();
                    p.scratch.push(param);
                    if nameless {
                        // tsc reads the next parameter straight away (`f(! b)` is two), and
                        // skips a comma there with TS1138.
                        if p.at(T::Comma) {
                            p.error(messages::PARAMETER_DECLARATION_EXPECTED);
                            p.bump();
                        }
                        continue;
                    }
                    if matches!(p.ast.node(param), Node::Param { rest: true, .. }) {
                        p.check_rest_trailing_comma(T::RParen);
                    }
                    if !p.eat(T::Comma) && !matches!(p.kind(), T::RParen | T::Eof) {
                        p.error_expected(",");
                        // As in tsc, the list goes on at anything that starts a parameter
                        // (or a type, which becomes a parameter with a missing name) and
                        // skips a few tokens that cannot end it; other tokens end it.
                        if p.tok().start == before
                            || !(p.at_identifier()
                                || matches!(
                                    p.kind(),
                                    T::LBrace | T::LBracket | T::DotDotDot | T::At
                                )
                                || p.at_nameless_param_start()
                                || p.at_skipped_in_params())
                        {
                            break;
                        }
                    }
                }
            },
        );
        let params = self.finish_list(mark);
        self.check_params(params);
        self.expect_closing(T::RParen, T::LParen, open);
        params
    }

    pub(crate) fn parse_param(&mut self) -> NodeId {
        let start = self.start();
        let decorators = if self.at(T::At) {
            self.parse_decorators()
        } else {
            NodeList::EMPTY
        };
        // Also in JavaScript, and modifiers no parameter allows (`static x`): the checker
        // reports them.
        let modifiers = self.parse_modifiers(ModCtx::Param);
        let rest = self.eat(T::DotDotDot);
        if self.at_nameless_param_start() {
            // `f(a!: T)`, `f(a<T>)`, `f(a 1)`: tsc makes a parameter with a missing name
            // (declared as `(Missing)`) of just the token, which is skipped.
            self.error(messages::IDENTIFIER_EXPECTED);
            let pattern = self.missing();
            self.bump();
            return self.finish_node(
                start,
                Node::Param {
                    decorators,
                    modifiers,
                    rest,
                    pattern,
                    optional: false,
                    type_ann: None,
                    init: None,
                },
            );
        }
        let pattern = if self.at(T::This) {
            let s = self.start();
            let name = self.intern_token();
            self.bump();
            self.finish_node(s, Node::Ident { name })
        } else if self.at_identifier()
            || matches!(self.kind(), T::Await | T::Yield | T::LBrace | T::LBracket)
        {
            self.parse_binding_target()
        } else {
            let k = self.kind();
            if k.is_reserved() && !reserved_word_ends_params(k) {
                // A reserved word as a parameter name (`f(null)`, `f(export)`): TS1359, the
                // name is missing, as in tsc, and the word is consumed.
                let span = self.tok().span();
                let word = self
                    .src
                    .get(span.start as usize..span.end as usize)
                    .unwrap_or("");
                self.error_at(messages::RESERVED_WORD_CANNOT_BE_USED_HERE, span, &[word]);
                let name = self.missing();
                self.bump();
                name
            } else if k.is_identifier_or_keyword() && !reserved_word_ends_params(k) {
                // A contextually reserved name (`yield` in a generator): an identifier, which
                // the binder reports.
                self.error(messages::PARAMETER_DECLARATION_EXPECTED);
                let s = self.start();
                let name = self.intern_token();
                self.bump();
                self.finish_node(s, Node::Ident { name })
            } else if rest {
                // `f(...)`: a missing name after `...` is TS1003 (restArgMissingName).
                self.error(messages::IDENTIFIER_EXPECTED);
                self.missing()
            } else {
                self.error(messages::PARAMETER_DECLARATION_EXPECTED);
                self.missing()
            }
        };
        let optional = self.ts && self.eat(T::Question);
        let type_ann = if self.ts && self.eat(T::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let init = if self.eat(T::Eq) {
            Some(self.parse_assignment())
        } else {
            None
        };
        self.finish_node(
            start,
            Node::Param {
                decorators,
                modifiers,
                rest,
                pattern,
                optional,
                type_ann,
                init,
            },
        )
    }

    /// A token that starts a type but not a binding (`!`, `<`, `|`, a literal): in a
    /// parameter list tsc reads it as a parameter whose name is missing.
    fn at_nameless_param_start(&self) -> bool {
        matches!(
            self.kind(),
            T::Bang
                | T::Lt
                | T::Pipe
                | T::Amp
                | T::Star
                | T::String
                | T::Number
                | T::BigInt
                | T::NoSubstTemplate
        )
    }

    /// Tokens tsc skips inside a parameter list, with TS1138.
    fn at_skipped_in_params(&self) -> bool {
        matches!(
            self.kind(),
            T::Colon | T::Semi | T::Dot | T::Arrow | T::RBrace
        )
    }

    // ---- Decorators ----

    pub(crate) fn parse_decorators(&mut self) -> NodeList {
        let mark = self.scratch.len();
        while self.at(T::At) && !self.aborting() {
            let start = self.start();
            self.bump();
            let expr = self.with_ctx(Ctx::DECORATOR, Ctx::IN_CONDITIONAL_TRUE, |p| p.parse_lhs());
            if !self.is_valid_decorator_expression(expr) {
                let span = self.ast.span(expr);
                self.grammar_error_at(messages::DECORATOR_NEEDS_PARENTHESES, span, &[]);
            }
            let d = self.finish_node(start, Node::Decorator { expr });
            self.scratch.push(d);
        }
        self.finish_list(mark)
    }

    /// Decorator expressions tsc 7 accepts without TS1497 (a grammar error there): `(e)`,
    /// or a chain of an identifier, `.name` and `!` links, optionally called once
    /// (`@a.b!.c<T>()`). Optional chains, `a()()`, `a().b`, `(a).b`, `this.x`, `new a()`
    /// and tagged templates need parentheses. `Missing` (already an error) is accepted.
    fn is_valid_decorator_expression(&self, expr: NodeId) -> bool {
        let chain = match *self.ast.node(expr) {
            Node::Paren { .. } | Node::Missing {} => return true,
            Node::Call {
                callee,
                optional: false,
                ..
            } => callee,
            _ => expr,
        };
        let mut id = chain;
        loop {
            match *self.ast.node(id) {
                Node::Ident { .. } => return true,
                Node::Member {
                    obj,
                    optional: false,
                    ..
                } => id = obj,
                Node::NonNull { expr } => id = expr,
                _ => return false,
            }
        }
    }
}

/// A reserved word that ends a parameter list instead of being taken as a (missing)
/// parameter name, as in tsc: one that starts a statement or declaration (`f(enum) {}`
/// leaves `enum` for an enum declaration). Expression keywords (`f(null)`, `f(void)`) and
/// `default` are consumed.
fn reserved_word_ends_params(k: T) -> bool {
    matches!(
        k,
        T::Function
            | T::Class
            | T::New
            | T::Enum
            | T::Var
            | T::Const
            | T::If
            | T::For
            | T::While
            | T::Do
            | T::Switch
            | T::Try
            | T::Throw
            | T::Return
            | T::Break
            | T::Continue
            | T::With
            | T::Debugger
            | T::Import
    )
}
