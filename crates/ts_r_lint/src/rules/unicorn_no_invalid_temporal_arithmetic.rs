//! `unicorn/no-invalid-temporal-arithmetic`: Temporal method calls whose arguments are known
//! (statically, without side effects, through `const` bindings) to throw.
//!
//! A call `x.m(...)` (no spread argument, any member form) is checked when `m` is an
//! arithmetic method of a Temporal type and `x` is that type (`unicorn_types::Checker` on
//! `Temporal.<Type>`; a source is `new Temporal.T(..)`, `Temporal.T.from(..)`,
//! `Temporal.Instant.fromEpochMilliseconds/fromEpochNanoseconds(..)` or `Temporal.Now.*ISO()` /
//! `Temporal.Now.instant()`). Types are tried in the order Instant, Duration, PlainTime,
//! PlainDate, PlainYearMonth, PlainDateTime, ZonedDateTime; each has `add`/`subtract`, the
//! Duration also `round`/`total`, the others `since`/`until` and (not PlainDate or
//! PlainYearMonth) `round`. `Temporal.Duration.compare(a, b, options)` is checked by syntax.
//!
//! Durations are read from `new Temporal.Duration(...)`, `Temporal.Duration.from(x)`, an
//! object literal of plain properties (later keys win, `__proto__` or anything else gives up),
//! a `const` binding to one of those that isn't an object literal, or a string in the common
//! ISO subset `±PnYnMnWnDTnHnMn(.f)S`. A field is unknown unless it is a safe integer.
//! - `add`/`subtract`: an Instant must not get nonzero years..days, a Duration years..weeks
//!   (in the argument or in the receiver), a PlainYearMonth weeks or smaller units; `overflow`
//!   must be `constrain`/`reject` (not PlainTime). The Instant report suggests `hours: 24n`
//!   for a lone `days: n`.
//! - `round`/`total`/`since`/`until`: the options (an object of plain properties, `undefined`,
//!   or a string unit for `round`/`total`) are checked in order: units allowed for the type
//!   and method, `roundingMode`, `roundingIncrement` in 1..=1e9; a required unit; `largestUnit`
//!   not smaller than `smallestUnit` (with each method's defaults); the increment dividing the
//!   unit's maximum; `relativeTo` when a Duration involves calendar units. A missing unit
//!   suggests renaming `unit` <-> `smallestUnit` when that alone makes the call valid.
//! - `compare`: two fully known durations that differ and use calendar units need
//!   `relativeTo`.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::unwrap_expression;
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::has_comment_in;
use super::unicorn_types::{Checker, TargetNodeFn};
use super::unicorn_util::static_property_name;
use crate::ast_util::number_to_string;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-temporal-arithmetic",
        "Disallow statically known invalid Temporal arithmetic.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoInvalidTemporalArithmetic>,
};

#[derive(Debug, Default)]
struct NoInvalidTemporalArithmetic;

const UNITS: [&str; 10] = [
    "year",
    "month",
    "week",
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];
const FIELDS: [&str; 10] = [
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];
const ROUNDING_MODES: &[&str] = &[
    "ceil",
    "floor",
    "expand",
    "trunc",
    "halfCeil",
    "halfFloor",
    "halfExpand",
    "halfTrunc",
    "halfEven",
];
const TYPES: [&str; 7] = [
    "Instant",
    "Duration",
    "PlainTime",
    "PlainDate",
    "PlainYearMonth",
    "PlainDateTime",
    "ZonedDateTime",
];
const ARITHMETIC: &[&str] = &["add", "subtract", "round", "total", "since", "until"];

fn has_method(ty: &str, method: &str) -> bool {
    match method {
        "add" | "subtract" => true,
        "round" | "total" if ty == "Duration" => true,
        "since" | "until" => ty != "Duration",
        "round" => !matches!(ty, "Duration" | "PlainDate" | "PlainYearMonth"),
        _ => false,
    }
}

fn now_method(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "Instant" => "instant",
        "PlainTime" => "plainTimeISO",
        "PlainDate" => "plainDateISO",
        "PlainDateTime" => "plainDateTimeISO",
        "ZonedDateTime" => "zonedDateTimeISO",
        _ => return None,
    })
}

