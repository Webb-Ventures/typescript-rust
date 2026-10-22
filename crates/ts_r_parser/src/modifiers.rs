//! Modifier keywords (`export`, `public`, `static`, `async`, ...) in every position.
//!
//! As in tsc 7, a modifier keyword is parsed as a modifier wherever one could start, even
//! where no modifier is allowed (`namespace N { public class A {} }`, `function f(static x)`,
//! `{ static m() {} }`), and the "cannot appear here" errors are left to the checker's
//! grammar pass. Every keyword taken as a modifier is recorded in `Ast::modifier_tokens`.
//!
//! The rules below were observed by running tsc 7.0.2 on small files:
//!
//! - Which keywords count depends on the position (`ModCtx`): `override`, `in`, `out` are
//!   not statement modifiers; `const` only modifies class members and type parameters;
//!   `export` does not modify type parameters; `default` only follows `export`.
//! - A keyword is a modifier only if the next token can follow one, on the same line, except
//!   after `static` and `export`, which may be followed by a line break.
//! - `static` after a `static` modifier is a name (`static static x` declares `static`).
//! - `export` modifies a parameter (`f(export e)`) unless `{`, `as` or `default` follows, or
//!   `type` without a parameter start after it; then it is a reserved word (TS1359).

use ts_r_ast::{ModifierToken, Modifiers};
use ts_r_lexer::TokenKind as T;

use crate::Parser;

/// Where a modifier list is parsed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ModCtx {
    /// Before a declaration statement (after the declaration lookahead succeeded).
    Statement,
    ClassMember,
    /// Interface and type literal members.
    TypeMember,
    ObjectMember,
    Param,
    TypeParam,
}

/// Can `k` modify a parameter? For the arrow-function and function-type lookaheads.
pub(crate) fn is_param_modifier(k: T) -> bool {
    matches!(
        k,
        T::Public
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
    )
}

/// Can `k` follow a parameter modifier (so the keyword before it is a modifier)?
fn is_param_start_after_modifier(k: T) -> bool {
    matches!(k, T::LBracket | T::LBrace | T::DotDotDot | T::This) || k.is_identifier_like()
}

/// In the arrow-function and function-type lookaheads: after `(export`, does `k` make
/// `export` a parameter modifier? (`parse_modifiers` decides for real.)
pub(crate) fn is_export_param_modifier_follow(k: T) -> bool {
    is_param_start_after_modifier(k) && !matches!(k, T::LBrace | T::As | T::Default)
}

/// Longest modifier chain (`export declare abstract ...`) a declaration lookahead follows.
pub(crate) const MAX_MODIFIER_CHAIN: u32 = 16;

impl<'a> Parser<'a> {
    /// The modifier the current token is in `ctx`, if it is a modifier keyword there.
    pub(crate) fn modifier_keyword(&self, ctx: ModCtx) -> Option<Modifiers> {
        let flag = match self.kind() {
            T::Public => Modifiers::PUBLIC,
            T::Private => Modifiers::PRIVATE,
            T::Protected => Modifiers::PROTECTED,
            T::Readonly => Modifiers::READONLY,
            T::Static => Modifiers::STATIC,
            T::Abstract => Modifiers::ABSTRACT,
            T::Declare => Modifiers::DECLARE,
            T::Accessor => Modifiers::ACCESSOR,
            T::Override => Modifiers::OVERRIDE,
            T::Async => Modifiers::ASYNC,
            T::Export => Modifiers::EXPORT,
            T::Const => Modifiers::CONST,
            T::In => Modifiers::IN,
            T::Out => Modifiers::OUT,
            _ => return None,
        };
        let excluded = match ctx {
            ModCtx::Statement => Modifiers::OVERRIDE
                .union(Modifiers::IN)
                .union(Modifiers::OUT)
                .union(Modifiers::CONST),
            ModCtx::ClassMember => Modifiers::NONE,
            ModCtx::TypeMember | ModCtx::ObjectMember | ModCtx::Param => Modifiers::CONST,
            ModCtx::TypeParam => Modifiers::EXPORT,
        };
        (!excluded.intersects(flag)).then_some(flag)
    }

    /// Parses the modifier keywords at the current token.
    pub(crate) fn parse_modifiers(&mut self, ctx: ModCtx) -> Modifiers {
        let mut m = Modifiers::NONE;
        while let Some(flag) = self.modifier_keyword(ctx) {
            if self.aborting() || (flag == Modifiers::STATIC && m.contains(Modifiers::STATIC)) {
                break;
            }
            // `static {` starts a class static block (which may follow modifiers).
            if ctx == ModCtx::ClassMember
                && flag == Modifiers::STATIC
                && self.peek_kind() == T::LBrace
            {
                break;
            }
            if !self.next_can_follow_modifier(ctx, flag) {
                break;
            }
            m |= self.bump_modifier(flag);
        }
        m
    }

    /// Consumes the current token as the modifier `flag`, recording it.
    pub(crate) fn bump_modifier(&mut self, flag: Modifiers) -> Modifiers {
        let span = self.tok().span();
        self.ast.modifier_tokens.push(ModifierToken { span, flag });
        self.bump();
        flag
    }

