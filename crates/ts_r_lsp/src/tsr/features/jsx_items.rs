//! JSX completions: attribute names inside an opening tag, the tag to close after `</`, and
//! no list in JSX text.
//!
//! Rules learned from tsc 7 (`tsc: tsc/internal/ls/completions.go`: the containing-JSX-element
//! test, the JSX attribute filter and the closing-tag completion), written here on our AST.

use super::position::token_before;
use std::collections::HashSet;
use ts_r_ast::{Node, NodeId};
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

/// Where in JSX the word being typed is.
pub enum JsxSpot {
    /// An attribute name of this opening (or self-closing) element.
    Attrs { opening: NodeId },
    /// In a closing tag: the open element's tag name (with `>` when the tag lacks one).
    Closing { label: String },
    /// JSX text or between child elements: no list.
    Text,
    /// The tag name of an opening element: intrinsic element names along with values.
    Tag,
}

/// The JSX spot of the word starting at `word` (`node` is the node holding it).
pub fn jsx_spot(c: &Checker<'_>, file: FileId, node: NodeId, word: u32) -> Option<JsxSpot> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    if let Some(opening) = unclosed_opening_before(c, file, word) {
        return Some(JsxSpot::Attrs { opening });
    }
    let mut cur = Some(node);
    // JSX nests a bounded number of levels above the word before an expression boundary.
    for _ in 0..256 {
        let n = cur?;
        let s = ast.span(n);
        match *ast.node(n) {
            // Inside `{ }` the code is an expression.
            // An unclosed `{...|` holds the word too.
            Node::JsxExprContainer { .. }
            | Node::JsxSpreadChild { .. }
            | Node::JsxSpreadAttr { .. }
                if s.start < word
                    && (word < s.end || !text[s.start as usize..s.end as usize].ends_with('}')) =>
            {
                return None;
            }
            Node::JsxAttr { value, .. } => {
                // The value (or `foo=|` before one) is an expression or a string; right after
                // the value the next attribute name goes.
                let in_value = match value {
                    Some(v) => ast.span(v).start <= word && word < ast.span(v).end,
                    None => false,
                };
                let after_value = value.is_some_and(|v| {
                    let vs = ast.span(v);
                    vs.start < vs.end && vs.end <= word
                });
                if !after_value
                    && (in_value
                        || text[s.start as usize..(word as usize).min(text.len())].contains('='))
                {
                    return None;
                }
            }
            // Before the `<` of a child tag: JSX text of the element around it.
            Node::JsxOpening { .. } | Node::JsxClosing { .. } if word <= s.start => {}
            Node::JsxOpening {
                name, type_args, ..
            } => {
                let ns = ast.span(name);
                if ns.start <= word && word <= ns.end && ns.start < ns.end {
                    return Some(JsxSpot::Tag);
                }
                // `<|` with no name yet.
                if token_before(ast, text, word).is_some_and(|(at, t)| t == "<" && at == s.start) {
                    return Some(JsxSpot::Tag);
                }
                // Type arguments only count when the word comes after them: an unclosed tag
                // followed by `</X>` parses the `<` as the start of type arguments.
                let head_end = match type_args {
                    Some(t) if ast.span(t).start <= word => ast.span(t).end,
                    _ => ns.end,
                };
                if word < head_end || word < s.start + 1 {
                    return None;
                }
                // `x=|` without braces: tsc offers values.
                if token_before(ast, text, word).is_some_and(|(_, t)| t == "=") {
                    return None;
                }
                if word < s.end || !text[s.start as usize..s.end as usize].ends_with('>') {
                    return Some(JsxSpot::Attrs { opening: n });
                }
            }
            // `< />` is an opening tag missing its name, not a closing tag.
            Node::JsxClosing { .. } if text.as_bytes().get(s.start as usize + 1) != Some(&b'/') => {
                return Some(JsxSpot::Tag);
            }
            Node::JsxClosing { name } => {
                let el = parents.parent(n)?;
                let Node::JsxElement { opening, .. } = *ast.node(el) else {
                    return None;
                };
                let Node::JsxOpening { name: open, .. } = *ast.node(opening) else {
                    return None;
                };
                let os = ast.span(open);
                let tag: String = text[os.start as usize..os.end as usize]
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect();
                if tag.is_empty() {
                    return None;
                }
                let after = ast.span(name).end.max(s.start + 2).min(s.end) as usize;
                let closed = text[after..s.end as usize].contains('>');
                let label = if closed { tag } else { format!("{tag}>") };
                return Some(JsxSpot::Closing { label });
            }
            Node::JsxText {} => return Some(JsxSpot::Text),
            Node::JsxElement {
                opening, closing, ..
            } => {
                let from = ast.span(opening).end;
                let to = closing.map_or(s.end, |c| ast.span(c).start);
                if from <= word && word <= to {
                    return Some(JsxSpot::Text);
                }
            }
            Node::JsxFragment { .. } => {
                if s.start + 2 <= word && word + 3 <= s.end {
                    return Some(JsxSpot::Text);
                }
            }
            _ => {}
        }
        cur = parents.parent(n);
    }
    None
}

