//! `unicorn/prefer-temporal`: uses of the global `Date` (followed through `globalThis`-like
//! objects and `const` aliases, as ESLint's reference tracker does).
//!
//! - `new Date()` suggests `Temporal.Now.instant()` / `zonedDateTimeISO()` /
//!   `plainDateTimeISO()`; `new Date(n)` with a static, side-effect-free safe integer within
//!   the `Date` range suggests `Temporal.Instant.fromEpochMilliseconds(n)` (no suggestion when a
//!   comment outside the arguments would be dropped); a static string argument is reported as
//!   string parsing, two or more arguments as the zero-indexed month.
//! - `Date()`, `Date.parse(…)` (parsing) and `Date.UTC(…)` (month) are reported;
//!   `Date.now()` only with `checkDateNow`, fixed to `Temporal.Now.instant().epochMilliseconds`.
//! - With `checkReferences`, other reads of `Date` (not a callee, not a member's object).
//! - `checkMethods` needs the type checker (Date instance methods) and does nothing here.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util::{
    global_path, in_unparenthesized_new_callee, is_modified_global, skip_wrappers,
    static_property_name,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-temporal",
        "Prefer `Temporal` over `Date`.",
        NO_PRESET,
        Category::Restriction,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferTemporal>,
};

#[derive(Debug, Default)]
struct PreferTemporal {
    check_date_now: bool,
    check_references: bool,
}

impl FromOptions for PreferTemporal {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferTemporal {
            check_date_now: bool_field(o, "checkDateNow", false),
            check_references: bool_field(o, "checkReferences", false),
        })
    }
}

const DATE_MAX_TIME: f64 = 8_640_000_000_000_000.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The callee could name a `Date` path: `Date`, a local alias, or a member named `parse`,
/// `UTC` or `now` (only those are traced, so other calls cost nothing).
fn may_name_date(cx: &Ctx<'_, '_>, callee: NodeId, call: bool) -> bool {
    let callee = skip_wrappers(cx.ast(), callee);
    match *cx.node(callee) {
        Node::Ident { .. } => {
            if ident_name(cx.ast(), callee) == Some("Date") {
                return true;
            }
            let sem = cx.semantic();
            sem.reference_at(callee)
                .is_some_and(|r| matches!(sem.reference(r).resolution, Resolution::Symbol(_)))
        }
        Node::Member { .. } | Node::Index { .. } => {
            static_property_name(cx, callee).is_some_and(|n| {
                matches!(n.as_str(), "Date")
                    || (call && matches!(n.as_str(), "parse" | "UTC" | "now"))
            })
        }
        _ => false,
    }
}

impl PreferTemporal {
    fn suggestions(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        args: &[NodeId],
        replacements: &[String],
    ) -> Vec<(String, Fix)> {
        let span = cx.span(node);
        let kept: Vec<Span> = args.iter().map(|&a| paren_span(cx, a)).collect();
        if would_remove_comments(cx, span, &kept) {
            return Vec::new();
        }
        let parens = in_unparenthesized_new_callee(cx, node);
        replacements
            .iter()
            .map(|r| {
                let text = if parens { format!("({r})") } else { r.clone() };
                (r.clone(), Fix::replace(span, text))
            })
            .collect()
    }

