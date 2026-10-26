//! JSDoc type expressions as eslint-plugin-jsdoc reads them (through
//! `jsdoc-type-pratt-parser` 8): which strings parse in each mode (`jsdoc`, `closure`,
//! `typescript`), the tree they give, and that tree written back as text.
//!
//! The reader is a Pratt parser driven by a *grammar*: an ordered list of rules, each of
//! which may start an expression (prefix) and/or extend one (infix, gated by binding
//! power). The first rule that accepts the current token wins, so the order of a grammar is
//! part of its meaning. Object keys, name-path segments and method signatures are read with
//! their own grammars, which hand back to the enclosing reader for values. See AGENTS.md,
//! "jsdoc type rules".

use ts_r_lexer::{is_id_part, is_id_start};

/// Which type dialect to read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Jsdoc,
    Closure,
    Typescript,
}

impl Mode {
    /// `settings.jsdoc.mode` (`permissive` has no parser of its own: callers try each mode).
    pub fn from_setting(s: &str) -> Option<Mode> {
        match s {
            "jsdoc" => Some(Mode::Jsdoc),
            "closure" => Some(Mode::Closure),
            "typescript" => Some(Mode::Typescript),
            _ => None,
        }
    }
}

pub type Id = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quote {
    None,
    Single,
    Double,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathKind {
    Property,
    Brackets,
    Inner,
    Instance,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Special {
    Module,
    Event,
    External,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sep {
    Comma,
    Semicolon,
    Linebreak,
    CommaLinebreak,
    SemicolonLinebreak,
}

#[derive(Clone, PartialEq, Debug)]
pub enum SigKind {
    Call,
    Construct,
    Method { name: String, quote: Quote },
}

#[derive(Clone, PartialEq, Debug)]
pub enum FieldKey {
    Text(String),
    Node(Id),
}

/// One node; child ids index [`TypeTree::nodes`].
#[derive(Clone, PartialEq, Debug)]
pub enum N {
    Name(String),
    Number(f64),
    BigInt(String),
    Str {
        value: String,
        quote: Quote,
    },
    Null,
    Undefined,
    Any,
    Unknown,
    UniqueSymbol,
    Nullable {
        el: Id,
        prefix: bool,
    },
    NotNullable {
        el: Id,
        prefix: bool,
    },
    Optional {
        el: Id,
        prefix: bool,
    },
    Parenthesis(Id),
    ParameterList(Vec<Id>),
    KeyValue {
        key: String,
        right: Option<Id>,
        optional: bool,
        variadic: bool,
    },
    Generic {
        left: Id,
        elements: Vec<Id>,
        square: bool,
        dot: bool,
    },
    Union(Vec<Id>),
    Intersection(Vec<Id>),
    Function {
        type_params: Option<Vec<Id>>,
        params: Vec<Id>,
        ret: Option<Id>,
        arrow: bool,
        constructor: bool,
        parenthesis: bool,
    },
    /// `position`: `Some(true)` prefix, `Some(false)` suffix, `None` a bare `...`.
    Variadic {
        el: Option<Id>,
        position: Option<bool>,
        square: bool,
    },
    NamePath {
        left: Id,
        right: Id,
        kind: PathKind,
    },
    Property {
        value: String,
        quote: Quote,
    },
    SpecialNamePath {
        special: Special,
        value: String,
        quote: Quote,
    },
    Symbol {
        value: String,
        el: Option<Id>,
    },
    Object {
        elements: Vec<Id>,
        sep: Sep,
    },
    ObjectField {
        key: FieldKey,
        right: Option<Id>,
        optional: bool,
        readonly: bool,
        quote: Quote,
    },
    JsdocObjectField {
        left: Id,
        right: Id,
    },
    Typeof(Id),
    Keyof(Id),
    Import(Id),
    Tuple(Vec<Id>),
    Asserts {
        left: Id,
        right: Id,
    },
    AssertsPlain(Id),
    Predicate {
        left: Id,
        right: Id,
    },
    Conditional {
        checks: Id,
        extends: Id,
        then: Id,
        other: Id,
    },
    ReadonlyArray(Id),
    ReadonlyProperty(Id),
    Infer(Id),
    TypeParameter {
        name: Id,
        constraint: Option<Id>,
        default: Option<Id>,
    },
    Signature {
        kind: SigKind,
        type_params: Option<Vec<Id>>,
        params: Vec<Id>,
        ret: Id,
    },
    IndexSignature {
        key: String,
        right: Id,
    },
    MappedType {
        key: String,
        right: Id,
    },
    IndexedAccessIndex(Id),
    TemplateLiteral {
        literals: Vec<String>,
        interps: Vec<Id>,
    },
    ComputedProperty(Id),
    ComputedMethod {
        value: Id,
        optional: bool,
        type_params: Option<Vec<Id>>,
        params: Vec<Id>,
        ret: Id,
    },
}

impl N {
    /// The node's `type` in the parser's output.
    pub fn type_name(&self) -> &'static str {
        match self {
            N::Name(_) => "JsdocTypeName",
            N::Number(_) => "JsdocTypeNumber",
            N::BigInt(_) => "JsdocTypeBigInt",
            N::Str { .. } => "JsdocTypeStringValue",
            N::Null => "JsdocTypeNull",
            N::Undefined => "JsdocTypeUndefined",
            N::Any => "JsdocTypeAny",
            N::Unknown => "JsdocTypeUnknown",
            N::UniqueSymbol => "JsdocTypeUniqueSymbol",
            N::Nullable { .. } => "JsdocTypeNullable",
            N::NotNullable { .. } => "JsdocTypeNotNullable",
            N::Optional { .. } => "JsdocTypeOptional",
            N::Parenthesis(_) => "JsdocTypeParenthesis",
            N::ParameterList(_) => "JsdocTypeParameterList",
            N::KeyValue { .. } => "JsdocTypeKeyValue",
            N::Generic { .. } => "JsdocTypeGeneric",
            N::Union(_) => "JsdocTypeUnion",
            N::Intersection(_) => "JsdocTypeIntersection",
            N::Function { .. } => "JsdocTypeFunction",
            N::Variadic { .. } => "JsdocTypeVariadic",
            N::NamePath { .. } => "JsdocTypeNamePath",
            N::Property { .. } => "JsdocTypeProperty",
            N::SpecialNamePath { .. } => "JsdocTypeSpecialNamePath",
            N::Symbol { .. } => "JsdocTypeSymbol",
            N::Object { .. } => "JsdocTypeObject",
            N::ObjectField { .. } => "JsdocTypeObjectField",
            N::JsdocObjectField { .. } => "JsdocTypeJsdocObjectField",
            N::Typeof(_) => "JsdocTypeTypeof",
            N::Keyof(_) => "JsdocTypeKeyof",
            N::Import(_) => "JsdocTypeImport",
            N::Tuple(_) => "JsdocTypeTuple",
            N::Asserts { .. } => "JsdocTypeAsserts",
            N::AssertsPlain(_) => "JsdocTypeAssertsPlain",
            N::Predicate { .. } => "JsdocTypePredicate",
            N::Conditional { .. } => "JsdocTypeConditional",
            N::ReadonlyArray(_) => "JsdocTypeReadonlyArray",
            N::ReadonlyProperty(_) => "JsdocTypeReadonlyProperty",
            N::Infer(_) => "JsdocTypeInfer",
            N::TypeParameter { .. } => "JsdocTypeTypeParameter",
            N::Signature { kind, .. } => match kind {
                SigKind::Call => "JsdocTypeCallSignature",
                SigKind::Construct => "JsdocTypeConstructorSignature",
                SigKind::Method { .. } => "JsdocTypeMethodSignature",
            },
            N::IndexSignature { .. } => "JsdocTypeIndexSignature",
            N::MappedType { .. } => "JsdocTypeMappedType",
            N::IndexedAccessIndex(_) => "JsdocTypeIndexedAccessIndex",
            N::TemplateLiteral { .. } => "JsdocTypeTemplateLiteral",
            N::ComputedProperty(_) => "JsdocTypeComputedProperty",
            N::ComputedMethod { .. } => "JsdocTypeComputedMethod",
        }
    }

    /// Nodes that only exist inside other nodes (never a whole type).
    fn is_intermediate(&self) -> bool {
        matches!(
            self,
            N::KeyValue { .. }
                | N::ParameterList(_)
                | N::Property { .. }
                | N::ReadonlyProperty(_)
                | N::ObjectField { .. }
                | N::JsdocObjectField { .. }
                | N::IndexSignature { .. }
                | N::MappedType { .. }
                | N::TypeParameter { .. }
                | N::Signature { .. }
                | N::IndexedAccessIndex(_)
                | N::ComputedProperty(_)
                | N::ComputedMethod { .. }
        )
    }
}

/// A parsed type.
#[derive(Clone, Debug)]
pub struct TypeTree {
    pub nodes: Vec<N>,
    pub root: Id,
}

// ---------------------------------------------------------------------------------------------
// Tokens.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tk {
    Arrow,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Pipe,
    Amp,
    Lt,
    Gt,
    Comma,
    Semi,
    Star,
    Question,
    Bang,
    Eq,
    Colon,
    Ellipsis,
    Dot,
    Hash,
    Tilde,
    Slash,
    At,
    Undefined,
    Null,
    Function,
    This,
    New,
    Module,
    Event,
    Extends,
    External,
    Infer,
    Typeof,
    Keyof,
    Readonly,
    Import,
    Is,
    In,
    Asserts,
    Ident,
    Str,
    Template,
    Num,
    Eof,
}

const PUNCT: &[(&str, Tk)] = &[
    ("=>", Tk::Arrow),
    ("(", Tk::LParen),
    (")", Tk::RParen),
    ("{", Tk::LBrace),
    ("}", Tk::RBrace),
    ("[", Tk::LBracket),
    ("]", Tk::RBracket),
    ("|", Tk::Pipe),
    ("&", Tk::Amp),
    ("<", Tk::Lt),
    (">", Tk::Gt),
    (",", Tk::Comma),
    (";", Tk::Semi),
    ("*", Tk::Star),
    ("?", Tk::Question),
    ("!", Tk::Bang),
    ("=", Tk::Eq),
    (":", Tk::Colon),
    ("...", Tk::Ellipsis),
    (".", Tk::Dot),
    ("#", Tk::Hash),
    ("~", Tk::Tilde),
    ("/", Tk::Slash),
    ("@", Tk::At),
];

const KEYWORDS: &[(&str, Tk)] = &[
    ("undefined", Tk::Undefined),
    ("null", Tk::Null),
    ("function", Tk::Function),
    ("this", Tk::This),
    ("new", Tk::New),
    ("module", Tk::Module),
    ("event", Tk::Event),
    ("extends", Tk::Extends),
    ("external", Tk::External),
    ("infer", Tk::Infer),
    ("typeof", Tk::Typeof),
    ("keyof", Tk::Keyof),
    ("readonly", Tk::Readonly),
    ("import", Tk::Import),
    ("is", Tk::Is),
    ("in", Tk::In),
    ("asserts", Tk::Asserts),
];

/// Keywords that also read as plain names in name grammars.
const NAME_TOKENS: &[Tk] = &[
    Tk::Module,
    Tk::Keyof,
    Tk::Event,
    Tk::External,
    Tk::Readonly,
    Tk::Is,
    Tk::Typeof,
    Tk::In,
    Tk::Null,
    Tk::Undefined,
    Tk::Function,
    Tk::Asserts,
    Tk::Infer,
    Tk::Extends,
    Tk::Import,
];

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Tk,
    start: u32,
    end: u32,
    start_of_line: bool,
}

/// JavaScript's `\s` (WhiteSpace and LineTerminator).
fn js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Identifier characters are tested one UTF-16 unit at a time upstream, so characters
/// outside the BMP never count.
fn id_start(c: char) -> bool {
    (c as u32) <= 0xFFFF && is_id_start(c)
}

