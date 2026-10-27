//! Where signature help applies at a cursor, and which argument it is at.
//!
//! Rules learned from tsc 7's answers on its fourslash tests (`signatureHelp*`,
//! `genericParameterHelp*`, `signatureHelp_contextual`) and from
//! `tsc: tsc/internal/ls/signaturehelp.go` (how the argument list holding the cursor is
//! found), written here on our AST and source text:
//!
//! - Three kinds of site: the arguments (or the type arguments) of a call, `new`, tagged
//!   template or `super(...)`; a possible type argument list that did not parse as one
//!   (`f<|`, `new C<|`, `let x: C<|`); and the parameter list of a function or arrow, or a
//!   parenthesized expression (`takesCb((|))`), whose contextual type has a call signature.
//! - The innermost site wins: the one whose opening `(` or `<` comes last.
//! - A contextual site needs the token before the cursor to be that list's `(` or one of its
//!   top-level commas, or the cursor to be inside one of the function's parameters.
//! - A possible type argument list is read backwards from the cursor: closing brackets skip
//!   to their opener, `>` needs one more `<`, commas count the arguments before the cursor, and
//!   the `<` that opens the list must follow an identifier that is not a declaration's name.
//!   Tokens that cannot be part of a type end the search.
//! - The argument index counts the top-level commas before the cursor; a spread of a tuple
//!   stands for its fixed elements. The argument count is the commas plus one, or zero for
//!   an empty list.

use super::locate::{ident_at, node_at};
use super::position::token_before;
use ts_r_ast::{Ast, Node, NodeId};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::TupleElement;

/// Levels walked up from the cursor at most.
const MAX_UP: usize = 10_000;
/// Tokens read backwards at most when looking for a type argument list.
const MAX_TOKENS: usize = 4096;

pub enum SiteKind {
    /// The arguments (`type_list`: the type arguments) of a call, `new`, tagged template or
    /// `super(...)`.
    Call {
        call: NodeId,
        callee: NodeId,
        kind: SigKind,
        type_list: bool,
    },
    /// `f<|` with `f` this identifier, not parsed as a call.
    TypeArgs { called: NodeId },
    /// A function, arrow, method (its function expression) or parenthesized expression
    /// whose contextual type gives the signature.
    Contextual { node: NodeId },
    /// The attributes of a JSX opening tag (`<C |`): the tag's signatures.
    Jsx { opening: NodeId, name: NodeId },
}

pub struct Site {
    pub kind: SiteKind,
    /// The argument (or parameter) the cursor is at.
    pub index: u32,
    /// How many arguments the list appears to have.
    pub count: u32,
    /// Where the list opens: the innermost site wins.
    open: u32,
}

/// The signature help sites at `offset`, innermost first.
pub fn sites_at(ck: &mut Ck<'_, '_>, file: FileId, offset: u32) -> Vec<Site> {
    let call = call_site(ck, file, offset);
    let ctx = contextual_site(ck, file, offset);
    let targs = type_args_site(ck, file, offset);
    let jsx = jsx_site(ck, file, offset);
    // A call before a type argument list with the same opener: the call's.
    let mut out: Vec<Site> = [call, ctx, targs, jsx].into_iter().flatten().collect();
    out.sort_by(|a, b| b.open.cmp(&a.open));
    out
}

/// The innermost call, `new` or tagged template whose arguments or type arguments hold
/// `offset`.
fn call_site(ck: &mut Ck<'_, '_>, file: FileId, offset: u32) -> Option<Site> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let found = call_site_from(ck, file, node_at(ast, offset), offset);
    if found.is_some() {
        return found;
    }
    // An unclosed list can end before the whitespace the cursor is in.
    let (tok_at, _) = token_before(ast, text, offset)?;
    call_site_from(ck, file, node_at(ast, tok_at), offset)
}