    fn new_date(&self, cx: &mut Ctx<'_, '_>, node: NodeId, args: &[NodeId]) {
        let ast = cx.ast();
        let span = cx.span(node);
        let (id, desc, suggestions): (&'static str, &str, Vec<(String, Fix)>) = match args {
            [] => (
                "prefer-temporal",
                "new Date()",
                self.suggestions(
                    cx,
                    node,
                    args,
                    &[
                        "Temporal.Now.instant()".to_owned(),
                        "Temporal.Now.zonedDateTimeISO()".to_owned(),
                        "Temporal.Now.plainDateTimeISO()".to_owned(),
                    ],
                ),
            ),
            [_, _, ..] => ("prefer-temporal/month", "new Date(…)", Vec::new()),
            [arg] if matches!(ast.node(*arg), Node::Spread { .. }) => {
                ("prefer-temporal", "new Date(…)", Vec::new())
            }
            [arg] => match static_value_no_side_effects(cx, *arg) {
                Some(Static::Num(n))
                    if n.fract() == 0.0
                        && n.abs() <= MAX_SAFE_INTEGER
                        && n.abs() <= DATE_MAX_TIME =>
                {
                    let text = paren_span(cx, *arg).text(cx.src());
                    (
                        "prefer-temporal",
                        "new Date(…)",
                        self.suggestions(
                            cx,
                            node,
                            args,
                            &[format!("Temporal.Instant.fromEpochMilliseconds({text})")],
                        ),
                    )
                }
                Some(Static::Str(_)) => ("prefer-temporal/parse", "new Date(…)", Vec::new()),
                _ => ("prefer-temporal", "new Date(…)", Vec::new()),
            },
        };
        let msg = message(id, desc);
        let mut report = cx.report(span, msg).id(id);
        for (replacement, fix) in suggestions {
            report = report.suggest(
                "prefer-temporal/suggestion",
                format!("Replace with `{replacement}`."),
                fix,
            );
        }
    }
}

fn message(id: &str, desc: &str) -> String {
    match id {
        "prefer-temporal/parse" => format!(
            "Prefer `Temporal` over `{desc}`. Parsing date strings with `Date` is inconsistent across implementations."
        ),
        "prefer-temporal/month" => format!(
            "Prefer `Temporal` over `{desc}`. The month argument of `Date` is zero-indexed, which is error-prone."
        ),
        _ => format!("Prefer `Temporal` over `{desc}`."),
    }
}

/// The ESTree parent of `id` with parentheses and the reference tracker's pass-through TS
/// wrappers skipped: `(parent, child on the path)`.
fn tracker_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        match *cx.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => cur = p,
            _ => return Some((p, cur)),
        }
    }
    None
}

impl Rule for PreferTemporal {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call, NodeKind::Ident]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::New { callee, args, .. } => {
                if may_name_date(cx, callee, false)
                    && global_path(cx, callee).as_deref() == Some("Date")
                {
                    self.new_date(cx, node, ast.list(args));
                }
            }
            Node::Call { callee, .. } => {
                if !may_name_date(cx, callee, true) {
                    return;
                }
                let span = cx.span(node);
                let (id, desc) = match global_path(cx, callee).as_deref() {
                    Some("Date") => ("prefer-temporal", "Date()"),
                    Some("Date.parse") => ("prefer-temporal/parse", "Date.parse(…)"),
                    Some("Date.UTC") => ("prefer-temporal/month", "Date.UTC(…)"),
                    Some("Date.now") if self.check_date_now => {
                        let fix = ast.comments_in(span).is_empty().then(|| {
                            Fix::replace(span, "Temporal.Now.instant().epochMilliseconds")
                        });
                        cx.report(span, message("prefer-temporal", "Date.now()"))
                            .id("prefer-temporal")
                            .fix_opt(fix);
                        return;
                    }
                    _ => return,
                };
                cx.report(span, message(id, desc)).id(id);
            }
            Node::Ident { .. } if self.check_references => {
                if ident_name(ast, node) != Some("Date") {
                    return;
                }
                let sem = cx.semantic();
                let Some(r) = sem.reference_at(node) else {
                    return;
                };
                let r = sem.reference(r);
                let global = match r.resolution {
                    Resolution::Global => true,
                    Resolution::Symbol(s) => {
                        sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL)
                    }
                    Resolution::Unresolved => false,
                };
                if !global || !r.is_read() || r.is_type() || is_modified_global(cx, "Date") {
                    return;
                }
                if let Some((p, child)) = tracker_parent(cx, node) {
                    match *ast.node(p) {
                        Node::New { callee, .. } | Node::Call { callee, .. } if callee == child => {
                            return;
                        }
                        Node::Member { obj, .. } | Node::Index { obj, .. } if obj == child => {
                            return;
                        }
                        _ => {}
                    }
                }
                cx.report_node(node, message("prefer-temporal", "Date"))
                    .id("prefer-temporal");
            }
            _ => {}
        }
    }
}
