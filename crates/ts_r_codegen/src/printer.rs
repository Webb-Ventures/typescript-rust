//! Printer state: output buffer, indentation, token spacing, comments and source mapping.

use ts_r_ast::{Ast, Comment, CommentKind, Node, NodeId, NodeKind, Span};

use crate::sourcemap::{LineLookup, SourceMapBuilder};
use crate::{
    CodegenOptions, CodegenOutput, Layout, PrintHints, SpecifierKind, SpecifierSpan, Syntax,
};

/// Prints an [`Ast`]. Create with [`Printer::new`], call [`Printer::program`] (or
/// [`Printer::any`]) and then [`Printer::finish`].
#[derive(Debug)]
pub struct Printer<'a> {
    pub(crate) ast: &'a Ast,
    pub(crate) src: &'a str,
    pub(crate) opts: &'a CodegenOptions,
    hints: &'a PrintHints<'a>,
    pub(crate) out: String,
    pub(crate) indent: u32,
    /// The next write starts a new line and must emit indentation first.
    at_line_start: bool,
    // Source map state.
    map: Option<SourceMapBuilder>,
    lines: Option<LineLookup<'a>>,
    gen_line: u32,
    gen_col: u32,
    // Comment state: comments starting before `comment_cursor` are consumed.
    comment_cursor: u32,
    /// Output length right after a leading block comment that stays on its line.
    inline_comment_end: usize,
    /// Layout::Tsc: end of the original statement being printed (0 outside one, or inside a
    /// synthesized one). Comments inside expressions are printed only up to it.
    pub(crate) stmt_end: u32,
    /// Layout::Tsc: the source range holding the `(` of the next signature printed, for the
    /// comments inside empty parameters.
    pub(crate) params_paren: Option<(u32, u32)>,
    /// `in` must be parenthesized (inside a `for (init; ...)` head).
    pub(crate) no_in: bool,
    pub(crate) specifiers: Vec<SpecifierSpan>,
    pub(crate) compact: bool,
    /// [`Layout::Tsc`]: line breaks follow the source, as tsc prints them.
    pub(crate) tsc: bool,
    /// The next block printed is a function body ([`Printer::function_body`]).
    pub(crate) body_block: bool,
    /// [`Syntax::JavaScript`]: drop the modifiers tsc does not emit to JavaScript.
    pub(crate) js: bool,
    /// Compact layout: a statement-ending `;` not yet written. It is dropped when the next
    /// token is `}` and written before anything else.
    pending_semi: bool,
    /// Compact layout: a keyword separator not yet written. It is written only when the next
    /// token would otherwise merge with the previous one (`return x`, not `return(x)`).
    pending_space: bool,
    /// Source-map segments recorded while a separator was pending: their generated column is
    /// known only once the next token decides whether the separator is written.
    deferred_maps: Vec<(u32, u32, Option<&'a str>)>,
    /// [`Printer::tsc_maps`]: print without segments (what tsc leaves unmapped).
    pub(crate) map_muted: bool,
    /// Ends of the original expressions being printed ([`Printer::expr`]), innermost last.
    pub(crate) expr_ends: Vec<u32>,
    /// [`Layout::Tsc`] with a source map: segments follow tsc 7 (observed): the start and
    /// end of original nodes and of some tokens ([`Printer::map_start`], [`Printer::map_end`],
    /// [`Printer::map_token`]), merged by [`SourceMapBuilder::add_tsc`]. The per-token
    /// segments of the other layouts are off.
    pub(crate) tsc_maps: bool,
    /// [`Printer::tsc_maps`]: the source range holding the modifiers the next
    /// `ts_modifiers` call prints (set by the caller, taken by the call).
    pub(crate) mod_src: Option<(u32, u32)>,
    /// Generated line and column where the last line break was written.
    line_end: (u32, u32),
    /// [`PrintHints::moved_comments`]: the source ranges whose comments print with the
    /// statement standing for them (leading trivia to trailing same-line comments), sorted.
    moved: Vec<Span>,
    /// The moved range whose statement is being printed.
    moving: Option<Span>,
}