fn call_site_from(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    start: Option<NodeId>,
    offset: u32,
) -> Option<Site> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let bytes = text.as_bytes();
    let parents = c.file(file).parents();
    let mut cur = start;
    for _ in 0..MAX_UP {
        let n = cur?;
        let (callee, type_args, args, kind) = match *ast.node(n) {
            Node::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                let kind = if matches!(ast.node(callee), Node::Super {}) {
                    SigKind::Construct
                } else {
                    SigKind::Call
                };
                (callee, type_args, args, kind)
            }
            Node::New {
                callee,
                type_args,
                has_args,
                args,
            } if has_args || type_args.is_some() => (callee, type_args, args, SigKind::Construct),
            Node::TaggedTemplate { tag, quasi, .. } => {
                if let Some(arg) = template_argument(bytes, ast.span(quasi).start, offset) {
                    let count = match *ast.node(quasi) {
                        Node::TemplateLit { parts } => ast.list(parts).len() as u32 / 2 + 1,
                        _ => 1,
                    };
                    return Some(Site {
                        kind: SiteKind::Call {
                            call: n,
                            callee: tag,
                            kind: SigKind::Call,
                            type_list: false,
                        },
                        index: arg,
                        count,
                        open: ast.span(quasi).start,
                    });
                }
                cur = parents.parent(n);
                continue;
            }
            _ => {
                cur = parents.parent(n);
                continue;
            }
        };
        let span = ast.span(n);
        // Inside the type arguments: `f<A, |>(...)`.
        if let Some(ta) = type_args {
            let ts = ast.span(ta);
            let closed = bytes.get(ts.end as usize - 1) == Some(&b'>') && ts.end > ts.start + 1;
            if ts.start < offset && (offset < ts.end || !closed) {
                let close = if closed { ts.end - 1 } else { ts.end };
                let Node::TypeArgs { args: targs } = *ast.node(ta) else {
                    return None;
                };
                let index = commas(text, ts.start + 1, offset, true);
                let total = commas(text, ts.start + 1, close, true);
                let count = if ast.list(targs).is_empty() && total == 0 {
                    0
                } else {
                    total + 1
                };
                return Some(Site {
                    kind: SiteKind::Call {
                        call: n,
                        callee,
                        kind,
                        type_list: true,
                    },
                    index,
                    count,
                    open: ts.start,
                });
            }
        }
        // The `(` after the callee (and its type arguments).
        let after = type_args.map_or(ast.span(callee).end, |t| ast.span(t).end);
        let mut p = after as usize;
        while p < bytes.len() && p < span.end as usize && bytes[p] != b'(' {
            p += 1;
        }
        if p < bytes.len() && bytes[p] == b'(' && (p as u32) < offset {
            let closed =
                span.end as usize > p + 1 && bytes.get(span.end as usize - 1) == Some(&b')');
            if !closed || offset < span.end {
                let open = p as u32;
                let close = if closed { span.end - 1 } else { span.end };
                let list = ast.list(args).to_vec();
                let spread_before = spread_extra(ck, file, &list, offset);
                let spread_all = spread_extra(ck, file, &list, close);
                // Arguments the parser separated without commas count too (`f(a b |c`).
                let written_before = match token_before(ast, text, offset) {
                    Some((t, _)) => list.iter().filter(|&&a| ast.span(a).end <= t).count() as i64,
                    None => 0,
                };
                let index = (commas(text, open + 1, offset, false) as i64 + spread_before)
                    .max(written_before)
                    .max(0);
                let total = commas(text, open + 1, close, false);
                let count = if list.is_empty() && total == 0 {
                    0
                } else {
                    (total as i64 + 1 + spread_all).max(0) as u32
                };
                return Some(Site {
                    kind: SiteKind::Call {
                        call: n,
                        callee,
                        kind,
                        type_list: false,
                    },
                    index: index as u32,
                    count,
                    open,
                });
            }
        }
        cur = parents.parent(n);
    }
    None
}

