//! JSX elements, fragments, attributes and children. The parser drives the lexer's JSX
//! modes: children are scanned with `next_jsx_child`, attribute strings with
//! `next_jsx_attribute_value`, and dashed names with `rescan_jsx_identifier`.

use ts_r_ast::{Node, NodeId, NodeList, Span};
use ts_r_diagnostics::messages;
use ts_r_lexer::{TokenFlags, TokenKind as T};

use crate::lists::List;
use crate::{Ctx, Parser};

impl<'a> Parser<'a> {
    /// Consumes the current token and scans the next one as JSX child content.
    fn bump_jsx_child(&mut self) {
        self.prev_end = self.tok().end;
        self.lx.next_jsx_child();
    }

    /// Consumes the `>` that ends a tag: children follow (`child_mode`) or normal code.
    fn eat_tag_end(&mut self, child_mode: bool) -> bool {
        if !self.at(T::Gt) {
            self.error_expected(">");
            return false;
        }
        if child_mode {
            self.bump_jsx_child()
        } else {
            self.bump()
        }
        true
    }

    /// At `<`: an element or fragment. `in_child` is true when this element is a child of
    /// another, so the token after its end is scanned in JSX child mode.
    pub(crate) fn parse_jsx_element_or_fragment(&mut self, in_child: bool) -> NodeId {
        let operand = self.unary_operand_start == self.start();
        let first = self.parse_jsx_single(in_child);
        if in_child || operand || !self.at(T::Lt) {
            return first;
        }
        // `<a/><b/>` outside children: TS2657 over all the adjacent elements, which become a
        // comma sequence (as in tsc 7). It starts with a zero-width `Missing` standing for
        // tsc's synthesized comma: the checker reports no unused left side (TS2695) for it.
        let start = self.ast.span(first).start;
        let mark = self.scratch.len();
        let comma = self.add_node(Span::at(start), Node::Missing {});
        self.scratch.push(comma);
        self.scratch.push(first);
        while self.at(T::Lt) && !self.aborting() {
            let before = self.start();
            let e = self.parse_jsx_single(false);
            self.scratch.push(e);
            if self.start() == before {
                break;
            }
        }
        let span = Span::new(start, self.prev_end);
        self.error_at(messages::JSX_ONE_PARENT, span, &[]);
        let exprs = self.finish_list(mark);
        self.finish_node(start, Node::Seq { exprs })
    }

    fn parse_jsx_single(&mut self, in_child: bool) -> NodeId {
        if !self.enter() {
            self.leave();
            return self.missing();
        }
        let r = self.with_ctx(
            0,
            Ctx::DISALLOW_IN | Ctx::IN_CONDITIONAL_TRUE | Ctx::DECORATOR | Ctx::TYPE,
            |p| p.parse_jsx_inner(in_child),
        );
        self.leave();
        r
    }

