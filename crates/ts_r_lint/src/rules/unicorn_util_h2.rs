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