/// The JSX opening tag whose attributes hold `offset` (after the tag name, before its `>`
/// or `/>`, or anywhere after the name in a tag left open).
fn jsx_site(ck: &mut Ck<'_, '_>, file: FileId, offset: u32) -> Option<Site> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    let before = token_before(ast, text, offset).and_then(|(t, _)| node_at(ast, t));
    let opening = [node_at(ast, offset), before]
        .into_iter()
        .flatten()
        .find_map(|n| jsx_opening_above(ast, parents, n))?;
    let Node::JsxOpening {
        name, self_closing, ..
    } = *ast.node(opening)
    else {
        return None;
    };
    let sp = ast.span(opening);
    let closed = text.as_bytes().get((sp.end as usize).checked_sub(1)?) == Some(&b'>');
    let end = match (closed, self_closing) {
        (false, _) => text.len() as u32 + 1,
        (true, false) => sp.end - 1,
        (true, true) => sp.end.saturating_sub(2),
    };
    let name_end = ast.span(name).end;
    (name_end <= offset && offset < end).then_some(Site {
        kind: SiteKind::Jsx { opening, name },
        index: 0,
        count: 1,
        open: name_end,
    })
}

/// The JSX opening tag holding `n` (within a few levels).
fn jsx_opening_above(ast: &Ast, parents: &ts_r_ast::ParentMap, n: NodeId) -> Option<NodeId> {
    let mut cur = Some(n);
    for _ in 0..64 {
        let n = cur?;
        if matches!(ast.node(n), Node::JsxOpening { .. }) {
            return Some(n);
        }
        cur = parents.parent(n);
    }
    None
}

/// What the spreads before `to` add to the comma count: a spread of a tuple stands for its
/// fixed elements (`f(...pair, |)` is at argument 2), any other spread for none.
fn spread_extra(ck: &mut Ck<'_, '_>, file: FileId, args: &[NodeId], to: u32) -> i64 {
    let ast = ck.c.ast(file);
    let mut extra = 0i64;
    for &a in args {
        if ast.span(a).end >= to {
            break;
        }
        let Node::Spread { arg } = *ast.node(a) else {
            continue;
        };
        let t = ck.quietly(|ck| ck.check_expr(Loc::new(file, arg)));
        let n = match ck.store().tuple_shape_of(t) {
            Some(shape) => shape
                .elements
                .iter()
                .take_while(|e| e.kind == TupleElement::REQUIRED)
                .count() as i64,
            None => 0,
        };
        extra += n - 1;
    }
    extra
}

/// The parameter list of a function or arrow, or a parenthesized expression, that the
/// cursor is in (see the module docs); `None` when its context gives no signature is decided
/// by the caller.
fn contextual_site(ck: &mut Ck<'_, '_>, file: FileId, offset: u32) -> Option<Site> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    let (tok_at, tok) = token_before(ast, text, offset)?;
    let mut cur = super::position::node_covering(ast, tok_at);
    // Past the token's own list only a parameter around the cursor counts.
    let mut only_param = false;
    for _ in 0..MAX_UP {
        let n = cur?;
        match *ast.node(n) {
            Node::Param { .. } => {
                let f = parents.parent(n)?;
                let list = function_params(ast, f)?;
                let index = ast.list(list).iter().position(|&p| p == n)? as u32;
                let open = params_open(ast, text, f)?;
                let count = param_count(ast, text, f, open);
                return Some(Site {
                    kind: SiteKind::Contextual { node: f },
                    index,
                    count,
                    open,
                });
            }
            Node::Arrow { .. } | Node::FunctionExpr { .. } if !only_param => {
                let open = params_open(ast, text, n)?;
                let close = params_close(ast, text, n, open);
                let at_list = (tok == "(" && tok_at == open)
                    || (tok == ","
                        && open < tok_at
                        && tok_at < close
                        && is_top_comma(text, open, tok_at));
                if !at_list {
                    only_param = true;
                    cur = parents.parent(n);
                    continue;
                }
                return Some(Site {
                    kind: SiteKind::Contextual { node: n },
                    index: commas(text, open + 1, offset, false),
                    count: param_count(ast, text, n, open),
                    open,
                });
            }
            Node::Paren { .. } if !only_param => {
                let open = ast.span(n).start;
                let at_list = (tok == "(" && tok_at == open)
                    || (tok == "," && is_top_comma(text, open, tok_at));
                if !at_list {
                    only_param = true;
                    cur = parents.parent(n);
                    continue;
                }
                let end = ast.span(n).end;
                let close = if text.as_bytes().get(end as usize - 1) == Some(&b')') {
                    end - 1
                } else {
                    end
                };
                let total = commas(text, open + 1, close, false);
                let empty = text
                    .get(open as usize + 1..close as usize)
                    .is_none_or(|s| s.trim().is_empty());
                return Some(Site {
                    kind: SiteKind::Contextual { node: n },
                    index: commas(text, open + 1, offset, false),
                    count: if empty && total == 0 { 0 } else { total + 1 },
                    open,
                });
            }
            Node::Seq { .. } | Node::Missing {} | Node::Ident { .. } => {}
            _ => only_param = true,
        }
        cur = parents.parent(n);
    }
    None
}