/// `(largest, smallest)` defaults of `since`/`until`.
fn difference_defaults(ty: &str) -> (&'static str, &'static str) {
    match ty {
        "Instant" => ("second", "nanosecond"),
        "PlainTime" => ("hour", "nanosecond"),
        "PlainDate" => ("day", "day"),
        "PlainYearMonth" => ("year", "month"),
        "PlainDateTime" => ("day", "nanosecond"),
        _ => ("hour", "nanosecond"),
    }
}

fn maximum_increment(unit: &str) -> Option<f64> {
    Some(match unit {
        "hour" => 24.0,
        "minute" => 60.0,
        "second" => 60.0,
        "millisecond" | "microsecond" | "nanosecond" => 1000.0,
        _ => return None,
    })
}

fn instant_increment(unit: &str) -> Option<f64> {
    Some(match unit {
        "hour" => 24.0,
        "minute" => 1440.0,
        "second" => 86_400.0,
        "millisecond" => 86_400_000.0,
        "microsecond" => 86_400_000_000.0,
        "nanosecond" => 86_400_000_000_000.0,
        _ => return None,
    })
}

/// `Temporal.<name>` as a plain member access.
fn is_temporal_type(ast: &Ast, id: NodeId, name: &str) -> bool {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, id))
    else {
        return false;
    };
    ident_name(ast, prop) == Some(name)
        && ident_name(ast, skip_parens(ast, obj)) == Some("Temporal")
}

fn no_spread(ast: &Ast, args: &[NodeId]) -> bool {
    !args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

fn is_temporal_source(ast: &Ast, id: NodeId, ty: &str) -> bool {
    let id = skip_parens(ast, id);
    if let Node::New { callee, .. } = *ast.node(id) {
        return is_temporal_type(ast, callee, ty);
    }
    let Some(call) = method_call(ast, id) else {
        return false;
    };
    if !call.plain() || !no_spread(ast, call.args) {
        return false;
    }
    let factory = call.name == "from"
        || (ty == "Instant"
            && matches!(call.name, "fromEpochMilliseconds" | "fromEpochNanoseconds"));
    let max = if matches!(ty, "Duration" | "Instant") {
        1
    } else {
        2
    };
    if factory && (1..=max).contains(&call.args.len()) {
        return is_temporal_type(ast, call.obj, ty);
    }
    now_method(ty).is_some_and(|m| m == call.name)
        && call.args.len() <= usize::from(ty != "Instant")
        && is_temporal_type(ast, call.obj, "Now")
}

fn source<const T: usize>(cx: &Ctx<'_, '_>, id: NodeId, _r: &dyn Fn(NodeId) -> bool) -> bool {
    is_temporal_source(cx.ast(), id, TYPES[T])
}

const fn checker(names: &'static [&'static str], f: TargetNodeFn) -> Checker {
    Checker {
        target_names: names,
        class_heritage: false,
        target_node: Some(f),
        ..Checker::DEFAULT
    }
}

static RECEIVERS: [Checker; 7] = [
    checker(&["Temporal.Instant"], source::<0>),
    checker(&["Temporal.Duration"], source::<1>),
    checker(&["Temporal.PlainTime"], source::<2>),
    checker(&["Temporal.PlainDate"], source::<3>),
    checker(&["Temporal.PlainYearMonth"], source::<4>),
    checker(&["Temporal.PlainDateTime"], source::<5>),
    checker(&["Temporal.ZonedDateTime"], source::<6>),
];

/// A statically known value: `Unknown` covers objects, functions and anything not static.
#[derive(Clone, Debug, PartialEq)]
enum Val {
    Unknown,
    Undefined,
    Prim(Static),
    /// A well-known symbol, by description (`Symbol.iterator`).
    Symbol(String),
}

impl Val {
    fn text(&self) -> Option<String> {
        match self {
            Val::Prim(v) => v.to_js_string(),
            Val::Symbol(d) => Some(format!("Symbol({d})")),
            _ => None,
        }
    }
}

