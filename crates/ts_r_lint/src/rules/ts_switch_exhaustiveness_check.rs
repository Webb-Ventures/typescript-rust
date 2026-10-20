//! `@typescript-eslint/switch-exhaustiveness-check`: a `switch` over a union of literal
//! types handles every member (and, optionally, has a `default` for other types).

use super::prelude::*;
use crate::regex;
use std::cmp::Ordering;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/switch-exhaustiveness-check",
        "Require switch-case statements to be exhaustive",
        NO_PRESET,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create::<SwitchExhaustivenessCheck>,
};

#[derive(Debug)]
struct SwitchExhaustivenessCheck {
    allow_default_for_exhaustive: bool,
    consider_default_exhaustive: bool,
    require_default_for_non_union: bool,
    comment_pattern: Option<String>,
}

impl FromOptions for SwitchExhaustivenessCheck {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(SwitchExhaustivenessCheck {
            allow_default_for_exhaustive: bool_field(
                o,
                "allowDefaultCaseForExhaustiveSwitch",
                true,
            ),
            consider_default_exhaustive: bool_field(o, "considerDefaultExhaustiveForUnions", false),
            require_default_for_non_union: bool_field(o, "requireDefaultForNonUnion", false),
            comment_pattern: str_field(o, "defaultCaseCommentPattern").map(str::to_owned),
        })
    }
}

/// A literal-like type a `case` can match exactly.
fn is_unit(ti: &dyn TypeInfo, t: Ty) -> bool {
    t != Ty::UNRESOLVED
        && ti.flags(t).intersects(
            TypeFlags::LITERAL
                | TypeFlags::ENUM_LITERAL
                | TypeFlags::UNIQUE_ES_SYMBOL
                | TypeFlags::NULL
                | TypeFlags::UNDEFINED,
        )
}

/// The unit type a union member stands for (`'a' & { brand }` stands for `'a'`).
fn unit_of(ti: &dyn TypeInfo, t: Ty) -> Option<Ty> {
    if is_unit(ti, t) {
        return Some(t);
    }
    if ti.flags(t).contains(TypeFlags::INTERSECTION) {
        return ti
            .intersection_members(t)
            .into_iter()
            .find(|&m| is_unit(ti, m));
    }
    None
}

fn same_unit(ti: &dyn TypeInfo, a: Ty, b: Ty) -> bool {
    if a == b {
        return true;
    }
    let (fa, fb) = (ti.flags(a), ti.flags(b));
    if fa != fb {
        return false;
    }
    if fa.intersects(TypeFlags::NULL | TypeFlags::UNDEFINED) {
        return true;
    }
    // Fresh and regular literal types of one value are different types.
    if fa.intersects(TypeFlags::ENUM_LITERAL | TypeFlags::UNIQUE_ES_SYMBOL) {
        return ti.type_symbol(a).is_some() && ti.type_symbol(a) == ti.type_symbol(b);
    }
    fa.intersects(TypeFlags::LITERAL) && ti.type_to_string(a) == ti.type_to_string(b)
}

/// Collation close to `String.prototype.localeCompare`: punctuation, then digits, then
/// letters (case-insensitive first, lower case before upper case on ties).
fn locale_cmp(a: &str, b: &str) -> Ordering {
    fn key(c: char) -> (u8, u32) {
        const PUNCT: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
        if c.is_whitespace() {
            (0, c as u32)
        } else if let Some(i) = PUNCT.find(c) {
            (1, i as u32)
        } else if c.is_ascii_digit() {
            (2, c as u32)
        } else if c.is_alphabetic() {
            (3, c.to_lowercase().next().unwrap_or(c) as u32)
        } else {
            (1, 1000 + c as u32)
        }
    }
    let primary = a.chars().map(key).cmp(b.chars().map(key));
    if primary != Ordering::Equal {
        return primary;
    }
    // Lower case sorts before upper case.
    let tert = |s: &str| s.chars().map(|c| c.is_uppercase()).collect::<Vec<_>>();
    tert(a).cmp(&tert(b)).then_with(|| a.cmp(b))
}