/// The parameter list of a function-like node.
fn function_params(ast: &Ast, f: NodeId) -> Option<ts_r_ast::NodeList> {
    match *ast.node(f) {
        Node::Arrow { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::FunctionDecl { params, .. } => Some(params),
        _ => None,
    }
}

/// The `(` that opens a function's or arrow's parameter list (`None` for `x => ...`).
fn params_open(ast: &Ast, text: &str, f: NodeId) -> Option<u32> {
    let b = text.as_bytes();
    let list = function_params(ast, f)?;
    let span = ast.span(f);
    if let Some(&first) = ast.list(list).first() {
        let mut p = ast.span(first).start as usize;
        while p > span.start as usize {
            p -= 1;
            match b[p] {
                b'(' => return Some(p as u32),
                c if c.is_ascii_whitespace() => {}
                _ => return None,
            }
        }
        return None;
    }
    let from = match *ast.node(f) {
        Node::FunctionExpr {
            name, type_params, ..
        } => type_params.or(name).map_or(span.start, |n| ast.span(n).end),
        Node::Arrow { type_params, .. } => type_params.map_or(span.start, |n| ast.span(n).end),
        _ => span.start,
    };
    let mut p = from as usize;
    while p < span.end as usize && p < b.len() {
        if b[p] == b'(' {
            return Some(p as u32);
        }
        p += 1;
    }
    None
}

/// The end of a parameter list opened at `open`: its `)`, or the function's end.
fn params_close(ast: &Ast, text: &str, f: NodeId, open: u32) -> u32 {
    let b = text.as_bytes();
    let end = ast.span(f).end as usize;
    let mut depth = 0i32;
    let mut p = open as usize;
    while p < end && p < b.len() {
        match b[p] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return p as u32;
                }
            }
            _ => {}
        }
        p += 1;
    }
    end as u32
}

/// The parameter count of a list opened at `open` (a trailing comma adds one).
fn param_count(ast: &Ast, text: &str, f: NodeId, open: u32) -> u32 {
    let close = params_close(ast, text, f, open);
    let total = commas(text, open + 1, close, false);
    let n = function_params(ast, f).map_or(0, |l| ast.list(l).len() as u32);
    if n == 0 && total == 0 { 0 } else { total + 1 }
}

/// Whether the comma at `at` is at nesting depth 0 after the opener at `open`.
fn is_top_comma(text: &str, open: u32, at: u32) -> bool {
    commas(text, open + 1, at + 1, false) > commas(text, open + 1, at, false)
}

