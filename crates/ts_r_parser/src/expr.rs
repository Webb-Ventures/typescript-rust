//! Expressions: assignment, arrow functions, conditional, binary (precedence climbing),
//! unary, call/member chains, primaries, object/array literals, templates, and binding
//! patterns.

use ts_r_ast::{
    AssignOp, Atom, BinaryOp, F64Bits, FnFlags, ImportPhase, KeywordTypeKind, MetaPropKind,
    MethodKind, Modifiers, Node, NodeId, NodeList, Span, UnaryOp, UpdateOp,
};
use ts_r_diagnostics::messages;
use ts_r_lexer::{TokenFlags, TokenKind as T};

use crate::lists::List;
use crate::modifiers::{ModCtx, is_export_param_modifier_follow, is_param_modifier};
use crate::stmt::BodyRule;
use crate::{Ctx, Parser};

/// Precedence of relational operators (`<`, `instanceof`, `in`, `as`, `satisfies`).
const PREC_RELATIONAL: u8 = 8;

fn binary_op(k: T) -> Option<(u8, BinaryOp)> {
    use BinaryOp as B;
    Some(match k {
        // `??` binds like `||` (left to right), as in tsc 7: `a ?? b || c` is
        // `(a ?? b) || c` and `a ?? b && c` is `a ?? (b && c)`. Mixing them without
        // parentheses is TS5076 (`check_mixed_coalesce`).
        T::QuestionQuestion => (2, B::Coalesce),
        T::PipePipe => (2, B::LogicalOr),
        T::AmpAmp => (3, B::LogicalAnd),
        T::Pipe => (4, B::BitOr),
        T::Caret => (5, B::BitXor),
        T::Amp => (6, B::BitAnd),
        T::EqEq => (7, B::Eq),
        T::NotEq => (7, B::NotEq),
        T::EqEqEq => (7, B::StrictEq),
        T::NotEqEq => (7, B::StrictNotEq),
        T::Lt => (8, B::Lt),
        T::Gt => (8, B::Gt),
        T::LtEq => (8, B::LtEq),
        T::GtEq => (8, B::GtEq),
        T::InstanceOf => (8, B::InstanceOf),
        T::In => (8, B::In),
        T::LtLt => (9, B::Shl),
        T::GtGt => (9, B::Shr),
        T::GtGtGt => (9, B::UShr),
        T::Plus => (10, B::Add),
        T::Minus => (10, B::Sub),
        T::Star => (11, B::Mul),
        T::Slash => (11, B::Div),
        T::Percent => (11, B::Mod),
        T::StarStar => (12, B::Exp),
        _ => return None,
    })
}

fn assign_op(k: T) -> Option<AssignOp> {
    use AssignOp as A;
    Some(match k {
        T::Eq => A::Assign,
        T::PlusEq => A::AddAssign,
        T::MinusEq => A::SubAssign,
        T::StarEq => A::MulAssign,
        T::SlashEq => A::DivAssign,
        T::PercentEq => A::ModAssign,
        T::StarStarEq => A::ExpAssign,
        T::LtLtEq => A::ShlAssign,
        T::GtGtEq => A::ShrAssign,
        T::GtGtGtEq => A::UShrAssign,
        T::PipeEq => A::BitOrAssign,
        T::CaretEq => A::BitXorAssign,
        T::AmpEq => A::BitAndAssign,
        T::PipePipeEq => A::OrAssign,
        T::AmpAmpEq => A::AndAssign,
        T::QuestionQuestionEq => A::CoalesceAssign,
        _ => return None,
    })
}

/// Context flags cleared inside brackets, braces and parentheses.
const NESTED_CLEAR: u32 = Ctx::DISALLOW_IN | Ctx::IN_CONDITIONAL_TRUE | Ctx::DECORATOR | Ctx::TYPE;

impl<'a> Parser<'a> {
    pub(crate) fn parse_expression(&mut self) -> NodeId {
        let start = self.start();
        let first = self.parse_assignment();
        if !self.at(T::Comma) {
            return first;
        }
        let mark = self.scratch.len();
        self.scratch.push(first);
        while self.eat(T::Comma) {
            if self.aborting() {
                break;
            }
            let e = self.parse_assignment();
            self.scratch.push(e);
        }
        let exprs = self.finish_list(mark);
        self.finish_node(start, Node::Seq { exprs })
    }

    pub(crate) fn parse_assignment(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.parse_assignment_inner();
        self.leave();
        r
    }

    fn parse_assignment_inner(&mut self) -> NodeId {
        let start = self.start();
        if self.at(T::Yield) && self.ctx.has(Ctx::YIELD) {
            return self.parse_yield();
        }
        if self.at(T::Yield) && self.yield_outside_generator() {
            return self.parse_yield_outside_generator();
        }
        if let Some(arrow) = self.try_arrow_function() {
            return arrow;
        }
        let left = self.parse_conditional();
        if self.at(T::Gt) {
            self.lx.rescan_greater();
        }
        // As in tsc 7, only a left-hand-side expression takes an assignment operator:
        // `await x = 1`, `-x = 1`, `a + b = c`, `x as T = 1` leave the `=` unconsumed (the
        // statement then reports `';' expected`). `1 = 2` parses (the checker rejects it).
        if let Some(op) = assign_op(self.kind())
            && self.is_lhs_expression(left)
        {
            self.bump();
            let right = self.parse_assignment();
            return self.finish_node(start, Node::Assign { left, op, right });
        }
        left
    }

    /// Whether `id` is a left-hand-side expression in tsc's sense: not a unary, update,
    /// `await`, binary, `as`/`satisfies`, type assertion, conditional, arrow or yield.
    fn is_lhs_expression(&self, id: NodeId) -> bool {
        !matches!(
            self.ast.node(id),
            Node::Unary { .. }
                | Node::Update { .. }
                | Node::Await { .. }
                | Node::Binary { .. }
                | Node::As { .. }
                | Node::Satisfies { .. }
                | Node::TypeAssertion { .. }
                | Node::Conditional { .. }
                | Node::Arrow { .. }
                | Node::Yield { .. }
        )
    }

    fn parse_yield(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        let mut delegate = false;
        let arg = if self.newline_before() {
            None
        } else if self.eat(T::Star) {
            delegate = true;
            Some(self.parse_assignment())
        } else if self.yield_has_argument() {
            Some(self.parse_assignment())
        } else {
            None
        };
        self.finish_node(start, Node::Yield { delegate, arg })
    }

    /// Outside a generator, `yield` starting an assignment expression is a yield expression
    /// (which the checker rejects with TS1163) when a name, keyword, private name or
    /// string/number/bigint literal follows on the same line, as in tsc 7, whatever the
    /// strictness: `yield 1`, `yield foo`, `yield this`. Otherwise (`yield;`, `yield * 2`,
    /// `yield(x)`, `yield => 1`, `yield\n1`) it is an identifier.
    fn yield_outside_generator(&mut self) -> bool {
        if self.tok().flags.has(TokenFlags::ESCAPED) {
            return false;
        }
        let next = self.peek();
        !next.newline_before()
            && (next.kind.is_identifier_or_keyword()
                || matches!(
                    next.kind,
                    T::String | T::Number | T::BigInt | T::PrivateName
                ))
    }

    /// `yield arg` outside a generator: the argument is always parsed (`yield in o` has a
    /// missing left operand, `yield of` yields `of`).
    fn parse_yield_outside_generator(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        let arg = Some(self.parse_assignment());
        self.finish_node(
            start,
            Node::Yield {
                delegate: false,
                arg,
            },
        )
    }