    fn parse_jsx_inner(&mut self, in_child: bool) -> NodeId {
        let parent_name = self.jsx_parent_name.take();
        let start = self.start();
        let full_start = self.prev_end.min(start);
        self.bump(); // `<`
        if self.at(T::Gt) {
            // Fragment.
            let open_end = self.tok().end;
            self.bump_jsx_child();
            let children = self.parse_jsx_children(None);
            // At `<` of `</>`.
            if self.at(T::Lt) {
                self.bump();
                self.expect(T::Slash);
                if self.at(T::Gt) {
                    self.eat_tag_end(in_child);
                } else {
                    // `<>x</a>`: the fragment ends after `</`, and what follows is read
                    // again by the parent: as JSX text in children (`a>` is text with
                    // TS1382 at the `>`), as code otherwise (as in tsc 7).
                    self.error(messages::JSX_EXPECTED_CLOSING_FRAGMENT);
                    if in_child && !self.at(T::Eof) {
                        self.lx.seek(self.prev_end);
                        self.lx.next_jsx_child();
                    }
                }
            } else {
                // tsc 7 reports it from the end of the previous token to the `<>`.
                self.closing_tag_expected_at_eof();
                let span = Span::new(full_start, open_end);
                self.error_at(messages::JSX_FRAGMENT_NO_CLOSING, span, &[]);
            }
            return self.finish_node(start, Node::JsxFragment { children });
        }
        let name = self.parse_jsx_tag_name();
        let type_args = if self.ts && !self.js_ts && self.at(T::Lt) {
            Some(self.parse_type_args())
        } else {
            None
        };
        let attrs = self.parse_jsx_attributes();
        if self.at(T::Slash) {
            self.bump();
            self.eat_tag_end(in_child);
            let opening = self.finish_node(
                start,
                Node::JsxOpening {
                    name,
                    type_args,
                    attrs,
                    self_closing: true,
                },
            );
            return self.finish_node(
                start,
                Node::JsxElement {
                    opening,
                    children: NodeList::EMPTY,
                    closing: None,
                },
            );
        }
        // Neither `>` nor `/>`: tsc 7 expects the `/` of a self-closing tag (`<div ;`).
        let ok = self.at(T::Gt);
        if ok {
            self.bump_jsx_child();
        } else {
            self.error_expected("/");
        }
        if !ok && in_child && !self.at(T::Eof) {
            // The element ends before the token that is not `>`, which the parent reads
            // again as JSX text (`<1234> x` in children is text `1234> x`, TS1382 at `>`).
            self.lx.seek(self.prev_end);
            self.lx.next_jsx_child();
        }
        // Without the `>` the tag is read as self-closing, as tsc does (`<a b="c;` prints
        // `<a b="c;/>`).
        let opening = self.finish_node(
            start,
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                self_closing: !ok,
            },
        );
        if !ok {
            return self.finish_node(
                start,
                Node::JsxElement {
                    opening,
                    children: NodeList::EMPTY,
                    closing: None,
                },
            );
        }
        let children = self.parse_jsx_children(Some(name));
        let mut closing = None;
        if self.at(T::Lt) {
            let cstart = self.start();
            let cp = self.checkpoint();
            self.bump();
            self.expect(T::Slash);
            let cname = if self.at(T::Gt) {
                self.error(messages::IDENTIFIER_EXPECTED);
                self.missing()
            } else {
                self.parse_jsx_tag_name()
            };
            let open_text = self.ast.span(name).text(self.src);
            let names_match = open_text == self.ast.span(cname).text(self.src)
                || self.jsx_tag_key(name) == self.jsx_tag_key(cname);
            if !names_match
                && parent_name.is_some_and(|n| self.jsx_tag_key(n) == self.jsx_tag_key(cname))
            {
                // `<a><b>x</a>`: the closing tag is the parent's; this element is unclosed
                // (as in tsc 7, only for the direct parent).
                self.rewind(cp);
                let span = self.ast.span(name);
                self.error_at(messages::JSX_NO_CLOSING_TAG, span, &[open_text]);
            } else {
                if !names_match {
                    let span = self.ast.span(cname);
                    self.error_at(messages::JSX_EXPECTED_CLOSING_TAG, span, &[open_text]);
                }
                self.eat_tag_end(in_child);
                closing = Some(self.finish_node(cstart, Node::JsxClosing { name: cname }));
            }
        } else if self.at(T::Unknown) && self.src[self.start() as usize..].starts_with("<<<<<<<") {
            // Children that run into a conflict marker (conflictMarkerTrivia3): tsc 7 reports
            // `'</' expected.` from the end of the last non-blank child to the end of the
            // marker line, and nothing about the unclosed element.
            let tok = self.tok();
            let before = self.src[..tok.start as usize].trim_end();
            let span = Span::new(before.len() as u32, tok.end);
            self.error_at(messages::EXPECTED, span, &["</"]);
            self.bump();
        } else {
            self.closing_tag_expected_at_eof();
            let text = self.ast.span(name).text(self.src);
            let span = self.ast.span(name);
            self.error_at(messages::JSX_NO_CLOSING_TAG, span, &[text]);
        }
        self.finish_node(
            start,
            Node::JsxElement {
                opening,
                children,
                closing,
            },
        )
    }

    /// Children that run to the end of the file: `'</' expected.` there, once for all the
    /// unclosed elements.
    fn closing_tag_expected_at_eof(&mut self) {
        let eof = self.start();
        if self.at(T::Eof) && !self.diags.iter().rev().any(|d| d.span.start == eof) {
            self.error_expected("</");
        }
    }

    /// Children until `</` or end of file. Leaves the parser at the `<` of the closing tag.
    fn parse_jsx_children(&mut self, parent_name: Option<NodeId>) -> NodeList {
        let mark = self.scratch.len();
        loop {
            if self.aborting() {
                break;
            }
            match self.kind() {
                T::JsxText => {
                    let span = self.tok().span();
                    let n = self.add_node(span, Node::JsxText {});
                    self.scratch.push(n);
                    self.bump_jsx_child();
                }
                T::LBrace => {
                    let n = self.parse_jsx_expression_container(true);
                    self.scratch.push(n);
                }
                T::Lt => {
                    if self.peek_kind() == T::Slash {
                        break;
                    }
                    self.jsx_parent_name = parent_name;
                    let n = self.parse_jsx_element_or_fragment(true);
                    self.scratch.push(n);
                }
                _ => break,
            }
        }
        self.finish_list(mark)
    }

    /// `{expr}`, `{...expr}` or `{}` at `{`. In children, the token after `}` is scanned in
    /// child mode.
    fn parse_jsx_expression_container(&mut self, in_children: bool) -> NodeId {
        let start = self.start();
        let open = self.start();
        self.bump();
        let mut spread = false;
        let expr = if self.at(T::RBrace) {
            None
        } else if in_children && self.at(T::DotDotDot) {
            self.bump();
            spread = true;
            Some(self.parse_expression())
        } else {
            Some(self.parse_expression())
        };
        // `{a, b}`: TS18007 (a grammar error in tsc 7). The tree keeps the `Seq`.
        if let Some(e) = expr
            && let Node::Seq { .. } = self.ast.node(e)
        {
            let span = self.ast.span(e);
            self.grammar_error_at(messages::JSX_COMMA_EXPRESSION, span, &[]);
        }
        if self.at(T::RBrace) {
            if in_children {
                self.bump_jsx_child()
            } else {
                self.bump()
            }
        } else if in_children {
            // As tsc: the container ends here and the children go on from the token after
            // the expression, read again as JSX text (`<a>{"s";}</a>` has the text `;}`,
            // TS1381 at its `}`).
            self.expect_closing(T::RBrace, T::LBrace, open);
            if !self.at(T::Eof) {
                self.lx.seek(self.prev_end);
                self.lx.next_jsx_child();
            }
        } else {
            self.expect_closing(T::RBrace, T::LBrace, open);
            // Resynchronize: skip to the closing brace.
            while !matches!(self.kind(), T::RBrace | T::Eof | T::Lt) {
                self.bump();
            }
            if self.at(T::RBrace) {
                if in_children {
                    self.bump_jsx_child()
                } else {
                    self.bump()
                }
            }
        }
        match (spread, expr) {
            (true, Some(expr)) => self.finish_node(start, Node::JsxSpreadChild { expr }),
            (_, expr) => self.finish_node(start, Node::JsxExprContainer { expr }),
        }
    }

    /// `div`, `my-elem`, `ns:name`, `A.B.C`, `this.x`.
    fn parse_jsx_tag_name(&mut self) -> NodeId {
        let start = self.start();
        if self.at(T::This) && self.peek_kind() == T::Dot {
            self.bump();
            let mut obj = self.finish_node(start, Node::This {});
            while self.eat(T::Dot) {
                let prop = self.parse_jsx_member_name();
                obj = self.finish_node(start, Node::JsxMemberExpr { obj, prop });
            }
            return obj;
        }
        let first = self.parse_jsx_identifier();
        if self.at(T::Colon) {
            self.bump();
            let name = self.parse_jsx_identifier();
            return self.finish_node(start, Node::JsxNamespacedName { ns: first, name });
        }
        let mut obj = first;
        while self.eat(T::Dot) {
            let prop = self.parse_jsx_member_name();
            obj = self.finish_node(start, Node::JsxMemberExpr { obj, prop });
        }
        obj
    }

    /// The name after `.` in a tag name. A private name (`<this.#prop />`) is taken, and is
    /// TS1003 just after it, as in tsc 7 (privateNameJsx).
    fn parse_jsx_member_name(&mut self) -> NodeId {
        if self.at(T::PrivateName) {
            let name = self.parse_private_name();
            self.error_at(messages::IDENTIFIER_EXPECTED, Span::at(self.prev_end), &[]);
            return name;
        }
        self.parse_jsx_identifier()
    }

    /// An identifier or keyword extended with `-` characters.
    fn parse_jsx_identifier(&mut self) -> NodeId {
        if !self.kind().is_identifier_or_keyword() {
            self.error(messages::IDENTIFIER_EXPECTED);
            return self.missing();
        }
        self.lx.rescan_jsx_identifier();
        // tsc decodes escapes in JSX names (`<\u0061></a>` closes) but reports them, on the
        // whole name (parseIdentifierNameErrorOnUnicodeEscapeSequence).
        if self.tok().flags.has(TokenFlags::ESCAPED) {
            self.error(messages::JSX_NAME_UNICODE_ESCAPE);
        }
        let start = self.start();
        let name = self.intern_token();
        self.bump();
        self.finish_node(start, Node::Ident { name })
    }

    /// A tag name as tsc compares opening and closing tags: names decoded, no trivia
    /// (`a . b` is `a.b`). Member chains are walked without recursion.
    fn jsx_tag_key(&self, mut n: NodeId) -> String {
        let mut parts = Vec::new();
        while let Node::JsxMemberExpr { obj, prop } = *self.ast.node(n) {
            parts.push(prop);
            n = obj;
        }
        parts.push(n);
        let mut out = String::new();
        for (i, &p) in parts.iter().rev().enumerate() {
            if i > 0 {
                out.push('.');
            }
            match *self.ast.node(p) {
                Node::Ident { name } => out.push_str(self.ast.atom(name)),
                Node::This {} => out.push_str("this"),
                Node::JsxNamespacedName { ns, name } => {
                    for (j, part) in [ns, name].into_iter().enumerate() {
                        if j > 0 {
                            out.push(':');
                        }
                        match *self.ast.node(part) {
                            Node::Ident { name } => out.push_str(self.ast.atom(name)),
                            _ => out.push_str(self.ast.span(part).text(self.src)),
                        }
                    }
                }
                _ => out.push_str(self.ast.span(p).text(self.src)),
            }
        }
        out
    }

    fn parse_jsx_attributes(&mut self) -> NodeList {
        let mark = self.scratch.len();
        let mut empty_reported = false;
        loop {
            if self.aborting() {
                break;
            }
            let start = self.start();
            match self.kind() {
                T::Gt | T::Slash | T::Eof => break,
                T::LBrace => {
                    let open = self.start();
                    self.bump();
                    self.expect(T::DotDotDot);
                    let arg = self.parse_assignment();
                    self.expect_closing(T::RBrace, T::LBrace, open);
                    let n = self.finish_node(start, Node::JsxSpreadAttr { arg });
                    self.scratch.push(n);
                }
                k if k.is_identifier_or_keyword() => {
                    let first = self.parse_jsx_identifier();
                    let name = if self.at(T::Colon) {
                        self.bump();
                        let n = self.parse_jsx_identifier();
                        self.finish_node(start, Node::JsxNamespacedName { ns: first, name: n })
                    } else {
                        first
                    };
                    let value = if self.at(T::Eq) {
                        self.prev_end = self.tok().end;
                        self.lx.next_jsx_attribute_value();
                        self.parse_jsx_attribute_value()
                    } else {
                        None
                    };
                    // `attr={}`: a checker grammar error in tsc 7, on the braces, for the
                    // first such attribute of the element only.
                    if let Some(v) = value
                        && !empty_reported
                        && let Node::JsxExprContainer { expr: None } = self.ast.node(v)
                    {
                        empty_reported = true;
                        let span = self.ast.span(v);
                        self.deferred_grammar_error_at(
                            messages::JSX_EMPTY_ATTRIBUTE_EXPRESSION,
                            span,
                            &[],
                        );
                    }
                    let n = self.finish_node(start, Node::JsxAttr { name, value });
                    self.scratch.push(n);
                }
                // tsc's list recovery: `Identifier expected.`, then the list ends if an
                // enclosing list can use the token (`<1234> x` at a statement start; the tag
                // then lacks its `>`), else the token is skipped (`< {:>`).
                _ => {
                    if self.in_list(List::JsxAttributes, |p| {
                        p.abort_list_or_skip(List::JsxAttributes)
                    }) {
                        break;
                    }
                }
            }
        }
        self.finish_list(mark)
    }

    /// The value after `=`. None when it is missing (`<a b= />`, `<a b=c />`): the error,
    /// and, as in tsc, an attribute without a value (`<a b />`, `b: true`); the token is
    /// left for the attribute list.
    fn parse_jsx_attribute_value(&mut self) -> Option<NodeId> {
        let start = self.start();
        match self.kind() {
            T::String => {
                let tok = self.tok();
                let inner = Span::new(tok.start + 1, tok.end.saturating_sub(1).max(tok.start + 1));
                let value = self.ast.intern(inner.text(self.src));
                self.bump();
                Some(self.finish_node(start, Node::StringLit { value }))
            }
            T::LBrace => Some(self.parse_jsx_expression_container(false)),
            T::Lt => Some(self.parse_jsx_element_or_fragment(false)),
            _ => {
                self.error(messages::JSX_ATTRIBUTE_VALUE_EXPECTED);
                None
            }
        }
    }
}
