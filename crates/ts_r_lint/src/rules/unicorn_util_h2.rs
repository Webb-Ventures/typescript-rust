//! Helpers shared by the round 5 h2 eslint-plugin-unicorn rules.

use super::prelude::*;
use ts_r_ast::Ast;

/// `expr` without parentheses and any number of `await`s around it.
pub(crate) fn skip_awaits(ast: &Ast, mut expr: NodeId) -> NodeId {
    loop {
        expr = skip_parens(ast, expr);
        match *ast.node(expr) {
            Node::Await { arg } => expr = arg,
            _ => return expr,
        }
    }
}

/// `expr` without `await`s, or, when that is an identifier, the initialiser (without
/// `await`s) of the `const` it names, provided the binding is a plain identifier declared
/// once before `expr` and read only here. `None` for any other identifier.
pub(crate) fn direct_or_single_use_const(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let expr = skip_awaits(ast, expr);
    if !matches!(ast.node(expr), Node::Ident { .. }) {
        return Some(expr);
    }
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(expr)?).symbol()?;
    let sym = sem.symbol(sym);
    let [decl] = sym.decls[..] else {
        return None;
    };
    // The declaration's own initialising write counts as one of the two references.
    if sym.refs.len() != 2 || decl.kind != DeclKind::Const {
        return None;
    }
    let Node::VarDeclarator { name, init, .. } = *ast.node(decl.node) else {
        return None;
    };
    if name != decl.name || cx.span(name).start > cx.span(expr).start {
        return None;
    }
    if !matches!(
        ast.node(decl.parent),
        Node::VarDecl {
            kind: VarKind::Const,
            ..
        }
    ) {
        return None;
    }
    Some(skip_awaits(ast, init?))
}

