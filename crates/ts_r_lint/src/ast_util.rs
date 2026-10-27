//! AST helpers shared by rules.

use std::borrow::Cow;

use ts_r_ast::{Ast, FieldDump, MethodKind, Node, NodeId, NodeKind, ParentMap};
use ts_r_span::Span;

/// Strips `Paren` wrappers.
pub fn skip_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren { expr } = *ast.node(id) {
        id = expr;
    }
    id
}

/// Strips parentheses and TS-only wrappers (`as`, `satisfies`, `!`, `<T>x`).
pub fn skip_ts_wrappers(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// The parent, skipping `Paren` nodes.
pub fn parent_skip_parens(ast: &Ast, parents: &ParentMap, id: NodeId) -> Option<NodeId> {
    let mut p = parents.parent(id)?;
    while matches!(ast.node(p), Node::Paren { .. }) {
        p = parents.parent(p)?;
    }
    Some(p)
}

pub fn is_function(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::FunctionDecl | NodeKind::FunctionExpr | NodeKind::Arrow
    )
}

/// Function-like nodes that create a `this`/`arguments` boundary (not arrows).
pub fn is_non_arrow_function(k: NodeKind) -> bool {
    matches!(k, NodeKind::FunctionDecl | NodeKind::FunctionExpr)
}

/// The identifier name, if `id` is an `Ident`.
pub fn ident_name(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}

/// True for `undefined` (the identifier).
pub fn is_undefined(ast: &Ast, id: NodeId) -> bool {
    ident_name(ast, id) == Some("undefined")
}

/// The static name of a property key: identifiers, string/number literals, and computed
/// keys with a literal or no-substitution template.
pub fn key_name<'a>(ast: &'a Ast, src: &'a str, key: NodeId) -> Option<Cow<'a, str>> {
    match *ast.node(key) {
        Node::Ident { name } | Node::PrivateName { name } => Some(Cow::Borrowed(ast.atom(name))),
        Node::StringLit { value } => Some(Cow::Borrowed(ast.atom(value))),
        Node::NumberLit { value } => Some(Cow::Owned(number_to_string(value.get()))),
        Node::BigIntLit { value } => Some(Cow::Owned(
            parse_bigint(ast.atom(value)).unwrap_or_else(|| ast.atom(value).to_owned()),
        )),
        Node::ComputedKey { expr } => static_value_string(ast, src, expr),
        _ => None,
    }
}

/// Like [`key_name`] but only for non-computed keys (what ESLint calls a static name of a
/// `MemberExpression` property is handled by [`member_name`]).
pub fn static_key_name<'a>(ast: &'a Ast, src: &'a str, key: NodeId) -> Option<Cow<'a, str>> {
    key_name(ast, src, key)
}

/// The static string value of an expression: string literal, no-substitution template,
/// number, `null`, booleans, regex, bigint (as ESLint's `getStaticStringValue`).
pub fn static_value_string<'a>(ast: &'a Ast, src: &'a str, id: NodeId) -> Option<Cow<'a, str>> {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::StringLit { value } => Some(Cow::Borrowed(ast.atom(value))),
        Node::NumberLit { value } => Some(Cow::Owned(number_to_string(value.get()))),
        Node::BigIntLit { value } => Some(Cow::Owned(
            parse_bigint(ast.atom(value)).unwrap_or_else(|| ast.atom(value).to_owned()),
        )),
        Node::NullLit {} => Some(Cow::Borrowed("null")),
        Node::BoolLit { value } => Some(Cow::Borrowed(if value { "true" } else { "false" })),
        Node::RegexLit { .. } => Some(Cow::Borrowed(ast.span(id).text(src))),
        Node::TemplateLit { parts } => {
            let p = ast.list(parts);
            if p.len() == 1
                && let Node::TemplateElement { cooked, .. } = *ast.node(p[0])
            {
                return Some(Cow::Borrowed(ast.atom(cooked)));
            }
            None
        }
        _ => None,
    }
}

/// The property name of `a.b` / `a["b"]` / `a[0]`.
pub fn member_name<'a>(ast: &'a Ast, src: &'a str, id: NodeId) -> Option<Cow<'a, str>> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(Cow::Borrowed(ast.atom(name))),
            Node::PrivateName { name } => Some(Cow::Owned(format!("#{}", ast.atom(name)))),
            _ => None,
        },
        Node::Index { index, .. } => static_value_string(ast, src, index),
        _ => None,
    }
}

/// `obj.prop` / `obj?.prop` / `obj["prop"]`: returns `(obj, name)`.
pub fn member_parts<'a>(ast: &'a Ast, src: &'a str, id: NodeId) -> Option<(NodeId, Cow<'a, str>)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            Some((obj, member_name(ast, src, id)?))
        }
        _ => None,
    }
}