    fn yield_has_argument(&mut self) -> bool {
        !matches!(
            self.kind(),
            T::RParen
                | T::RBracket
                | T::RBrace
                | T::Comma
                | T::Semi
                | T::Colon
                | T::Eof
                | T::Question
                | T::In
                | T::Of
                | T::Eq
        ) && self.kind() != T::Gt
            && (self.at_expression_start() || matches!(self.kind(), T::Lt))
    }

    // ---- Arrow functions ----

    fn try_arrow_function(&mut self) -> Option<NodeId> {
        let start = self.start();
        match self.kind() {
            T::Async if !self.tok().flags.has(TokenFlags::ESCAPED) => {
                let next = self.peek();
                if next.kind == T::Arrow {
                    // `async => async`: a parameter named `async`.
                    return Some(self.parse_simple_arrow(start, FnFlags::NONE));
                }
                if next.newline_before() {
                    return None;
                }
                match next.kind {
                    T::LParen | T::Lt => {
                        if self.ts || next.kind == T::LParen {
                            return self.parse_paren_arrow(start, true);
                        }
                        None
                    }
                    k if k.is_identifier_like() && k != T::Function => {
                        // `async x => ...`
                        let is_arrow = self.lookahead(|p| {
                            p.bump();
                            p.bump();
                            p.at(T::Arrow) && !p.newline_before()
                        });
                        if !is_arrow {
                            return None;
                        }
                        self.bump_modifier(Modifiers::ASYNC);
                        Some(self.parse_simple_arrow(start, FnFlags::ASYNC))
                    }
                    _ => None,
                }
            }
            T::LParen => {
                if self.paren_may_start_arrow() {
                    self.parse_paren_arrow(start, false)
                } else {
                    None
                }
            }
            T::Lt if self.ts => {
                if self.jsx && !self.tsx_lt_starts_generic_arrow() {
                    return None;
                }
                self.parse_paren_arrow(start, false)
            }
            k if k.is_identifier_like() => {
                if !self.at_identifier() {
                    return None;
                }
                let next = self.peek();
                // `x\n=> x` is still an arrow (TS1200), as in tsc.
                if next.kind == T::Arrow {
                    Some(self.parse_simple_arrow(start, FnFlags::NONE))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// At `=>`: a line break before it is TS1200, but the arrow function still parses
    /// (as in tsc), since `=>` cannot start anything else.
    fn check_arrow_line_break(&mut self) {
        if self.newline_before() {
            let span = self.tok().span();
            self.grammar_error_at(messages::LINE_TERMINATOR_BEFORE_ARROW, span, &[]);
        }
    }

    /// `x => body` with the current token being `x`.
    fn parse_simple_arrow(&mut self, start: u32, flags: FnFlags) -> NodeId {
        let pstart = self.start();
        let name = self.intern_token();
        self.bump();
        let ident = self.finish_node(pstart, Node::Ident { name });
        let param = self.finish_node(
            pstart,
            Node::Param {
                decorators: NodeList::EMPTY,
                modifiers: Modifiers::NONE,
                rest: false,
                pattern: ident,
                optional: false,
                type_ann: None,
                init: None,
            },
        );
        let params = self.ast.add_list(&[param]);
        self.check_arrow_line_break();
        self.expect(T::Arrow);
        let (body, flags) = self.parse_arrow_body(flags);
        self.finish_node(
            start,
            Node::Arrow {
                flags,
                type_params: None,
                params,
                return_type: None,
                body,
            },
        )
    }

    /// Quick check after `(`: can this be an arrow function's parameter list?
    /// At `(`: can this only be an arrow function's parameter list? `()` before `:` or `{`,
    /// `(...`, `(x:`, or a parameter modifier. Then a missing `=>` is reported, not
    /// re-parsed as a parenthesised expression.
    fn paren_is_definitely_arrow(&mut self) -> bool {
        if !self.at(T::LParen) {
            return false;
        }
        self.lookahead(|p| {
            p.bump();
            match p.kind() {
                T::RParen => {
                    p.bump();
                    matches!(p.kind(), T::Colon | T::LBrace)
                }
                T::DotDotDot => true,
                k if k.is_identifier_like() => {
                    p.bump();
                    p.at(T::Colon)
                        || (is_param_modifier(k)
                            && (p.kind().is_identifier_like()
                                || matches!(p.kind(), T::LBrace | T::LBracket)))
                }
                _ => false,
            }
        })
    }

    fn paren_may_start_arrow(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            match p.kind() {
                T::RParen => {
                    p.bump();
                    matches!(p.kind(), T::Arrow | T::Colon | T::LBrace)
                }
                T::DotDotDot | T::LBracket | T::LBrace | T::At => true,
                T::This => true,
                k if k.is_identifier_like() => {
                    p.bump();
                    match p.kind() {
                        T::Comma | T::Eq | T::Question | T::Colon => true,
                        T::RParen => {
                            p.bump();
                            matches!(p.kind(), T::Arrow)
                                || (p.ts && p.at(T::Colon))
                                || (p.at(T::LBrace) && !p.newline_before())
                        }
                        // Parameter modifiers: `(public x)`, `(static x)` (misplaced).
                        k2 if k2.is_identifier_like()
                            || matches!(k2, T::LBrace | T::LBracket | T::DotDotDot) =>
                        {
                            is_param_modifier(k)
                        }
                        _ => false,
                    }
                }
                // `(export e) => e`: a misplaced parameter modifier (request R7).
                T::Export => {
                    p.bump();
                    is_export_param_modifier_follow(p.kind())
                }
                _ => false,
            }
        })
    }

    /// In TSX, `<T,>` and `<T extends X>` start generic arrows; `<T>` is JSX.
    fn tsx_lt_starts_generic_arrow(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if p.at(T::Const) {
                p.bump();
            }
            if !p.kind().is_identifier_like() {
                return false;
            }
            p.bump();
            match p.kind() {
                T::Comma => true,
                T::Extends => {
                    p.bump();
                    !matches!(p.kind(), T::Eq | T::Gt | T::Slash)
                }
                _ => false,
            }
        })
    }