fn primitive(cx: &Ctx<'_, '_>, id: Option<NodeId>) -> Val {
    let Some(id) = id else {
        return Val::Undefined;
    };
    match static_value_control_flow(cx, id) {
        Some(Static::Builtin(p)) if is_well_known_symbol(&p) => Val::Symbol(p),
        None
        | Some(Static::Object(_) | Static::Array(_) | Static::RegExp { .. } | Static::Builtin(_)) => {
            Val::Unknown
        }
        Some(Static::Undefined) => Val::Undefined,
        Some(v) => Val::Prim(v),
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

const MAX_SAFE: f64 = 9_007_199_254_740_991.0;

fn safe_integer(n: f64) -> Option<i64> {
    // `as` is exact here: the value is an integer within ±2^53.
    (n.is_finite() && n.trunc() == n && n.abs() <= MAX_SAFE).then_some(n as i64)
}

/// `None` is unknown.
fn integer(v: &Val) -> Option<i64> {
    match v {
        Val::Unknown | Val::Symbol(_) | Val::Prim(Static::BigInt(_)) => None,
        Val::Undefined => Some(0),
        Val::Prim(p) => safe_integer(p.to_number()?),
    }
}

/// One plain property of an object literal.
#[derive(Clone, Copy)]
struct Prop {
    whole: NodeId,
    key: NodeId,
    value: NodeId,
    shorthand: bool,
}

fn key_name(ast: &Ast, key: NodeId) -> Option<String> {
    Some(match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => ast.atom(name).to_owned(),
        Node::NumberLit { value } => number_to_string(value.get()),
        Node::BigIntLit { value } => ast.atom(value).trim_end_matches('n').to_owned(),
        _ => return None,
    })
}

/// The properties of an object literal (through TS wrappers), when every one is a plain
/// `key: value` or shorthand with a static name other than `__proto__`.
fn properties(ast: &Ast, id: NodeId) -> Option<Vec<(String, Prop)>> {
    let id = unwrap_expression(ast, id);
    let Node::ObjectLit { props } = *ast.node(id) else {
        return None;
    };
    let mut out: Vec<(String, Prop)> = Vec::new();
    for &p in ast.list(props) {
        let (name, prop) = match *ast.node(p) {
            Node::Property { key, value, .. } => (
                key_name(ast, key)?,
                Prop {
                    whole: p,
                    key,
                    value,
                    shorthand: false,
                },
            ),
            Node::ShorthandProp {
                name, init: None, ..
            } => (
                ident_name(ast, name)?.to_owned(),
                Prop {
                    whole: p,
                    key: name,
                    value: name,
                    shorthand: true,
                },
            ),
            _ => return None,
        };
        if name == "__proto__" {
            return None;
        }
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = prop,
            None => out.push((name, prop)),
        }
    }
    Some(out)
}

fn property(props: &[(String, Prop)], name: &str) -> Option<Prop> {
    props.iter().find(|(n, _)| n == name).map(|&(_, p)| p)
}

#[derive(Clone)]
struct Opt {
    name: String,
    value: Val,
    node: NodeId,
    prop: Option<Prop>,
}

type Options = Vec<Opt>;

fn options(cx: &Ctx<'_, '_>, id: Option<NodeId>, shorthand: Option<&str>) -> Option<Options> {
    if let Some(props) = id.and_then(|id| properties(cx.ast(), id)) {
        return Some(
            props
                .into_iter()
                .map(|(name, p)| Opt {
                    name,
                    value: primitive(cx, Some(p.value)),
                    node: p.value,
                    prop: Some(p),
                })
                .collect(),
        );
    }
    let value = primitive(cx, id);
    if value == Val::Undefined {
        return Some(Vec::new());
    }
    match (shorthand, &value, id) {
        (Some(name), Val::Prim(Static::Str(_)), Some(node)) => Some(vec![Opt {
            name: name.to_owned(),
            value,
            node,
            prop: None,
        }]),
        _ => None,
    }
}

fn get<'o>(options: &'o Options, name: &str) -> Option<&'o Opt> {
    options.iter().find(|o| o.name == name)
}

/// The option's value; an absent option reads as `undefined`.
fn option(options: &Options, name: &str) -> Val {
    get(options, name).map_or(Val::Undefined, |o| o.value.clone())
}

type Duration = [Option<i64>; 10];