/// Formats a number the way JavaScript's `String(n)` does (for property names).
pub fn number_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if v == 0.0 {
        return "0".to_owned();
    }
    if v.fract() == 0.0 && v.abs() < 1e21 {
        return format!("{v:.0}");
    }
    let abs = v.abs();
    if (1e-6..1e21).contains(&abs) {
        // Shortest round-trip representation.
        let s = format!("{v}");
        return s;
    }
    // Exponential form: d.ddde+x
    let s = format!("{v:e}");
    // Rust prints `1e21`, `1.5e-7`; JS prints `1e+21`, `1.5e-7`.
    match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => s,
    }
}

/// Decimal string of a bigint literal (`0x10n` -> `16`), if it fits in u128.
pub fn parse_bigint(text: &str) -> Option<String> {
    let t = text.replace('_', "");
    let (radix, digits) = if let Some(r) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, r)
    } else if let Some(r) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (8, r)
    } else if let Some(r) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (2, r)
    } else {
        (10, t.as_str())
    };
    u128::from_str_radix(digits, radix)
        .ok()
        .map(|v| v.to_string())
}

/// Structural equality of two subtrees (same kinds, same scalar fields, same children).
/// Used where ESLint compares token sequences.
pub fn same_tree(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    same_tree_depth(ast, a, b, 0)
}

