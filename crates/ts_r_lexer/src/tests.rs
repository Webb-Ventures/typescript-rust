// Real-world or generated input: abort instead of exhausting the machine's memory.
ts_r_testing::cap_memory!();
use super::*;
use TokenKind as T;

fn lex(src: &str) -> Vec<(TokenKind, String)> {
    let mut lx = Lexer::new(src);
    let mut out = Vec::new();
    loop {
        let k = lx.next_token();
        if k == T::Eof {
            break;
        }
        out.push((k, lx.token_text().to_owned()));
        if out.len() > 10_000 {
            panic!("runaway lexer");
        }
    }
    out
}

fn kinds(src: &str) -> Vec<TokenKind> {
    lex(src).into_iter().map(|(k, _)| k).collect()
}

fn errors(src: &str) -> Vec<u32> {
    let mut lx = Lexer::new(src);
    while lx.next_token() != T::Eof {}
    lx.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn punctuators() {
    assert_eq!(
        kinds(
            "{ } ( ) [ ] . ... ; , < > <= == != === !== => + - * ** / % ++ -- << & | ^ ! ~ && || ? ?? ?. : @ #"
        ),
        vec![
            T::LBrace,
            T::RBrace,
            T::LParen,
            T::RParen,
            T::LBracket,
            T::RBracket,
            T::Dot,
            T::DotDotDot,
            T::Semi,
            T::Comma,
            T::Lt,
            T::Gt,
            T::LtEq,
            T::EqEq,
            T::NotEq,
            T::EqEqEq,
            T::NotEqEq,
            T::Arrow,
            T::Plus,
            T::Minus,
            T::Star,
            T::StarStar,
            T::Slash,
            T::Percent,
            T::PlusPlus,
            T::MinusMinus,
            T::LtLt,
            T::Amp,
            T::Pipe,
            T::Caret,
            T::Bang,
            T::Tilde,
            T::AmpAmp,
            T::PipePipe,
            T::Question,
            T::QuestionQuestion,
            T::QuestionDot,
            T::Colon,
            T::At,
            T::PrivateName,
        ]
    );
    assert_eq!(
        kinds("= += -= *= **= /= %= <<= &= |= ^= &&= ||= ??="),
        vec![
            T::Eq,
            T::PlusEq,
            T::MinusEq,
            T::StarEq,
            T::StarStarEq,
            T::SlashEq,
            T::PercentEq,
            T::LtLtEq,
            T::AmpEq,
            T::PipeEq,
            T::CaretEq,
            T::AmpAmpEq,
            T::PipePipeEq,
            T::QuestionQuestionEq,
        ]
    );
    // `?.` followed by a digit is a conditional.
    assert_eq!(
        kinds("a?.5:1"),
        vec![T::Ident, T::Question, T::Number, T::Colon, T::Number]
    );
}

#[test]
fn greater_is_split_and_rescanned() {
    assert_eq!(kinds(">>>="), vec![T::Gt, T::Gt, T::Gt, T::Eq]);
    for (src, want) in [
        (">", T::Gt),
        (">=", T::GtEq),
        (">>", T::GtGt),
        (">>=", T::GtGtEq),
        (">>>", T::GtGtGt),
        (">>>=", T::GtGtGtEq),
    ] {
        let mut lx = Lexer::new(src);
        lx.next_token();
        assert_eq!(lx.rescan_greater(), want, "{src}");
        assert_eq!(lx.next_token(), T::Eof);
    }
    // Not merged across whitespace.
    let mut lx = Lexer::new("> >");
    lx.next_token();
    assert_eq!(lx.rescan_greater(), T::Gt);
}

#[test]
fn identifiers_and_keywords() {
    assert_eq!(
        lex("foo $bar _baz9 class let type ñame 中文"),
        vec![
            (T::Ident, "foo".into()),
            (T::Ident, "$bar".into()),
            (T::Ident, "_baz9".into()),
            (T::Class, "class".into()),
            (T::Let, "let".into()),
            (T::Type, "type".into()),
            (T::Ident, "ñame".into()),
            (T::Ident, "中文".into()),
        ]
    );
    let mut lx = Lexer::new(concat!("\\", "u0061bc a\\u{62}c"));
    lx.next_token();
    assert_eq!(lx.ident_value(lx.token()), "abc");
    assert!(lx.token().flags.has(TokenFlags::ESCAPED));
    lx.next_token();
    assert_eq!(lx.ident_value(lx.token()), "abc");
    // An escaped keyword is the keyword, flagged.
    let mut lx = Lexer::new(concat!("\\", "u0076ar"));
    assert_eq!(lx.next_token(), T::Var);
    assert!(lx.token().flags.has(TokenFlags::ESCAPED_KEYWORD));
    assert_eq!(kinds("#priv #"), vec![T::PrivateName, T::PrivateName]);
    // After an ASCII prefix, escapes and non-ASCII characters only need to continue it.
    let mut lx = Lexer::new(concat!("a", "\\", "u0030"));
    lx.next_token();
    assert_eq!(lx.ident_value(lx.token()), "a0");
    assert_eq!(lex("e\u{301}x"), vec![(T::Ident, "e\u{301}x".into())]);
    assert!(errors(concat!("const a", "\\", "u0030 = 1;")).is_empty());
    // An escape still has to start an identifier on its own.
    assert_eq!(errors(concat!("\\", "u0030")), vec![1127]);
}

#[test]
fn numbers() {
    let cases = [
        ("0", 0.0),
        ("123", 123.0),
        ("1_000_000", 1e6),
        ("0x1f", 31.0),
        ("0XFF", 255.0),
        ("0o17", 15.0),
        ("0b1010", 10.0),
        ("017", 15.0),
        ("089", 89.0),
        ("1.5", 1.5),
        (".25", 0.25),
        ("5.", 5.0),
        ("1e10", 1e10),
        ("2E-3", 0.002),
        ("0.0001", 0.0001),
    ];
    for (src, want) in cases {
        let mut lx = Lexer::new(src);
        assert_eq!(lx.next_token(), T::Number, "{src}");
        assert_eq!(lx.token_text(), src);
        assert_eq!(lx.number_value(lx.token()), want, "{src}");
        assert!(lx.diagnostics.is_empty(), "{src}: {:?}", lx.diagnostics);
    }
    assert_eq!(
        kinds("10n 0x1Fn 0b1n"),
        vec![T::BigInt, T::BigInt, T::BigInt]
    );
    assert_eq!(
        lex("1..toString"),
        vec![
            (T::Number, "1.".into()),
            (T::Dot, ".".into()),
            (T::Ident, "toString".into())
        ]
    );
    assert_eq!(errors("1__0"), vec![6189]);
    assert_eq!(errors("1_"), vec![6188]);
    assert_eq!(errors("0x"), vec![1125]);
    assert_eq!(errors("3in"), vec![1351]);
    assert_eq!(errors("1e"), vec![1124]);
    // TS1351 only after a plain decimal literal; the name is its own token (`3in x`).
    assert_eq!(kinds("3in x"), vec![T::Number, T::In, T::Ident]);
    assert_eq!(errors("0x1g"), Vec::<u32>::new());
    assert_eq!(errors("0b1x"), Vec::<u32>::new());
    // `0_1`: a separator after a leading zero, reported once.
    assert_eq!(errors("0_1"), vec![6188]);
    assert_eq!(errors("0_1_2"), vec![6188]);
    // Bigint literal errors (parseBigInt).
    assert_eq!(errors("0bn 0on 0xn"), vec![1177, 1178, 1125]);
    assert_eq!(errors("1e2n 4.1n .1n"), vec![1352, 1353, 1353]);
    assert_eq!(kinds("1e2n"), vec![T::Number]);
    assert_eq!(errors("0123n"), Vec::<u32>::new());
    // A backslash at the end of the file ends a string or template: TS1126.
    assert_eq!(errors("\"\\"), vec![1126]);
    assert_eq!(errors("`\\"), vec![1126]);
    // A lone `#`: TS1127.
    assert_eq!(errors("# x"), vec![1127]);
}

#[test]
fn strings() {
    let mut lx = Lexer::new(
        r#""a\"b" 'c\'d' "\x41\u{1F600}" 'line\
cont'"#,
    );
    let mut vals = Vec::new();
    while lx.next_token() != T::Eof {
        assert_eq!(lx.kind(), T::String);
        vals.push(lx.string_value(lx.token()).into_owned());
    }
    assert_eq!(vals, ["a\"b", "c'd", "A😀", "linecont"]);
    assert!(lx.diagnostics.is_empty());
    assert_eq!(errors("'abc\nx'"), vec![1002, 1002]);
    assert_eq!(errors("\"abc"), vec![1002]);
}

#[test]
fn comments_and_newlines() {
    let mut lx = Lexer::new("#!/usr/bin/env node\na // c1\n/* c2 */ b /* multi\nline */ c");
    assert_eq!(lx.next_token(), T::Ident);
    assert!(lx.token().newline_before());
    assert_eq!(lx.next_token(), T::Ident);
    assert!(lx.token().newline_before());
    assert_eq!(lx.next_token(), T::Ident);
    assert!(
        lx.token().newline_before(),
        "multi-line comment counts as a line break"
    );
    let kinds: Vec<_> = lx.comments.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            CommentKind::Hashbang,
            CommentKind::Line,
            CommentKind::Block,
            CommentKind::Block
        ]
    );
    assert_eq!(lx.text(lx.comments[1].span), "// c1");
    assert!(lx.comments[2].newline_before);
    assert_eq!(errors("/* open"), vec![1010]);
    // Unicode whitespace and line separators.
    let mut lx = Lexer::new("a\u{00A0}\u{FEFF}b\u{2028}c");
    lx.next_token();
    lx.next_token();
    assert!(!lx.token().newline_before());
    lx.next_token();
    assert!(lx.token().newline_before());
    assert_eq!(lx.token_text(), "c");
    // U+0085 (NEXT LINE) is a space, not a line break and not an invalid character.
    let mut lx = Lexer::new("=\u{0085}0");
    lx.next_token();
    lx.next_token();
    assert!(!lx.token().newline_before());
    assert_eq!(lx.token_text(), "0");
    assert!(lx.diagnostics.is_empty());
    // A keyword directly followed by non-ASCII whitespace is still a keyword
    // (fileWithNextLine3: `return` + U+0085 + `0`).
    let mut lx = Lexer::new("return\u{0085}0 if\u{00A0}x");
    lx.next_token();
    assert_eq!(lx.kind(), TokenKind::Return);
    lx.next_token();
    assert_eq!(lx.token_text(), "0");
    lx.next_token();
    assert_eq!(lx.kind(), TokenKind::If);
}

