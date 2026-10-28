//! Occurrences that are not found through a symbol: the keywords of a statement or a
//! function (document highlights on `if`, `return`, `break`, `async`, a modifier, ...), and
//! the special references of `this`, `super`, labels and primitive type keywords.
//!
//! Each rule was observed on tsc 7.0.2's answers to the fourslash tests (the
//! `getOccurrences*`, `findAllRefsThisKeyword*`, `findAllRefsPrimitive` and `renameLabel*`
//! tests); `src/services/documentHighlights.ts` and `findAllReferences.ts` explain them. All
//! the walks here use explicit stacks, so deep trees cannot overflow the stack.

use ts_r_ast::{
    Ast, KeywordTypeKind, MethodKind, ModifierToken, Modifiers, Node, NodeId, ParentMap, Span,
};
use ts_r_checker::Checker;
use ts_r_diagnostics::FileId;

use super::locate::node_at;

/// A span in a file.
pub type Occurrence = (FileId, u32, u32);

/// One file's tree, text and parents.
#[derive(Clone, Copy)]
pub struct Doc<'a> {
    pub file: FileId,
    pub ast: &'a Ast,
    pub text: &'a str,
    pub parents: &'a ParentMap,
    /// The file is a module (`this` at its top level is not the global `this`).
    pub is_module: bool,
}

/// How far parent walks go before giving up (deeper trees are not real code).
const MAX_DEPTH: usize = 4096;

impl<'a> Doc<'a> {
    pub fn new(c: &Checker<'a>, file: FileId) -> Doc<'a> {
        let sf = c.file(file);
        Doc {
            file,
            ast: &sf.ast,
            text: sf.text.as_str(),
            parents: sf.parents(),
            is_module: sf.bound.is_module,
        }
    }

    fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.parents.parent(n)
    }

    fn node(&self, n: NodeId) -> Node {
        *self.ast.node(n)
    }

    fn span(&self, n: NodeId) -> Span {
        self.ast.span(n)
    }

    fn bytes(&self) -> &'a [u8] {
        self.text.as_bytes()
    }

    /// The word (identifier or keyword) containing `offset`, or ending at it.
    pub(super) fn word_at(&self, offset: u32) -> Option<(u32, u32)> {
        let b = self.bytes();
        let o = (offset as usize).min(b.len());
        let mut s = o;
        if !(o < b.len() && is_word(b[o])) && (o == 0 || !is_word(b[o - 1])) {
            return None;
        }
        while s > 0 && is_word(b[s - 1]) {
            s -= 1;
        }
        let mut e = o;
        while e < b.len() && is_word(b[e]) {
            e += 1;
        }
        (s < e).then_some((s as u32, e as u32))
    }

    /// The word after `pos`, past whitespace and comments.
    fn next_word(&self, pos: u32) -> Option<(u32, u32)> {
        let b = self.bytes();
        let mut i = pos as usize;
        while i < b.len() {
            match b[i] {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => i += 1,
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                        i += 1;
                    }
                    i = (i + 2).min(b.len());
                }
                // Non-ASCII whitespace (no-break space, BOM, ...) is skipped too.
                0xc2 if b.get(i + 1) == Some(&0xa0) => i += 2,
                0xef if b.get(i + 1) == Some(&0xbb) && b.get(i + 2) == Some(&0xbf) => i += 3,
                _ => break,
            }
        }
        let s = i;
        while i < b.len() && is_word(b[i]) {
            i += 1;
        }
        (s < i).then_some((s as u32, i as u32))
    }

    /// `kw` when it is the word at the start of `n`.
    pub(super) fn keyword_at_start(&self, n: NodeId, kw: &str) -> Option<(u32, u32)> {
        let s = self.span(n).start;
        let w = self.next_word(s)?;
        (w.0 == s && self.word(w) == kw).then_some(w)
    }

    pub(super) fn word(&self, w: (u32, u32)) -> &'a str {
        self.text.get(w.0 as usize..w.1 as usize).unwrap_or("")
    }

    /// The word after `pos` when it is `kw`.
    pub(super) fn keyword_after(&self, pos: u32, kw: &str) -> Option<(u32, u32)> {
        let w = self.next_word(pos)?;
        (self.word(w) == kw).then_some(w)
    }

    fn name(&self, n: NodeId) -> Option<&'a str> {
        match self.node(n) {
            Node::Ident { name } => Some(self.ast.atom(name)),
            _ => None,
        }
    }

    /// Every node under `root` (itself included) for which `f` holds, not descending into
    /// nodes for which `stop` holds (they are still offered to `f`).
    fn collect(
        &self,
        root: NodeId,
        stop: impl Fn(&Doc<'a>, NodeId) -> bool,
        mut f: impl FnMut(NodeId),
    ) {
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            f(n);
            if n != root && stop(self, n) {
                continue;
            }
            let at = stack.len();
            self.ast.for_each_child(n, |c| stack.push(c));
            stack[at..].reverse();
        }
    }

    fn modifier(&self, n: NodeId, flag: Modifiers) -> Option<ModifierToken> {
        self.ast
            .modifier_tokens_of(n)
            .iter()
            .find(|m| m.flag == flag)
            .copied()
    }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

fn span_of(w: (u32, u32)) -> Span {
    Span::new(w.0, w.1)
}

fn is_function_like(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::IndexSignature { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
    )
}

fn is_class_like(n: &Node) -> bool {
    matches!(n, Node::ClassDecl { .. } | Node::ClassExpr { .. })
}