fn is_identifier(name: &str) -> bool {
    let mut cs = name.chars();
    cs.next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn escape_single(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// How the unit type `t` is shown in the message, and the source text a `case` uses for it.
fn unit_texts(ti: &dyn TypeInfo, t: Ty) -> (String, String) {
    let f = ti.flags(t);
    if f.intersects(TypeFlags::ENUM_LITERAL | TypeFlags::UNIQUE_ES_SYMBOL)
        && let Some(s) = ti.type_symbol(t)
    {
        let name = ti.symbol_name(s);
        if f.contains(TypeFlags::UNIQUE_ES_SYMBOL) {
            return (format!("typeof {name}"), name);
        }
        let mut path = Vec::new();
        let mut p = ti.symbol_parent(s);
        let mut guard = 0;
        while let Some(ps) = p
            && guard < 32
        {
            let facts = ti.symbol_facts(ps);
            let n = ti.symbol_name(ps);
            if !(facts.enum_ || facts.module) || n.starts_with('"') || n.starts_with("__") {
                break;
            }
            path.push(n);
            p = ti.symbol_parent(ps);
            guard += 1;
        }
        path.reverse();
        let prefix = path.join(".");
        return if is_identifier(&name) {
            (format!("{prefix}.{name}"), format!("{prefix}.{name}"))
        } else {
            (
                format!("(typeof {prefix})[{}]", json_quote(&name)),
                format!("{prefix}['{}']", escape_single(&name)),
            )
        };
    }
    let shown = ti.type_to_string(t);
    (shown.clone(), shown)
}

fn json_quote(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn line_indent(src: &str, pos: u32) -> String {
    let pos = (pos as usize).min(src.len());
    let start = src[..pos].rfind('\n').map_or(0, |i| i + 1);
    src[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Where a `default` lives: a real `default:` case or a "no default" comment.
#[derive(Clone, Copy)]
enum DefaultAt {
    Case(NodeId),
    Comment(Span),
}

impl SwitchExhaustivenessCheck {
    fn comment_default(&self, cx: &Ctx<'_, '_>, after: u32, end: u32) -> Option<Span> {
        let ast = cx.ast();
        let src = cx.src();
        let c = ast.comments_in(Span::new(after, end)).last()?;
        // Only a comment that is the last thing in the switch body counts.
        if skip_trivia_forward(src, c.span.end as usize) != (end as usize).saturating_sub(1) {
            return None;
        }
        let t = c.span.text(src);
        let t = t
            .strip_prefix("//")
            .or_else(|| {
                t.strip_prefix("/*")
                    .map(|t| t.strip_suffix("*/").unwrap_or(t))
            })
            .unwrap_or(t)
            .trim();
        let matched = match &self.comment_pattern {
            Some(p) => regex::compile(p).is_some_and(|r| r.is_match(t, false)),
            None => t.eq_ignore_ascii_case("no default"),
        };
        matched.then_some(c.span)
    }

    /// The suggestion inserting `items` (case texts) into the switch.
    fn insert_fix(
        cx: &Ctx<'_, '_>,
        switch: NodeId,
        disc: NodeId,
        cases: &[NodeId],
        default: Option<DefaultAt>,
        items: &[String],
    ) -> Option<Fix> {
        let src = cx.src();
        let before_default = |pos: u32| {
            let indent = line_indent(src, pos);
            let text: String = items.iter().map(|i| format!("{i}\n{indent}")).collect();
            Fix::insert(pos, text)
        };
        match default {
            Some(DefaultAt::Case(d)) => return Some(before_default(cx.span(d).start)),
            Some(DefaultAt::Comment(s)) => return Some(before_default(s.start)),
            None => {}
        }
        if let Some(&last) = cases.last() {
            let indent = line_indent(src, cx.span(last).start);
            let text: String = items.iter().map(|i| format!("\n{indent}{i}")).collect();
            return Some(Fix::insert(cx.span(last).end, text));
        }
        let open = find_char(src, cx.span(disc).end, b'{')?;
        let close = cx.span(switch).end.checked_sub(1)?;
        if close < open + 1 {
            return None;
        }
        let indent = line_indent(src, cx.span(switch).start);
        let mut text = String::from("\n");
        for i in items {
            text.push_str(&format!("{indent}{i}\n"));
        }
        text.push_str(&indent);
        Some(Fix::replace(Span::new(open + 1, close), text))
    }
}

impl Rule for SwitchExhaustivenessCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Switch]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Switch { disc, cases } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let cases: Vec<NodeId> = ast.list(cases).to_vec();
        let ti = cx.types();
        let Some(t) = ti.type_at(skip_parens(ast, disc)) else {
            return;
        };
        if has_unresolved_part(ti, t) {
            return;
        }
        let t = constrained(ti, t);
        let mut units: Vec<Ty> = Vec::new();
        let mut non_unit = false;
        for p in parts(ti, t) {
            if p == Ty::UNRESOLVED {
                return;
            }
            match unit_of(ti, p) {
                Some(u) => units.push(u),
                None => non_unit = true,
            }
        }
        // Cases and the default.
        let mut default = None;
        let mut covered: Vec<Ty> = Vec::new();
        for &c in &cases {
            match *cx.node(c) {
                Node::SwitchCase { test: None, .. } => default = Some(DefaultAt::Case(c)),
                Node::SwitchCase {
                    test: Some(test), ..
                } => {
                    if let Some(ct) = ti.type_at(skip_parens(ast, test))
                        && ct != Ty::UNRESOLVED
                    {
                        for p in parts(ti, ct) {
                            if let Some(u) = unit_of(ti, p) {
                                covered.push(u);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if default.is_none() {
            let after = cases.last().map_or(cx.span(disc).end, |&c| cx.span(c).end);
            if let Some(s) = self.comment_default(cx, after, cx.span(node).end) {
                default = Some(DefaultAt::Comment(s));
            }
        }
        let mut missing: Vec<Ty> = Vec::new();
        for &u in &units {
            if !covered.iter().any(|&c| same_unit(ti, c, u))
                && !missing.iter().any(|&m| same_unit(ti, m, u))
            {
                missing.push(u);
            }
        }
        let disc_node = skip_parens(ast, disc);
        if !missing.is_empty() && (default.is_none() || !self.consider_default_exhaustive) {
            let mut shown: Vec<(String, String)> =
                missing.iter().map(|&m| unit_texts(ti, m)).collect();
            shown.sort_by(|a, b| {
                let key = |s: &str| {
                    if s.starts_with("typeof ") {
                        String::new()
                    } else {
                        s.to_string()
                    }
                };
                locale_cmp(&key(&a.0), &key(&b.0))
            });
            let msg = format!(
                "Switch is not exhaustive. Cases not matched: {}",
                shown
                    .iter()
                    .map(|s| s.0.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            let items: Vec<String> = shown
                .iter()
                .map(|(_, c)| {
                    format!(
                        "case {c}: {{ throw new Error('Not implemented yet: {} case') }}",
                        escape_single(c)
                    )
                })
                .collect();
            let fix = Self::insert_fix(cx, node, disc, &cases, default, &items);
            let mut r = cx.report_node(disc_node, msg).id("switchIsNotExhaustive");
            if let Some(f) = fix {
                r = r.suggest("addMissingCases", "Add branches for missing cases.", f);
            }
            let _ = r;
        }
        if non_unit && default.is_none() && self.require_default_for_non_union {
            let items = vec!["default: { throw new Error('default case') }".to_string()];
            let fix = Self::insert_fix(cx, node, disc, &cases, None, &items);
            let mut r = cx
                .report_node(
                    disc_node,
                    "Switch is not exhaustive. Cases not matched: default",
                )
                .id("switchIsNotExhaustive");
            if let Some(f) = fix {
                r = r.suggest("addMissingCases", "Add branches for missing cases.", f);
            }
            let _ = r;
        }
        if !self.allow_default_for_exhaustive
            && missing.is_empty()
            && !non_unit
            && !units.is_empty()
            && let Some(d) = default
        {
            let msg = "The switch statement is exhaustive, so the default case is unnecessary.";
            match d {
                DefaultAt::Case(c) => {
                    cx.report_node(c, msg).id("dangerousDefaultCase");
                }
                DefaultAt::Comment(s) => {
                    cx.report(s, msg).id("dangerousDefaultCase");
                }
            }
        }
    }
}