    /// Can the token after the current modifier keyword follow it (so the keyword is a
    /// modifier, not a name)?
    fn next_can_follow_modifier(&mut self, ctx: ModCtx, flag: Modifiers) -> bool {
        let next = self.peek();
        if next.newline_before() && !flag.intersects(Modifiers::STATIC.union(Modifiers::EXPORT)) {
            return false;
        }
        let k = next.kind;
        match ctx {
            // `static export as namespace N;` is a namespace export with a misplaced
            // modifier (umd-errors), not `export` as a modifier.
            ModCtx::Statement => {
                k.is_identifier_or_keyword() && !(flag == Modifiers::EXPORT && k == T::As)
            }
            ModCtx::ClassMember => {
                matches!(
                    k,
                    T::String
                        | T::Number
                        | T::BigInt
                        | T::LBracket
                        | T::LBrace
                        | T::PrivateName
                        | T::Star
                        | T::At
                ) || k.is_identifier_or_keyword()
            }
            ModCtx::ObjectMember => {
                matches!(
                    k,
                    T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName | T::Star
                ) || k.is_identifier_or_keyword()
            }
            ModCtx::TypeMember => {
                matches!(
                    k,
                    T::String | T::Number | T::BigInt | T::LBracket | T::PrivateName
                ) || k.is_identifier_or_keyword()
            }
            ModCtx::Param => {
                if flag == Modifiers::EXPORT {
                    // `f(export e)` is a (misplaced) modifier, but not before `{`, `as` or
                    // `default`, and `export type` only when `type` could take a modifier
                    // itself (`f(export type x)`, not `f(export type)`).
                    match k {
                        T::LBrace | T::As | T::Default => return false,
                        T::Type => {
                            return self.lookahead(|p| {
                                p.bump();
                                p.bump();
                                is_param_start_after_modifier(p.kind())
                                    && !matches!(p.kind(), T::LBrace | T::As | T::Default)
                            });
                        }
                        _ => {}
                    }
                }
                is_param_start_after_modifier(k)
            }
            ModCtx::TypeParam => k.is_identifier_like() || k == T::In,
        }
    }

    /// At the start of a statement: do modifier keywords (if any) lead to a declaration?
    /// `public class A {}`, `static var x`, `declare public let y` are declarations whose
    /// misplaced modifiers the checker reports; `public\nclass A {}` is not.
    pub(crate) fn declaration_follows_modifiers(&mut self) -> bool {
        self.lookahead(|p| {
            for _ in 0..MAX_MODIFIER_CHAIN {
                match p.kind() {
                    T::Public
                    | T::Private
                    | T::Protected
                    | T::Readonly
                    | T::Abstract
                    | T::Accessor
                    | T::Async
                    | T::Declare => {
                        p.bump();
                        if p.newline_before() {
                            return false;
                        }
                    }
                    T::Static => p.bump(),
                    _ => break,
                }
            }
            p.at_declaration_after_modifiers()
        })
    }

    /// A declaration keyword after a modifier list.
    fn at_declaration_after_modifiers(&mut self) -> bool {
        match self.kind() {
            T::Var | T::Let | T::Const | T::Function | T::Class | T::Enum | T::Export => true,
            T::Global => true,
            T::Import => !matches!(self.peek_kind(), T::LParen | T::Dot),
            T::Interface | T::Type => {
                let next = self.peek();
                !next.newline_before() && next.kind.is_identifier_like()
            }
            T::Namespace | T::Module => {
                let next = self.peek();
                !next.newline_before() && (next.kind.is_identifier_like() || next.kind == T::String)
            }
            T::Using => self.is_using_declaration(),
            T::Await => self.is_await_using_declaration(),
            _ => false,
        }
    }

    /// A statement-start modifier keyword that does not start a declaration but is followed
    /// on its line by a name (`public x = 1;`, `static test()`). tsc reports TS1128 at the
    /// keyword and skips it rather than reading it as an identifier.
    pub(crate) fn is_stray_statement_modifier(&mut self) -> bool {
        if !matches!(
            self.kind(),
            T::Public | T::Private | T::Protected | T::Static | T::Readonly | T::Accessor
        ) {
            return false;
        }
        let next = self.peek();
        !next.newline_before() && next.kind.is_identifier_or_keyword()
    }

    /// `export` followed on its line by a name that starts neither a declaration nor another
    /// export form (`export foo;`): TS1128 at `export`, which is skipped.
    pub(crate) fn is_stray_export(&mut self) -> bool {
        self.lookahead(|p| {
            p.bump();
            if p.newline_before()
                || !p.kind().is_identifier_or_keyword()
                || matches!(p.kind(), T::Default | T::As | T::Import)
            {
                return false;
            }
            // `export type { a }`, `export type * from`, `export type A = ...`; but
            // `export type 100 {}` is a stray `export` (parseInvalidNames).
            if p.at(T::Type) {
                let next = p.peek();
                return !(matches!(next.kind, T::LBrace | T::Star)
                    || (next.kind.is_identifier_or_keyword() && !next.newline_before()));
            }
            !p.declaration_follows_modifiers()
        })
    }
}
