//! What kind of position the cursor is at, for completions: the token before it, whether a
//! type or a value is expected there.
//!
//! The rules follow tsc 7's completion data (`tsc: tsc/internal/ls/completions.go`, the
//! type-only-location test and the context-token checks), written against our AST: tsc looks
//! at the token before the word being typed; we read that token from the source text and find
//! the node that holds it.

use ts_r_ast::{Ast, HeritageKind, Node, NodeId};
use ts_r_checker::Checker;
use ts_r_diagnostics::FileId;

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The token that ends at or before `pos`, skipping whitespace and comments: its start and
/// text. A word is returned whole; any other token as its last character (callers that care
/// about `==` or `=>` look at the neighbours).
pub fn token_before<'t>(ast: &Ast, text: &'t str, pos: u32) -> Option<(u32, &'t str)> {
    let b = text.as_bytes();
    let mut p = (pos as usize).min(b.len());
    // Each round skips one comment, so this ends after at most one round per comment.
    for _ in 0..=ast.comments.len() {
        while p > 0 && b[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        let i = ast.comments.partition_point(|c| (c.span.end as usize) < p);
        match ast.comments.get(i) {
            Some(c) if c.span.end as usize == p && (c.span.start as usize) < p => {
                p = c.span.start as usize;
            }
            _ => break,
        }
    }
    if p == 0 {
        return None;
    }
    if is_ident_byte(b[p - 1]) {
        let mut s = p;
        while s > 0 && is_ident_byte(b[s - 1]) {
            s -= 1;
        }
        return Some((s as u32, &text[s..p]));
    }
    let s = (0..p).rev().find(|&i| text.is_char_boundary(i))?;
    Some((s as u32, &text[s..p]))
}

/// The deepest node whose span strictly holds the byte at `pos` (a node that only touches it
/// does not count).
pub fn node_covering(ast: &Ast, pos: u32) -> Option<NodeId> {
    let sp = ast.span(ast.root);
    if pos < sp.start || pos >= sp.end {
        return None;
    }
    let mut cur = ast.root;
    // Child ids are smaller than their parent's, so this descends at most node_count times.
    for _ in 0..ast.node_count() {
        let mut inside = None;
        ast.for_each_child(cur, |c| {
            let s = ast.span(c);
            if inside.is_none() && s.start <= pos && pos < s.end {
                inside = Some(c);
            }
        });
        match inside {
            Some(c) if c != cur => cur = c,
            _ => break,
        }
    }
    Some(cur)
}

/// A node that is (part of) a type, as opposed to a declaration or expression.
fn is_type_node(n: &Node) -> bool {
    matches!(
        n,
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::ThisType {}
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::ParenType { .. }
            | Node::TypeArgs { .. }
    )
}

/// Whether only types (and namespaces holding them) are expected where the word starting at
/// `word` is typed: in a type annotation, after `as`/`satisfies`, in a type alias or type
/// parameter, in a type argument list or `implements` clause. `typeof |` expects a value.
pub fn is_type_location(c: &Checker<'_>, file: FileId, anchor: NodeId, word: u32) -> bool {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let b = text.as_bytes();
    if let Some((at, tok)) = token_before(ast, text, word) {
        let holder = || node_covering(ast, at).map(|n| ast.node(n));
        match tok {
            "typeof" => return false,
            ":" => {
                if holder().is_some_and(|n| {
                    matches!(
                        n,
                        Node::VarDeclarator { .. }
                            | Node::Param { .. }
                            | Node::PropertyDef { .. }
                            | Node::PropertySignature { .. }
                            | Node::FunctionDecl { .. }
                            | Node::FunctionExpr { .. }
                            | Node::Arrow { .. }
                            | Node::MethodDef { .. }
                            | Node::MethodSignature { .. }
                            | Node::CallSignature { .. }
                            | Node::ConstructSignature { .. }
                            | Node::FunctionType { .. }
                            | Node::ConstructorType { .. }
                    )
                }) {
                    return true;
                }
            }
            "=" => {
                let i = at as usize;
                let lone = (i == 0 || !b"=!<>+-*/%&|^?".contains(&b[i - 1]))
                    && !matches!(b.get(i + 1), Some(b'=' | b'>'));
                if lone
                    && holder().is_some_and(|n| {
                        matches!(n, Node::TypeAlias { .. } | Node::TypeParam { .. })
                    })
                {
                    return true;
                }
            }
            "as" if holder().is_some_and(|n| matches!(n, Node::As { .. })) => return true,
            "satisfies" if holder().is_some_and(|n| matches!(n, Node::Satisfies { .. })) => {
                return true;
            }
            "<" if holder().is_some_and(|n| {
                matches!(n, Node::TypeRef { .. } | Node::TypeAssertion { .. })
            }) =>
            {
                return true;
            }
            "extends" if holder().is_some_and(|n| matches!(n, Node::TypeParam { .. })) => {
                return true;
            }
            _ => {}
        }
    }
    let parents = c.file(file).parents();
    let node = ast.node(anchor);
    if !matches!(node, Node::Ident { .. }) {
        return is_type_node(node);
    }
    let Some(p) = parents.parent(anchor) else {
        return false;
    };
    match *ast.node(p) {
        Node::TypeQuery { .. } => false,
        Node::ExprWithTypeArgs { expr, .. } if expr == anchor => {
            parents.parent(p).is_some_and(|h| match *ast.node(h) {
                Node::HeritageClause { kind, .. } => kind == HeritageKind::Implements,
                Node::Interface { .. } => true,
                _ => false,
            })
        }
        ref n => is_type_node(n),
    }
}

/// Words that may stand in a type argument list being scanned backwards: identifiers, and
/// of the keywords only these.
const TYPE_ARG_KEYWORDS: &[&str] = &[
    "typeof",
    "extends",
    "keyof",
    "true",
    "false",
    "any",
    "unknown",
    "number",
    "bigint",
    "boolean",
    "string",
    "symbol",
    "void",
    "undefined",
    "null",
    "never",
    "object",
    "this",
];

/// Reserved words that are not identifiers (a `<` after one of them starts no type
/// arguments).
const RESERVED: &[&str] = &[
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
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "let",
    "await",
];

/// For a word at `pos` that may be a type argument of an incomplete `f<...` (`f<|`,
/// `new C<A, |`, `f<{ a: 1 }, |`): the start of the identifier before the unmatched `<` and
/// how many type arguments come before the word. Scans tokens backwards, balancing `<>`,
/// braces, parens and brackets, and gives up at a token no type contains (tsc: the possible
/// type arguments scan in `tsc/internal/ls/utilities.go`).
pub fn possible_type_arguments(ast: &Ast, text: &str, pos: u32) -> Option<(u32, u32)> {
    if !text.as_bytes()[..(pos as usize).min(text.len())].contains(&b'<') {
        return None;
    }
    let b = text.as_bytes();
    // The token before `p`, a string literal taken whole.
    let prev = |p: u32| -> Option<(u32, &str)> {
        let (at, tok) = token_before(ast, text, p)?;
        if matches!(tok, "\"" | "'" | "`") {
            let n = node_covering(ast, at)?;
            if matches!(
                ast.node(n),
                Node::StringLit { .. } | Node::TemplateLit { .. } | Node::TemplateElement { .. }
            ) {
                let start = ast.span(n).start;
                return Some((start, &text[start as usize..at as usize + 1]));
            }
        }
        Some((at, tok))
    };
    let mut remaining = 0u32;
    let mut args = 0u32;
    let mut close: Vec<u8> = Vec::new();
    let mut p = pos;
    for _ in 0..100_000 {
        let (at, tok) = prev(p)?;
        if !close.is_empty() {
            match tok {
                ")" | "]" | "}" => close.push(tok.as_bytes()[0]),
                "(" | "[" | "{" => {
                    let want = match tok {
                        "(" => b')',
                        "[" => b']',
                        _ => b'}',
                    };
                    if close.pop() != Some(want) {
                        return None;
                    }
                }
                _ => {}
            }
            p = at;
            continue;
        }
        match tok {
            "<" => {
                let mut q = prev(at)?;
                if q.1 == "." && at > 0 && prev(q.0).is_some_and(|r| r.1 == "?") {
                    q = prev(prev(q.0)?.0)?;
                }
                let (cs, word) = q;
                let ident = word
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_' || c == b'$')
                    && !RESERVED.contains(&word);
                if !ident {
                    return None;
                }
                if remaining == 0 {
                    return Some((cs, args));
                }
                remaining -= 1;
            }
            ">" => {
                // `=>` is one token.
                if at > 0 && b[at as usize - 1] == b'=' {
                    p = at - 1;
                    continue;
                }
                remaining += 1;
            }
            ")" | "]" | "}" => close.push(tok.as_bytes()[0]),
            "," => args += 1,
            "." | "|" | "?" | ":" => {}
            _ if tok.starts_with(['"', '\'', '`']) => {}
            _ => {
                let first = tok.as_bytes()[0];
                if first.is_ascii_digit() {
                } else if is_ident_byte(first) {
                    if RESERVED.contains(&tok) && !TYPE_ARG_KEYWORDS.contains(&tok) {
                        return None;
                    }
                } else {
                    return None;
                }
            }
        }
        p = at;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_before_skips_space_and_comments() {
        let src = "let a = b /* c */  // d\n  ";
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        assert_eq!(token_before(&ast, src, src.len() as u32), Some((8, "b")));
        assert_eq!(token_before(&ast, src, 8), Some((6, "=")));
        assert_eq!(token_before(&ast, src, 0), None);
        // A multi-byte character before the position is returned whole.
        let src = "é ";
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        assert_eq!(token_before(&ast, src, 3), Some((0, "é")));
    }

    #[test]
    fn possible_type_arguments_scan_back_to_the_callee() {
        let at = |src: &str, needle: &str| {
            let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
            let pos = src.find(needle).expect("needle") as u32;
            possible_type_arguments(&ast, src, pos)
        };
        assert_eq!(at("f<Ty", "Ty"), Some((0, 0)));
        assert_eq!(at("x; f2<A, () => B, Ty", "Ty"), Some((3, 2)));
        assert_eq!(at("new C<C<Ty;", "Ty"), Some((6, 0)));
        assert_eq!(at("g<{ a: \"<\" }, Ty", "Ty"), Some((0, 1)));
        assert_eq!(at("a < b; Ty", "Ty"), None);
        assert_eq!(at("return <Ty", "Ty"), None);
        assert_eq!(at("no angle", "angle"), None);
    }
}