    /// Speculatively parses `async? <T>? (params): R =>` and the body.
    fn parse_paren_arrow(&mut self, start: u32, is_async: bool) -> Option<NodeId> {
        let key = (start as u64) << 16 | (self.ctx.0 as u64 & 0x7fff) << 1 | is_async as u64;
        if self.failed_arrows.contains(&key) {
            return None;
        }
        let in_cond = self.ctx.has(Ctx::IN_CONDITIONAL_TRUE);
        let flags = if is_async {
            FnFlags::ASYNC
        } else {
            FnFlags::NONE
        };
        let outer_cp = self.checkpoint();
        let definite = self.paren_is_definitely_arrow();
        let head_fn = |p: &mut Self| {
            if is_async {
                p.bump_modifier(Modifiers::ASYNC);
            }
            let mut add = 0;
            let mut remove = Ctx::DECORATOR | Ctx::IN_CONDITIONAL_TRUE;
            if is_async {
                add |= Ctx::AWAIT
            } else {
                remove |= 0
            }
            p.with_ctx(add, remove, |p| {
                let type_params = if p.at(T::Lt) {
                    if !p.ts {
                        return None;
                    }
                    Some(p.parse_type_params_opt()?)
                } else {
                    None
                };
                if !p.at(T::LParen) {
                    return None;
                }
                let params = p.parse_params();
                if p.aborting() {
                    return None;
                }
                let return_type = if p.at(T::Colon) {
                    if !p.ts {
                        return None;
                    }
                    p.parse_return_type_opt(T::Colon)
                } else {
                    None
                };
                if !p.at(T::Arrow) {
                    // tsc keeps a head that is surely an arrow's, or is followed by `{` on
                    // its line, with `'=>' expected` (arrowFunctionsMissingTokens).
                    let brace = p.at(T::LBrace) && !p.newline_before();
                    if brace || definite {
                        return Some((type_params, params, return_type, false));
                    }
                    return None;
                }
                p.check_arrow_line_break();
                Some((type_params, params, return_type, true))
            })
        };
        // A head that can only be an arrow's (`()`, `(...`, `(x:`; not `(readonly as`) is
        // kept with its errors, as in tsc (`(...) => 105` is TS1003 at the `)`:
        // fatarrowfunctionsOptionalArgsErrors3).
        let commit = definite
            && self.lookahead(|p| {
                p.bump();
                match p.kind() {
                    T::RParen | T::DotDotDot => true,
                    k if k.is_identifier_like() => {
                        p.bump();
                        p.at(T::Colon)
                    }
                    _ => false,
                }
            });
        let head = if commit {
            let r = head_fn(self);
            if r.is_none() {
                self.rewind(outer_cp);
            }
            r
        } else {
            self.try_parse(head_fn)
        };
        let Some((type_params, params, return_type, has_arrow)) = head else {
            self.failed_arrows.insert(key);
            return None;
        };
        if !has_arrow {
            self.error_expected("=>");
            let (body, flags) = self.parse_arrow_body(flags);
            return Some(self.finish_node(
                start,
                Node::Arrow {
                    flags,
                    type_params,
                    params,
                    return_type,
                    body,
                },
            ));
        }
        if in_cond && return_type.is_some() {
            // `a ? (b): c => d : e` vs `a ? (b) : c => d`: keep the arrow only if a `:` follows.
            self.bump();
            let (body, flags) = self.parse_arrow_body(flags);
            if self.at(T::Colon) {
                return Some(self.finish_node(
                    start,
                    Node::Arrow {
                        flags,
                        type_params,
                        params,
                        return_type,
                        body,
                    },
                ));
            }
            // Re-parse everything as a parenthesized expression.
            self.rewind(outer_cp);
            self.failed_arrows.insert(key);
            return None;
        }
        self.bump();
        let (body, flags) = self.parse_arrow_body(flags);
        Some(self.finish_node(
            start,
            Node::Arrow {
                flags,
                type_params,
                params,
                return_type,
                body,
            },
        ))
    }

    pub(crate) fn parse_arrow_body(&mut self, flags: FnFlags) -> (NodeId, FnFlags) {
        let mut add = Ctx::IN_FUNCTION;
        let mut remove = Ctx::YIELD | Ctx::DECORATOR;
        if flags.contains(FnFlags::ASYNC) {
            add |= Ctx::AWAIT
        } else {
            remove |= Ctx::AWAIT
        }
        self.with_ctx(add, remove, |p| {
            if p.at(T::LBrace) {
                let b = p.with_ctx(0, Ctx::DISALLOW_IN | Ctx::IN_CONDITIONAL_TRUE, |p| {
                    p.parse_function_body_block()
                });
                (b, flags)
            } else if !p.at(T::Semi) && !p.is_start_of_expression() && p.is_start_of_statement() {
                // `() => var k = 10; }`: a block body missing its `{`, read up to the `}`,
                // as in tsc.
                let start = p.start();
                p.error_expected("{");
                let stmts = p.with_ctx(0, Ctx::DISALLOW_IN | Ctx::IN_CONDITIONAL_TRUE, |p| {
                    p.parse_statements(false, true)
                });
                p.expect(T::RBrace);
                (p.finish_node(start, Node::Block { stmts }), flags)
            } else {
                (p.parse_assignment(), flags | FnFlags::EXPRESSION_BODY)
            }
        })
    }

    // ---- Conditional and binary ----

    fn parse_conditional(&mut self) -> NodeId {
        let start = self.start();
        let test = self.parse_binary(0);
        if !self.at(T::Question) {
            return test;
        }
        self.bump();
        let cons = self.with_ctx(Ctx::IN_CONDITIONAL_TRUE, Ctx::DISALLOW_IN, |p| {
            p.parse_assignment()
        });
        self.expect(T::Colon);
        let alt = self.with_ctx(0, Ctx::IN_CONDITIONAL_TRUE, |p| p.parse_assignment());
        self.finish_node(start, Node::Conditional { test, cons, alt })
    }

    fn parse_binary(&mut self, min_prec: u8) -> NodeId {
        let start = self.start();
        let left = self.parse_unary();
        self.parse_binary_rest(start, left, min_prec)
    }

    fn parse_binary_rest(&mut self, start: u32, mut left: NodeId, min_prec: u8) -> NodeId {
        loop {
            if self.aborting() {
                return left;
            }
            if self.at(T::Gt) {
                self.lx.rescan_greater();
            }
            let k = self.kind();
            if (k == T::As || k == T::Satisfies)
                && self.ts
                && !self.tok().flags.has(TokenFlags::ESCAPED)
            {
                if self.newline_before() || PREC_RELATIONAL <= min_prec {
                    break;
                }
                self.bump();
                let ty = if k == T::As && self.at(T::Const) {
                    let s = self.start();
                    self.bump();
                    self.finish_node(
                        s,
                        Node::KeywordType {
                            kind: KeywordTypeKind::Const,
                        },
                    )
                } else {
                    self.parse_type()
                };
                let node = if k == T::As {
                    Node::As { expr: left, ty }
                } else {
                    Node::Satisfies { expr: left, ty }
                };
                left = self.finish_node(start, node);
                continue;
            }
            let Some((prec, op)) = binary_op(k) else {
                break;
            };
            if prec <= min_prec {
                break;
            }
            if k == T::In && self.ctx.has(Ctx::DISALLOW_IN) {
                break;
            }
            if op == BinaryOp::Exp {
                self.check_exponentiation_left(left);
            }
            self.bump();
            let rstart = self.start();
            let right = self.parse_unary();
            let next_min = if op == BinaryOp::Exp { prec - 1 } else { prec };
            let right = self.parse_binary_rest(rstart, right, next_min);
            self.check_mixed_coalesce(op, left, right);
            left = self.finish_node(start, Node::Binary { left, op, right });
        }
        left
    }

    /// `-a ** b`, `await a ** b`, `<T>a ** b`: TS17006 / TS17007 at the operand, a syntax
    /// error in tsc 7. The tree stays `Binary(Exp, Unary, b)`.
    fn check_exponentiation_left(&mut self, left: NodeId) {
        let op = match *self.ast.node(left) {
            Node::Unary { op, .. } => op.as_str(),
            Node::Await { .. } => "await",
            Node::TypeAssertion { .. } => {
                let span = self.ast.span(left);
                self.error_at(messages::TYPE_ASSERTION_LEFT_OF_EXPONENTIATION, span, &[]);
                return;
            }
            _ => return,
        };
        let span = self.ast.span(left);
        self.error_at(messages::UNARY_LEFT_OF_EXPONENTIATION, span, &[op]);
    }

    /// `??` mixed with `||` or `&&` without parentheses: TS5076 at the inner operation, the
    /// operators named in source order. tsc 7 reports this from its grammar check (next to
    /// semantic errors); we report it here so tools see the file as invalid.
    fn check_mixed_coalesce(&mut self, op: BinaryOp, left: NodeId, right: NodeId) {
        let logical = |o: BinaryOp| matches!(o, BinaryOp::LogicalOr | BinaryOp::LogicalAnd);
        let clash = |inner: BinaryOp| {
            if op == BinaryOp::Coalesce {
                logical(inner)
            } else {
                logical(op) && inner == BinaryOp::Coalesce
            }
        };
        for (side, is_left) in [(left, true), (right, false)] {
            if let Node::Binary { op: inner, .. } = *self.ast.node(side)
                && clash(inner)
            {
                let (first, second) = if is_left { (inner, op) } else { (op, inner) };
                let span = self.ast.span(side);
                self.grammar_error_at(
                    messages::MIXED_COALESCE,
                    span,
                    &[first.as_str(), second.as_str()],
                );
            }
        }
    }

