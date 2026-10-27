//! `sort-keys`: the keys of each object literal in ascending (or descending) order,
//! optionally case-insensitive and natural, restarting after spreads and (with
//! `allowLineSeparatedGroups`) blank lines.

use std::cmp::Ordering;

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "sort-keys",
        "Require object keys to be sorted",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<SortKeys>,
};

#[derive(Debug)]
struct SortKeys {
    desc: bool,
    insensitive: bool,
    natural: bool,
    min_keys: usize,
    allow_line_separated_groups: bool,
    ignore_computed_keys: bool,
}

impl FromOptions for SortKeys {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 1);
        Ok(SortKeys {
            desc: opts.first().and_then(Value::as_str) == Some("desc"),
            insensitive: !bool_field(o, "caseSensitive", true),
            natural: bool_field(o, "natural", false),
            min_keys: o.get("minKeys").and_then(Value::as_u64).unwrap_or(2) as usize,
            allow_line_separated_groups: bool_field(o, "allowLineSeparatedGroups", false),
            ignore_computed_keys: bool_field(o, "ignoreComputedKeys", false),
        })
    }
}

/// Where a character sorts in natural order: `-`, then other punctuation, digits, upper
/// case letters and lower case letters (ASCII); everything else by its code.
fn natural_rank(c: char) -> u32 {
    let c = c as u32;
    match c {
        _ if !(45..=127).contains(&c) => c,
        45 => 65,
        46..=47 => c - 1,
        48..=57 => c + 18,
        58..=64 => c - 11,
        65..=90 => c + 11,
        91..=96 => c - 37,
        97..=122 => c + 5,
        _ => c - 63,
    }
}

/// Natural order: runs of digits starting with 1-9 compare by their numeric value.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j) = (0, 0);
    loop {
        let ca = a.get(i).copied();
        let cb = b.get(j).copied();
        let (Some(x), Some(y)) = (ca, cb) else {
            return ca.is_some().cmp(&cb.is_some());
        };
        if ('1'..='9').contains(&x) && ('1'..='9').contains(&y) {
            let run = |s: &[char], k: usize| {
                let end = (k..s.len())
                    .find(|&e| !s[e].is_ascii_digit())
                    .unwrap_or(s.len());
                (end, s[k..end].iter().collect::<String>())
            };
            let (ea, na) = run(&a, i);
            let (eb, nb) = run(&b, j);
            // Without leading zeros, a longer run is a larger number.
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
            if ord != Ordering::Equal {
                return ord;
            }
            (i, j) = (ea, eb);
            continue;
        }
        let ord = natural_rank(x).cmp(&natural_rank(y));
        if ord != Ordering::Equal {
            return ord;
        }
        i += 1;
        j += 1;
    }
}

/// A blank line in the text between two properties (outside comments).
fn blank_line_between(cx: &Ctx<'_, '_>, from: u32, to: u32) -> bool {
    let src = cx.src();
    let mut pos = from;
    let mut gaps = Vec::new();
    for c in cx.ast().comments_in(Span::new(from, to)) {
        gaps.push(Span::new(pos, c.span.start));
        pos = c.span.end;
    }
    gaps.push(Span::new(pos, to));
    gaps.iter().any(|g| {
        g.text(src).split(|c: char| !c.is_whitespace()).any(|ws| {
            let breaks = ws
                .chars()
                .filter(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
                .count();
            breaks - ws.matches("\r\n").count() > 1
        })
    })
}

impl SortKeys {
    fn valid(&self, prev: &str, this: &str) -> bool {
        let (a, b) = if self.desc {
            (this, prev)
        } else {
            (prev, this)
        };
        let (a, b) = if self.insensitive {
            (a.to_lowercase(), b.to_lowercase())
        } else {
            (a.to_owned(), b.to_owned())
        };
        if self.natural {
            natural_cmp(&a, &b) != Ordering::Greater
        } else {
            a <= b
        }
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, props: &[NodeId]) {
        let ast = cx.ast();
        let src = cx.src();
        let num_keys = props.len();
        let mut prev_node: Option<NodeId> = None;
        let mut prev_name: Option<String> = None;
        let mut prev_blank = false;
        for &p in props {
            let (key, computed) = match *ast.node(p) {
                Node::Spread { .. } => {
                    prev_name = None;
                    continue;
                }
                Node::Property { key, .. } | Node::MethodDef { key, .. } => {
                    (key, matches!(ast.node(key), Node::ComputedKey { .. }))
                }
                Node::ShorthandProp { name, .. } => (name, false),
                _ => continue,
            };
            if self.ignore_computed_keys && computed {
                prev_name = None;
                continue;
            }
            let this_name = key_name(ast, src, key).map(|n| n.into_owned()).or_else(|| {
                let Node::ComputedKey { expr } = *ast.node(key) else {
                    return None;
                };
                let e = skip_parens(ast, expr);
                matches!(ast.node(e), Node::Ident { .. })
                    .then(|| ident_name(ast, e))
                    .flatten()
                    .map(String::from)
            });
            let mut blank = prev_blank;
            if let Some(pn) = prev_node {
                blank |= blank_line_between(cx, cx.span(pn).end, cx.span(p).start);
            }
            prev_node = Some(p);
            let prev = prev_name.take();
            prev_name = this_name.clone().or_else(|| prev.clone());
            if self.allow_line_separated_groups && blank {
                prev_blank = this_name.is_none();
                continue;
            }
            let (Some(prev), Some(this)) = (prev, this_name) else {
                continue;
            };
            if num_keys < self.min_keys || self.valid(&prev, &this) {
                continue;
            }
            let msg = format!(
                "Expected object keys to be in {}{}{}ending order. '{this}' should be before '{prev}'.",
                if self.natural { "natural " } else { "" },
                if self.insensitive { "insensitive " } else { "" },
                if self.desc { "desc" } else { "asc" },
            );
            // ESTree's key of `[k]` is `k`.
            let at = match *ast.node(key) {
                Node::ComputedKey { expr } => expr,
                _ => key,
            };
            cx.report_node(at, msg).id("sortKeys");
        }
    }
}

impl Rule for SortKeys {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ObjectLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::ObjectLit { props } = *cx.node(node) else {
            return;
        };
        if is_assignment_target(cx.ast(), cx.file.parents, node) {
            return;
        }
        self.check(cx, cx.ast().list(props));
    }
}
