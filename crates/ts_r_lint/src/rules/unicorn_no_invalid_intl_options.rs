//! `unicorn/no-invalid-intl-options`: options objects passed to `Intl` APIs that use unknown
//! names, values the specification rejects, or combinations that are ignored or conflict.
//!
//! Checked calls: `new Intl.<Ctor>(locales, options)` for the ten constructors, the call forms
//! of `Intl.Collator`/`DateTimeFormat`/`NumberFormat`, `Intl.<Ctor>.supportedLocalesOf(locales,
//! options)` (not `Locale`; only `localeMatcher`), all followed through
//! `unicorn_util::global_path`; and `str.localeCompare(other, locales, options)` on a string
//! (Collator), `n.toLocaleString(locales, options)` on a number, bigint or typed array
//! (NumberFormat), `date.toLocale{,Date,Time}String(locales, options)` on a Date
//! (DateTimeFormat). Receivers come from `unicorn_types` checkers (untyped: syntax, annotations,
//! static values).
//!
//! The options argument must be an object literal (through TS wrappers) of plain properties
//! with static, distinct names (no `__proto__: x`); spreads before it skip the call. Each
//! property is reported once, at its key (or at the object), with the first problem found:
//! - an unknown name, with a rename suggestion when exactly one known name is one edit away
//!   (case-insensitive, adjacent transposition counts) and not already present;
//! - a value that doesn't normalize (booleans always do; numbers must lie in the range, then
//!   are floored, `roundingIncrement` from the fixed set; strings from the list or matching the
//!   identifier shape; symbols can't become strings; `useGrouping` also takes `true`/falsy);
//! - NumberFormat/PluralRules digit options: fraction and significant digits are only validated
//!   when that rounding is in use, min <= max, `roundingIncrement` needs `auto` priority,
//!   fraction rounding and equal fraction digits, fraction digits are ignored under
//!   significant rounding, `compactDisplay` without compact notation;
//! - NumberFormat `style` requiring `currency`/`unit` and ignoring their companions;
//!   DisplayNames requiring `type` and `languageDisplay` only for languages; DateTimeFormat
//!   style vs component and method conflicts, `formatMatcher` and `hourCycle` ignored;
//!   DurationFormat units continuing a numeric or fractional style, and no `always` display
//!   for fractional units.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::has_comment_in;
use super::unicorn_types::{Checker, Guess, is_string};
use super::unicorn_util::global_path;
use super::unicorn_util_h1::static_property_name;
use crate::ast_util::number_to_string;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-intl-options",
        "Disallow invalid or ignored Intl options.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoInvalidIntlOptions>,
};

#[derive(Debug, Default)]
struct NoInvalidIntlOptions;

/// How an option's value is normalized.
#[derive(Clone, Copy)]
enum Def {
    Bool,
    Str,
    Range(f64, f64),
    List(&'static [&'static str]),
    Shape(fn(&str) -> bool),
}

const TEXT: Def = Def::List(&["long", "short", "narrow"]);
const NUMERIC: Def = Def::List(&["numeric", "2-digit"]);
const DATE_STYLES: Def = Def::List(&["full", "long", "medium", "short"]);
const LOCALE_MATCHER: Def = Def::List(&["lookup", "best fit"]);
const HOUR_CYCLE: Def = Def::List(&["h11", "h12", "h23", "h24"]);
const CASE_FIRST: Def = Def::List(&["upper", "lower", "false"]);
const UNICODE_TYPE: Def = Def::Shape(is_unicode_type);
const ROUNDING_INCREMENTS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 2500.0,
    5000.0,
];
const FRACTION: [&str; 2] = ["minimumFractionDigits", "maximumFractionDigits"];
const SIGNIFICANT: [&str; 2] = ["minimumSignificantDigits", "maximumSignificantDigits"];
const DATE_COMPONENTS: [&str; 11] = [
    "weekday",
    "era",
    "year",
    "month",
    "day",
    "dayPeriod",
    "hour",
    "minute",
    "second",
    "fractionalSecondDigits",
    "timeZoneName",
];
const TIME_UNITS: [&str; 6] = [
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];
const UNITS: &[&str] = &[
    "acre",
    "bit",
    "byte",
    "celsius",
    "centimeter",
    "day",
    "degree",
    "fahrenheit",
    "fluid-ounce",
    "foot",
    "gallon",
    "gigabit",
    "gigabyte",
    "gram",
    "hectare",
    "hour",
    "inch",
    "kilobit",
    "kilobyte",
    "kilogram",
    "kilometer",
    "liter",
    "megabit",
    "megabyte",
    "meter",
    "microsecond",
    "mile",
    "mile-scandinavian",
    "milliliter",
    "millimeter",
    "millisecond",
    "minute",
    "month",
    "nanosecond",
    "ounce",
    "percent",
    "petabyte",
    "pound",
    "second",
    "stone",
    "terabit",
    "terabyte",
    "week",
    "yard",
    "year",
];