impl<'a> Printer<'a> {
    pub fn new(
        ast: &'a Ast,
        src: &'a str,
        opts: &'a CodegenOptions,
        hints: &'a PrintHints<'a>,
    ) -> Printer<'a> {
        let with_map = opts.source_map.is_some();
        let mut p = Printer {
            ast,
            src,
            opts,
            hints,
            out: String::with_capacity(src.len() + src.len() / 4 + 64),
            indent: 0,
            at_line_start: true,
            map: with_map.then(SourceMapBuilder::new),
            lines: with_map.then(|| LineLookup::new(src)),
            gen_line: 0,
            gen_col: 0,
            comment_cursor: 0,
            inline_comment_end: usize::MAX,
            stmt_end: 0,
            params_paren: None,
            no_in: false,
            specifiers: Vec::new(),
            compact: opts.layout == Layout::Compact,
            tsc: opts.layout == Layout::Tsc,
            body_block: false,
            js: false,
            pending_semi: false,
            pending_space: false,
            deferred_maps: Vec::new(),
            map_muted: false,
            expr_ends: Vec::new(),
            tsc_maps: with_map && opts.layout == Layout::Tsc,
            mod_src: None,
            line_end: (0, 0),
            moved: Vec::new(),
            moving: None,
        };
        if p.tsc {
            let mut moved: Vec<Span> = hints
                .moved_comments
                .iter()
                .map(|&(_, r)| p.moved_extent(r))
                .collect();
            moved.sort_by_key(|s| s.start);
            p.moved = moved;
        }
        p
    }

    /// The comments a moved source range `r` takes along: from after the previous token's
    /// line (its same-line comments trail that token) to the comments on `r`'s last line.
    fn moved_extent(&self, r: Span) -> Span {
        let comments = &self.ast.comments;
        let mut start = self.prev_token_end(r.start);
        let mut i = comments.partition_point(|c| c.span.start < start);
        while let Some(c) = comments.get(i)
            && c.span.start < r.start
            && !self.has_newline_between(start, c.span.start)
        {
            start = c.span.end;
            i += 1;
        }
        let mut end = r.end;
        let mut i = comments.partition_point(|c| c.span.start < end);
        while let Some(c) = comments.get(i)
            && self
                .src
                .get(end as usize..c.span.start as usize)
                .is_some_and(|g| g.bytes().all(|b| b == b' ' || b == b'\t'))
        {
            end = c.span.end;
            i += 1;
        }
        Span::new(start, end.max(start))
    }

    /// A synthesized node whose [`PrintHints::map_ranges`] range maps neither side: an
    /// inserted statement that stands for no original one (`exports.x = void 0;`), even at 0.
    pub(crate) fn unmapped_insert(&self, id: NodeId) -> bool {
        let ranges = self.hints.map_ranges;
        ranges
            .binary_search_by_key(&id, |&(n, _)| n)
            .is_ok_and(|i| ranges[i].1.start == crate::NO_MAP && ranges[i].1.end == crate::NO_MAP)
    }

    /// [`PrintHints::moved_comments`]: the source range statement `id` stands for.
    pub(crate) fn moved_of(&self, id: NodeId) -> Option<Span> {
        if !self.tsc {
            return None;
        }
        let m = self.hints.moved_comments;
        m.binary_search_by_key(&id, |&(n, _)| n)
            .ok()
            .map(|i| m[i].1)
    }

    /// A comment not printed at `pos` in the linear pass: inside erased code, or inside a
    /// moved range other than the one being printed.
    fn comment_hidden(&self, pos: u32) -> bool {
        if self.in_erased(pos) {
            return true;
        }
        if self.moved.is_empty() || self.moving.is_some_and(|m| m.start <= pos && pos < m.end) {
            return false;
        }
        let i = self.moved.partition_point(|s| s.start <= pos);
        i > 0 && self.moved[i - 1].end > pos
    }

    /// A statement standing for a moved source range `orig` (a field initializer in the
    /// constructor): printed with the range's comments, wherever the cursor is.
    pub(crate) fn moved_statement(&mut self, id: NodeId, orig: Span) {
        let saved = (self.comment_cursor, self.stmt_end, self.moving);
        let extent = self.moved_extent(orig);
        self.moving = Some(extent);
        self.comment_cursor = extent.start;
        self.leading_comments(orig.start);
        if !self.after_inline_comment() {
            self.newline();
        }
        self.stmt_end = orig.end;
        self.statement(id);
        self.trailing_comments(orig.end);
        self.newline();
        (self.comment_cursor, self.stmt_end, self.moving) = saved;
    }

    /// Sets what the output is ([`Syntax::TypeScript`] unless set).
    pub fn with_syntax(mut self, syntax: Syntax) -> Printer<'a> {
        self.js = syntax == Syntax::JavaScript;
        self
    }

    pub fn finish(mut self) -> CodegenOutput {
        // A trailing `;` stays: fragments are concatenated, and ASI would not separate
        // `a = b` from a following `(c)`.
        self.resolve_pending(b';');
        if !self.compact && !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push_str(self.opts.newline);
        }
        let map = match (self.map.take(), &self.opts.source_map) {
            (Some(b), Some(o)) => Some(b.finish(
                o.file.clone(),
                o.source.clone(),
                o.include_content.then(|| self.src.to_owned()),
                o.source_root.clone(),
            )),
            _ => None,
        };
        CodegenOutput {
            code: self.out,
            map,
            specifiers: self.specifiers,
        }
    }

    // ---- low-level output ----

    /// Appends raw text, tracking the generated position. No spacing logic.
    fn push(&mut self, s: &str) {
        let Some(&first) = s.as_bytes().first() else {
            return;
        };
        self.resolve_pending(first);
        self.flush_indent();
        self.out.push_str(s);
        if self.tsc_maps && s.contains(['\u{2028}', '\u{2029}', '\r']) {
            // tsc's generated positions count U+2028, U+2029 and a lone `\r` as line breaks
            // too (a string literal or comment copied verbatim can hold them).
            self.advance_tsc_lines(s);
        } else if self.map.is_some() {
            let bytes = s.as_bytes();
            match bytes.iter().rposition(|&b| b == b'\n') {
                Some(i) => {
                    self.gen_line += bytes.iter().filter(|&&b| b == b'\n').count() as u32;
                    self.gen_col = utf16_len(&s[i + 1..]);
                }
                None => self.gen_col += utf16_len(s),
            }
        }
    }

    /// Advances the generated position over `s`, line breaks counted as tsc counts them.
    fn advance_tsc_lines(&mut self, s: &str) {
        let mut chars = s.char_indices().peekable();
        let mut col_from = 0;
        while let Some((i, c)) = chars.next() {
            let brk = match c {
                '\r' => {
                    if let Some(&(_, '\n')) = chars.peek() {
                        chars.next();
                        i + 2
                    } else {
                        i + 1
                    }
                }
                '\n' => i + 1,
                '\u{2028}' | '\u{2029}' => i + 3,
                _ => continue,
            };
            self.gen_line += 1;
            self.gen_col = 0;
            col_from = brk;
        }
        self.gen_col += utf16_len(&s[col_from..]);
    }

    /// Appends text verbatim (no token spacing), tracking positions.
    pub(crate) fn raw_append(&mut self, s: &str) {
        self.push(s);
    }

    /// Writes pending indentation.
    fn flush_indent(&mut self) {
        if self.at_line_start {
            self.at_line_start = false;
            if !self.compact && self.indent > 0 {
                let n = (self.indent * self.opts.indent_width as u32) as usize;
                for _ in 0..n {
                    self.out.push(' ');
                }
                if self.map.is_some() {
                    self.gen_col += n as u32;
                }
            }
        }
    }

    /// Writes the pending `;` and separator (compact layout) as the next token, starting
    /// with `first`, requires: the `;` unless the token is `}`, the space only when the two
    /// tokens would merge. Then records the source-map segments that waited for it.
    fn resolve_pending(&mut self, first: u8) {
        if self.pending_semi {
            self.pending_semi = false;
            if first != b'}' {
                self.write_unpended(";");
            }
        }
        if self.pending_space {
            self.pending_space = false;
            if first != b' ' && self.needs_space(first) {
                self.write_unpended(" ");
            }
        }
        if !self.deferred_maps.is_empty() {
            let (gl, gc) = (self.gen_line, self.gen_col);
            let maps = std::mem::take(&mut self.deferred_maps);
            if let Some(m) = &mut self.map {
                for (line, col, name) in &maps {
                    m.add(gl, gc, *line, *col, *name);
                }
            }
            self.deferred_maps = maps;
            self.deferred_maps.clear();
        }
    }

    /// Appends text with no pending-token logic.
    fn write_unpended(&mut self, s: &str) {
        self.flush_indent();
        self.out.push_str(s);
        if self.map.is_some() {
            self.gen_col += utf16_len(s);
        }
    }

    /// Whether a token starting with `first` would merge with the output's last byte.
    fn needs_space(&self, first: u8) -> bool {
        if self.at_line_start {
            return false;
        }
        let Some(&last) = self.out.as_bytes().last() else {
            return false;
        };
        (is_word_byte(last) && (is_word_byte(first) || first == b'#'))
            || (last == b'+' && first == b'+')
            || (last == b'-' && first == b'-')
            || (last == b'/' && (first == b'/' || first == b'*'))
            || (last == b'<' && (first == b'!' || first == b'<'))
            || (last == b'!' && first == b'=')
    }

    /// Writes a token, inserting a space if it would otherwise merge with the previous one
    /// (`return x`, `a - -b`, `a / /re/`, `x + +y`).
    pub(crate) fn tok(&mut self, s: &str) {
        let Some(&first) = s.as_bytes().first() else {
            return;
        };
        self.resolve_pending(first);
        if self.needs_space(first) {
            self.push(" ");
        }
        self.push(s);
    }

    /// A statement-ending `;`. In compact layout it is written lazily and dropped before `}`.
    pub(crate) fn semi(&mut self) {
        if self.compact {
            self.resolve_pending(b';');
            self.pending_semi = true;
        } else {
            self.tok(";");
        }
    }

    /// A space in readable layout only.
    pub(crate) fn space(&mut self) {
        if !self.compact && !self.at_line_start {
            self.push(" ");
        }
    }

    /// A required space (between keywords and what follows), unless one is implied. In
    /// compact layout it is written only if the next token would merge with the keyword
    /// (`return x` but `return(x)`, `let{a}=b`).
    pub(crate) fn hard_space(&mut self) {
        if self.compact {
            self.resolve_pending(b' ');
            if !self.at_line_start {
                self.pending_space = true;
            }
            return;
        }
        if !self.at_line_start && !self.out.ends_with(' ') {
            self.push(" ");
        }
    }

    pub(crate) fn newline(&mut self) {
        if self.compact {
            return;
        }
        if !self.at_line_start {
            self.out.push_str(self.opts.newline);
            if self.map.is_some() {
                self.line_end = (self.gen_line, self.gen_col);
                self.gen_line += 1;
                self.gen_col = 0;
            }
            self.at_line_start = true;
        }
    }

    /// A newline even in compact layout (after line comments).
    fn forced_newline(&mut self) {
        self.resolve_pending(b'\n');
        if !self.at_line_start {
            self.out.push_str(self.opts.newline);
            if self.map.is_some() {
                self.line_end = (self.gen_line, self.gen_col);
                self.gen_line += 1;
                self.gen_col = 0;
            }
            self.at_line_start = true;
        }
    }

    pub(crate) fn indent(&mut self) {
        self.indent += 1;
    }

    pub(crate) fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    /// Output byte length (for specifier spans).
    pub(crate) fn pos(&mut self) -> u32 {
        // Flush indentation so the position is where the next token starts (a specifier's
        // string literal, which never merges with a keyword).
        self.resolve_pending(b'"');
        self.flush_indent();
        self.out.len() as u32
    }

    pub(crate) fn record_specifier(&mut self, kind: SpecifierKind, start: u32, end: u32) {
        if self.opts.collect_specifiers {
            self.specifiers.push(SpecifierSpan { kind, start, end });
        }
    }

    // ---- source maps ----

    /// Adds a mapping from the current output position to `span.start`.
    pub(crate) fn map_span(&mut self, span: Span) {
        self.map_span_named(span, None);
    }

    /// `name`: the original name, a slice of the source text.
    pub(crate) fn map_span_named(&mut self, span: Span, name: Option<&'a str>) {
        if self.map.is_none() || self.tsc_maps || (span.start == 0 && span.end == 0) {
            return;
        }
        let (line, col) = match &mut self.lines {
            Some(l) => l.lookup(span.start),
            None => return,
        };
        if self.pending_semi || self.pending_space {
            // The node's first token decides where it starts.
            self.deferred_maps.push((line, col, name));
            return;
        }
        // Map the token, not the indentation.
        self.flush_indent();
        let (gl, gc) = (self.gen_line, self.gen_col);
        if let Some(m) = &mut self.map {
            m.add(gl, gc, line, col, name);
        }
    }

    pub(crate) fn map_node(&mut self, id: NodeId) {
        let span = self.ast.span(id);
        self.map_span(span);
    }

    /// [`Printer::tsc_maps`]: a segment from the current output position to source `pos`.
    pub(crate) fn map_pos(&mut self, pos: u32) {
        self.map_pos_at(pos, false);
    }

    /// [`Printer::map_pos`]; `end`: the end of what was printed last, so at the start of a
    /// line it is the end of the previous one (tsc writes the separating line break after it).
    fn map_pos_at(&mut self, pos: u32, end: bool) {
        if !self.tsc_maps || self.map_muted {
            return;
        }
        let Some(lines) = &mut self.lines else {
            return;
        };
        let (line, col) = lines.lookup(pos);
        let (gl, gc) = if end && self.at_line_start && self.gen_line > 0 {
            self.line_end
        } else {
            // Map the token, not the indentation.
            self.flush_indent();
            (self.gen_line, self.gen_col)
        };
        if let Some(m) = &mut self.map {
            m.add_tsc(gl, gc, line, col);
        }
    }

    /// [`Printer::tsc_maps`]: the start of an original node, or of the range a transform gave
    /// a synthesized one ([`PrintHints::map_ranges`]).
    pub(crate) fn map_start(&mut self, id: NodeId) {
        if let Some(r) = self.map_range_of(id)
            && r.start != crate::NO_MAP
        {
            self.map_pos(r.start);
        }
    }

    /// [`Printer::tsc_maps`]: the end of an original node (or a synthesized one's range).
    pub(crate) fn map_end(&mut self, id: NodeId) {
        if let Some(r) = self.map_range_of(id)
            && r.end != crate::NO_MAP
        {
            self.map_pos_at(r.end, true);
        }
    }

    /// [`Printer::tsc_maps`]: the source range `id` maps to, if any: a transform's range
    /// ([`crate::PrintHints::map_ranges`]) first, else an original node's span.
    pub(crate) fn map_range_of(&self, id: NodeId) -> Option<Span> {
        if !self.tsc_maps {
            return None;
        }
        let ranges = self.hints.map_ranges;
        if let Ok(i) = ranges.binary_search_by_key(&id, |&(n, _)| n) {
            return Some(ranges[i].1);
        }
        (!self.is_synthetic(id)).then(|| self.ast.span(id))
    }

    /// Writes the token `text`; with [`Printer::tsc_maps`] and a source position `at`
    /// (`Some`: the token as written), maps its start and end.
    pub(crate) fn map_token(&mut self, text: &str, at: Option<u32>) {
        let at = at.filter(|_| self.tsc_maps);
        if let Some(p) = at {
            // The token's start: resolve spacing first so the segment lands on it.
            let first = text.as_bytes().first().copied().unwrap_or(b' ');
            self.resolve_pending(first);
            if self.needs_space(first) {
                self.push(" ");
            }
            self.map_pos(p);
        }
        self.tok(text);
        if let Some(p) = at {
            self.map_pos(p + text.len() as u32);
        }
    }

    // ---- helpers ----

    #[inline]
    pub(crate) fn node(&self, id: NodeId) -> Node {
        *self.ast.node(id)
    }

    #[inline]
    pub(crate) fn kind(&self, id: NodeId) -> NodeKind {
        self.ast.kind(id)
    }

    #[inline]
    pub(crate) fn span(&self, id: NodeId) -> Span {
        self.ast.span(id)
    }

    /// The source text of an original node, or `None` for synthesized nodes.
    /// [`Layout::Tsc`]: the source text of a name written with unicode escapes
    /// (`\u0078`, `#\u{78}`) when it still spells `text`; tsc prints names as written.
    pub(crate) fn escaped_raw(&self, id: NodeId, text: &str) -> Option<&'a str> {
        if !self.tsc {
            return None;
        }
        let raw = self.raw(id)?;
        if !raw.contains('\\') || raw.len() > 1024 {
            return None;
        }
        (unescape_name(raw)? == text).then_some(raw)
    }

    /// The source starts with code at offset 0, so a span hint of 0 is a real position (a
    /// stand-in for the first statement) rather than "no position".
    pub(crate) fn code_at_zero(&self) -> bool {
        self.src
            .as_bytes()
            .first()
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != 0xEF)
            && self.ast.comments.first().is_none_or(|c| c.span.start != 0)
    }

    pub(crate) fn raw(&self, id: NodeId) -> Option<&'a str> {
        let s = self.ast.span(id);
        if s.start >= s.end {
            return None;
        }
        self.src.get(s.start as usize..s.end as usize)
    }

    /// [`Layout::Tsc`]: the transform asked for `id` on one line ([`PrintHints::single_line`]).
    pub(crate) fn hint_single_line(&self, id: NodeId) -> bool {
        self.tsc && self.hints.single_line.binary_search(&id).is_ok()
    }

    /// [`Layout::Tsc`]: the transform asked for one element per line
    /// ([`PrintHints::multi_line`]).
    pub(crate) fn hint_multi_line(&self, id: NodeId) -> bool {
        self.tsc && self.hints.multi_line.binary_search(&id).is_ok()
    }

    pub(crate) fn hint_single_quoted(&self, id: NodeId) -> bool {
        self.tsc && self.hints.single_quoted.binary_search(&id).is_ok()
    }

    pub(crate) fn is_synthetic(&self, id: NodeId) -> bool {
        let s = self.ast.span(id);
        s.start >= s.end
    }

    /// True if the source between `from` and `to` contains a line break.
    pub(crate) fn has_newline_between(&self, from: u32, to: u32) -> bool {
        if from >= to {
            return false;
        }
        self.src
            .get(from as usize..to as usize)
            .is_some_and(|t| t.bytes().any(|b| b == b'\n' || b == b'\r'))
    }

    // ---- source line breaks (Layout::Tsc) ----

    /// Layout::Tsc: whether the source has a line break between the original nodes `a` and
    /// `b` (false outside the tsc layout or when either is synthesized).
    pub(crate) fn source_breaks(&self, a: NodeId, b: NodeId) -> bool {
        self.tsc
            && !self.is_synthetic(a)
            && !self.is_synthetic(b)
            && self.has_newline_between(self.span(a).end, self.span(b).start)
    }

    /// Layout::Tsc: line breaks in the source before and after the operator `tok` that
    /// separates the original nodes `a` and `b`.
    pub(crate) fn op_breaks(&self, a: NodeId, b: NodeId, tok: &str) -> (bool, bool) {
        // A synthesized left operand that stands at an original one's place (`exports.x`
        // for `x`) still keeps the breaks around the operator.
        // A field initializer moved into the constructor (`this.x = v`) does not (observed).
        let a_at = self.ast.span(a).start;
        let moved_field = matches!(self.ast.node(a), Node::Member { obj, .. } | Node::Index { obj, .. }
            if matches!(self.ast.node(*obj), Node::This {}));
        let replaced =
            self.tsc && self.is_synthetic(a) && a_at > 0 && !moved_field && !self.is_synthetic(b);
        if !replaced && !self.source_breaks(a, b) {
            return (false, false);
        }
        let from = if replaced { a_at } else { self.span(a).end };
        let to = self.span(b).start;
        if from >= to || !self.has_newline_between(from, to) {
            return (false, false);
        }
        match self.token_between(from, to, tok) {
            // Before: from the token in front of the operator (an erased `as T)` around `a`
            // ends where tsc's operand did).
            Some(p) => (
                self.has_newline_between(self.prev_token_end(p).clamp(from, p), p),
                self.has_newline_between(p + tok.len() as u32, self.assertion_start(to)),
            ),
            None => (false, false),
        }
    }

    /// The start of the expression at `pos` before an erased `<T>` assertion in front of it
    /// (`<T>x` printed as `x` starts at `<`), else `pos`.
    fn assertion_start(&self, pos: u32) -> u32 {
        let b = self.src.as_bytes();
        let gt = self.prev_token_end(pos);
        if gt == 0 || b.get(gt as usize - 1) != Some(&b'>') {
            return pos;
        }
        let ty_end = self.prev_token_end(gt - 1);
        if ty_end == 0 || self.erased_end(ty_end - 1) != Some(ty_end) {
            return pos;
        }
        let Some(ty_start) = self.erased_start(ty_end - 1) else {
            return pos;
        };
        let lt = self.prev_token_end(ty_start);
        if lt > 0 && b.get(lt as usize - 1) == Some(&b'<') {
            lt - 1
        } else {
            pos
        }
    }

    /// A line break (indented one more level) where the source has one, else a space.
    /// Returns the indentation levels added (to undo with [`Printer::dedent_n`]).
    pub(crate) fn break_or_space(&mut self, brk: bool) -> u32 {
        if brk {
            self.indent();
            self.newline();
            1
        } else {
            self.space();
            0
        }
    }

    pub(crate) fn dedent_n(&mut self, n: u32) {
        for _ in 0..n {
            self.dedent();
        }
    }

    // ---- comments ----

    fn comment_text(&self, c: &Comment) -> &'a str {
        self.src
            .get(c.span.start as usize..c.span.end as usize)
            .unwrap_or("")
    }

    /// Whether a comment is printed at statement level.
    fn keep_comment(&self, c: &Comment, text: &str) -> bool {
        if c.kind == CommentKind::Hashbang {
            return false;
        }
        if is_legal_comment(text) {
            return true;
        }
        if !self.opts.comments {
            return false;
        }
        if text.starts_with("//# sourceMappingURL=") || text.starts_with("//@ sourceMappingURL=") {
            // A new map is generated: the old URL would be wrong.
            return self.opts.source_map.is_none();
        }
        if self.compact {
            return is_annotation_comment(text);
        }
        true
    }

    /// End of the last erased range that lies inside `(from, to]`, if any.
    fn erased_cut(&self, from: u32, to: u32) -> Option<u32> {
        let erased = self.hints.erased;
        if erased.is_empty() {
            return None;
        }
        // Erased ranges are sorted and disjoint, so ends are sorted too.
        let i = erased.partition_point(|s| s.end <= to);
        if i == 0 {
            return None;
        }
        let e = erased[i - 1];
        (e.end > from).then_some(e.end)
    }

    /// The first erased range at or after `pos` is an import (`import ...`, elided).
    fn erased_import_after(&self, pos: u32) -> bool {
        let erased = self.hints.erased;
        let i = erased.partition_point(|s| s.start < pos);
        erased.get(i).is_some_and(|e| {
            let text = self.src.get(e.start as usize..).unwrap_or("");
            text.strip_prefix("import")
                .is_some_and(|rest| !rest.bytes().next().is_some_and(is_word_byte))
        })
    }

    /// Start of the erased range holding `pos`.
    pub(crate) fn erased_start(&self, pos: u32) -> Option<u32> {
        let erased = self.hints.erased;
        let i = erased.partition_point(|s| s.start <= pos);
        (i > 0 && erased[i - 1].end > pos).then(|| erased[i - 1].start)
    }

    /// End of the erased range holding `pos`.
    pub(crate) fn erased_end(&self, pos: u32) -> Option<u32> {
        let erased = self.hints.erased;
        let i = erased.partition_point(|s| s.start <= pos);
        (i > 0 && erased[i - 1].end > pos).then(|| erased[i - 1].end)
    }

    pub(crate) fn in_erased(&self, pos: u32) -> bool {
        let erased = self.hints.erased;
        let i = erased.partition_point(|s| s.start <= pos);
        i > 0 && erased[i - 1].end > pos
    }

    /// Emits the comments between the cursor and `pos` on their own lines (statement-level
    /// leading comments), then moves the cursor to `pos`.
    pub(crate) fn leading_comments(&mut self, pos: u32) {
        if pos <= self.comment_cursor {
            return;
        }
        let from = match self.erased_cut(self.comment_cursor, pos) {
            Some(cut) => cut,
            None => self.comment_cursor,
        };
        let comments = &self.ast.comments;
        if self.tsc
            && from != self.comment_cursor
            && self.opts.comments
            && !self.erased_import_after(self.comment_cursor)
        {
            // Triple-slash directives at the top of the file outlive the erased declaration
            // they lead (`/// <reference types="x" />` above `export interface I {}`), not an
            // elided import (observed).
            let mut j = comments.partition_point(|c| c.span.start < self.comment_cursor);
            while let Some(&c) = comments.get(j) {
                if c.span.start >= from {
                    break;
                }
                j += 1;
                let text = self.comment_text(&c);
                if c.kind == CommentKind::Line
                    && is_triple_slash_directive(text)
                    && self.trivia_start(c.span.start) == 0
                {
                    self.write_comment(&c, text, true);
                }
            }
        }
        let comments = &self.ast.comments;
        let mut i = comments.partition_point(|c| c.span.start < from);
        if self.tsc && from != self.comment_cursor {
            // Comments on the erased code's last line, after it, went with it (tsc drops
            // `declare function f(); // c`).
            let mut end = from;
            while let Some(c) = comments.get(i) {
                let gap = self
                    .src
                    .get(end as usize..c.span.start as usize)
                    .unwrap_or("x");
                if c.span.start >= pos || !gap.bytes().all(|b| b == b' ' || b == b'\t') {
                    break;
                }
                end = c.span.end;
                i += 1;
            }
        }
        while i < comments.len() {
            let c = comments[i];
            if c.span.start >= pos {
                break;
            }
            i += 1;
            let text = self.comment_text(&c);
            if self.comment_hidden(c.span.start) || !self.keep_comment(&c, text) {
                continue;
            }
            self.write_comment(&c, text, true);
        }
        self.comment_cursor = pos;
    }

    /// Prints comments after a statement on the same source line (`foo(); // why`).
    pub(crate) fn trailing_comments(&mut self, end: u32) {
        if end < self.comment_cursor {
            return;
        }
        self.comment_cursor = end;
        let comments = &self.ast.comments;
        let mut i = comments.partition_point(|c| c.span.start < end);
        while i < comments.len() {
            let c = comments[i];
            if c.newline_before || self.has_newline_between(self.comment_cursor, c.span.start) {
                break;
            }
            // Only whitespace may separate the statement and the comment.
            let gap = self
                .src
                .get(self.comment_cursor as usize..c.span.start as usize)
                .unwrap_or("x");
            if !gap.bytes().all(|b| b == b' ' || b == b'\t') {
                break;
            }
            i += 1;
            self.comment_cursor = c.span.end;
            let text = self.comment_text(&c);
            if self.comment_hidden(c.span.start) || !self.keep_comment(&c, text) {
                continue;
            }
            if self.compact && c.kind == CommentKind::Line {
                continue;
            }
            self.space();
            self.write_comment(&c, text, false);
        }
    }

    // ---- comments inside statements (Layout::Tsc) ----

    /// Whether comments at `pos` inside the current original statement may still print.
    fn expr_comments_at(&self, pos: u32) -> bool {
        self.tsc && self.opts.comments && pos >= self.comment_cursor && pos <= self.stmt_end
    }

    /// Start of the whitespace and comments directly in front of `pos`.
    /// [`crate::PrintHints::strip_numeric_separators`] applies to the numeric literal `raw`.
    pub(crate) fn strip_separators(&self, raw: &str) -> bool {
        self.hints.strip_numeric_separators && raw.contains('_')
    }

    /// End of the last token before `pos` (whitespace and comments skipped).
    pub(crate) fn prev_token_end(&self, pos: u32) -> u32 {
        let b = self.src.as_bytes();
        let mut p = (pos as usize).min(b.len());
        let comments = &self.ast.comments;
        loop {
            while p > 0 && b[p - 1].is_ascii_whitespace() {
                p -= 1;
            }
            let i = comments.partition_point(|c| (c.span.end as usize) < p);
            match comments.get(i) {
                Some(c) if c.span.end as usize == p && (c.span.start as usize) < p => {
                    p = c.span.start as usize;
                }
                _ => break,
            }
        }
        p as u32
    }

    /// Start of the first token at or after `pos` (whitespace and comments skipped).
    pub(crate) fn next_token_start(&self, pos: u32) -> u32 {
        let b = self.src.as_bytes();
        let mut p = (pos as usize).min(b.len());
        let comments = &self.ast.comments;
        loop {
            while p < b.len() && b[p].is_ascii_whitespace() {
                p += 1;
            }
            let i = comments.partition_point(|c| (c.span.start as usize) < p);
            match comments.get(i) {
                Some(c) if c.span.start as usize == p && c.span.end as usize > p => {
                    p = c.span.end as usize;
                }
                _ => break,
            }
        }
        p as u32
    }

    pub(crate) fn trivia_start(&self, pos: u32) -> u32 {
        let b = self.src.as_bytes();
        let mut p = (pos as usize).min(b.len());
        let comments = &self.ast.comments;
        loop {
            while p > 0 && b[p - 1].is_ascii_whitespace() {
                p -= 1;
            }
            let i = comments.partition_point(|c| (c.span.end as usize) < p);
            match comments.get(i) {
                Some(c) if c.span.end as usize == p && c.kind != CommentKind::Hashbang => {
                    p = c.span.start as usize;
                }
                _ => break,
            }
        }
        p as u32
    }

    /// The position after the spaces, tabs and erased code from `from` on.
    fn same_line_gap_end(&self, from: u32) -> u32 {
        let b = self.src.as_bytes();
        let mut p = from as usize;
        loop {
            while p < b.len() && (b[p] == b' ' || b[p] == b'\t') {
                p += 1;
            }
            let erased = self.hints.erased;
            let i = erased.partition_point(|s| s.start as usize <= p);
            if i > 0 && erased[i - 1].end as usize > p {
                p = erased[i - 1].end as usize;
                continue;
            }
            return p as u32;
        }
    }

    /// Layout::Tsc: the leading comments of a node starting at `pos` (the comments in the
    /// trivia right before it): `/* c */ ` on the line, `// c` and a line break. A comment the
    /// source starts on a new line starts one here too. Comments before that trivia that are
    /// still pending are dropped, as tsc drops comments it attaches to no node.
    pub(crate) fn expr_leading(&mut self, pos: u32) {
        if !self.expr_comments_at(pos) || pos == self.comment_cursor {
            return;
        }
        let from = self.trivia_start(pos).max(self.comment_cursor);
        let comments = &self.ast.comments;
        let mut i = comments.partition_point(|c| c.span.start < from);
        while let Some(&c) = comments.get(i) {
            if c.span.start >= pos {
                break;
            }
            i += 1;
            let text = self.comment_text(&c);
            if self.comment_hidden(c.span.start) || !self.keep_comment(&c, text) {
                continue;
            }
            if c.newline_before && !self.at_line_start {
                self.newline();
            }
            match c.kind {
                CommentKind::Line => {
                    self.line_comment_text(&c, text.trim_end_matches(['\r', '\n']));
                    self.forced_newline();
                }
                CommentKind::Block => {
                    self.map_pos(c.span.start);
                    if text.contains('\n') {
                        self.reindented_comment(&c, text);
                    } else {
                        self.push(text);
                    }
                    self.map_pos(c.span.end);
                    // A comment on its own line stays there.
                    if c.newline_before && self.followed_by_newline(&c) {
                        self.newline();
                    } else {
                        self.push(" ");
                    }
                }
                CommentKind::Hashbang => {}
            }
        }
        self.comment_cursor = pos;
    }

    /// Layout::Tsc: `...` and the block comments between it and the original node `arg`,
    /// written as the token's trailing comments: `... /* c */arg` (no space after them).
    pub(crate) fn dots(&mut self, arg: NodeId) {
        self.tok("...");
        if !self.tsc || self.is_synthetic(arg) {
            return;
        }
        let pos = self.span(arg).start;
        if !self.expr_comments_at(pos) || pos <= self.comment_cursor {
            return;
        }
        let from = self.trivia_start(pos).max(self.comment_cursor);
        let comments = &self.ast.comments;
        let i = comments.partition_point(|c| c.span.start < from);
        let run: Vec<Comment> = comments[i..]
            .iter()
            .take_while(|c| c.span.start < pos)
            .copied()
            .collect();
        // A comment on a later line is the argument's (`expr_leading`).
        if run
            .iter()
            .any(|c| c.kind != CommentKind::Block || c.newline_before)
        {
            return;
        }
        for c in run {
            let text = self.comment_text(&c);
            if self.comment_hidden(c.span.start) || !self.keep_comment(&c, text) {
                continue;
            }
            self.push(" ");
            self.map_pos(c.span.start);
            self.push(text);
            self.map_pos(c.span.end);
        }
        self.comment_cursor = pos;
    }

    /// Layout::Tsc: the comments right after `end` on its line (spaces, tabs and erased code
    /// between), as trailing comments: ` /* c */`, ` // c` and a line break. Not at the end of
    /// the statement, whose own trailing comments come after its `;`.
    pub(crate) fn expr_trailing(&mut self, end: u32) {
        if !self.expr_comments_at(end) || end >= self.stmt_end {
            return;
        }
        let comments = &self.ast.comments;
        let mut pos = end;
        let mut i = comments.partition_point(|c| c.span.start < end);
        while let Some(&c) = comments.get(i) {
            if c.span.start != self.same_line_gap_end(pos)
                || c.span.start >= self.stmt_end
                || self.has_newline_between(pos, c.span.start)
            {
                break;
            }
            i += 1;
            pos = c.span.end;
            let text = self.comment_text(&c);
            if !self.keep_comment(&c, text) {
                continue;
            }
            self.space();
            self.write_comment(&c, text, false);
        }
        if pos > self.comment_cursor {
            self.comment_cursor = pos;
        }
    }

    /// Layout::Tsc: trailing comments after the `,` between the original nodes `a` and `b`.
    pub(crate) fn comma_comments(&mut self, a: NodeId, b: NodeId) {
        if !self.tsc || self.is_synthetic(a) || self.is_synthetic(b) {
            return;
        }
        let (from, to) = (self.span(a).end, self.span(b).start);
        if from < to
            && let Some(p) = self.token_between(from, to, ",")
        {
            self.expr_trailing(p + 1);
        }
    }

    /// Whether a line break follows the comment `c` in the source.
    fn followed_by_newline(&self, c: &Comment) -> bool {
        self.src
            .get(c.span.end as usize..)
            .map(|rest| {
                let t = rest.trim_start_matches([' ', '\t']);
                t.starts_with('\n') || t.starts_with('\r')
            })
            .unwrap_or(false)
    }

    /// Whether the output ends with a leading block comment that stays on its line.
    pub(crate) fn after_inline_comment(&self) -> bool {
        self.inline_comment_end == self.out.len()
    }

    /// Skips the comments after `end` on the same source line (tsc drops them).
    pub(crate) fn drop_same_line_comments(&mut self, end: u32) {
        if end < self.comment_cursor {
            return;
        }
        let comments = &self.ast.comments;
        let mut pos = end;
        let mut i = comments.partition_point(|c| c.span.start < end);
        while let Some(c) = comments.get(i) {
            let gap = self
                .src
                .get(pos as usize..c.span.start as usize)
                .unwrap_or("x");
            if !gap.bytes().all(|b| b == b' ' || b == b'\t') {
                break;
            }
            pos = c.span.end;
            i += 1;
        }
        self.comment_cursor = pos;
    }

    /// A comment a transform attached to a node (`Ast::synthetic_comments`): `/* c */ ` before
    /// it or ` /* c */` after it, on the same line. Dropped without `comments` (tsc's
    /// `removeComments`) and in the compact layout. Returns whether it was printed.
    pub(crate) fn synthetic_comment(&mut self, text: &str, trailing: bool) -> bool {
        if !self.opts.comments || self.compact {
            return false;
        }
        if trailing {
            self.push(" ");
            self.push(text);
        } else {
            self.tok(text);
            self.push(" ");
        }
        true
    }

    /// Moves the comment cursor forward (comments before `pos` will not be printed).
    pub(crate) fn skip_comments_to(&mut self, pos: u32) {
        if pos > self.comment_cursor {
            self.comment_cursor = pos;
        }
    }

    fn write_comment(&mut self, c: &Comment, text: &str, own_line: bool) {
        match c.kind {
            CommentKind::Line => {
                if own_line {
                    self.newline_if_needed();
                }
                if self.tsc {
                    // tsc keeps a line comment's trailing whitespace.
                    self.line_comment_text(c, text.trim_end_matches(['\r', '\n']));
                } else {
                    self.push(text.trim_end());
                }
                self.forced_newline();
            }
            CommentKind::Block => {
                if own_line {
                    self.newline_if_needed();
                }
                // tsc maps the start and end of every comment it copies.
                self.map_pos(c.span.start);
                if self.tsc && text.contains('\n') {
                    self.reindented_comment(c, text);
                } else {
                    self.push(text);
                }
                self.map_pos(c.span.end);
                // Keep the comment on its own line if it was followed by a line break.
                let followed_by_newline = self
                    .src
                    .get(c.span.end as usize..)
                    .map(|rest| {
                        let t = rest.trim_start_matches([' ', '\t']);
                        t.starts_with('\n') || t.starts_with('\r')
                    })
                    .unwrap_or(false);
                if own_line && followed_by_newline && !self.compact {
                    self.newline();
                } else if self.tsc && !own_line {
                    // A trailing comment: what follows brings its own spacing.
                } else if !self.compact {
                    self.push(" ");
                    // tsc keeps what follows on the comment's line (`/* c */ f();`).
                    self.inline_comment_end = self.out.len();
                }
            }
            CommentKind::Hashbang => {}
        }
    }

    /// A multi-line block comment in [`Layout::Tsc`], moved as tsc moves it (observed): every
    /// line after the first is shifted by the distance the comment's start moved (its
    /// leading whitespace measured with tab stops of 4, never below column 0, rewritten as
    /// spaces) and loses its trailing whitespace; empty lines stay empty.
    fn reindented_comment(&mut self, c: &Comment, text: &str) {
        let line_start = self.src[..c.span.start as usize]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        let from_col = ws_columns(&self.src[line_start..c.span.start as usize]) as i64;
        self.flush_indent();
        let to_col = self.out[self.out.rfind('\n').map_or(0, |i| i + 1)..]
            .chars()
            .count() as i64;
        let shift = to_col - from_col;
        self.shifted_comment_lines(text, shift);
    }

    /// A multi-line comment that does not start its source line (`{/* a\n b */}` in JSX):
    /// tsc indents its later lines by their offset from that line's indentation, on top of
    /// the current indentation (observed).
    pub(crate) fn writer_indented_comment(&mut self, start: u32, text: &str) {
        let line_start = self.src[..start as usize].rfind('\n').map_or(0, |i| i + 1);
        let line = &self.src[line_start..start as usize];
        let ws = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        let base = if self.compact {
            0
        } else {
            i64::from(self.indent * u32::from(self.opts.indent_width))
        };
        let shift = base - ws_columns(ws) as i64;
        self.flush_indent();
        self.shifted_comment_lines(text, shift);
    }

    /// Writes `text`, its later lines moved `shift` columns.
    fn shifted_comment_lines(&mut self, text: &str, shift: i64) {
        let mut lines = text.split('\n');
        if let Some(first) = lines.next() {
            self.push(first.trim_end_matches('\r'));
        }
        for line in lines {
            let line = line.trim_end_matches('\r');
            self.out.push_str(self.opts.newline);
            if self.map.is_some() {
                self.gen_line += 1;
                self.gen_col = 0;
            }
            if line.is_empty() {
                continue;
            }
            let rest = line.trim_start_matches([' ', '\t']);
            let ws = &line[..line.len() - rest.len()];
            let col = (ws_columns(ws) as i64 + shift).max(0) as usize;
            let mut s = " ".repeat(col);
            s.push_str(rest.trim_end_matches([' ', '\t']));
            // Mid-comment: `at_line_start` is false, so no indentation is added.
            self.push(&s);
        }
    }

    /// A line comment's text (no line break), its start and end mapped.
    fn line_comment_text(&mut self, c: &Comment, text: &str) {
        self.map_pos(c.span.start);
        self.push(text);
        self.map_pos(c.span.start + text.len() as u32);
    }

    fn newline_if_needed(&mut self) {
        if !self.at_line_start {
            self.newline();
        }
    }

    /// Prints `/*#__PURE__*/`-style annotations directly in front of `pos` (calls and `new`).
    pub(crate) fn annotation_comments(&mut self, pos: u32) {
        let comments = &self.ast.comments;
        let i = comments.partition_point(|c| c.span.start < pos);
        if i == 0 {
            return;
        }
        // Walk back over adjacent comments separated only by whitespace.
        let mut j = i;
        let mut end = pos;
        while j > 0 {
            let c = comments[j - 1];
            if c.span.end > end {
                break;
            }
            let gap = self
                .src
                .get(c.span.end as usize..end as usize)
                .unwrap_or("x");
            if !gap.bytes().all(|b| b.is_ascii_whitespace()) {
                break;
            }
            j -= 1;
            end = c.span.start;
        }
        for &c in &comments[j..i] {
            if c.span.start < self.comment_cursor || c.kind != CommentKind::Block {
                continue;
            }
            let text = self.comment_text(&c);
            if is_annotation_comment(text) && !self.comment_hidden(c.span.start) {
                self.tok(text);
                self.space();
                self.comment_cursor = c.span.end;
            }
        }
    }

    /// Prints the hashbang line, if any.
    fn hashbang(&mut self) {
        if let Some(c) = self.ast.comments.first()
            && c.kind == CommentKind::Hashbang
        {
            let text = self.comment_text(c).trim_end().to_owned();
            self.push(&text);
            self.forced_newline();
            self.comment_cursor = c.span.end;
        }
    }

    /// End of the first comment at the comment cursor that a blank line follows, the comments
    /// before it separated only by single line breaks (tsc's detached comments: the first
    /// such run).
    fn detached_comments_end(&self) -> Option<u32> {
        let comments = &self.ast.comments;
        let mut i = comments.partition_point(|c| c.span.start < self.comment_cursor);
        let mut end = self.comment_cursor;
        let mut cut = None;
        while let Some(c) = comments.get(i) {
            let gap = self
                .src
                .get(end as usize..c.span.start as usize)
                .unwrap_or("x");
            if !gap.bytes().all(|b| b.is_ascii_whitespace()) {
                break;
            }
            end = c.span.end;
            i += 1;
            let text = self.comment_text(c);
            let rest = self.src.get(end as usize..).unwrap_or("");
            let ws = &rest[..rest.len() - rest.trim_start().len()];
            let breaks = ws.matches('\n').count() + usize::from(text.ends_with('\n'));
            if breaks >= 2 {
                // Only the first run: the next comments lead the first statement.
                cut = Some(end);
                break;
            }
        }
        cut
    }

    /// End of the triple-slash directives at the comment cursor (only whitespace between
    /// them), if the file starts with any.
    fn triple_slash_run_end(&self) -> Option<u32> {
        let comments = &self.ast.comments;
        let mut i = comments.partition_point(|c| c.span.start < self.comment_cursor);
        let mut end = self.comment_cursor;
        let mut cut = None;
        while let Some(c) = comments.get(i) {
            let gap = self
                .src
                .get(end as usize..c.span.start as usize)
                .unwrap_or("x");
            let text = self.comment_text(c);
            if !gap.bytes().all(|b| b.is_ascii_whitespace())
                || c.kind != CommentKind::Line
                || !is_triple_slash_directive(text)
            {
                break;
            }
            end = c.span.end;
            cut = Some(end);
            i += 1;
        }
        cut
    }

    // ---- entry points ----

    /// Prints a `Program`.
    pub fn program(&mut self, root: NodeId) {
        self.hashbang();
        let stmts = match self.node(root) {
            Node::Program { stmts } => self.ast.list(stmts).to_vec(),
            _ => {
                self.any(root);
                return;
            }
        };
        let prologue = stmts
            .iter()
            .take_while(|&&s| self.kind(s) == NodeKind::Directive)
            .count();
        for &s in &stmts[..prologue] {
            self.statement_in_list(s);
        }
        // Imports a transform adds at the top (`react/jsx-runtime`, `tslib`) come before the
        // file's detached comments and triple-slash directives in tsc's output.
        let mut prologue = prologue;
        if self.tsc {
            while let Some(&s) = stmts.get(prologue)
                && self.is_synthetic(s)
                && self.kind(s) == NodeKind::ImportDecl
            {
                self.statement_in_list(s);
                prologue += 1;
            }
        }
        if self.tsc
            && !self.hints.no_detached_comments
            && let Some(end) = self.detached_comments_end()
        {
            // tsc prints the file's detached comments (those a blank line separates from the
            // first statement) right after the prologue, before helpers, and keeps them when
            // that statement is erased.
            self.leading_comments(end);
        }
        if self.tsc
            && let Some(end) = self.triple_slash_run_end()
            && let [only] = stmts[prologue..]
            && self.is_synthetic(only)
            && matches!(self.node(only), Node::ExportNamed { source: None, specifiers, .. }
                if self.ast.list(specifiers).is_empty())
        {
            // The file's triple-slash directives come before the `export {};` tsc writes in
            // a module whose statements were all erased.
            self.leading_comments(end);
        }
        if !self.hints.prelude.is_empty() {
            self.newline();
            let prelude = self.hints.prelude;
            if self.compact {
                // Helpers are written for readability; keep them as is.
                self.push(prelude.trim_end());
                self.forced_newline();
            } else {
                for line in prelude.lines() {
                    if line.is_empty() {
                        self.out.push_str(self.opts.newline);
                        if self.map.is_some() {
                            self.gen_line += 1;
                            self.gen_col = 0;
                        }
                        self.at_line_start = true;
                    } else {
                        self.push(line);
                        self.forced_newline();
                    }
                }
            }
        }
        self.statements(&stmts[prologue..]);
        // Comments after the last statement.
        let end = self.span(root).end.max(self.src.len() as u32);
        self.leading_comments(end + 1);
    }

    /// Prints any node: statement, expression, pattern, type or JSX.
    pub fn any(&mut self, id: NodeId) {
        let n = self.node(id);
        if n.is_statement() || matches!(n, Node::Program { .. }) {
            if let Node::Program { stmts } = n {
                let items = self.ast.list(stmts).to_vec();
                self.statements(&items);
            } else {
                self.statement(id);
            }
        } else if crate::types::is_type_node(n.kind()) {
            self.ty(id);
        } else {
            self.expr(id, crate::expr::Prec::Lowest);
        }
    }
}