fn same_tree_depth(ast: &Ast, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return false;
    }
    let (na, nb) = (ast.node(a), ast.node(b));
    if na.kind() != nb.kind() {
        return false;
    }
    match (na, nb) {
        (Node::Ident { name: x }, Node::Ident { name: y })
        | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => return x == y,
        (Node::StringLit { .. }, Node::StringLit { .. })
        | (Node::NumberLit { .. }, Node::NumberLit { .. })
        | (Node::BigIntLit { .. }, Node::BigIntLit { .. })
        | (Node::RegexLit { .. }, Node::RegexLit { .. })
        | (Node::TemplateElement { .. }, Node::TemplateElement { .. })
        | (Node::JsxText {}, Node::JsxText {}) => return raw_eq(ast, a, b),
        _ => {}
    }
    let fa = na.dump_fields(ast);
    let fb = nb.dump_fields(ast);
    if fa.len() != fb.len() {
        return false;
    }
    for ((_, x), (_, y)) in fa.iter().zip(fb.iter()) {
        match (x, y) {
            (FieldDump::Skip, FieldDump::Skip) => {}
            (FieldDump::Scalar(s), FieldDump::Scalar(t)) => {
                if s != t {
                    return false;
                }
            }
            (FieldDump::Child(c), FieldDump::Child(d)) => {
                if !same_tree_depth(ast, *c, *d, depth + 1) {
                    return false;
                }
            }
            (FieldDump::List(l), FieldDump::List(m)) => {
                let (l, m) = (ast.list(*l), ast.list(*m));
                if l.len() != m.len() {
                    return false;
                }
                for (c, d) in l.iter().zip(m) {
                    if !same_tree_depth(ast, *c, *d, depth + 1) {
                        return false;
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

fn raw_eq(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    // Literal nodes: compare cooked values via dump (string/number/regex fields).
    let fa = ast.node(a).dump_fields(ast);
    let fb = ast.node(b).dump_fields(ast);
    fa.iter()
        .zip(fb.iter())
        .all(|((_, x), (_, y))| match (x, y) {
            (FieldDump::Scalar(s), FieldDump::Scalar(t)) => s == t,
            (FieldDump::Skip, FieldDump::Skip) => true,
            _ => false,
        })
}

/// Same as [`same_tree`] but also requires identical source text (exact token match).
pub fn same_tokens(ast: &Ast, src: &str, a: NodeId, b: NodeId) -> bool {
    same_tree(ast, a, b) && literal_texts_equal(ast, src, a, b)
}

fn literal_texts_equal(ast: &Ast, src: &str, a: NodeId, b: NodeId) -> bool {
    // Compare raw text of literals in both trees, in order.
    let mut la = Vec::new();
    let mut lb = Vec::new();
    collect_literals(ast, a, &mut la, 0);
    collect_literals(ast, b, &mut lb, 0);
    la.len() == lb.len() && la.iter().zip(&lb).all(|(x, y)| x.text(src) == y.text(src))
}

fn collect_literals(ast: &Ast, id: NodeId, out: &mut Vec<Span>, depth: u32) {
    if depth > 500 {
        return;
    }
    match ast.kind(id) {
        NodeKind::StringLit
        | NodeKind::NumberLit
        | NodeKind::BigIntLit
        | NodeKind::RegexLit
        | NodeKind::TemplateElement => out.push(ast.span(id)),
        _ => ast.for_each_child(id, |c| collect_literals(ast, c, out, depth + 1)),
    }
}

/// The first non-whitespace, non-comment byte position at or after `pos`.
pub fn skip_trivia_forward(src: &str, mut pos: usize) -> usize {
    let b = src.as_bytes();
    while pos < b.len() {
        let c = b[pos];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == 0x0b || c == 0x0c {
            pos += 1;
        } else if c == b'/' && b.get(pos + 1) == Some(&b'/') {
            while pos < b.len() && b[pos] != b'\n' && b[pos] != b'\r' {
                pos += 1;
            }
        } else if c == b'/' && b.get(pos + 1) == Some(&b'*') {
            pos += 2;
            while pos + 1 < b.len() && !(b[pos] == b'*' && b[pos + 1] == b'/') {
                pos += 1;
            }
            pos = (pos + 2).min(b.len());
        } else if c >= 0x80 {
            let ch = src[pos..].chars().next().unwrap_or('a');
            if ch.is_whitespace() || ch == '\u{feff}' {
                pos += ch.len_utf8();
            } else {
                return pos;
            }
        } else {
            return pos;
        }
    }
    pos
}

/// The position just after the last non-trivia character before `pos` (skips whitespace
/// and comments backwards using the AST's comment table).
pub fn skip_trivia_backward(ast: &Ast, src: &str, mut pos: usize) -> usize {
    let b = src.as_bytes();
    loop {
        while pos > 0 {
            let c = b[pos - 1];
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == 0x0b || c == 0x0c {
                pos -= 1;
            } else if c >= 0x80 {
                let ch = src[..pos].chars().next_back().unwrap_or('a');
                if ch.is_whitespace() || ch == '\u{feff}' {
                    pos -= ch.len_utf8();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        // A comment ending here?
        let idx = ast
            .comments
            .partition_point(|c| (c.span.end as usize) < pos);
        match ast.comments.get(idx) {
            Some(c) if c.span.end as usize == pos && pos > 0 => pos = c.span.start as usize,
            _ => return pos,
        }
    }
}

/// True if a line break occurs in `src[a..b]`.
pub fn has_newline(src: &str, a: u32, b: u32) -> bool {
    src.get(a as usize..b as usize)
        .is_some_and(|s| s.contains(['\n', '\r', '\u{2028}', '\u{2029}']))
}

/// The kind of function for messages: "function 'foo'", "method 'bar'", "arrow function",
/// "getter 'x'", ... (ESLint's `getFunctionNameWithKind`).
pub fn function_name_with_kind(ast: &Ast, src: &str, parents: &ParentMap, f: NodeId) -> String {
    let parent = parents.parent(f);
    let (is_async, is_gen, own_name) = match *ast.node(f) {
        Node::FunctionDecl { flags, name, .. } | Node::FunctionExpr { flags, name, .. } => (
            flags.contains(ts_r_ast::FnFlags::ASYNC),
            flags.contains(ts_r_ast::FnFlags::GENERATOR),
            name.and_then(|n| ident_name(ast, n)),
        ),
        Node::Arrow { flags, .. } => (flags.contains(ts_r_ast::FnFlags::ASYNC), false, None),
        _ => (false, false, None),
    };
    let mut tokens: Vec<String> = Vec::new();
    // Method / accessor / property context.
    if let Some(p) = parent {
        match *ast.node(p) {
            Node::MethodDef {
                kind,
                key,
                modifiers,
                ..
            } => {
                if modifiers.contains(ts_r_ast::Modifiers::STATIC) {
                    tokens.push("static".into());
                }
                let is_private = matches!(ast.node(key), Node::PrivateName { .. });
                if is_private {
                    tokens.push("private".into());
                }
                if is_async {
                    tokens.push("async".into());
                }
                if is_gen {
                    tokens.push("generator".into());
                }
                tokens.push(
                    match kind {
                        MethodKind::Get => "getter",
                        MethodKind::Set => "setter",
                        MethodKind::Constructor => "constructor",
                        MethodKind::Method => "method",
                    }
                    .into(),
                );
                if kind != MethodKind::Constructor {
                    // A private name is shown bare (`#x`), others quoted.
                    if is_private && let Some(n) = key_name(ast, src, key) {
                        tokens.push(format!("#{n}"));
                    } else if let Some(n) = key_name(ast, src, key) {
                        tokens.push(format!("'{n}'"));
                    }
                }
                return tokens.join(" ");
            }
            Node::Property { key, value, .. } if value == f => {
                if is_async {
                    tokens.push("async".into());
                }
                if is_gen {
                    tokens.push("generator".into());
                }
                tokens.push("method".into());
                if let Some(n) = key_name(ast, src, key) {
                    tokens.push(format!("'{n}'"));
                }
                return tokens.join(" ");
            }
            Node::PropertyDef { key, modifiers, .. } => {
                if modifiers.contains(ts_r_ast::Modifiers::STATIC) {
                    tokens.push("static".into());
                }
                let is_private = matches!(ast.node(key), Node::PrivateName { .. });
                if is_private {
                    tokens.push("private".into());
                }
                if is_async {
                    tokens.push("async".into());
                }
                if is_gen {
                    tokens.push("generator".into());
                }
                tokens.push(
                    if ast.kind(f) == NodeKind::Arrow {
                        "arrow function"
                    } else {
                        "method"
                    }
                    .into(),
                );
                if let Some(n) = key_name(ast, src, key) {
                    tokens.push(if is_private {
                        format!("#{n}")
                    } else {
                        format!("'{n}'")
                    });
                }
                return tokens.join(" ");
            }
            _ => {}
        }
    }
    if is_async {
        tokens.push("async".into());
    }
    if is_gen {
        tokens.push("generator".into());
    }
    tokens.push(
        if ast.kind(f) == NodeKind::Arrow {
            "arrow function"
        } else {
            "function"
        }
        .into(),
    );
    if let Some(n) = own_name {
        tokens.push(format!("'{n}'"));
    } else if let Some(p) = parent {
        // `const foo = function() {}` / `{ foo: () => {} }`
        let inferred = match *ast.node(p) {
            Node::VarDeclarator { name, .. } => ident_name(ast, name).map(str::to_owned),
            Node::Property { key, .. } if !matches!(ast.node(key), Node::ComputedKey { .. }) => {
                key_name(ast, src, key).map(|c| c.into_owned())
            }
            Node::Property { key, .. } => key_name(ast, src, key).map(|c| c.into_owned()),
            Node::Assign { left, .. } => ident_name(ast, left).map(str::to_owned),
            _ => None,
        };
        if let Some(n) = inferred {
            tokens.push(format!("'{n}'"));
        }
    }
    tokens.join(" ")
}

/// The span to report for a function, like ESLint's `getFunctionHeadLoc`.
pub fn function_head_span(ast: &Ast, src: &str, parents: &ParentMap, f: NodeId) -> Span {
    let span = ast.span(f);
    if let Some(p) = parents.parent(f)
        && matches!(*ast.node(p), Node::Property { value, .. } if value == f)
    {
        // `{ key: function () {} }` / `{ key: () => {} }`: from the key to the `(`.
        let start = ast.span(p).start;
        if let Some(paren) = paren_after(src, start, span.end) {
            return Span::new(start, paren);
        }
    }
    match *ast.node(f) {
        Node::Arrow { body, .. } => {
            // From the arrow token to its end... ESLint uses the `=>` token loc.
            let body_start = ast.span(body).start as usize;
            let before = src.get(span.start as usize..body_start).unwrap_or("");
            match before.rfind("=>") {
                Some(i) => Span::new(span.start + i as u32, span.start + i as u32 + 2),
                None => span,
            }
        }
        _ => {
            let parent = parents.parent(f);
            if let Some(p) = parent
                && matches!(
                    ast.node(p),
                    Node::MethodDef { .. } | Node::PropertyDef { .. }
                )
            {
                // From the start of the method (or key) to the `(` of its parameters,
                // which follows the key (a computed key can hold parentheses).
                let start = ast.span(p).start;
                let key_end = match *ast.node(p) {
                    Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => {
                        ast.span(key).end.max(start)
                    }
                    _ => start,
                };
                let paren = paren_after(src, key_end, span.end)
                    .or_else(|| paren_after(src, start, span.end))
                    .unwrap_or(span.start);
                return Span::new(start, paren);
            }
            // ESTree's function declaration starts after `export` / `default` / `declare`.
            let mut start = span.start as usize;
            if matches!(ast.node(f), Node::FunctionDecl { .. }) {
                for word in ["export", "default", "declare"] {
                    let at = skip_trivia_forward(src, start);
                    if src.get(at..).is_some_and(|r| {
                        r.starts_with(word) && !r[word.len()..].starts_with(is_ident_char)
                    }) {
                        start = at + word.len();
                    }
                }
                start = skip_trivia_forward(src, start);
            }
            let start = start as u32;
            let paren = paren_after(src, start, span.end).unwrap_or(span.end);
            Span::new(start, paren)
        }
    }
}

fn paren_after(src: &str, from: u32, to: u32) -> Option<u32> {
    let s = src.get(from as usize..to as usize)?;
    // Skip over a type parameter list and the name.
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '(' if depth <= 0 => return Some(from + i as u32),
            _ => {}
        }
    }
    None
}

/// True when `id` is inside an optional chain that short-circuits through it
/// (`a?.b.c` - `.c` continues the chain started by `?.`).
pub fn chain_has_optional(ast: &Ast, mut id: NodeId) -> bool {
    loop {
        match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            Node::NonNull { expr } => id = expr,
            _ => return false,
        }
    }
}

/// The name of a JS keyword-like global constant check: `NaN`, `Infinity`.
pub fn is_global_name(ast: &Ast, id: NodeId, name: &str) -> bool {
    ident_name(ast, skip_parens(ast, id)) == Some(name)
}

/// Iterates `node` and its ancestors.
pub fn ancestors_inclusive(parents: &ParentMap, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(Some(id), move |&n| parents.parent(n))
}

/// Statements of a block-like node (`Block`, `Program`, `ModuleBlock`, `StaticBlock`,
/// `SwitchCase`).
pub fn statement_list(ast: &Ast, id: NodeId) -> Option<&[NodeId]> {
    match *ast.node(id) {
        Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => {
            Some(ast.list(stmts))
        }
        Node::StaticBlock { body } | Node::SwitchCase { body, .. } => Some(ast.list(body)),
        _ => None,
    }
}

/// The value written for a literal regex flags/pattern.
pub fn regex_parts(ast: &Ast, id: NodeId) -> Option<(&str, &str)> {
    match *ast.node(id) {
        Node::RegexLit { pattern, flags } => Some((ast.atom(pattern), ast.atom(flags))),
        _ => None,
    }
}

/// The position of the first `ch` at or after `from` that is not inside a comment. Only use
/// it where no string literal can come first (between punctuation and keywords).
pub fn find_char(src: &str, from: u32, ch: u8) -> Option<u32> {
    let b = src.as_bytes();
    let mut pos = from as usize;
    loop {
        pos = skip_trivia_forward(src, pos);
        if pos >= b.len() {
            return None;
        }
        if b[pos] == ch {
            return Some(pos as u32);
        }
        pos += src[pos..].chars().next().map_or(1, char::len_utf8);
    }
}

/// True when `id` (an `ObjectLit` / `ArrayLit` / `Ident` / member) is the target of a
/// destructuring or plain assignment, including nested pattern positions and `for-in/of`
/// heads.
pub fn is_assignment_target(ast: &Ast, parents: &ParentMap, id: NodeId) -> bool {
    let mut cur = id;
    while let Some(p) = parents.parent(cur) {
        match *ast.node(p) {
            Node::Paren { .. } => {}
            Node::Assign { left, .. } => return left == cur,
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == cur,
            Node::ArrayLit { .. } | Node::Spread { .. } => {}
            Node::Property { value, .. } => {
                if value != cur {
                    return false;
                }
            }
            Node::ShorthandProp { .. } => {}
            Node::ObjectLit { .. } => {}
            _ => return false,
        }
        cur = p;
    }
    false
}

/// Source text with whitespace and comments between tokens removed (string and template
/// contents are kept), like joining ESLint's token values.
pub fn compact_text(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' || c == b'"' || c == b'`' {
            let q = c;
            let start = i;
            i += 1;
            while i < b.len() && b[i] != q {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&text[start..i]);
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The numeric value of a constant expression: number literals, `+`/`-`/`~`, arithmetic on
/// constants, and identifiers bound to a never-reassigned variable with a constant
/// initializer. `None` when not statically known.
pub fn static_number(cx: &crate::rule::Ctx<'_, '_>, id: NodeId) -> Option<f64> {
    static_number_depth(cx, id, 0)
}

fn static_number_depth(cx: &crate::rule::Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<f64> {
    if depth > 20 {
        return None;
    }
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::NumberLit { value } => Some(value.get()),
        Node::BoolLit { value } => Some(if value { 1.0 } else { 0.0 }),
        Node::NullLit {} => Some(0.0),
        Node::Unary { op, arg } => {
            let v = static_number_depth(cx, arg, depth + 1)?;
            match op {
                ts_r_ast::UnaryOp::Minus => Some(-v),
                ts_r_ast::UnaryOp::Plus => Some(v),
                ts_r_ast::UnaryOp::BitNot => Some(!(v as i64 as i32) as f64),
                _ => None,
            }
        }
        Node::Binary { left, op, right } => {
            let (a, b) = (
                static_number_depth(cx, left, depth + 1)?,
                static_number_depth(cx, right, depth + 1)?,
            );
            use ts_r_ast::BinaryOp::*;
            Some(match op {
                Add => a + b,
                Sub => a - b,
                Mul => a * b,
                Div => a / b,
                Mod => a % b,
                Exp => a.powf(b),
                _ => return None,
            })
        }
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let sym = sem.symbol_of_ident(id)?;
            let s = sem.symbol(sym);
            if s.decls.len() != 1 {
                return None;
            }
            let mut init = None;
            for r in &s.refs {
                let r = sem.reference(*r);
                if r.is_write() {
                    if !r.flags.contains(crate::semantic::RefFlags::INIT) || init.is_some() {
                        return None;
                    }
                    init = r.write_value;
                }
            }
            static_number_depth(cx, init?, depth + 1)
        }
        _ => None,
    }
}

/// The outermost node that writes to the target `id` (an identifier or member expression):
/// the assignment (the outermost one for destructuring), update, `for-in`/`for-of`
/// statement or `delete`. `None` when `id` is not written.
pub fn write_root(ast: &Ast, parents: &ParentMap, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    let mut found = None;
    while let Some(p) = parents.parent(cur) {
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::Spread { .. }
            | Node::ArrayPattern { .. }
            | Node::ObjectPattern { .. }
            | Node::Rest { .. } => {}
            Node::ShorthandProp { name, .. } => {
                if name != cur {
                    break;
                }
            }
            Node::Property { value, .. } => {
                if value != cur {
                    break;
                }
            }
            Node::BindingProperty { key, .. } => {
                if key == cur {
                    break;
                }
            }
            Node::AssignPattern { left, .. } => {
                if left != cur {
                    break;
                }
            }
            Node::Assign { left, .. } => {
                if left != cur {
                    break;
                }
                found = Some(p);
            }
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => {
                return if left == cur { Some(p) } else { found };
            }
            Node::Update { .. } => return Some(p),
            Node::Unary {
                op: ts_r_ast::UnaryOp::Delete,
                ..
            } => return Some(p),
            _ => break,
        }
        cur = p;
    }
    found
}

/// Operator precedence of an expression, following ESLint's `getPrecedence`: sequence 0,
/// assignment/arrow/yield 1, conditional 3, `||`/`??` 4, `&&` 5, bitwise 6-8, equality 9,
/// relational 10, shift 11, additive 12, multiplicative 13, `**` 14, unary/await 16,
/// update 17, call/new/member 18, primary 20.
pub fn precedence(ast: &Ast, id: NodeId) -> u8 {
    use ts_r_ast::BinaryOp::*;
    match *ast.node(id) {
        Node::Seq { .. } => 0,
        Node::Assign { .. } | Node::Arrow { .. } | Node::Yield { .. } => 1,
        Node::Conditional { .. } => 3,
        Node::Binary { op, .. } => match op {
            LogicalOr | Coalesce => 4,
            LogicalAnd => 5,
            BitOr => 6,
            BitXor => 7,
            BitAnd => 8,
            Eq | NotEq | StrictEq | StrictNotEq => 9,
            Lt | LtEq | Gt | GtEq | In | InstanceOf => 10,
            Shl | Shr | UShr => 11,
            Add | Sub => 12,
            Mul | Div | Mod => 13,
            Exp => 14,
        },
        Node::As { .. } | Node::Satisfies { .. } => 10,
        Node::Unary { .. } | Node::Await { .. } | Node::TypeAssertion { .. } => 16,
        Node::Update { .. } => 17,
        Node::Call { .. }
        | Node::New { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::TaggedTemplate { .. }
        | Node::NonNull { .. }
        | Node::ImportCall { .. }
        | Node::Instantiation { .. } => 18,
        _ => 20,
    }
}

/// The identifiers bound by a binding pattern (`a`, `{ a, b: [c] }`, `...d`), in source
/// order.
pub fn binding_idents(ast: &Ast, pattern: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(pattern) {
        Node::Ident { .. } => out.push(pattern),
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => {
                        binding_idents(ast, value.unwrap_or(key), out);
                    }
                    _ => binding_idents(ast, p, out),
                }
            }
        }
        Node::ArrayPattern { elements } => {
            for &e in ast.list(elements) {
                binding_idents(ast, e, out);
            }
        }
        Node::AssignPattern { left, .. } => binding_idents(ast, left, out),
        Node::Rest { arg } => binding_idents(ast, arg, out),
        Node::Param { pattern, .. } => binding_idents(ast, pattern, out),
        _ => {}
    }
}

/// The targets of an assignment pattern (`[a, b.c] = x`, `({ a, b: c = 1 } = x)`):
/// identifiers and member expressions, in source order.
pub fn assign_targets(ast: &Ast, target: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(target) {
        Node::Ident { .. } | Node::Member { .. } | Node::Index { .. } => out.push(target),
        Node::Paren { expr } => assign_targets(ast, expr, out),
        Node::ArrayLit { elements } => {
            for &e in ast.list(elements) {
                assign_targets(ast, e, out);
            }
        }
        Node::ObjectLit { props } => {
            for &p in ast.list(props) {
                assign_targets(ast, p, out);
            }
        }
        Node::Property { value, .. } => assign_targets(ast, value, out),
        Node::ShorthandProp { name, .. } => out.push(name),
        Node::Spread { arg } => assign_targets(ast, arg, out),
        Node::Assign { left, .. } => assign_targets(ast, left, out),
        Node::NonNull { expr } | Node::As { expr, .. } | Node::Satisfies { expr, .. } => {
            assign_targets(ast, expr, out)
        }
        Node::TypeAssertion { expr, .. } => assign_targets(ast, expr, out),
        _ => {}
    }
}

/// Start of the `var`/`let`/`const` keyword of a declaration (after `export`/`declare`).
pub fn decl_keyword_start(src: &str, decl_span: Span) -> u32 {
    let mut pos = decl_span.start as usize;
    loop {
        pos = skip_trivia_forward(src, pos);
        let rest = &src[pos..decl_span.end as usize];
        let word: &str = rest.split(|c: char| !is_ident_char(c)).next().unwrap_or("");
        if matches!(word, "export" | "declare" | "default") {
            pos += word.len();
        } else {
            return pos as u32;
        }
    }
}

/// True when `c` can continue an identifier.
pub fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Prefixes a space to `text` when it would otherwise merge with the token ending at `pos`
/// (`yield!!a` -> `yield a`).
pub fn space_before(src: &str, pos: u32, text: String) -> String {
    let prev = src[..pos as usize].chars().next_back();
    let first = text.chars().next();
    match (prev, first) {
        (Some(p), Some(f)) if is_ident_char(p) && is_ident_char(f) => format!(" {text}"),
        _ => text,
    }
}

/// The initializer of the `const` binding an identifier reference resolves to.
pub fn const_init(cx: &crate::rule::Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    use crate::semantic::{DeclKind, Resolution};
    let ast = cx.ast();
    let sem = cx.semantic();
    let r = sem.reference_at(ident)?;
    let Resolution::Symbol(s) = sem.reference(r).resolution else {
        return None;
    };
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    if decl.kind != DeclKind::Const {
        return None;
    }
    match *ast.node(decl.node) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => Some(init),
        _ => None,
    }
}

/// The value of a string-valued expression built from literals, templates, `+` and `const`
/// bindings (a subset of ESLint's `getStaticValue`).
pub fn static_string_eval(cx: &crate::rule::Ctx<'_, '_>, id: NodeId) -> Option<String> {
    fn eval(cx: &crate::rule::Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<String> {
        if depth > 16 {
            return None;
        }
        let ast = cx.ast();
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            Node::TemplateLit { parts } => {
                let mut s = String::new();
                for &p in ast.list(parts) {
                    match *ast.node(p) {
                        Node::TemplateElement {
                            cooked,
                            invalid_escape: false,
                        } => s.push_str(ast.atom(cooked)),
                        Node::TemplateElement { .. } => return None,
                        _ => s.push_str(&eval(cx, p, depth + 1)?),
                    }
                }
                Some(s)
            }
            Node::Binary {
                left,
                op: ts_r_ast::BinaryOp::Add,
                right,
            } => {
                let mut s = eval(cx, left, depth + 1)?;
                s.push_str(&eval(cx, right, depth + 1)?);
                Some(s)
            }
            Node::Ident { .. } => eval(cx, const_init(cx, id)?, depth + 1),
            _ => None,
        }
    }
    eval(cx, id, 0)
}

/// The arguments of `RegExp(...)` / `new RegExp(...)` calling the global `RegExp`; also
/// through `globalThis.RegExp` when `global_this`, and through `const` aliases when
/// `aliases`.
pub fn regexp_call_args<'a>(
    cx: &crate::rule::Ctx<'a, '_>,
    node: NodeId,
    global_this: bool,
    aliases: bool,
) -> Option<&'a [NodeId]> {
    fn is_ctor(cx: &crate::rule::Ctx<'_, '_>, c: NodeId, gt: bool, al: bool, depth: u32) -> bool {
        let ast = cx.ast();
        let c = skip_parens(ast, c);
        match *ast.node(c) {
            Node::Ident { .. } => {
                cx.is_global_ref(c, "RegExp")
                    || (al
                        && depth < 8
                        && const_init(cx, c).is_some_and(|i| is_ctor(cx, i, gt, al, depth + 1)))
            }
            Node::Member { obj, prop, .. } => {
                gt && ident_name(ast, prop) == Some("RegExp") && cx.is_global_ref(obj, "globalThis")
            }
            _ => false,
        }
    }
    let ast = cx.ast();
    let (callee, args) = match *ast.node(node) {
        Node::Call { callee, args, .. } | Node::New { callee, args, .. } => (callee, args),
        _ => return None,
    };
    is_ctor(cx, callee, global_this, aliases, 0).then(|| ast.list(args))
}