const DIGIT_OPTIONS: &[(&str, Def)] = &[
    ("minimumIntegerDigits", Def::Range(1.0, 21.0)),
    ("minimumFractionDigits", Def::Range(0.0, 100.0)),
    ("maximumFractionDigits", Def::Range(0.0, 100.0)),
    ("minimumSignificantDigits", Def::Range(1.0, 21.0)),
    ("maximumSignificantDigits", Def::Range(1.0, 21.0)),
    ("roundingIncrement", Def::Range(1.0, 5000.0)),
    (
        "roundingMode",
        Def::List(&[
            "ceil",
            "floor",
            "expand",
            "trunc",
            "halfCeil",
            "halfFloor",
            "halfExpand",
            "halfTrunc",
            "halfEven",
        ]),
    ),
    (
        "roundingPriority",
        Def::List(&["auto", "morePrecision", "lessPrecision"]),
    ),
    (
        "trailingZeroDisplay",
        Def::List(&["auto", "stripIfInteger"]),
    ),
    (
        "notation",
        Def::List(&["standard", "scientific", "engineering", "compact"]),
    ),
    ("compactDisplay", Def::List(&["short", "long"])),
];

const COLLATOR: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("usage", Def::List(&["sort", "search"])),
    ("collation", UNICODE_TYPE),
    ("numeric", Def::Bool),
    ("caseFirst", CASE_FIRST),
    (
        "sensitivity",
        Def::List(&["base", "accent", "case", "variant"]),
    ),
    ("ignorePunctuation", Def::Bool),
];

const DATE_TIME_FORMAT: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("calendar", UNICODE_TYPE),
    ("numberingSystem", UNICODE_TYPE),
    ("hour12", Def::Bool),
    ("hourCycle", HOUR_CYCLE),
    ("timeZone", Def::Str),
    ("weekday", TEXT),
    ("era", TEXT),
    ("year", NUMERIC),
    (
        "month",
        Def::List(&["numeric", "2-digit", "long", "short", "narrow"]),
    ),
    ("day", NUMERIC),
    ("dayPeriod", TEXT),
    ("hour", NUMERIC),
    ("minute", NUMERIC),
    ("second", NUMERIC),
    ("fractionalSecondDigits", Def::Range(1.0, 3.0)),
    (
        "timeZoneName",
        Def::List(&[
            "short",
            "long",
            "shortOffset",
            "longOffset",
            "shortGeneric",
            "longGeneric",
        ]),
    ),
    ("formatMatcher", Def::List(&["basic", "best fit"])),
    ("dateStyle", DATE_STYLES),
    ("timeStyle", DATE_STYLES),
];

const DISPLAY_NAMES: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("style", TEXT),
    (
        "type",
        Def::List(&[
            "language",
            "region",
            "script",
            "currency",
            "calendar",
            "dateTimeField",
        ]),
    ),
    ("fallback", Def::List(&["code", "none"])),
    ("languageDisplay", Def::List(&["dialect", "standard"])),
];

const DISPLAY: Def = Def::List(&["auto", "always"]);
const DURATION_TIME: Def = Def::List(&["long", "short", "narrow", "numeric", "2-digit"]);
const DURATION_FRACTION: Def = Def::List(&["long", "short", "narrow", "numeric"]);

const DURATION_FORMAT: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("numberingSystem", UNICODE_TYPE),
    ("style", Def::List(&["long", "short", "narrow", "digital"])),
    ("years", TEXT),
    ("months", TEXT),
    ("weeks", TEXT),
    ("days", TEXT),
    ("hours", DURATION_TIME),
    ("minutes", DURATION_TIME),
    ("seconds", DURATION_TIME),
    ("milliseconds", DURATION_FRACTION),
    ("microseconds", DURATION_FRACTION),
    ("nanoseconds", DURATION_FRACTION),
    ("yearsDisplay", DISPLAY),
    ("monthsDisplay", DISPLAY),
    ("weeksDisplay", DISPLAY),
    ("daysDisplay", DISPLAY),
    ("hoursDisplay", DISPLAY),
    ("minutesDisplay", DISPLAY),
    ("secondsDisplay", DISPLAY),
    ("millisecondsDisplay", DISPLAY),
    ("microsecondsDisplay", DISPLAY),
    ("nanosecondsDisplay", DISPLAY),
    ("fractionalDigits", Def::Range(0.0, 9.0)),
];

const LIST_FORMAT: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("type", Def::List(&["conjunction", "disjunction", "unit"])),
    ("style", TEXT),
];

const LOCALE: &[(&str, Def)] = &[
    ("language", Def::Shape(is_language)),
    ("script", Def::Shape(is_script)),
    ("region", Def::Shape(is_region)),
    ("variants", Def::Shape(is_variants)),
    ("calendar", UNICODE_TYPE),
    ("collation", UNICODE_TYPE),
    ("firstDayOfWeek", Def::Shape(is_first_day_of_week)),
    ("hourCycle", HOUR_CYCLE),
    ("caseFirst", CASE_FIRST),
    ("numeric", Def::Bool),
    ("numberingSystem", UNICODE_TYPE),
];

