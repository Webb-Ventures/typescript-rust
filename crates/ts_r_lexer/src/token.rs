//! Token kinds and the token record.

/// Kind of a token. Keywords come last: `Break..=With` are reserved words, `Implements..=Yield`
/// are strict-mode reserved words, and the rest are contextual keywords that the parser may
/// treat as identifiers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum TokenKind {
    #[default]
    Eof,
    /// An invalid character (already reported).
    Unknown,

    // Literals and names.
    Ident,
    /// `#name`
    PrivateName,
    Number,
    BigInt,
    String,
    Regex,
    NoSubstTemplate,
    TemplateHead,
    TemplateMiddle,
    TemplateTail,
    JsxText,

    // Punctuators.
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DotDotDot,
    Semi,
    Comma,
    Lt,
    Gt,
    LtEq,
    GtEq,
    EqEq,
    NotEq,
    EqEqEq,
    NotEqEq,
    Arrow,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    Percent,
    PlusPlus,
    MinusMinus,
    LtLt,
    GtGt,
    GtGtGt,
    Amp,
    Pipe,
    Caret,
    Bang,
    Tilde,
    AmpAmp,
    PipePipe,
    Question,
    QuestionQuestion,
    QuestionDot,
    Colon,
    At,
    Hash,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    StarStarEq,
    SlashEq,
    PercentEq,
    LtLtEq,
    GtGtEq,
    GtGtGtEq,
    AmpEq,
    PipeEq,
    CaretEq,
    AmpAmpEq,
    PipePipeEq,
    QuestionQuestionEq,

    // Reserved words.
    Break,
    Case,
    Catch,
    Class,
    Const,
    Continue,
    Debugger,
    Default,
    Delete,
    Do,
    Else,
    Enum,
    Export,
    Extends,
    False,
    Finally,
    For,
    Function,
    If,
    Import,
    In,
    InstanceOf,
    New,
    Null,
    Return,
    Super,
    Switch,
    This,
    Throw,
    True,
    Try,
    TypeOf,
    Var,
    Void,
    While,
    With,

    // Strict-mode reserved words.
    Implements,
    Interface,
    Let,
    Package,
    Private,
    Protected,
    Public,
    Static,
    Yield,

    // Contextual keywords.
    Abstract,
    Accessor,
    Any,
    As,
    Assert,
    Asserts,
    Async,
    Await,
    BigIntKw,
    Boolean,
    Constructor,
    Declare,
    Defer,
    From,
    Get,
    Global,
    Infer,
    Intrinsic,
    Is,
    KeyOf,
    Module,
    Namespace,
    Never,
    NumberKw,
    ObjectKw,
    Of,
    Out,
    Override,
    Readonly,
    Require,
    Satisfies,
    Set,
    Source,
    StringKw,
    SymbolKw,
    Type,
    Undefined,
    Unique,
    UnknownKw,
    Using,
}

impl TokenKind {
    #[inline]
    pub fn is_keyword(self) -> bool {
        self >= TokenKind::Break
    }

    /// Reserved words that can never be identifiers.
    #[inline]
    pub fn is_reserved(self) -> bool {
        self >= TokenKind::Break && self <= TokenKind::With
    }

    /// Strict-mode reserved words (`let`, `static`, `yield`, ...).
    #[inline]
    pub fn is_strict_reserved(self) -> bool {
        self >= TokenKind::Implements && self <= TokenKind::Yield
    }

    #[inline]
    pub fn is_contextual_keyword(self) -> bool {
        self >= TokenKind::Abstract
    }

    /// An identifier or a keyword that is allowed as an identifier in sloppy code.
    #[inline]
    pub fn is_identifier_like(self) -> bool {
        self == TokenKind::Ident || self >= TokenKind::Implements
    }

    /// Any identifier or keyword (valid as a property name).
    #[inline]
    pub fn is_identifier_or_keyword(self) -> bool {
        self == TokenKind::Ident || self >= TokenKind::Break
    }

    #[inline]
    pub fn is_template_start(self) -> bool {
        matches!(self, TokenKind::NoSubstTemplate | TokenKind::TemplateHead)
    }

    /// Assignment operators (`=`, `+=`, ...).
    #[inline]
    pub fn is_assignment(self) -> bool {
        self >= TokenKind::Eq && self <= TokenKind::QuestionQuestionEq
    }

    /// Literal tokens.
    #[inline]
    pub fn is_literal(self) -> bool {
        matches!(
            self,
            TokenKind::Number
                | TokenKind::BigInt
                | TokenKind::String
                | TokenKind::NoSubstTemplate
                | TokenKind::Regex
        )
    }