/// The expression's last token can be continued by a following `(`, `[`, `` ` ``, ...
/// (so a statement starting with one of those would join it without a `;`).
pub fn expr_continuable(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Update { prefix: false, .. } | Node::Yield { arg: None, .. } => false,
        Node::Yield { arg: Some(a), .. }
        | Node::Unary { arg: a, .. }
        | Node::Await { arg: a }
        | Node::Assign { right: a, .. }
        | Node::Binary { right: a, .. }
        | Node::Conditional { alt: a, .. }
        | Node::TypeAssertion { expr: a, .. } => expr_continuable(ast, a),
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&e| expr_continuable(ast, e)),
        Node::Arrow { flags, body, .. } => {
            flags.contains(ts_r_ast::FnFlags::EXPRESSION_BODY) && expr_continuable(ast, body)
        }
        Node::As { .. } | Node::Satisfies { .. } => false,
        _ => true,
    }
}

/// The statement ends without `;` in an expression that a following statement starting
/// with `(`, `[`, ... would continue.
pub fn stmt_continuable(ast: &Ast, src: &str, stmt: NodeId) -> bool {
    if ast.span(stmt).text(src).trim_end().ends_with(';') {
        return false;
    }
    match *ast.node(stmt) {
        Node::ExprStmt { expr }
        | Node::ExportDefaultExpr { expr }
        | Node::Throw { arg: expr }
        | Node::Return { arg: Some(expr) } => expr_continuable(ast, expr),
        Node::VarDecl { decls, .. } => ast.list(decls).last().is_some_and(|&d| {
            matches!(*ast.node(d), Node::VarDeclarator { init: Some(i), .. } if expr_continuable(ast, i))
        }),
        Node::Directive { .. } => true,
        _ => false,
    }
}