const NUMBER_FORMAT: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("numberingSystem", UNICODE_TYPE),
    (
        "style",
        Def::List(&["decimal", "percent", "currency", "unit"]),
    ),
    ("currency", Def::Shape(is_currency)),
    (
        "currencyDisplay",
        Def::List(&["code", "symbol", "narrowSymbol", "name"]),
    ),
    ("currencySign", Def::List(&["standard", "accounting"])),
    ("unit", Def::Shape(is_unit)),
    ("unitDisplay", Def::List(&["short", "narrow", "long"])),
    (
        "useGrouping",
        Def::List(&["min2", "auto", "always", "true", "false"]),
    ),
    (
        "signDisplay",
        Def::List(&["auto", "never", "always", "exceptZero", "negative"]),
    ),
];

const PLURAL_RULES: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("type", Def::List(&["cardinal", "ordinal"])),
];

const RELATIVE_TIME_FORMAT: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("numberingSystem", UNICODE_TYPE),
    ("style", TEXT),
    ("numeric", Def::List(&["always", "auto"])),
];

const SEGMENTER: &[(&str, Def)] = &[
    ("localeMatcher", LOCALE_MATCHER),
    ("granularity", Def::List(&["grapheme", "word", "sentence"])),
];

const SUPPORTED_LOCALES_OF: &[(&str, Def)] = &[("localeMatcher", LOCALE_MATCHER)];

const CONSTRUCTORS: [&str; 10] = [
    "Collator",
    "DateTimeFormat",
    "DisplayNames",
    "DurationFormat",
    "ListFormat",
    "Locale",
    "NumberFormat",
    "PluralRules",
    "RelativeTimeFormat",
    "Segmenter",
];

/// The option table of a constructor (NumberFormat and PluralRules add the digit options).
fn definitions(ctor: &str) -> (&'static [(&'static str, Def)], bool) {
    match ctor {
        "Collator" => (COLLATOR, false),
        "DateTimeFormat" => (DATE_TIME_FORMAT, false),
        "DisplayNames" => (DISPLAY_NAMES, false),
        "DurationFormat" => (DURATION_FORMAT, false),
        "ListFormat" => (LIST_FORMAT, false),
        "Locale" => (LOCALE, false),
        "NumberFormat" => (NUMBER_FORMAT, true),
        "PluralRules" => (PLURAL_RULES, true),
        "RelativeTimeFormat" => (RELATIVE_TIME_FORMAT, false),
        "Segmenter" => (SEGMENTER, false),
        _ => (SUPPORTED_LOCALES_OF, false),
    }
}

