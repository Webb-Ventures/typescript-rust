//! `unicorn/prefer-temporal-conversion`: building a `Temporal.Instant`, `PlainDateTime`,
//! `PlainDate` or `PlainTime` from a `ZonedDateTime` (or, for the date and time, a
//! `PlainDateTime`) instead of calling its `.to<Target>()`.
//!
//! The constructions are `new Temporal.<T>(...)` and `Temporal.<T>.from(x)` (plus
//! `Temporal.Instant.fromEpochMilliseconds/Nanoseconds(x)`), non-optional, no spread, on the
//! bare `Temporal` name. The source is found from:
//!
//! - `.from(s.toString())` / `.from(s.toJSON())` (suggestion only for `Instant`, which
//!   would lose the time zone's offset ambiguity), or `.from(s)` with `s` not an object
//!   literal;
//! - for `Instant`, `s.epochNanoseconds` (fixed) or, via `fromEpochMilliseconds`,
//!   `s.epochMilliseconds` (suggestion: it drops sub-millisecond precision);
//! - otherwise every field of the target read from one side-effect-free receiver: the
//!   constructor's positional arguments (an extra one is `calendar`) or a plain object
//!   literal's keys, `monthCode` standing for `month` and `calendar` read as `calendarId`.
//!   With date fields this only fixes `.from({...})` with `calendar`; other date
//!   reconstructions are suggestions (the calendar would change).
//!
//! The source must be a `Temporal.<Source>` value (unicorn's type guess: annotations,
//! `new Temporal.X(...)`, `Temporal.X.from(a, b?)`, `Temporal.Now.zonedDateTimeISO(x?)` /
//! `plainDateTimeISO(x?)`). Comments outside the source block the fix.

use super::prelude::*;
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{
    is_same_reference, member_object_text, needs_semicolon, paren_span, would_remove_comments,
};
use super::unicorn_types::Checker;
use super::unicorn_util::{in_unparenthesized_new_callee, skip_wrappers, space_around_keyword};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-temporal-conversion",
        "Prefer direct Temporal conversion methods.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferTemporalConversion>,
};

#[derive(Debug, Default)]
struct PreferTemporalConversion;

const DATE_FIELDS: &[&str] = &["year", "month", "day"];
const TIME_FIELDS: &[&str] = &[
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];
const DATE_TIME_FIELDS: &[&str] = &[
    "year",
    "month",
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];

fn name_is(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    matches!(cx.node(id), Node::Ident { .. }) && ident_name(cx.ast(), id) == Some(name)
}

/// `Temporal.<name>` (non-optional).
fn is_temporal_type(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    matches!(
        *ast.node(skip_parens(ast, id)),
        Node::Member { obj, optional: false, prop }
            if name_is(cx, skip_parens(ast, obj), "Temporal") && name_is(cx, prop, name)
    )
}

/// A plain `<obj>.<method>(...)` with at most `max` arguments and no spread among them:
/// the receiver and arguments.
fn plain_method<'a>(
    cx: &Ctx<'a, '_>,
    call: NodeId,
    methods: &[&str],
    max: usize,
) -> Option<(NodeId, &'a str, &'a [NodeId])> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let name = ident_name(ast, prop)?;
    let args = ast.list(args);
    (matches!(ast.node(prop), Node::Ident { .. })
        && methods.contains(&name)
        && args.len() <= max
        && args
            .iter()
            .all(|&a| !matches!(ast.node(a), Node::Spread { .. })))
    .then_some((obj, name, args))
}

fn is_temporal_source(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Node::New { callee, .. } = *ast.node(id) {
        return is_temporal_type(cx, callee, name);
    }
    if let Some((obj, _, args)) = plain_method(cx, id, &["from"], 2)
        && !args.is_empty()
    {
        return is_temporal_type(cx, obj, name);
    }
    let now = if name == "ZonedDateTime" {
        "zonedDateTimeISO"
    } else {
        "plainDateTimeISO"
    };
    plain_method(cx, id, &[now], 1).is_some_and(|(obj, _, _)| is_temporal_type(cx, obj, "Now"))
}

fn zoned_node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    is_temporal_source(cx, id, "ZonedDateTime")
}

fn zoned_or_plain_node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    is_temporal_source(cx, id, "ZonedDateTime") || is_temporal_source(cx, id, "PlainDateTime")
}

static ZONED: Checker = Checker {
    target_names: &["Temporal.ZonedDateTime"],
    class_heritage: false,
    target_node: Some(zoned_node),
    ..Checker::DEFAULT
};

static ZONED_OR_PLAIN: Checker = Checker {
    target_names: &["Temporal.ZonedDateTime", "Temporal.PlainDateTime"],
    class_heritage: false,
    target_node: Some(zoned_or_plain_node),
    ..Checker::DEFAULT
};

/// `<x>.<property>` (through TS wrappers, non-optional): `x`.
fn property_of(cx: &Ctx<'_, '_>, id: NodeId, property: &str) -> Option<NodeId> {
    match *cx.node(skip_wrappers(cx.ast(), id)) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } if name_is(cx, prop, property) => Some(obj),
        _ => None,
    }
}

struct Conversion {
    source: NodeId,
    autofix: bool,
}