    // ---- Unary ----

    pub(crate) fn parse_unary(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.parse_unary_inner();
        self.leave();
        r
    }

    fn parse_unary_inner(&mut self) -> NodeId {
        let start = self.start();
        let op = match self.kind() {
            T::Delete => Some(UnaryOp::Delete),
            T::Void => Some(UnaryOp::Void),
            T::TypeOf => Some(UnaryOp::TypeOf),
            T::Plus => Some(UnaryOp::Plus),
            T::Minus => Some(UnaryOp::Minus),
            T::Tilde => Some(UnaryOp::BitNot),
            T::Bang => Some(UnaryOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let arg = self.parse_unary();
            return self.finish_node(start, Node::Unary { op, arg });
        }
        match self.kind() {
            T::PlusPlus | T::MinusMinus => {
                let op = if self.at(T::PlusPlus) {
                    UpdateOp::Increment
                } else {
                    UpdateOp::Decrement
                };
                self.bump();
                // As in tsc 7 the operand is a left-hand-side expression, with no postfix
                // operator: `++a++` is `++a` then `';' expected`; `++-a` and `++await a`
                // are TS1109 at the operator.
                let arg = self.parse_lhs();
                return self.finish_node(
                    start,
                    Node::Update {
                        op,
                        prefix: true,
                        arg,
                    },
                );
            }
            T::Await if self.is_await_expression() => {
                self.bump();
                let arg = self.parse_unary();
                return self.finish_node(start, Node::Await { arg });
            }
            T::Lt if self.ts && !self.jsx => {
                // `<T>expr` type assertion.
                self.bump();
                let ty = if self.at(T::Const) {
                    let s = self.start();
                    self.bump();
                    self.finish_node(
                        s,
                        Node::KeywordType {
                            kind: KeywordTypeKind::Const,
                        },
                    )
                } else {
                    self.with_ctx(Ctx::TYPE, 0, |p| p.parse_type())
                };
                self.expect(T::Gt);
                let expr = self.parse_unary();
                return self.finish_node(start, Node::TypeAssertion { ty, expr });
            }
            _ => {}
        }
        let expr = self.parse_lhs();
        if matches!(self.kind(), T::PlusPlus | T::MinusMinus) && !self.newline_before() {
            let op = if self.at(T::PlusPlus) {
                UpdateOp::Increment
            } else {
                UpdateOp::Decrement
            };
            self.bump();
            return self.finish_node(
                start,
                Node::Update {
                    op,
                    prefix: false,
                    arg: expr,
                },
            );
        }
        expr
    }

    fn is_await_expression(&mut self) -> bool {
        if self.ctx.has(Ctx::AWAIT) {
            return true;
        }
        if self.ctx.has(Ctx::IN_FUNCTION) {
            // `await x` in a function that is not async: an await expression when a name,
            // keyword or literal follows on the same line (as tsc parses it, for recovery);
            // the checker reports TS1308.
            let next = self.peek();
            return !next.newline_before()
                && (next.kind.is_identifier_or_keyword()
                    || matches!(
                        next.kind,
                        T::Number | T::BigInt | T::String | T::NoSubstTemplate
                    ));
        }
        // Top-level await: `await` followed by an expression on the same line.
        let next = self.peek();
        if next.newline_before() {
            return false;
        }
        match next.kind {
            T::Ident
            | T::Number
            | T::BigInt
            | T::String
            | T::NoSubstTemplate
            | T::TemplateHead
            | T::LParen
            | T::LBracket
            | T::LBrace
            | T::Function
            | T::Class
            | T::New
            | T::This
            | T::Super
            | T::Null
            | T::True
            | T::False
            | T::Import
            | T::Bang
            | T::Tilde
            | T::TypeOf
            | T::Void
            | T::Delete
            | T::Await
            | T::Slash
            | T::Lt
            | T::PlusPlus
            | T::MinusMinus => true,
            k => {
                k.is_identifier_like()
                    && !matches!(k, T::In | T::InstanceOf | T::As | T::Satisfies | T::Of)
            }
        }
    }

    // ---- Call and member chains ----

    /// Left-hand-side expression (also decorators and heritage clauses). Depth-limited.
    pub(crate) fn parse_lhs(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.parse_lhs_inner();
        self.leave();
        r
    }

    fn parse_lhs_inner(&mut self) -> NodeId {
        let start = self.start();
        let expr = match self.kind() {
            T::New => self.parse_new(),
            T::Import => self.parse_import_expression(),
            T::Super => {
                self.bump();
                self.finish_node(start, Node::Super {})
            }
            _ => self.parse_primary(),
        };
        self.parse_member_tail(start, expr, true)
    }

    fn parse_import_expression(&mut self) -> NodeId {
        let start = self.start();
        if !matches!(self.peek_kind(), T::LParen | T::Dot) {
            // `var x = import { foo } from "m"`: `Expression expected` at `import`, which is
            // left for the import declaration that follows (importCallExpressionIncorrect2).
            self.error(messages::EXPRESSION_EXPECTED);
            return self.missing();
        }
        self.bump();
        let mut phase = ImportPhase::Evaluation;
        if self.at(T::Dot) {
            self.bump();
            if self.at_contextual(T::Defer) || self.at_contextual(T::Source) {
                phase = if self.at(T::Defer) {
                    ImportPhase::Defer
                } else {
                    ImportPhase::Source
                };
                self.bump();
            } else {
                if self.text() != "meta" {
                    self.error_expected("meta");
                }
                if self.kind().is_identifier_or_keyword() {
                    self.bump();
                }
                return self.finish_node(
                    start,
                    Node::MetaProperty {
                        kind: MetaPropKind::ImportMeta,
                    },
                );
            }
        }
        let open = self.start();
        if !self.expect(T::LParen) {
            let arg = self.missing();
            return self.finish_node(
                start,
                Node::ImportCall {
                    phase,
                    arg,
                    options: None,
                },
            );
        }
        // An argument list as in a call (importAssertion1, plainJSGrammarErrors): the
        // checker's rules are grammar errors here. A third argument and later ones are
        // dropped (tsc: TS1450, a message, not an error).
        let (args, trailing) = self.with_ctx(0, NESTED_CLEAR, |p| {
            p.parse_delimited(List::ArgumentExpressions, |p| {
                let s = p.start();
                if p.eat(T::DotDotDot) {
                    let arg = p.parse_assignment();
                    let spread = p.finish_node(s, Node::Spread { arg });
                    let span = p.ast.span(spread);
                    p.deferred_grammar_error_at(messages::IMPORT_SPREAD_ARGUMENT, span, &[]);
                    spread
                } else {
                    p.parse_assignment()
                }
            })
        });
        self.expect_closing(T::RParen, T::LParen, open);
        let list = self.ast.list(args);
        let arg = list.first().copied();
        let options = list.get(1).copied();
        if let (Some(_), None, Some(comma)) = (arg, options, trailing) {
            // `import(x,)` (dynamicImportTrailingComma); `import(x, {},)` is fine.
            self.deferred_grammar_error_at(
                messages::TRAILING_COMMA_NOT_ALLOWED,
                Span::new(comma, comma + 1),
                &[],
            );
        }
        let arg = arg.unwrap_or_else(|| {
            let pos = self.prev_end.saturating_sub(1);
            self.add_node(Span::at(pos), Node::Missing {})
        });
        self.finish_node(
            start,
            Node::ImportCall {
                phase,
                arg,
                options,
            },
        )
    }

    /// `new X(...)`, `new.target`; depth-limited because `new new new X` recurses.
    fn parse_new(&mut self) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.parse_new_inner();
        self.leave();
        r
    }