fn all_alnum(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn all_alpha(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_unicode_type(s: &str) -> bool {
    s.split('-')
        .all(|p| (3..=8).contains(&p.len()) && all_alnum(p))
}

fn is_language(s: &str) -> bool {
    matches!(s.len(), 2 | 3 | 5..=8) && all_alpha(s)
}

fn is_script(s: &str) -> bool {
    s.len() == 4 && all_alpha(s)
}

fn is_region(s: &str) -> bool {
    (s.len() == 2 && all_alpha(s)) || (s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
}

fn is_currency(s: &str) -> bool {
    s.len() == 3 && all_alpha(s)
}

fn is_variants(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let well_formed = parts.iter().all(|p| {
        all_alnum(p)
            && ((5..=8).contains(&p.len()) || (p.len() == 4 && p.as_bytes()[0].is_ascii_digit()))
    });
    let mut lower: Vec<String> = parts.iter().map(|p| p.to_ascii_lowercase()).collect();
    lower.sort();
    lower.dedup();
    well_formed && lower.len() == parts.len()
}

fn is_first_day_of_week(s: &str) -> bool {
    (s.len() == 1 && (b'0'..=b'7').contains(&s.as_bytes()[0])) || is_unicode_type(s)
}

fn is_unit(s: &str) -> bool {
    if UNITS.contains(&s) {
        return true;
    }
    let parts: Vec<&str> = s.split("-per-").collect();
    parts.len() == 2 && parts.iter().all(|p| UNITS.contains(p))
}

/// A statically known option value; `Unknown` covers objects, functions and non-static code.
#[derive(Clone, Debug, PartialEq)]
enum V {
    Unknown,
    Undefined,
    /// A well-known symbol, by description.
    Symbol(String),
    Prim(Static),
}

impl V {
    fn is_str(&self, s: &str) -> bool {
        matches!(self, V::Prim(Static::Str(v)) if v == s)
    }

    fn num(&self) -> Option<f64> {
        match self {
            V::Prim(Static::Num(n)) => Some(*n),
            _ => None,
        }
    }
}

fn is_well_known_symbol(path: &str) -> bool {
    path.strip_prefix("Symbol.").is_some_and(|name| {
        matches!(
            name,
            "asyncIterator"
                | "hasInstance"
                | "isConcatSpreadable"
                | "iterator"
                | "match"
                | "matchAll"
                | "replace"
                | "search"
                | "species"
                | "split"
                | "toPrimitive"
                | "toStringTag"
                | "unscopables"
        )
    })
}

fn primitive(cx: &Ctx<'_, '_>, id: NodeId) -> V {
    match static_value_control_flow(cx, id) {
        Some(Static::Builtin(p)) if is_well_known_symbol(&p) => V::Symbol(p),
        None
        | Some(Static::Object(_) | Static::Array(_) | Static::RegExp { .. } | Static::Builtin(_)) => {
            V::Unknown
        }
        Some(Static::Undefined) => V::Undefined,
        Some(v) => V::Prim(v),
    }
}

/// One plain property of the options object.
#[derive(Clone, Copy)]
struct Prop {
    /// The ESTree key: the expression inside `[...]` for a computed key.
    key: NodeId,
    value: NodeId,
    shorthand: bool,
    computed: bool,
}

fn static_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<(String, NodeId, bool)> {
    let ast = cx.ast();
    Some(match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => {
            (ast.atom(name).to_owned(), key, false)
        }
        Node::NumberLit { value } => (number_to_string(value.get()), key, false),
        Node::BigIntLit { value } => (ast.atom(value).trim_end_matches('n').to_owned(), key, false),
        Node::ComputedKey { expr } => {
            let expr = skip_parens(ast, expr);
            let name = match primitive(cx, expr) {
                V::Unknown | V::Symbol(_) => return None,
                V::Undefined => "undefined".to_owned(),
                V::Prim(p) => p.to_js_string()?,
            };
            (name, expr, true)
        }
        _ => return None,
    })
}

fn properties(cx: &Ctx<'_, '_>, object: NodeId) -> Option<Vec<(String, Prop)>> {
    let ast = cx.ast();
    let Node::ObjectLit { props } = *ast.node(object) else {
        return None;
    };
    let mut out: Vec<(String, Prop)> = Vec::new();
    for &p in ast.list(props) {
        let (name, prop) = match *ast.node(p) {
            Node::Property { key, value, .. } => {
                let (name, key, computed) = static_name(cx, key)?;
                if name == "__proto__" && !computed {
                    return None;
                }
                (
                    name,
                    Prop {
                        key,
                        value,
                        shorthand: false,
                        computed,
                    },
                )
            }
            Node::ShorthandProp {
                name, init: None, ..
            } => (
                ident_name(ast, name)?.to_owned(),
                Prop {
                    key: name,
                    value: name,
                    shorthand: true,
                    computed: false,
                },
            ),
            _ => return None,
        };
        if out.iter().any(|(n, _)| *n == name) {
            return None;
        }
        out.push((name, prop));
    }
    Some(out)
}

/// One edit apart (case-insensitively), an adjacent transposition counting as one.
fn is_similar_name(left: &str, right: &str) -> bool {
    let l: Vec<char> = left.to_lowercase().chars().collect();
    let r: Vec<char> = right.to_lowercase().chars().collect();
    if l == r {
        return true;
    }
    if l.len().abs_diff(r.len()) > 1 {
        return false;
    }
    let tail = |v: &[char], k: usize| -> Vec<char> { v.get(k..).unwrap_or(&[]).to_vec() };
    let mut i = 0;
    while i < l.len().min(r.len()) && l[i] == r[i] {
        i += 1;
    }
    if l.len() == r.len() {
        return tail(&l, i + 1) == tail(&r, i + 1)
            || (l.get(i) == r.get(i + 1)
                && l.get(i + 1) == r.get(i)
                && tail(&l, i + 2) == tail(&r, i + 2));
    }
    if l.len() > r.len() {
        tail(&l, i + 1) == tail(&r, i)
    } else {
        tail(&l, i) == tail(&r, i + 1)
    }
}

/// `Some(true)`/`Some(false)`, or `None` when unknown.
#[derive(Clone, Copy)]
struct Usage {
    fraction: Option<bool>,
    significant: Option<bool>,
}

struct Values(Vec<(String, V)>);

impl Values {
    fn get(&self, name: &str) -> Option<&V> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    /// `values.get(name) === undefined`.
    fn undefined(&self, name: &str) -> bool {
        matches!(self.get(name), None | Some(V::Undefined))
    }

    fn unknown(&self, name: &str) -> bool {
        matches!(self.get(name), Some(V::Unknown))
    }

    fn defined(&self, name: &str) -> bool {
        !self.undefined(name) && !self.unknown(name)
    }

    /// The value, or the string `fallback` when it is `undefined`.
    fn or_str(&self, name: &str, fallback: &str) -> V {
        match self.get(name) {
            None | Some(V::Undefined) => V::Prim(Static::Str(fallback.to_owned())),
            Some(v) => v.clone(),
        }
    }

    fn set(&mut self, name: &str, v: V) {
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = v,
            None => self.0.push((name.to_owned(), v)),
        }
    }

    fn number(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(V::num)
    }

    fn usage(&self) -> Usage {
        let unknown = Usage {
            fraction: None,
            significant: None,
        };
        let priority = self.or_str("roundingPriority", "auto");
        if priority == V::Unknown {
            return unknown;
        }
        if !priority.is_str("auto") {
            return Usage {
                fraction: Some(true),
                significant: Some(true),
            };
        }
        let has_significant = SIGNIFICANT.iter().any(|n| self.defined(n));
        if !has_significant && SIGNIFICANT.iter().any(|n| self.unknown(n)) {
            return unknown;
        }
        let has_fraction = FRACTION.iter().any(|n| self.defined(n));
        let notation = self.or_str("notation", "standard");
        if !has_significant
            && !has_fraction
            && (notation == V::Unknown
                || (notation.is_str("compact") && FRACTION.iter().any(|n| self.unknown(n))))
        {
            return Usage {
                significant: Some(false),
                fraction: None,
            };
        }
        Usage {
            significant: Some(has_significant),
            fraction: Some(
                !has_significant
                    && (has_fraction || (notation != V::Unknown && !notation.is_str("compact"))),
            ),
        }
    }
}

/// The normalized value, or what was expected.
fn normalize(value: &V, def: Def, name: &str) -> Result<V, String> {
    let p = match value {
        V::Undefined => return Ok(V::Undefined),
        V::Unknown => return Ok(V::Unknown),
        V::Symbol(_) => None,
        V::Prim(p) => Some(p),
    };
    match def {
        Def::Bool => {
            return Ok(V::Prim(Static::Bool(p.is_none_or(Static::truthy))));
        }
        Def::Range(min, max) => {
            let n = match p {
                None | Some(Static::BigInt(_)) => f64::NAN,
                Some(p) => p.to_number().unwrap_or(f64::NAN),
            };
            if !n.is_finite() || n < min || n > max {
                return Err(format!(
                    "a number between {} and {}",
                    number_to_string(min),
                    number_to_string(max)
                ));
            }
            let integer = n.floor();
            if name == "roundingIncrement" && !ROUNDING_INCREMENTS.contains(&integer) {
                let list: Vec<String> = ROUNDING_INCREMENTS
                    .iter()
                    .map(|n| number_to_string(*n))
                    .collect();
                return Err(format!("one of {}", list.join(", ")));
            }
            return Ok(V::Prim(Static::Num(integer)));
        }
        _ => {}
    }
    if name == "useGrouping"
        && let Some(p) = p
        && (*p == Static::Bool(true) || !p.truthy())
    {
        return Ok(V::Prim(Static::Bool(p.truthy())));
    }
    let Some(p) = p else {
        return Err("convertible to a string".to_owned());
    };
    let Some(s) = p.to_js_string() else {
        return Ok(V::Unknown);
    };
    let valid = match def {
        Def::Str => true,
        Def::List(list) => {
            if !list.contains(&s.as_str()) {
                let quoted: Vec<String> = list.iter().map(|v| format!("\"{v}\"")).collect();
                return Err(format!("one of {}", quoted.join(", ")));
            }
            true
        }
        Def::Shape(f) => f(&s),
        Def::Bool | Def::Range(..) => true,
    };
    if valid {
        Ok(V::Prim(Static::Str(s)))
    } else {
        Err("a well-formed identifier".to_owned())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Unknown,
    Invalid,
    Required,
    Conflicting,
    Ignored,
}

struct Problem {
    name: String,
    node: NodeId,
    kind: Kind,
    text: String,
    rename: Option<(String, Fix)>,
}

struct Problems<'p> {
    props: &'p [(String, Prop)],
    object: NodeId,
    list: Vec<Problem>,
}

impl Problems<'_> {
    fn add(&mut self, name: &str, kind: Kind, text: String) {
        if self.list.iter().any(|p| p.name == name) {
            return;
        }
        let node = self
            .props
            .iter()
            .find(|(n, _)| n == name)
            .map_or(self.object, |(_, p)| p.key);
        self.list.push(Problem {
            name: name.to_owned(),
            node,
            kind,
            text,
            rename: None,
        });
    }

    fn conflicting(&mut self, name: &str, reason: &str) {
        self.add(name, Kind::Conflicting, format!("`{name}` {reason}."));
    }

    fn ignored(&mut self, name: &str, reason: &str) {
        self.add(
            name,
            Kind::Ignored,
            format!("`{name}` is ignored {reason}."),
        );
    }

    fn required(&mut self, name: &str, required: &str, reason: &str) {
        self.add(
            name,
            Kind::Required,
            format!("The `{required}` option is required {reason}."),
        );
    }
}

