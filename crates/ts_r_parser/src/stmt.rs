//! Statements, declarations, and module syntax (import/export).

use ts_r_ast::{
    FnFlags, ImportPhase, Modifiers, ModuleKind, Node, NodeId, NodeList, Span, VarKind,
};
use ts_r_diagnostics::messages;
use ts_r_lexer::TokenKind as T;

use crate::grammar::{Container, Jump};
use crate::lists::List;
use crate::modifiers::{MAX_MODIFIER_CHAIN, ModCtx};
use crate::{Ctx, Parser};

/// Whether a function-like may omit its body (see `Parser::parse_function_rest`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyRule {
    /// Object-literal methods: `'{' expected` (see `Accessor`), with an empty body.
    Required,
    /// Declarations and overloads: `;` or TS1144.
    Optional,
    /// Accessors outside ambient contexts: before `;` or where one could be inserted, a
    /// grammar `'{' expected`; else a syntax error.
    Accessor,
}

impl<'a> Parser<'a> {
    pub(crate) fn parse_program(&mut self) {
        let stmts = self.parse_statements(true, true);
        self.check_ambient_statements(stmts, 0);
        self.check_statement_list(stmts, Container::File);
        let end = self.src.len() as u32;
        self.ast.root = self.ast.add(Node::Program { stmts }, Span::new(0, end));
    }

    /// Statements until `}` (unless `top_level`) or end of file.
    pub(crate) fn parse_statements(&mut self, top_level: bool, directives: bool) -> NodeList {
        let list = if top_level {
            List::SourceElements
        } else {
            List::BlockStatements
        };
        self.parse_statement_list(list, directives)
    }

    /// The statements of `list` (source elements, a block, or a `case` clause), with tsc's
    /// list recovery: a token that starts no statement is reported and skipped, or ends the
    /// list when an enclosing list can use it.
    pub(crate) fn parse_statement_list(&mut self, list: List, directives: bool) -> NodeList {
        let mark = self.scratch.len();
        let mut prologue = directives;
        self.in_list(list, |p| {
            loop {
                if p.aborting() || p.is_list_terminator(list) {
                    break;
                }
                if prologue {
                    if p.at(T::String) && p.is_directive() {
                        let start = p.start();
                        let tok = p.tok();
                        let value = p.lx.string_value(tok);
                        let value = p.ast.intern(&value);
                        p.bump();
                        p.semicolon();
                        let d = p.finish_node(start, Node::Directive { value });
                        p.scratch.push(d);
                        continue;
                    }
                    prologue = false;
                }
                if !p.is_list_element(list, false) {
                    if p.abort_list_or_skip(list) {
                        break;
                    }
                    continue;
                }
                let before = p.tok().start;
                let before_kind = p.kind();
                if let Some(s) = p.parse_statement() {
                    p.scratch.push(s);
                }
                if p.tok().start == before
                    && p.kind() == before_kind
                    && !p.at(T::Eof)
                    && p.abort_list_or_skip(list)
                {
                    break;
                }
            }
        });
        self.finish_list(mark)
    }

    /// A string literal statement that forms a directive (`"use strict";`). After a newline
    /// the string must still be the whole expression: `'a'\n.length` and `'a'\n[0]` go on.
    fn is_directive(&mut self) -> bool {
        let next = self.peek();
        if matches!(next.kind, T::Semi | T::RBrace | T::Eof) {
            return true;
        }
        if !next.newline_before() {
            return false;
        }
        let end = self.tok().end;
        self.lookahead(|p| {
            p.parse_expression();
            p.prev_end == end
        })
    }

    /// Parses one statement or declaration. Returns `None` after reporting an error for a
    /// token that cannot start a statement (the caller skips it).
    pub(crate) fn parse_statement(&mut self) -> Option<NodeId> {
        if !self.enter() {
            self.leave();
            self.skip_to_statement_end();
            return None;
        }
        let r = self.parse_statement_inner();
        self.leave();
        r
    }