/// A possible type argument list before the cursor that did not parse as a call's.
fn type_args_site(ck: &mut Ck<'_, '_>, file: FileId, offset: u32) -> Option<Site> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    if !text.contains('<') {
        return None;
    }
    let parents = c.file(file).parents();
    let mut pos = offset;
    let mut remaining = 0u32;
    let mut n_args = 0u32;
    for _ in 0..MAX_TOKENS {
        let (at, tok) = prev_token(ast, text, pos)?;
        match tok {
            Tok::Less => {
                let (mut ident_at_pos, mut t) = prev_token(ast, text, at)?;
                if t == Tok::QuestionDot {
                    (ident_at_pos, t) = prev_token(ast, text, ident_at_pos)?;
                }
                let Tok::Word(w) = t else { return None };
                if !is_identifier(w) {
                    return None;
                }
                if remaining == 0 {
                    let called = ident_at(ast, ident_at_pos)?;
                    if !matches!(ast.node(called), Node::Ident { .. }) {
                        return None;
                    }
                    // A declaration's own type parameters: `class C<|`.
                    if let Some(p) = parents.parent(called)
                        && ts_r_binder::decl_name_node(ast, p) == Some(called)
                    {
                        return None;
                    }
                    // The type arguments of a parsed call are a call site.
                    if let Some(p) = parents.parent(called)
                        && let Node::Call {
                            type_args: Some(_),
                            callee,
                            ..
                        }
                        | Node::New {
                            type_args: Some(_),
                            callee,
                            ..
                        } = *ast.node(p)
                        && callee == called
                    {
                        return None;
                    }
                    return Some(Site {
                        kind: SiteKind::TypeArgs { called },
                        index: n_args,
                        count: n_args + 1,
                        open: at,
                    });
                }
                remaining -= 1;
                pos = at;
                continue;
            }
            Tok::Greater(k) => remaining += k,
            Tok::Close(open) => {
                pos = matching_open(ast, text, at, open)?;
                continue;
            }
            Tok::Comma => n_args += 1,
            // Inside a tuple or type literal in the list: the list is still ahead.
            Tok::Open(b'[' | b'{') => n_args = 0,
            Tok::Arrow | Tok::Str | Tok::Punct(b'.' | b'|' | b'?' | b':') => {}
            Tok::Word(w) if !STATEMENT_WORDS.contains(&w) => {}
            _ => return None,
        }
        pos = at;
    }
    None
}

/// Words that cannot be part of a type.
const STATEMENT_WORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "return",
    "switch",
    "throw",
    "try",
    "var",
    "while",
    "with",
    "yield",
    "await",
];

fn is_identifier(w: &str) -> bool {
    w.chars()
        .next()
        .is_some_and(|ch| ch.is_alphabetic() || ch == '_' || ch == '$')
        && !STATEMENT_WORDS.contains(&w)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok<'t> {
    Less,
    /// `>`, `>>` or `>>>`: how many `<` it closes.
    Greater(u32),
    Arrow,
    QuestionDot,
    Comma,
    Str,
    Open(u8),
    /// A closing bracket and the opener it needs.
    Close(u8),
    Word(&'t str),
    Punct(u8),
}

/// The token before `pos` (strings whole, `=>`, `?.` and `>>` as one token).
fn prev_token<'t>(ast: &Ast, text: &'t str, pos: u32) -> Option<(u32, Tok<'t>)> {
    let (at, t) = token_before(ast, text, pos)?;
    let b = text.as_bytes();
    let first = t.as_bytes()[0];
    if first.is_ascii_alphanumeric() || first == b'_' || first == b'$' || !first.is_ascii() {
        return Some((at, Tok::Word(t)));
    }
    let prev = |i: u32| {
        if i == 0 {
            None
        } else {
            b.get(i as usize - 1).copied()
        }
    };
    Some(match first {
        b'<' => (at, Tok::Less),
        b'>' => {
            if prev(at) == Some(b'=') {
                (at - 1, Tok::Arrow)
            } else {
                let mut k = 1;
                let mut s = at;
                while k < 3 && prev(s) == Some(b'>') {
                    k += 1;
                    s -= 1;
                }
                (s, Tok::Greater(k))
            }
        }
        b'.' if prev(at) == Some(b'?') => (at - 1, Tok::QuestionDot),
        b',' => (at, Tok::Comma),
        b'"' | b'\'' => {
            // Back to the opening quote on the same line.
            let mut p = at as usize;
            loop {
                if p == 0 || b[p - 1] == b'\n' {
                    return None;
                }
                p -= 1;
                if b[p] == first && (p == 0 || b[p - 1] != b'\\') {
                    break;
                }
            }
            (p as u32, Tok::Str)
        }
        b'(' | b'[' | b'{' => (at, Tok::Open(first)),
        b')' => (at, Tok::Close(b'(')),
        b']' => (at, Tok::Close(b'[')),
        b'}' => (at, Tok::Close(b'{')),
        other => (at, Tok::Punct(other)),
    })
}

/// The start of the opener matching the closer at `at`.
fn matching_open(ast: &Ast, text: &str, at: u32, open: u8) -> Option<u32> {
    let mut depth = 1u32;
    let mut pos = at;
    for _ in 0..MAX_TOKENS {
        let (p, t) = prev_token(ast, text, pos)?;
        match t {
            Tok::Close(o) if o == open => depth += 1,
            Tok::Open(o) if o == open => {
                depth -= 1;
                if depth == 0 {
                    return Some(p);
                }
            }
            _ => {}
        }
        pos = p;
    }
    None
}