#[test]
fn templates() {
    let mut lx = Lexer::new("`a${b}c${d}e` `plain`");
    assert_eq!(lx.next_token(), T::TemplateHead);
    assert_eq!(lx.template_cooked(lx.token()).unwrap(), "a");
    assert_eq!(lx.next_token(), T::Ident);
    assert_eq!(lx.next_token(), T::RBrace);
    assert_eq!(lx.rescan_template_continuation(), T::TemplateMiddle);
    assert_eq!(lx.template_cooked(lx.token()).unwrap(), "c");
    assert_eq!(lx.next_token(), T::Ident);
    assert_eq!(lx.next_token(), T::RBrace);
    assert_eq!(lx.rescan_template_continuation(), T::TemplateTail);
    assert_eq!(lx.text(lx.template_raw_span(lx.token())), "e");
    assert_eq!(lx.next_token(), T::NoSubstTemplate);
    assert_eq!(lx.template_cooked(lx.token()).unwrap(), "plain");
    assert_eq!(lx.next_token(), T::Eof);

    let mut lx = Lexer::new(r"`\unicode`");
    lx.next_token();
    assert!(lx.template_cooked(lx.token()).is_err());
    assert_eq!(errors("`abc"), vec![1160]);
    assert_eq!(kinds("`$`"), vec![T::NoSubstTemplate]);
}

