//! List contexts and tsc's list recovery.
//!
//! Every list the parser is inside (statements, class members, arguments, parameters, ...)
//! is a bit in `Parser::lists`. When a list meets a token that can neither start one of its
//! elements nor end it, tsc 7 (observed on the upstream conformance baselines):
//!
//! 1. reports the list's error at the token (`Declaration or statement expected.`,
//!    `Argument expression expected.`, ...; dropped when an error is already there), then
//! 2. ends the list without consuming the token if any list it is inside could use the token
//!    (as an element or as its terminator), and otherwise skips the token and goes on.
//!
//! So `constructor() { private x = 1; }` ends the constructor body at `private` (a class
//! member can start there) and parses `private x = 1;` as a property, and `f(function() { ) })`
//! ends the function body at `)`, which ends the argument list.
//!
//! Delimited lists (arguments, parameters, ...) also report `',' expected` when an element is
//! not followed by `,` or a terminator, and go on with the next element.

use ts_r_ast::{NodeId, NodeList};
use ts_r_diagnostics::{DiagnosticMessage, messages};
use ts_r_lexer::TokenKind as T;

use crate::Parser;
use crate::modifiers::MAX_MODIFIER_CHAIN;

/// A list context. The discriminant is the bit in `Parser::lists`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum List {
    SourceElements,
    BlockStatements,
    SwitchClauses,
    SwitchClauseStatements,
    TypeMembers,
    ClassMembers,
    EnumMembers,
    HeritageClauseElement,
    VariableDeclarations,
    ObjectBindingElements,
    ArrayBindingElements,
    ArgumentExpressions,
    ObjectLiteralMembers,
    JsxAttributes,
    JsxChildren,
    ArrayLiteralMembers,
    Parameters,
    TypeParameters,
    TypeArguments,
    TupleElementTypes,
    HeritageClauses,
    ImportOrExportSpecifiers,
    ImportAttributes,
}

const ALL_LISTS: [List; 23] = [
    List::SourceElements,
    List::BlockStatements,
    List::SwitchClauses,
    List::SwitchClauseStatements,
    List::TypeMembers,
    List::ClassMembers,
    List::EnumMembers,
    List::HeritageClauseElement,
    List::VariableDeclarations,
    List::ObjectBindingElements,
    List::ArrayBindingElements,
    List::ArgumentExpressions,
    List::ObjectLiteralMembers,
    List::JsxAttributes,
    List::JsxChildren,
    List::ArrayLiteralMembers,
    List::Parameters,
    List::TypeParameters,
    List::TypeArguments,
    List::TupleElementTypes,
    List::HeritageClauses,
    List::ImportOrExportSpecifiers,
    List::ImportAttributes,
];

impl List {
    #[inline]
    pub(crate) fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl<'a> Parser<'a> {
    /// Runs `f` inside list context `l`.
    pub(crate) fn in_list<R>(&mut self, l: List, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.lists;
        self.lists |= l.bit();
        let r = f(self);
        self.lists = saved;
        r
    }

    /// Runs `f` outside every list context (a construct that starts afresh, such as a
    /// JSX expression container's contents being re-entered from the top).
    #[allow(dead_code)]
    pub(crate) fn outside_lists<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.lists;
        self.lists = 0;
        let r = f(self);
        self.lists = saved;
        r
    }

    /// Does the current token end list `l`?
    pub(crate) fn is_list_terminator(&mut self, l: List) -> bool {
        let k = self.kind();
        if k == T::Eof {
            return true;
        }
        match l {
            List::SourceElements => false,
            List::BlockStatements
            | List::SwitchClauses
            | List::TypeMembers
            | List::ClassMembers
            | List::EnumMembers
            | List::ObjectLiteralMembers
            | List::ObjectBindingElements
            | List::ImportOrExportSpecifiers
            | List::ImportAttributes => k == T::RBrace,
            List::SwitchClauseStatements => matches!(k, T::RBrace | T::Case | T::Default),
            List::HeritageClauseElement => matches!(k, T::LBrace | T::Extends | T::Implements),
            List::VariableDeclarations => {
                self.can_insert_semicolon() || matches!(k, T::In | T::Of | T::Arrow)
            }
            List::TypeParameters => {
                matches!(
                    k,
                    T::Gt | T::LParen | T::LBrace | T::Extends | T::Implements
                )
            }
            List::ArgumentExpressions => matches!(k, T::RParen | T::Semi),
            List::ArrayLiteralMembers | List::TupleElementTypes | List::ArrayBindingElements => {
                k == T::RBracket
            }
            List::Parameters => matches!(k, T::RParen | T::RBracket),
            // Anything but `,` ends a type argument list.
            List::TypeArguments => k != T::Comma,
            List::HeritageClauses => matches!(k, T::LBrace | T::RBrace),
            List::JsxAttributes => matches!(k, T::Gt | T::Slash),
            List::JsxChildren => k == T::Lt && self.peek_kind() == T::Slash,
        }
    }