/// The statement before `stmt` in its statement list.
pub fn prev_statement(ast: &Ast, parents: &ParentMap, stmt: NodeId) -> Option<NodeId> {
    let list = statement_list(ast, parents.parent(stmt)?)?;
    let i = list.iter().position(|&s| s == stmt)?;
    i.checked_sub(1).map(|j| list[j])
}

/// Replacing `node` with text starting with `[`/`(` needs a leading `;` to keep it
/// from joining the previous statement.
pub fn needs_leading_semicolon(ast: &Ast, src: &str, parents: &ParentMap, node: NodeId) -> bool {
    let Some(stmt) = parents.parent(node) else {
        return false;
    };
    if !matches!(ast.node(stmt), Node::ExprStmt { .. })
        || ast.span(stmt).start != ast.span(node).start
    {
        return false;
    }
    prev_statement(ast, parents, stmt).is_some_and(|p| stmt_continuable(ast, src, p))
}

/// The first child of `id` that its source text starts with, for expressions whose first
/// token belongs to a sub-expression (`a.b`, `a()`, `a + b`, `a ? b : c`, `a!`, `a as T`).
pub fn leading_child(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => Some(obj),
        Node::Call { callee, .. } => Some(callee),
        Node::TaggedTemplate { tag, .. } => Some(tag),
        Node::Binary { left, .. } | Node::Assign { left, .. } => Some(left),
        Node::Conditional { test, .. } => Some(test),
        Node::Seq { exprs } => ast.list(exprs).first().copied(),
        Node::Update {
            prefix: false, arg, ..
        } => Some(arg),
        Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::NonNull { expr }
        | Node::Instantiation { expr, .. } => Some(expr),
        _ => None,
    }
}

