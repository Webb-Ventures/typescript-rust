# ts_r_span

Source text, byte spans and line/column conversion for every ts-r crate. Root rules:
[../../AGENTS.md](../../AGENTS.md).

## API in one breath

`Span { start, end }` (UTF-8 byte offsets, `u32`, half-open) with `new`, `at` (zero-width),
`len`, `cover`, `contains`, `contains_span`, `text(src)`. `LineIndex::new(text)` gives
`line_of`, `line_start`, `line_span`, `line_col` (0-based line, byte column) and
`line_col_utf16` (0-based line, UTF-16 column, what tsc and LSP print), plus the reverse
`offset` / `offset_utf16`. `SourceText { name, text }` builds its `LineIndex` lazily
(`OnceLock`) and forwards the same queries.

## Invariants

- Positions are UTF-8 byte offsets everywhere. UTF-16 columns exist only for output
  (diagnostic rendering, LSP).
- Line terminators are `\n`, `\r\n`, a lone `\r`, U+2028 and U+2029, as in ECMAScript. `\r\n`
  is one terminator.
- Queries never panic: offsets past the end clamp to the end; an offset inside a multi-byte
  character counts that character as complete for UTF-16 columns.
- `LineIndex` construction is linear; ASCII-only text takes a fast path (no UTF-16 counting).

## Testing

`cargo test -p ts_r_span`. Unit tests cover every line terminator (CRLF, lone CR, U+2028),
astral characters (UTF-16 surrogate columns), empty text and span helpers.

## Dependencies

- `memchr`: SIMD search for line terminators when building `LineIndex` (the hot part of
  rendering diagnostics for large files).

## Gaps

- No file ids or source map; `ts_r_diagnostics::FileId` indexes a caller-owned list of
  `SourceText`s for now.