/// The common ISO subset `[+-]P[nY][nM][nW][nD][T[nH][nM][n[.f]S]]`.
fn string_duration(s: &str) -> Option<Duration> {
    let b = s.as_bytes();
    let mut i = 0;
    let negative = match b.first() {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    if b.get(i) != Some(&b'P') {
        return None;
    }
    i += 1;
    let mut parts: [Option<&str>; 8] = [None; 8];
    let digits = |i: &mut usize| -> Option<&str> {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        (*i > start).then(|| &s[start..*i])
    };
    let mut next = 0;
    while i < b.len() && b[i] != b'T' {
        let d = digits(&mut i)?;
        let slot = match b.get(i)? {
            b'Y' => 0,
            b'M' => 1,
            b'W' => 2,
            b'D' => 3,
            _ => return None,
        };
        if slot < next {
            return None;
        }
        parts[slot] = Some(d);
        next = slot + 1;
        i += 1;
    }
    let has_t = i < b.len();
    if has_t {
        i += 1;
        let mut next = 4;
        while i < b.len() {
            let d = digits(&mut i)?;
            let mut fraction = None;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let f = digits(&mut i)?;
                if f.len() > 9 || b.get(i) != Some(&b'S') {
                    return None;
                }
                fraction = Some(f);
            }
            let slot = match b.get(i)? {
                b'H' => 4,
                b'M' => 5,
                b'S' => 6,
                _ => return None,
            };
            if slot < next {
                return None;
            }
            parts[slot] = Some(d);
            parts[7] = fraction;
            next = slot + 1;
            i += 1;
        }
    }
    if parts.iter().all(Option::is_none) || (has_t && parts[4..].iter().all(Option::is_none)) {
        return None;
    }
    let sign = if negative { -1 } else { 1 };
    let mut out: Duration = [Some(0); 10];
    for (slot, part) in out.iter_mut().zip(&parts[..7]) {
        let v = match part {
            None => 0,
            Some(d) => safe_integer(d.parse::<f64>().ok()?)?,
        };
        *slot = Some(v * sign);
    }
    let mut fraction = parts[7].unwrap_or("").to_owned();
    while fraction.len() < 9 {
        fraction.push('0');
    }
    for (k, start) in [0, 3, 6].into_iter().enumerate() {
        let v: i64 = fraction[start..start + 3].parse().ok()?;
        out[7 + k] = Some(v * sign);
    }
    Some(out)
}

fn duration(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<Duration> {
    let ast = cx.ast();
    let id = unwrap_expression(ast, id);
    if depth > 64 {
        return None;
    }
    match *ast.node(id) {
        Node::Ident { .. } => {
            let init = const_initializer(cx, id)?;
            if matches!(
                ast.node(unwrap_expression(ast, init)),
                Node::ObjectLit { .. }
            ) {
                return None;
            }
            return duration(cx, init, depth + 1);
        }
        Node::New { callee, args, .. } if is_temporal_type(ast, callee, "Duration") => {
            let args = ast.list(args);
            if !no_spread(ast, args) {
                return None;
            }
            let mut out: Duration = [None; 10];
            for (k, slot) in out.iter_mut().enumerate() {
                *slot = integer(&primitive(cx, args.get(k).copied()));
            }
            return Some(out);
        }
        _ => {}
    }
    if let Some(call) = method_call(ast, id)
        && call.name == "from"
        && call.plain()
        && call.args.len() == 1
        && no_spread(ast, call.args)
        && is_temporal_type(ast, call.obj, "Duration")
    {
        return duration(cx, call.args[0], depth + 1);
    }
    if let Some(props) = properties(ast, id) {
        let mut out: Duration = [None; 10];
        for (slot, field) in out.iter_mut().zip(FIELDS) {
            let value = property(&props, field).map(|p| p.value);
            *slot = integer(&primitive(cx, value));
        }
        return Some(out);
    }
    match primitive(cx, Some(id)) {
        Val::Prim(Static::Str(s)) => string_duration(&s),
        _ => None,
    }
}

fn known_nonzero(v: &Option<i64>) -> bool {
    matches!(v, Some(n) if *n != 0)
}

fn has_calendar_units(fields: Option<&Duration>) -> bool {
    fields.is_some_and(|f| f[..3].iter().any(known_nonzero))
}

/// A unit: `Absent` is `undefined`, `Unknown` a value not known statically.
#[derive(Clone, Debug, PartialEq)]
enum Unit {
    Absent,
    Unknown,
    Name(String),
}

impl Unit {
    fn is(&self, name: &str) -> bool {
        matches!(self, Unit::Name(n) if n == name)
    }

    /// `units.indexOf(unit)`.
    fn index(&self) -> i64 {
        match self {
            Unit::Name(n) => UNITS.iter().position(|u| u == n).map_or(-1, |p| p as i64),
            _ => -1,
        }
    }

    fn is_calendar(&self) -> bool {
        (0..3).contains(&self.index())
    }
}

fn unit_at(index: i64) -> Unit {
    usize::try_from(index)
        .ok()
        .and_then(|i| UNITS.get(i))
        .map_or(Unit::Absent, |u| Unit::Name((*u).to_owned()))
}

fn largest_duration_unit(fields: Option<&Duration>) -> Unit {
    let Some(fields) = fields else {
        return Unit::Unknown;
    };
    for (k, v) in fields.iter().enumerate() {
        match v {
            None => return Unit::Unknown,
            Some(0) => {}
            Some(_) => return Unit::Name(UNITS[k].to_owned()),
        }
    }
    Unit::Name("nanosecond".to_owned())
}

fn normalize(v: Val) -> Unit {
    match v {
        Val::Undefined => Unit::Absent,
        Val::Unknown => Unit::Unknown,
        v => match v.text() {
            Some(name) => match FIELDS.iter().position(|f| *f == name) {
                Some(k) => Unit::Name(UNITS[k].to_owned()),
                None => Unit::Name(name),
            },
            None => Unit::Unknown,
        },
    }
}

fn larger(first: &Unit, second: &Unit) -> Unit {
    if *first == Unit::Unknown || *second == Unit::Unknown {
        return Unit::Unknown;
    }
    unit_at(first.index().min(second.index()))
}

fn allowed_units(ty: &str, method: &str) -> &'static [&'static str] {
    match ty {
        "Duration" => &UNITS,
        "Instant" | "PlainTime" => &UNITS[4..],
        _ if method == "round" => &UNITS[3..],
        "PlainDate" => &UNITS[..4],
        "PlainYearMonth" => &UNITS[..2],
        _ => &UNITS,
    }
}