fn is_type_node(n: &Node) -> bool {
    matches!(
        n,
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::ThisType { .. }
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::TypeLiteral { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::InferType { .. }
            | Node::MappedType { .. }
            | Node::ParenType { .. }
            | Node::TypePredicate { .. }
            | Node::ImportType { .. }
    )
}

fn is_loop(n: &Node) -> bool {
    matches!(
        n,
        Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
    )
}

fn is_static(n: &Node) -> bool {
    match n {
        Node::MethodDef { modifiers, .. }
        | Node::PropertyDef { modifiers, .. }
        | Node::IndexSignature { modifiers, .. } => modifiers.contains(Modifiers::STATIC),
        Node::StaticBlock { .. } => true,
        _ => false,
    }
}

// ---- Keyword highlights ----

/// The keyword occurrences to highlight for the keyword at `offset`, or `None` when the
/// cursor is not on a keyword these rules know (then highlights come from references).
/// Highlights on the tag name of a JSX element's opening or closing tag: the two tags
/// (`tsc: tsc/internal/ls/documenthighlights.go`); none for a self-closing element.
pub fn jsx_tag_highlights(d: &Doc<'_>, offset: u32) -> Option<Vec<(u32, u32)>> {
    let n = node_around(d, d.word_at(offset)?)?;
    let tag = d.parent(n)?;
    match d.node(tag) {
        Node::JsxOpening {
            name,
            self_closing: false,
            ..
        } if name == n => {}
        Node::JsxClosing { name } if name == n => {}
        _ => return None,
    }
    let Node::JsxElement {
        opening, closing, ..
    } = d.node(d.parent(tag)?)
    else {
        return Some(Vec::new());
    };
    Some(
        std::iter::once(opening)
            .chain(closing)
            .map(|t| (d.span(t).start, d.span(t).end))
            .collect(),
    )
}

pub fn keyword_highlights(d: &Doc<'_>, offset: u32) -> Option<Vec<(u32, u32)>> {
    let w = d.word_at(offset)?;
    let word = d.word(w);
    if let Some(m) = modifier_token(d, w) {
        return Some(modifier_occurrences(d, m));
    }
    let mut n = node_around(d, w)?;
    for _ in 0..3 {
        if let Some(r) = keyword_of(d, n, word, w) {
            return Some(r);
        }
        n = d.parent(n)?;
    }
    None
}

/// The innermost node that holds all of the word `w`.
fn node_around(d: &Doc<'_>, w: (u32, u32)) -> Option<NodeId> {
    let mut n = node_at(d.ast, w.0)?;
    for _ in 0..MAX_DEPTH {
        let s = d.span(n);
        if s.start <= w.0 && w.1 <= s.end {
            return Some(n);
        }
        n = d.parent(n)?;
    }
    None
}

/// Highlights when `w` (spelled `word`) is a keyword of `n` itself.
fn keyword_of(d: &Doc<'_>, n: NodeId, word: &str, w: (u32, u32)) -> Option<Vec<(u32, u32)>> {
    let at_start = d.span(n).start == w.0;
    let is = |kw: &str| word == kw && at_start;
    match d.node(n) {
        Node::If { cons, alt, .. } => {
            let on_else = word == "else"
                && alt.is_some()
                && d.keyword_after(d.span(cons).end, "else") == Some(w);
            (is("if") || on_else).then(|| if_else(d, n))
        }
        Node::Return { .. } if is("return") => Some(returns_of_return(d, n)),
        Node::Throw { .. } if is("throw") => Some(throws_of_throw(d, n)),
        Node::Try { .. }
            if is("try") || (word == "finally" && finally_keyword(d, n) == Some(w)) =>
        {
            Some(try_keywords(d, n))
        }
        Node::CatchClause { .. } if is("catch") => {
            let t = d.parent(n)?;
            matches!(d.node(t), Node::Try { .. }).then(|| try_keywords(d, t))
        }
        Node::Switch { .. } if is("switch") => Some(switch_keywords(d, n)),
        Node::SwitchCase { .. } if is("case") || is("default") => {
            let s = d.parent(n)?;
            matches!(d.node(s), Node::Switch { .. }).then(|| switch_keywords(d, s))
        }
        Node::Break { .. } if is("break") => Some(jump_keywords(d, n)),
        Node::Continue { .. } if is("continue") => Some(jump_keywords(d, n)),
        Node::For { .. } | Node::ForIn { .. } | Node::ForOf { .. } if is("for") => {
            Some(loop_keywords(d, n))
        }
        Node::While { .. } if is("while") => Some(loop_keywords(d, n)),
        Node::DoWhile { body, .. }
            if is("do")
                || (word == "while" && d.keyword_after(d.span(body).end, "while") == Some(w)) =>
        {
            Some(loop_keywords(d, n))
        }
        Node::MethodDef {
            kind: MethodKind::Constructor,
            key,
            ..
        } if word == "constructor" && d.span(key).start == w.0 => Some(constructors(d, n)),
        Node::MethodDef {
            kind: MethodKind::Get | MethodKind::Set,
            ..
        }
        | Node::MethodSignature {
            kind: MethodKind::Get | MethodKind::Set,
            ..
        } if (word == "get" || word == "set") && accessor_keyword(d, n) == Some(w) => {
            Some(accessors(d, n))
        }
        Node::Await { .. } if is("await") => Some(
            containing_function(d, n)
                .map(|f| async_await(d, f))
                .unwrap_or_default(),
        ),
        Node::Yield { .. } if is("yield") => Some(
            containing_function(d, n)
                .map(|f| yields(d, f))
                .unwrap_or_default(),
        ),
        _ => None,
    }
}

/// `if` and `else` of an `if`/`else if` chain; `else if` on one line is one span.
fn if_else(d: &Doc<'_>, n: NodeId) -> Vec<(u32, u32)> {
    let mut top = n;
    for _ in 0..MAX_DEPTH {
        match d.parent(top).map(|p| (p, d.node(p))) {
            Some((p, Node::If { alt: Some(a), .. })) if a == top => top = p,
            _ => break,
        }
    }
    let mut kws: Vec<((u32, u32), bool)> = Vec::new();
    let mut cur = top;
    for _ in 0..MAX_DEPTH {
        let Node::If { cons, alt, .. } = d.node(cur) else {
            break;
        };
        if let Some(k) = d.keyword_at_start(cur, "if") {
            kws.push((k, false));
        }
        if alt.is_some()
            && let Some(k) = d.keyword_after(d.span(cons).end, "else")
        {
            kws.push((k, true));
        }
        match alt {
            Some(a) if matches!(d.node(a), Node::If { .. }) => cur = a,
            _ => break,
        }
    }
    let b = d.bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < kws.len() {
        let (k, is_else) = kws[i];
        if is_else
            && let Some(&(next, false)) = kws.get(i + 1)
            && b[k.1 as usize..next.0 as usize]
                .iter()
                .all(|&c| matches!(c, b' ' | b'\t' | 0x0b | 0x0c))
        {
            out.push((k.0, next.1));
            i += 2;
            continue;
        }
        out.push(k);
        i += 1;
    }
    out
}

/// The nearest function-like node around `n` (the `FunctionExpr` of a method).
fn containing_function(d: &Doc<'_>, n: NodeId) -> Option<NodeId> {
    let mut cur = n;
    for _ in 0..MAX_DEPTH {
        cur = d.parent(cur)?;
        if is_function_like(&d.node(cur)) {
            return Some(cur);
        }
    }
    None
}

fn fn_body(d: &Doc<'_>, f: NodeId) -> Option<NodeId> {
    match d.node(f) {
        Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body,
        Node::Arrow { body, .. } => Some(body),
        _ => None,
    }
}

/// `return` keywords in the statements of a function body (not in nested functions).
fn returns_in(d: &Doc<'_>, body: NodeId, out: &mut Vec<(u32, u32)>) {
    d.collect(
        body,
        |d, n| {
            !matches!(
                d.node(n),
                Node::Block { .. }
                    | Node::If { .. }
                    | Node::DoWhile { .. }
                    | Node::While { .. }
                    | Node::For { .. }
                    | Node::ForIn { .. }
                    | Node::ForOf { .. }
                    | Node::With { .. }
                    | Node::Switch { .. }
                    | Node::SwitchCase { .. }
                    | Node::Labeled { .. }
                    | Node::Try { .. }
                    | Node::CatchClause { .. }
            )
        },
        |n| {
            if matches!(d.node(n), Node::Return { .. })
                && let Some(k) = d.keyword_at_start(n, "return")
            {
                out.push(k);
            }
        },
    );
}

/// `throw` keywords of the throws `n` owns: not in functions, and not in the block of a
/// `try` that has a `catch`.
fn owned_throws(d: &Doc<'_>, n: NodeId, out: &mut Vec<(u32, u32)>) {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        match d.node(n) {
            Node::Throw { .. } => {
                if let Some(k) = d.keyword_at_start(n, "throw") {
                    out.push(k);
                }
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                stack.extend(finalizer);
                stack.push(handler.unwrap_or(block));
            }
            ref x if is_function_like(x) => {}
            _ => d.ast.for_each_child(n, |c| stack.push(c)),
        }
    }
}

fn returns_of_return(d: &Doc<'_>, r: NodeId) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let Some(body) = containing_function(d, r).and_then(|f| fn_body(d, f)) else {
        return out;
    };
    if !matches!(d.node(body), Node::Block { .. }) {
        return out;
    }
    returns_in(d, body, &mut out);
    owned_throws(d, body, &mut out);
    out
}

fn is_function_block(d: &Doc<'_>, n: NodeId) -> bool {
    matches!(d.node(n), Node::Block { .. })
        && d.parent(n)
            .is_some_and(|p| is_function_like(&d.node(p)) && fn_body(d, p) == Some(n))
}

fn throws_of_throw(d: &Doc<'_>, t: NodeId) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut child = t;
    let mut owner = None;
    for _ in 0..MAX_DEPTH {
        let Some(p) = d.parent(child) else { break };
        if is_function_block(d, p) || matches!(d.node(p), Node::Program { .. }) {
            owner = Some(p);
            break;
        }
        if let Node::Try {
            block,
            handler: Some(_),
            ..
        } = d.node(p)
            && block == child
        {
            owner = Some(child);
            break;
        }
        child = p;
    }
    let Some(owner) = owner else { return out };
    owned_throws(d, owner, &mut out);
    if is_function_block(d, owner) {
        returns_in(d, owner, &mut out);
    }
    out
}

fn finally_keyword(d: &Doc<'_>, t: NodeId) -> Option<(u32, u32)> {
    let Node::Try {
        block,
        handler,
        finalizer: Some(_),
    } = d.node(t)
    else {
        return None;
    };
    d.keyword_after(d.span(handler.unwrap_or(block)).end, "finally")
}

/// `try` and `finally` (tsc 7 leaves `catch` out).
fn try_keywords(d: &Doc<'_>, t: NodeId) -> Vec<(u32, u32)> {
    d.keyword_at_start(t, "try")
        .into_iter()
        .chain(finally_keyword(d, t))
        .collect()
}