    /// Can the current token start an element of list `l`? `recovery`: asked for an
    /// enclosing list while another list recovers (a little stricter).
    pub(crate) fn is_list_element(&mut self, l: List, recovery: bool) -> bool {
        let k = self.kind();
        match l {
            List::SourceElements | List::BlockStatements | List::SwitchClauseStatements => {
                !(recovery && k == T::Semi) && self.is_start_of_statement()
            }
            List::SwitchClauses => matches!(k, T::Case | T::Default),
            List::TypeMembers => self.is_type_member_start(),
            List::ClassMembers => (k == T::Semi && !recovery) || self.is_class_member_start(),
            List::EnumMembers => k == T::LBracket || self.at_literal_property_name(),
            List::HeritageClauseElement => {
                if k == T::LBrace {
                    return false;
                }
                if recovery {
                    k.is_identifier_like() && !matches!(k, T::Extends | T::Implements)
                } else {
                    self.at_lhs_expression_start()
                }
            }
            List::VariableDeclarations => self.at_binding_identifier_or_pattern(),
            List::ObjectBindingElements => {
                matches!(k, T::LBracket | T::DotDotDot) || self.at_literal_property_name()
            }
            List::ArrayBindingElements => {
                matches!(k, T::Comma | T::DotDotDot) || self.at_binding_identifier_or_pattern()
            }
            List::ArgumentExpressions => k == T::DotDotDot || self.is_start_of_expression(),
            List::ObjectLiteralMembers => {
                matches!(
                    k,
                    T::LBracket | T::Star | T::DotDotDot | T::Dot | T::PrivateName
                ) || self.at_literal_property_name()
            }
            List::ArrayLiteralMembers => {
                matches!(k, T::Comma | T::Dot | T::DotDotDot) || self.is_start_of_expression()
            }
            List::Parameters => self.is_start_of_parameter(),
            List::TypeParameters => matches!(k, T::In | T::Const) || self.at_identifier(),
            List::TypeArguments | List::TupleElementTypes => {
                k == T::Comma || self.is_start_of_type(false)
            }
            List::HeritageClauses => matches!(k, T::Extends | T::Implements),
            List::ImportOrExportSpecifiers => {
                if k == T::From && self.peek_kind() == T::String {
                    return false;
                }
                k == T::String || k.is_identifier_or_keyword()
            }
            List::ImportAttributes => k == T::String || k.is_identifier_or_keyword(),
            List::JsxAttributes => k == T::LBrace || k.is_identifier_or_keyword(),
            List::JsxChildren => true,
        }
    }

