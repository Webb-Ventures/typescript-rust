# ts_r_lexer

Byte-oriented, pull-based scanner for TypeScript, TSX, JavaScript and `.d.ts`. The parser
drives it and re-scans tokens whose meaning depends on syntax (`/` vs regex, `>` vs `>>`,
template continuations, JSX). Root rules: [../../AGENTS.md](../../AGENTS.md).

## API in one breath

`Lexer::new(src)`, then `next_token()` returns a `TokenKind`. `token()` gives a `Token { kind,
flags, start, end }` (8 bytes), and `token_text()` / `text(span)` return the slice.

- Parser-driven re-scans:
  - `rescan_greater` merges `>` into `>=`, `>>`, `>>=`, `>>>`, `>>>=`.
  - `split_less_than` turns `<<` into `<` in type arguments; `split_star_eq` turns `*=`
    into `*` (the JSDoc type `*=`).
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
- Escaped keywords (`const`) scan as the keyword with `ESCAPED_KEYWORD` (and
  `ESCAPED`), as tsc reads them. The parser reports TS1260 when it consumes one as a keyword
  rather than reading it as a name (`intern_token`).
- `NEWLINE_BEFORE` is set when a line terminator (or a multi-line block comment) precedes the
  token. ASI, `async`/arrow and modifier rules depend on it.
- A leading BOM is skipped and `#!` on the first line becomes a `Hashbang` comment. Elsewhere
  `#!` is TS18026 and only the `#` is skipped (an `Unknown` token).
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
  - Legacy octal escapes in strings and templates are TS1487 (`'\x05'` for `\05`) and
    `\8`/`\9` TS1488, on the whole escape: `\0`-`\3` take up to two more octal digits,
    `\4`-`\7` one, `\0` alone is fine and `\08` is `\0` then `8` (tsc:
    scanner.go scanEscapeSequence). The value is still cooked; tagged templates allow them.
  - Regex flags: unknown TS1499, duplicate TS1500, `u` with `v` TS1502, and TS1501 with the
    target a flag needs (`d` es2022, `s` es2018, `v` es2024) for every accepted flag: the
    checker keeps TS1501 only for older targets.
  - Regex patterns (`regex.rs`, written from tsc/internal/scanner/regexp.go's rules): the
    TS1503-TS1537 errors (groups and named groups, modifiers, quantifiers, escapes,
    backreferences, classes, `v`-mode class sets, `\p{...}`), and TS1487 for octal escapes.
    It is recursive descent with a depth limit (256; deeper patterns report nothing), errors
    at one position are kept once, and every loop consumes a character or stops. TS1503
    (named groups need ES2018) is reported for every named group; the checker filters it by
    target. No "Did you mean" suggestions (tsc's TS1369 chains).
  - Unicode property names and values (`regex_props.rs`) are the tables V8 accepts in Node 24
    (Unicode 17): General_Category, Script/Script_Extensions values with aliases, binary
    properties and the properties of strings (`v` only). They come from the Unicode
    `PropertyAliases.txt`/`PropertyValueAliases.txt` data filtered by what `new RegExp`
    accepts; keep them sorted (`property_tables_are_sorted`).
  - Numbers: a name right after a plain decimal literal is TS1351 on the name, which stays
    its own token (`3in x` is `3 in x`); hex, octal, binary and leading-zero literals are not
    checked, nor a name starting with an escape (the parser's `';' expected`). `0_1` is
    TS6188 at the `_`, once; every `_` before the first digit is TS6188 (`0x___1`), later
    repeats TS6189. An empty `0b`/`0o`/`0x` is TS1177/TS1178/
    TS1125. `1e2n` and `4.1n` take the `n` but stay `Number`, with TS1352/TS1353 on the
    whole literal.
- A backslash at the end of the file ends a string or template with TS1126 (not
  TS1002/TS1160). A lone `#` is TS1127 and still a `PrivateName` token (`Hash` is
  never produced).
- In JSX children a `<<<<<<<` conflict marker is TS1185 and its line one `Unknown` token.
- Merge conflict markers are trivia (`is_conflict_marker`, observed in tsc 7): seven `<`,
  `|`, `=` or `>` with only blanks before them on the line, then a space (`=======` takes
  any next character), are TS1185 on the seven characters. `<<<<<<<` and `>>>>>>>` skip
  their line; `|||||||` skips to the next `=======` or `>>>>>>>` marker, `=======` to the
  next `>>>>>>>` marker (or the end of the file).
- U+FFFD where a token starts means a binary file: TS1490 at offset 0 (once), and the rest
  of the file is one `Unknown` token (no TS1127), so the parser reports one error at it.
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

- Regex pattern errors carry no "Did you mean" suggestions, and `Katakana_Or_Hiragana` is
  rejected (V8's table has no such script value). The parser defers all pattern and flag
  errors (checker errors in tsc 7, only in a file without syntax errors).
- An unterminated regex ends at the first `)`, `]` or `}` it did not open, or before a
  trailing run of `;`, as observed in tsc.
- No JSX HTML entity decoding (left to codegen).