fn label_of<'a>(d: &Doc<'a>, n: NodeId) -> Option<&'a str> {
    match d.node(n) {
        Node::Break { label: Some(l) } | Node::Continue { label: Some(l) } => d.name(l),
        _ => None,
    }
}

/// Is `n` labelled `name` (through a chain of labels)?
fn labelled_by(d: &Doc<'_>, n: NodeId, name: &str) -> bool {
    let mut cur = n;
    for _ in 0..MAX_DEPTH {
        match d.parent(cur).map(|p| (p, d.node(p))) {
            Some((p, Node::Labeled { label, .. })) => {
                if d.name(label) == Some(name) {
                    return true;
                }
                cur = p;
            }
            _ => return false,
        }
    }
    false
}

/// The loop or switch a `break`/`continue` leaves.
fn jump_owner(d: &Doc<'_>, j: NodeId) -> Option<NodeId> {
    let is_continue = matches!(d.node(j), Node::Continue { .. });
    let label = label_of(d, j);
    let mut cur = j;
    for _ in 0..MAX_DEPTH {
        cur = d.parent(cur)?;
        let n = d.node(cur);
        let target = is_loop(&n) || (!is_continue && matches!(n, Node::Switch { .. }));
        if target && label.is_none_or(|l| labelled_by(d, cur, l)) {
            return Some(cur);
        }
        if is_function_like(&n) {
            return None;
        }
    }
    None
}

/// The `break`/`continue` keywords under `n` that leave `owner` (`breaks_only`: just
/// `break`s).
fn owned_jumps(
    d: &Doc<'_>,
    n: NodeId,
    owner: NodeId,
    breaks_only: bool,
    out: &mut Vec<(u32, u32)>,
) {
    d.collect(
        n,
        |d, n| is_function_like(&d.node(n)),
        |j| {
            let kw = match d.node(j) {
                Node::Break { .. } => "break",
                Node::Continue { .. } if !breaks_only => "continue",
                _ => return,
            };
            if jump_owner(d, j) == Some(owner)
                && let Some(k) = d.keyword_at_start(j, kw)
            {
                out.push(k);
            }
        },
    );
}

fn loop_keywords(d: &Doc<'_>, l: NodeId) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let body = match d.node(l) {
        Node::For { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::While { body, .. } => {
            let kw = if matches!(d.node(l), Node::While { .. }) {
                "while"
            } else {
                "for"
            };
            out.extend(d.keyword_at_start(l, kw));
            body
        }
        Node::DoWhile { body, .. } => {
            if let Some(k) = d.keyword_at_start(l, "do") {
                out.push(k);
                out.extend(d.keyword_after(d.span(body).end, "while"));
            }
            body
        }
        _ => return out,
    };
    owned_jumps(d, body, l, false, &mut out);
    out
}

fn switch_keywords(d: &Doc<'_>, s: NodeId) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let Node::Switch { cases, .. } = d.node(s) else {
        return out;
    };
    out.extend(d.keyword_at_start(s, "switch"));
    for &case in d.ast.list(cases) {
        if let Node::SwitchCase { test, .. } = d.node(case) {
            let kw = if test.is_some() { "case" } else { "default" };
            out.extend(d.keyword_at_start(case, kw));
        }
        owned_jumps(d, case, s, true, &mut out);
    }
    out
}

fn jump_keywords(d: &Doc<'_>, j: NodeId) -> Vec<(u32, u32)> {
    match jump_owner(d, j) {
        Some(o) if matches!(d.node(o), Node::Switch { .. }) => switch_keywords(d, o),
        Some(o) => loop_keywords(d, o),
        None => Vec::new(),
    }
}

fn members(d: &Doc<'_>, n: NodeId) -> Vec<NodeId> {
    match d.node(n) {
        Node::ClassDecl { members, .. }
        | Node::ClassExpr { members, .. }
        | Node::TypeLiteral { members } => d.ast.list(members).to_vec(),
        Node::Interface { body, .. } => d.ast.list(body).to_vec(),
        Node::ObjectLit { props } => d.ast.list(props).to_vec(),
        _ => Vec::new(),
    }
}

/// `constructor` keywords of the class's constructors.
pub fn constructors(d: &Doc<'_>, ctor: NodeId) -> Vec<(u32, u32)> {
    let Some(class) = d.parent(ctor) else {
        return Vec::new();
    };
    members(d, class)
        .into_iter()
        .filter_map(|m| match d.node(m) {
            Node::MethodDef {
                kind: MethodKind::Constructor,
                key,
                ..
            } => {
                let s = d.span(key);
                (d.word((s.start, s.end)) == "constructor").then_some((s.start, s.end))
            }
            _ => None,
        })
        .collect()
}

/// The `get`/`set` keyword of an accessor.
fn accessor_keyword(d: &Doc<'_>, n: NodeId) -> Option<(u32, u32)> {
    let mut from = d.span(n).start;
    if let Node::MethodDef { decorators, .. } = d.node(n)
        && let Some(&last) = d.ast.list(decorators).last()
    {
        from = from.max(d.span(last).end);
    }
    if let Some(m) = d.ast.modifier_tokens_of(n).last() {
        from = from.max(m.span.end);
    }
    let w = d.next_word(from)?;
    matches!(d.word(w), "get" | "set").then_some(w)
}

fn accessor_parts(d: &Doc<'_>, n: NodeId) -> Option<(NodeId, bool)> {
    match d.node(n) {
        Node::MethodDef {
            kind: MethodKind::Get | MethodKind::Set,
            key,
            modifiers,
            ..
        } => Some((key, modifiers.contains(Modifiers::STATIC))),
        Node::MethodSignature {
            kind: MethodKind::Get | MethodKind::Set,
            key,
            ..
        } => Some((key, false)),
        _ => None,
    }
}

fn key_text<'a>(d: &Doc<'a>, key: NodeId) -> Option<String> {
    match d.node(key) {
        Node::Ident { name } => Some(d.ast.atom(name).to_string()),
        Node::PrivateName { name } => Some(format!("#{}", d.ast.atom(name))),
        Node::StringLit { value } => Some(d.ast.atom(value).to_string()),
        Node::NumberLit { .. } => {
            let s = d.span(key);
            Some(d.word((s.start, s.end)).to_string())
        }
        _ => None,
    }
}

/// The `get`/`set` keywords of the accessors of one property.
fn accessors(d: &Doc<'_>, n: NodeId) -> Vec<(u32, u32)> {
    let Some((key, is_static)) = accessor_parts(d, n) else {
        return Vec::new();
    };
    let Some(name) = key_text(d, key) else {
        return d.keyword_at_start(n, "get").into_iter().collect();
    };
    let Some(owner) = d.parent(n) else {
        return Vec::new();
    };
    members(d, owner)
        .into_iter()
        .filter(|&m| {
            accessor_parts(d, m)
                .is_some_and(|(k, st)| st == is_static && key_text(d, k).as_deref() == Some(&name))
        })
        .filter_map(|m| accessor_keyword(d, m))
        .collect()
}

/// The node whose `async` modifier marks `f` (a method's `FunctionExpr` has it on the
/// method).
fn function_owner(d: &Doc<'_>, f: NodeId) -> NodeId {
    match d.parent(f).map(|p| (p, d.node(p))) {
        Some((p, Node::MethodDef { value, .. })) if value == f => p,
        _ => f,
    }
}

/// Stop at nodes `await` and `yield` cannot be seen through.
fn stops_await(d: &Doc<'_>, n: NodeId) -> bool {
    let x = d.node(n);
    is_function_like(&x)
        || is_class_like(&x)
        || is_type_node(&x)
        || matches!(
            x,
            Node::Interface { .. } | Node::Module { .. } | Node::TypeAlias { .. }
        )
}

fn async_await(d: &Doc<'_>, f: NodeId) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    if let Some(m) = d.modifier(function_owner(d, f), Modifiers::ASYNC) {
        out.push((m.span.start, m.span.end));
    }
    d.collect(f, stops_await, |n| {
        if matches!(d.node(n), Node::Await { .. })
            && let Some(k) = d.keyword_at_start(n, "await")
        {
            out.push(k);
        }
    });
    out
}

fn yields(d: &Doc<'_>, f: NodeId) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    d.collect(f, stops_await, |n| {
        if matches!(d.node(n), Node::Yield { .. })
            && let Some(k) = d.keyword_at_start(n, "yield")
        {
            out.push(k);
        }
    });
    out
}