    fn parse_new_inner(&mut self) -> NodeId {
        let start = self.start();
        self.bump();
        if self.at(T::Dot) {
            self.bump();
            if self.text() != "target" {
                self.error_expected("target");
            }
            if self.kind().is_identifier_or_keyword() {
                self.bump();
            }
            return self.finish_node(
                start,
                Node::MetaProperty {
                    kind: MetaPropKind::NewTarget,
                },
            );
        }
        let cstart = self.start();
        let mut callee = match self.kind() {
            T::New => self.parse_new(),
            T::Import => self.parse_import_expression(),
            T::Super => {
                self.bump();
                self.finish_node(cstart, Node::Super {})
            }
            _ => self.parse_primary(),
        };
        let mut type_args;
        loop {
            callee = self.parse_member_tail(cstart, callee, false);
            type_args = None;
            if self.ts
                && !self.js_ts
                && matches!(self.kind(), T::Lt | T::LtLt)
                && !self.type_args_known_to_fail()
            {
                type_args = self.try_parse(|p| {
                    let ta = p.parse_type_args();
                    if p.at(T::LParen) || p.can_follow_type_args() {
                        Some(ta)
                    } else {
                        None
                    }
                });
            }
            // `new tag<T> `x` (args)`: the type arguments belong to the tagged template,
            // which is the callee (tsc 7), not to a `new tag<T>` that the template tags.
            match type_args {
                Some(ta) if matches!(self.kind(), T::NoSubstTemplate | T::TemplateHead) => {
                    let quasi = self.parse_template(true);
                    callee = self.finish_node(
                        cstart,
                        Node::TaggedTemplate {
                            tag: callee,
                            type_args: Some(ta),
                            quasi,
                        },
                    );
                }
                _ => break,
            }
        }
        let (has_args, args) = if self.at(T::LParen) {
            (true, self.parse_arguments())
        } else {
            (false, NodeList::EMPTY)
        };
        // `new a?.b()`: TS1209 at `?.` (a syntax error in tsc 7). The chain goes on as
        // `(new a)?.b()`, as in tsc's tree.
        if !has_args && self.at(T::QuestionDot) {
            let src = self.src;
            let s = self.ast.span(callee);
            let text = src.get(s.start as usize..s.end as usize).unwrap_or("");
            let span = self.tok().span();
            self.error_at(messages::INVALID_OPTIONAL_CHAIN_FROM_NEW, span, &[text]);
        }
        self.finish_node(
            start,
            Node::New {
                callee,
                type_args,
                has_args,
                args,
            },
        )
    }

    /// `a?.#x`, `a?.b.#x`, `a?.b().#x`: a private name in an optional chain is TS18030 at
    /// the name (a syntax error in tsc 7, so reported in unchecked JavaScript too). Parentheses
    /// end the chain.
    fn check_private_name_in_chain(&mut self, prop: NodeId, in_chain: bool) {
        if in_chain && matches!(*self.ast.node(prop), Node::PrivateName { .. }) {
            let span = self.ast.span(prop);
            self.grammar_error_at(messages::OPTIONAL_CHAIN_PRIVATE_NAME, span, &[]);
        }
    }

    /// `.x`, `?.x`, `[x]`, `(args)`, `` `tpl` ``, `!`, `<T>(args)` after an expression.
    pub(crate) fn parse_member_tail(
        &mut self,
        start: u32,
        mut expr: NodeId,
        allow_call: bool,
    ) -> NodeId {
        // The head is a primary expression, `new`, or a tagged template; each starts a
        // fresh chain. Track membership as tails are appended instead of walking prefixes.
        let mut in_chain = false;
        loop {
            if self.aborting() {
                return expr;
            }
            match self.kind() {
                T::Dot => {
                    self.bump();
                    let prop = self.parse_member_name();
                    self.check_private_name_in_chain(prop, in_chain);
                    expr = self.finish_node(
                        start,
                        Node::Member {
                            obj: expr,
                            optional: false,
                            prop,
                        },
                    );
                }
                T::QuestionDot => {
                    if !allow_call {
                        // `new a?.b()` is invalid; stop here.
                        return expr;
                    }
                    self.bump();
                    in_chain = true;
                    match self.kind() {
                        T::LParen => {
                            let args = self.parse_arguments();
                            expr = self.finish_node(
                                start,
                                Node::Call {
                                    callee: expr,
                                    optional: true,
                                    type_args: None,
                                    args,
                                },
                            );
                        }
                        T::LBracket => {
                            let index = self.parse_index_bracket();
                            expr = self.finish_node(
                                start,
                                Node::Index {
                                    obj: expr,
                                    optional: true,
                                    index,
                                },
                            );
                        }
                        T::Lt | T::LtLt if self.ts && !self.js_ts => {
                            let type_args = self.parse_type_args();
                            let args = if self.at(T::LParen) {
                                self.parse_arguments()
                            } else {
                                self.error_expected("(");
                                NodeList::EMPTY
                            };
                            expr = self.finish_node(
                                start,
                                Node::Call {
                                    callee: expr,
                                    optional: true,
                                    type_args: Some(type_args),
                                    args,
                                },
                            );
                        }
                        _ => {
                            let prop = self.parse_member_name();
                            self.check_private_name_in_chain(prop, in_chain);
                            expr = self.finish_node(
                                start,
                                Node::Member {
                                    obj: expr,
                                    optional: true,
                                    prop,
                                },
                            );
                        }
                    }
                }
                T::LBracket if !self.ctx.has(Ctx::DECORATOR) => {
                    let index = self.parse_index_bracket();
                    expr = self.finish_node(
                        start,
                        Node::Index {
                            obj: expr,
                            optional: false,
                            index,
                        },
                    );
                }
                T::Bang if self.ts && !self.newline_before() => {
                    self.bump();
                    expr = self.finish_node(start, Node::NonNull { expr });
                }
                T::NoSubstTemplate | T::TemplateHead => {
                    let quasi = self.parse_template(true);
                    in_chain = false;
                    expr = self.finish_node(
                        start,
                        Node::TaggedTemplate {
                            tag: expr,
                            type_args: None,
                            quasi,
                        },
                    );
                }
                T::LParen if allow_call => {
                    let args = self.parse_arguments();
                    expr = self.finish_node(
                        start,
                        Node::Call {
                            callee: expr,
                            optional: false,
                            type_args: None,
                            args,
                        },
                    );
                }
                T::Lt | T::LtLt if self.ts && !self.js_ts && allow_call => {
                    if self.type_args_known_to_fail() {
                        return expr;
                    }
                    let Some(type_args) = self.try_parse(|p| {
                        let ta = p.parse_type_args();
                        if p.can_follow_type_args() {
                            Some(ta)
                        } else {
                            None
                        }
                    }) else {
                        return expr;
                    };
                    match self.kind() {
                        T::LParen => {
                            let args = self.parse_arguments();
                            expr = self.finish_node(
                                start,
                                Node::Call {
                                    callee: expr,
                                    optional: false,
                                    type_args: Some(type_args),
                                    args,
                                },
                            );
                        }
                        T::NoSubstTemplate | T::TemplateHead => {
                            let quasi = self.parse_template(true);
                            in_chain = false;
                            expr = self.finish_node(
                                start,
                                Node::TaggedTemplate {
                                    tag: expr,
                                    type_args: Some(type_args),
                                    quasi,
                                },
                            );
                        }
                        _ => {
                            in_chain = false;
                            expr = self.finish_node(start, Node::Instantiation { expr, type_args });
                            // tsc reports this at the type arguments, not at the `.`. An
                            // optional call (`a<b>?.()`) is not a property access.
                            if self.at(T::Dot)
                                || (self.at(T::QuestionDot) && self.peek().kind != T::LParen)
                            {
                                let span = self.ast.span(type_args);
                                self.error_at(
                                    messages::INSTANTIATION_EXPRESSION_PROPERTY_ACCESS,
                                    span,
                                    &[],
                                );
                            }
                        }
                    }
                }
                _ => return expr,
            }
        }
    }