fn id_part(c: char) -> bool {
    (c as u32) <= 0xFFFF && (is_id_part(c) || c == '\u{200C}' || c == '\u{200D}')
}

fn number_len(s: &str, loose: bool) -> Option<usize> {
    let b = s.as_bytes();
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    // `-?\d+n` (a bigint) first.
    let neg = usize::from(b.first() == Some(&b'-'));
    let d = digits(neg);
    if d > 0 && b.get(neg + d) == Some(&b'n') {
        return Some(neg + d + 1);
    }
    if loose && s.starts_with("NaN") {
        return Some(3);
    }
    let mut i = neg;
    if b.get(i + d) == Some(&b'.') && digits(i + d + 1) > 0 {
        i += d + 1 + digits(i + d + 1);
    } else if d > 0 {
        i += d;
    } else {
        if loose && s[neg..].starts_with("Infinity") {
            return Some(neg + 8);
        }
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let e = digits(j);
        if e > 0 {
            i = j + e;
        }
    }
    Some(i)
}

fn ident_len(s: &str, loose: bool) -> Option<usize> {
    let mut it = s.char_indices();
    let (_, c) = it.next()?;
    if !id_start(c) {
        return None;
    }
    for (i, c) in it {
        if !(id_part(c) || (loose && c == '-')) {
            return Some(i);
        }
    }
    Some(s.len())
}

/// A quoted string or template from its opening mark; `Err` when unterminated.
fn quoted_len(s: &str, mark: char) -> Result<usize, ()> {
    let mut escaped = false;
    let mut last = None;
    for (i, c) in s.char_indices().skip(1) {
        last = Some(c);
        if !escaped && c == mark {
            return Ok(i + c.len_utf8());
        }
        escaped = !escaped && c == '\\';
    }
    // Upstream checks the last character it looked at, so a string that ends in an escaped
    // mark still counts as closed.
    if last == Some(mark) {
        Ok(s.len())
    } else {
        Err(())
    }
}

/// Splits `text` into tokens; the flag says lexing stopped at an unreadable token.
fn tokenize(text: &str, loose: bool) -> (Vec<Token>, bool) {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        let rest = &text[pos..];
        let trimmed = rest.trim_start_matches(js_ws);
        let ws = &rest[..rest.len() - trimmed.len()];
        let start_of_line = ws.contains('\n');
        let start = pos + ws.len();
        let tok = |kind, len: usize| Token {
            kind,
            start: start as u32,
            end: (start + len) as u32,
            start_of_line,
        };
        if trimmed.is_empty() {
            out.push(tok(Tk::Eof, 0));
            return (out, false);
        }
        let found = PUNCT
            .iter()
            .find(|(p, _)| trimmed.starts_with(p))
            .map(|&(p, k)| (k, p.len()))
            .or_else(|| {
                KEYWORDS.iter().find_map(|&(w, k)| {
                    let after = trimmed.strip_prefix(w)?;
                    match after.chars().next() {
                        Some(c) if id_part(c) => None,
                        _ => Some((k, w.len())),
                    }
                })
            })
            .or_else(|| number_len(trimmed, loose).map(|n| (Tk::Num, n)))
            .or_else(|| ident_len(trimmed, loose).map(|n| (Tk::Ident, n)));
        let found = match found {
            Some(f) => Some(f),
            None => match trimmed.chars().next() {
                Some(m @ ('\'' | '"')) => match quoted_len(trimmed, m) {
                    Ok(n) => Some((Tk::Str, n)),
                    Err(()) => None,
                },
                Some('`') => match quoted_len(trimmed, '`') {
                    Ok(n) => Some((Tk::Template, n)),
                    Err(()) => None,
                },
                _ => None,
            },
        };
        let Some((kind, len)) = found else {
            return (out, true);
        };
        out.push(tok(kind, len));
        pos = start + len;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fail {
    /// No rule could start an expression at the current token (some callers recover).
    NoRule,
    Other,
}

type R<T> = Result<T, Fail>;

struct Lexer<'t> {
    text: &'t str,
    loose: bool,
    tokens: Vec<Token>,
    failed: bool,
    pos: usize,
}

impl<'t> Lexer<'t> {
    fn new(text: &'t str, loose: bool) -> R<Lexer<'t>> {
        let (tokens, failed) = tokenize(text, loose);
        let lx = Lexer {
            text,
            loose,
            tokens,
            failed,
            pos: 0,
        };
        // The current and next tokens are read up front.
        lx.at(0)?;
        lx.at(1)?;
        Ok(lx)
    }

    fn at(&self, i: usize) -> R<Token> {
        match self.tokens.get(i) {
            Some(t) => Ok(*t),
            None if self.failed => Err(Fail::Other),
            None => Ok(*self.tokens.last().expect("an EOF token")),
        }
    }

    fn cur(&self) -> Token {
        self.at(self.pos).expect("current token was read")
    }

    fn peek(&self) -> Token {
        self.at(self.pos + 1).expect("next token was read")
    }

    fn text(&self, t: Token) -> &'t str {
        &self.text[t.start as usize..t.end as usize]
    }

    fn is(&self, k: Tk) -> bool {
        self.cur().kind == k
    }

    /// Advances past the current token when it is `k` (reading one more token ahead).
    fn eat(&mut self, k: Tk) -> R<bool> {
        if self.cur().kind != k {
            return Ok(false);
        }
        self.at(self.pos + 2)?;
        self.pos += 1;
        Ok(true)
    }

    /// The text from the next token on.
    fn remaining(&self) -> &'t str {
        &self.text[self.peek().start as usize..]
    }
}

// ---------------------------------------------------------------------------------------------
// Grammars.

// Binding powers, lowest first.
const ALL: u8 = 0;
const PARAMETER_LIST: u8 = 1;
const OBJECT: u8 = 2;
const KEY_VALUE: u8 = 3;
const INDEX_BRACKETS: u8 = 4;
const UNION: u8 = 5;
const INTERSECTION: u8 = 6;
const PREFIX: u8 = 7;
const INFIX: u8 = 8;
const SYMBOL: u8 = 10;
const OPTIONAL: u8 = 11;
const NULLABLE: u8 = 12;
const KEY_OF_TYPE_OF: u8 = 13;
const FUNCTION: u8 = 14;
const ARROW: u8 = 15;
const ARRAY_BRACKETS: u8 = 16;
const GENERIC: u8 = 17;
const NAME_PATH: u8 = 18;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum G {
    BasePath,
    Path,
    Closure,
    ClosureField,
    Jsdoc,
    JsdocField,
    Ts,
    TsField,
    Name,
    JsdocNamePath,
    TsNamePath,
    ClosureNamePathSpecial,
    JsdocNamePathSpecial,
    TsNamePathSpecial,
}

#[derive(Clone, Copy, Debug)]
enum P {
    Nullable,
    Optional,
    Number,
    Parenthesis,
    SpecialTypes,
    NotNullable,
    ParamList,
    Generic,
    Union,
    Object {
        field: G,
        signatures: bool,
    },
    Name(&'static [Tk]),
    TypeOf,
    Function {
        without_paren: bool,
        named: &'static [&'static str],
        new_kw: bool,
    },
    Variadic {
        brackets: bool,
        postfix: bool,
    },
    SpecialNamePath(&'static [Tk], G),
    NamePath {
        square_any: bool,
        jsdoc_paths: bool,
        path: Option<G>,
    },
    KeyValue {
        optional: bool,
        variadic: bool,
        accept_list: bool,
    },
    Symbol,
    ArrayBrackets,
    StringValue,
    ObjectField {
        squared: bool,
        key_types: bool,
        optional: bool,
        readonly: bool,
    },
    ReadonlyArray,
    KeyOf,
    Import,
    Tuple,
    Asserts,
    Conditional,
    UniqueSymbol,
    Arrow,
    GenericArrow,
    Intersection,
    Predicate,
    Template,
    FunctionProperty,
    ReadonlyProperty,
    ObjectSquared,
}

macro_rules! with_base {
    ($($x:expr),* $(,)?) => {
        &[
            P::Nullable,
            P::Optional,
            P::Number,
            P::Parenthesis,
            P::SpecialTypes,
            P::NotNullable,
            P::ParamList,
            P::Generic,
            P::Union,
            P::Optional,
            $($x),*
        ]
    };
}

macro_rules! with_jsdoc_base {
    ($($x:expr),* $(,)?) => {
        with_base![
            P::Function { without_paren: true, named: &["this", "new"], new_kw: false },
            P::StringValue,
            P::SpecialNamePath(&[Tk::Module, Tk::External, Tk::Event], G::Path),
            P::Variadic { brackets: true, postfix: true },
            P::Name(&[Tk::Keyof]),
            P::Symbol,
            P::ArrayBrackets,
            P::NamePath { square_any: false, jsdoc_paths: true, path: Some(G::Path) },
            $($x),*
        ]
    };
}

const JSDOC_PATHS: P = P::NamePath {
    square_any: false,
    jsdoc_paths: true,
    path: Some(G::Path),
};

static BASE_PATH: &[P] = &[
    P::Name(&[Tk::External, Tk::Module]),
    P::StringValue,
    P::Number,
    P::NamePath {
        square_any: false,
        jsdoc_paths: true,
        path: None,
    },
];

static PATH: &[P] = &[
    P::Name(&[Tk::External, Tk::Module]),
    P::StringValue,
    P::Number,
    P::NamePath {
        square_any: false,
        jsdoc_paths: true,
        path: None,
    },
    P::SpecialNamePath(&[Tk::Event], G::BasePath),
    P::Name(NAME_TOKENS),
];

static CLOSURE_FIELD: &[P] = &[
    P::Name(&[
        Tk::Typeof,
        Tk::Module,
        Tk::Keyof,
        Tk::Event,
        Tk::External,
        Tk::In,
    ]),
    P::Nullable,
    P::Optional,
    P::StringValue,
    P::Number,
    P::ObjectField {
        squared: false,
        key_types: false,
        optional: false,
        readonly: false,
    },
];

static CLOSURE: &[P] = with_base![
    P::Object {
        field: G::ClosureField,
        signatures: false
    },
    P::Name(&[Tk::Event, Tk::External, Tk::In]),
    P::TypeOf,
    P::Function {
        without_paren: false,
        named: &["this", "new"],
        new_kw: false
    },
    P::Variadic {
        brackets: false,
        postfix: false
    },
    P::Name(&[Tk::Keyof]),
    P::SpecialNamePath(&[Tk::Module], G::Path),
    JSDOC_PATHS,
    P::KeyValue {
        optional: false,
        variadic: false,
        accept_list: false
    },
    P::Symbol,
];

static JSDOC: &[P] = with_jsdoc_base![
    P::Object {
        field: G::JsdocField,
        signatures: false
    },
    P::KeyValue {
        optional: true,
        variadic: true,
        accept_list: false
    },
];

static JSDOC_FIELD: &[P] = &[
    P::Name(&[Tk::Typeof, Tk::Module, Tk::In]),
    P::ObjectField {
        squared: false,
        key_types: true,
        optional: false,
        readonly: false,
    },
    P::Nullable,
    P::Optional,
    P::Number,
    P::Parenthesis,
    P::SpecialTypes,
    P::NotNullable,
    P::ParamList,
    P::Generic,
    P::Union,
    P::Optional,
    P::Function {
        without_paren: true,
        named: &["this", "new"],
        new_kw: false,
    },
    P::StringValue,
    P::SpecialNamePath(&[Tk::Module, Tk::External, Tk::Event], G::Path),
    P::Variadic {
        brackets: true,
        postfix: true,
    },
    P::Name(&[Tk::Keyof]),
    P::Symbol,
    P::ArrayBrackets,
    JSDOC_PATHS,
];

static TS_FIELD: &[P] = &[
    P::FunctionProperty,
    P::ReadonlyProperty,
    P::Name(NAME_TOKENS),
    P::Nullable,
    P::Optional,
    P::StringValue,
    P::Number,
    P::ObjectField {
        squared: true,
        key_types: false,
        optional: true,
        readonly: true,
    },
    P::ObjectSquared,
];

static TS: &[P] = with_base![
    P::Object {
        field: G::TsField,
        signatures: true
    },
    P::ReadonlyArray,
    P::TypeOf,
    P::KeyOf,
    P::Import,
    P::StringValue,
    P::Function {
        without_paren: true,
        named: &["this", "new", "args"],
        new_kw: true
    },
    P::Tuple,
    P::Variadic {
        brackets: false,
        postfix: false
    },
    P::Asserts,
    P::Conditional,
    P::UniqueSymbol,
    P::Name(&[Tk::Event, Tk::External, Tk::In]),
    P::SpecialNamePath(&[Tk::Module], G::Path),
    P::ArrayBrackets,
    P::Arrow,
    P::GenericArrow,
    P::NamePath {
        square_any: true,
        jsdoc_paths: false,
        path: Some(G::Path)
    },
    P::Intersection,
    P::Predicate,
    P::Template,
    P::KeyValue {
        optional: true,
        variadic: true,
        accept_list: false
    },
];

static NAME: &[P] = &[P::Generic, P::ArrayBrackets, P::Name(NAME_TOKENS)];

static JSDOC_NAME_PATH: &[P] = &[
    P::Generic,
    P::ArrayBrackets,
    P::Name(NAME_TOKENS),
    JSDOC_PATHS,
];

static TS_NAME_PATH: &[P] = &[
    P::Generic,
    P::ArrayBrackets,
    P::Name(NAME_TOKENS),
    P::NamePath {
        square_any: true,
        jsdoc_paths: true,
        path: Some(G::Path),
    },
];

static CLOSURE_NAME_PATH_SPECIAL: &[P] = &[
    P::SpecialNamePath(&[Tk::Module], G::Path),
    P::Generic,
    P::ArrayBrackets,
    P::Name(NAME_TOKENS),
    JSDOC_PATHS,
];

static JSDOC_NAME_PATH_SPECIAL: &[P] = &[
    P::SpecialNamePath(&[Tk::Module, Tk::External, Tk::Event], G::Path),
    P::Generic,
    P::ArrayBrackets,
    P::Name(NAME_TOKENS),
    JSDOC_PATHS,
];

static TS_NAME_PATH_SPECIAL: &[P] = &[
    P::SpecialNamePath(&[Tk::Module], G::Path),
    P::Generic,
    P::ArrayBrackets,
    P::Name(NAME_TOKENS),
    P::NamePath {
        square_any: true,
        jsdoc_paths: true,
        path: Some(G::Path),
    },
];

fn rules(g: G) -> &'static [P] {
    match g {
        G::BasePath => BASE_PATH,
        G::Path => PATH,
        G::Closure => CLOSURE,
        G::ClosureField => CLOSURE_FIELD,
        G::Jsdoc => JSDOC,
        G::JsdocField => JSDOC_FIELD,
        G::Ts => TS,
        G::TsField => TS_FIELD,
        G::Name => NAME,
        G::JsdocNamePath => JSDOC_NAME_PATH,
        G::TsNamePath => TS_NAME_PATH,
        G::ClosureNamePathSpecial => CLOSURE_NAME_PATH_SPECIAL,
        G::JsdocNamePathSpecial => JSDOC_NAME_PATH_SPECIAL,
        G::TsNamePathSpecial => TS_NAME_PATH_SPECIAL,
    }
}