/// The declaration a modifier keyword belongs to.
fn modifier_owner(d: &Doc<'_>, m: ModifierToken) -> Option<NodeId> {
    let mut owner = node_at(d.ast, m.span.start)?;
    for _ in 0..MAX_DEPTH {
        if d.ast
            .modifier_tokens_of(owner)
            .iter()
            .any(|t| t.span == m.span)
        {
            return Some(owner);
        }
        owner = d.parent(owner)?;
    }
    None
}

/// The modifier token spelled by the word `w`, if it is one.
fn modifier_token(d: &Doc<'_>, w: (u32, u32)) -> Option<ModifierToken> {
    d.ast
        .modifiers_in(span_of(w))
        .iter()
        .find(|m| m.span.start == w.0 && m.span.end == w.1)
        .copied()
}

/// A modifier keyword: the same modifier on the declarations around this one.
fn modifier_occurrences(d: &Doc<'_>, m: ModifierToken) -> Vec<(u32, u32)> {
    if m.flag.intersects(
        Modifiers::DEFAULT
            .union(Modifiers::IN)
            .union(Modifiers::OUT),
    ) {
        return Vec::new();
    }
    let Some(owner) = modifier_owner(d, m) else {
        return Vec::new();
    };
    if m.flag == Modifiers::ASYNC {
        let f = match d.node(owner) {
            Node::MethodDef { value, .. } => value,
            _ => owner,
        };
        return async_await(d, f);
    }
    let Some(container) = d.parent(owner) else {
        return Vec::new();
    };
    let mut search: Vec<NodeId> = Vec::new();
    match d.node(container) {
        Node::Program { stmts }
        | Node::ModuleBlock { stmts }
        | Node::Block { stmts }
        | Node::SwitchCase { body: stmts, .. } => {
            if m.flag == Modifiers::ABSTRACT && matches!(d.node(owner), Node::ClassDecl { .. }) {
                search = members(d, owner);
                search.push(owner);
            } else {
                search = d.ast.list(stmts).to_vec();
            }
        }
        Node::FunctionDecl { params, .. } => search = d.ast.list(params).to_vec(),
        Node::FunctionExpr { params, .. } => {
            if let Some((md, Node::MethodDef { kind, .. })) =
                d.parent(container).map(|p| (p, d.node(p)))
                && matches!(kind, MethodKind::Method | MethodKind::Constructor)
            {
                search = d.ast.list(params).to_vec();
                if let Some(class) = d.parent(md)
                    && is_class_like(&d.node(class))
                {
                    search.extend(members(d, class));
                }
            }
        }
        Node::ClassDecl { .. }
        | Node::ClassExpr { .. }
        | Node::Interface { .. }
        | Node::TypeLiteral { .. } => {
            search = members(d, container);
            let access = Modifiers::PUBLIC
                .union(Modifiers::PRIVATE)
                .union(Modifiers::PROTECTED)
                .union(Modifiers::READONLY);
            if m.flag.intersects(access) {
                let ctor = search.iter().copied().find(|&x| {
                    matches!(
                        d.node(x),
                        Node::MethodDef {
                            kind: MethodKind::Constructor,
                            ..
                        }
                    )
                });
                if let Some(Node::MethodDef { value, .. }) = ctor.map(|c| d.node(c))
                    && let Node::FunctionExpr { params, .. } = d.node(value)
                {
                    search.extend_from_slice(d.ast.list(params));
                }
            } else if m.flag == Modifiers::ABSTRACT {
                search.push(container);
            }
        }
        _ => {}
    }
    search
        .into_iter()
        .filter_map(|n| d.modifier(n, m.flag))
        .map(|t| (t.span.start, t.span.end))
        .collect()
}

// ---- Go to definition on keywords ----

/// Where go to definition on a keyword leads.
pub enum KeywordTarget {
    /// These spans (possibly none).
    Spans(Vec<(u32, u32)>),
    /// The definition of this declaration name (a modifier goes to its declaration).
    Name(NodeId),
    /// `override`: the member of the base class, else the member's own name.
    Override { member: NodeId, name: NodeId },
    /// `super`: the base class (and for `super(...)`, the constructor called).
    Super(NodeId),
}

/// Go to definition on a keyword: `return`/`await`/`yield` go to their function, `case` and
/// `default` to their `switch`, `this` to its declaration, a modifier to the declaration it
/// modifies. `None` when the cursor is not on such a keyword: a word that is itself the
/// declared name (`let` of `var let = 1`) is a name, never a keyword standing for one.
pub fn keyword_definition(d: &Doc<'_>, offset: u32) -> Option<KeywordTarget> {
    let t = keyword_definition_at(d, offset)?;
    if let KeywordTarget::Name(n) | KeywordTarget::Override { name: n, .. } = t
        && let Some(w) = d.word_at(offset)
        && d.span(n).start < w.1
        && w.0 < d.span(n).end
    {
        return None;
    }
    Some(t)
}

fn keyword_definition_at(d: &Doc<'_>, offset: u32) -> Option<KeywordTarget> {
    if let Some(n) = node_at(d.ast, offset) {
        match d.node(n) {
            Node::Super {} => return Some(KeywordTarget::Super(n)),
            Node::This {} | Node::ThisType {} => {
                return Some(KeywordTarget::Spans(
                    this_definition(d, n).into_iter().collect(),
                ));
            }
            Node::Ident { .. } if is_this(d, n) => {
                return Some(KeywordTarget::Spans(
                    this_definition(d, n).into_iter().collect(),
                ));
            }
            _ => {}
        }
    }
    let w = d.word_at(offset)?;
    let word = d.word(w);
    if let Some(m) = modifier_token(d, w) {
        let owner = modifier_owner(d, m)?;
        // A modifier of a variable statement has no declaration of its own.
        if matches!(d.node(owner), Node::VarDecl { .. }) {
            return Some(KeywordTarget::Spans(Vec::new()));
        }
        let name = declaration_name(d, owner)?;
        return Some(if m.flag == Modifiers::OVERRIDE {
            KeywordTarget::Override {
                member: owner,
                name,
            }
        } else {
            KeywordTarget::Name(name)
        });
    }
    let mut n = node_around(d, w)?;
    for _ in 0..3 {
        let at_start = d.span(n).start == w.0;
        let is = |kw: &str| word == kw && at_start;
        let target = match d.node(n) {
            Node::Return { .. } if is("return") => Some(function_target(d, n)),
            Node::Await { .. } if is("await") => Some(function_target(d, n)),
            Node::Yield { .. } if is("yield") => Some(function_target(d, n)),
            Node::SwitchCase { .. } if is("case") || is("default") => d
                .parent(n)
                .and_then(|s| d.keyword_at_start(s, "switch"))
                .map(|k| vec![k]),
            // `function` goes to its function, `const` to the first variable, `class`
            // (and the other declaration keywords) to the declared name.
            Node::FunctionDecl { name, .. } | Node::FunctionExpr { name, .. }
                if word == "function" =>
            {
                if let Some(name) = name {
                    return Some(KeywordTarget::Name(name));
                }
                Some(function_target_of(d, n))
            }
            Node::VarDecl { .. } if matches!(word, "const" | "let" | "var") => {
                return declaration_name(d, n).map(KeywordTarget::Name);
            }
            Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Interface { .. }
            | Node::Enum { .. }
            | Node::TypeAlias { .. }
            | Node::Module { .. }
                if matches!(
                    word,
                    "class" | "interface" | "enum" | "type" | "namespace" | "module"
                ) =>
            {
                match ts_r_binder::decl_name_node(d.ast, n) {
                    Some(name) if d.span(name).start >= w.1 => {
                        return Some(KeywordTarget::Name(name));
                    }
                    _ => None,
                }
            }
            Node::ExportDefaultExpr { .. }
                if word == "default"
                    && d.keyword_after(d.span(n).start + 6, "default") == Some(w) =>
            {
                let s = d.span(n);
                Some(vec![(s.start, s.end)])
            }
            _ => None,
        };
        if let Some(t) = target {
            return Some(KeywordTarget::Spans(t));
        }
        n = d.parent(n)?;
    }
    None
}