/// `None` is unknown.
fn rounding_increment(options: &Options) -> Option<f64> {
    match option(options, "roundingIncrement") {
        Val::Unknown => None,
        Val::Undefined => Some(1.0),
        Val::Prim(Static::BigInt(_)) | Val::Symbol(_) => Some(f64::NAN),
        Val::Prim(p) => Some(p.to_number().unwrap_or(f64::NAN).trunc()),
    }
}

struct Op<'o> {
    ty: &'static str,
    method: &'static str,
    options: &'o Options,
    fields: Option<Duration>,
}

#[derive(Clone, Copy, PartialEq)]
enum Msg {
    InstantCalendar,
    DurationCalendar,
    YearMonth,
    RelativeTo,
    InvalidOption,
    MissingUnit,
    UnitOrder,
    InvalidIncrement,
}

struct Problem {
    node: NodeId,
    msg: Msg,
    option: &'static str,
    required: &'static str,
}

/// `(message, option)` of the first option that is invalid on its own.
fn individual_problem(op: &Op<'_>) -> Option<(Msg, &'static str)> {
    let keys: &[&'static str] = if op.method == "total" {
        &["unit"]
    } else if op.ty == "Duration" || op.method != "round" {
        &["largestUnit", "smallestUnit"]
    } else {
        &["smallestUnit"]
    };
    let allowed = allowed_units(op.ty, op.method);
    for &key in keys {
        if let Unit::Name(unit) = normalize(option(op.options, key))
            && !(key == "largestUnit" && unit == "auto")
            && !allowed.contains(&unit.as_str())
        {
            return Some((Msg::InvalidOption, key));
        }
    }
    if op.method == "total" {
        return None;
    }
    if let Some(mode) = option(op.options, "roundingMode").text()
        && !ROUNDING_MODES.contains(&mode.as_str())
    {
        return Some((Msg::InvalidOption, "roundingMode"));
    }
    if let Some(inc) = rounding_increment(op.options)
        && (!inc.is_finite() || !(1.0..=1_000_000_000.0).contains(&inc))
    {
        return Some((Msg::InvalidIncrement, "roundingIncrement"));
    }
    None
}

fn rounding_units(op: &Op<'_>) -> (Unit, Unit) {
    let key = if op.method == "total" {
        "unit"
    } else {
        "smallestUnit"
    };
    let mut smallest = normalize(option(op.options, key));
    let mut largest = normalize(option(op.options, "largestUnit"));
    let unset = |u: &Unit| *u == Unit::Absent || u.is("auto");
    if matches!(op.method, "since" | "until") {
        let (dl, ds) = difference_defaults(op.ty);
        if smallest == Unit::Absent {
            smallest = Unit::Name(ds.to_owned());
        }
        if unset(&largest) {
            largest = larger(&Unit::Name(dl.to_owned()), &smallest);
        }
    } else if op.ty == "Duration" && op.method == "round" {
        if smallest == Unit::Absent {
            smallest = Unit::Name("nanosecond".to_owned());
        }
        if unset(&largest) {
            largest = larger(&largest_duration_unit(op.fields.as_ref()), &smallest);
        }
    }
    (smallest, largest)
}

