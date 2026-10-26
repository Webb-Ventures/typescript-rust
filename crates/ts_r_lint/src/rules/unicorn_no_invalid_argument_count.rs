//! `unicorn/no-invalid-argument-count`: calls and `new` with a number of arguments the
//! callee can't take.
//!
//! - Known globals (`unicorn_argument_counts::DEFAULT_COUNTS`): the callee's dotted path
//!   (through parentheses, TS wrappers and optional links; plain names only) with leading
//!   `globalThis` / `global` / `self` / `window` dropped, when its root is the global binding.
//!   Too few or too many arguments, or not one of the allowed counts; with a spread only the
//!   other arguments are counted, against the maximum.
//! - Options add patterns (`"foo.bar"`, `"*.bar"`, `"new Foo"`, `"this.x"`) with a count, a
//!   list of allowed counts or `{min, max}`. A pattern that names a default replaces it (and
//!   stays global-only); the others match the raw path without scope. Defaults are tried
//!   first, then the options in order; the first one the call breaks reports.
//! - Otherwise, a call (no spread) to a function written in place or a never-reassigned
//!   single declaration (`function`, or a `const`/`let`/`var` initialized with a function)
//!   with more arguments than parameters (`this` aside; no rest). Too few is never reported.

use super::prelude::*;
use super::unicorn_argument_counts::DEFAULT_COUNTS;
use super::unicorn_prefer_observer_apis::listener_function;
use super::unicorn_util::skip_wrappers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-argument-count",
        "Disallow calling functions and constructors with an invalid number of arguments.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoInvalidArgumentCount>,
};

/// What a callee accepts: `min..=max` (no `max`: unbounded), or exactly one of `allowed`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Expected<'a> {
    min: u32,
    max: Option<u32>,
    allowed: Option<&'a [u32]>,
}

impl<'a> Expected<'a> {
    pub(crate) const fn exact(n: u32) -> Self {
        Self::range(n, Some(n))
    }
    pub(crate) const fn range(min: u32, max: Option<u32>) -> Self {
        Expected {
            min,
            max,
            allowed: None,
        }
    }
    /// `counts` sorted ascending.
    pub(crate) const fn one_of(counts: &'a [u32]) -> Self {
        Expected {
            min: counts[0],
            max: Some(counts[counts.len() - 1]),
            allowed: Some(counts),
        }
    }

    fn is_broken_by(&self, args: usize, spread: bool) -> bool {
        let args = args as u64;
        let above_max = self.max.is_some_and(|m| args > u64::from(m));
        if spread {
            return above_max;
        }
        match self.allowed {
            Some(allowed) => !allowed.iter().any(|&a| u64::from(a) == args),
            None => args < u64::from(self.min) || above_max,
        }
    }

    fn text(&self) -> String {
        if let Some(allowed) = self.allowed {
            let counts: Vec<String> = allowed.iter().map(|&c| count_text(c as usize)).collect();
            return match counts.as_slice() {
                [one] => one.clone(),
                [a, b] => format!("{a} or {b}"),
                [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
                [] => String::new(),
            };
        }
        match self.max {
            Some(max) if max == self.min => count_text(max as usize),
            Some(max) if self.min == 0 => format!("at most {}", count_text(max as usize)),
            Some(max) => format!("between {} and {}", self.min, count_text(max as usize)),
            None => format!("at least {}", count_text(self.min as usize)),
        }
    }
}

fn count_text(n: usize) -> String {
    format!("{n} {}", if n == 1 { "argument" } else { "arguments" })
}

/// A pattern from the options.
#[derive(Debug)]
struct Custom {
    is_new: bool,
    parts: Vec<String>,
    wildcard: bool,
    /// The pattern names a default, so it matches like one.
    global: bool,
    min: u32,
    max: Option<u32>,
    allowed: Option<Vec<u32>>,
}

impl Custom {
    fn expected(&self) -> Expected<'_> {
        Expected {
            min: self.min,
            max: self.max,
            allowed: self.allowed.as_deref(),
        }
    }
}

#[derive(Debug, Default)]
struct NoInvalidArgumentCount {
    customs: Vec<Custom>,
}

fn count_value(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn default_counts(key: &str) -> Option<Expected<'static>> {
    DEFAULT_COUNTS
        .binary_search_by(|(k, _)| (*k).cmp(key))
        .ok()
        .map(|i| DEFAULT_COUNTS[i].1)
}

impl FromOptions for NoInvalidArgumentCount {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut customs = Vec::new();
        let Some(o) = opts.first().and_then(Value::as_object) else {
            return Ok(Self { customs });
        };
        for (pattern, v) in o {
            let (min, max, allowed) = match v {
                Value::Number(_) => {
                    let n = count_value(v).ok_or("counts must be non-negative integers")?;
                    (n, Some(n), None)
                }
                Value::Array(list) => {
                    let mut counts = list
                        .iter()
                        .map(count_value)
                        .collect::<Option<Vec<u32>>>()
                        .ok_or("counts must be non-negative integers")?;
                    counts.sort_unstable();
                    counts.dedup();
                    let (Some(&lo), Some(&hi)) = (counts.first(), counts.last()) else {
                        return Err("a list of counts can't be empty".into());
                    };
                    (lo, Some(hi), Some(counts))
                }
                Value::Object(m) => {
                    let min = match m.get("min") {
                        Some(v) => count_value(v).ok_or("`min` must be a non-negative integer")?,
                        None => 0,
                    };
                    let max = match m.get("max") {
                        Some(v) => {
                            Some(count_value(v).ok_or("`max` must be a non-negative integer")?)
                        }
                        None => None,
                    };
                    if max.is_some_and(|m| min > m) {
                        return Err("`min` must be less than or equal to `max`.".into());
                    }
                    (min, max, None)
                }
                _ => return Err(format!("invalid count for `{pattern}`")),
            };
            let (is_new, path) = match pattern.strip_prefix("new ") {
                Some(p) => (true, p),
                None => (false, pattern.as_str()),
            };
            customs.push(Custom {
                is_new,
                parts: path.split('.').map(str::to_owned).collect(),
                wildcard: path.contains('*'),
                global: default_counts(pattern).is_some(),
                min,
                max,
                allowed,
            });
        }
        Ok(Self { customs })
    }
}