    /// After `>` closing type arguments in an expression: is this really a type argument list?
    fn can_follow_type_args(&mut self) -> bool {
        match self.kind() {
            T::LParen | T::NoSubstTemplate | T::TemplateHead => true,
            T::Lt | T::Gt | T::Plus | T::Minus => false,
            _ => {
                if self.newline_before() {
                    return true;
                }
                let k = self.kind();
                binary_op(k).is_some()
                    || assign_op(k).is_some()
                    || !self.at_expression_start()
                    || matches!(k, T::As | T::Satisfies)
            }
        }
    }

    fn parse_index_bracket(&mut self) -> NodeId {
        let open = self.start();
        self.bump();
        let index = if self.at(T::RBracket) {
            self.error(messages::ELEMENT_ACCESS_NEEDS_ARGUMENT);
            self.missing()
        } else {
            self.with_ctx(0, NESTED_CLEAR, |p| p.parse_expression())
        };
        self.expect_closing(T::RBracket, T::LBracket, open);
        index
    }

    /// Property name after `.`: identifier, keyword or private name.
    fn parse_member_name(&mut self) -> NodeId {
        if let Some(missing) = self.missing_name_after_dot() {
            return missing;
        }
        match self.kind() {
            T::PrivateName => self.parse_private_name(),
            k if k.is_identifier_or_keyword() => self.parse_identifier_name(),
            _ => {
                self.error(messages::IDENTIFIER_EXPECTED);
                self.missing()
            }
        }
    }

    pub(crate) fn parse_arguments(&mut self) -> NodeList {
        let open = self.start();
        self.expect(T::LParen);
        let (args, _) = self.with_ctx(0, NESTED_CLEAR, |p| {
            p.parse_delimited(List::ArgumentExpressions, |p| {
                let s = p.start();
                if p.eat(T::DotDotDot) {
                    let arg = p.parse_assignment();
                    p.finish_node(s, Node::Spread { arg })
                } else {
                    p.parse_assignment()
                }
            })
        });
        self.expect_closing(T::RParen, T::LParen, open);
        args
    }

    // ---- Primary expressions ----

    fn parse_primary(&mut self) -> NodeId {
        let start = self.start();
        match self.kind() {
            T::This => {
                self.bump();
                self.finish_node(start, Node::This {})
            }
            T::Null => {
                self.bump();
                self.finish_node(start, Node::NullLit {})
            }
            T::True | T::False => {
                let value = self.at(T::True);
                self.bump();
                self.finish_node(start, Node::BoolLit { value })
            }
            T::Number => self.parse_numeric_literal(),
            T::BigInt => self.parse_bigint_literal(),
            T::String => self.parse_string_literal(),
            T::NoSubstTemplate | T::TemplateHead => self.parse_template(false),
            T::Slash | T::SlashEq => {
                self.lx.rescan_slash_as_regex();
                let tok = self.tok();
                let (ps, fs) = self.lx.regex_parts(tok);
                let pattern = self.ast.intern(self.lx.text(ps));
                let flags = self.ast.intern(self.lx.text(fs));
                self.bump();
                self.finish_node(start, Node::RegexLit { pattern, flags })
            }
            T::LParen => self.parse_paren_expression(),
            T::LBracket => self.parse_array_literal(),
            T::LBrace => self.parse_object_literal(),
            T::Function => self.parse_function(start, Modifiers::NONE, true),
            T::Async if self.next_is_function_on_same_line() => {
                self.bump_modifier(Modifiers::ASYNC);
                self.parse_function(start, Modifiers::ASYNC, true)
            }
            T::Class => self.parse_class(start, NodeList::EMPTY, Modifiers::NONE, true),
            T::At => {
                let decorators = self.parse_decorators();
                if self.at(T::Class) {
                    self.parse_class(start, decorators, Modifiers::NONE, true)
                } else {
                    self.error_expected("class");
                    self.missing()
                }
            }
            T::Lt if self.jsx => self.parse_jsx_element_or_fragment(false),
            T::PrivateName => self.parse_private_name(),
            T::New => self.parse_new(),
            // In a generator, `yield` is not an operand (`<T> yield x`, `1 + yield x`): tsc
            // reports TS1109 and leaves `yield` for the next statement or argument.
            T::Yield if self.ctx.has(Ctx::YIELD) && !self.tok().flags.has(TokenFlags::ESCAPED) => {
                self.error(messages::EXPRESSION_EXPECTED);
                self.missing()
            }
            // Likewise `await` in an async function where only an operand may appear
            // (`++await x`): TS1109, `await` left for the next statement.
            T::Await if self.ctx.has(Ctx::AWAIT) && !self.tok().flags.has(TokenFlags::ESCAPED) => {
                self.error(messages::EXPRESSION_EXPECTED);
                self.missing()
            }
            k if k.is_identifier_like() => {
                if !(self.at_identifier() || k == T::Await || k == T::Yield) {
                    self.error(messages::IDENTIFIER_EXPECTED);
                }
                self.parse_identifier_unchecked()
            }
            _ => {
                if self.kind().is_reserved() {
                    // `x = class` etc.
                    self.error(messages::EXPRESSION_EXPECTED);
                } else {
                    self.error(messages::EXPRESSION_EXPECTED);
                }
                self.missing()
            }
        }
    }

    fn parse_paren_expression(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let expr = self.with_ctx(0, NESTED_CLEAR, |p| p.parse_expression());
        self.expect_closing(T::RParen, T::LParen, open);
        self.finish_node(start, Node::Paren { expr })
    }

    fn parse_array_literal(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let (elements, _) = self.with_ctx(0, NESTED_CLEAR, |p| {
            p.parse_delimited(List::ArrayLiteralMembers, |p| match p.kind() {
                // A hole; the `,` is the list's.
                T::Comma => p.add_node(Span::at(p.start()), Node::Hole {}),
                T::DotDotDot => {
                    let s = p.start();
                    p.bump();
                    let arg = p.parse_assignment();
                    p.finish_node(s, Node::Spread { arg })
                }
                _ => p.parse_assignment(),
            })
        });
        self.expect_closing(T::RBracket, T::LBracket, open);
        self.finish_node(start, Node::ArrayLit { elements })
    }

    fn parse_object_literal(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let (props, _) = self.with_ctx(0, NESTED_CLEAR, |p| {
            p.parse_delimited(List::ObjectLiteralMembers, |p| p.parse_object_member())
        });
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(start, Node::ObjectLit { props })
    }