    /// The error tsc reports at a token list `l` cannot use.
    fn list_error(&self, l: List) -> (DiagnosticMessage, bool) {
        // The bool: the message takes the token text when the token is a keyword.
        match l {
            List::SourceElements | List::BlockStatements => {
                (messages::DECLARATION_OR_STATEMENT_EXPECTED, false)
            }
            List::SwitchClauses => (messages::CASE_OR_DEFAULT_EXPECTED, false),
            List::SwitchClauseStatements => (messages::STATEMENT_EXPECTED, false),
            List::TypeMembers => (messages::PROPERTY_OR_SIGNATURE_EXPECTED, false),
            List::ClassMembers => (messages::UNEXPECTED_TOKEN_CLASS_MEMBER, false),
            List::EnumMembers => (messages::ENUM_MEMBER_EXPECTED, false),
            List::HeritageClauseElement => (messages::EXPRESSION_EXPECTED, false),
            List::VariableDeclarations => (messages::VARIABLE_DECLARATION_EXPECTED, true),
            List::ObjectBindingElements => {
                (messages::PROPERTY_DESTRUCTURING_PATTERN_EXPECTED, false)
            }
            List::ArrayBindingElements => (
                messages::ARRAY_ELEMENT_DESTRUCTURING_PATTERN_EXPECTED,
                false,
            ),
            List::ArgumentExpressions => (messages::ARGUMENT_EXPRESSION_EXPECTED, false),
            List::ObjectLiteralMembers => (messages::PROPERTY_ASSIGNMENT_EXPECTED, false),
            List::ArrayLiteralMembers => (messages::EXPRESSION_OR_COMMA_EXPECTED, false),
            List::Parameters => (messages::PARAMETER_DECLARATION_EXPECTED, true),
            List::TypeParameters => (messages::TYPE_PARAMETER_DECLARATION_EXPECTED, false),
            List::TypeArguments => (messages::TYPE_ARGUMENT_EXPECTED, false),
            List::TupleElementTypes => (messages::TYPE_EXPECTED, false),
            List::HeritageClauses => (messages::UNEXPECTED_TOKEN, false),
            List::ImportOrExportSpecifiers | List::JsxAttributes | List::JsxChildren => {
                (messages::IDENTIFIER_EXPECTED, false)
            }
            List::ImportAttributes => (messages::IDENTIFIER_OR_STRING_LITERAL_EXPECTED, false),
        }
    }

    /// Reports list `l`'s error at the current token. Returns true if a list the parser is
    /// inside can use the token (the caller ends its list without consuming it); otherwise
    /// skips the token and returns false.
    pub(crate) fn abort_list_or_skip(&mut self, l: List) -> bool {
        let (mut msg, keyword_text) = self.list_error(l);
        // `{ a: 0 } = value;`, `class A { m() {} = 1 }`: TS2809 in any list (tsc 7).
        if self.at_equals_after_block() {
            msg = messages::EQUALS_AFTER_BLOCK;
        }
        let k = self.kind();
        if keyword_text && k.is_reserved() {
            let text = self.lx.ident_value(self.tok());
            let msg = if l == List::Parameters {
                messages::NOT_ALLOWED_AS_PARAMETER_NAME
            } else {
                messages::NOT_ALLOWED_AS_VARIABLE_NAME
            };
            let span = self.error_span();
            self.error_at(msg, span, &[&text]);
        } else {
            self.error(msg);
        }
        if self.in_some_list_using_token() {
            return true;
        }
        self.bump();
        false
    }

    /// tsc's delimited list `l`: `element` parses one element where one starts; elements are
    /// separated by `,`. A missing `,` between elements is `',' expected` and the list goes
    /// on; any other token is handled by [`Self::abort_list_or_skip`]. Returns the elements
    /// and the start of a trailing `,`, if any.
    pub(crate) fn parse_delimited(
        &mut self,
        l: List,
        mut element: impl FnMut(&mut Self) -> NodeId,
    ) -> (NodeList, Option<u32>) {
        let mark = self.scratch.len();
        let mut trailing = None;
        self.in_list(l, |p| {
            while !p.aborting() {
                if p.is_list_element(l, false) {
                    let before = p.tok().start;
                    let e = element(p);
                    p.scratch.push(e);
                    trailing = None;
                    if p.at(T::Comma) {
                        trailing = Some(p.start());
                        p.bump();
                        continue;
                    }
                    if p.is_list_terminator(l) {
                        break;
                    }
                    p.error_expected(",");
                    // In an object literal a `;` on the same line stands for the `,`. The
                    // element must have consumed something.
                    let semi =
                        l == List::ObjectLiteralMembers && p.at(T::Semi) && !p.newline_before();
                    if semi || p.tok().start == before {
                        p.bump();
                    }
                    continue;
                }
                if p.is_list_terminator(l) || p.abort_list_or_skip(l) {
                    break;
                }
            }
        });
        (self.finish_list(mark), trailing)
    }

    /// Is the current token an element start or a terminator of any list we are inside?
    fn in_some_list_using_token(&mut self) -> bool {
        let lists = self.lists;
        for l in ALL_LISTS {
            if lists & l.bit() != 0 && (self.is_list_element(l, true) || self.is_list_terminator(l))
            {
                return true;
            }
        }
        false
    }