/// The expression whose first token starts `id` (`{}` in `{}.a + 1`).
pub fn leftmost_expr(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match leading_child(ast, id) {
            Some(c) => id = c,
            None => return id,
        }
    }
    id
}

/// A position where a leading `{`, `function` or `class` parses differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExprStart {
    /// The start of an expression statement.
    Statement,
    /// The start of a concise arrow body.
    ArrowBody,
    /// The start of `export default <expr>`.
    ExportDefault,
}

/// Whether `id`'s first token is also the first token of a statement, a concise arrow body
/// or an `export default` expression.
pub fn expr_start(ast: &Ast, parents: &ParentMap, mut id: NodeId) -> Option<ExprStart> {
    for _ in 0..10_000 {
        let p = parents.parent(id)?;
        match *ast.node(p) {
            Node::ExprStmt { .. } => return Some(ExprStart::Statement),
            Node::ExportDefaultExpr { .. } => return Some(ExprStart::ExportDefault),
            Node::Arrow { body, .. } if body == id => return Some(ExprStart::ArrowBody),
            _ => {}
        }
        if leading_child(ast, p) != Some(id) {
            return None;
        }
        id = p;
    }
    None
}

/// True when an expression whose first token belongs to `first` (see [`leftmost_expr`])
/// needs parentheses at `start`.
pub fn needs_parens_at(ast: &Ast, start: Option<ExprStart>, first: NodeId) -> bool {
    let n = ast.node(first);
    match start {
        Some(ExprStart::Statement) => matches!(
            n,
            Node::ObjectLit { .. } | Node::FunctionExpr { .. } | Node::ClassExpr { .. }
        ),
        Some(ExprStart::ArrowBody) => matches!(n, Node::ObjectLit { .. }),
        Some(ExprStart::ExportDefault) => {
            matches!(n, Node::FunctionExpr { .. } | Node::ClassExpr { .. })
        }
        None => false,
    }
}