/// The declared name a keyword before it stands for (a modifier, `function`, `class`,
/// `const`, ...), as references and type definition read it.
pub fn keyword_name(d: &Doc<'_>, offset: u32) -> Option<NodeId> {
    match keyword_definition(d, offset) {
        Some(KeywordTarget::Name(n) | KeywordTarget::Override { name: n, .. }) => Some(n),
        Some(_) => None,
        None => accessor_keyword_name(d, offset).or_else(|| export_equals_name(d, offset)),
    }
}

/// The `export` of `export = N`: the name `N` (renamed with it, as tsc 7 does).
fn export_equals_name(d: &Doc<'_>, offset: u32) -> Option<NodeId> {
    let w = d.word_at(offset)?;
    if d.word(w) != "export" {
        return None;
    }
    let n = node_around(d, w)?;
    match d.node(n) {
        Node::ExportAssignment { expr }
            if d.span(n).start == w.0 && matches!(d.node(expr), Node::Ident { .. }) =>
        {
            Some(expr)
        }
        _ => None,
    }
}

/// The node find-all-references reads for the keyword at `offset`: the declared name of a
/// declaration keyword or modifier ([`keyword_name`], not `default`), else the operand a
/// statement, expression or type keyword introduces (observed on tsc 7.0.2's answers to
/// `referencesFor{Statement,Expression,Type,Declaration}Keywords`):
///
/// - `import`/`type` of `import x = ...` the name, `require` its module specifier;
///   `import`/`type`/`export` of an import or export declaration its only specifier's name
///   (`* as n`'s `n`), else the module specifier when there is no clause; `from` the module
///   specifier; `as` the name it introduces; `export` of `export =` / `export default` the
///   expression;
/// - `new`, `void`, `typeof`, `delete`, `await`, `yield` their operand, `in` / `instanceof`
///   the right operand, `as` / `satisfies` the type;
/// - `extends` / `implements` with one type that type; `extends` of a type parameter its
///   constraint, of a conditional type the type after it; `infer` and a mapped type's `in`
///   the type parameter; `keyof` / `readonly` / `unique` their operand.
///
/// An operand is read as its name: an identifier, a member access's property, a type
/// reference's (last) name, an array type's element.
pub fn reference_keyword(d: &Doc<'_>, offset: u32) -> Option<NodeId> {
    let w = d.word_at(offset)?;
    // A word inside a comment is not a keyword.
    let i = d.ast.comments.partition_point(|c| c.span.end < w.1);
    if d.ast.comments.get(i).is_some_and(|c| c.span.start <= w.0) {
        return None;
    }
    let word = d.word(w);
    if word != "default"
        && let Some(n) = keyword_name(d, offset)
    {
        return Some(n);
    }
    let n = node_around(d, w)?;
    if matches!(d.node(n), Node::Ident { .. } | Node::PrivateName { .. }) {
        return None;
    }
    let at_start = d.span(n).start == w.0;
    let only = |list: ts_r_ast::NodeList| match d.ast.list(list) {
        [one] => Some(*one),
        _ => None,
    };
    let spec_name = |s: NodeId| match d.node(s) {
        Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local } => {
            Some(local)
        }
        Node::ImportSpecifier { local, .. } => Some(local),
        Node::ExportSpecifier {
            local, exported, ..
        } => Some(exported.unwrap_or(local)),
        _ => None,
    };
    let target = match d.node(n) {
        Node::ImportEquals {
            name, module_ref, ..
        } => match word {
            "import" | "type" if d.span(name).start >= w.1 => Some(name),
            "require" => match d.node(module_ref) {
                Node::ExternalModuleRef { expr } => Some(expr),
                _ => None,
            },
            _ => None,
        },
        Node::ExternalModuleRef { expr } if word == "require" => Some(expr),
        Node::ImportDecl {
            has_clause,
            specifiers,
            source,
            ..
        } => match word {
            "from" => Some(source),
            "import" if at_start && !has_clause => Some(source),
            "import" | "type" => only(specifiers).and_then(spec_name),
            _ => None,
        },
        Node::ImportNamespaceSpecifier { local } if word == "as" => Some(local),
        Node::ImportSpecifier { local, .. } if word == "as" => Some(local),
        Node::ExportSpecifier {
            exported: Some(e), ..
        } if word == "as" => Some(e),
        Node::ExportAll {
            exported, source, ..
        } => match word {
            "from" => Some(source),
            "as" => exported,
            "export" | "type" => Some(exported.unwrap_or(source)),
            _ => None,
        },
        Node::ExportNamed {
            specifiers, source, ..
        } => match word {
            "from" => source,
            "export" | "type" => match d.ast.list(specifiers) {
                [] => source,
                [one] => spec_name(*one),
                _ => None,
            },
            _ => None,
        },
        Node::ExportAssignment { expr } | Node::ExportDefaultExpr { expr }
            if word == "export" && at_start =>
        {
            operand_name(d, expr)
        }
        Node::New { callee, .. } if word == "new" && at_start => operand_name(d, callee),
        Node::Unary { op, arg }
            if at_start
                && matches!(
                    (op, word),
                    (ts_r_ast::UnaryOp::Void, "void")
                        | (ts_r_ast::UnaryOp::TypeOf, "typeof")
                        | (ts_r_ast::UnaryOp::Delete, "delete")
                ) =>
        {
            operand_name(d, arg)
        }
        Node::Await { arg } if word == "await" && at_start => operand_name(d, arg),
        Node::Yield { arg: Some(a), .. } if word == "yield" && at_start => operand_name(d, a),
        Node::Binary { right, op, .. }
            if matches!(
                (op, word),
                (ts_r_ast::BinaryOp::In, "in") | (ts_r_ast::BinaryOp::InstanceOf, "instanceof")
            ) =>
        {
            operand_name(d, right)
        }
        Node::As { ty, .. } if word == "as" => operand_name(d, ty),
        Node::Satisfies { ty, .. } if word == "satisfies" => operand_name(d, ty),
        Node::HeritageClause { types, .. } if matches!(word, "extends" | "implements") => {
            only(types).and_then(|t| operand_name(d, t))
        }
        Node::Interface { extends, name, .. } if word == "extends" && d.span(name).end <= w.0 => {
            only(extends).and_then(|t| operand_name(d, t))
        }
        Node::TypeParam {
            name, constraint, ..
        } => match word {
            "extends" => constraint.and_then(|t| operand_name(d, t)),
            "in" if d
                .parent(n)
                .is_some_and(|p| matches!(d.node(p), Node::MappedType { .. })) =>
            {
                Some(name)
            }
            _ => None,
        },
        Node::MappedType { type_param, .. } if word == "in" => match d.node(type_param) {
            Node::TypeParam { name, .. } => Some(name),
            _ => None,
        },
        Node::ConditionalType { extends, .. } if word == "extends" => operand_name(d, extends),
        Node::InferType { type_param } if word == "infer" => match d.node(type_param) {
            Node::TypeParam { name, .. } => Some(name),
            _ => None,
        },
        Node::TypeOperator { ty, .. }
            if at_start && matches!(word, "keyof" | "readonly" | "unique") =>
        {
            operand_name(d, ty)
        }
        Node::TypeQuery { expr_name, .. } if word == "typeof" && at_start => {
            operand_name(d, expr_name)
        }
        _ => None,
    };
    target.filter(|&t| d.span(t).start != w.0 || d.span(t).end != w.1)
}

/// The name an operand stands for in [`reference_keyword`].
fn operand_name(d: &Doc<'_>, n: NodeId) -> Option<NodeId> {
    let mut n = n;
    for _ in 0..16 {
        n = match d.node(n) {
            Node::Ident { .. } | Node::StringLit { .. } => return Some(n),
            Node::Member { prop, .. } => return Some(prop),
            Node::QualifiedName { right, .. } => return Some(right),
            Node::Paren { expr } => expr,
            Node::ParenType { ty } => ty,
            Node::TypeRef { name, .. } => name,
            Node::ArrayType { elem } => elem,
            Node::ExprWithTypeArgs { expr, .. } => expr,
            _ => return None,
        };
    }
    None
}

/// The key of the accessor whose `get` or `set` keyword is at `offset`.
fn accessor_keyword_name(d: &Doc<'_>, offset: u32) -> Option<NodeId> {
    let w = d.word_at(offset)?;
    let kind = match d.word(w) {
        "get" => MethodKind::Get,
        "set" => MethodKind::Set,
        _ => return None,
    };
    let mut n = node_around(d, w)?;
    for _ in 0..3 {
        if let Node::MethodDef { kind: k, key, .. } = d.node(n)
            && k == kind
            && d.span(key).start >= w.1
            && d.span(n).start <= w.0
        {
            return Some(key);
        }
        n = d.parent(n)?;
    }
    None
}