/// Bytes that continue an identifier/number/keyword token.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'\\' || b >= 0x80
}

/// The column after the whitespace `ws` (tabs stop at multiples of 4; other characters
/// count one).
fn ws_columns(ws: &str) -> usize {
    ws.chars().fold(0, |col, ch| {
        if ch == '\t' {
            (col / 4 + 1) * 4
        } else {
            col + 1
        }
    })
}

fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        s.len() as u32
    } else {
        s.chars().map(|c| c.len_utf16() as u32).sum()
    }
}

/// `/*! ... */`, `@license`, `@preserve` (kept even without comments).
pub(crate) fn is_legal_comment(text: &str) -> bool {
    if text.starts_with("/*!") || text.starts_with("//!") {
        return true;
    }
    // Every comment is checked: find each `@` (a memchr scan) instead of two substring searches.
    let mut rest = text;
    while let Some(i) = rest.find('@') {
        rest = &rest[i + 1..];
        if rest.starts_with("license") || rest.starts_with("preserve") {
            return true;
        }
    }
    false
}

/// `/// <reference ... />` and `/// <amd-...>` directives.
pub(crate) fn is_triple_slash_directive(text: &str) -> bool {
    text.strip_prefix("///")
        .map(str::trim_start)
        .is_some_and(|t| t.starts_with("<reference") || t.starts_with("<amd-"))
}