    // ---- Element-start predicates ----

    /// Identifier, keyword, string, number or bigint: a property name that is not computed.
    pub(crate) fn at_literal_property_name(&self) -> bool {
        matches!(self.kind(), T::String | T::Number | T::BigInt)
            || self.kind().is_identifier_or_keyword()
    }

    /// A binding name or pattern start: an identifier (`await` and `yield` included, which
    /// are then TS1359 where they are operators), `{`, `[` or a private name.
    pub(crate) fn at_binding_identifier_or_pattern(&self) -> bool {
        matches!(self.kind(), T::LBrace | T::LBracket | T::PrivateName)
            || self.kind().is_identifier_like()
    }

    /// Can a statement start here? Mirrors the cases `parse_statement` handles without
    /// reporting "Declaration or statement expected".
    pub(crate) fn is_start_of_statement(&mut self) -> bool {
        match self.kind() {
            T::Eof | T::RBrace | T::RParen | T::RBracket => false,
            // `default function () {}` is a declaration missing its `export`.
            T::Default => matches!(self.peek_kind(), T::Function | T::Class | T::Abstract),
            T::Else | T::Case => false,
            T::Catch | T::Finally => true,
            T::At
            | T::Semi
            | T::LBrace
            | T::Var
            | T::Function
            | T::Class
            | T::If
            | T::For
            | T::While
            | T::Do
            | T::Return
            | T::Break
            | T::Continue
            | T::Throw
            | T::Try
            | T::Switch
            | T::With
            | T::Debugger
            | T::Enum
            | T::Const => true,
            // `export;` starts nothing (`var export;` skips the `export`).
            T::Export => {
                !matches!(
                    self.peek_kind(),
                    T::Semi | T::RBrace | T::RParen | T::Comma | T::Eof
                ) && !self.is_stray_export()
            }
            T::Import => self.is_import_statement_start(),
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
                true
            }
            _ if self.is_stray_statement_modifier() => false,
            _ => self.is_start_of_expression(),
        }
    }

    /// `import` starts a statement unless what follows can neither continue an import
    /// declaration nor an `import(...)`/`import.meta` expression (`import 10;`).
    fn is_import_statement_start(&mut self) -> bool {
        let next = self.peek_kind();
        matches!(
            next,
            T::LParen | T::Dot | T::LBrace | T::Star | T::String | T::Lt
        ) || next.is_identifier_or_keyword()
    }

    /// Can an expression start here? As in tsc, a binary operator counts (the left operand
    /// is then missing: `* b` is `Expression expected.` at `*`).
    pub(crate) fn is_start_of_expression(&mut self) -> bool {
        if self.at_expression_start() {
            return true;
        }
        matches!(self.kind(), T::Await | T::Yield | T::Hash) || self.at_binary_operator()
    }

    fn at_binary_operator(&self) -> bool {
        matches!(
            self.kind(),
            T::QuestionQuestion
                | T::PipePipe
                | T::AmpAmp
                | T::Pipe
                | T::Caret
                | T::Amp
                | T::EqEq
                | T::NotEq
                | T::EqEqEq
                | T::NotEqEq
                | T::Lt
                | T::Gt
                | T::LtEq
                | T::GtEq
                | T::InstanceOf
                | T::In
                | T::LtLt
                | T::GtGt
                | T::GtGtGt
                | T::Plus
                | T::Minus
                | T::Star
                | T::Slash
                | T::Percent
                | T::StarStar
                | T::As
                | T::Satisfies
        )
    }

    /// The start of a left-hand-side expression (for heritage clauses).
    fn at_lhs_expression_start(&self) -> bool {
        matches!(
            self.kind(),
            T::This
                | T::Super
                | T::Null
                | T::True
                | T::False
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
                | T::Slash
                | T::SlashEq
                | T::Import
                | T::Lt
                | T::PrivateName
        ) || self.kind().is_identifier_like()
    }

    /// A class member can start here (lookahead).
    pub(crate) fn is_class_member_start(&mut self) -> bool {
        if self.at(T::At) {
            return true;
        }
        self.lookahead(|p| {
            let mut name: Option<T> = None;
            for _ in 0..MAX_MODIFIER_CHAIN {
                match p.kind() {
                    // Class member modifiers start a member whatever follows.
                    T::Public
                    | T::Private
                    | T::Protected
                    | T::Static
                    | T::Readonly
                    | T::Accessor
                    | T::Override
                    | T::Abstract => return true,
                    T::Async | T::Declare | T::Export | T::Const | T::In | T::Out => {
                        name = Some(p.kind());
                        p.bump();
                    }
                    _ => break,
                }
            }
            if p.at(T::Star) {
                return true;
            }
            if p.at_literal_property_name() || p.at(T::PrivateName) {
                name = Some(p.kind());
                p.bump();
            }
            if p.at(T::LBracket) {
                return true;
            }
            let Some(name) = name else {
                return false;
            };
            if !name.is_keyword() || matches!(name, T::Get | T::Set) {
                return true;
            }
            matches!(
                p.kind(),
                T::LParen | T::Lt | T::Bang | T::Colon | T::Eq | T::Question
            ) || p.can_insert_semicolon()
        })
    }

    /// An interface or type literal member can start here (lookahead).
    pub(crate) fn is_type_member_start(&mut self) -> bool {
        if matches!(self.kind(), T::LParen | T::Lt | T::Get | T::Set) {
            return true;
        }
        self.lookahead(|p| {
            let mut modifier = false;
            for _ in 0..MAX_MODIFIER_CHAIN {
                if matches!(
                    p.kind(),
                    T::Public
                        | T::Private
                        | T::Protected
                        | T::Static
                        | T::Readonly
                        | T::Abstract
                        | T::Declare
                        | T::Export
                        | T::Async
                        | T::Override
                        | T::Accessor
                ) && {
                    // A modifier only when a name follows (`async?: boolean` is a name).
                    let next = p.peek_kind();
                    matches!(
                        next,
                        T::LBracket | T::PrivateName | T::String | T::Number | T::BigInt
                    ) || next.is_identifier_or_keyword()
                } {
                    modifier = true;
                    p.bump();
                } else {
                    break;
                }
            }
            if p.at(T::LBracket) {
                return true;
            }
            if p.at_literal_property_name() || p.at(T::PrivateName) {
                p.bump();
                return matches!(
                    p.kind(),
                    T::LParen | T::Lt | T::Question | T::Colon | T::Comma
                ) || p.can_insert_semicolon();
            }
            modifier && p.can_insert_semicolon()
        })
    }

    /// A parameter can start here: `...`, a binding name or pattern, a modifier, `@`,
    /// `this`, or (for error recovery) anything that starts a type.
    pub(crate) fn is_start_of_parameter(&mut self) -> bool {
        matches!(
            self.kind(),
            T::DotDotDot
                | T::At
                | T::This
                | T::Public
                | T::Private
                | T::Protected
                | T::Readonly
                | T::Override
                | T::Static
                | T::Abstract
                | T::Declare
                | T::Async
                | T::Export
                | T::Accessor
                | T::In
                | T::Out
                | T::Const
        ) || self.at_binding_identifier_or_pattern()
            || self.is_start_of_type(true)
    }

    /// Can a type start here? `in_parameter`: asked for a parameter, where `(`, `-` and
    /// `function` do not count.
    pub(crate) fn is_start_of_type(&mut self, in_parameter: bool) -> bool {
        match self.kind() {
            T::Any
            | T::UnknownKw
            | T::StringKw
            | T::NumberKw
            | T::BigIntKw
            | T::Boolean
            | T::Readonly
            | T::SymbolKw
            | T::Unique
            | T::Void
            | T::Undefined
            | T::Null
            | T::This
            | T::TypeOf
            | T::Never
            | T::LBrace
            | T::LBracket
            | T::Lt
            | T::Pipe
            | T::Amp
            | T::New
            | T::String
            | T::Number
            | T::BigInt
            | T::True
            | T::False
            | T::ObjectKw
            | T::Star
            | T::Question
            | T::Bang
            | T::DotDotDot
            | T::Infer
            | T::Import
            | T::Asserts
            | T::NoSubstTemplate
            | T::TemplateHead
            | T::KeyOf => true,
            T::Function => !in_parameter,
            T::Minus => !in_parameter && matches!(self.peek_kind(), T::Number | T::BigInt),
            T::LParen => !in_parameter,
            _ => self.at_identifier(),
        }
    }
}