fn rename_suggestion(
    cx: &Ctx<'_, '_>,
    name: &str,
    prop: Prop,
    defs: &[(&str, Def)],
    props: &[(String, Prop)],
) -> Option<(String, Fix)> {
    let mut matches = defs.iter().filter(|(d, _)| is_similar_name(name, d));
    let (replacement, _) = matches.next()?;
    if matches.next().is_some() || props.iter().any(|(n, _)| n == replacement) {
        return None;
    }
    let span = cx.span(prop.key);
    if has_comment_in(cx, span) {
        return None;
    }
    let key_text = cx.text(prop.key);
    let text = if prop.shorthand {
        format!("{replacement}: {key_text}")
    } else if prop.computed || !matches!(cx.node(prop.key), Node::Ident { .. }) {
        format!("'{replacement}'")
    } else {
        (*replacement).to_owned()
    };
    Some((
        format!("Rename `{name}` to `{replacement}`."),
        Fix::replace(span, text),
    ))
}

fn validate(values: &mut Values, problems: &mut Problems<'_>, defs: &[(&str, Def)], name: &str) {
    let Some(value) = values.get(name).cloned() else {
        return;
    };
    if value == V::Unknown {
        return;
    }
    let Some(&(_, def)) = defs.iter().find(|(d, _)| *d == name) else {
        return;
    };
    match normalize(&value, def, name) {
        Ok(v) => values.set(name, v),
        Err(expected) => {
            problems.add(name, Kind::Invalid, format!("`{name}` must be {expected}."));
            values.set(name, V::Unknown);
        }
    }
}