/// Bundler annotations that must stay attached to the following call.
pub(crate) fn is_annotation_comment(text: &str) -> bool {
    let inner = text
        .strip_prefix("/*")
        .and_then(|t| t.strip_suffix("*/"))
        .map(str::trim)
        .unwrap_or("");
    matches!(
        inner,
        "#__PURE__" | "@__PURE__" | "#__NO_SIDE_EFFECTS__" | "@__NO_SIDE_EFFECTS__"
    )
}

/// Decodes the `\uXXXX` and `\u{X}` escapes of an identifier's source text; `None` if it has
/// any other backslash or an invalid escape.
fn unescape_name(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let after = rest[i + 1..].strip_prefix('u')?;
        let (hex, tail) = if let Some(braced) = after.strip_prefix('{') {
            let close = braced.find('}')?;
            (&braced[..close], &braced[close + 1..])
        } else {
            (after.get(..4)?, after.get(4..)?)
        };
        if hex.is_empty() || hex.len() > 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
        rest = tail;
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod unescape_tests {
    use super::unescape_name;

    #[test]
    fn decodes_identifier_escapes() {
        assert_eq!(unescape_name("x\\u0078").as_deref(), Some("xx"));
        assert_eq!(unescape_name("#\\u{78}").as_deref(), Some("#x"));
        assert_eq!(unescape_name("\\u{110000}"), None);
        assert_eq!(unescape_name("\\x41"), None);
        assert_eq!(unescape_name("\\u00"), None);
    }
}