    /// Source text of punctuators and keywords, for "'x' expected." messages.
    pub fn as_str(self) -> &'static str {
        use TokenKind::*;
        match self {
            Eof => "end of file",
            Unknown => "unknown",
            Ident => "identifier",
            PrivateName => "private identifier",
            Number => "number",
            BigInt => "bigint",
            String => "string",
            Regex => "regular expression",
            NoSubstTemplate | TemplateHead | TemplateMiddle | TemplateTail => "template",
            JsxText => "JSX text",
            LBrace => "{",
            RBrace => "}",
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            Dot => ".",
            DotDotDot => "...",
            Semi => ";",
            Comma => ",",
            Lt => "<",
            Gt => ">",
            LtEq => "<=",
            GtEq => ">=",
            EqEq => "==",
            NotEq => "!=",
            EqEqEq => "===",
            NotEqEq => "!==",
            Arrow => "=>",
            Plus => "+",
            Minus => "-",
            Star => "*",
            StarStar => "**",
            Slash => "/",
            Percent => "%",
            PlusPlus => "++",
            MinusMinus => "--",
            LtLt => "<<",
            GtGt => ">>",
            GtGtGt => ">>>",
            Amp => "&",
            Pipe => "|",
            Caret => "^",
            Bang => "!",
            Tilde => "~",
            AmpAmp => "&&",
            PipePipe => "||",
            Question => "?",
            QuestionQuestion => "??",
            QuestionDot => "?.",
            Colon => ":",
            At => "@",
            Hash => "#",
            Eq => "=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            StarStarEq => "**=",
            SlashEq => "/=",
            PercentEq => "%=",
            LtLtEq => "<<=",
            GtGtEq => ">>=",
            GtGtGtEq => ">>>=",
            AmpEq => "&=",
            PipeEq => "|=",
            CaretEq => "^=",
            AmpAmpEq => "&&=",
            PipePipeEq => "||=",
            QuestionQuestionEq => "??=",
            Break => "break",
            Case => "case",
            Catch => "catch",
            Class => "class",
            Const => "const",
            Continue => "continue",
            Debugger => "debugger",
            Default => "default",
            Delete => "delete",
            Do => "do",
            Else => "else",
            Enum => "enum",
            Export => "export",
            Extends => "extends",
            False => "false",
            Finally => "finally",
            For => "for",
            Function => "function",
            If => "if",
            Import => "import",
            In => "in",
            InstanceOf => "instanceof",
            New => "new",
            Null => "null",
            Return => "return",
            Super => "super",
            Switch => "switch",
            This => "this",
            Throw => "throw",
            True => "true",
            Try => "try",
            TypeOf => "typeof",
            Var => "var",
            Void => "void",
            While => "while",
            With => "with",
            Implements => "implements",
            Interface => "interface",
            Let => "let",
            Package => "package",
            Private => "private",
            Protected => "protected",
            Public => "public",
            Static => "static",
            Yield => "yield",
            Abstract => "abstract",
            Accessor => "accessor",
            Any => "any",
            As => "as",
            Assert => "assert",
            Asserts => "asserts",
            Async => "async",
            Await => "await",
            BigIntKw => "bigint",
            Boolean => "boolean",
            Constructor => "constructor",
            Declare => "declare",
            Defer => "defer",
            From => "from",
            Get => "get",
            Global => "global",
            Infer => "infer",
            Intrinsic => "intrinsic",
            Is => "is",
            KeyOf => "keyof",
            Module => "module",
            Namespace => "namespace",
            Never => "never",
            NumberKw => "number",
            ObjectKw => "object",
            Of => "of",
            Out => "out",
            Override => "override",
            Readonly => "readonly",
            Require => "require",
            Satisfies => "satisfies",
            Set => "set",
            Source => "source",
            StringKw => "string",
            SymbolKw => "symbol",
            Type => "type",
            Undefined => "undefined",
            Unique => "unique",
            UnknownKw => "unknown",
            Using => "using",
        }
    }
}

/// Maps identifier text to its keyword kind, or `Ident`.
#[inline]
pub fn keyword_kind(s: &[u8]) -> TokenKind {
    use TokenKind::*;
    if s.len() < 2 || s.len() > 11 || !s[0].is_ascii_lowercase() {
        return Ident;
    }
    match s {
        b"as" => As,
        b"do" => Do,
        b"if" => If,
        b"in" => In,
        b"is" => Is,
        b"of" => Of,
        b"any" => Any,
        b"for" => For,
        b"get" => Get,
        b"let" => Let,
        b"new" => New,
        b"out" => Out,
        b"set" => Set,
        b"try" => Try,
        b"var" => Var,
        b"case" => Case,
        b"else" => Else,
        b"enum" => Enum,
        b"from" => From,
        b"null" => Null,
        b"this" => This,
        b"true" => True,
        b"type" => Type,
        b"void" => Void,
        b"with" => With,
        b"async" => Async,
        b"await" => Await,
        b"break" => Break,
        b"catch" => Catch,
        b"class" => Class,
        b"const" => Const,
        b"defer" => Defer,
        b"false" => False,
        b"infer" => Infer,
        b"keyof" => KeyOf,
        b"never" => Never,
        b"super" => Super,
        b"throw" => Throw,
        b"using" => Using,
        b"while" => While,
        b"yield" => Yield,
        b"assert" => Assert,
        b"bigint" => BigIntKw,
        b"delete" => Delete,
        b"export" => Export,
        b"global" => Global,
        b"import" => Import,
        b"module" => Module,
        b"number" => NumberKw,
        b"object" => ObjectKw,
        b"public" => Public,
        b"return" => Return,
        b"source" => Source,
        b"static" => Static,
        b"string" => StringKw,
        b"switch" => Switch,
        b"symbol" => SymbolKw,
        b"typeof" => TypeOf,
        b"unique" => Unique,
        b"asserts" => Asserts,
        b"boolean" => Boolean,
        b"declare" => Declare,
        b"default" => Default,
        b"extends" => Extends,
        b"finally" => Finally,
        b"package" => Package,
        b"private" => Private,
        b"require" => Require,
        b"unknown" => UnknownKw,
        b"abstract" => Abstract,
        b"accessor" => Accessor,
        b"continue" => Continue,
        b"debugger" => Debugger,
        b"function" => Function,
        b"override" => Override,
        b"readonly" => Readonly,
        b"interface" => Interface,
        b"intrinsic" => Intrinsic,
        b"namespace" => Namespace,
        b"protected" => Protected,
        b"satisfies" => Satisfies,
        b"undefined" => Undefined,
        b"implements" => Implements,
        b"instanceof" => InstanceOf,
        b"constructor" => Constructor,
        _ => Ident,
    }
}