#[test]
fn regex_rescan() {
    let mut lx = Lexer::new(r"/[/]\/x/gi.test");
    assert_eq!(lx.next_token(), T::Slash);
    assert_eq!(lx.rescan_slash_as_regex(), T::Regex);
    assert_eq!(lx.token_text(), r"/[/]\/x/gi");
    let (p, f) = lx.regex_parts(lx.token());
    assert_eq!(lx.text(p), r"[/]\/x");
    assert_eq!(lx.text(f), "gi");
    assert_eq!(lx.next_token(), T::Dot);
    let mut lx = Lexer::new("/=a/");
    assert_eq!(lx.next_token(), T::SlashEq);
    assert_eq!(lx.rescan_slash_as_regex(), T::Regex);
    assert_eq!(lx.token_text(), "/=a/");
    let mut lx = Lexer::new("/abc\n/");
    lx.next_token();
    lx.rescan_slash_as_regex();
    assert_eq!(lx.diagnostics[0].code, 1161);
    // An unterminated regex ends at a `)` it did not open, or before a trailing `;`.
    for (src, text) in [
        ("/a (b) c) d", "/a (b) c"),
        ("/a b;;", "/a b"),
        ("/a; b", "/a; b"),
    ] {
        let mut lx = Lexer::new(src);
        lx.next_token();
        lx.rescan_slash_as_regex();
        assert_eq!(lx.token_text(), text);
    }
    // Pattern errors (the parser defers them).
    let codes = |src: &str| {
        let mut lx = Lexer::new(src);
        lx.next_token();
        lx.rescan_slash_as_regex();
        lx.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>()
    };
    assert_eq!(codes(r"/[b-a]/"), vec![1517]);
    assert_eq!(codes(r"/[\u{1F600}-\u{1F601}]/u"), Vec::<u32>::new());
    assert_eq!(codes(r"/[😀-😁]/"), vec![1517]);
    assert_eq!(codes(r"/\x4/"), vec![1125]);
    assert_eq!(codes(r"/a}]/u"), vec![1508, 1508]);
    assert_eq!(codes(r"/a{1,2}\p{L}[\q{x}]/v"), Vec::<u32>::new());
}