/// The `(field, value)` pairs of the arguments or of the object literal.
fn field_entries(cx: &Ctx<'_, '_>, node: NodeId, fields: &[&str]) -> Option<Vec<(String, NodeId)>> {
    let ast = cx.ast();
    match *ast.node(node) {
        Node::New { args, .. } => Some(
            ast.list(args)
                .iter()
                .enumerate()
                .map(|(i, &v)| (fields.get(i).copied().unwrap_or("calendar").to_owned(), v))
                .collect(),
        ),
        Node::Call { args, .. } => {
            let &[arg] = ast.list(args) else {
                return None;
            };
            let Node::ObjectLit { props } = *ast.node(skip_wrappers(ast, arg)) else {
                return None;
            };
            let mut out = Vec::new();
            for &p in ast.list(props) {
                let Node::Property { key, value, .. } = *ast.node(p) else {
                    return None;
                };
                let name = match *ast.node(key) {
                    Node::Ident { .. } => ident_name(ast, key)?.to_owned(),
                    Node::StringLit { value } => ast.atom(value).to_owned(),
                    Node::NumberLit { value } => value.get().to_string(),
                    _ => return None,
                };
                out.push((name, value));
            }
            Some(out)
        }
        _ => None,
    }
}

fn field_reconstruction(cx: &Ctx<'_, '_>, node: NodeId, fields: &[&str]) -> Option<Conversion> {
    let entries = field_entries(cx, node, fields)?;
    let has_date = fields.contains(&"year");
    let is_constructor = matches!(cx.node(node), Node::New { .. });
    let has_calendar = entries.iter().any(|(n, _)| n == "calendar");
    let mut remaining: Vec<&str> = fields.to_vec();
    if has_date && has_calendar {
        remaining.push("calendar");
    }
    let mut source: Option<NodeId> = None;
    for (name, value) in &entries {
        let field = if name == "monthCode" { "month" } else { name };
        let at = remaining.iter().position(|f| *f == field)?;
        remaining.swap_remove(at);
        let property = if name == "calendar" {
            "calendarId"
        } else {
            name
        };
        let receiver = property_of(cx, *value, property)?;
        if has_side_effect(cx.ast(), receiver)
            || source.is_some_and(|s| !is_same_reference(cx, s, receiver))
        {
            return None;
        }
        source.get_or_insert(receiver);
    }
    if !remaining.is_empty() {
        return None;
    }
    Some(Conversion {
        source: source?,
        autofix: !has_date || (!is_constructor && has_calendar),
    })
}

fn conversion(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    method: Option<&str>,
    target: &str,
    fields: &[&str],
) -> Option<Conversion> {
    let ast = cx.ast();
    let argument = match *ast.node(node) {
        Node::Call { args, .. } | Node::New { args, .. } => *ast.list(args).first()?,
        _ => return None,
    };
    let unwrapped = skip_wrappers(ast, argument);
    let is_from = method == Some("from");
    if is_from {
        if let Some((obj, _, [])) = plain_method(cx, unwrapped, &["toString", "toJSON"], 0) {
            return Some(Conversion {
                source: obj,
                autofix: target != "Instant",
            });
        }
        if !matches!(ast.node(unwrapped), Node::ObjectLit { .. }) {
            return Some(Conversion {
                source: argument,
                autofix: true,
            });
        }
    }
    if target != "Instant" {
        return field_reconstruction(cx, node, fields);
    }
    let milliseconds = method == Some("fromEpochMilliseconds");
    let property = if milliseconds {
        "epochMilliseconds"
    } else {
        "epochNanoseconds"
    };
    Some(Conversion {
        source: property_of(cx, argument, property)?,
        autofix: !milliseconds,
    })
}

impl Rule for PreferTemporalConversion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (constructor, method, args) = match *ast.node(node) {
            Node::New { callee, args, .. } => (callee, None, ast.list(args)),
            Node::Call { .. } => {
                let methods = ["from", "fromEpochMilliseconds", "fromEpochNanoseconds"];
                let Some((obj, name, args)) = plain_method(cx, node, &methods, 1) else {
                    return;
                };
                if args.len() != 1 {
                    return;
                }
                (obj, Some(name), args)
            }
            _ => return,
        };
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(skip_parens(ast, constructor))
        else {
            return;
        };
        if !name_is(cx, skip_parens(ast, obj), "Temporal") {
            return;
        }
        let Some(target) = ident_name(ast, prop) else {
            return;
        };
        let (fields, checker) = match target {
            "Instant" => (&[][..], &ZONED),
            "PlainDateTime" => (DATE_TIME_FIELDS, &ZONED),
            "PlainDate" => (DATE_FIELDS, &ZONED_OR_PLAIN),
            "PlainTime" => (TIME_FIELDS, &ZONED_OR_PLAIN),
            _ => return,
        };
        if args.is_empty()
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            || (target == "Instant" && args.len() != 1)
            || (target != "Instant" && method.is_some_and(|m| m != "from"))
        {
            return;
        }
        let Some(Conversion { source, autofix }) = conversion(cx, node, method, target, fields)
        else {
            return;
        };
        if !checker.is_target(cx, source) {
            return;
        }
        let to = format!("to{target}");
        let message = format!("Prefer `.{to}()` when converting to `Temporal.{target}`.");
        let span = cx.span(node);
        if would_remove_comments(cx, span, &[paren_span(cx, source)]) {
            cx.report(span, message).id("prefer-temporal-conversion");
            return;
        }
        let mut text = format!("{}.{to}()", member_object_text(cx, source, node));
        if in_unparenthesized_new_callee(cx, node) {
            text = format!("({text})");
        }
        if needs_semicolon(cx, span.start, &text) {
            text.insert(0, ';');
        }
        let mut fix = Fix::replace(span, text);
        for (s, t) in space_around_keyword(cx, node) {
            fix = fix.and(s, t);
        }
        let report = cx.report(span, message).id("prefer-temporal-conversion");
        if autofix {
            report.fix(fix);
        } else {
            report.suggest(
                "prefer-temporal-conversion/suggestion",
                format!("Replace with `.{to}()`."),
                fix,
            );
        }
    }
}