fn rounding_increment_conflict(values: &Values, usage: Usage) -> Option<&'static str> {
    let increment = match values.get("roundingIncrement") {
        None | Some(V::Undefined) => return None,
        Some(v) => v,
    };
    if *increment == V::Unknown || increment.num() == Some(1.0) {
        return None;
    }
    let priority = values.or_str("roundingPriority", "auto");
    if priority != V::Unknown && !priority.is_str("auto") {
        return Some("requires `roundingPriority` to be \"auto\"");
    }
    if usage.significant == Some(true) {
        return Some("cannot be combined with significant-digit rounding");
    }
    if usage.fraction == Some(false) {
        return Some("requires fraction-digit rounding instead of the default compact rounding");
    }
    if usage.fraction == Some(true)
        && let (Some(a), Some(b)) = (values.number(FRACTION[0]), values.number(FRACTION[1]))
        && a != b
    {
        return Some("requires equal `minimumFractionDigits` and `maximumFractionDigits`");
    }
    None
}

fn digit_combinations(values: &Values, problems: &mut Problems<'_>) {
    let usage = values.usage();
    for (names, active) in [(FRACTION, usage.fraction), (SIGNIFICANT, usage.significant)] {
        let [minimum, maximum] = names;
        if active == Some(true)
            && let (Some(a), Some(b)) = (values.number(minimum), values.number(maximum))
            && a > b
        {
            problems.conflicting(
                maximum,
                &format!("must be greater than or equal to `{minimum}`"),
            );
        }
    }
    let priority = values.or_str("roundingPriority", "auto");
    if let Some(reason) = rounding_increment_conflict(values, usage) {
        problems.conflicting("roundingIncrement", reason);
    }
    if usage.significant == Some(true) && priority.is_str("auto") {
        for name in FRACTION {
            if values.defined(name) {
                problems.ignored(name, "when significant-digit rounding takes precedence");
            }
        }
    }
    let notation = values.or_str("notation", "standard");
    if notation != V::Unknown && !notation.is_str("compact") && values.defined("compactDisplay") {
        problems.ignored("compactDisplay", "unless `notation` is \"compact\"");
    }
}

fn duration_options(values: &Values, problems: &mut Problems<'_>) {
    let base = values.or_str("style", "short");
    let mut previous = V::Prim(Static::Str(String::new()));
    let numeric_like = |v: &V| v.is_str("numeric") || v.is_str("2-digit");
    for unit in TIME_UNITS {
        let mut style = match values.get(unit) {
            None | Some(V::Undefined) => {
                if base.is_str("digital")
                    || numeric_like(&previous)
                    || previous.is_str("fractional")
                {
                    V::Prim(Static::Str("numeric".to_owned()))
                } else if base == V::Unknown || previous == V::Unknown {
                    V::Unknown
                } else {
                    base.clone()
                }
            }
            Some(v) => v.clone(),
        };
        let fractional_unit = matches!(unit, "milliseconds" | "microseconds" | "nanoseconds");
        if style.is_str("numeric") && fractional_unit {
            style = V::Prim(Static::Str("fractional".to_owned()));
        }
        let needs_fractional = previous.is_str("fractional") && !style.is_str("fractional");
        let needs_numeric =
            numeric_like(&previous) && !(numeric_like(&style) || style.is_str("fractional"));
        if style != V::Unknown && previous != V::Unknown && (needs_fractional || needs_numeric) {
            problems.conflicting(
                unit,
                "must continue the preceding numeric or fractional unit style",
            );
        }
        let display = format!("{unit}Display");
        if style.is_str("fractional") && values.get(&display).is_some_and(|v| v.is_str("always")) {
            problems.conflicting(&display, "cannot be \"always\" for a fractional unit");
        }
        previous = style;
    }
}