/// A grammar as a reader uses it: `signature` is the method-signature variant (its own
/// key-value rule, which also accepts a parameter list before `:`, replaces the grammar's).
#[derive(Clone, Copy, Debug)]
struct Gram {
    id: G,
    signature: bool,
}

const SIGNATURE_KV: P = P::KeyValue {
    optional: true,
    variadic: true,
    accept_list: true,
};

/// One reader: a grammar, the reader that started it (object keys and signatures read
/// their values with it) and the reserved-word checks of the top-level call.
struct Cx<'p> {
    gram: Gram,
    base: Option<&'p Cx<'p>>,
    /// `module`/`strictMode` were given (only to the top-level reader).
    strict: bool,
    /// `classContext: true`: `this` is a valid type name.
    class_context: bool,
}

const MAX_DEPTH: u32 = 64;

struct Arena {
    nodes: Vec<N>,
    depth: u32,
}

impl Arena {
    fn add(&mut self, n: N) -> Id {
        self.nodes.push(n);
        self.nodes.len() - 1
    }
}

fn unknown_follows(k: Tk) -> bool {
    matches!(
        k,
        Tk::RBrace | Tk::Eof | Tk::Pipe | Tk::Comma | Tk::RParen | Tk::Gt
    )
}

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
];
const RESERVED_AS_ROOT_TYPES: &[&str] = &["false", "null", "true", "void"];
const STRICT_RESERVED: &[&str] = &[
    "let",
    "static",
    "yield",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    "arguments",
    "eval",
    "await",
];

