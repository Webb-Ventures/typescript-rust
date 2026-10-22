//! tsc's errors for a statement or class property that ends where no `;` can be inserted
//! (the next token is on the same line and is not `;`, `}` or the end of the file).
//!
//! Observed in tsc 7 (commonMissingSemicolons, staticModifierAlreadySeen,
//! derivedClassSuperCallsInNonConstructorMembers):
//! - After an expression statement that is a lone name, or a class property that is a lone
//!   name (no `?`/`!`, type or initialiser):
//!   - `declare` reports nothing (the next statement starts at the next token).
//!   - `interface {` is TS1438 and `module {`/`namespace {` TS1437, at the `{`.
//!   - A name close to one of [`SUGGESTIONS`] is TS1435 "Did you mean 'X'?" at the name
//!     (`asynd`, `clasd`, `vard`), and so is a name that is a keyword glued to a capitalised
//!     name or a second keyword (`interfaceMyInterface` -> 'interface MyInterface',
//!     `declareconst` -> 'declare const').
//!   - Any other name is TS1434 at the name.
//! - After a class property with a type and no initialiser: TS1441 at a `(`, which is
//!   skipped, otherwise TS1442 "Expected '=' for property initializer." at the next token.
//! - After a tagged template (`module \`M\` {}`, templateStringInModuleName): TS1443 at the
//!   template.
//! - Anything else is `';' expected` at the next token.
//!
//! Apart from that `(`, the next token is left for the next statement or member.

use ts_r_ast::{Node, NodeId};
use ts_r_diagnostics::messages;
use ts_r_lexer::TokenKind as T;

use crate::Parser;

/// The keywords tsc suggests for a misspelled name at the start of a statement.
const SUGGESTIONS: [&str; 9] = [
    "async",
    "class",
    "const",
    "declare",
    "function",
    "interface",
    "let",
    "type",
    "var",
];

impl Parser<'_> {
    /// The `;` after an expression statement.
    pub(crate) fn expression_statement_semicolon(&mut self, expr: NodeId) {
        if self.eat(T::Semi) || self.can_insert_semicolon() {
            return;
        }
        // `module \`M\` {}`, `foo \`x\` bar` (templateStringInModuleName): a tagged template
        // statement with no `;` after it is TS1443 at the template, as in tsc 7.
        if let Node::TaggedTemplate { quasi, .. } = *self.ast.node(expr) {
            let span = self.ast.span(quasi);
            self.error_at(messages::MODULE_NAME_QUOTED_STRINGS, span, &[]);
            return;
        }
        // Before an invalid character the error is `';' expected` at it, which gives way to
        // the scanner's TS1127 (invalidUnicodeEscapeSequance3: `a\u`).
        if matches!(self.ast.node(expr), Node::Ident { .. }) && !self.at(T::Unknown) {
            self.missing_semicolon_after_name(expr);
        } else {
            self.error_expected(";");
        }
    }

    /// The `;` after a class property declaration.
    pub(crate) fn property_semicolon(&mut self, key: NodeId, bare: bool, typed: bool) {
        if self.eat(T::Semi) || self.can_insert_semicolon() {
            return;
        }
        // `prop @dec\n m() {}`: TS1436 at the `@`, which then decorates the next member
        // (decoratorOnClassMethod17).
        if self.at(T::At) {
            self.error(messages::DECORATORS_MUST_PRECEDE_NAME_AND_KEYWORDS);
            return;
        }
        if bare && matches!(self.ast.node(key), Node::Ident { .. }) {
            self.missing_semicolon_after_name(key);
        } else if typed && self.at(T::LParen) {
            // The `(` is skipped (`a: super();` goes on with TS1068 at the `)`).
            self.error(messages::FUNCTION_CALL_IN_TYPE_ANNOTATION);
            self.bump();
        } else if typed {
            self.error(messages::EXPECTED_EQUALS_FOR_PROPERTY_INITIALIZER);
        } else {
            self.error_expected(";");
        }
    }

    /// A lone name `name` (an `Ident` node) followed by a token that cannot follow it.
    fn missing_semicolon_after_name(&mut self, name: NodeId) {
        let Node::Ident { name: atom } = *self.ast.node(name) else {
            return;
        };
        let text = self.ast.atom(atom).to_owned();
        let span = self.ast.span(name);
        match text.as_str() {
            "declare" => return,
            "interface" if self.at(T::LBrace) => {
                self.error(messages::INTERFACE_MUST_BE_GIVEN_A_NAME);
                return;
            }
            "module" | "namespace" if self.at(T::LBrace) => {
                self.error(messages::NAMESPACE_MUST_BE_GIVEN_A_NAME);
                return;
            }
            // `interface void {}`, `type void = I`, `namespace 100 {}`: the next token is not
            // a name, so the declaration keyword was a lone identifier; the error is about
            // the would-be name (interfacesWithPredefinedTypesAsNames, parseInvalidNames).
            "interface" | "type" | "module" | "namespace" => {
                let msg = match text.as_str() {
                    "interface" => messages::INTERFACE_NAME_CANNOT_BE,
                    "type" => messages::TYPE_ALIAS_NAME_CANNOT_BE,
                    _ => messages::NAMESPACE_NAME_CANNOT_BE,
                };
                let span = self.tok().span();
                let next = span.text(self.src).to_owned();
                self.error_at(msg, span, &[&next]);
                return;
            }
            _ => {}
        }
        let suggestion = spelling_suggestion(&text)
            .map(str::to_owned)
            .or_else(|| glued_keyword(&text));
        match suggestion {
            Some(s) => {
                self.error_at(
                    messages::UNKNOWN_KEYWORD_OR_IDENTIFIER_DID_YOU_MEAN,
                    span,
                    &[&s],
                );
            }
            None => self.error_at(messages::UNEXPECTED_KEYWORD_OR_IDENTIFIER, span, &[]),
        }
    }
}

