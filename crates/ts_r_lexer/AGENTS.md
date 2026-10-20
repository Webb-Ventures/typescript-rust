# ts_r_lexer

Byte-oriented, pull-based scanner for TypeScript, TSX, JavaScript and `.d.ts`. The parser
drives it and re-scans tokens whose meaning depends on syntax (`/` vs regex, `>` vs `>>`,
template continuations, JSX). Root rules: [../../AGENTS.md](../../AGENTS.md).

## API in one breath

`Lexer::new(src)`, then `next_token()` returns a `TokenKind`. `token()` gives a `Token { kind,
flags, start, end }` (8 bytes), and `token_text()` / `text(span)` return the slice.

- Parser-driven re-scans:
  - `rescan_greater` merges `>` into `>=`, `>>`, `>>=`, `>>>`, `>>>=`.
  - `split_less_than` turns `<<` into `<` in type arguments.
  - `rescan_slash_as_regex`, `rescan_template_continuation` (after `}` in `${}`).
  - `next_jsx_child`, `rescan_jsx_identifier` (dashed names), `next_jsx_attribute_value`.
- Values are decoded on demand:
  - `ident_value` decodes escapes.
  - `string_value` and `template_cooked` (a `Result`: tagged templates may keep an invalid escape).
  - `template_raw_span`, `number_value` (`parse_number`), `regex_parts`.
- `seek(pos)` moves the lexer to a byte offset (clamped to a char boundary) for the JSDoc
  type parser; call `next_token()` after it.
- Speculation: `checkpoint()` / `rewind(cp)` restore position, token, and the comment and
  diagnostic lists.
- Outputs: `comments` (line, block and hashbang, with `newline_before`) and `diagnostics`.

## Invariants

- **No allocation per token.** Tokens are spans plus flags. Identifier and string values are
  only allocated when they contain escapes (`TokenFlags::ESCAPED`).
- `>` is always scanned as a single `Gt`. Only the parser knows whether `>>` is a shift or two
  closing type-argument brackets, so it calls `rescan_greater` in expression context.
- `TokenKind` order is load-bearing:
  - Reserved words are `Break..=With`, strict-mode reserved words `Implements..=Yield`, then
    contextual keywords `Abstract..` in alphabetical order.
  - `is_identifier_like()` is `Ident` or `>= Implements`.
  - Assignment operators are `Eq..=QuestionQuestionEq`.
  - Keep these ranges when adding tokens.
- Escaped keywords (`const`) scan as `Ident` with `ESCAPED_KEYWORD`, never as the
  keyword.
- `NEWLINE_BEFORE` is set when a line terminator (or a multi-line block comment) precedes the
  token. ASI, `async`/arrow and modifier rules depend on it.
- A leading BOM is skipped and `#!` on the first line becomes a `Hashbang` comment.
- U+0085 (NEXT LINE) is whitespace, not a line break, as in tsc (`chars::is_unicode_whitespace`).
  A keyword directly followed by a non-ASCII character that ends the word (`return` + U+0085)
  is still the keyword.
- Error positions match tsc 7:
  - An unterminated string is reported at its end.
  - An invalid escape is reported at the offending character, every one in a string
    (`template_cooked` returns the first). `\u{...}`: no digits is TS1125 just after the
    `{`, a value over 0x10FFFF TS1198 at the digits, a missing `}` TS1199 after them; at
    the end of an unterminated string the escape's error replaces TS1002.
  - In an identifier, a backslash that starts no valid escape ends the identifier and is an
    invalid character (TS1127): `arg\u003` is `arg`, TS1127, `u003`.
  - Regex flags: unknown TS1499, duplicate TS1500, `u` with `v` TS1502.
- Never panics on any input; slicing uses `get`.
- Every scanning loop advances or stops at end of input. As a backstop, `next_token` turns a
  zero-width non-EOF token into a one-character `Unknown`, so the parser can never stall on
  the lexer.

## Testing

`cargo test -p ts_r_lexer` (unit tests in `src/tests.rs`, memory-capped with
`ts_r_testing::cap_memory!()`): punctuators, keywords, escapes,
numbers (separators, bigint, legacy octal), strings, templates, regex re-scan, JSX modes,
comments, checkpoints and invalid input. tsc-matched error positions are pinned in
`ts_r_parser`'s `first_error_matches_tsc`; the parser's corpus run exercises the lexer on real
code.

Do not type a backslash-u escape sequence literally in a test file with the Write tool. It
turns the sequence into the character. Build it with `concat!("\\", "u0061")`.

## Dependencies

- `memchr`: SIMD search for string terminators, `*/` and newlines in comments, and JSX text
  ends. These dominate lexing time on real code.
- `unicode-ident`: `ID_Start` / `ID_Continue` tables for non-ASCII identifiers.
  Hand-written tables would be a large, error-prone copy of Unicode data.
- `ts_r_span`, `ts_r_ast` (`Comment`, `CommentKind`), `ts_r_diagnostics` (messages).
- Dev: `ts_r_testing` (memory cap for tests).

## Gaps

- Regex *patterns* are not validated (tsc 7 reports e.g. unbalanced groups, TS1005 `')'
  expected` inside a regex); only termination and flags are checked.
- Flag availability by `target` (TS1501) is not reported; the lexer does not know the target.
- No JSX HTML entity decoding (left to codegen).