/// The tag argument at `offset` in the template starting at `start` (its backtick): 0 in the
/// template text, `i` inside the `i`-th `${...}`. `None` before the opening backtick or after
/// the closing one.
pub fn template_argument(text: &[u8], start: u32, offset: u32) -> Option<u32> {
    #[derive(Clone, Copy)]
    enum Mode {
        Text,
        Expr(u32),
    }
    if text.get(start as usize) != Some(&b'`') || offset <= start {
        return None;
    }
    let end = (offset as usize).min(text.len());
    let mut stack = vec![Mode::Text];
    let mut arg = 0;
    let mut i = start as usize + 1;
    while i < end {
        let b = text[i];
        match stack.last_mut()? {
            Mode::Text => match b {
                b'\\' => i += 1,
                b'`' => {
                    stack.pop();
                    if stack.is_empty() {
                        return None;
                    }
                }
                b'$' if text.get(i + 1) == Some(&b'{') && i + 1 < end => {
                    if stack.len() == 1 {
                        arg += 1;
                    }
                    stack.push(Mode::Expr(0));
                    i += 1;
                }
                _ => {}
            },
            Mode::Expr(depth) => match b {
                b'{' => *depth += 1,
                b'}' if *depth == 0 => {
                    stack.pop();
                }
                b'}' => *depth -= 1,
                b'`' => stack.push(Mode::Text),
                q @ (b'"' | b'\'') => {
                    i += 1;
                    while i < end && text[i] != q && text[i] != b'\n' {
                        if text[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    Some(if stack.len() == 1 { 0 } else { arg })
}

/// Commas at nesting depth 0 between `from` and `to` (skipping strings and comments);
/// `angles` counts `<`/`>` as brackets too (type argument lists).
pub fn commas(text: &str, from: u32, to: u32, angles: bool) -> u32 {
    let b = text.as_bytes();
    let (mut i, end) = (from as usize, (to as usize).min(b.len()));
    let mut depth = 0i32;
    let mut commas = 0;
    while i < end {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b'<' if angles => depth += 1,
            b'>' if angles && i > 0 && b[i - 1] != b'=' => depth -= 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => commas += 1,
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < end && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < end && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < end && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    commas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commas_at_depth_zero() {
        let t = "f(a, [b, c], \"x,y\", g(1, 2), ";
        assert_eq!(commas(t, 2, t.len() as u32, false), 4);
        assert_eq!(commas("f(/* , */ a", 2, 11, false), 0);
        let t = "f<A<B, C>, (x: D) => E, |";
        assert_eq!(commas(t, 2, t.len() as u32, true), 2);
    }

    #[test]
    fn template_argument_counts_substitutions() {
        let t = b"f `a ${ x } b ${ g `c ${ 1 }` } d`;";
        let at = |s: &str| t.windows(s.len()).position(|w| w == s.as_bytes()).unwrap() as u32;
        assert_eq!(template_argument(t, 2, 2), None); // before the backtick
        assert_eq!(template_argument(t, 2, 4), Some(0)); // in the text
        assert_eq!(template_argument(t, 2, at("$")), Some(0));
        assert_eq!(template_argument(t, 2, at("$") + 1), Some(0)); // between `$` and `{`
        assert_eq!(template_argument(t, 2, at("x")), Some(1));
        assert_eq!(template_argument(t, 2, at("} b")), Some(1)); // before the `}`
        assert_eq!(template_argument(t, 2, at(" b")), Some(0));
        assert_eq!(template_argument(t, 2, at("1")), Some(2)); // in the nested template
        assert_eq!(template_argument(t, 2, at(" d")), Some(0));
        assert_eq!(template_argument(t, 2, at(";")), None); // after the closing backtick
        // Unterminated templates; an escaped `\${` is text.
        assert_eq!(template_argument(b"f `${ a } ${ b", 2, 14), Some(2));
        assert_eq!(template_argument(b"f `${ a } \\${ b", 2, 15), Some(0));
    }
}
