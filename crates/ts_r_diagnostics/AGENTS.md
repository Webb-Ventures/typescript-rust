# ts_r_diagnostics

Diagnostics with tsc's codes and texts, and the two output formats tsc prints. Root rules:
[../../AGENTS.md](../../AGENTS.md).

## API in one breath

`messages::*` are `DiagnosticMessage { code, category, text }` constants
(`messages::EXPECTED` is TS1005 `'{0}' expected.`); `messages::by_code` and `messages::ALL`
look them up. `Diagnostic::new(msg, span, &args)` formats the text once; `with_file`,
`with_related` add a `FileId` and `RelatedInfo` (for example TS1007 "The parser expected to
find a '}' to match the '{' token here."). Rendering takes any `Files` (a `SourceText`, a
slice or `Vec` of them indexed by `FileId`):

- `format_plain`: `file(line,col): error TS1005: ',' expected.`, the `tsc --pretty false` form.
- `format_pretty` via `Renderer { color }`: tsc's pretty form with the code frame and `~`
  underline.
- `format_summary`: the `Found N errors in M files.` footer.

## Invariants

- **tsc 7 is the spec.** A message that tsc also reports uses tsc's code, category and text
  exactly. Check by running `npx -y -p typescript@7 tsc --noEmit` on a tiny file; never read
  TypeScript's source.
- ts-r-only messages use codes 99000 and up (`MAXIMUM_NESTING_DEPTH` 99001, `FILE_TOO_LARGE`
  99002, `PARSER_NO_PROGRESS` 99003) so they cannot collide with tsc codes.
- Sound-mode codes (design record 0007) are stored as `SOUND_CODE_BASE + n` (`100_000 + n`)
  and rendered `TSR{n:04}` by `code_label` (`TSR0201`); everything below prints `TS{code}`.
- Columns in output are 1-based UTF-16 columns, computed only at render time. Diagnostics
  store byte spans.
- Rendering is deterministic and never panics on spans past the end of the text (they clamp).
- The pretty format shows up to 4 lines of a multi-line span and elides the rest with `...`,
  like tsc.
- Related information uses tsc 7's layout: `  file:line:col - message`, the code frame
  indented by 4, then a blank line (related info without a location: `    message`). A
  diagnostic without a location ends with an extra newline; callers drop one when the text
  ends in a blank line, so a global diagnostic prints no blank line unless related blocks
  follow it.

## Testing

`cargo test -p ts_r_diagnostics`. Tests pin the plain format (including UTF-16 columns), the
pretty frame (colour, related info, elided multi-line spans), the summary line, argument
formatting and the message table (unique codes).

## Dependencies

- `ts_r_span`: spans, `SourceText` and line/UTF-16 column conversion.

## Gaps

- The catalogue holds the syntax messages the parser and lexer report, plus a few others. The
  checker will add its own.
- No message chains (`DiagnosticMessageChain`) or suggestions/fixes yet.
- No localisation; English texts only.