/// The callee's dotted path of plain names and its root (an identifier or `this`).
fn callee_path<'a>(cx: &Ctx<'a, '_>, callee: NodeId) -> Option<(Vec<&'a str>, NodeId)> {
    let ast = cx.ast();
    let mut node = skip_wrappers(ast, callee);
    let mut parts = Vec::new();
    for _ in 0..10_000 {
        let Node::Member { obj, prop, .. } = *ast.node(node) else {
            break;
        };
        if !matches!(ast.node(prop), Node::Ident { .. }) {
            return None;
        }
        parts.push(ident_name(ast, prop)?);
        node = skip_wrappers(ast, obj);
    }
    match ast.node(node) {
        Node::Ident { .. } => parts.push(ident_name(ast, node)?),
        Node::This {} => parts.push("this"),
        _ => return None,
    }
    parts.reverse();
    Some((parts, node))
}

const GLOBAL_OBJECTS: &[&str] = &["globalThis", "global", "self", "window"];

impl NoInvalidArgumentCount {
    fn configured(
        &self,
        cx: &mut Ctx<'_, '_>,
        callee: NodeId,
        is_new: bool,
        args: &[NodeId],
    ) -> bool {
        let Some((parts, root)) = callee_path(cx, callee) else {
            return false;
        };
        let mut candidates: Vec<Expected<'_>> = Vec::new();
        let is_global_root =
            matches!(cx.node(root), Node::Ident { .. }) && cx.is_global_ref(root, parts[0]);
        let stripped = {
            let mut start = 0;
            while start + 1 < parts.len() && GLOBAL_OBJECTS.contains(&parts[start]) {
                start += 1;
            }
            parts[start..].join(".")
        };
        if is_global_root {
            let key = if is_new {
                format!("new {stripped}")
            } else {
                stripped.clone()
            };
            if !self
                .customs
                .iter()
                .any(|c| c.global && c.is_new == is_new && c.parts.join(".") == stripped)
                && let Some(e) = default_counts(&key)
            {
                candidates.push(e);
            }
        }
        for c in self.customs.iter().filter(|c| c.is_new == is_new) {
            let matched = if c.global {
                is_global_root && c.parts.join(".") == stripped
            } else {
                c.parts.len() == parts.len()
                    && c.parts
                        .iter()
                        .zip(&parts)
                        .all(|(p, &q)| (c.wildcard && p == "*") || p == q)
            };
            if matched {
                candidates.push(c.expected());
            }
        }
        let spread = args
            .iter()
            .filter(|&&a| matches!(cx.node(a), Node::Spread { .. }))
            .count();
        let counted = args.len() - spread;
        let Some(broken) = candidates
            .iter()
            .find(|e| e.is_broken_by(counted, spread > 0))
        else {
            return false;
        };
        let actual = if spread > 0 {
            format!("at least {}", count_text(counted))
        } else {
            count_text(counted)
        };
        let message = format!("Expected {}, but got {actual}.", broken.text());
        let span = cx.span(skip_parens(cx.ast(), callee));
        cx.report(span, message).id("no-invalid-argument-count");
        true
    }
}

/// (non-optional parameters before the trailing optional ones, parameter count; `None`
/// with a rest parameter).
fn arity(cx: &Ctx<'_, '_>, function: NodeId) -> Option<(usize, usize)> {
    let ast = cx.ast();
    let params = match *ast.node(function) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. } => ast.list(params),
        _ => return None,
    };
    let params = match params.first().map(|&p| *ast.node(p)) {
        Some(Node::Param { pattern, .. })
            if matches!(ast.node(pattern), Node::This {})
                || (matches!(ast.node(pattern), Node::Ident { .. })
                    && ident_name(ast, pattern) == Some("this")) =>
        {
            &params[1..]
        }
        _ => params,
    };
    let mut min = params.len();
    for &p in params.iter().rev() {
        let Node::Param {
            rest,
            optional,
            init,
            ..
        } = *ast.node(p)
        else {
            break;
        };
        if rest {
            return None;
        }
        if !optional && init.is_none() {
            break;
        }
        min -= 1;
    }
    Some((min, params.len()))
}

impl Rule for NoInvalidArgumentCount {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args, is_new) = match *ast.node(node) {
            Node::Call { callee, args, .. } => (callee, ast.list(args), false),
            Node::New { callee, args, .. } => (callee, ast.list(args), true),
            _ => return,
        };
        if self.configured(cx, callee, is_new, args) || is_new {
            return;
        }
        if args
            .iter()
            .any(|&a| matches!(cx.node(a), Node::Spread { .. }))
        {
            return;
        }
        let Some(function) = listener_function(cx, callee) else {
            return;
        };
        let Some((min, max)) = arity(cx, function) else {
            return;
        };
        if args.len() <= max {
            return;
        }
        let expected = if min == max {
            count_text(max)
        } else {
            format!("at most {}", count_text(max))
        };
        let message = format!("Expected {expected}, but got {}.", count_text(args.len()));
        let span = cx.span(skip_parens(cx.ast(), callee));
        cx.report(span, message).id("no-invalid-argument-count");
    }
}