fn invalid_increment(op: &Op<'_>, increment: Option<f64>, smallest: &Unit, largest: &Unit) -> bool {
    let Some(inc) = increment else {
        return false;
    };
    if op.method == "total" || *smallest == Unit::Unknown {
        return false;
    }
    let name = match smallest {
        Unit::Name(n) => n.as_str(),
        _ => "",
    };
    let mut maximum = maximum_increment(name);
    let mut inclusive = false;
    if op.ty == "Instant" && op.method == "round" {
        maximum = instant_increment(name);
        inclusive = true;
    } else if op.method == "round" && op.ty != "Duration" && name == "day" {
        maximum = Some(1.0);
        inclusive = true;
    }
    if let Some(max) = maximum
        && (inc > max || (!inclusive && inc == max) || max % inc != 0.0)
    {
        return true;
    }
    op.ty == "Duration"
        && op.method == "round"
        && UNITS[..4].contains(&name)
        && inc > 1.0
        && *largest != Unit::Unknown
        && largest != smallest
}

fn required_unit(op: &Op<'_>) -> Option<&'static str> {
    if op.method == "total" && option(op.options, "unit") == Val::Undefined {
        return Some("`unit`");
    }
    if op.method == "round" && option(op.options, "smallestUnit") == Val::Undefined {
        if op.ty != "Duration" {
            return Some("`smallestUnit`");
        }
        if option(op.options, "largestUnit") == Val::Undefined {
            return Some("`smallestUnit` or `largestUnit`");
        }
    }
    None
}

fn options_problem(op: &Op<'_>, property: NodeId) -> Option<Problem> {
    let problem = |msg: Msg, option: &'static str, required: &'static str| Problem {
        node: get(op.options, option).map_or(property, |o| o.node),
        msg,
        option,
        required,
    };
    if let Some((msg, option)) = individual_problem(op) {
        return Some(problem(msg, option, ""));
    }
    if let Some(required) = required_unit(op) {
        return Some(Problem {
            node: property,
            msg: Msg::MissingUnit,
            option: "",
            required,
        });
    }
    let (smallest, largest) = rounding_units(op);
    let duration_round = op.ty == "Duration" && op.method == "round";
    let difference = matches!(op.method, "since" | "until");
    if (difference || duration_round)
        && largest != Unit::Unknown
        && smallest != Unit::Unknown
        && largest.index() > smallest.index()
    {
        return Some(problem(Msg::UnitOrder, "largestUnit", ""));
    }
    if invalid_increment(op, rounding_increment(op.options), &smallest, &largest) {
        return Some(problem(Msg::InvalidIncrement, "roundingIncrement", ""));
    }
    if op.ty == "Duration"
        && option(op.options, "relativeTo") == Val::Undefined
        && (has_calendar_units(op.fields.as_ref())
            || smallest.is_calendar()
            || (duration_round && largest.is_calendar()))
    {
        return Some(problem(Msg::RelativeTo, "relativeTo", ""));
    }
    None
}

struct Suggestion {
    message_id: &'static str,
    message: String,
    fix: Fix,
}

fn unit_key_suggestion(cx: &Ctx<'_, '_>, op: &Op<'_>, property: NodeId) -> Option<Suggestion> {
    let (expected, actual) = if op.method == "total" {
        ("unit", "smallestUnit")
    } else {
        ("smallestUnit", "unit")
    };
    let entry = get(op.options, actual)?;
    let prop = entry.prop?;
    if get(op.options, expected).is_some() {
        return None;
    }
    let mut corrected = op.options.clone();
    corrected.push(Opt {
        name: expected.to_owned(),
        ..entry.clone()
    });
    if matches!(option(&corrected, expected), Val::Unknown | Val::Undefined) {
        return None;
    }
    let fixed = Op {
        options: &corrected,
        fields: op.fields,
        ..*op
    };
    if options_problem(&fixed, property).is_some() {
        return None;
    }
    let text = if prop.shorthand {
        format!("{expected}: {actual}")
    } else {
        expected.to_owned()
    };
    Some(Suggestion {
        message_id: "correct-unit-key",
        message: format!("Use `{expected}` instead of `{actual}`."),
        fix: Fix::replace(cx.span(prop.key), text),
    })
}

fn elapsed_days_suggestion(cx: &Ctx<'_, '_>, argument: NodeId) -> Option<Suggestion> {
    let ast = cx.ast();
    let props = properties(ast, argument)?;
    let prop = property(&props, "days")?;
    let Node::ObjectLit { props: list } = *ast.node(unwrap_expression(ast, argument)) else {
        return None;
    };
    let days = integer(&primitive(cx, Some(prop.value)))?;
    let hours = days.checked_mul(24)?;
    if ast.list(list).len() != 1
        || days == 0
        || hours.unsigned_abs() > MAX_SAFE as u64
        || has_comment_in(cx, cx.span(prop.value))
    {
        return None;
    }
    let fix = if prop.shorthand {
        Fix::replace(cx.span(prop.whole), format!("hours: {hours}"))
    } else {
        Fix::replace(cx.span(prop.key), "hours").and(cx.span(prop.value), hours.to_string())
    };
    Some(Suggestion {
        message_id: "elapsed-days",
        message: "Treat each day as 24 elapsed hours.".to_owned(),
        fix,
    })
}

struct CallInfo<'a> {
    obj: NodeId,
    property: NodeId,
    args: &'a [NodeId],
}