impl<'p> Cx<'p> {
    fn sub(&'p self, gram: Gram) -> Cx<'p> {
        Cx {
            gram,
            base: Some(self),
            strict: false,
            class_context: false,
        }
    }

    fn not_reserved(&self, ar: &Arena, id: Id) -> R<Id> {
        let mut n = id;
        while let N::Parenthesis(el) = ar.nodes[n] {
            n = el;
        }
        let N::Name(text) = &ar.nodes[n] else {
            return Ok(id);
        };
        let t = text.as_str();
        if RESERVED.contains(&t)
            && !RESERVED_AS_ROOT_TYPES.contains(&t)
            && (t != "this" || !self.class_context)
        {
            return Err(Fail::Other);
        }
        if t == "enum" || (self.strict && STRICT_RESERVED.contains(&t)) {
            return Err(Fail::Other);
        }
        Ok(id)
    }

    fn parse_intermediate(&self, lx: &mut Lexer, ar: &mut Arena, prec: u8) -> R<Id> {
        if ar.depth >= MAX_DEPTH {
            return Err(Fail::Other);
        }
        ar.depth += 1;
        let r = self.parse_intermediate_inner(lx, ar, prec);
        ar.depth -= 1;
        r
    }

    fn parse_intermediate_inner(&self, lx: &mut Lexer, ar: &mut Arena, prec: u8) -> R<Id> {
        let left = self.try_rules(lx, ar, None, prec)?.ok_or(Fail::NoRule)?;
        self.parse_infix(lx, ar, left, prec)
    }

    fn parse_infix(&self, lx: &mut Lexer, ar: &mut Arena, mut left: Id, prec: u8) -> R<Id> {
        while let Some(r) = self.try_rules(lx, ar, Some(left), prec)? {
            left = r;
        }
        Ok(left)
    }

    fn parse_type(&self, lx: &mut Lexer, ar: &mut Arena, prec: u8) -> R<Id> {
        let r = self.parse_intermediate(lx, ar, prec)?;
        root(ar, r)
    }

    fn try_rules(
        &self,
        lx: &mut Lexer,
        ar: &mut Arena,
        left: Option<Id>,
        prec: u8,
    ) -> R<Option<Id>> {
        let list = rules(self.gram.id);
        if self.gram.signature
            && let Some(r) = self.apply(SIGNATURE_KV, lx, ar, left, prec)?
        {
            return Ok(Some(r));
        }
        for &p in list {
            if self.gram.signature && matches!(p, P::KeyValue { .. }) {
                continue;
            }
            if let Some(r) = self.apply(p, lx, ar, left, prec)? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Runs one rule: `Ok(None)` when it doesn't apply here.
    fn apply(
        &self,
        p: P,
        lx: &mut Lexer,
        ar: &mut Arena,
        left: Option<Id>,
        prec: u8,
    ) -> R<Option<Id>> {
        let cur = lx.cur().kind;
        let next = lx.peek().kind;
        // Infix rules: the token they react to and their binding power.
        let infix = |k: Tk, power: u8| left.is_some() && cur == k && power > prec;
        let n = match p {
            P::Nullable => match left {
                None if cur == Tk::Question && !unknown_follows(next) => {
                    lx.eat(Tk::Question)?;
                    let el = self.parse_type(lx, ar, NULLABLE)?;
                    N::Nullable { el, prefix: true }
                }
                Some(l) if cur == Tk::Question && NULLABLE > prec => {
                    lx.eat(Tk::Question)?;
                    N::Nullable {
                        el: root(ar, l)?,
                        prefix: false,
                    }
                }
                _ => return Ok(None),
            },
            P::Optional | P::NotNullable => {
                let (k, power) = match p {
                    P::Optional => (Tk::Eq, OPTIONAL),
                    _ => (Tk::Bang, NULLABLE),
                };
                let (el, prefix) = match left {
                    None if cur == k => {
                        lx.eat(k)?;
                        (self.parse_type(lx, ar, power)?, true)
                    }
                    Some(l) if infix(k, power) => {
                        lx.eat(k)?;
                        (root(ar, l)?, false)
                    }
                    _ => return Ok(None),
                };
                match p {
                    P::Optional => N::Optional { el, prefix },
                    _ => N::NotNullable { el, prefix },
                }
            }
            P::Number => {
                if left.is_some() || cur != Tk::Num {
                    return Ok(None);
                }
                let text = lx.text(lx.cur());
                lx.eat(Tk::Num)?;
                match text.strip_suffix('n') {
                    Some(digits) => N::BigInt(bigint_text(digits)),
                    None => N::Number(parse_float(text)),
                }
            }
            P::Parenthesis => {
                if left.is_some() || cur != Tk::LParen {
                    return Ok(None);
                }
                lx.eat(Tk::LParen)?;
                if lx.eat(Tk::RParen)? {
                    N::ParameterList(Vec::new())
                } else {
                    let r = self.parse_intermediate(lx, ar, ALL)?;
                    if !lx.eat(Tk::RParen)? {
                        return Err(Fail::Other);
                    }
                    match ar.nodes[r] {
                        N::ParameterList(_) => return Ok(Some(r)),
                        N::KeyValue { .. } => N::ParameterList(vec![r]),
                        _ => N::Parenthesis(root(ar, r)?),
                    }
                }
            }
            P::SpecialTypes => {
                if left.is_some() {
                    return Ok(None);
                }
                let n = match cur {
                    Tk::Question if unknown_follows(next) => N::Unknown,
                    Tk::Null => N::Null,
                    Tk::Undefined => N::Undefined,
                    Tk::Star => N::Any,
                    _ => return Ok(None),
                };
                lx.eat(cur)?;
                n
            }
            P::ParamList => {
                let Some(l) = left else { return Ok(None) };
                if !infix(Tk::Comma, PARAMETER_LIST) {
                    return Ok(None);
                }
                let mut elements = vec![kv_or_root(ar, l)?];
                lx.eat(Tk::Comma)?;
                loop {
                    match self.parse_intermediate(lx, ar, PARAMETER_LIST) {
                        Ok(n) => elements.push(kv_or_root(ar, n)?),
                        Err(Fail::NoRule) => break,
                        Err(e) => return Err(e),
                    }
                    if !lx.eat(Tk::Comma)? {
                        break;
                    }
                }
                if elements[..elements.len() - 1]
                    .iter()
                    .any(|&e| matches!(ar.nodes[e], N::Variadic { .. }))
                {
                    return Err(Fail::Other);
                }
                N::ParameterList(elements)
            }
            P::Generic => {
                let Some(l) = left else { return Ok(None) };
                if !((cur == Tk::Lt || (cur == Tk::Dot && next == Tk::Lt)) && GENERIC > prec) {
                    return Ok(None);
                }
                let dot = lx.eat(Tk::Dot)?;
                lx.eat(Tk::Lt)?;
                let mut elements = Vec::new();
                loop {
                    if lx.eat(Tk::Infer)? {
                        let name = self.parse_intermediate(lx, ar, SYMBOL)?;
                        if !matches!(ar.nodes[name], N::Name(_)) {
                            return Err(Fail::Other);
                        }
                        elements.push(ar.add(N::Infer(name)));
                    } else {
                        elements.push(self.parse_type(lx, ar, PARAMETER_LIST)?);
                    }
                    if !lx.eat(Tk::Comma)? {
                        break;
                    }
                }
                if !lx.eat(Tk::Gt)? {
                    return Err(Fail::Other);
                }
                N::Generic {
                    left: root(ar, l)?,
                    elements,
                    square: false,
                    dot,
                }
            }
            P::Union | P::Intersection => {
                let (k, power) = match p {
                    P::Union => (Tk::Pipe, UNION),
                    _ => (Tk::Amp, INTERSECTION),
                };
                let Some(l) = left else { return Ok(None) };
                if !infix(k, power) {
                    return Ok(None);
                }
                lx.eat(k)?;
                let mut rest = Vec::new();
                loop {
                    rest.push(self.parse_type(lx, ar, power)?);
                    if !lx.eat(k)? {
                        break;
                    }
                }
                let first = root(ar, l)?;
                let mut elements = vec![self.not_reserved(ar, first)?];
                for e in rest {
                    elements.push(self.not_reserved(ar, e)?);
                }
                match p {
                    P::Union => N::Union(elements),
                    _ => N::Intersection(elements),
                }
            }
            P::Object { field, signatures } => {
                if left.is_some() || cur != Tk::LBrace {
                    return Ok(None);
                }
                return self.object(lx, ar, field, signatures).map(Some);
            }
            P::Name(extra) => {
                if left.is_some()
                    || !(matches!(cur, Tk::Ident | Tk::This | Tk::New) || extra.contains(&cur))
                {
                    return Ok(None);
                }
                let text = lx.text(lx.cur()).to_owned();
                lx.eat(cur)?;
                N::Name(text)
            }
            P::TypeOf | P::KeyOf => {
                let k = if matches!(p, P::TypeOf) {
                    Tk::Typeof
                } else {
                    Tk::Keyof
                };
                if left.is_some() || cur != k {
                    return Ok(None);
                }
                lx.eat(k)?;
                let el = self.parse_type(lx, ar, KEY_OF_TYPE_OF)?;
                if k == Tk::Typeof {
                    N::Typeof(el)
                } else {
                    N::Keyof(el)
                }
            }
            P::Function {
                without_paren,
                named,
                new_kw,
            } => {
                if left.is_some()
                    || !(cur == Tk::Function || (new_kw && cur == Tk::New && next == Tk::LParen))
                {
                    return Ok(None);
                }
                let constructor = lx.eat(Tk::New)?;
                lx.eat(Tk::Function)?;
                if !lx.is(Tk::LParen) {
                    if !without_paren {
                        return Err(Fail::Other);
                    }
                    N::Name("function".to_owned())
                } else {
                    let value = self.parse_intermediate(lx, ar, FUNCTION)?;
                    if constructor && let N::Function { arrow: true, .. } = ar.nodes[value] {
                        if let N::Function { constructor, .. } = &mut ar.nodes[value] {
                            *constructor = true;
                        }
                        return Ok(Some(value));
                    }
                    let params = parameters(ar, value)?;
                    for &q in &params {
                        if let N::KeyValue { key, .. } = &ar.nodes[q]
                            && !named.contains(&key.as_str())
                        {
                            return Err(Fail::Other);
                        }
                    }
                    let ret = if lx.eat(Tk::Colon)? {
                        Some(self.parse_type(lx, ar, PREFIX)?)
                    } else {
                        None
                    };
                    N::Function {
                        type_params: None,
                        params,
                        ret,
                        arrow: false,
                        constructor,
                        parenthesis: true,
                    }
                }
            }
            P::Variadic { brackets, postfix } => match left {
                None if cur == Tk::Ellipsis => {
                    lx.eat(Tk::Ellipsis)?;
                    let square = brackets && lx.eat(Tk::LBracket)?;
                    match self.parse_type(lx, ar, PREFIX) {
                        Ok(el) => {
                            if square && !lx.eat(Tk::RBracket)? {
                                return Err(Fail::Other);
                            }
                            N::Variadic {
                                el: Some(el),
                                position: Some(true),
                                square,
                            }
                        }
                        Err(Fail::NoRule) if !square => N::Variadic {
                            el: None,
                            position: None,
                            square: false,
                        },
                        Err(_) => return Err(Fail::Other),
                    }
                }
                Some(l) if postfix && infix(Tk::Ellipsis, PREFIX) => {
                    lx.eat(Tk::Ellipsis)?;
                    N::Variadic {
                        el: Some(root(ar, l)?),
                        position: Some(false),
                        square: false,
                    }
                }
                _ => return Ok(None),
            },
            P::SpecialNamePath(allowed, path) => {
                if left.is_some() || !allowed.contains(&cur) {
                    return Ok(None);
                }
                let special = match cur {
                    Tk::Module => Special::Module,
                    Tk::Event => Special::Event,
                    _ => Special::External,
                };
                let word = lx.text(lx.cur()).to_owned();
                lx.eat(cur)?;
                if !lx.eat(Tk::Colon)? {
                    N::Name(word)
                } else {
                    let t = lx.cur();
                    let node = if lx.eat(Tk::Str)? {
                        let text = lx.text(t);
                        N::SpecialNamePath {
                            special,
                            value: text[1..text.len() - 1].to_owned(),
                            quote: quote_of(text),
                        }
                    } else {
                        let mut value = String::new();
                        loop {
                            let t = lx.cur();
                            if !matches!(t.kind, Tk::Ident | Tk::At | Tk::Slash) {
                                break;
                            }
                            value.push_str(lx.text(t));
                            lx.eat(t.kind)?;
                        }
                        N::SpecialNamePath {
                            special,
                            value,
                            quote: Quote::None,
                        }
                    };
                    let start = ar.add(node);
                    let sub = self.sub(Gram {
                        id: path,
                        signature: false,
                    });
                    let r = sub.parse_infix(lx, ar, start, ALL)?;
                    return root(ar, r).map(Some);
                }
            }
            P::NamePath {
                square_any,
                jsdoc_paths,
                path,
            } => {
                let Some(l) = left else { return Ok(None) };
                if prec >= NAME_PATH {
                    return Ok(None);
                }
                let accept = (cur == Tk::Dot && next != Tk::Lt)
                    || (cur == Tk::LBracket && (square_any || matches!(ar.nodes[l], N::Name(_))))
                    || (jsdoc_paths && matches!(cur, Tk::Tilde | Tk::Hash));
                if !accept {
                    return Ok(None);
                }
                let kind = match cur {
                    Tk::Dot => PathKind::Property,
                    Tk::LBracket => PathKind::Brackets,
                    Tk::Tilde => PathKind::Inner,
                    _ => PathKind::Instance,
                };
                lx.eat(cur)?;
                let brackets = kind == PathKind::Brackets;
                let parsed = match path {
                    Some(g) if !(brackets && square_any) => self
                        .sub(Gram {
                            id: g,
                            signature: false,
                        })
                        .parse_type(lx, ar, NAME_PATH)?,
                    _ => self.parse_type(lx, ar, NAME_PATH)?,
                };
                let right = match &ar.nodes[parsed] {
                    N::Name(v) => ar.add(N::Property {
                        value: v.clone(),
                        quote: Quote::None,
                    }),
                    N::Number(v) => {
                        let value = js_number(*v);
                        ar.add(N::Property {
                            value,
                            quote: Quote::None,
                        })
                    }
                    N::Str { value, quote } => {
                        let (value, quote) = (value.clone(), *quote);
                        ar.add(N::Property { value, quote })
                    }
                    N::SpecialNamePath {
                        special: Special::Event,
                        ..
                    } => parsed,
                    N::BigInt(_) | N::SpecialNamePath { .. } => return Err(Fail::Other),
                    _ if brackets && square_any => ar.add(N::IndexedAccessIndex(parsed)),
                    _ => return Err(Fail::Other),
                };
                if brackets && !lx.eat(Tk::RBracket)? {
                    return Err(Fail::Other);
                }
                N::NamePath {
                    left: root(ar, l)?,
                    right,
                    kind,
                }
            }
            P::KeyValue {
                optional,
                variadic,
                accept_list,
            } => {
                let Some(mut l) = left else { return Ok(None) };
                if !infix(Tk::Colon, KEY_VALUE) {
                    return Ok(None);
                }
                let (mut opt, mut var) = (false, false);
                if optional && let N::Nullable { el, .. } = ar.nodes[l] {
                    opt = true;
                    l = el;
                }
                if variadic && let N::Variadic { el: Some(el), .. } = ar.nodes[l] {
                    var = true;
                    l = el;
                }
                let N::Name(key) = &ar.nodes[l] else {
                    if accept_list && matches!(ar.nodes[l], N::ParameterList(_)) {
                        lx.eat(Tk::Colon)?;
                        return Ok(Some(l));
                    }
                    return Err(Fail::Other);
                };
                let key = key.clone();
                lx.eat(Tk::Colon)?;
                let right = self.parse_type(lx, ar, KEY_VALUE)?;
                N::KeyValue {
                    key,
                    right: Some(right),
                    optional: opt,
                    variadic: var,
                }
            }
            P::Symbol => {
                let Some(l) = left else { return Ok(None) };
                if !infix(Tk::LParen, SYMBOL) {
                    return Ok(None);
                }
                let N::Name(value) = &ar.nodes[l] else {
                    return Err(Fail::Other);
                };
                let value = value.clone();
                lx.eat(Tk::LParen)?;
                let mut el = None;
                if !lx.eat(Tk::RParen)? {
                    let n = self.parse_intermediate(lx, ar, SYMBOL)?;
                    let ok = match &ar.nodes[n] {
                        N::Variadic { el: Some(e), .. } => matches!(ar.nodes[*e], N::Name(_)),
                        N::Number(_) | N::Name(_) => true,
                        _ => false,
                    };
                    if !ok || !lx.eat(Tk::RParen)? {
                        return Err(Fail::Other);
                    }
                    el = Some(n);
                }
                N::Symbol { value, el }
            }
            P::ArrayBrackets => {
                let Some(l) = left else { return Ok(None) };
                if !(cur == Tk::LBracket && next == Tk::RBracket && ARRAY_BRACKETS > prec) {
                    return Ok(None);
                }
                lx.eat(Tk::LBracket)?;
                lx.eat(Tk::RBracket)?;
                let el = root(ar, l)?;
                let array = ar.add(N::Name("Array".to_owned()));
                N::Generic {
                    left: array,
                    elements: vec![el],
                    square: true,
                    dot: false,
                }
            }
            P::StringValue => {
                if left.is_some() || cur != Tk::Str {
                    return Ok(None);
                }
                let text = lx.text(lx.cur());
                lx.eat(Tk::Str)?;
                N::Str {
                    value: text[1..text.len() - 1].to_owned(),
                    quote: quote_of(text),
                }
            }
            P::ObjectField {
                squared,
                key_types,
                optional,
                readonly,
            } => {
                let Some(mut l) = left else { return Ok(None) };
                if !infix(Tk::Colon, KEY_VALUE) {
                    return Ok(None);
                }
                let (mut opt, mut ro) = (false, false);
                if optional && let N::Nullable { el, .. } = ar.nodes[l] {
                    opt = true;
                    l = el;
                }
                if readonly && let N::ReadonlyProperty(el) = ar.nodes[l] {
                    ro = true;
                    l = el;
                }
                let parent = self.base.unwrap_or(self);
                let key = match &ar.nodes[l] {
                    N::BigInt(_) => return Err(Fail::Other),
                    N::Number(v) => Some((js_number(*v), Quote::None)),
                    N::Name(v) => Some((v.clone(), Quote::None)),
                    N::Str { value, quote } => Some((value.clone(), *quote)),
                    N::IndexSignature { .. } | N::MappedType { .. } => {
                        if !squared {
                            return Err(Fail::Other);
                        }
                        None
                    }
                    _ => {
                        if !key_types {
                            return Err(Fail::Other);
                        }
                        lx.eat(Tk::Colon)?;
                        let right = parent.parse_type(lx, ar, KEY_VALUE)?;
                        let left = root(ar, l)?;
                        return Ok(Some(ar.add(N::JsdocObjectField { left, right })));
                    }
                };
                lx.eat(Tk::Colon)?;
                let right = parent.parse_type(lx, ar, KEY_VALUE)?;
                let (key, quote) = match key {
                    Some((k, q)) => (FieldKey::Text(k), q),
                    None => (FieldKey::Node(l), Quote::None),
                };
                N::ObjectField {
                    key,
                    right: Some(right),
                    optional: opt,
                    readonly: ro,
                    quote,
                }
            }
            P::ReadonlyArray => {
                if left.is_some() || cur != Tk::Readonly {
                    return Ok(None);
                }
                lx.eat(Tk::Readonly)?;
                let el = self.parse_intermediate(lx, ar, ALL)?;
                if !matches!(ar.nodes[el], N::Tuple(_) | N::Generic { square: true, .. }) {
                    return Err(Fail::Other);
                }
                N::ReadonlyArray(el)
            }
            P::Import => {
                if left.is_some() || cur != Tk::Import {
                    return Ok(None);
                }
                lx.eat(Tk::Import)?;
                if !lx.eat(Tk::LParen)? {
                    return Err(Fail::Other);
                }
                let path = self.parse_type(lx, ar, PREFIX)?;
                if !matches!(ar.nodes[path], N::Str { .. }) || !lx.eat(Tk::RParen)? {
                    return Err(Fail::Other);
                }
                N::Import(path)
            }
            P::Tuple => {
                if left.is_some() || cur != Tk::LBracket {
                    return Ok(None);
                }
                lx.eat(Tk::LBracket)?;
                if lx.eat(Tk::RBracket)? {
                    N::Tuple(Vec::new())
                } else {
                    let list = self.parse_intermediate(lx, ar, ALL)?;
                    let elements = match &ar.nodes[list] {
                        N::ParameterList(els) => {
                            let els = els.clone();
                            let kv = matches!(ar.nodes[els[0]], N::KeyValue { .. });
                            for &e in &els {
                                if kv != matches!(ar.nodes[e], N::KeyValue { .. }) {
                                    return Err(Fail::Other);
                                }
                            }
                            els
                        }
                        N::KeyValue { .. } => vec![list],
                        _ => vec![root(ar, list)?],
                    };
                    if !lx.eat(Tk::RBracket)? {
                        return Err(Fail::Other);
                    }
                    if elements.iter().any(|&e| matches!(ar.nodes[e], N::Unknown)) {
                        return Err(Fail::Other);
                    }
                    N::Tuple(elements)
                }
            }
            P::Asserts => {
                if left.is_some() || cur != Tk::Asserts {
                    return Ok(None);
                }
                lx.eat(Tk::Asserts)?;
                let l = self.parse_intermediate(lx, ar, SYMBOL)?;
                if !matches!(ar.nodes[l], N::Name(_)) {
                    return Err(Fail::Other);
                }
                if !lx.eat(Tk::Is)? {
                    N::AssertsPlain(l)
                } else {
                    let r = self.parse_intermediate(lx, ar, INFIX)?;
                    N::Asserts {
                        left: l,
                        right: root(ar, r)?,
                    }
                }
            }
            P::Conditional => {
                let Some(l) = left else { return Ok(None) };
                if !infix(Tk::Extends, INFIX) {
                    return Ok(None);
                }
                lx.eat(Tk::Extends)?;
                let extends = self.parse_type(lx, ar, KEY_OF_TYPE_OF)?;
                lx.eat(Tk::Question)?;
                let then = self.parse_type(lx, ar, INFIX)?;
                lx.eat(Tk::Colon)?;
                let checks = root(ar, l)?;
                let other = self.parse_type(lx, ar, INFIX)?;
                N::Conditional {
                    checks,
                    extends,
                    then,
                    other,
                }
            }
            P::UniqueSymbol => {
                if left.is_some()
                    || cur != Tk::Ident
                    || lx.text(lx.cur()) != "unique"
                    || next != Tk::Ident
                    || lx.text(lx.peek()) != "symbol"
                {
                    return Ok(None);
                }
                lx.eat(Tk::Ident)?;
                lx.eat(Tk::Ident)?;
                N::UniqueSymbol
            }
            P::Arrow => {
                let Some(l) = left else { return Ok(None) };
                if !infix(Tk::Arrow, ARROW) {
                    return Ok(None);
                }
                lx.eat(Tk::Arrow)?;
                let params = parameters(ar, l)?;
                if params
                    .iter()
                    .any(|&q| !matches!(ar.nodes[q], N::Name(_) | N::KeyValue { .. }))
                {
                    return Err(Fail::Other);
                }
                let ret = self.parse_type(lx, ar, OBJECT)?;
                N::Function {
                    type_params: None,
                    params,
                    ret: Some(ret),
                    arrow: true,
                    constructor: false,
                    parenthesis: true,
                }
            }
            P::GenericArrow => {
                if left.is_some() || cur != Tk::Lt {
                    return Ok(None);
                }
                lx.eat(Tk::Lt)?;
                let tps = self.type_params(lx, ar)?;
                let f = self.parse_intermediate(lx, ar, SYMBOL)?;
                if let N::Function { type_params, .. } = &mut ar.nodes[f] {
                    *type_params = Some(tps);
                }
                return Ok(Some(f));
            }
            P::Predicate => {
                let Some(l) = left else { return Ok(None) };
                if !infix(Tk::Is, INFIX) {
                    return Ok(None);
                }
                if !matches!(ar.nodes[l], N::Name(_)) {
                    return Err(Fail::Other);
                }
                lx.eat(Tk::Is)?;
                let r = self.parse_intermediate(lx, ar, INFIX)?;
                N::Predicate {
                    left: l,
                    right: root(ar, r)?,
                }
            }
            P::Template => {
                if left.is_some() || cur != Tk::Template {
                    return Ok(None);
                }
                let text = lx.text(lx.cur());
                lx.eat(Tk::Template)?;
                return self
                    .template(lx.loose, &text[1..text.len() - 1], ar)
                    .map(Some);
            }
            P::FunctionProperty => {
                let call_like = matches!(next, Tk::LParen | Tk::Lt);
                let accept = left.is_none()
                    && (matches!(cur, Tk::New | Tk::Ident | Tk::Str) && call_like
                        || matches!(cur, Tk::LParen | Tk::Lt));
                if !accept {
                    return Ok(None);
                }
                let kind = if lx.eat(Tk::New)? {
                    SigKind::Construct
                } else if cur == Tk::Ident {
                    let name = lx.text(lx.cur()).to_owned();
                    lx.eat(Tk::Ident)?;
                    SigKind::Method {
                        name,
                        quote: Quote::None,
                    }
                } else if cur == Tk::Str {
                    let text = lx.text(lx.cur());
                    lx.eat(Tk::Str)?;
                    SigKind::Method {
                        name: text[1..text.len() - 1].to_owned(),
                        quote: quote_of(text),
                    }
                } else {
                    SigKind::Call
                };
                let type_params = if lx.eat(Tk::Lt)? {
                    Some(self.type_params(lx, ar)?)
                } else {
                    None
                };
                if !lx.is(Tk::LParen) {
                    return Err(Fail::Other);
                }
                let ret = ar.add(N::Name("void".to_owned()));
                N::Signature {
                    kind,
                    type_params,
                    params: Vec::new(),
                    ret,
                }
            }
            P::ReadonlyProperty => {
                if left.is_some() || cur != Tk::Readonly || next == Tk::Colon {
                    return Ok(None);
                }
                lx.eat(Tk::Readonly)?;
                N::ReadonlyProperty(self.parse_intermediate(lx, ar, KEY_VALUE)?)
            }
            P::ObjectSquared => {
                if left.is_some() || cur != Tk::LBracket {
                    return Ok(None);
                }
                return self.squared_property(lx, ar).map(Some);
            }
        };
        Ok(Some(ar.add(n)))
    }

    /// Type parameters after `<` up to `>` (also stops, without error, before anything that
    /// is neither `,` nor `>`).
    fn type_params(&self, lx: &mut Lexer, ar: &mut Arena) -> R<Vec<Id>> {
        let mut out = Vec::new();
        loop {
            let mut default = None;
            let mut name = self.parse_intermediate(lx, ar, SYMBOL)?;
            if let N::Optional { el, .. } = ar.nodes[name] {
                name = el;
                default = Some(self.parse_type(lx, ar, SYMBOL)?);
            }
            if !matches!(ar.nodes[name], N::Name(_)) {
                return Err(Fail::Other);
            }
            let mut constraint = None;
            if lx.eat(Tk::Extends)? {
                let mut c = self.parse_type(lx, ar, SYMBOL)?;
                if let N::Optional { el, .. } = ar.nodes[c] {
                    c = el;
                    default = Some(self.parse_type(lx, ar, SYMBOL)?);
                }
                constraint = Some(c);
            }
            out.push(ar.add(N::TypeParameter {
                name,
                constraint,
                default,
            }));
            if lx.eat(Tk::Gt)? || !lx.eat(Tk::Comma)? {
                break;
            }
        }
        Ok(out)
    }

    fn object(&self, lx: &mut Lexer, ar: &mut Arena, field: G, signatures: bool) -> R<Id> {
        lx.eat(Tk::LBrace)?;
        let mut elements = Vec::new();
        if lx.eat(Tk::RBrace)? {
            return Ok(ar.add(N::Object {
                elements,
                sep: Sep::Comma,
            }));
        }
        let fields = self.sub(Gram {
            id: field,
            signature: false,
        });
        let mut sep = None;
        loop {
            let mut f = fields.parse_intermediate(lx, ar, OBJECT)?;
            let mut optional = false;
            if let N::Nullable { el, .. } = ar.nodes[f] {
                optional = true;
                f = el;
            }
            match &ar.nodes[f] {
                N::Number(_) | N::Name(_) | N::Str { .. } => {
                    let (key, quote) = match &ar.nodes[f] {
                        N::Number(v) => (js_number(*v), Quote::None),
                        N::Name(v) => (v.clone(), Quote::None),
                        N::Str { value, quote } => (value.clone(), *quote),
                        _ => unreachable!(),
                    };
                    elements.push(ar.add(N::ObjectField {
                        key: FieldKey::Text(key),
                        right: None,
                        optional,
                        readonly: false,
                        quote,
                    }));
                }
                N::Signature { .. } if signatures => {
                    let sig = self.sub(Gram {
                        id: self.gram.id,
                        signature: true,
                    });
                    let list = sig.parse_intermediate(lx, ar, OBJECT)?;
                    let params = parameters(ar, list)?;
                    let returns = self.parse_type(lx, ar, OBJECT)?;
                    if let N::Signature { params: p, ret, .. } = &mut ar.nodes[f] {
                        *p = params;
                        *ret = returns;
                    }
                    elements.push(f);
                }
                N::ObjectField { .. } | N::JsdocObjectField { .. } => elements.push(f),
                N::ReadonlyProperty(el) if matches!(ar.nodes[*el], N::ObjectField { .. }) => {
                    let el = *el;
                    if let N::ObjectField {
                        key: FieldKey::Node(k),
                        ..
                    } = ar.nodes[el]
                        && matches!(ar.nodes[k], N::ComputedMethod { .. })
                    {
                        return Err(Fail::Other);
                    }
                    if let N::ObjectField { readonly, .. } = &mut ar.nodes[el] {
                        *readonly = true;
                    }
                    elements.push(el);
                }
                _ => return Err(Fail::Other),
            }
            if lx.cur().start_of_line {
                sep.get_or_insert(Sep::Linebreak);
                if !lx.eat(Tk::Comma)? {
                    lx.eat(Tk::Semi)?;
                }
            } else if lx.eat(Tk::Comma)? {
                sep = Some(if lx.cur().start_of_line {
                    Sep::CommaLinebreak
                } else {
                    Sep::Comma
                });
            } else if lx.eat(Tk::Semi)? {
                sep = Some(if lx.cur().start_of_line {
                    Sep::SemicolonLinebreak
                } else {
                    Sep::Semicolon
                });
            } else {
                break;
            }
            if lx.is(Tk::RBrace) {
                break;
            }
        }
        if !lx.eat(Tk::RBrace)? {
            return Err(Fail::Other);
        }
        Ok(ar.add(N::Object {
            elements,
            sep: sep.unwrap_or(Sep::Comma),
        }))
    }

    /// `[...]` at the start of a TypeScript object member: an index signature, a mapped
    /// type clause, or a computed property or method.
    fn squared_property(&self, lx: &mut Lexer, ar: &mut Arena) -> R<Id> {
        let Some(base) = self.base else {
            return Err(Fail::Other);
        };
        lx.eat(Tk::LBracket)?;
        let inner = self
            .parse_intermediate(lx, ar, OBJECT)
            .map_err(|_| Fail::Other)?;
        if let N::ObjectField {
            key: FieldKey::Text(k),
            optional: false,
            readonly: false,
            right: Some(r),
            ..
        } = &ar.nodes[inner]
        {
            let (k, r) = (k.clone(), *r);
            if !lx.eat(Tk::RBracket)? || !lx.eat(Tk::Colon)? {
                return Err(Fail::Other);
            }
            let sig = ar.add(N::IndexSignature { key: k, right: r });
            let right = base.parse_type(lx, ar, INDEX_BRACKETS)?;
            return Ok(ar.add(N::ObjectField {
                key: FieldKey::Node(sig),
                right: Some(right),
                optional: false,
                readonly: false,
                quote: Quote::None,
            }));
        }
        if let N::Name(name) = &ar.nodes[inner] {
            let name = name.clone();
            if lx.eat(Tk::In)? {
                let mapped = base.parse_type(lx, ar, INDEX_BRACKETS)?;
                if !lx.eat(Tk::RBracket)? {
                    return Err(Fail::Other);
                }
                let optional = lx.eat(Tk::Question)?;
                if !lx.eat(Tk::Colon)? {
                    return Err(Fail::Other);
                }
                let right = base.parse_type(lx, ar, INDEX_BRACKETS)?;
                let key = ar.add(N::MappedType {
                    key: name,
                    right: mapped,
                });
                return Ok(ar.add(N::ObjectField {
                    key: FieldKey::Node(key),
                    right: Some(right),
                    optional,
                    readonly: false,
                    quote: Quote::None,
                }));
            }
        }
        if !lx.eat(Tk::RBracket)? {
            return Err(Fail::Other);
        }
        let mut optional = lx.eat(Tk::Question)?;
        let type_params = if lx.eat(Tk::Lt)? {
            Some(self.type_params(lx, ar)?)
        } else {
            None
        };
        if lx.is(Tk::LParen) {
            let sig = self.sub(Gram {
                id: base.gram.id,
                signature: true,
            });
            let list = sig.parse_intermediate(lx, ar, OBJECT)?;
            let params = parameters(ar, list)?;
            if !optional {
                optional = lx.eat(Tk::Question)?;
            }
            lx.eat(Tk::Colon)?;
            let ret = self.parse_type(lx, ar, INDEX_BRACKETS)?;
            let key = ar.add(N::ComputedMethod {
                value: inner,
                optional,
                type_params: type_params.filter(|t| !t.is_empty()),
                params,
                ret,
            });
            return Ok(ar.add(N::ObjectField {
                key: FieldKey::Node(key),
                right: None,
                optional: false,
                readonly: false,
                quote: Quote::None,
            }));
        }
        if !optional {
            optional = lx.eat(Tk::Question)?;
        }
        if !lx.eat(Tk::Colon)? {
            return Err(Fail::Other);
        }
        let right = self.parse_type(lx, ar, INDEX_BRACKETS)?;
        let key = ar.add(N::ComputedProperty(inner));
        Ok(ar.add(N::ObjectField {
            key: FieldKey::Node(key),
            right: Some(right),
            optional,
            readonly: false,
            quote: Quote::None,
        }))
    }

    /// A template literal type's text between the backticks. Each `${` is followed by the
    /// longest prefix of the rest that reads as a type ending at a `}`.
    fn template(&self, loose: bool, body: &str, ar: &mut Arena) -> R<Id> {
        let mut literals = Vec::new();
        let mut interps = Vec::new();
        let mut current = body.to_owned();
        let push_literal = |current: &mut String, literals: &mut Vec<String>| {
            let n = template_literal_part(current);
            literals.push(current[..n].replace("\\`", "`"));
            current.drain(..n);
        };
        push_literal(&mut current, &mut literals);
        while let Some(rest) = current.strip_prefix("${") {
            let mut snipped = rest.to_owned();
            let mut remnant = String::new();
            let after = loop {
                if snipped.is_empty() {
                    return Err(Fail::Other);
                }
                let attempt = (|| {
                    let mut lx = Lexer::new(&snipped, loose)?;
                    let cx = Cx {
                        gram: self.gram,
                        base: None,
                        strict: false,
                        class_context: false,
                    };
                    let t = cx.parse_type(&mut lx, ar, ALL)?;
                    let closes = lx.text(lx.cur()) == "}";
                    Ok::<_, Fail>((t, closes, lx.remaining().to_owned()))
                })();
                match attempt {
                    Ok((t, closes, rem)) => {
                        interps.push(t);
                        if !closes {
                            return Err(Fail::Other);
                        }
                        break rem;
                    }
                    Err(_) => {
                        let c = snipped.pop().expect("not empty");
                        remnant.insert(0, c);
                    }
                }
            };
            current = after + &remnant;
            push_literal(&mut current, &mut literals);
        }
        Ok(ar.add(N::TemplateLiteral { literals, interps }))
    }
}

/// The length of the literal text at the start of a template body (up to an unescaped
/// backtick or `${`). Its first character is never treated as an escape.
fn template_literal_part(text: &str) -> usize {
    if text.starts_with('`') || text.starts_with("${") {
        return 0;
    }
    let mut escaped = false;
    let mut it = text.char_indices().skip(1).peekable();
    while let Some((i, c)) = it.next() {
        if !escaped && (c == '`' || (c == '$' && it.peek().is_some_and(|&(_, n)| n == '{'))) {
            return i;
        }
        escaped = !escaped && c == '\\';
    }
    text.len()
}

fn quote_of(text: &str) -> Quote {
    if text.starts_with('\'') {
        Quote::Single
    } else {
        Quote::Double
    }
}

fn root(ar: &Arena, id: Id) -> R<Id> {
    if ar.nodes[id].is_intermediate() {
        Err(Fail::Other)
    } else {
        Ok(id)
    }
}

fn kv_or_root(ar: &Arena, id: Id) -> R<Id> {
    if matches!(ar.nodes[id], N::KeyValue { .. }) {
        Ok(id)
    } else {
        root(ar, id)
    }
}

fn parameters(ar: &Arena, id: Id) -> R<Vec<Id>> {
    let list = match &ar.nodes[id] {
        N::ParameterList(els) => els.clone(),
        N::Parenthesis(el) => vec![*el],
        _ => return Err(Fail::Other),
    };
    for &p in &list {
        kv_or_root(ar, p)?;
    }
    Ok(list)
}

/// `BigInt(text).toString()`.
fn bigint_text(digits: &str) -> String {
    let (neg, d) = match digits.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, digits),
    };
    let d = d.trim_start_matches('0');
    match (d.is_empty(), neg) {
        (true, _) => "0".to_owned(),
        (false, true) => format!("-{d}"),
        (false, false) => d.to_owned(),
    }
}

/// `parseFloat` of a number token.
fn parse_float(text: &str) -> f64 {
    match text {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => text.parse().unwrap_or(f64::NAN),
    }
}

/// JavaScript's `Number.prototype.toString()`.
pub fn js_number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if v == 0.0 {
        return "0".to_owned();
    }
    let a = v.abs();
    if (1e-6..1e21).contains(&a) {
        return format!("{v}");
    }
    let s = format!("{v:e}");
    match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => s,
    }
}