/// The span from the start of a keyword at `start` (`await`, `void`) through the whitespace
/// after it, up to the next token or comment.
pub fn keyword_and_space(src: &str, start: u32, keyword: &str) -> Span {
    let mut end = start as usize + keyword.len();
    let b = src.as_bytes();
    while end < b.len() && src[end..].starts_with(|c: char| c.is_whitespace()) {
        end += src[end..].chars().next().map_or(1, char::len_utf8);
    }
    Span::new(start, end as u32)
}

/// The span ESLint gives a `TemplateElement`: its raw text plus the delimiters around it
/// (the opening `` ` `` or `}`, and the closing `` ` `` or `${`).
pub fn template_element_span(src: &str, raw: Span) -> Span {
    let s = src.as_bytes();
    let end = match s.get(raw.end as usize) {
        Some(b'`') => raw.end + 1,
        Some(b'$') => raw.end + 2,
        _ => raw.end,
    };
    Span::new(raw.start.saturating_sub(1), end.min(s.len() as u32))
}

/// The span of the string literal itself for a `StringLit` or a `Directive` (whose span also
/// covers its `;` and anything up to it).
pub fn string_literal_span(ast: &Ast, src: &str, id: NodeId) -> Span {
    let span = ast.span(id);
    if !matches!(ast.node(id), Node::Directive { .. }) {
        return span;
    }
    let b = span.text(src).as_bytes();
    let Some(&quote) = b.first() else {
        return span;
    };
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == quote => return Span::new(span.start, span.start + i as u32 + 1),
            _ => i += 1,
        }
    }
    span
}