/// The opening tag the token before `word` ends, when that tag has no `>` yet (`<Foo a |`
/// with the next line already outside it): the word is its next attribute.
fn unclosed_opening_before(c: &Checker<'_>, file: FileId, word: u32) -> Option<NodeId> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (at, tok) = token_before(ast, text, word)?;
    let tok_end = at + tok.len() as u32;
    let parents = c.file(file).parents();
    let mut cur = super::position::node_covering(ast, at);
    // The token sits a few levels below its tag (name, attribute, member name).
    for _ in 0..8 {
        let n = cur?;
        if let Node::JsxOpening { .. } = ast.node(n) {
            let s = ast.span(n);
            let closed = text[s.start as usize..s.end as usize].ends_with('>');
            return (!closed && s.end == tok_end && s.end < word).then_some(n);
        }
        cur = parents.parent(n);
    }
    None
}

/// A name a JSX attribute may have (an identifier that may also hold `-`).
pub fn is_jsx_attr_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_alphabetic() || ch == '_' || ch == '$')
        && chars.all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$' | '-'))
}

/// The attributes the opening element may still take: the props of its props type, minus the
/// attributes already written (not the one being typed). The second value is true for
/// members a spread attribute already provides.
pub fn attr_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    opening: NodeId,
    offset: u32,
) -> Vec<(String, SymId, bool)> {
    let c = ck.c;
    let ast = c.ast(file);
    let Node::JsxOpening { name, .. } = *ast.node(opening) else {
        return Vec::new();
    };
    let mut props = match ck.quietly(|ck| ck.jsx_props_type(Loc::new(file, opening))) {
        // A generic component's props are its type parameter: offer its constraint's props.
        Some(t) => {
            let t = ck.apparent_type(t);
            super::complete::object_props(ck, t)
        }
        None => Vec::new(),
    };
    if props.is_empty() {
        props = declared_props(ck, file, name);
    }
    let Node::JsxOpening { attrs, .. } = *ast.node(opening) else {
        return Vec::new();
    };
    let mut written: HashSet<String> = HashSet::new();
    let mut spread: HashSet<String> = HashSet::new();
    for &a in ast.list(attrs) {
        let sp = ast.span(a);
        if sp.start <= offset && offset <= sp.end {
            continue;
        }
        match *ast.node(a) {
            Node::JsxAttr { name, .. } => {
                let ns = ast.span(name);
                written.insert(text_of(c, file, ns.start, ns.end));
            }
            Node::JsxSpreadAttr { arg } => {
                let t = ck.quietly(|ck| ck.check_expr(Loc::new(file, arg)));
                let t = ck.apparent_type(t);
                for (n, _) in ck.properties_of(t) {
                    spread.insert(ck.store().str(n).to_string());
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<(String, SymId, bool)> = props
        .into_iter()
        .filter(|(n, _)| is_jsx_attr_name(n) && !written.contains(n))
        .map(|(n, s)| {
            let by_spread = spread.contains(&n);
            (n, s, by_spread)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The props the tag's own signatures declare, for a tag the checker could not resolve (an
/// unclosed tag the parser gave stray type arguments, `<Foo |` before `</X>`) or a generic
/// class component: the first parameter of a call signature, or the `props` member of what a
/// construct signature returns, each read through its constraint.
fn declared_props(ck: &mut Ck<'_, '_>, file: FileId, name: NodeId) -> Vec<(String, SymId)> {
    use ts_r_checker::members::SigKind;
    let t = ck.quietly(|ck| ck.check_expr(Loc::new(file, name)));
    let t = ck.apparent_type(t);
    let p = if let Some(&sig) = ck.signatures_of(t, SigKind::Call).first() {
        ck.param_type_at(sig, 0)
    } else if let Some(&sig) = ck.signatures_of(t, SigKind::Construct).first() {
        let inst = ck.return_type_of(sig);
        let inst = ck.apparent_type(inst);
        ck.property_of_type(inst, "props")
            .map(|s| ck.type_of_symbol(s))
    } else {
        None
    };
    match p {
        Some(p) => {
            let p = ck.apparent_type(p);
            super::complete::object_props(ck, p)
        }
        None => Vec::new(),
    }
}

fn text_of(c: &Checker<'_>, file: FileId, start: u32, end: u32) -> String {
    c.file(file).text[start as usize..end as usize].to_string()
}

/// The names of the intrinsic elements (`JSX.IntrinsicElements`'s properties) at `at`.
pub fn intrinsic_tags(ck: &mut Ck<'_, '_>, at: Loc) -> Vec<(String, SymId)> {
    let c = ck.c;
    let Some(ns) = c.jsx_namespace(at) else {
        return Vec::new();
    };
    let Some(ie) = c.export_of(ns, "IntrinsicElements") else {
        return Vec::new();
    };
    let t = ck.declared_type_of_symbol(ie);
    let mut out: Vec<(String, SymId)> = ck
        .properties_of(t)
        .into_iter()
        .map(|(n, s)| (ck.store().str(n).to_string(), s))
        .filter(|(n, _)| is_jsx_attr_name(n))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