// ---------------------------------------------------------------------------------------------
// Entry points.

fn run(text: &str, loose: bool, gram: G, top: Option<bool>) -> Option<TypeTree> {
    let mut lx = Lexer::new(text, loose).ok()?;
    let mut ar = Arena {
        nodes: Vec::new(),
        depth: 0,
    };
    let cx = Cx {
        gram: Gram {
            id: gram,
            signature: false,
        },
        base: None,
        strict: top.is_some(),
        class_context: top == Some(true),
    };
    let r = cx.parse_type(&mut lx, &mut ar, ALL).ok()?;
    if !lx.is(Tk::Eof) {
        return None;
    }
    let r = if top.is_some() {
        cx.not_reserved(&ar, r).ok()?
    } else {
        r
    };
    Some(TypeTree {
        nodes: ar.nodes,
        root: r,
    })
}

/// `parse(type, mode, { classContext })`.
pub fn parse(text: &str, mode: Mode, class_context: bool) -> Option<TypeTree> {
    let (loose, g) = match mode {
        Mode::Jsdoc => (true, G::Jsdoc),
        Mode::Closure => (true, G::Closure),
        Mode::Typescript => (false, G::Ts),
    };
    run(text, loose, g, Some(class_context))
}

/// `tryParse`: TypeScript, then Closure, then JSDoc.
pub fn try_parse(text: &str, class_context: bool) -> Option<TypeTree> {
    [Mode::Typescript, Mode::Closure, Mode::Jsdoc]
        .into_iter()
        .find_map(|m| parse(text, m, class_context))
}