/// The value of a JSX attribute string as ESTree parsers give it: character references
/// (`&#45;`, `&#x2D;`, and the HTML 4 names plus `&apos;`) decoded. A reference is at most
/// ten characters before its `;`; anything else stays as written.
pub(crate) fn decode_jsx_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after
            .char_indices()
            .take(11)
            .find(|&(_, c)| c == ';')
            .and_then(|(semi, _)| Some((semi, entity(&after[..semi])?)));
        match decoded {
            Some((semi, c)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(body: &str) -> Option<char> {
    if let Some(num) = body.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix('x') {
            Some(hex) => (hex, 16),
            None => (num, 10),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        // `String.fromCharCode` keeps the low 16 bits; a lone surrogate cannot be kept.
        let v = digits.chars().fold(0u32, |v, c| {
            (v.wrapping_mul(radix) + c.to_digit(radix).unwrap_or(0)) & 0xffff
        });
        return Some(char::from_u32(v).unwrap_or('\u{fffd}'));
    }
    let i = HTML_ENTITIES
        .binary_search_by(|(n, _)| (*n).cmp(body))
        .ok()?;
    char::from_u32(HTML_ENTITIES[i].1)
}

/// The HTML 4 named character references and `apos`, sorted by name (byte order).
const HTML_ENTITIES: &[(&str, u32)] = &[
    ("AElig", 198),
    ("Aacute", 193),
    ("Acirc", 194),
    ("Agrave", 192),
    ("Alpha", 913),
    ("Aring", 197),
    ("Atilde", 195),
    ("Auml", 196),
    ("Beta", 914),
    ("Ccedil", 199),
    ("Chi", 935),
    ("Dagger", 8225),
    ("Delta", 916),
    ("ETH", 208),
    ("Eacute", 201),
    ("Ecirc", 202),
    ("Egrave", 200),
    ("Epsilon", 917),
    ("Eta", 919),
    ("Euml", 203),
    ("Gamma", 915),
    ("Iacute", 205),
    ("Icirc", 206),
    ("Igrave", 204),
    ("Iota", 921),
    ("Iuml", 207),
    ("Kappa", 922),
    ("Lambda", 923),
    ("Mu", 924),
    ("Ntilde", 209),
    ("Nu", 925),
    ("OElig", 338),
    ("Oacute", 211),
    ("Ocirc", 212),
    ("Ograve", 210),
    ("Omega", 937),
    ("Omicron", 927),
    ("Oslash", 216),
    ("Otilde", 213),
    ("Ouml", 214),
    ("Phi", 934),
    ("Pi", 928),
    ("Prime", 8243),
    ("Psi", 936),
    ("Rho", 929),
    ("Scaron", 352),
    ("Sigma", 931),
    ("THORN", 222),
    ("Tau", 932),
    ("Theta", 920),
    ("Uacute", 218),
    ("Ucirc", 219),
    ("Ugrave", 217),
    ("Upsilon", 933),
    ("Uuml", 220),
    ("Xi", 926),
    ("Yacute", 221),
    ("Yuml", 376),
    ("Zeta", 918),
    ("aacute", 225),
    ("acirc", 226),
    ("acute", 180),
    ("aelig", 230),
    ("agrave", 224),
    ("alefsym", 8501),
    ("alpha", 945),
    ("amp", 38),
    ("and", 8743),
    ("ang", 8736),
    ("apos", 39),
    ("aring", 229),
    ("asymp", 8776),
    ("atilde", 227),
    ("auml", 228),
    ("bdquo", 8222),
    ("beta", 946),
    ("brvbar", 166),
    ("bull", 8226),
    ("cap", 8745),
    ("ccedil", 231),
    ("cedil", 184),
    ("cent", 162),
    ("chi", 967),
    ("circ", 710),
    ("clubs", 9827),
    ("cong", 8773),
    ("copy", 169),
    ("crarr", 8629),
    ("cup", 8746),
    ("curren", 164),
    ("dArr", 8659),
    ("dagger", 8224),
    ("darr", 8595),
    ("deg", 176),
    ("delta", 948),
    ("diams", 9830),
    ("divide", 247),
    ("eacute", 233),
    ("ecirc", 234),
    ("egrave", 232),
    ("empty", 8709),
    ("emsp", 8195),
    ("ensp", 8194),
    ("epsilon", 949),
    ("equiv", 8801),
    ("eta", 951),
    ("eth", 240),
    ("euml", 235),
    ("euro", 8364),
    ("exist", 8707),
    ("fnof", 402),
    ("forall", 8704),
    ("frac12", 189),
    ("frac14", 188),
    ("frac34", 190),
    ("frasl", 8260),
    ("gamma", 947),
    ("ge", 8805),
    ("gt", 62),
    ("hArr", 8660),
    ("harr", 8596),
    ("hearts", 9829),
    ("hellip", 8230),
    ("iacute", 237),
    ("icirc", 238),
    ("iexcl", 161),
    ("igrave", 236),
    ("image", 8465),
    ("infin", 8734),
    ("int", 8747),
    ("iota", 953),
    ("iquest", 191),
    ("isin", 8712),
    ("iuml", 239),
    ("kappa", 954),
    ("lArr", 8656),
    ("lambda", 955),
    ("lang", 9001),
    ("laquo", 171),
    ("larr", 8592),
    ("lceil", 8968),
    ("ldquo", 8220),
    ("le", 8804),
    ("lfloor", 8970),
    ("lowast", 8727),
    ("loz", 9674),
    ("lrm", 8206),
    ("lsaquo", 8249),
    ("lsquo", 8216),
    ("lt", 60),
    ("macr", 175),
    ("mdash", 8212),
    ("micro", 181),
    ("middot", 183),
    ("minus", 8722),
    ("mu", 956),
    ("nabla", 8711),
    ("nbsp", 160),
    ("ndash", 8211),
    ("ne", 8800),
    ("ni", 8715),
    ("not", 172),
    ("notin", 8713),
    ("nsub", 8836),
    ("ntilde", 241),
    ("nu", 957),
    ("oacute", 243),
    ("ocirc", 244),
    ("oelig", 339),
    ("ograve", 242),
    ("oline", 8254),
    ("omega", 969),
    ("omicron", 959),
    ("oplus", 8853),
    ("or", 8744),
    ("ordf", 170),
    ("ordm", 186),
    ("oslash", 248),
    ("otilde", 245),
    ("otimes", 8855),
    ("ouml", 246),
    ("para", 182),
    ("part", 8706),
    ("permil", 8240),
    ("perp", 8869),
    ("phi", 966),
    ("pi", 960),
    ("piv", 982),
    ("plusmn", 177),
    ("pound", 163),
    ("prime", 8242),
    ("prod", 8719),
    ("prop", 8733),
    ("psi", 968),
    ("quot", 34),
    ("rArr", 8658),
    ("radic", 8730),
    ("rang", 9002),
    ("raquo", 187),
    ("rarr", 8594),
    ("rceil", 8969),
    ("rdquo", 8221),
    ("real", 8476),
    ("reg", 174),
    ("rfloor", 8971),
    ("rho", 961),
    ("rlm", 8207),
    ("rsaquo", 8250),
    ("rsquo", 8217),
    ("sbquo", 8218),
    ("scaron", 353),
    ("sdot", 8901),
    ("sect", 167),
    ("shy", 173),
    ("sigma", 963),
    ("sigmaf", 962),
    ("sim", 8764),
    ("spades", 9824),
    ("sub", 8834),
    ("sube", 8838),
    ("sum", 8721),
    ("sup", 8835),
    ("sup1", 185),
    ("sup2", 178),
    ("sup3", 179),
    ("supe", 8839),
    ("szlig", 223),
    ("tau", 964),
    ("there4", 8756),
    ("theta", 952),
    ("thetasym", 977),
    ("thinsp", 8201),
    ("thorn", 254),
    ("tilde", 732),
    ("times", 215),
    ("trade", 8482),
    ("uArr", 8657),
    ("uacute", 250),
    ("uarr", 8593),
    ("ucirc", 251),
    ("ugrave", 249),
    ("uml", 168),
    ("upsih", 978),
    ("upsilon", 965),
    ("uuml", 252),
    ("weierp", 8472),
    ("xi", 958),
    ("yacute", 253),
    ("yen", 165),
    ("yuml", 255),
    ("zeta", 950),
    ("zwj", 8205),
    ("zwnj", 8204),
];

/// Adds to `fix` the edits that turn the call `obj.name(...)` into `obj`: the member's
/// `.name` (from the end of the parenthesized object) and everything from the end of the
/// parenthesized callee to the end of the call.
pub(crate) fn remove_method_call(cx: &Ctx<'_, '_>, call: NodeId, fix: Fix) -> Fix {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *ast.node(call) else {
        return fix;
    };
    let member = skip_parens(ast, callee);
    let Node::Member { obj, .. } = *ast.node(member) else {
        return fix;
    };
    let obj_end = super::unicorn_text::paren_span(cx, obj).end;
    let member_span = cx.span(member);
    let callee_end = super::unicorn_text::paren_span(cx, member).end;
    fix.and(Span::new(obj_end, member_span.end), "")
        .and(Span::new(callee_end, cx.span(call).end), "")
}

/// eslint-utils' `hasSideEffect` with `considerGetters` and `considerImplicitTypeConversion`:
/// calls, `new`, assignments, updates, `await`, `yield`, `import()`, `delete` and every
/// member access are side effects, and so are the converting operators (`+x`, `a + b`,
/// `a < b`, `k in o`, ...) unless their operands are literals, and computed keys that are
/// not literals. Function bodies are not looked into (a function declaration's is).
pub(crate) fn has_side_effect_strict(ast: &Ast, id: NodeId) -> bool {
    strict_effect(ast, id, 0)
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

fn computed_non_literal(ast: &Ast, key: NodeId) -> bool {
    matches!(*ast.node(key), Node::ComputedKey { expr } if !is_literal(ast, expr))
}

fn strict_effect(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return true;
    }
    match *ast.node(id) {
        Node::Call { .. }
        | Node::New { .. }
        | Node::Assign { .. }
        | Node::Update { .. }
        | Node::Await { .. }
        | Node::Yield { .. }
        | Node::ImportCall { .. }
        | Node::Member { .. }
        | Node::Index { .. } => return true,
        Node::FunctionExpr { .. } | Node::Arrow { .. } => return false,
        Node::Unary { op, arg } => {
            if op == UnaryOp::Delete
                || (matches!(
                    op,
                    UnaryOp::Minus | UnaryOp::Plus | UnaryOp::Not | UnaryOp::BitNot
                ) && !is_literal(ast, arg))
            {
                return true;
            }
        }
        Node::Binary { left, op, right } => {
            let converts = !matches!(
                op,
                BinaryOp::StrictEq
                    | BinaryOp::StrictNotEq
                    | BinaryOp::Exp
                    | BinaryOp::InstanceOf
                    | BinaryOp::LogicalAnd
                    | BinaryOp::LogicalOr
                    | BinaryOp::Coalesce
            );
            if converts && !(is_literal(ast, left) && is_literal(ast, right)) {
                return true;
            }
        }
        Node::Property { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertyDef { key, .. }
            if computed_non_literal(ast, key) =>
        {
            return true;
        }
        _ => {}
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        if !found && !is_type_node(ast, c) && strict_effect(ast, c, depth + 1) {
            found = true;
        }
    });
    found
}

/// Type annotations and arguments are never evaluated.
fn is_type_node(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::TypeArgs { .. }
            | Node::TypeParams { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::TypeLiteral { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::FunctionType { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::ParenType { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::MappedType { .. }
            | Node::ImportType { .. }
            | Node::TemplateLitType { .. }
    )
}

/// The static name of a non-computed property key (`a`, `"a"`, `0`), as `getPropertyName`
/// gives it.
pub(crate) fn plain_key_name(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => Some(ast.atom(name).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::BigIntLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    }
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The offset where the line holding `pos` starts.
pub(crate) fn line_start(src: &str, pos: usize) -> usize {
    src[..pos]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_line_break(c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// The spaces and tabs a line starts with (ESLint's `getLineIndent`).
pub(crate) fn line_indent(src: &str, pos: usize) -> &str {
    let start = line_start(src, pos);
    let rest = &src[start..];
    &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()]
}

/// The file's lines as ESLint splits them (`\r\n`, `\n`, `\r`, U+2028, U+2029), with their
/// start offsets.
fn source_lines(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = src.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_line_break(c) {
            out.push((start, &src[start..i]));
            let mut next = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                chars.next();
                next += 1;
            }
            start = next;
        }
    }
    out.push((start, &src[start..]));
    out
}

/// One level of indentation in the file (the plugin's `getIndentUnit`, built on
/// `detect-indent`): the most common change of indentation between consecutive non-empty
/// lines, spaces or tabs, with continuation lines of multi-line tokens ignored. A tab when
/// the file indents with tabs or not at all.
pub(crate) fn indent_unit(cx: &Ctx<'_, '_>) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let lines = source_lines(src);
    let line_of = |pos: usize| lines.partition_point(|&(s, _)| s <= pos).saturating_sub(1);
    let mut blank = vec![false; lines.len()];
    let mut mark = |span: Span| {
        let (a, b) = (line_of(span.start as usize), line_of(span.end as usize));
        for flag in blank.iter_mut().take(b + 1).skip(a + 1) {
            *flag = true;
        }
    };
    for c in &ast.comments {
        mark(c.span);
    }
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        if matches!(
            ast.node(id),
            Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText {}
        ) {
            mark(cx.span(id));
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    let text: Vec<&str> = lines
        .iter()
        .zip(&blank)
        .map(|(&(_, l), &b)| if b { "" } else { l })
        .collect();
    let mut best = indent_stats(&text, true);
    if best.is_empty() {
        best = indent_stats(&text, false);
    }
    let mut result: Option<((bool, usize), (u32, u32))> = None;
    for &(key, (used, weight)) in &best {
        if result.is_none_or(|(_, (u, w))| used > u || (used == u && weight > w)) {
            result = Some((key, (used, weight)));
        }
    }
    match result {
        Some(((true, n), _)) => " ".repeat(n),
        _ => "\t".to_owned(),
    }
}

/// `(is_space, size)` keys with `(uses, weight)`, in first-seen order.
fn indent_stats(lines: &[&str], ignore_single_spaces: bool) -> Vec<((bool, usize), (u32, u32))> {
    let mut stats: Vec<((bool, usize), (u32, u32))> = Vec::new();
    let mut previous = 0usize;
    let mut previous_type: Option<bool> = None;
    let mut key: Option<(bool, usize)> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let tabs = line.len() - line.trim_start_matches('\t').len();
        if spaces == 0 && tabs == 0 {
            previous = 0;
            previous_type = None;
            continue;
        }
        let (is_space, indent) = if spaces > 0 {
            (true, spaces)
        } else {
            (false, tabs)
        };
        if ignore_single_spaces && is_space && indent == 1 {
            continue;
        }
        if previous_type != Some(is_space) {
            previous = 0;
        }
        previous_type = Some(is_space);
        let diff = indent as isize - previous as isize;
        previous = indent;
        let (used, weight) = if diff == 0 {
            (0, 1)
        } else {
            let abs = diff.unsigned_abs();
            if ignore_single_spaces && is_space && abs == 1 {
                continue;
            }
            key = Some((is_space, abs));
            (1, 0)
        };
        let Some(k) = key else {
            continue;
        };
        match stats.iter_mut().find(|(sk, _)| *sk == k) {
            Some((_, (u, w))) => {
                *u += used;
                *w += weight;
            }
            None => stats.push((k, (1, 0))),
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::{HTML_ENTITIES, decode_jsx_entities};

    #[test]
    fn entities() {
        assert!(HTML_ENTITIES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(HTML_ENTITIES.len(), 253);
        assert_eq!(
            decode_jsx_entities("a&#45;b&#x2D;&amp;&nope;&"),
            "a-b-&&nope;&"
        );
        assert_eq!(decode_jsx_entities("&#39;x&#39;&#59;"), "'x';");
    }
}