    /// After a depth error: skips tokens to a likely statement end.
    fn skip_to_statement_end(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => return,
                T::LBrace | T::LParen | T::LBracket => depth += 1,
                T::RBrace | T::RParen | T::RBracket => {
                    depth -= 1;
                    if depth < 0 {
                        return;
                    }
                }
                T::Semi if depth <= 0 => {
                    self.bump();
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_statement_inner(&mut self) -> Option<NodeId> {
        let start = self.start();
        match self.kind() {
            T::LBrace => Some(self.parse_block()),
            T::Semi => {
                self.bump();
                Some(self.finish_node(start, Node::Empty {}))
            }
            T::Var => {
                self.bump();
                Some(self.parse_var_statement(start, Modifiers::NONE, VarKind::Var))
            }
            T::Let if self.is_let_declaration() => {
                self.bump();
                Some(self.parse_var_statement(start, Modifiers::NONE, VarKind::Let))
            }
            T::Public
            | T::Private
            | T::Protected
            | T::Static
            | T::Readonly
            | T::Accessor
            | T::Abstract
            | T::Async
            | T::Declare
                if self.declaration_follows_modifiers() =>
            {
                Some(self.parse_declaration_with_modifiers(start, NodeList::EMPTY, Modifiers::NONE))
            }
            _ if self.is_stray_statement_modifier() => {
                self.error(messages::DECLARATION_OR_STATEMENT_EXPECTED);
                self.bump();
                None
            }
            T::Export if self.is_stray_export() => {
                self.error(messages::DECLARATION_OR_STATEMENT_EXPECTED);
                self.bump();
                None
            }
            T::Const => {
                if self.ts && self.peek_kind() == T::Enum {
                    let m = self.bump_modifier(Modifiers::CONST);
                    return Some(self.parse_enum(start, m));
                }
                self.bump();
                Some(self.parse_var_statement(start, Modifiers::NONE, VarKind::Const))
            }
            T::Using if self.is_using_declaration() => {
                self.bump();
                Some(self.parse_var_statement(start, Modifiers::NONE, VarKind::Using))
            }
            T::Await if self.is_await_using_declaration() => {
                self.bump();
                self.bump();
                Some(self.parse_var_statement(start, Modifiers::NONE, VarKind::AwaitUsing))
            }
            T::Function => Some(self.parse_function_declaration(start, Modifiers::NONE)),
            T::Class => Some(self.parse_class(start, NodeList::EMPTY, Modifiers::NONE, false)),
            T::At => Some(self.parse_decorated_statement(start)),
            T::If => Some(self.parse_if()),
            T::For => Some(self.parse_for()),
            T::While => {
                self.bump();
                let test = self.parse_paren_condition();
                let body = self.with_jump(Jump::Loop, |p| p.parse_embedded_statement());
                Some(self.finish_node(start, Node::While { test, body }))
            }
            T::Do => Some(self.parse_do_while()),
            T::Return => {
                self.bump();
                let arg = if self.can_insert_semicolon() {
                    None
                } else {
                    Some(self.parse_expression())
                };
                self.semicolon();
                let stmt = self.finish_node(start, Node::Return { arg });
                self.check_return(stmt);
                Some(stmt)
            }
            T::Break | T::Continue => {
                let is_break = self.at(T::Break);
                self.bump();
                let label = if self.can_insert_semicolon() {
                    None
                } else if self.at_identifier() {
                    Some(self.parse_identifier())
                } else {
                    // `break 1;`, `break await;` in an async function: TS1003 (TS1359 for a
                    // reserved word), and the token is left for the next statement.
                    if self.kind().is_reserved() {
                        self.reserved_word_error();
                    } else {
                        self.error(messages::IDENTIFIER_EXPECTED);
                    }
                    None
                };
                self.semicolon();
                let node = if is_break {
                    Node::Break { label }
                } else {
                    Node::Continue { label }
                };
                let stmt = self.finish_node(start, node);
                self.check_jump(stmt, is_break, label);
                Some(stmt)
            }
            T::Throw => {
                self.bump();
                if self.newline_before() {
                    // At the end of `throw`, as in tsc 7.
                    let at = Span::at(self.prev_end);
                    self.error_at(messages::LINE_BREAK_NOT_PERMITTED_HERE, at, &[]);
                }
                let arg = self.parse_expression();
                self.semicolon();
                Some(self.finish_node(start, Node::Throw { arg }))
            }
            T::Try | T::Catch | T::Finally => Some(self.parse_try()),
            T::Switch => Some(self.parse_switch()),
            T::With => {
                self.bump();
                let obj = self.parse_paren_condition();
                let body = self.in_with_body(|p| p.parse_embedded_statement());
                Some(self.finish_node(start, Node::With { obj, body }))
            }
            T::Debugger => {
                self.bump();
                self.semicolon();
                Some(self.finish_node(start, Node::Debugger {}))
            }
            T::Import => {
                let next = self.peek_kind();
                if matches!(next, T::LParen | T::Dot)
                    || (self.ts && matches!(next, T::Lt | T::LtLt))
                {
                    Some(self.parse_expression_statement())
                } else {
                    Some(self.parse_import(start, Modifiers::NONE))
                }
            }
            T::Export => Some(self.parse_export(start, NodeList::EMPTY)),
            T::Enum => Some(self.parse_enum(start, Modifiers::NONE)),
            T::Interface
            | T::Type
            | T::Namespace
            | T::Module
            | T::Declare
            | T::Abstract
            | T::Global
                if self.is_contextual_declaration() =>
            {
                Some(self.parse_declaration_with_modifiers(start, NodeList::EMPTY, Modifiers::NONE))
            }
            // `default function () {}`: `'export' expected` at `default`, then the
            // declaration (defaultKeywordWithoutExport2, classAbstractManyKeywords).
            T::Default if matches!(self.peek_kind(), T::Function | T::Class | T::Abstract) => {
                self.error_expected("export");
                self.bump();
                self.parse_statement_inner()
            }
            T::Else | T::Case | T::Default => {
                self.error(messages::DECLARATION_OR_STATEMENT_EXPECTED);
                self.bump();
                None
            }
            T::RBrace | T::RParen | T::RBracket | T::Eof => {
                self.error(messages::DECLARATION_OR_STATEMENT_EXPECTED);
                None
            }
            // `</` in JSX is one token for tsc, a closing tag that starts nothing: it is
            // skipped whole (jsxInvalidEsprimaTestSuite `</>;`).
            T::Lt if self.jsx && self.at_lt_slash() => {
                self.error(messages::DECLARATION_OR_STATEMENT_EXPECTED);
                self.bump();
                self.bump();
                None
            }
            _ => {
                // A binary operator starts an expression whose left operand is missing
                // (`* b` is TS1109 at `*`), as in tsc.
                if self.is_start_of_expression() {
                    Some(self.parse_expression_statement())
                } else {
                    self.error(messages::DECLARATION_OR_STATEMENT_EXPECTED);
                    self.bump();
                    None
                }
            }
        }
    }

    /// A substatement (`if (x) <stmt>`); always yields a node.
    fn parse_embedded_statement(&mut self) -> NodeId {
        let s = self.parse_label_body();
        self.check_embedded_declaration(s);
        s
    }

    /// A labelled statement's body: as [`Self::parse_embedded_statement`], but a declaration
    /// is fine here unless the label itself is a substatement.
    fn parse_label_body(&mut self) -> NodeId {
        // `if (a) }`, `if (a) else b`: tsc reads an expression statement here, so the token
        // is `Expression expected` and is left to the enclosing construct
        // (parserErrorRecoveryIfStatement4).
        let ends_construct = match self.kind() {
            T::RBrace | T::RParen | T::RBracket | T::Eof | T::Else | T::Case => true,
            T::Default => !matches!(self.peek_kind(), T::Function | T::Class | T::Abstract),
            _ => false,
        };
        if ends_construct {
            self.error(messages::EXPRESSION_EXPECTED);
            let pos = self.prev_end;
            return self.add_node(Span::at(pos), Node::Empty {});
        }
        match self.parse_statement() {
            Some(s) => s,
            None => {
                let pos = self.prev_end;
                self.add_node(Span::at(pos), Node::Empty {})
            }
        }
    }

    fn parse_expression_statement(&mut self) -> NodeId {
        let start = self.start();
        let expr = self.parse_expression();
        if self.at(T::Colon)
            && matches!(self.ast.node(expr), Node::Ident { .. })
            && self.ast.span(expr).start == start
        {
            self.bump();
            let jump = match *self.ast.node(expr) {
                Node::Ident { name } => Jump::Label {
                    name,
                    is_loop: self.at_loop_statement(),
                },
                _ => Jump::Boundary,
            };
            let body = self.with_jump(jump, |p| p.parse_label_body());
            let stmt = self.finish_node(start, Node::Labeled { label: expr, body });
            self.check_label(stmt, body);
            return stmt;
        }
        self.expression_statement_semicolon(expr);
        self.finish_node(start, Node::ExprStmt { expr })
    }

    pub(crate) fn parse_block(&mut self) -> NodeId {
        self.parse_block_with(false)
    }

    /// `{ ... }` function body with directive prologue. Returns a `Block`.
    pub(crate) fn parse_function_body_block(&mut self) -> NodeId {
        self.with_jump(Jump::Boundary, |p| p.parse_block_with(true))
    }

    /// A block. Without its `{` it is empty, as in tsc: nothing after the error is read as
    /// its statements.
    fn parse_block_with(&mut self, directives: bool) -> NodeId {
        let start = self.start();
        let open = self.start();
        if !self.expect(T::LBrace) {
            return self.finish_node(
                start,
                Node::Block {
                    stmts: NodeList::EMPTY,
                },
            );
        }
        let stmts = self.parse_statements(false, directives);
        self.check_statement_list(stmts, Container::Block);
        if self.expect_closing(T::RBrace, T::LBrace, open) {
            self.last_block_end = self.prev_end;
        }
        self.finish_node(start, Node::Block { stmts })
    }

    fn parse_paren_condition(&mut self) -> NodeId {
        let open = self.start();
        self.expect(T::LParen);
        let e = self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_expression());
        self.expect_closing(T::RParen, T::LParen, open);
        e
    }

    fn parse_if(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        let test = self.parse_paren_condition();
        let cons = self.parse_embedded_statement();
        if matches!(self.ast.node(cons), Node::Empty {}) && self.with_depth == 0 {
            let span = self.ast.span(cons);
            if span.end > span.start {
                self.deferred_grammar_error_at(messages::EMPTY_IF_BODY, span, &[]);
            }
        }
        let alt = if self.eat(T::Else) {
            Some(self.parse_embedded_statement())
        } else {
            None
        };
        self.finish_node(start, Node::If { test, cons, alt })
    }

    fn parse_do_while(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        let body = self.with_jump(Jump::Loop, |p| p.parse_embedded_statement());
        self.expect(T::While);
        let test = self.parse_paren_condition();
        // `do ... while (x)` may be followed by anything: a `;` is optional.
        self.eat(T::Semi);
        self.finish_node(start, Node::DoWhile { body, test })
    }

    fn parse_for(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        let await_span = self.tok().span();
        let is_await = self.at(T::Await) && {
            self.bump();
            true
        };
        let in_static_block = self.ctx.has(Ctx::STATIC_BLOCK);
        if is_await
            && (in_static_block || self.ctx.has(Ctx::IN_FUNCTION) && !self.ctx.has(Ctx::AWAIT))
        {
            let m = if in_static_block {
                messages::FOR_AWAIT_IN_STATIC_BLOCK
            } else {
                messages::FOR_AWAIT_NOT_ASYNC
            };
            self.deferred_grammar_error_at(m, await_span, &[]);
        }
        let open = self.start();
        self.expect(T::LParen);
        let init_start = self.start();
        let mut init_is_decl = false;
        let init = self.with_ctx(Ctx::DISALLOW_IN, 0, |p| {
            let kind = match p.kind() {
                T::Semi => return None,
                T::Var => Some(VarKind::Var),
                T::Const => Some(VarKind::Const),
                // `for (let in x)` is an empty `let` list in tsc 7 (TS1123, then the `for-in`;
                // panicForInEmptyDeclarationList), not the name `let`.
                T::Let if p.is_let_declaration() || p.peek_kind() == T::In => Some(VarKind::Let),
                T::Using if p.is_using_declaration_in_for() => Some(VarKind::Using),
                T::Await if p.is_await_using_declaration() => Some(VarKind::AwaitUsing),
                _ => None,
            };
            match kind {
                Some(kind) => {
                    init_is_decl = true;
                    p.bump();
                    if kind == VarKind::AwaitUsing {
                        p.bump();
                    }
                    let decls = p.parse_var_declarators(kind);
                    Some(p.finish_node(
                        init_start,
                        Node::VarDecl {
                            modifiers: Modifiers::NONE,
                            kind,
                            decls,
                        },
                    ))
                }
                None => Some(p.parse_expression()),
            }
        });
        if let Some(left) = init {
            if self.at_contextual(T::Of) {
                // `for (async of x)` (also with an escaped `async`, not `for await` or
                // `(async)`): TS1106. A grammar error in tsc 7, not reported where `await`
                // is an operator (an async function: observed with tsc 7.0.2).
                if !is_await
                    && !self.ctx.has(Ctx::AWAIT)
                    && let Node::Ident { name } = *self.ast.node(left)
                    && self.ast.atom(name) == "async"
                {
                    let span = self.ast.span(left);
                    self.grammar_error_at(messages::FOR_OF_ASYNC, span, &[]);
                }
                self.check_for_in_of_head(left, true);
                self.check_assignment_pattern(left, 0);
                self.bump();
                let right = self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_assignment());
                self.expect_closing(T::RParen, T::LParen, open);
                let body = self.with_jump(Jump::Loop, |p| p.parse_embedded_statement());
                return self.finish_node(
                    start,
                    Node::ForOf {
                        is_await,
                        left,
                        right,
                        body,
                    },
                );
            }
            if self.at(T::In) {
                // `for await (x in y)`: `'of' expected` at the `in` (parser.forAwait.es2018).
                if is_await {
                    self.error_expected("of");
                }
                self.check_for_in_of_head(left, false);
                self.check_assignment_pattern(left, 0);
                self.bump();
                let right = self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_expression());
                self.expect_closing(T::RParen, T::LParen, open);
                let body = self.with_jump(Jump::Loop, |p| p.parse_embedded_statement());
                return self.finish_node(start, Node::ForIn { left, right, body });
            }
        }
        let _ = init_is_decl;
        if let Some(init) = init
            && let Node::VarDecl { kind, decls, .. } = *self.ast.node(init)
        {
            self.check_var_initializers(kind, decls);
        }
        self.expect(T::Semi);
        let test = if self.at(T::Semi) {
            None
        } else {
            Some(self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_expression()))
        };
        self.expect(T::Semi);
        let update = if self.at(T::RParen) {
            None
        } else {
            Some(self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_expression()))
        };
        self.expect_closing(T::RParen, T::LParen, open);
        let body = self.with_jump(Jump::Loop, |p| p.parse_embedded_statement());
        self.finish_node(
            start,
            Node::For {
                init,
                test,
                update,
                body,
            },
        )
    }

    /// `try` statement. A stray `catch` or `finally` starts one too, with `'try' expected`
    /// and an empty block, as in tsc.
    fn parse_try(&mut self) -> NodeId {
        let start = self.start();
        let block = if self.expect(T::Try) {
            self.parse_block()
        } else {
            self.add_node(
                Span::at(self.prev_end),
                Node::Block {
                    stmts: NodeList::EMPTY,
                },
            )
        };
        let mut handler = None;
        if self.at(T::Catch) {
            let cstart = self.start();
            self.bump();
            let (mut param, mut type_ann) = (None, None);
            if self.at(T::LParen) {
                let open = self.start();
                self.bump();
                param = Some(self.parse_binding_target());
                if self.ts && self.eat(T::Colon) {
                    type_ann = Some(self.parse_type());
                }
                if self.eat(T::Eq) {
                    // A checker grammar error, at the initializer (catchClauseWithInitializer1).
                    let init = self.parse_assignment();
                    let span = self.ast.span(init);
                    self.deferred_grammar_error_at(
                        messages::CATCH_CLAUSE_VARIABLE_INITIALIZER,
                        span,
                        &[],
                    );
                }
                self.expect_closing(T::RParen, T::LParen, open);
            }
            let body = self.parse_block();
            handler = Some(self.finish_node(
                cstart,
                Node::CatchClause {
                    param,
                    type_ann,
                    body,
                },
            ));
        }
        let finalizer = if self.eat(T::Finally) {
            Some(self.parse_block())
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            self.error(messages::CATCH_OR_FINALLY_EXPECTED);
        }
        self.finish_node(
            start,
            Node::Try {
                block,
                handler,
                finalizer,
            },
        )
    }

    fn parse_switch(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        let disc = self.parse_paren_condition();
        let open = self.start();
        self.expect(T::LBrace);
        let mark = self.scratch.len();
        let mut seen_default = 0u32;
        {
            self.with_jump(Jump::Switch, |p| {
                p.in_list(List::SwitchClauses, |p| {
                    loop {
                        if p.aborting() || p.is_list_terminator(List::SwitchClauses) {
                            break;
                        }
                        let cstart = p.start();
                        let test = match p.kind() {
                            T::Case => {
                                p.bump();
                                Some(p.parse_expression())
                            }
                            T::Default => {
                                // A checker grammar error, for the first extra `default` only
                                // (switchStatementsWithMultipleDefaults1).
                                if seen_default == 1 {
                                    let span = p.tok().span();
                                    p.deferred_grammar_error_at(
                                        messages::ONLY_ONE_DEFAULT_CLAUSE,
                                        span,
                                        &[],
                                    );
                                }
                                seen_default = seen_default.saturating_add(1);
                                p.bump();
                                None
                            }
                            _ => {
                                if p.abort_list_or_skip(List::SwitchClauses) {
                                    break;
                                }
                                continue;
                            }
                        };
                        p.expect(T::Colon);
                        let body = p.parse_statement_list(List::SwitchClauseStatements, false);
                        p.check_using_in_case(body);
                        let c = p.finish_node(cstart, Node::SwitchCase { test, body });
                        p.scratch.push(c);
                    }
                })
            });
        }
        let cases = self.finish_list(mark);
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(start, Node::Switch { disc, cases })
    }

    // ---- Lookahead predicates ----

    /// `let` starts a declaration if followed by an identifier, `[` or `{`.
    fn is_let_declaration(&mut self) -> bool {
        let next = self.peek();
        match next.kind {
            T::LBracket | T::LBrace => true,
            k if k.is_identifier_like() => {
                // `let in` / `let instanceof` are expressions in sloppy code.
                !matches!(k, T::In | T::InstanceOf)
            }
            _ => false,
        }
    }

    /// `using x` (identifier on the same line, not `in`/`instanceof`), or `using {`, a
    /// declaration with a pattern (TS1492, usingDeclarations.6).
    pub(crate) fn is_using_declaration(&mut self) -> bool {
        let next = self.peek();
        !next.newline_before()
            && (next.kind == T::LBrace
                || next.kind.is_identifier_like() && !matches!(next.kind, T::In | T::InstanceOf))
    }

    /// In a `for` head, `using of` is ambiguous; treat `using of` as an identifier `using`.
    fn is_using_declaration_in_for(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if p.at(T::LBrace) && !p.newline_before() {
                return true;
            }
            if p.newline_before()
                || !p.kind().is_identifier_like()
                || matches!(p.kind(), T::In | T::InstanceOf)
            {
                return false;
            }
            if p.at(T::Of) {
                p.bump();
                // `for (using of = x;;)` and `for (using of: T ...)` declare `of`;
                // `for (using of x)` and `for (using of of x)` iterate `using` (tsc 7).
                return matches!(p.kind(), T::Colon | T::Eq | T::Semi);
            }
            true
        })
    }

    /// `await using x`.
    pub(crate) fn is_await_using_declaration(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if !p.at(T::Using) || p.newline_before() {
                return false;
            }
            p.bump();
            !p.newline_before()
                && (p.at(T::LBrace)
                    || p.kind().is_identifier_like() && !matches!(p.kind(), T::In | T::InstanceOf))
        })
    }

    pub(crate) fn next_is_function_on_same_line(&mut self) -> bool {
        let next = self.peek();
        next.kind == T::Function && !next.newline_before()
    }

    /// For `interface`, `type`, `namespace`, `module`, `declare`, `abstract`, `global` at the
    /// start of a statement: does a declaration follow?
    fn is_contextual_declaration(&mut self) -> bool {
        let kind = self.kind();
        self.lookahead(|p| {
            p.bump();
            let next = p.tok();
            match kind {
                T::Interface | T::Type => !next.newline_before() && next.kind.is_identifier_like(),
                T::Namespace | T::Module => {
                    !next.newline_before()
                        && (next.kind.is_identifier_like() || next.kind == T::String)
                }
                // `global x` is a global augmentation missing its body: `';' expected` at
                // the `x` (nestedGlobalNamespaceInClass).
                T::Global => {
                    next.kind == T::LBrace
                        || (!next.newline_before() && next.kind.is_identifier_like())
                }
                T::Abstract => {
                    !next.newline_before() && matches!(next.kind, T::Class | T::Abstract)
                }
                T::Declare => {
                    if next.newline_before() {
                        return false;
                    }
                    match next.kind {
                        T::Var
                        | T::Let
                        | T::Const
                        | T::Function
                        | T::Class
                        | T::Enum
                        | T::Export
                        | T::Import => true,
                        T::Interface
                        | T::Type
                        | T::Namespace
                        | T::Module
                        | T::Global
                        | T::Abstract
                        | T::Declare
                        | T::Async
                        | T::Using => p.is_contextual_declaration_after_modifier(),
                        T::Await => true,
                        _ => false,
                    }
                }
                _ => false,
            }
        })
    }

    /// Helper for `declare <kw>`: the current token is a contextual keyword after `declare`.
    fn is_contextual_declaration_after_modifier(&mut self) -> bool {
        // Bounded: `declare declare declare ...` would otherwise rescan the whole chain from
        // every statement (quadratic time, unbounded recursion).
        if self.modifier_lookahead >= MAX_MODIFIER_CHAIN {
            return false;
        }
        self.modifier_lookahead += 1;
        let r = match self.kind() {
            T::Global => true,
            T::Async => self.next_is_function_on_same_line(),
            T::Using => self.is_using_declaration(),
            _ => self.is_contextual_declaration(),
        };
        self.modifier_lookahead -= 1;
        r
    }

    // ---- Declarations ----

    /// Parses statement modifiers (`declare`, `abstract`, `async`, `export`, and misplaced ones
    /// such as `public` or `static`) and the declaration they apply to. `decorators` and
    /// `modifiers` (`export`) were parsed by the caller.
    fn parse_declaration_with_modifiers(
        &mut self,
        start: u32,
        decorators: NodeList,
        modifiers: Modifiers,
    ) -> NodeId {
        let modifiers = modifiers | self.parse_modifiers(ModCtx::Statement);
        let ambient = modifiers.contains(Modifiers::DECLARE);
        let add = if ambient { Ctx::AMBIENT } else { 0 };
        // tsc does not reparse an ambient statement for top-level `await`: there `await` is
        // still a name (`declare class C extends await {}`, topLevelAwait.3).
        let remove = if ambient && self.module_await && !self.ctx.has(Ctx::IN_FUNCTION) {
            Ctx::AWAIT
        } else {
            0
        };
        self.with_ctx(add, remove, |p| {
            p.parse_declaration_after_modifiers(start, decorators, modifiers)
        })
    }

    fn parse_declaration_after_modifiers(
        &mut self,
        start: u32,
        decorators: NodeList,
        modifiers: Modifiers,
    ) -> NodeId {
        match self.kind() {
            T::Var => {
                self.bump();
                self.parse_var_statement(start, modifiers, VarKind::Var)
            }
            T::Let => {
                self.bump();
                self.parse_var_statement(start, modifiers, VarKind::Let)
            }
            T::Const => {
                if self.peek_kind() == T::Enum {
                    let m = self.bump_modifier(Modifiers::CONST);
                    return self.parse_enum(start, modifiers | m);
                }
                self.bump();
                self.parse_var_statement(start, modifiers, VarKind::Const)
            }
            // After decorators, `using` always starts a declaration (`@dec using 1` is
            // TS1134 at the `1`: decoratorOnUsing).
            T::Using if !decorators.is_empty() || self.is_using_declaration() => {
                self.bump();
                self.parse_var_statement(start, modifiers, VarKind::Using)
            }
            T::Await if self.is_await_using_declaration() => {
                self.bump();
                self.bump();
                self.parse_var_statement(start, modifiers, VarKind::AwaitUsing)
            }
            T::Function => self.parse_function_declaration(start, modifiers),
            T::Class => self.parse_class(start, decorators, modifiers, false),
            T::Enum => self.parse_enum(start, modifiers),
            T::Interface => self.parse_interface(start, modifiers),
            T::Type => self.parse_type_alias(start, modifiers),
            T::Namespace => {
                self.bump();
                self.parse_module_rest(start, modifiers, ModuleKind::Namespace)
            }
            T::Module => {
                self.bump();
                self.parse_module_rest(start, modifiers, ModuleKind::Module)
            }
            T::Global => {
                let name_start = self.start();
                let name = self.intern_token();
                self.bump();
                let name = self.finish_node(name_start, Node::Ident { name });
                let body = if self.at(T::LBrace) {
                    Some(self.parse_module_block(Container::ExternalModule))
                } else {
                    self.semicolon();
                    None
                };
                self.finish_node(
                    start,
                    Node::Module {
                        modifiers,
                        kind: ModuleKind::Global,
                        name,
                        body,
                    },
                )
            }
            T::Import => self.parse_import(start, modifiers),
            // `declare export { a }`, `async export * from "x"`: export forms that take no
            // modifiers (the checker reports the ones written).
            T::Export => {
                let node = self.parse_export(start, decorators);
                // `export export = x`, `export declare export = x` (TS1120).
                if !modifiers.is_empty()
                    && matches!(self.ast.node(node), Node::ExportAssignment { .. })
                {
                    let span = Span::new(start, start + 1);
                    self.deferred_grammar_error_at(
                        messages::EXPORT_ASSIGNMENT_MODIFIERS,
                        span,
                        &[],
                    );
                }
                node
            }
            _ => {
                // tsc reports this at the end of the modifiers, not at the next token.
                self.error_at(messages::DECLARATION_EXPECTED, Span::at(self.prev_end), &[]);
                let expr = self.missing();
                self.finish_node(start, Node::ExprStmt { expr })
            }
        }
    }

    fn parse_decorated_statement(&mut self, start: u32) -> NodeId {
        let decorators = self.parse_decorators();
        match self.kind() {
            T::Export => self.parse_export(start, decorators),
            T::Class => self.parse_class(start, decorators, Modifiers::NONE, false),
            T::Using => self.parse_declaration_with_modifiers(start, decorators, Modifiers::NONE),
            // `@dec default class {}`: TS1029 at `default`, a checker grammar error
            // (defaultKeywordWithoutExport1).
            T::Default if self.peek_kind() == T::Class => {
                let span = self.tok().span();
                self.deferred_grammar_error_at(
                    messages::MODIFIER_MUST_PRECEDE,
                    span,
                    &["export", "default"],
                );
                self.bump();
                self.parse_class(start, decorators, Modifiers::DEFAULT, false)
            }
            _ if self.declaration_follows_modifiers() => {
                self.parse_declaration_with_modifiers(start, decorators, Modifiers::NONE)
            }
            _ => {
                self.error_at(messages::DECLARATION_EXPECTED, Span::at(self.prev_end), &[]);
                let expr = self.missing();
                self.finish_node(start, Node::ExprStmt { expr })
            }
        }
    }

    fn parse_var_statement(&mut self, start: u32, modifiers: Modifiers, kind: VarKind) -> NodeId {
        let decls = self.parse_var_declarators(kind);
        self.check_var_initializers(kind, decls);
        if matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
            self.check_using_statement(start, modifiers, kind);
        }
        self.semicolon();
        self.finish_node(
            start,
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            },
        )
    }

    /// Declarators after `var`/`let`/`const`/`using`, as tsc's delimited list: a reserved
    /// word is TS1389 (`var export;`), a missing `,` is `',' expected`. An empty list is
    /// TS1123 after the keyword and a trailing `,` TS1009, both only in a file without
    /// syntax errors. In a `for` head, `of` before a name and `)` is not a declarator
    /// (`for (var of X)` is an empty list).
    pub(crate) fn parse_var_declarators(&mut self, kind: VarKind) -> NodeList {
        let after_keyword = self.prev_end;
        let of_in_for_head = self.ctx.has(Ctx::DISALLOW_IN)
            && self.at(T::Of)
            && self.lookahead(|p| {
                p.bump();
                let name = p.kind().is_identifier_like();
                p.bump();
                name && p.at(T::RParen)
            });
        if of_in_for_head {
            self.deferred_grammar_error_at(
                messages::VARIABLE_DECLARATION_LIST_CANNOT_BE_EMPTY,
                Span::at(after_keyword),
                &[],
            );
            return NodeList::EMPTY;
        }
        let (decls, trailing) =
            self.parse_delimited(List::VariableDeclarations, |p| p.parse_var_declarator());
        if decls.is_empty() {
            self.deferred_grammar_error_at(
                messages::VARIABLE_DECLARATION_LIST_CANNOT_BE_EMPTY,
                Span::at(after_keyword),
                &[],
            );
        } else if let Some(comma) = trailing {
            self.deferred_grammar_error_at(
                messages::TRAILING_COMMA_NOT_ALLOWED,
                Span::new(comma, comma + 1),
                &[],
            );
        }
        if matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
            // `using {a} = x`: TS1492 at each pattern (usingDeclarations.5/6).
            let keyword = if kind == VarKind::Using {
                "using"
            } else {
                "await using"
            };
            for i in 0..self.ast.list(decls).len() {
                let d = self.ast.list(decls)[i];
                let Node::VarDeclarator { name, .. } = *self.ast.node(d) else {
                    continue;
                };
                if matches!(
                    self.ast.node(name),
                    Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                ) {
                    let span = self.ast.span(name);
                    self.deferred_grammar_error_at(
                        messages::USING_BINDING_PATTERN,
                        Span::at(span.start),
                        &[keyword],
                    );
                }
            }
        }
        decls
    }

    /// One declarator: a name or pattern, `!`, a type and an initialiser.
    fn parse_var_declarator(&mut self) -> NodeId {
        let dstart = self.start();
        let name = if self.at(T::PrivateName) {
            let span = self.tok().span();
            self.grammar_error_at(
                messages::PRIVATE_IDENTIFIER_IN_VARIABLE_DECLARATION,
                span,
                &[],
            );
            self.parse_private_name()
        } else {
            self.check_await_binding_in_static_block();
            self.parse_binding_target()
        };
        let bang = self.tok().span();
        let definite = self.ts && !self.newline_before() && self.at(T::Bang) && {
            self.bump();
            true
        };
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
        if definite {
            let ambient = self.ctx.has(Ctx::AMBIENT);
            self.check_definite(bang, ambient, type_ann.is_some(), init.is_some());
        }
        self.finish_node(
            dstart,
            Node::VarDeclarator {
                name,
                definite,
                type_ann,
                init,
            },
        )
    }

    fn parse_function_declaration(&mut self, start: u32, modifiers: Modifiers) -> NodeId {
        self.parse_function(start, modifiers, false)
    }

    fn parse_enum(&mut self, start: u32, modifiers: Modifiers) -> NodeId {
        self.expect(T::Enum);
        // `enum void {}`: as in tsc, the name is missing and the reserved word is left for
        // the next statement (`void {}`), and so is anything else that is not `{`.
        let name = if self.kind().is_reserved() {
            self.reserved_word_error();
            self.missing()
        } else {
            self.parse_identifier()
        };
        let open = self.start();
        if !self.expect(T::LBrace) {
            return self.finish_node(
                start,
                Node::Enum {
                    modifiers,
                    name,
                    members: NodeList::EMPTY,
                },
            );
        }
        let mark = self.scratch.len();
        self.in_list(List::EnumMembers, |p| p.parse_enum_members());
        let members = self.finish_list(mark);
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(
            start,
            Node::Enum {
                modifiers,
                name,
                members,
            },
        )
    }

    /// The members of an enum body up to `}`. As in tsc, a token that is no member is
    /// `Enum member expected` and is skipped unless an enclosing list uses it (`@dec` starts a
    /// statement: decoratorOnEnum2), and a member followed by anything but `,` or `}` is
    /// TS1357, after which the list goes on (parserEnum5: `a: 1` is two members).
    fn parse_enum_members(&mut self) {
        while !matches!(self.kind(), T::RBrace | T::Eof) && !self.aborting() {
            let mstart = self.start();
            let mname = match self.kind() {
                T::String => self.parse_string_literal(),
                T::Number => self.parse_numeric_literal(),
                T::BigInt => self.parse_bigint_literal(),
                T::LBracket => {
                    let key = self.parse_computed_key();
                    self.check_enum_computed_name(key);
                    key
                }
                T::PrivateName => self.parse_private_name(),
                k if k.is_identifier_or_keyword() => self.parse_identifier_name(),
                T::Comma => {
                    self.error(messages::ENUM_MEMBER_EXPECTED);
                    self.missing()
                }
                _ => {
                    if self.abort_list_or_skip(List::EnumMembers) {
                        break;
                    }
                    continue;
                }
            };
            self.check_enum_member_name(mname);
            let init = if self.eat(T::Eq) {
                Some(self.parse_assignment())
            } else {
                None
            };
            let m = self.finish_node(mstart, Node::EnumMember { name: mname, init });
            self.scratch.push(m);
            if !self.eat(T::Comma) && !matches!(self.kind(), T::RBrace | T::Eof) {
                self.error(messages::ENUM_MEMBER_SEPARATOR_EXPECTED);
            }
        }
    }

    fn parse_interface(&mut self, start: u32, modifiers: Modifiers) -> NodeId {
        self.expect(T::Interface);
        let name = self.parse_identifier();
        let type_params = self.parse_type_params_opt();
        let mut extends = NodeList::EMPTY;
        if self.eat(T::Extends) {
            extends = self.parse_heritage_types("extends");
        }
        while self.at(T::Extends) {
            // `interface I extends A extends B {}`: the checker's TS1172 at the second
            // `extends`; that clause is parsed and dropped (parserInterfaceDeclaration1).
            let span = self.tok().span();
            self.deferred_grammar_error_at(messages::EXTENDS_CLAUSE_ALREADY_SEEN, span, &[]);
            self.bump();
            self.parse_heritage_types("extends");
        }
        if self.at(T::Implements) {
            // TS1176 at `implements`; the clause is parsed and dropped
            // (interfaceWithImplements1, parserInterfaceDeclaration2).
            self.error(messages::INTERFACE_IMPLEMENTS_CLAUSE);
            self.bump();
            self.parse_heritage_types("implements");
        }
        let body = self.parse_object_type_members(true);
        self.finish_node(
            start,
            Node::Interface {
                modifiers,
                name,
                type_params,
                extends,
                body,
            },
        )
    }

    /// `A, B<C>, D.E` after `extends`/`implements`.
    pub(crate) fn parse_heritage_types(&mut self, keyword: &str) -> NodeList {
        // `class C extends {` starts the body unless it is an object literal such as
        // `extends {} {`; an empty list is reported.
        // Likewise `class C extends implements I {` (parserErrorRecovery_ExtendsOrImplements-
        // Clause3).
        if (self.at(T::LBrace) && !self.is_heritage_object_literal())
            || (self.at(T::Implements) && keyword == "extends")
        {
            let span = Span::at(self.prev_end);
            self.deferred_grammar_error_at(messages::LIST_CANNOT_BE_EMPTY, span, &[keyword]);
            return NodeList::EMPTY;
        }
        let mark = self.scratch.len();
        loop {
            let before = self.tok().start;
            let s = self.start();
            let expr = self.parse_heritage_expression();
            let (expr, type_args) =
                if let Some(unwrapped) = self.unwrap_heritage_instantiation(expr) {
                    unwrapped
                } else if self.ts && self.at(T::Lt) {
                    (expr, Some(self.parse_type_args()))
                } else {
                    (expr, None)
                };
            let n = self.finish_node(s, Node::ExprWithTypeArgs { expr, type_args });
            self.scratch.push(n);
            if self.aborting() {
                break;
            }
            if self.tok().start == before {
                // `extends await<T>` where `await` is an operator (a module's top level):
                // as in tsc the element is missing (TS1109, reported), `await` is skipped
                // and the list goes on at the next token that can start a left-hand side
                // (topLevelAwaitErrors.1).
                if self.at(T::Await) && self.ctx.has(Ctx::AWAIT) {
                    self.bump();
                    if self.heritage_skip_to_element(true) {
                        continue;
                    }
                }
                break;
            }
            let comma = self.tok().span();
            if self.eat(T::Comma) {
                // `extends C, {`: the `{` starts the body; TS1009 at the `,`, a checker
                // grammar error (classHeritageWithTrailingSeparator). Likewise before
                // another clause (parserErrorRecovery_ExtendsOrImplementsClause5).
                if (self.at(T::LBrace) && !self.is_heritage_object_literal())
                    || matches!(self.kind(), T::Extends | T::Implements)
                {
                    self.deferred_grammar_error_at(
                        messages::TRAILING_COMMA_NOT_ALLOWED,
                        comma,
                        &[],
                    );
                    break;
                }
                continue;
            }
            if !self.heritage_list_recover() {
                break;
            }
        }
        self.finish_list(mark)
    }

    /// After a heritage type that is not followed by `,`. As in tsc, a token other than a
    /// terminator (`{`, `extends`, `implements`) is `',' expected` and the list goes on:
    /// tokens that cannot start a type are skipped (TS1109, usually at the same position),
    /// and `}`, `)`, `]` or the end of file end it. Returns true to parse another element.
    fn heritage_list_recover(&mut self) -> bool {
        if matches!(self.kind(), T::LBrace | T::Extends | T::Implements | T::Eof) {
            return false;
        }
        self.error_expected(",");
        self.heritage_skip_to_element(false)
    }

    /// Skips tokens that cannot start a heritage element (TS1109 at each) up to one that
    /// can (returns true) or a terminator (false). With `lhs_only`, also `<` and the unary
    /// operators, which cannot start a left-hand-side expression.
    fn heritage_skip_to_element(&mut self, lhs_only: bool) -> bool {
        loop {
            let unary = matches!(
                self.kind(),
                T::Lt
                    | T::Plus
                    | T::Minus
                    | T::Tilde
                    | T::Bang
                    | T::Delete
                    | T::TypeOf
                    | T::Void
                    | T::PlusPlus
                    | T::MinusMinus
            );
            match self.kind() {
                T::LBrace
                | T::Extends
                | T::Implements
                | T::RBrace
                | T::RParen
                | T::RBracket
                | T::Eof => return false,
                _ if self.at_expression_start() && !(lhs_only && unary) => return true,
                _ if self.aborting() => return false,
                _ => {
                    self.error(messages::EXPRESSION_EXPECTED);
                    self.bump();
                }
            }
        }
    }

    /// `extends A<T>` with a line break after `>`: the member tail takes `A<T>` as an
    /// instantiation expression (a newline may follow type arguments), but as in tsc the
    /// heritage type gets the type arguments: `ExprWithTypeArgs { expr: A, type_args: <T> }`.
    /// The `Instantiation` node is the last one allocated, so it is dropped, not orphaned.
    /// Another `<...>` after it is not parsed (tsc: `',' expected`).
    fn unwrap_heritage_instantiation(&mut self, id: NodeId) -> Option<(NodeId, Option<NodeId>)> {
        let Node::Instantiation { expr, type_args } = *self.ast.node(id) else {
            return None;
        };
        if id.index() + 1 == self.ast.nodes.len() {
            self.ast.nodes.pop();
            self.ast.spans.pop();
        }
        Some((expr, Some(type_args)))
    }

    /// At `{` in a heritage list: `{}` followed by `,`, `{`, `extends` or `implements`.
    fn is_heritage_object_literal(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            // `extends { foo: string; } {}` is an object literal (classExtendsEveryObjectType):
            // only an empty `{}` that is not followed by the body is the body itself.
            if !p.eat(T::RBrace) {
                return true;
            }
            matches!(p.kind(), T::Comma | T::LBrace | T::Extends | T::Implements)
        })
    }

    fn parse_type_alias(&mut self, start: u32, modifiers: Modifiers) -> NodeId {
        self.expect(T::Type);
        let name = self.parse_identifier();
        let type_params = self.parse_type_params_opt();
        self.expect(T::Eq);
        let ty = if self.at_contextual(T::Intrinsic) && {
            let n = self.peek();
            n.newline_before() || matches!(n.kind, T::Semi | T::Eof | T::RBrace)
        } {
            let s = self.start();
            self.bump();
            self.finish_node(
                s,
                Node::KeywordType {
                    kind: ts_r_ast::KeywordTypeKind::Intrinsic,
                },
            )
        } else {
            self.parse_type()
        };
        self.semicolon();
        self.finish_node(
            start,
            Node::TypeAlias {
                modifiers,
                name,
                type_params,
                ty,
            },
        )
    }

    /// After `namespace`/`module`: name and body.
    fn parse_module_rest(&mut self, start: u32, modifiers: Modifiers, kind: ModuleKind) -> NodeId {
        let name = if self.at(T::String) {
            self.parse_string_literal()
        } else {
            let first_start = self.start();
            let mut name = self.parse_identifier();
            while self.at(T::Dot) {
                self.bump();
                let right = self.parse_identifier_name();
                name = self.finish_node(first_start, Node::QualifiedName { left: name, right });
            }
            if kind == ModuleKind::Module {
                self.check_module_keyword(name);
            }
            name
        };
        let body = if self.at(T::LBrace) {
            let container = if matches!(self.ast.node(name), Node::StringLit { .. }) {
                Container::ExternalModule
            } else {
                Container::Namespace
            };
            Some(self.parse_module_block(container))
        } else {
            self.semicolon();
            None
        };
        self.finish_node(
            start,
            Node::Module {
                modifiers,
                kind,
                name,
                body,
            },
        )
    }

    fn parse_module_block(&mut self, container: Container) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.expect(T::LBrace);
        // Module bodies are not function bodies: `await` at their top level is not allowed,
        // and `return` is an error, but we only care about syntax here.
        let mark = self.deferred.len();
        let clear = Ctx::IN_FUNCTION | Ctx::AWAIT | Ctx::YIELD | Ctx::STATIC_BLOCK;
        let stmts = self.with_ctx(0, clear, |p| p.parse_statements(false, false));
        self.check_ambient_statements(stmts, mark);
        self.check_statement_list(stmts, container);
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(start, Node::ModuleBlock { stmts })
    }

    // ---- Import / export ----

    pub(crate) fn parse_import(&mut self, start: u32, modifiers: Modifiers) -> NodeId {
        self.expect(T::Import);
        if self.at(T::String) {
            let source = self.parse_string_literal();
            let attributes = self.parse_import_attributes_opt();
            self.semicolon();
            return self.finish_node(
                start,
                Node::ImportDecl {
                    type_only: false,
                    phase: ImportPhase::Evaluation,
                    has_clause: false,
                    specifiers: NodeList::EMPTY,
                    source,
                    attributes,
                },
            );
        }
        let mut type_only = false;
        if self.ts && self.at_contextual(T::Type) {
            let is_modifier = self.lookahead(|p| {
                p.bump();
                match p.kind() {
                    T::LBrace | T::Star => true,
                    T::From => {
                        // `import type from "x"` imports a default named `type`.
                        p.bump();
                        !matches!(p.kind(), T::String)
                    }
                    T::Eq => false,
                    T::Comma => false,
                    k => k.is_identifier_like(),
                }
            });
            if is_modifier {
                type_only = true;
                self.bump();
            }
        }
        let mut phase = ImportPhase::Evaluation;
        let clause_start = self.start();
        // A JSDoc `@import defer * as ns` has no phase: `defer` is a default import, and
        // `'from' expected` follows at the `*` (importDeferJsdoc).
        if !type_only
            && !self.jsdoc_import
            && (self.at_contextual(T::Defer) || self.at_contextual(T::Source))
        {
            let is_phase = self.lookahead(|p| {
                let k = p.kind();
                p.bump();
                // `import defer * as ns`, and (invalid, TS18058/TS18059) `import defer x`,
                // `import defer { a }`, `import defer from from "m"`.
                if k == T::Defer && matches!(p.kind(), T::Star | T::LBrace) {
                    true
                } else if p.at(T::From) {
                    p.bump();
                    p.at(T::From)
                } else {
                    p.kind().is_identifier_like()
                }
            });
            if is_phase {
                phase = if self.at(T::Defer) {
                    ImportPhase::Defer
                } else {
                    ImportPhase::Source
                };
                self.bump();
            }
        }
        let mark = self.scratch.len();
        // `import class C {}`, `import\nimport ...`: no clause at all, so no `from` either;
        // the module specifier is read as an expression (tsc 7).
        let no_clause = !type_only
            && phase == ImportPhase::Evaluation
            && !self.kind().is_identifier_like()
            && !matches!(self.kind(), T::LBrace | T::Star);
        // Bindings after the default one need a `,` (`import a * as b` is `'from' expected`).
        let mut more = true;
        // Default binding or `import x = ...`.
        if self.at_identifier()
            || (self.kind().is_identifier_like() && !self.at(T::From))
            || (self.at(T::From) && self.peek_kind() != T::String)
        {
            let id_start = self.start();
            let name = self.parse_identifier();
            if self.at(T::Eq) {
                self.scratch.truncate(mark);
                return self.parse_import_equals(start, modifiers, type_only, name);
            }
            // `import abstract class D {}`: a name followed by neither `,` nor `from` is an
            // `import x =` missing its `=`; a reference is read only if a name follows (tsc 7).
            if phase == ImportPhase::Evaluation
                && !self.jsdoc_import
                && !matches!(self.kind(), T::Comma | T::From)
            {
                self.scratch.truncate(mark);
                self.error_expected("=");
                let module_ref = if self.kind().is_identifier_like() {
                    self.parse_entity_name(false)
                } else {
                    self.missing()
                };
                self.semicolon();
                return self.finish_node(
                    start,
                    Node::ImportEquals {
                        modifiers,
                        type_only,
                        name,
                        module_ref,
                    },
                );
            }
            let spec = self.finish_node(id_start, Node::ImportDefaultSpecifier { local: name });
            self.scratch.push(spec);
            more = self.eat(T::Comma);
            if more && !matches!(self.kind(), T::Star | T::LBrace) {
                self.error_expected("{");
            }
        }
        if !more {
        } else if self.at(T::Star) {
            let s = self.start();
            self.bump();
            self.expect(T::As);
            let local = self.parse_identifier();
            let spec = self.finish_node(s, Node::ImportNamespaceSpecifier { local });
            self.scratch.push(spec);
        } else if self.at(T::LBrace) {
            self.parse_named_specifiers(true);
        }
        let specifiers = self.finish_list(mark);
        if phase == ImportPhase::Defer {
            // Only `import defer * as ns` is valid: checker grammar errors on the clause.
            let list = self.ast.list(specifiers);
            let msg = if list
                .iter()
                .any(|&n| matches!(self.ast.node(n), Node::ImportDefaultSpecifier { .. }))
            {
                Some(messages::DEFERRED_DEFAULT_IMPORT)
            } else if list
                .iter()
                .any(|&n| matches!(self.ast.node(n), Node::ImportSpecifier { .. }))
            {
                Some(messages::DEFERRED_NAMED_IMPORTS)
            } else {
                None
            };
            if let Some(msg) = msg {
                let span = Span::new(clause_start, self.prev_end);
                self.deferred_grammar_error_at(msg, span, &[]);
            }
        }
        if !no_clause {
            self.expect(T::From);
        }
        let source = self.parse_module_specifier();
        let attributes = self.parse_import_attributes_opt();
        self.semicolon();
        self.finish_node(
            start,
            Node::ImportDecl {
                type_only,
                phase,
                has_clause: !no_clause,
                specifiers,
                source,
                attributes,
            },
        )
    }

    /// The module specifier after `from`. Anything but a string is parsed as an expression
    /// (dropped from the tree) and is TS1141 on its span, a checker error in tsc, so it is
    /// reported after the expression's own errors (`import { a }, from "m"` is `'from'
    /// expected` and TS1141 at `,`, then `';' expected` at the string).
    fn parse_module_specifier(&mut self) -> NodeId {
        if self.at(T::String) {
            return self.parse_string_literal();
        }
        let first = self.start();
        let expr = self.parse_expression();
        if !matches!(self.ast.node(expr), Node::Missing {}) {
            let span = match *self.ast.node(expr) {
                // A class expression's error span is its name.
                Node::ClassExpr {
                    name: Some(name), ..
                } => self.ast.span(name),
                _ => Span::new(first, self.prev_end.max(first)),
            };
            self.checker_error_at(messages::STRING_LITERAL_EXPECTED, span);
        }
        self.missing()
    }

    fn parse_import_equals(
        &mut self,
        start: u32,
        modifiers: Modifiers,
        type_only: bool,
        name: NodeId,
    ) -> NodeId {
        self.expect(T::Eq);
        let module_ref = if self.at_contextual(T::Require) && self.peek_kind() == T::LParen {
            let s = self.start();
            self.bump();
            let open = self.start();
            self.bump();
            let expr = if self.at(T::String) {
                self.parse_string_literal()
            } else {
                self.error(messages::STRING_LITERAL_EXPECTED);
                self.parse_assignment()
            };
            self.expect_closing(T::RParen, T::LParen, open);
            self.finish_node(s, Node::ExternalModuleRef { expr })
        } else if self.kind().is_identifier_or_keyword() && !self.kind().is_identifier_like() {
            // `import q = null;`: TS1359, and the word is left for the next statement
            // (aliasErrors; `import t = void;` is then TS1109 at `;`).
            self.reserved_word_error();
            self.missing()
        } else {
            self.parse_entity_name(true)
        };
        self.semicolon();
        self.finish_node(
            start,
            Node::ImportEquals {
                modifiers,
                type_only,
                name,
                module_ref,
            },
        )
    }

    /// `{ a, b as c, type d }` for imports (`is_import`) or exports. Pushes onto scratch.
    fn parse_named_specifiers(&mut self, is_import: bool) {
        let open = self.start();
        self.expect(T::LBrace);
        while !matches!(self.kind(), T::RBrace | T::Eof) && !self.aborting() {
            // `export { x, from "./a"`: `from` and a string end the list, so the `}` is
            // missing at `from` (unclosedExportClause01).
            if self.at(T::From) && self.peek_kind() == T::String {
                break;
            }
            let s = self.start();
            if !(self.kind().is_identifier_or_keyword() || self.at(T::String)) {
                self.error(messages::IDENTIFIER_EXPECTED);
                // A stray `,` is skipped; any other token ends the list, as in tsc
                // (`import { * } from "m"`, `export { 0n as foo }`).
                if !self.at(T::Comma) {
                    break;
                }
                self.bump();
                continue;
            }
            let mut type_only = false;
            if self.ts && self.at_contextual(T::Type) {
                // Disambiguate `type` as a modifier vs. a name.
                let (modifier, _) = self.lookahead(|p| {
                    p.bump();
                    match p.kind() {
                        T::Comma | T::RBrace => (false, 0),
                        T::As => {
                            p.bump();
                            match p.kind() {
                                // `{ type as }` → type-only `as`.
                                T::Comma | T::RBrace => (true, 0),
                                T::As => {
                                    p.bump();
                                    // `{ type as as }` → `type` as `as`; `{ type as as x }` → type-only.
                                    (!matches!(p.kind(), T::Comma | T::RBrace), 0)
                                }
                                // `{ type as x }` → `type` renamed to `x`.
                                _ => (false, 0),
                            }
                        }
                        k => (k.is_identifier_or_keyword() || k == T::String, 0),
                    }
                });
                if modifier {
                    type_only = true;
                    self.bump();
                }
            }
            let mut local_tok = self.tok();
            let first = self.parse_module_export_name();
            let (imported, local) = if self.at_contextual(T::As) {
                self.bump();
                local_tok = self.tok();
                let second = if is_import {
                    self.parse_identifier_name_as_binding()
                } else {
                    self.parse_module_export_name()
                };
                (Some(first), second)
            } else {
                (None, first)
            };
            // An imported binding that is a reserved word or a string (`import { default }`,
            // `import { a as if }`) is `Identifier expected.` at it, as in tsc.
            if is_import && (local_tok.kind.is_reserved() || local_tok.kind == T::String) {
                self.error_at(messages::IDENTIFIER_EXPECTED, local_tok.span(), &[]);
            }
            let spec = if is_import {
                self.finish_node(
                    s,
                    Node::ImportSpecifier {
                        type_only,
                        imported,
                        local,
                    },
                )
            } else {
                // For exports, `local` is the first name and `exported` the alias.
                match imported {
                    Some(l) => self.finish_node(
                        s,
                        Node::ExportSpecifier {
                            type_only,
                            local: l,
                            exported: Some(local),
                        },
                    ),
                    None => self.finish_node(
                        s,
                        Node::ExportSpecifier {
                            type_only,
                            local,
                            exported: None,
                        },
                    ),
                }
            };
            self.scratch.push(spec);
            if !self.eat(T::Comma) && !matches!(self.kind(), T::RBrace) {
                self.error_expected(",");
                if !self.kind().is_identifier_or_keyword() && !self.at(T::String) {
                    break;
                }
            }
        }
        self.expect_closing(T::RBrace, T::LBrace, open);
    }

    /// An identifier, keyword or string used as an import/export name.
    fn parse_module_export_name(&mut self) -> NodeId {
        if self.at(T::String) {
            self.parse_string_literal()
        } else {
            self.parse_identifier_name()
        }
    }

    /// Local binding after `as` in an import specifier.
    fn parse_identifier_name_as_binding(&mut self) -> NodeId {
        if self.kind().is_identifier_or_keyword() {
            self.parse_identifier_name()
        } else {
            self.parse_identifier()
        }
    }

    fn parse_import_attributes_opt(&mut self) -> Option<NodeId> {
        let assert = if self.at(T::With) {
            false
        } else if self.at_contextual(T::Assert) && !self.newline_before() {
            true
        } else {
            return None;
        };
        let start = self.start();
        self.bump();
        Some(self.parse_import_attributes_body(start, assert))
    }

    /// `{ key: "value", ... }` after `with`/`assert`.
    pub(crate) fn parse_import_attributes_body(&mut self, start: u32, assert: bool) -> NodeId {
        let open = self.start();
        self.expect(T::LBrace);
        // tsc's delimited list: a token that cannot start an attribute is TS1478 and ends
        // the list when an enclosing list can use it (`{ with: {1234, ...} }` ends there).
        let (entries, _) = self.parse_delimited(List::ImportAttributes, |p| {
            let s = p.start();
            let key = if p.at(T::String) {
                p.parse_string_literal()
            } else {
                p.parse_identifier_name()
            };
            p.expect(T::Colon);
            let value = p.parse_assignment();
            p.finish_node(s, Node::ImportAttribute { key, value })
        });
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(start, Node::ImportAttributes { assert, entries })
    }

    fn parse_export(&mut self, start: u32, decorators: NodeList) -> NodeId {
        // `export` is recorded as a modifier token, and dropped again below when the
        // statement turns out to be an export form rather than an exported declaration.
        let export_token = self.ast.modifier_tokens.len();
        self.bump_modifier(Modifiers::EXPORT);
        // `export @dec class` (`@a export @b class` keeps both groups)
        let decorators = if self.at(T::At) {
            let after = self.parse_decorators();
            self.join_decorators(decorators, after)
        } else {
            decorators
        };
        match self.kind() {
            T::Eq => {
                self.ast.modifier_tokens.remove(export_token);
                self.bump();
                let expr = self.parse_assignment();
                self.semicolon();
                self.finish_node(start, Node::ExportAssignment { expr })
            }
            T::As if self.peek_kind() == T::Namespace => {
                self.ast.modifier_tokens.remove(export_token);
                self.bump();
                self.bump();
                let name = self.parse_identifier();
                self.semicolon();
                self.finish_node(start, Node::NamespaceExport { name })
            }
            T::Default => {
                let default_token = self.ast.modifier_tokens.len();
                self.bump_modifier(Modifiers::DEFAULT);
                self.parse_export_default(start, decorators, [export_token, default_token])
            }
            T::Star => {
                self.ast.modifier_tokens.remove(export_token);
                self.parse_export_all(start, false)
            }
            T::Type if self.ts && matches!(self.peek_kind(), T::Star) => {
                self.ast.modifier_tokens.remove(export_token);
                self.bump();
                self.parse_export_all(start, true)
            }
            T::Type if self.ts && matches!(self.peek_kind(), T::LBrace) => {
                self.ast.modifier_tokens.remove(export_token);
                self.bump();
                self.parse_export_named(start, true)
            }
            T::LBrace => {
                self.ast.modifier_tokens.remove(export_token);
                self.parse_export_named(start, false)
            }
            // `export import { a } from "m"`: an import declaration with a misplaced modifier
            // (TS1191 from the checker), as in tsc.
            T::Import if self.is_import_declaration_after_export() => {
                self.parse_import(start, Modifiers::EXPORT)
            }
            T::Import => {
                self.bump();
                let mut type_only = false;
                if self.at_contextual(T::Type)
                    && self.peek().kind.is_identifier_like()
                    && self.peek_kind() != T::Eq
                {
                    type_only = true;
                    self.bump();
                }
                let name = self.parse_identifier();
                self.parse_import_equals(start, Modifiers::EXPORT, type_only, name)
            }
            T::At => {
                let decorators = self.parse_decorators();
                self.parse_declaration_with_modifiers(start, decorators, Modifiers::EXPORT)
            }
            _ => self.parse_declaration_with_modifiers(start, decorators, Modifiers::EXPORT),
        }
    }

    /// The decorators before `export` and those after it, in source order (`@a export @b
    /// class X {}` keeps both; tsc's checker reports TS8038 at `@b`).
    fn join_decorators(&mut self, before: NodeList, after: NodeList) -> NodeList {
        if before.is_empty() {
            return after;
        }
        if after.is_empty() {
            return before;
        }
        let mut all = self.ast.list(before).to_vec();
        all.extend_from_slice(self.ast.list(after));
        self.ast.add_list(&all)
    }

    /// At `import` after `export`: is this an import declaration (`{ a } from`, `* as ns`,
    /// `x from`, `x, { y } from`, `"m"`) rather than `export import x = ...`?
    fn is_import_declaration_after_export(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if p.at_contextual(T::Type) && matches!(p.peek_kind(), T::LBrace | T::Star) {
                p.bump();
            }
            match p.kind() {
                T::LBrace | T::Star | T::String => true,
                k if k.is_identifier_like() => matches!(p.peek_kind(), T::From | T::Comma),
                _ => false,
            }
        })
    }

    /// After `export default`; `tokens` are the indices of the two keywords in
    /// `Ast::modifier_tokens`, dropped if what follows is an expression.
    fn parse_export_default(
        &mut self,
        start: u32,
        decorators: NodeList,
        tokens: [usize; 2],
    ) -> NodeId {
        let m = Modifiers::EXPORT | Modifiers::DEFAULT;
        let decorators = if self.at(T::At) {
            let after = self.parse_decorators();
            self.join_decorators(decorators, after)
        } else {
            decorators
        };
        match self.kind() {
            T::Function => return self.parse_function(start, m, false),
            T::Async if self.next_is_function_on_same_line() => {
                self.bump_modifier(Modifiers::ASYNC);
                return self.parse_function(start, m | Modifiers::ASYNC, false);
            }
            T::Class => return self.parse_class(start, decorators, m, false),
            T::Abstract
                if {
                    let n = self.peek();
                    n.kind == T::Class && !n.newline_before()
                } =>
            {
                self.bump_modifier(Modifiers::ABSTRACT);
                return self.parse_class(start, decorators, m | Modifiers::ABSTRACT, false);
            }
            T::Interface
                if self.ts && {
                    let n = self.peek();
                    n.kind.is_identifier_like()
                } =>
            {
                return self.parse_interface(start, m);
            }
            _ => {}
        }
        // Higher index first, so the lower one stays valid.
        self.ast.modifier_tokens.remove(tokens[1]);
        self.ast.modifier_tokens.remove(tokens[0]);
        let expr = self.parse_assignment();
        self.semicolon();
        self.finish_node(start, Node::ExportDefaultExpr { expr })
    }

    fn parse_export_all(&mut self, start: u32, type_only: bool) -> NodeId {
        self.expect(T::Star);
        let exported = if self.at_contextual(T::As) {
            self.bump();
            Some(self.parse_module_export_name())
        } else {
            None
        };
        self.expect(T::From);
        let source = self.parse_module_specifier();
        let attributes = self.parse_import_attributes_opt();
        self.semicolon();
        self.finish_node(
            start,
            Node::ExportAll {
                type_only,
                exported,
                source,
                attributes,
            },
        )
    }

    fn parse_export_named(&mut self, start: u32, type_only: bool) -> NodeId {
        let mark = self.scratch.len();
        self.parse_named_specifiers(false);
        let specifiers = self.finish_list(mark);
        let (source, attributes) = if self.at(T::From) {
            self.bump();
            let source = self.parse_module_specifier();
            (Some(source), self.parse_import_attributes_opt())
        } else {
            (None, None)
        };
        self.semicolon();
        self.finish_node(
            start,
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                attributes,
            },
        )
    }

    /// `function` keyword through the body. `modifiers` may include `ASYNC`.
    pub(crate) fn parse_function(
        &mut self,
        start: u32,
        mut modifiers: Modifiers,
        is_expr: bool,
    ) -> NodeId {
        self.expect(T::Function);
        let mut flags = FnFlags::NONE;
        if modifiers.contains(Modifiers::ASYNC) {
            flags |= FnFlags::ASYNC;
            modifiers.remove(Modifiers::ASYNC);
        }
        let star = self.at(T::Star).then(|| self.tok().span());
        if self.eat(T::Star) {
            flags |= FnFlags::GENERATOR;
        }
        let name = if self.at_function_name() {
            // `async function await() {}` as an expression: the name is in the function's
            // own `await` context (TS1359, from tsc's checker).
            if is_expr
                && self.at(T::Await)
                && flags.contains(FnFlags::ASYNC)
                && self.at_identifier()
            {
                let span = self.tok().span();
                self.deferred_grammar_error_at(
                    messages::RESERVED_WORD_CANNOT_BE_USED_HERE,
                    span,
                    &["await"],
                );
            }
            self.check_await_binding_in_static_block();
            if is_expr && self.at(T::Yield) {
                Some(self.parse_identifier_unchecked())
            } else {
                Some(self.parse_identifier())
            }
        } else if !is_expr && !modifiers.contains(Modifiers::DEFAULT) {
            // `function throw() {}`: a declaration whose name is missing (declared as
            // `(Missing)`, as in tsc); the reserved word is left for the next statement.
            if self.kind().is_reserved() {
                self.reserved_word_error();
            } else {
                self.error(messages::IDENTIFIER_EXPECTED);
            }
            Some(self.missing())
        } else {
            None
        };
        let (type_params, params, return_type, body) =
            self.parse_function_rest(flags, BodyRule::Optional);
        if !is_expr {
            self.check_ambient_function(star, body);
        }
        let node = if is_expr {
            Node::FunctionExpr {
                modifiers,
                flags,
                name,
                type_params,
                params,
                return_type,
                body,
            }
        } else {
            if body.is_none() && !self.ts {
                // JS requires a body; already reported by parse_function_rest.
            }
            Node::FunctionDecl {
                modifiers,
                flags,
                name,
                type_params,
                params,
                return_type,
                body,
            }
        };
        self.finish_node(start, node)
    }

    /// A function name: any identifier-like token. `yield` and `await` are names too (their
    /// errors come from the binder and from `parse_identifier`; a function expression's name
    /// is outside an enclosing generator's or async function's context).
    fn at_function_name(&self) -> bool {
        self.kind().is_identifier_like()
    }

    /// Type parameters, parameters, return type and body (or `;` for overloads).
    /// Sets async/generator context for params and body.
    pub(crate) fn parse_function_rest(
        &mut self,
        flags: FnFlags,
        rule: BodyRule,
    ) -> (Option<NodeId>, NodeList, Option<NodeId>, Option<NodeId>) {
        let mut add = Ctx::IN_FUNCTION;
        let mut remove = Ctx::DISALLOW_IN
            | Ctx::IN_CONDITIONAL_TRUE
            | Ctx::DECORATOR
            | Ctx::TYPE
            | Ctx::STATIC_BLOCK;
        if flags.contains(FnFlags::ASYNC) {
            add |= Ctx::AWAIT
        } else {
            remove |= Ctx::AWAIT
        }
        if flags.contains(FnFlags::GENERATOR) {
            add |= Ctx::YIELD
        } else {
            remove |= Ctx::YIELD
        }
        let outer_await = self.ctx.has(Ctx::AWAIT);
        self.with_ctx(add, remove, |p| {
            let type_params = p.parse_type_params_opt();
            let saved = p.param_decorator_await.replace(outer_await);
            let params = p.parse_params();
            p.param_decorator_await = saved;
            let return_type = p.parse_return_type_opt(T::Colon);
            // Without a body: `'{' expected` where one is needed (nothing is consumed: an
            // object literal goes on at the token), else `;` or TS1144 `'{' or ';'
            // expected` (`function f() => 4;`: the statement list then skips the `=>`).
            // After either error the body is there but empty, as in tsc: no TS7010 or
            // TS2391 for a missing implementation (objectLiteralMemberWithoutBlock1,
            // objectTypesWithOptionalProperties2).
            // An accessor or object-literal method without a body before `;` (or where one
            // could be inserted) is not a syntax error: tsc's checker reports `'{' expected`
            // at its last character (accessorWithoutBody1, objectLiteralMemberWithoutBlock1).
            let body = if p.at(T::LBrace) {
                Some(p.parse_function_body_block())
            } else {
                let empty_body = match rule {
                    BodyRule::Accessor | BodyRule::Required
                        if p.eat(T::Semi) || p.can_insert_semicolon() =>
                    {
                        let end = p.prev_end;
                        p.deferred_grammar_error_at(
                            messages::EXPECTED,
                            Span::new(end.saturating_sub(1), end),
                            &["{"],
                        );
                        rule == BodyRule::Required
                    }
                    BodyRule::Required | BodyRule::Accessor => {
                        p.error_expected("{");
                        true
                    }
                    BodyRule::Optional if !p.ts && !p.ctx.has(Ctx::AMBIENT) => {
                        p.error_expected("{");
                        false
                    }
                    BodyRule::Optional => {
                        let missing = !p.eat(T::Semi) && !p.can_insert_semicolon();
                        if missing {
                            p.error(messages::LBRACE_OR_SEMICOLON_EXPECTED);
                        }
                        missing
                    }
                };
                empty_body.then(|| {
                    p.add_node(
                        Span::at(p.prev_end),
                        Node::Block {
                            stmts: NodeList::EMPTY,
                        },
                    )
                })
            };
            (type_params, params, return_type, body)
        })
    }

    /// Statement-level check used by `parse_statements` callers: can the token start an
    /// expression?
    pub(crate) fn at_expression_start(&mut self) -> bool {
        match self.kind() {
            T::Ident
            | T::Number
            | T::BigInt
            | T::String
            | T::NoSubstTemplate
            | T::TemplateHead
            | T::Regex
            | T::LParen
            | T::LBracket
            | T::LBrace
            | T::Function
            | T::Class
            | T::New
            | T::Slash
            | T::SlashEq
            | T::Plus
            | T::Minus
            | T::Tilde
            | T::Bang
            | T::Delete
            | T::TypeOf
            | T::Void
            | T::PlusPlus
            | T::MinusMinus
            | T::Lt
            | T::This
            | T::Super
            | T::Null
            | T::True
            | T::False
            | T::Import
            | T::At
            | T::PrivateName
            | T::Hash => true,
            k => k.is_identifier_like(),
        }
    }
}