/// `parse` for a `settings.jsdoc.mode` (`permissive` tries every mode).
pub fn parse_for_setting(text: &str, mode: &str, class_context: bool) -> Option<TypeTree> {
    match Mode::from_setting(mode) {
        Some(m) => parse(text, m, class_context),
        None => try_parse(text, class_context),
    }
}

/// `parseNamePath(path, mode, { includeSpecial })`.
pub fn parse_name_path(text: &str, mode: Mode, include_special: bool) -> Option<TypeTree> {
    let g = match (mode, include_special) {
        (Mode::Jsdoc | Mode::Closure, false) => G::JsdocNamePath,
        (Mode::Typescript, false) => G::TsNamePath,
        (Mode::Jsdoc, true) => G::JsdocNamePathSpecial,
        (Mode::Closure, true) => G::ClosureNamePathSpecial,
        (Mode::Typescript, true) => G::TsNamePathSpecial,
    };
    run(text, mode != Mode::Typescript, g, None)
}

/// `parseName(name, mode)`.
pub fn parse_name(text: &str, mode: Mode) -> Option<TypeTree> {
    run(text, mode != Mode::Typescript, G::Name, None)
}

// ---------------------------------------------------------------------------------------------
// Walking and writing.

impl TypeTree {
    /// The children of `id` in visiting order, with the property each sits in.
    pub fn children(&self, id: Id) -> Vec<(Id, &'static str)> {
        let mut out = Vec::new();
        let mut one = |c: Option<Id>, p: &'static str| {
            if let Some(c) = c {
                out.push((c, p));
            }
        };
        match &self.nodes[id] {
            N::Nullable { el, .. }
            | N::NotNullable { el, .. }
            | N::Optional { el, .. }
            | N::Parenthesis(el)
            | N::Typeof(el)
            | N::Keyof(el)
            | N::Import(el)
            | N::AssertsPlain(el)
            | N::ReadonlyArray(el)
            | N::ReadonlyProperty(el)
            | N::Infer(el) => one(Some(*el), "element"),
            N::Variadic { el, .. } | N::Symbol { el, .. } => one(*el, "element"),
            N::ParameterList(els) | N::Union(els) | N::Intersection(els) | N::Tuple(els) => {
                for &e in els {
                    one(Some(e), "elements");
                }
            }
            N::Object { elements, .. } => {
                for &e in elements {
                    one(Some(e), "elements");
                }
            }
            N::KeyValue { right, .. } => one(*right, "right"),
            N::Generic { left, elements, .. } => {
                one(Some(*left), "left");
                for &e in elements {
                    one(Some(e), "elements");
                }
            }
            N::Function {
                type_params,
                params,
                ret,
                ..
            } => {
                for &t in type_params.iter().flatten() {
                    one(Some(t), "typeParameters");
                }
                for &p in params {
                    one(Some(p), "parameters");
                }
                one(*ret, "returnType");
            }
            N::NamePath { left, right, .. }
            | N::JsdocObjectField { left, right }
            | N::Asserts { left, right }
            | N::Predicate { left, right } => {
                one(Some(*left), "left");
                one(Some(*right), "right");
            }
            N::ObjectField { key, right, .. } => {
                if let FieldKey::Node(k) = key {
                    one(Some(*k), "key");
                }
                one(*right, "right");
            }
            N::Conditional {
                checks,
                extends,
                then,
                other,
            } => {
                one(Some(*checks), "checksType");
                one(Some(*extends), "extendsType");
                one(Some(*then), "trueType");
                one(Some(*other), "falseType");
            }
            N::TypeParameter {
                name,
                constraint,
                default,
            } => {
                one(Some(*name), "name");
                one(*constraint, "constraint");
                one(*default, "defaultValue");
            }
            N::Signature {
                type_params,
                params,
                ret,
                ..
            } => {
                for &t in type_params.iter().flatten() {
                    one(Some(t), "typeParameters");
                }
                for &p in params {
                    one(Some(p), "parameters");
                }
                one(Some(*ret), "returnType");
            }
            N::IndexSignature { right, .. }
            | N::MappedType { right, .. }
            | N::IndexedAccessIndex(right) => one(Some(*right), "right"),
            N::TemplateLiteral { interps, .. } => {
                for &i in interps {
                    one(Some(i), "interpolations");
                }
            }
            N::ComputedProperty(v) => one(Some(*v), "value"),
            N::ComputedMethod {
                value,
                type_params,
                params,
                ret,
                ..
            } => {
                one(Some(*value), "value");
                for &t in type_params.iter().flatten() {
                    one(Some(t), "typeParameters");
                }
                for &p in params {
                    one(Some(p), "parameters");
                }
                one(Some(*ret), "returnType");
            }
            N::Name(_)
            | N::Number(_)
            | N::BigInt(_)
            | N::Str { .. }
            | N::Null
            | N::Undefined
            | N::Any
            | N::Unknown
            | N::UniqueSymbol
            | N::Property { .. }
            | N::SpecialNamePath { .. } => {}
        }
        out
    }

    /// Every node in pre-order with its parent and the property it sits in.
    pub fn walk(&self) -> Vec<(Id, Option<Id>, &'static str)> {
        let mut out = Vec::new();
        let mut stack = vec![(self.root, None, "")];
        while let Some((id, parent, prop)) = stack.pop() {
            out.push((id, parent, prop));
            for (c, p) in self.children(id).into_iter().rev() {
                stack.push((c, Some(id), p));
            }
        }
        out
    }

    pub fn stringify(&self) -> String {
        let mut s = String::new();
        self.write(self.root, &mut s);
        s
    }

    fn list(&self, ids: &[Id], sep: &str, out: &mut String) {
        for (i, &id) in ids.iter().enumerate() {
            if i > 0 {
                out.push_str(sep);
            }
            self.write(id, out);
        }
    }

    fn quoted(value: &str, q: Quote, out: &mut String) {
        match q {
            Quote::None => out.push_str(value),
            Quote::Single => {
                out.push('\'');
                out.push_str(value);
                out.push('\'');
            }
            Quote::Double => {
                out.push('"');
                out.push_str(value);
                out.push('"');
            }
        }
    }

    fn signature_tail(&self, tps: &Option<Vec<Id>>, params: &[Id], ret: Id, out: &mut String) {
        if let Some(t) = tps {
            out.push('<');
            self.list(t, ", ", out);
            out.push('>');
        }
        out.push('(');
        self.list(params, ", ", out);
        out.push_str("): ");
        self.write(ret, out);
    }

    fn write(&self, id: Id, out: &mut String) {
        match &self.nodes[id] {
            N::Name(v) => out.push_str(v),
            N::Number(v) => out.push_str(&js_number(*v)),
            N::BigInt(v) => {
                out.push_str(v);
                out.push('n');
            }
            N::Str { value, quote } | N::Property { value, quote } => {
                Self::quoted(value, *quote, out)
            }
            N::Null => out.push_str("null"),
            N::Undefined => out.push_str("undefined"),
            N::Any => out.push('*'),
            N::Unknown => out.push('?'),
            N::UniqueSymbol => out.push_str("unique symbol"),
            N::Nullable { el, prefix }
            | N::NotNullable { el, prefix }
            | N::Optional { el, prefix } => {
                let mark = match &self.nodes[id] {
                    N::Nullable { .. } => "?",
                    N::NotNullable { .. } => "!",
                    _ => "=",
                };
                if *prefix {
                    out.push_str(mark);
                }
                self.write(*el, out);
                if !*prefix {
                    out.push_str(mark);
                }
            }
            N::Parenthesis(el) => {
                out.push('(');
                self.write(*el, out);
                out.push(')');
            }
            N::ParameterList(els) => self.list(els, ", ", out),
            N::KeyValue {
                key,
                right,
                optional,
                variadic,
            } => {
                if *variadic && !*optional {
                    out.push_str("...");
                }
                out.push_str(key);
                if *optional {
                    out.push('?');
                }
                if let Some(r) = right {
                    out.push_str(": ");
                    self.write(*r, out);
                }
            }
            N::Generic {
                left,
                elements,
                square,
                dot,
            } => {
                if *square {
                    let el = elements[0];
                    let wrap = matches!(self.nodes[el], N::Union(_) | N::Intersection(_));
                    if wrap {
                        out.push('(');
                    }
                    self.write(el, out);
                    if wrap {
                        out.push(')');
                    }
                    out.push_str("[]");
                } else {
                    self.write(*left, out);
                    if *dot {
                        out.push('.');
                    }
                    out.push('<');
                    self.list(elements, ", ", out);
                    out.push('>');
                }
            }
            N::Union(els) => self.list(els, " | ", out),
            N::Intersection(els) => self.list(els, " & ", out),
            N::Function {
                type_params,
                params,
                ret,
                arrow,
                constructor,
                parenthesis,
            } => {
                if !*arrow {
                    out.push_str(if *constructor { "new" } else { "function" });
                    if !*parenthesis {
                        return;
                    }
                    out.push('(');
                    self.list(params, ", ", out);
                    out.push(')');
                    if let Some(r) = ret {
                        out.push_str(": ");
                        self.write(*r, out);
                    }
                } else {
                    if *constructor {
                        out.push_str("new ");
                    }
                    if let Some(t) = type_params {
                        out.push('<');
                        self.list(t, ", ", out);
                        out.push('>');
                    }
                    out.push('(');
                    self.list(params, ", ", out);
                    out.push_str(") => ");
                    if let Some(r) = ret {
                        self.write(*r, out);
                    }
                }
            }
            N::Variadic { el, position, .. } => match (position, el) {
                (Some(true), Some(e)) => {
                    out.push_str("...");
                    self.write(*e, out);
                }
                (Some(false), Some(e)) => {
                    self.write(*e, out);
                    out.push_str("...");
                }
                _ => out.push_str("..."),
            },
            N::NamePath { left, right, kind } => {
                self.write(*left, out);
                match kind {
                    PathKind::Inner => out.push('~'),
                    PathKind::Instance => out.push('#'),
                    PathKind::Property => out.push('.'),
                    PathKind::Brackets => out.push('['),
                }
                self.write(*right, out);
                if *kind == PathKind::Brackets {
                    out.push(']');
                }
            }
            N::SpecialNamePath {
                special,
                value,
                quote,
            } => {
                out.push_str(match special {
                    Special::Module => "module:",
                    Special::Event => "event:",
                    Special::External => "external:",
                });
                Self::quoted(value, *quote, out);
            }
            N::Symbol { value, el } => {
                out.push_str(value);
                out.push('(');
                if let Some(e) = el {
                    self.write(*e, out);
                }
                out.push(')');
            }
            N::Object { elements, sep } => {
                let lb = matches!(
                    sep,
                    Sep::Linebreak | Sep::CommaLinebreak | Sep::SemicolonLinebreak
                );
                let ending = match sep {
                    Sep::CommaLinebreak => ",\n",
                    Sep::SemicolonLinebreak => ";\n",
                    Sep::Linebreak => "\n",
                    _ => "",
                };
                out.push('{');
                let many = elements.len() > 1;
                if lb && many {
                    out.push_str("\n  ");
                }
                let joiner = match sep {
                    Sep::Comma => ", ".to_owned(),
                    _ if lb => format!("{ending}  "),
                    _ => "; ".to_owned(),
                };
                self.list(elements, &joiner, out);
                if lb && many {
                    out.push('\n');
                }
                out.push('}');
            }
            N::ObjectField {
                key,
                right,
                optional,
                readonly,
                quote,
            } => {
                if *readonly {
                    out.push_str("readonly ");
                }
                let mut method = false;
                match key {
                    FieldKey::Text(k) => Self::quoted(k, *quote, out),
                    FieldKey::Node(k) => {
                        method = matches!(self.nodes[*k], N::ComputedMethod { .. });
                        self.write(*k, out);
                    }
                }
                if !method && *optional {
                    out.push('?');
                }
                if let Some(r) = right {
                    out.push_str(": ");
                    self.write(*r, out);
                }
            }
            N::JsdocObjectField { left, right } => {
                self.write(*left, out);
                out.push_str(": ");
                self.write(*right, out);
            }
            N::Typeof(el) => {
                out.push_str("typeof ");
                self.write(*el, out);
            }
            N::Keyof(el) => {
                out.push_str("keyof ");
                self.write(*el, out);
            }
            N::Import(el) => {
                out.push_str("import(");
                self.write(*el, out);
                out.push(')');
            }
            N::Tuple(els) => {
                out.push('[');
                self.list(els, ", ", out);
                out.push(']');
            }
            N::Asserts { left, right } => {
                out.push_str("asserts ");
                self.write(*left, out);
                out.push_str(" is ");
                self.write(*right, out);
            }
            N::AssertsPlain(el) => {
                out.push_str("asserts ");
                self.write(*el, out);
            }
            N::Predicate { left, right } => {
                self.write(*left, out);
                out.push_str(" is ");
                self.write(*right, out);
            }
            N::Conditional {
                checks,
                extends,
                then,
                other,
            } => {
                self.write(*checks, out);
                out.push_str(" extends ");
                self.write(*extends, out);
                out.push_str(" ? ");
                self.write(*then, out);
                out.push_str(" : ");
                self.write(*other, out);
            }
            N::ReadonlyArray(el) | N::ReadonlyProperty(el) => {
                out.push_str("readonly ");
                self.write(*el, out);
            }
            N::Infer(el) => {
                out.push_str("infer ");
                self.write(*el, out);
            }
            N::TypeParameter {
                name,
                constraint,
                default,
            } => {
                self.write(*name, out);
                if let Some(c) = constraint {
                    out.push_str(" extends ");
                    self.write(*c, out);
                }
                if let Some(d) = default {
                    out.push_str(" = ");
                    self.write(*d, out);
                }
            }
            N::Signature {
                kind,
                type_params,
                params,
                ret,
            } => {
                match kind {
                    SigKind::Call => {}
                    SigKind::Construct => out.push_str("new "),
                    SigKind::Method { name, quote } => Self::quoted(name, *quote, out),
                }
                self.signature_tail(type_params, params, *ret, out);
            }
            N::IndexSignature { key, right } => {
                out.push('[');
                out.push_str(key);
                out.push_str(": ");
                self.write(*right, out);
                out.push(']');
            }
            N::MappedType { key, right } => {
                out.push('[');
                out.push_str(key);
                out.push_str(" in ");
                self.write(*right, out);
                out.push(']');
            }
            N::IndexedAccessIndex(r) => self.write(*r, out),
            N::TemplateLiteral { literals, interps } => {
                out.push('`');
                for (i, lit) in literals.iter().enumerate() {
                    out.push_str(&lit.replace('`', "\\`"));
                    if let Some(&t) = interps.get(i)
                        && i + 1 < literals.len()
                    {
                        out.push_str("${");
                        self.write(t, out);
                        out.push('}');
                    }
                }
                out.push('`');
            }
            N::ComputedProperty(v) => {
                out.push('[');
                self.write(*v, out);
                out.push(']');
            }
            N::ComputedMethod {
                value,
                optional,
                type_params,
                params,
                ret,
            } => {
                out.push('[');
                self.write(*value, out);
                out.push(']');
                if *optional {
                    out.push('?');
                }
                self.signature_tail(type_params, params, *ret, out);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The tree as the parser's JSON (for comparing with the real parser).
    pub(crate) fn to_json(t: &TypeTree, id: Id) -> serde_json::Value {
        use serde_json::{Map, Value, json};
        let j = |i: Id| to_json(t, i);
        let js = |ids: &[Id]| Value::Array(ids.iter().map(|&i| to_json(t, i)).collect());
        let q = |q: Quote| match q {
            Quote::None => Value::Null,
            Quote::Single => json!("single"),
            Quote::Double => json!("double"),
        };
        let pos = |p: bool| json!(if p { "prefix" } else { "suffix" });
        let mut m = Map::new();
        m.insert("type".into(), json!(t.nodes[id].type_name()));
        let mut set = |k: &str, v: Value| {
            m.insert(k.into(), v);
        };
        match &t.nodes[id] {
            N::Name(v) => set("value", json!(v)),
            N::Number(v) => set("value", json!(js_number(*v))),
            N::BigInt(v) => set("value", json!(v)),
            N::Str { value, quote } | N::Property { value, quote } => {
                set("value", json!(value));
                set("quote", q(*quote));
            }
            N::Nullable { el, prefix }
            | N::NotNullable { el, prefix }
            | N::Optional { el, prefix } => {
                set("element", j(*el));
                set("position", pos(*prefix));
            }
            N::Parenthesis(el)
            | N::Typeof(el)
            | N::Keyof(el)
            | N::Import(el)
            | N::AssertsPlain(el)
            | N::ReadonlyArray(el)
            | N::ReadonlyProperty(el)
            | N::Infer(el) => set("element", j(*el)),
            N::IndexedAccessIndex(el) => set("right", j(*el)),
            N::ComputedProperty(el) => set("value", j(*el)),
            N::ParameterList(e) | N::Union(e) | N::Intersection(e) | N::Tuple(e) => {
                set("elements", js(e))
            }
            N::KeyValue {
                key,
                right,
                optional,
                variadic,
            } => {
                set("key", json!(key));
                set("right", right.map_or(Value::Null, j));
                set("optional", json!(optional));
                set("variadic", json!(variadic));
            }
            N::Generic {
                left,
                elements,
                square,
                dot,
            } => {
                set("left", j(*left));
                set("elements", js(elements));
                set("brackets", json!(if *square { "square" } else { "angle" }));
                set("dot", json!(dot));
            }
            N::Function {
                type_params,
                params,
                ret,
                arrow,
                constructor,
                parenthesis,
            } => {
                set(
                    "typeParameters",
                    type_params.as_deref().map_or(Value::Null, js),
                );
                set("parameters", js(params));
                set("returnType", ret.map_or(Value::Null, j));
                set("arrow", json!(arrow));
                set("constructor", json!(constructor));
                set("parenthesis", json!(parenthesis));
            }
            N::Variadic {
                el,
                position,
                square,
            } => {
                set("element", el.map_or(Value::Null, j));
                set("position", position.map_or(Value::Null, pos));
                set("squareBrackets", json!(square));
            }
            N::NamePath { left, right, kind } => {
                set("left", j(*left));
                set("right", j(*right));
                set(
                    "pathType",
                    json!(match kind {
                        PathKind::Property => "property",
                        PathKind::Brackets => "property-brackets",
                        PathKind::Inner => "inner",
                        PathKind::Instance => "instance",
                    }),
                );
            }
            N::SpecialNamePath {
                special,
                value,
                quote,
            } => {
                set(
                    "specialType",
                    json!(match special {
                        Special::Module => "module",
                        Special::Event => "event",
                        Special::External => "external",
                    }),
                );
                set("value", json!(value));
                set("quote", q(*quote));
            }
            N::Symbol { value, el } => {
                set("value", json!(value));
                set("element", el.map_or(Value::Null, j));
            }
            N::Object { elements, sep } => {
                set("elements", js(elements));
                set(
                    "separator",
                    json!(match sep {
                        Sep::Comma => "comma",
                        Sep::Semicolon => "semicolon",
                        Sep::Linebreak => "linebreak",
                        Sep::CommaLinebreak => "comma-and-linebreak",
                        Sep::SemicolonLinebreak => "semicolon-and-linebreak",
                    }),
                );
            }
            N::ObjectField {
                key,
                right,
                optional,
                readonly,
                quote,
            } => {
                set(
                    "key",
                    match key {
                        FieldKey::Text(k) => json!(k),
                        FieldKey::Node(k) => j(*k),
                    },
                );
                set("right", right.map_or(Value::Null, j));
                set("optional", json!(optional));
                set("readonly", json!(readonly));
                set("quote", q(*quote));
            }
            N::JsdocObjectField { left, right }
            | N::Asserts { left, right }
            | N::Predicate { left, right } => {
                set("left", j(*left));
                set("right", j(*right));
            }
            N::Conditional {
                checks,
                extends,
                then,
                other,
            } => {
                set("checksType", j(*checks));
                set("extendsType", j(*extends));
                set("trueType", j(*then));
                set("falseType", j(*other));
            }
            N::TypeParameter {
                name,
                constraint,
                default,
            } => {
                set("name", j(*name));
                set("constraint", constraint.map_or(Value::Null, j));
                set("defaultValue", default.map_or(Value::Null, j));
            }
            N::Signature {
                kind,
                type_params,
                params,
                ret,
            } => {
                if let SigKind::Method { name, quote } = kind {
                    set("name", json!(name));
                    set("quote", q(*quote));
                }
                set(
                    "typeParameters",
                    type_params.as_deref().map_or(Value::Null, js),
                );
                set("parameters", js(params));
                set("returnType", j(*ret));
            }
            N::IndexSignature { key, right } | N::MappedType { key, right } => {
                set("key", json!(key));
                set("right", j(*right));
            }
            N::TemplateLiteral { literals, interps } => {
                set("literals", json!(literals));
                set("interpolations", js(interps));
            }
            N::ComputedMethod {
                value,
                optional,
                type_params,
                params,
                ret,
            } => {
                set("value", j(*value));
                set("optional", json!(optional));
                set(
                    "typeParameters",
                    type_params.as_deref().map_or(Value::Null, js),
                );
                set("parameters", js(params));
                set("returnType", j(*ret));
            }
            N::Null | N::Undefined | N::Any | N::Unknown | N::UniqueSymbol => {}
        }
        Value::Object(m)
    }

    fn ts(s: &str) -> Option<String> {
        parse(s, Mode::Typescript, false).map(|t| t.stringify())
    }

    #[test]
    fn basics() {
        assert_eq!(ts("Array<string>").as_deref(), Some("Array<string>"));
        assert_eq!(ts("Array<string"), None);
        assert_eq!(ts("str%ng"), None);
        assert_eq!(ts("string[]").as_deref(), Some("string[]"));
        assert_eq!(ts("(a|b)[]").as_deref(), Some("(a | b)[]"));
        assert_eq!(
            ts("{a: string, b?: number}").as_deref(),
            Some("{a: string, b?: number}")
        );
        assert_eq!(
            ts("(x: number) => void").as_deref(),
            Some("(x: number) => void")
        );
        assert_eq!(
            parse("Array.<string>", Mode::Jsdoc, false)
                .map(|t| t.stringify())
                .as_deref(),
            Some("Array.<string>")
        );
        assert_eq!(ts("this"), None);
        assert!(parse("this", Mode::Typescript, true).is_some());
    }

    #[test]
    fn deep_nesting_fails_without_overflow() {
        let deep = format!("{}x{}", "Array<".repeat(5000), ">".repeat(5000));
        assert!(parse(&deep, Mode::Typescript, false).is_none());
        let parens = format!("{}x{}", "(".repeat(5000), ")".repeat(5000));
        assert!(parse(&parens, Mode::Jsdoc, false).is_none());
        assert!(parse(&"?".repeat(5000), Mode::Closure, false).is_none());
    }

    /// Writes our parse of every line of `$TS_R_JSDOC_TYPE_CORPUS` (one JSON string per
    /// line) as JSON lines to `$TS_R_JSDOC_TYPE_OUT`, for comparing with the real parser.
    #[test]
    #[ignore]
    fn dump_corpus() {
        let (Ok(input), Ok(out)) = (
            std::env::var("TS_R_JSDOC_TYPE_CORPUS"),
            std::env::var("TS_R_JSDOC_TYPE_OUT"),
        ) else {
            return;
        };
        let text = std::fs::read_to_string(input).unwrap();
        let mut lines = Vec::new();
        for line in text.lines() {
            let s: String = serde_json::from_str(line).unwrap();
            let mut row = serde_json::Map::new();
            for (name, mode) in [
                ("jsdoc", Mode::Jsdoc),
                ("closure", Mode::Closure),
                ("typescript", Mode::Typescript),
            ] {
                let v = parse(&s, mode, true)
                    .map(|t| serde_json::json!([to_json(&t, t.root), t.stringify()]))
                    .unwrap_or(serde_json::Value::Null);
                row.insert(name.into(), v);
                let np = parse_name_path(&s, mode, true).is_some();
                row.insert(format!("{name}-np"), serde_json::json!(np));
                let nm = parse_name(&s, mode).is_some();
                row.insert(format!("{name}-name"), serde_json::json!(nm));
            }
            lines.push(serde_json::Value::Object(row).to_string());
        }
        std::fs::write(out, lines.join("\n")).unwrap();
    }
}