fn number_format_options(values: &Values, problems: &mut Problems<'_>) {
    let style = values.or_str("style", "decimal");
    let groups: [(&str, &[&str]); 2] = [
        ("currency", &["currency", "currencyDisplay", "currencySign"]),
        ("unit", &["unit", "unitDisplay"]),
    ];
    for (required, companions) in groups {
        if style.is_str(required) && values.undefined(required) {
            problems.required(
                "style",
                required,
                &format!("when `style` is \"{required}\""),
            );
        }
        if style != V::Unknown && !style.is_str(required) {
            for name in companions {
                if values.defined(name) {
                    problems.ignored(name, &format!("unless `style` is \"{required}\""));
                }
            }
        }
    }
}

fn display_names_options(values: &Values, problems: &mut Problems<'_>) {
    if values.undefined("type") {
        problems.required("type", "type", "for Intl.DisplayNames");
    } else if !values.unknown("type")
        && !values.get("type").is_some_and(|v| v.is_str("language"))
        && values.defined("languageDisplay")
    {
        problems.ignored("languageDisplay", "unless `type` is \"language\"");
    }
}

fn date_time_options(values: &Values, api: &str, problems: &mut Problems<'_>) {
    for (method, forbidden) in [
        ("toLocaleDateString", "timeStyle"),
        ("toLocaleTimeString", "dateStyle"),
    ] {
        if api.ends_with(&format!(".{method}")) && values.defined(forbidden) {
            problems.conflicting(forbidden, &format!("cannot be used with `{method}()`"));
        }
    }
    if let Some(style) = ["dateStyle", "timeStyle"]
        .into_iter()
        .find(|n| values.defined(n))
    {
        for name in DATE_COMPONENTS {
            if values.defined(name) {
                problems.conflicting(name, &format!("cannot be combined with `{style}`"));
            }
        }
        if values.defined("formatMatcher") {
            problems.ignored("formatMatcher", "when `dateStyle` or `timeStyle` is used");
        }
    }
    if values.defined("hour12") && values.defined("hourCycle") {
        problems.ignored("hourCycle", "when `hour12` is provided");
    }
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

fn check(cx: &mut Ctx<'_, '_>, args: &[NodeId], ctor: &str, api: &str) {
    let ast = cx.ast();
    let index = if api == "String.localeCompare" { 2 } else { 1 };
    if args
        .iter()
        .take(index + 1)
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return;
    }
    let Some(&argument) = args.get(index) else {
        return;
    };
    let object = unwrap_ts(ast, argument);
    let Some(props) = properties(cx, object) else {
        return;
    };
    let (own, digits) = definitions(ctor);
    let mut defs: Vec<(&str, Def)> = own.to_vec();
    if digits {
        defs.extend_from_slice(DIGIT_OPTIONS);
    }
    let mut values = Values(Vec::new());
    let mut problems = Problems {
        props: &props,
        object,
        list: Vec::new(),
    };
    for (name, prop) in &props {
        if !defs.iter().any(|(d, _)| d == name) {
            problems.add(
                name,
                Kind::Unknown,
                format!("`{name}` is not a recognized {api} option."),
            );
            let rename = rename_suggestion(cx, name, *prop, &defs, &props);
            if let Some(p) = problems.list.iter_mut().find(|p| p.name == *name) {
                p.rename = rename;
            }
            continue;
        }
        values.set(name, primitive(cx, prop.value));
    }
    let names: Vec<String> = values.0.iter().map(|(n, _)| n.clone()).collect();
    for name in &names {
        if !FRACTION.contains(&name.as_str()) && !SIGNIFICANT.contains(&name.as_str()) {
            validate(&mut values, &mut problems, &defs, name);
        }
    }
    if digits {
        if values.usage().significant == Some(true) {
            for name in SIGNIFICANT {
                validate(&mut values, &mut problems, &defs, name);
            }
        }
        if values.usage().fraction == Some(true) {
            for name in FRACTION {
                validate(&mut values, &mut problems, &defs, name);
            }
        }
        digit_combinations(&values, &mut problems);
    }
    match ctor {
        "NumberFormat" => number_format_options(&values, &mut problems),
        "DisplayNames" => display_names_options(&values, &mut problems),
        "DateTimeFormat" => date_time_options(&values, api, &mut problems),
        "DurationFormat" => duration_options(&values, &mut problems),
        _ => {}
    }
    for p in problems.list {
        let id = match p.kind {
            Kind::Unknown => "unknown",
            Kind::Invalid => "invalid",
            Kind::Required => "required",
            Kind::Conflicting => "conflicting",
            Kind::Ignored => "ignored",
        };
        let span = cx.span(p.node);
        let r = cx.report(span, p.text).id(id);
        if let Some((message, fix)) = p.rename {
            r.suggest("rename", message, fix);
        }
    }
}