    fn parse_object_member(&mut self) -> NodeId {
        let start = self.start();
        if self.eat(T::DotDotDot) {
            let arg = self.parse_assignment();
            return self.finish_node(start, Node::Spread { arg });
        }
        // `async` and misplaced modifiers (`{ static m() {} }`, `{ public x: 1 }`).
        let mut modifiers = self.parse_modifiers(ModCtx::ObjectMember);
        let mut flags = FnFlags::NONE;
        let mut kind = MethodKind::Method;
        let mut is_method = false;
        if self.at(T::Star) {
            self.bump();
            flags |= FnFlags::GENERATOR;
            is_method = true;
        }
        if !is_method
            && matches!(self.kind(), T::Get | T::Set)
            && self.next_is_property_name_after_modifier(false)
        {
            kind = if self.at(T::Get) {
                MethodKind::Get
            } else {
                MethodKind::Set
            };
            self.bump();
            is_method = true;
        }
        let key_tok = self.tok();
        let key = self.parse_property_key();
        if self.ts && self.at(T::Question) {
            // `{ a?() {} }` is an error but parse it.
            self.error(messages::AN_OBJECT_MEMBER_CANNOT_BE_DECLARED_OPTIONAL);
            self.bump();
        }
        if is_method || matches!(self.kind(), T::LParen | T::Lt) {
            if modifiers.contains(Modifiers::ASYNC) {
                modifiers.remove(Modifiers::ASYNC);
                flags |= FnFlags::ASYNC;
            }
            let fstart = self.start();
            let rule = if matches!(kind, MethodKind::Get | MethodKind::Set) {
                BodyRule::Accessor
            } else {
                BodyRule::Required
            };
            let (type_params, params, return_type, body) = self.parse_function_rest(flags, rule);
            self.method_grammar(kind, key, type_params, params, return_type);
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
            return self.finish_node(
                start,
                Node::MethodDef {
                    decorators: NodeList::EMPTY,
                    modifiers,
                    kind,
                    key,
                    optional: false,
                    value,
                },
            );
        }
        if self.eat(T::Colon) {
            let value = self.parse_assignment();
            return self.finish_node(
                start,
                Node::Property {
                    modifiers,
                    key,
                    value,
                },
            );
        }
        let shorthand_ok =
            key_tok.kind.is_identifier_like() && matches!(self.ast.node(key), Node::Ident { .. });
        if !shorthand_ok {
            self.error_expected(":");
            return key;
        }
        let init = if self.eat(T::Eq) {
            Some(self.parse_assignment())
        } else {
            None
        };
        if self.ts && self.at(T::Bang) {
            // `{ a! }` (invalid) — skip.
            self.bump();
        }
        self.finish_node(
            start,
            Node::ShorthandProp {
                modifiers,
                name: key,
                init,
            },
        )
    }

    /// After `get`/`set`/`async`/modifiers: does a property name follow (making the current
    /// token a modifier rather than the name itself)?
    pub(crate) fn next_is_property_name_after_modifier(&mut self, same_line: bool) -> bool {
        let next = self.peek();
        if same_line && next.newline_before() {
            return false;
        }
        match next.kind {
            T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName | T::Star => true,
            k => k.is_identifier_or_keyword(),
        }
    }

    /// Property name: identifier/keyword, string, number, bigint, `[computed]`, `#private`.
    pub(crate) fn parse_property_key(&mut self) -> NodeId {
        match self.kind() {
            T::String => self.parse_string_literal(),
            T::Number => self.parse_numeric_literal(),
            T::BigInt => self.parse_bigint_literal(),
            T::LBracket => self.parse_computed_key(),
            T::PrivateName => {
                if !self.ctx.has(Ctx::IN_CLASS) {
                    // `{ #x: 1 }`, `interface I { #x: T }`: a checker grammar error
                    // (privateNameBadDeclaration, privateNameAndPropertySignature). Member
                    // access (`a.#x`) is the checker's business.
                    let span = self.tok().span();
                    self.deferred_grammar_error_at(messages::PRIVATE_NAME_OUTSIDE_CLASS, span, &[]);
                }
                self.parse_private_name()
            }
            k if k.is_identifier_or_keyword() => self.parse_identifier_name(),
            _ => {
                self.error(messages::PROPERTY_ASSIGNMENT_EXPECTED);
                self.missing()
            }
        }
    }

    pub(crate) fn parse_computed_key(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let expr = self.with_ctx(0, NESTED_CLEAR, |p| p.parse_assignment());
        self.expect_closing(T::RBracket, T::LBracket, open);
        self.finish_node(start, Node::ComputedKey { expr })
    }

    // ---- Literals and names ----

    pub(crate) fn parse_string_literal(&mut self) -> NodeId {
        let start = self.start();
        let tok = self.tok();
        let v = self.lx.string_value(tok);
        let value = self.ast.intern(&v);
        self.bump();
        self.finish_node(start, Node::StringLit { value })
    }