/// The closest of [`SUGGESTIONS`] to `name`, by tsc's observed rules: insertions and deletions
/// cost 1, substitutions 2 (0.1 when only the case differs). A candidate qualifies when its
/// length is within `max(2, len * 0.34)` of the name's and its distance is below
/// `len * 0.4 + 1`. An exact match is no suggestion.
fn spelling_suggestion(name: &str) -> Option<&'static str> {
    let n: Vec<char> = name.chars().collect();
    let len = n.len();
    if len == 0 || len > 64 {
        return None;
    }
    let max_len_diff = 2.max(len * 34 / 100);
    // Costs in tenths.
    let mut best_distance = (len * 4 / 10 + 1) * 10;
    let mut best = None;
    for cand in SUGGESTIONS {
        if cand == name || len.abs_diff(cand.len()) > max_len_diff {
            continue;
        }
        let d = distance(&n, cand);
        if d < best_distance {
            best_distance = d;
            best = Some(cand);
        }
    }
    best
}

/// Weighted edit distance (in tenths) between `a` and the ASCII keyword `b`.
fn distance(a: &[char], b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).map(|j| j * 10).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = (i + 1) * 10;
        for (j, &cb) in b.iter().enumerate() {
            let sub = if ca == cb {
                0
            } else if ca.eq_ignore_ascii_case(&cb) {
                1
            } else {
                20
            };
            cur[j + 1] = (prev[j] + sub).min(prev[j + 1] + 10).min(cur[j] + 10);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// `interfaceMyInterface` -> `interface MyInterface`, `declareconst` -> `declare const`: a
/// suggested keyword glued to a capitalised name or to another suggested keyword.
fn glued_keyword(name: &str) -> Option<String> {
    SUGGESTIONS.iter().find_map(|k| {
        let rest = name.strip_prefix(k)?;
        let glued =
            rest.starts_with(|c: char| c.is_ascii_uppercase()) || SUGGESTIONS.contains(&rest);
        glued.then(|| format!("{k} {rest}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_match_tsc() {
        let cases = [
            ("asynd", Some("async")),
            ("sasync", Some("async")),
            ("clasd", Some("class")),
            ("classs", Some("class")),
            ("consd", Some("const")),
            ("constd", Some("const")),
            ("declared", Some("declare")),
            ("functiond", Some("function")),
            ("interfaced", Some("interface")),
            ("myInterface2", Some("interface")),
            ("letd", Some("let")),
            ("typed", Some("type")),
            ("vard", Some("var")),
            ("MyClass2", None),
            ("type", None),
            ("declareconst", None),
        ];
        for (name, want) in cases {
            assert_eq!(spelling_suggestion(name), want, "{name}");
        }
        assert_eq!(
            glued_keyword("declareconst").as_deref(),
            Some("declare const")
        );
        assert_eq!(
            glued_keyword("interfaceMyInterface").as_deref(),
            Some("interface MyInterface")
        );
        assert_eq!(glued_keyword("typeface"), None);
    }
}