/// Token flags.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct TokenFlags(pub u8);

impl TokenFlags {
    /// A line terminator appears between the previous token and this one.
    pub const NEWLINE_BEFORE: u8 = 1 << 0;
    /// The token contains escapes: an identifier with `\u`, a string or template with `\`.
    pub const ESCAPED: u8 = 1 << 1;
    /// String, template, regex or comment not closed.
    pub const UNTERMINATED: u8 = 1 << 2;
    /// Numeric literal: legacy octal (`017`) or leading-zero decimal (`08`).
    pub const LEGACY_OCTAL: u8 = 1 << 3;
    /// Numeric literal contains `_` separators.
    pub const SEPARATOR: u8 = 1 << 4;
    /// Numeric literal is hex, octal or binary (`0x`, `0o`, `0b`).
    pub const NON_DECIMAL: u8 = 1 << 5;
    /// Identifier that is a keyword written with escapes (`var`).
    pub const ESCAPED_KEYWORD: u8 = 1 << 6;

    #[inline]
    pub fn has(self, f: u8) -> bool {
        self.0 & f != 0
    }
}

/// One token: kind, flags and byte range. 12 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub flags: TokenFlags,
    pub start: u32,
    pub end: u32,
}

impl Token {
    #[inline]
    pub fn newline_before(&self) -> bool {
        self.flags.has(TokenFlags::NEWLINE_BEFORE)
    }

    #[inline]
    pub fn span(&self) -> ts_r_span::Span {
        ts_r_span::Span::new(self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_roundtrip() {
        let all = all_keywords();
        assert_eq!(
            all.len(),
            (TokenKind::Using as u8 - TokenKind::Break as u8 + 1) as usize
        );
        for kind in all {
            assert_eq!(keyword_kind(kind.as_str().as_bytes()), kind, "{kind:?}");
            assert!(kind.is_keyword());
        }
        assert_eq!(keyword_kind(b"foo"), TokenKind::Ident);
        assert_eq!(keyword_kind(b"Function"), TokenKind::Ident);
    }

    fn all_keywords() -> Vec<TokenKind> {
        use TokenKind::*;
        vec![
            Break,
            Case,
            Catch,
            Class,
            Const,
            Continue,
            Debugger,
            Default,
            Delete,
            Do,
            Else,
            Enum,
            Export,
            Extends,
            False,
            Finally,
            For,
            Function,
            If,
            Import,
            In,
            InstanceOf,
            New,
            Null,
            Return,
            Super,
            Switch,
            This,
            Throw,
            True,
            Try,
            TypeOf,
            Var,
            Void,
            While,
            With,
            Implements,
            Interface,
            Let,
            Package,
            Private,
            Protected,
            Public,
            Static,
            Yield,
            Abstract,
            Accessor,
            Any,
            As,
            Assert,
            Asserts,
            Async,
            Await,
            BigIntKw,
            Boolean,
            Constructor,
            Declare,
            Defer,
            From,
            Get,
            Global,
            Infer,
            Intrinsic,
            Is,
            KeyOf,
            Module,
            Namespace,
            Never,
            NumberKw,
            ObjectKw,
            Of,
            Out,
            Override,
            Readonly,
            Require,
            Satisfies,
            Set,
            Source,
            StringKw,
            SymbolKw,
            Type,
            Undefined,
            Unique,
            UnknownKw,
            Using,
        ]
    }

    #[test]
    fn classes() {
        assert!(TokenKind::Let.is_identifier_like());
        assert!(TokenKind::Type.is_identifier_like());
        assert!(!TokenKind::Class.is_identifier_like());
        assert!(TokenKind::Class.is_identifier_or_keyword());
        assert!(TokenKind::PlusEq.is_assignment());
        assert!(!TokenKind::Arrow.is_assignment());
    }
}