    pub(crate) fn parse_numeric_literal(&mut self) -> NodeId {
        let start = self.start();
        let tok = self.tok();
        let value = self.lx.number_value(tok);
        if tok.flags.has(TokenFlags::LEGACY_OCTAL) && self.ts {
            let text = self.text();
            if text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                let n = ts_r_lexer::parse_number(text) as u64;
                let suggestion = format!("0o{n:o}");
                self.error_at(
                    messages::OCTAL_LITERALS_NOT_ALLOWED,
                    tok.span(),
                    &[&suggestion],
                );
            } else {
                self.error_at(messages::DECIMALS_WITH_LEADING_ZEROS, tok.span(), &[]);
            }
        }
        self.bump();
        self.finish_node(
            start,
            Node::NumberLit {
                value: F64Bits::new(value),
            },
        )
    }

    pub(crate) fn parse_bigint_literal(&mut self) -> NodeId {
        let start = self.start();
        let text = self.text();
        let value = self.ast.intern(text.strip_suffix('n').unwrap_or(text));
        self.bump();
        self.finish_node(start, Node::BigIntLit { value })
    }

    pub(crate) fn parse_private_name(&mut self) -> NodeId {
        let start = self.start();
        let v = self.lx.ident_value(self.tok());
        let name = self.ast.intern(v.strip_prefix('#').unwrap_or(&v));
        self.bump();
        self.finish_node(start, Node::PrivateName { name })
    }

    /// An identifier reference or binding. Reports and returns `Missing` for non-identifiers.
    pub(crate) fn parse_identifier(&mut self) -> NodeId {
        if self.kind().is_identifier_like() {
            if !self.at_identifier() {
                // `await`/`yield` in a context where they are operators. `await` is TS1359
                // and stays a name, as in tsc (`async (await): T => {}` is still an arrow);
                // tsc reports it from the checker, so it does not stop semantic checking.
                if self.at(T::Await) {
                    let span = self.tok().span();
                    self.deferred_grammar_error_at(
                        messages::RESERVED_WORD_CANNOT_BE_USED_HERE,
                        span,
                        &["await"],
                    );
                } else if !self.at(T::Yield) {
                    self.error(messages::IDENTIFIER_EXPECTED);
                }
                // `yield` in a generator stays a name with no parse error (tsc 7): the binder
                // reports it as a strict-mode reserved word (TS1212-TS1214).
            }
            return self.parse_identifier_unchecked();
        }
        if self.kind().is_reserved() {
            self.reserved_word_error();
            // Consume reserved words used as names (`var class = 1`) when followed by
            // something that continues a declaration.
            let next = self.peek_kind();
            if matches!(
                next,
                T::Eq | T::Colon | T::Comma | T::Semi | T::LParen | T::Lt | T::LBrace
            ) {
                return self.parse_identifier_unchecked();
            }
            return self.missing();
        }
        self.error(messages::IDENTIFIER_EXPECTED);
        self.missing()
    }

    /// A reserved word where a name is needed: TS1359 "Identifier expected. 'void' is a
    /// reserved word that cannot be used here." (parserEnumDeclaration4,
    /// objectBindingPatternKeywordIdentifiers02).
    pub(crate) fn reserved_word_error(&mut self) {
        let span = self.tok().span();
        let word = span.text(self.src).to_owned();
        self.error_at(messages::RESERVED_WORD_CANNOT_BE_USED_HERE, span, &[&word]);
    }

    pub(crate) fn parse_identifier_unchecked(&mut self) -> NodeId {
        let start = self.start();
        let name = self.intern_token();
        self.bump();
        self.finish_node(start, Node::Ident { name })
    }

    /// Any identifier or keyword (property names, after `.`).
    /// The name after a `.` is missing at the end of the file, and before a declaration on
    /// the next line (`B.` + newline + `namespace B {}`,
    /// `x.` + newline + `var y`): TS1003 just after the `.`, and the next line starts a new
    /// statement (enumMemberResolution, incompleteDottedExpressionAtEOF,
    /// parserUnfinishedTypeNameBeforeKeyword1).
    pub(crate) fn missing_name_after_dot(&mut self) -> Option<NodeId> {
        let missing = self.at(T::Eof)
            || (self.newline_before()
                && self.kind().is_identifier_or_keyword()
                && {
                    let next = self.peek();
                    !next.newline_before()
                        && next.kind.is_identifier_or_keyword()
                        && binary_op(next.kind).is_none()
                        && !matches!(next.kind, T::As | T::Satisfies)
                }
                && self.declaration_follows_modifiers());
        if !missing {
            return None;
        }
        self.error_at(messages::IDENTIFIER_EXPECTED, Span::at(self.prev_end), &[]);
        Some(self.missing())
    }

    pub(crate) fn parse_identifier_name(&mut self) -> NodeId {
        if self.kind().is_identifier_or_keyword() {
            return self.parse_identifier_unchecked();
        }
        self.error(messages::IDENTIFIER_EXPECTED);
        self.missing()
    }

    // ---- Templates ----

    /// Template literal at the current `NoSubstTemplate` or `TemplateHead` token.
    pub(crate) fn parse_template(&mut self, tagged: bool) -> NodeId {
        let start = self.start();
        let mark = self.scratch.len();
        let first = self.template_element(tagged);
        self.scratch.push(first);
        if self.at(T::TemplateHead) {
            self.bump();
            loop {
                if self.aborting() {
                    break;
                }
                let e = self.with_ctx(0, NESTED_CLEAR, |p| p.parse_expression());
                self.scratch.push(e);
                if !self.at(T::RBrace) {
                    self.error_expected("}");
                    break;
                }
                self.lx.rescan_template_continuation();
                let el = self.template_element(tagged);
                self.scratch.push(el);
                let is_tail = self.at(T::TemplateTail);
                self.bump();
                if is_tail {
                    break;
                }
            }
        } else {
            self.bump();
        }
        let parts = self.finish_list(mark);
        self.finish_node(start, Node::TemplateLit { parts })
    }

    /// Builds a `TemplateElement` for the current template token without consuming it.
    pub(crate) fn template_element(&mut self, tagged: bool) -> NodeId {
        let tok = self.tok();
        let span = self.lx.template_raw_span(tok);
        let (cooked, invalid_escape) = match self.lx.template_cooked(tok) {
            Ok(c) => (self.ast.intern(&c), false),
            Err((at, msg)) => {
                if !tagged {
                    self.error_at(msg, at, &[]);
                }
                (Atom::EMPTY, true)
            }
        };
        self.add_node(
            span,
            Node::TemplateElement {
                cooked,
                invalid_escape,
            },
        )
    }

    // ---- Binding patterns ----

    /// Identifier, object pattern or array pattern.
    pub(crate) fn parse_binding_target(&mut self) -> NodeId {
        // Patterns nest without going through `parse_assignment`: guard them too.
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = match self.kind() {
            T::LBracket => self.parse_array_pattern(),
            T::LBrace => self.parse_object_pattern(),
            _ => self.parse_identifier(),
        };
        self.leave();
        r
    }

    fn parse_array_pattern(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let mark = self.scratch.len();
        loop {
            if self.aborting() {
                break;
            }
            match self.kind() {
                T::RBracket | T::Eof => break,
                T::Comma => {
                    let pos = self.start();
                    self.bump();
                    let h = self.add_node(Span::at(pos), Node::Hole {});
                    self.scratch.push(h);
                    continue;
                }
                T::DotDotDot => {
                    let s = self.start();
                    self.bump();
                    let arg = self.parse_binding_target();
                    let n = self.finish_node(s, Node::Rest { arg });
                    self.scratch.push(n);
                    self.check_rest_trailing_comma(T::RBracket);
                }
                // `[debugger, if]`: a reserved word ends the pattern (tsc: TS1181) rather
                // than becoming a missing name.
                k if k.is_reserved() => {
                    self.error(messages::ARRAY_ELEMENT_DESTRUCTURING_PATTERN_EXPECTED);
                    break;
                }
                _ => {
                    let s = self.start();
                    let left = self.parse_binding_target();
                    let n = if self.eat(T::Eq) {
                        let right = self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_assignment());
                        self.finish_node(s, Node::AssignPattern { left, right })
                    } else {
                        left
                    };
                    self.scratch.push(n);
                }
            }
            if !self.eat(T::Comma) {
                if !self.at(T::RBracket) {
                    self.error_expected(",");
                }
                break;
            }
        }
        let elements = self.finish_list(mark);
        self.expect_closing(T::RBracket, T::LBracket, open);
        self.finish_node(start, Node::ArrayPattern { elements })
    }

    fn parse_object_pattern(&mut self) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let mark = self.scratch.len();
        while !matches!(self.kind(), T::RBrace | T::Eof) && !self.aborting() {
            let s = self.start();
            if self.eat(T::DotDotDot) {
                let arg = self.parse_binding_target();
                let n = self.finish_node(s, Node::Rest { arg });
                self.scratch.push(n);
                self.check_rest_trailing_comma(T::RBrace);
            } else {
                let key_kind = self.kind();
                let key = self.parse_property_key();
                let (value, init) = if self.eat(T::Colon) {
                    let v = self.parse_binding_target();
                    let init = if self.eat(T::Eq) {
                        Some(self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_assignment()))
                    } else {
                        None
                    };
                    (Some(v), init)
                } else {
                    // Keywords and literals cannot be shorthand: `{ return x }` is reported
                    // as a missing `:` and the target is still parsed. Without a target the
                    // name is missing (tsc declares it as `(Missing)`).
                    let mut value = None;
                    if !key_kind.is_identifier_like() {
                        self.error_expected(":");
                        value = Some(
                            if self.at_identifier()
                                || matches!(self.kind(), T::LBrace | T::LBracket)
                            {
                                self.parse_binding_target()
                            } else {
                                self.missing()
                            },
                        );
                    }
                    let init = if self.eat(T::Eq) {
                        Some(self.with_ctx(0, Ctx::DISALLOW_IN, |p| p.parse_assignment()))
                    } else {
                        None
                    };
                    (value, init)
                };
                let n = self.finish_node(s, Node::BindingProperty { key, value, init });
                self.scratch.push(n);
            }
            if !self.eat(T::Comma) {
                if self.at(T::RBrace) {
                    break;
                }
                self.error_expected(",");
                // As in tsc, the list goes on at anything that starts a binding property
                // (`{ return 'thing' }` is two of them), and skips a `;`.
                if self.at(T::Semi) {
                    self.bump();
                    continue;
                }
                if !(self.kind().is_identifier_or_keyword()
                    || matches!(
                        self.kind(),
                        T::String | T::Number | T::BigInt | T::LBracket | T::DotDotDot
                    ))
                {
                    break;
                }
            }
        }
        let props = self.finish_list(mark);
        self.expect_closing(T::RBrace, T::LBrace, open);
        self.finish_node(start, Node::ObjectPattern { props })
    }
}