/// The name a declaration is shown at (the first variable of `export const a = 1, b`).
fn declaration_name(d: &Doc<'_>, n: NodeId) -> Option<NodeId> {
    if let Node::VarDecl { decls, .. } = d.node(n) {
        let first = *d.ast.list(decls).first()?;
        return match d.node(first) {
            Node::VarDeclarator { name, .. } => Some(name),
            _ => None,
        };
    }
    ts_r_binder::decl_name_node(d.ast, n)
}

/// The function a `return`, `await` or `yield` belongs to, shown at its name: a method's
/// key, the variable an unnamed function initializes, else the whole function.
fn function_target(d: &Doc<'_>, n: NodeId) -> Vec<(u32, u32)> {
    let mut cur = n;
    let mut f = None;
    for _ in 0..MAX_DEPTH {
        let Some(p) = d.parent(cur) else { break };
        let x = d.node(p);
        // `[yield x]() {}`: a computed key is evaluated by the method's container, but tsc
        // stops at the method.
        if is_function_like(&x) || matches!(x, Node::MethodDef { .. }) {
            f = Some(p);
            break;
        }
        cur = p;
    }
    match f {
        Some(f) => function_target_of(d, f),
        None => Vec::new(),
    }
}

/// A function shown at its name (see [`function_target`]).
fn function_target_of(d: &Doc<'_>, f: NodeId) -> Vec<(u32, u32)> {
    let owner = function_owner(d, f);
    let name = match d.node(owner) {
        Node::MethodDef { key, .. } => Some(key),
        Node::FunctionDecl { name, .. } | Node::FunctionExpr { name, .. } => name,
        _ => None,
    };
    let name = name.or_else(|| match d.parent(owner).map(|p| d.node(p)) {
        Some(Node::VarDeclarator {
            name,
            init: Some(i),
            ..
        }) if i == owner && matches!(d.node(name), Node::Ident { .. }) => Some(name),
        _ => None,
    });
    let s = d.span(name.unwrap_or(owner));
    vec![(s.start, s.end)]
}

/// The first parameter of a function when it is a `this` parameter.
fn this_param(d: &Doc<'_>, f: NodeId) -> Option<NodeId> {
    let f = match d.node(f) {
        Node::MethodDef { value, .. } => value,
        _ => f,
    };
    let params = match d.node(f) {
        Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => params,
        _ => return None,
    };
    let first = *d.ast.list(params).first()?;
    match d.node(first) {
        Node::Param { pattern, .. } if d.name(pattern) == Some("this") => Some(pattern),
        _ => None,
    }
}

/// Where `this` is declared: the function's `this` parameter, else the class.
fn this_definition(d: &Doc<'_>, n: NodeId) -> Option<(u32, u32)> {
    let cont = this_container(d, n)?;
    if let Some(p) = this_param(d, cont) {
        let s = d.span(p);
        return Some((s.start, s.end));
    }
    if !is_class_element(&d.node(cont)) {
        return None;
    }
    let class = d.parent(cont)?;
    let name = match d.node(class) {
        Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name?,
        _ => return None,
    };
    let s = d.span(name);
    Some((s.start, s.end))
}

/// The class a member belongs to, if it is a class member.
pub fn member_class(d: &Doc<'_>, member: NodeId) -> Option<NodeId> {
    let c = d.parent(member)?;
    is_class_like(&d.node(c)).then_some(c)
}

/// The class member `super` at `n` is used in, and whether it is static.
pub fn super_member(d: &Doc<'_>, n: NodeId) -> Option<(NodeId, bool)> {
    let m = super_container(d, n)?;
    Some((m, is_static(&d.node(m))))
}

/// Whether a class member is static.
pub fn member_is_static(d: &Doc<'_>, m: NodeId) -> bool {
    is_static(&d.node(m))
}

// ---- `this`, `super`, labels, primitive type keywords ----

/// `this` as an expression, a type, or a parameter name.
fn is_this(d: &Doc<'_>, n: NodeId) -> bool {
    match d.node(n) {
        Node::This {} | Node::ThisType {} => true,
        Node::Ident { .. } => {
            d.name(n) == Some("this") && (is_param_name(d, n) || is_type_query_head(d, n))
        }
        _ => false,
    }
}

/// Whether `n` is the first name of a `typeof` query (`typeof this`, `typeof this.x`).
fn is_type_query_head(d: &Doc<'_>, n: NodeId) -> bool {
    let mut cur = n;
    for _ in 0..MAX_DEPTH {
        let Some(p) = d.parent(cur) else {
            return false;
        };
        match d.node(p) {
            Node::QualifiedName { left, .. } if left == cur => cur = p,
            Node::TypeQuery { expr_name, .. } => return expr_name == cur,
            _ => return false,
        }
    }
    false
}

fn is_param_name(d: &Doc<'_>, n: NodeId) -> bool {
    matches!(d.parent(n).map(|p| d.node(p)), Some(Node::Param { pattern, .. }) if pattern == n)
}

fn is_class_element(n: &Node) -> bool {
    matches!(
        n,
        Node::MethodDef { .. }
            | Node::PropertyDef { .. }
            | Node::StaticBlock { .. }
            | Node::IndexSignature { .. }
    )
}

/// The member a decorator or computed key belongs to, which `this` and `super` look past.
fn skipped_member(d: &Doc<'_>, p: NodeId) -> Option<NodeId> {
    match d.node(p) {
        Node::ComputedKey { .. } => d.parent(p),
        Node::Decorator { .. } => {
            let q = d.parent(p)?;
            if matches!(d.node(q), Node::Param { .. })
                && let Some(m) = d.parent(q).and_then(|f| d.parent(f))
                && is_class_element(&d.node(m))
            {
                return Some(m);
            }
            Some(if is_class_element(&d.node(q)) { q } else { p })
        }
        _ => None,
    }
}