fn string_annotation(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::KeywordType {
            kind: KeywordTypeKind::String,
        }
        | Node::StringLit { .. } => true,
        Node::TemplateLitType { parts } => ast.list(parts).len() <= 1,
        _ => false,
    }
}

static STRING_OBJECT: Checker = Checker {
    target_names: &["String"],
    target_ctors: &["String"],
    target_annotation: Some(string_annotation),
    class_heritage: false,
    nullish_in_mixed_union: true,
    ..Checker::DEFAULT
};

static DATE: Checker = Checker {
    target_names: &["Date"],
    target_ctors: &["Date"],
    class_heritage: false,
    nullish_in_mixed_union: true,
    ..Checker::DEFAULT
};

const TYPED_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

fn numeric_node(cx: &Ctx<'_, '_>, id: NodeId, _r: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::NumberLit { .. } | Node::BigIntLit { .. } => true,
        Node::Call { callee, .. } => match *ast.node(skip_parens(ast, callee)) {
            Node::Member { obj, prop, .. } => {
                matches!(ident_name(ast, prop), Some("from" | "of"))
                    && ident_name(ast, obj).is_some_and(|n| TYPED_ARRAYS.contains(&n))
            }
            _ => false,
        },
        _ => false,
    }
}

fn numeric_annotation(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::KeywordType {
            kind: KeywordTypeKind::Number | KeywordTypeKind::BigInt,
        }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. } => true,
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => matches!(
            ast.node(arg),
            Node::NumberLit { .. } | Node::BigIntLit { .. }
        ),
        _ => false,
    }
}

fn numeric_static(v: &Static, _ast: &Ast, _id: NodeId) -> Guess {
    if matches!(v, Static::Num(_) | Static::BigInt(_)) {
        Guess::Target
    } else {
        Guess::Unknown
    }
}

static NUMERIC_VALUE: Checker = Checker {
    target_names: &[
        "Number",
        "BigInt",
        "Int8Array",
        "Uint8Array",
        "Uint8ClampedArray",
        "Int16Array",
        "Uint16Array",
        "Int32Array",
        "Uint32Array",
        "Float16Array",
        "Float32Array",
        "Float64Array",
        "BigInt64Array",
        "BigUint64Array",
    ],
    target_calls: &["Number", "BigInt"],
    target_ctors: &[
        "Number",
        "Int8Array",
        "Uint8Array",
        "Uint8ClampedArray",
        "Int16Array",
        "Uint16Array",
        "Int32Array",
        "Uint32Array",
        "Float16Array",
        "Float32Array",
        "Float64Array",
        "BigInt64Array",
        "BigUint64Array",
    ],
    target_node: Some(numeric_node),
    target_annotation: Some(numeric_annotation),
    static_type: Some(numeric_static),
    class_heritage: false,
    nullish_in_mixed_union: true,
    ..Checker::DEFAULT
};

/// The constructor and API name a tracked `Intl` path stands for.
fn tracked(path: &str, construct: bool) -> Option<(&'static str, String)> {
    let rest = path.strip_prefix("Intl.")?;
    let (name, member) = match rest.split_once('.') {
        Some((n, m)) => (n, Some(m)),
        None => (rest, None),
    };
    let &ctor = CONSTRUCTORS.iter().find(|c| **c == name)?;
    match member {
        None if construct || matches!(ctor, "Collator" | "DateTimeFormat" | "NumberFormat") => {
            Some((ctor, path.to_owned()))
        }
        Some("supportedLocalesOf") if !construct && ctor != "Locale" => {
            Some(("supportedLocalesOf", path.to_owned()))
        }
        _ => None,
    }
}

impl Rule for NoInvalidIntlOptions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args, construct) = match *ast.node(id) {
            Node::New { callee, args, .. } => (callee, args, true),
            Node::Call { callee, args, .. } => (callee, args, false),
            _ => return,
        };
        let args = ast.list(args);
        if let Some(path) = global_path(cx, callee)
            && let Some((ctor, api)) = tracked(&path, construct)
        {
            check(cx, args, ctor, &api);
            return;
        }
        if construct {
            return;
        }
        let member = unwrap_ts(ast, callee);
        let receiver = match *ast.node(member) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            _ => return,
        };
        let Some(method) = static_property_name(cx, member) else {
            return;
        };
        match method.as_str() {
            "localeCompare" => {
                if is_string(cx, receiver) || STRING_OBJECT.is_target(cx, receiver) {
                    check(cx, args, "Collator", "String.localeCompare");
                }
            }
            "toLocaleString" if NUMERIC_VALUE.is_target(cx, receiver) => {
                check(cx, args, "NumberFormat", "Number.toLocaleString");
            }
            "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" => {
                if DATE.is_target(cx, receiver) {
                    check(cx, args, "DateTimeFormat", &format!("Date.{method}"));
                }
            }
            _ => {}
        }
    }
}
