//! JSX elements, fragments, attributes and children. The parser drives the lexer's JSX
//! modes: children are scanned with `next_jsx_child`, attribute strings with
//! `next_jsx_attribute_value`, and dashed names with `rescan_jsx_identifier`.

use ts_r_ast::{Node, NodeId, NodeList, Span};
use ts_r_diagnostics::messages;
use ts_r_lexer::TokenKind as T;

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
        let start = self.start();
        self.bump(); // `<`
        if self.at(T::Gt) {
            // Fragment.
            self.bump_jsx_child();
            let children = self.parse_jsx_children();
            // At `<` of `</>`.
            if self.at(T::Lt) {
                self.bump();
                self.expect(T::Slash);
                if !self.at(T::Gt) {
                    self.error(messages::JSX_EXPECTED_CLOSING_FRAGMENT);
                    while !matches!(self.kind(), T::Gt | T::Eof) {
                        self.bump();
                    }
                }
                self.eat_tag_end(in_child);
            } else {
                self.error(messages::JSX_FRAGMENT_NO_CLOSING);
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
        let ok = self.eat_tag_end(true);
        let opening = self.finish_node(
            start,
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                self_closing: false,
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
        let children = self.parse_jsx_children();
        let mut closing = None;
        if self.at(T::Lt) {
            let cstart = self.start();
            self.bump();
            self.expect(T::Slash);
            let cname = if self.at(T::Gt) {
                self.error(messages::IDENTIFIER_EXPECTED);
                self.missing()
            } else {
                self.parse_jsx_tag_name()
            };
            let open_text = self.ast.span(name).text(self.src);
            let close_text = self.ast.span(cname).text(self.src);
            if !jsx_names_equal(open_text, close_text) {
                let span = self.ast.span(cname);
                self.error_at(messages::JSX_EXPECTED_CLOSING_TAG, span, &[open_text]);
            }
            self.eat_tag_end(in_child);
            closing = Some(self.finish_node(cstart, Node::JsxClosing { name: cname }));
        } else {
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

    /// Children until `</` or end of file. Leaves the parser at the `<` of the closing tag.
    fn parse_jsx_children(&mut self) -> NodeList {
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
                let prop = self.parse_jsx_identifier();
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
            let prop = self.parse_jsx_identifier();
            obj = self.finish_node(start, Node::JsxMemberExpr { obj, prop });
        }
        obj
    }

    /// An identifier or keyword extended with `-` characters.
    fn parse_jsx_identifier(&mut self) -> NodeId {
        if !self.kind().is_identifier_or_keyword() {
            self.error(messages::IDENTIFIER_EXPECTED);
            return self.missing();
        }
        self.lx.rescan_jsx_identifier();
        let start = self.start();
        let name = self.intern_token();
        self.bump();
        self.finish_node(start, Node::Ident { name })
    }

    fn parse_jsx_attributes(&mut self) -> NodeList {
        let mark = self.scratch.len();
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
                        Some(self.parse_jsx_attribute_value())
                    } else {
                        None
                    };
                    let n = self.finish_node(start, Node::JsxAttr { name, value });
                    self.scratch.push(n);
                }
                _ => {
                    self.error(messages::IDENTIFIER_EXPECTED);
                    // Skip the stray token unless it ends the tag.
                    self.bump();
                }
            }
        }
        self.finish_list(mark)
    }

    fn parse_jsx_attribute_value(&mut self) -> NodeId {
        let start = self.start();
        match self.kind() {
            T::String => {
                let tok = self.tok();
                let inner = Span::new(tok.start + 1, tok.end.saturating_sub(1).max(tok.start + 1));
                let value = self.ast.intern(inner.text(self.src));
                self.bump();
                self.finish_node(start, Node::StringLit { value })
            }
            T::LBrace => self.parse_jsx_expression_container(false),
            T::Lt => self.parse_jsx_element_or_fragment(false),
            _ => {
                self.error(messages::EXPRESSION_EXPECTED);
                self.missing()
            }
        }
    }
}

/// Compares tag names ignoring whitespace (`a . b` vs `a.b`).
fn jsx_names_equal(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    a == b || strip(a) == strip(b)
}