/// What `this` at `n` is bound by: a function, a class member, a namespace, the file, ...
fn this_container(d: &Doc<'_>, n: NodeId) -> Option<NodeId> {
    let mut cur = n;
    for _ in 0..MAX_DEPTH {
        let p = d.parent(cur)?;
        if let Some(m) = skipped_member(d, p) {
            cur = m;
            continue;
        }
        match d.node(p) {
            Node::FunctionExpr { .. } => return Some(function_owner(d, p)),
            Node::FunctionDecl { .. }
            | Node::Module { .. }
            | Node::StaticBlock { .. }
            | Node::PropertyDef { .. }
            | Node::PropertySignature { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::IndexSignature { .. }
            | Node::Enum { .. }
            | Node::Program { .. } => return Some(p),
            _ => cur = p,
        }
    }
    None
}

/// The class member `super` at `n` is in.
fn super_container(d: &Doc<'_>, n: NodeId) -> Option<NodeId> {
    let mut cur = n;
    for _ in 0..MAX_DEPTH {
        let p = d.parent(cur)?;
        if let Some(m) = skipped_member(d, p) {
            cur = m;
            continue;
        }
        match d.node(p) {
            Node::PropertyDef { .. }
            | Node::PropertySignature { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. }
            | Node::StaticBlock { .. } => return Some(p),
            Node::Program { .. } => return None,
            _ => cur = p,
        }
    }
    None
}

fn spans_where(d: &Doc<'_>, root: NodeId, mut keep: impl FnMut(NodeId) -> bool) -> Vec<Occurrence> {
    let mut out = Vec::new();
    d.collect(
        root,
        |_, _| false,
        |n| {
            if keep(n) {
                let s = d.span(n);
                out.push((d.file, s.start, s.end));
            }
        },
    );
    out
}

/// What the references of the `this`, `super`, label or primitive type keyword at
/// `offset` are, if it is one.
pub enum Special {
    This(NodeId),
    Super(NodeId),
    /// A label's definition or a `break`/`continue` target; `None` for an unknown label.
    Label(Option<NodeId>),
    Primitive(KeywordTypeKind),
}

pub fn special_at(d: &Doc<'_>, offset: u32) -> Option<Special> {
    let n = node_at(d.ast, offset)?;
    match d.node(n) {
        Node::This {} | Node::ThisType {} => Some(Special::This(n)),
        Node::Ident { .. } if is_this(d, n) => Some(Special::This(n)),
        Node::Super {} => Some(Special::Super(n)),
        Node::KeywordType { kind } if kind != KeywordTypeKind::Const => {
            Some(Special::Primitive(kind))
        }
        Node::Ident { .. } => {
            let p = d.parent(n)?;
            match d.node(p) {
                Node::Labeled { label, .. } if label == n => Some(Special::Label(Some(p))),
                Node::Break { label: Some(l) } | Node::Continue { label: Some(l) } if l == n => {
                    Some(Special::Label(label_target(d, p)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The labelled statement a `break label`/`continue label` names (across functions too).
fn label_target(d: &Doc<'_>, j: NodeId) -> Option<NodeId> {
    let name = label_of(d, j)?;
    let mut cur = j;
    for _ in 0..MAX_DEPTH {
        cur = d.parent(cur)?;
        if let Node::Labeled { label, .. } = d.node(cur)
            && d.name(label) == Some(name)
        {
            return Some(cur);
        }
    }
    None
}

/// The label of `labeled` and the `break`/`continue` labels that name it.
pub fn label_refs(d: &Doc<'_>, labeled: NodeId) -> Vec<Occurrence> {
    let Node::Labeled { label, .. } = d.node(labeled) else {
        return Vec::new();
    };
    let s = d.span(label);
    let mut out = vec![(d.file, s.start, s.end)];
    d.collect(
        labeled,
        |_, _| false,
        |n| {
            if let Node::Break { label: Some(l) } | Node::Continue { label: Some(l) } = d.node(n)
                && label_target(d, n) == Some(labeled)
            {
                let s = d.span(l);
                out.push((d.file, s.start, s.end));
            }
        },
    );
    out
}

/// The `this` keywords bound like the one at `n`. `others` are the other files to search
/// when it is the global `this` of a script.
pub fn this_refs(d: &Doc<'_>, n: NodeId, others: &[Doc<'_>]) -> Vec<Occurrence> {
    let Some(cont) = this_container(d, n) else {
        return Vec::new();
    };
    match d.node(cont) {
        Node::MethodDef { .. } | Node::PropertyDef { .. } => {
            let Some(space) = d.parent(cont) else {
                return Vec::new();
            };
            if !(is_class_like(&d.node(space)) || matches!(d.node(space), Node::ObjectLit { .. })) {
                return Vec::new();
            }
            let st = is_static(&d.node(cont));
            spans_where(d, space, |x| {
                is_this(d, x)
                    && this_container(d, x).is_some_and(|c2| {
                        d.parent(c2) == Some(space) && is_static(&d.node(c2)) == st
                    })
            })
        }
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => spans_where(d, cont, |x| {
            is_this(d, x) && this_container(d, x) == Some(cont)
        }),
        Node::Program { .. } if !d.is_module && !is_param_name(d, n) => {
            let mut out = Vec::new();
            for f in std::iter::once(d).chain(others.iter().filter(|o| !o.is_module)) {
                out.extend(spans_where(f, f.ast.root, |x| {
                    is_this(f, x)
                        && !is_param_name(f, x)
                        && this_container(f, x)
                            .is_some_and(|c2| matches!(f.node(c2), Node::Program { .. }))
                }));
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The `super` keywords of the same class and static side as the one at `n`.
pub fn super_refs(d: &Doc<'_>, n: NodeId) -> Vec<Occurrence> {
    let Some(cont) = super_container(d, n) else {
        return Vec::new();
    };
    if matches!(d.node(cont), Node::StaticBlock { .. }) {
        return Vec::new();
    }
    let Some(space) = d.parent(cont) else {
        return Vec::new();
    };
    let st = is_static(&d.node(cont));
    spans_where(d, space, |x| {
        matches!(d.node(x), Node::Super {})
            && super_container(d, x)
                .is_some_and(|c2| is_static(&d.node(c2)) == st && d.parent(c2) == Some(space))
    })
}

/// Every use of the primitive type keyword `kind` in `d`.
pub fn primitive_refs(d: &Doc<'_>, kind: KeywordTypeKind) -> Vec<Occurrence> {
    spans_where(
        d,
        d.ast.root,
        |x| matches!(d.node(x), Node::KeywordType { kind: k } if k == kind),
    )
}

/// The `this` keywords in the static methods and accessors of `class` (they are its
/// constructor, so they are references to the class), not inside nested functions or
/// classes.
pub fn static_this_refs(d: &Doc<'_>, class: NodeId) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for m in members(d, class) {
        let Node::MethodDef {
            kind: MethodKind::Method | MethodKind::Get | MethodKind::Set,
            modifiers,
            value,
            ..
        } = d.node(m)
        else {
            continue;
        };
        if !modifiers.contains(Modifiers::STATIC) {
            continue;
        }
        let Some(body) = fn_body(d, value) else {
            continue;
        };
        d.collect(
            body,
            |d, x| {
                let n = d.node(x);
                is_function_like(&n) || is_class_like(&n)
            },
            |x| {
                if matches!(d.node(x), Node::This {}) {
                    let s = d.span(x);
                    out.push((d.file, s.start, s.end));
                }
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_of<'a>(ast: &'a Ast, parents: &'a ParentMap, src: &'a str) -> Doc<'a> {
        Doc {
            file: FileId(0),
            ast,
            text: src,
            parents,
            is_module: false,
        }
    }

    /// The highlights at the first `at` (one byte in), each written as the source text
    /// from its start up to the end of the span's word, e.g. `else if` or `return 1`'s `return`.
    fn highlights(src: &str, at: &str) -> Vec<(u32, String)> {
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        let parents = ParentMap::build(&ast);
        let d = doc_of(&ast, &parents, src);
        let offset = src.find(at).expect("marker") as u32 + 1;
        let mut v: Vec<(u32, String)> = keyword_highlights(&d, offset)
            .unwrap_or_default()
            .into_iter()
            .map(|(s, e)| (s, src[s as usize..e as usize].to_string()))
            .collect();
        v.sort();
        v
    }

    /// `(offset, word)` of the `kw` at the start of each of `needles`.
    fn spans(src: &str, needles: &[(&str, &str)]) -> Vec<(u32, String)> {
        let mut v: Vec<(u32, String)> = needles
            .iter()
            .map(|(needle, kw)| (src.find(needle).expect("needle") as u32, kw.to_string()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn jsx_tag_names_highlight_both_tags() {
        let src = "const a = <div><div>x</div></div>;\nconst b = <div />;";
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Tsx).ast;
        let parents = ParentMap::build(&ast);
        let d = doc_of(&ast, &parents, src);
        let at = |needle: &str| src.find(needle).unwrap() as u32 + 2;
        let words = |v: Vec<(u32, u32)>| -> Vec<&str> {
            v.iter()
                .map(|&(s, e)| &src[s as usize..e as usize])
                .collect()
        };
        let inner = words(jsx_tag_highlights(&d, at("<div>x")).unwrap());
        assert_eq!(inner, ["<div>", "</div>"]);
        let outer = jsx_tag_highlights(&d, at("</div>;")).unwrap();
        assert_eq!(outer, vec![(10, 15), (27, 33)]);
        assert_eq!(jsx_tag_highlights(&d, at("<div />")), None);
    }

    #[test]
    fn if_else_chain_merges_else_if_on_one_line() {
        let src = "if (a) {} else if (b) {} else\nif (c) {} else {}";
        let r = highlights(src, "if (b)");
        let words: Vec<&str> = r.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(words, ["if", "else if", "else", "if", "else"]);
    }

    #[test]
    fn return_and_throw_share_a_function() {
        let src = "function f() { if (x) return 1; try { throw 1; } catch {} throw 2; \
                   function g() { return 3; } return 4; }";
        let want = spans(
            src,
            &[
                ("return 1", "return"),
                ("throw 2", "throw"),
                ("return 4", "return"),
            ],
        );
        assert_eq!(highlights(src, "return 1"), want);
        assert_eq!(highlights(src, "throw 2"), want);
        // A throw inside a `try` with a `catch` belongs to the `try` block.
        assert_eq!(
            highlights(src, "throw 1"),
            spans(src, &[("throw 1", "throw")])
        );
    }

    #[test]
    fn loops_own_their_breaks_and_continues() {
        let src = "a: for (;;) { while (x) { break a; continue; } break; } do { break; } while (y)";
        assert_eq!(
            highlights(src, "for"),
            spans(
                src,
                &[("for", "for"), ("break a", "break"), ("break; }", "break")]
            )
        );
        assert_eq!(
            highlights(src, "while (x)"),
            spans(src, &[("while (x)", "while"), ("continue", "continue")])
        );
        assert_eq!(
            highlights(src, "do {"),
            spans(
                src,
                &[
                    ("do {", "do"),
                    ("break; } while", "break"),
                    ("while (y)", "while")
                ]
            )
        );
    }

    #[test]
    fn switch_owns_its_cases_and_breaks() {
        let src = "switch (x) { case 1: break; default: for (;;) { break; } }";
        let want = spans(
            src,
            &[
                ("switch", "switch"),
                ("case", "case"),
                ("break; d", "break"),
                ("default", "default"),
            ],
        );
        assert_eq!(highlights(src, "default"), want);
        assert_eq!(highlights(src, "break; d"), want);
    }

    #[test]
    fn modifiers_and_accessors() {
        let src = "class C { private a; private get b() { return 1 } set b(v) {} \
                   constructor(private x, public y) {} }";
        assert_eq!(
            highlights(src, "private a"),
            spans(
                src,
                &[
                    ("private a", "private"),
                    ("private get", "private"),
                    ("private x", "private")
                ]
            )
        );
        assert_eq!(
            highlights(src, "set b"),
            spans(src, &[("get b", "get"), ("set b", "set")])
        );
        assert_eq!(
            highlights(src, "constructor"),
            spans(src, &[("constructor", "constructor")])
        );
    }

    #[test]
    fn abstract_class_in_a_switch_case() {
        let src = "switch (0) { case 0: abstract class A { abstract m(): void; } }";
        assert_eq!(
            highlights(src, "abstract class"),
            spans(
                src,
                &[("abstract class", "abstract"), ("abstract m", "abstract")]
            )
        );
    }

    #[test]
    fn async_await_and_try_finally() {
        let src = "async function f() { await a; () => { await b }; try {} catch {} finally {} }";
        assert_eq!(
            highlights(src, "async"),
            spans(src, &[("async", "async"), ("await a", "await")])
        );
        assert_eq!(
            highlights(src, "catch"),
            spans(src, &[("try", "try"), ("finally", "finally")])
        );
    }

    #[test]
    fn this_super_and_labels() {
        let src = "class C extends B { m() { this.a; super.m(); () => this; function g() { this } } \
                   static s() { this; } n() { this; super.n() } }\n\
                   l: for (;;) { l2: while (1) { break l; continue l2; } }";
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        let parents = ParentMap::build(&ast);
        let d = doc_of(&ast, &parents, src);
        let at = |needle: &str| src.find(needle).expect("needle") as u32;
        let starts = |v: Vec<Occurrence>| -> Vec<u32> {
            let mut s: Vec<u32> = v.into_iter().map(|o| o.1).collect();
            s.sort();
            s
        };
        let Some(Special::This(t)) = special_at(&d, at("this.a")) else {
            panic!("this");
        };
        assert_eq!(
            starts(this_refs(&d, t, &[])),
            [at("this.a"), at("this; f"), at("this; super")]
        );
        let Some(Special::Super(s)) = special_at(&d, at("super.m")) else {
            panic!("super");
        };
        assert_eq!(starts(super_refs(&d, s)), [at("super.m"), at("super.n")]);
        let Some(Special::Label(Some(l))) = special_at(&d, at("l;")) else {
            panic!("label");
        };
        assert_eq!(starts(label_refs(&d, l)), [at("l: for"), at("l;")]);
        let Some(class) = node_at(&ast, at("C ")).and_then(|n| d.parent(n)) else {
            panic!("class");
        };
        assert_eq!(starts(static_this_refs(&d, class)), [at("this; } n")]);
    }

    #[test]
    fn unterminated_and_empty_input_do_not_panic() {
        for src in [
            "",
            "if",
            "else",
            "do",
            "try {",
            "switch (x) { case",
            "class { get",
            "`",
            "l: break l",
        ] {
            let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
            let parents = ParentMap::build(&ast);
            let d = doc_of(&ast, &parents, src);
            for i in 0..=src.len() as u32 {
                let _ = keyword_highlights(&d, i);
                if let Some(Special::This(n)) = special_at(&d, i) {
                    let _ = this_refs(&d, n, &[]);
                }
            }
        }
    }

    /// The text that go to definition on the first `at` (one byte in) leads to, for the
    /// targets that need no checker.
    fn definition(src: &str, at: &str) -> Option<Vec<String>> {
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        let parents = ParentMap::build(&ast);
        let d = doc_of(&ast, &parents, src);
        let offset = src.find(at).expect("marker") as u32 + 1;
        let text = |s: u32, e: u32| src[s as usize..e as usize].to_string();
        match keyword_definition(&d, offset)? {
            KeywordTarget::Spans(v) => Some(v.into_iter().map(|(s, e)| text(s, e)).collect()),
            KeywordTarget::Name(n) | KeywordTarget::Override { name: n, .. } => {
                let s = ast.span(n);
                Some(vec![format!("name {}", text(s.start, s.end))])
            }
            KeywordTarget::Super(_) => Some(vec!["super".into()]),
        }
    }

    /// A declared name spelled like a keyword is a name: go to definition on it does not
    /// redirect to itself (which looped forever).
    #[test]
    fn a_name_spelled_like_a_keyword_is_not_redirected() {
        assert_eq!(definition("var let = 1;", "let"), None);
        assert_eq!(definition("var let;", "let"), None);
        assert_eq!(
            definition("var let = 1;", "var"),
            Some(vec!["name let".into()])
        );
        assert_eq!(definition("let x = 1;", "let"), Some(vec!["name x".into()]));
    }

    #[test]
    fn return_await_and_yield_go_to_their_function() {
        let src = "function foo() { return 1; }\nconst g = async () => { await 1; };\n\
                   class C { *m() { yield 1; } }";
        assert_eq!(definition(src, "return"), Some(vec!["foo".into()]));
        assert_eq!(definition(src, "await"), Some(vec!["g".into()]));
        assert_eq!(definition(src, "yield"), Some(vec!["m".into()]));
        // An anonymous function that initializes nothing is shown whole.
        let src = "f(function () { return 1; });";
        assert_eq!(
            definition(src, "return"),
            Some(vec!["function () { return 1; }".into()])
        );
    }

    #[test]
    fn case_and_default_go_to_their_switch() {
        let src = "switch (x) { case 1: break; default: break; }";
        assert_eq!(definition(src, "case"), Some(vec!["switch".into()]));
        assert_eq!(definition(src, "default"), Some(vec!["switch".into()]));
        let src = "export default 1 + 2;";
        assert_eq!(
            definition(src, "default"),
            Some(vec!["export default 1 + 2;".into()])
        );
    }

    #[test]
    fn modifiers_and_declaration_keywords_go_to_the_name() {
        let src = "export class A { private z: string; override b() {} }\n\
                   export const v = 1;\nconst w = 2, u = 3;\ninterface I {}";
        assert_eq!(definition(src, "export class"), Some(vec!["name A".into()]));
        assert_eq!(definition(src, "class"), Some(vec!["name A".into()]));
        assert_eq!(definition(src, "private"), Some(vec!["name z".into()]));
        assert_eq!(definition(src, "override"), Some(vec!["name b".into()]));
        assert_eq!(definition(src, "const w"), Some(vec!["name w".into()]));
        assert_eq!(definition(src, "interface"), Some(vec!["name I".into()]));
        // A variable statement's modifier has nowhere to go.
        assert_eq!(definition(src, "export const"), Some(vec![]));
    }

    #[test]
    fn this_goes_to_its_parameter_or_class() {
        let src = "function f(this: Window) { this; }";
        assert_eq!(definition(src, "this;"), Some(vec!["this".into()]));
        let src = "class K { m() { this; } }";
        assert_eq!(definition(src, "this"), Some(vec!["K".into()]));
        let src = "function g() { this; }";
        assert_eq!(definition(src, "this"), Some(vec![]));
    }
}