#[test]
fn jsx_modes() {
    let src = "<div data-x=\"a\\b\nc\">hi {x} &amp; </div>";
    let mut lx = Lexer::new(src);
    assert_eq!(lx.next_token(), T::Lt);
    assert_eq!(lx.next_token(), T::Ident);
    assert_eq!(lx.next_token(), T::Ident);
    assert_eq!(lx.rescan_jsx_identifier(), T::Ident);
    assert_eq!(lx.token_text(), "data-x");
    assert_eq!(lx.next_token(), T::Eq);
    assert_eq!(lx.next_jsx_attribute_value(), T::String);
    assert_eq!(lx.token_text(), "\"a\\b\nc\"");
    assert_eq!(lx.next_token(), T::Gt);
    assert_eq!(lx.next_jsx_child(), T::JsxText);
    assert_eq!(lx.token_text(), "hi ");
    assert_eq!(lx.next_jsx_child(), T::LBrace);
    assert_eq!(lx.next_token(), T::Ident);
    assert_eq!(lx.next_token(), T::RBrace);
    assert_eq!(lx.next_jsx_child(), T::JsxText);
    assert_eq!(lx.token_text(), " &amp; ");
    assert_eq!(lx.next_jsx_child(), T::Lt);
    assert_eq!(lx.next_token(), T::Slash);
    assert!(lx.diagnostics.is_empty());
}

#[test]
fn checkpoint_rewind() {
    let mut lx = Lexer::new("a /* c */ b c");
    lx.next_token();
    let cp = lx.checkpoint();
    lx.next_token();
    lx.next_token();
    assert_eq!(lx.comments.len(), 1);
    lx.rewind(cp);
    assert_eq!(lx.comments.len(), 0);
    assert_eq!(lx.token_text(), "a");
    lx.next_token();
    assert_eq!(lx.token_text(), "b");
    assert_eq!(lx.comments.len(), 1);
}

#[test]
fn invalid_chars_never_panic() {
    assert_eq!(errors("a ¬ b"), vec![1127]);
    assert_eq!(kinds("\u{1}"), vec![T::Unknown]);
    for src in [
        "\\",
        "\\u",
        "\\u{",
        "\\u{110000}",
        "'\\",
        "`\\",
        "0x",
        "0b2",
        "#!",
        "a#!b",
        "\u{FEFF}",
        "`${",
        "/*",
        "\r",
        "'\r",
    ] {
        let _ = lex(src);
    }
    assert_eq!(errors("a #!b"), vec![18026]);
}

#[test]
fn bom_and_split_less_than() {
    assert_eq!(kinds("\u{FEFF}x"), vec![T::Ident]);
    let mut lx = Lexer::new("<<T>");
    assert_eq!(lx.next_token(), T::LtLt);
    lx.split_less_than();
    assert_eq!(lx.kind(), T::Lt);
    assert_eq!(lx.next_token(), T::Lt);
    assert_eq!(lx.next_token(), T::Ident);
}