fn calendar_addition_problem(
    cx: &Ctx<'_, '_>,
    call: &CallInfo<'_>,
    ty: &'static str,
) -> Option<Problem> {
    if ty == "PlainTime" {
        return None;
    }
    let ast = cx.ast();
    if let Some(opts) = options(cx, call.args.get(1).copied(), None)
        && let Some(overflow) = option(&opts, "overflow").text()
        && !matches!(overflow.as_str(), "constrain" | "reject")
        && let Some(o) = get(&opts, "overflow")
    {
        return Some(Problem {
            node: o.node,
            msg: Msg::InvalidOption,
            option: "overflow",
            required: "",
        });
    }
    if ty != "PlainYearMonth" {
        return None;
    }
    let argument = duration(cx, call.args[0], 0)?;
    let k = (2..10).find(|&k| known_nonzero(&argument[k]))?;
    let node = properties(ast, call.args[0])
        .and_then(|p| property(&p, FIELDS[k]))
        .map_or(call.args[0], |p| p.value);
    Some(Problem {
        node,
        msg: Msg::YearMonth,
        option: "",
        required: "",
    })
}

fn addition_problem(
    cx: &Ctx<'_, '_>,
    call: &CallInfo<'_>,
    ty: &'static str,
) -> Option<(Problem, Option<Suggestion>)> {
    let &first = call.args.first()?;
    if !matches!(ty, "Instant" | "Duration") {
        return calendar_addition_problem(cx, call, ty).map(|p| (p, None));
    }
    let argument = duration(cx, first, 0);
    let fields = if ty == "Duration" {
        duration(cx, call.obj, 0)
    } else {
        None
    };
    let limit = if ty == "Instant" { 4 } else { 3 };
    let forbidden = argument.and_then(|a| (0..limit).find(|&k| known_nonzero(&a[k])));
    if forbidden.is_none() && !has_calendar_units(fields.as_ref()) {
        return None;
    }
    let node = match forbidden {
        None => call.property,
        Some(k) => properties(cx.ast(), first)
            .and_then(|p| property(&p, FIELDS[k]))
            .map_or(first, |p| p.value),
    };
    let msg = if ty == "Instant" {
        Msg::InstantCalendar
    } else {
        Msg::DurationCalendar
    };
    let suggestion = if ty == "Instant" {
        elapsed_days_suggestion(cx, first)
    } else {
        None
    };
    Some((
        Problem {
            node,
            msg,
            option: "",
            required: "",
        },
        suggestion,
    ))
}

fn arithmetic_problem(
    cx: &Ctx<'_, '_>,
    call: &CallInfo<'_>,
    ty: &'static str,
    method: &'static str,
) -> Option<(Problem, Option<Suggestion>)> {
    if matches!(method, "add" | "subtract") {
        return addition_problem(cx, call, ty);
    }
    let difference = matches!(method, "since" | "until");
    let shorthand = if method == "total" {
        "unit"
    } else {
        "smallestUnit"
    };
    let opts = options(
        cx,
        call.args.get(usize::from(difference)).copied(),
        (!difference).then_some(shorthand),
    )?;
    let fields = if ty == "Duration" {
        duration(cx, call.obj, 0)
    } else {
        None
    };
    let op = Op {
        ty,
        method,
        options: &opts,
        fields,
    };
    let problem = options_problem(&op, call.property)?;
    let suggestion = if problem.msg == Msg::MissingUnit {
        unit_key_suggestion(cx, &op, call.property)
    } else {
        None
    };
    Some((problem, suggestion))
}

fn compare_problem(cx: &Ctx<'_, '_>, call: &CallInfo<'_>) -> Option<Problem> {
    if call.args.len() < 2 {
        return None;
    }
    let opts = options(cx, call.args.get(2).copied(), None)?;
    if option(&opts, "relativeTo") != Val::Undefined {
        return None;
    }
    let first = duration(cx, call.args[0], 0)?;
    let second = duration(cx, call.args[1], 0)?;
    if first.contains(&None) || second.contains(&None) {
        return None;
    }
    (first != second && (has_calendar_units(Some(&first)) || has_calendar_units(Some(&second))))
        .then(|| Problem {
            node: get(&opts, "relativeTo").map_or(call.property, |o| o.node),
            msg: Msg::RelativeTo,
            option: "",
            required: "",
        })
}

fn message(p: &Problem, ty: &str, method: &str) -> (&'static str, String) {
    match p.msg {
        Msg::InstantCalendar => (
            "instant-calendar-unit",
            format!(
                "`Temporal.Instant.{method}()` does not support nonzero years, months, weeks, or days. Use elapsed time units or calendar arithmetic on a `Temporal.ZonedDateTime`."
            ),
        ),
        Msg::DurationCalendar => (
            "duration-calendar-unit",
            format!(
                "`Temporal.Duration.{method}()` does not support nonzero years, months, or weeks. Use date arithmetic relative to a starting point."
            ),
        ),
        Msg::YearMonth => (
            "year-month-unit",
            format!(
                "`Temporal.PlainYearMonth.{method}()` does not support nonzero weeks, days, or time units. Use years or months, or date arithmetic on a `Temporal.PlainDate`."
            ),
        ),
        Msg::RelativeTo => (
            "relative-to",
            format!("`Temporal.Duration.{method}()` requires `relativeTo` for calendar units."),
        ),
        Msg::InvalidOption => (
            "invalid-option",
            format!("Invalid `{}` for `Temporal.{ty}.{method}()`.", p.option),
        ),
        Msg::MissingUnit => (
            "missing-unit",
            format!("`Temporal.{ty}.{method}()` requires {}.", p.required),
        ),
        Msg::UnitOrder => (
            "unit-order",
            "`largestUnit` must be at least as large as `smallestUnit`.".to_owned(),
        ),
        Msg::InvalidIncrement => (
            "invalid-increment",
            format!(
                "Invalid `roundingIncrement` for `Temporal.{ty}.{method}()` and the selected units."
            ),
        ),
    }
}

impl Rule for NoInvalidTemporalArithmetic {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(id) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let (obj, property) = match *ast.node(callee) {
            Node::Member { obj, prop, .. } => (obj, prop),
            Node::Index { obj, index, .. } => (obj, index),
            _ => return,
        };
        let args = ast.list(args);
        if !no_spread(ast, args) {
            return;
        }
        let Some(name) = static_property_name(cx, callee) else {
            return;
        };
        let call = CallInfo {
            obj,
            property,
            args,
        };
        let (problem, suggestion, ty, method) =
            if name == "compare" && is_temporal_type(ast, obj, "Duration") {
                let Some(p) = compare_problem(cx, &call) else {
                    return;
                };
                (p, None, "Duration", "compare")
            } else {
                let Some(&method) = ARITHMETIC.iter().find(|m| **m == name) else {
                    return;
                };
                let Some(k) = (0..TYPES.len())
                    .find(|&k| has_method(TYPES[k], method) && RECEIVERS[k].is_target(cx, obj))
                else {
                    return;
                };
                let ty = TYPES[k];
                let Some((p, s)) = arithmetic_problem(cx, &call, ty, method) else {
                    return;
                };
                (p, s, ty, method)
            };
        let (message_id, text) = message(&problem, ty, method);
        let span = cx.span(problem.node);
        let r = cx.report(span, text).id(message_id);
        if let Some(s) = suggestion {
            r.suggest(s.message_id, s.message, s.fix);
        }
    }
}
