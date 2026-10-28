//! `unicorn/expiring-todo-comments`: TODO comments (any of `terms`, case-insensitive) can
//! carry conditions in brackets (`// TODO [2030-01-01]: ...`). Each line of a comment is read
//! on its own: the first `[` to the last `]` with no `}` between holds comma-separated
//! arguments. Two or more ISO dates are reported; one date is reported once it is before
//! `date` (default today, UTC) when `checkDates` is on (and, on a pull-request CI build, only
//! with `checkDatesOnPullRequests`). An argument that becomes a date once its spaces are
//! removed is reported too. Lines matching an `ignore` pattern are skipped.
//!
//! The plugin parses the package conditions (`[>1]`, `[+pkg]`, `[pkg@>=2]`,
//! `[engine:node@>=20]`, `[peer:x@>=1]`) only when its argument parser finds a
//! `package.json` above the process's working directory; this rule treats them as unknown
//! arguments, as the plugin does without one.
//!
//! With `allowWarningComments: false`, a line with a term but no condition that was used is
//! reported like ESLint's `no-warning-comments` (`location: "start"`): the term at the
//! line's start, shown as the first 40 characters of the line's words.

use super::prelude::*;
use super::unicorn_ast::is_js_space;
use super::unicorn_names::{RegexOpt, regex_list};
use crate::directives::InlineConfig;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/expiring-todo-comments",
        "Add expiration conditions to TODO comments.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ExpiringTodoComments>,
};

#[derive(Debug)]
struct ExpiringTodoComments {
    terms: Vec<String>,
    ignore: Vec<RegexOpt>,
    check_dates: bool,
    check_on_prs: bool,
    allow_warning_comments: bool,
    date: Option<String>,
}

impl FromOptions for ExpiringTodoComments {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let terms = match o.get("terms").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            None => vec!["todo".into(), "fixme".into(), "xxx".into()],
        };
        Ok(ExpiringTodoComments {
            terms,
            ignore: regex_list(o.get("ignore"), "ignore")?,
            check_dates: flag("checkDates", false),
            check_on_prs: flag("checkDatesOnPullRequests", false),
            allow_warning_comments: flag("allowWarningComments", true),
            date: o.get("date").and_then(Value::as_str).map(str::to_owned),
        })
    }
}

/// `/^\d{4}-\d{2}-\d{2}$/`.
fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// Days since 1970-01-01 of `y-m-d`; the day may run past the month's end (as `Date.parse`
/// accepts up to 31).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + (d - 1)
}

/// `Date.parse` of a `YYYY-MM-DD` string, in days; `None` where it gives `NaN`.
fn parse_date(s: &str) -> Option<i64> {
    if !is_iso_date(s) {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, 1) + d - 1)
}

fn today_days() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| (d.as_secs() / 86_400) as i64)
}

/// A pull-request build on a CI service (`ci-info`'s `isPR`, for the common services).
fn is_pull_request() -> bool {
    let var = |k: &str| std::env::var(k).ok();
    let has = |k: &str| var(k).is_some_and(|v| !v.is_empty());
    let ne_false = |k: &str| var(k).is_some_and(|v| v != "false");
    let checks: [(bool, bool); 12] = [
        (has("APPVEYOR"), has("APPVEYOR_PULL_REQUEST_NUMBER")),
        (
            has("TF_BUILD"),
            var("BUILD_REASON").as_deref() == Some("PullRequest"),
        ),
        (has("BITBUCKET_COMMIT"), has("BITBUCKET_PR_ID")),
        (has("BUILDKITE"), ne_false("BUILDKITE_PULL_REQUEST")),
        (has("CIRCLECI"), has("CIRCLE_PULL_REQUEST")),
        (
            has("DRONE"),
            var("DRONE_BUILD_EVENT").as_deref() == Some("pull_request"),
        ),
        (
            has("GITHUB_ACTIONS"),
            var("GITHUB_EVENT_NAME").as_deref() == Some("pull_request"),
        ),
        (has("GITLAB_CI"), has("CI_MERGE_REQUEST_ID")),
        (
            has("JENKINS_URL") && has("BUILD_ID"),
            has("ghprbPullId") || has("CHANGE_ID"),
        ),
        (has("NETLIFY"), ne_false("PULL_REQUEST")),
        (has("TRAVIS"), ne_false("TRAVIS_PULL_REQUEST")),
        (
            has("VERCEL") || has("NOW_BUILDER"),
            has("VERCEL_GIT_PULL_REQUEST_ID"),
        ),
    ];
    checks
        .iter()
        .find(|(vendor, _)| *vendor)
        .is_some_and(|(_, pr)| *pr)
}

/// `/\[([^}]+)]/`: from the first `[` that has one to the last `]` before any `}`.
fn bracket_arguments(s: &str) -> Option<&str> {
    for (i, c) in s.char_indices() {
        if c != '[' {
            continue;
        }
        let rest = &s[i + 1..];
        let limit = rest.find('}').unwrap_or(rest.len());
        if let Some(j) = rest[..limit].rfind(']')
            && j > 0
        {
            return Some(&rest[..j]);
        }
    }
    None
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// The text after the first `]`, without a leading `:`.
fn todo_message(line: &str) -> &str {
    let after = line.find(']').map_or(line, |i| &line[i + 1..]);
    let after = js_trim(after);
    match after.strip_prefix(':') {
        Some(r) => js_trim(r),
        None => after,
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `^\s*term\b` (case-insensitive) as `no-warning-comments` matches a term at the start.
fn starts_with_term(line: &str, term: &str) -> bool {
    let rest = line.trim_start_matches(is_js_space);
    let mut it = rest.chars();
    for t in term.chars() {
        match it.next() {
            Some(c) if c == t || c.to_lowercase().eq(t.to_lowercase()) => {}
            _ => return false,
        }
    }
    if term.chars().last().is_some_and(is_word) {
        return !it.next().is_some_and(is_word);
    }
    true
}

/// The line's words joined by single spaces while they fit in 40 UTF-16 units.
fn display(line: &str) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for w in js_trim(line).split(is_js_space).filter(|w| !w.is_empty()) {
        let next = if out.is_empty() {
            w.to_owned()
        } else {
            format!("{out} {w}")
        };
        if next.encode_utf16().count() <= 40 {
            out = next;
        } else {
            truncated = true;
            break;
        }
    }
    if truncated {
        out.push_str("...");
    }
    out
}

impl ExpiringTodoComments {
    /// Reports the line's conditions; true when nothing used it (left for the
    /// `no-warning-comments` part).
    fn process(&self, cx: &mut Ctx<'_, '_>, span: Span, line: &str, now: Option<i64>) -> bool {
        let lower = line.to_lowercase();
        if !self.terms.iter().any(|t| lower.contains(&t.to_lowercase())) {
            return true;
        }
        let Some(raw) = bracket_arguments(line) else {
            return true;
        };
        let mut dates = Vec::new();
        let mut unknowns = Vec::new();
        for a in raw.split(',') {
            let a = js_trim(a);
            if is_iso_date(a) {
                dates.push(a);
            } else {
                unknowns.push(a);
            }
        }
        let message = todo_message(line);
        let mut uses = 0;
        if dates.len() > 1 {
            uses += 1;
            cx.report(
                span,
                format!(
                    "Avoid using multiple expiration dates: {}. {message}",
                    dates.join(", ")
                ),
            )
            .id("unicorn/avoidMultipleDates");
        } else if let [date] = dates[..] {
            uses += 1;
            let check = self.check_dates && (self.check_on_prs || !is_pull_request());
            if check
                && let (Some(d), Some(n)) = (parse_date(date), now)
                && d < n
            {
                cx.report(span, format!("Past due date: {date}. {message}"))
                    .id("unicorn/expiredTodo");
            }
        }
        for u in unknowns {
            let without: String = u.chars().filter(|&c| c != ' ').collect();
            if !is_iso_date(&without) {
                continue;
            }
            uses += 1;
            cx.report(
                span,
                format!(
                    "Avoid using whitespace on TODO argument. On '{u}' use '{without}'. {message}"
                ),
            )
            .id("unicorn/removeWhitespaces");
        }
        uses == 0
    }
}

impl Rule for ExpiringTodoComments {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let directives: Vec<Span> = InlineConfig::parse(ast, src, cx.file.lines)
            .directives
            .iter()
            .map(|d| d.comment)
            .collect();
        let now = match &self.date {
            Some(d) => parse_date(d),
            None => Some(today_days()),
        };
        let terms_lower: Vec<String> = self.terms.iter().map(|t| t.to_lowercase()).collect();
        let mut unused: Vec<(Span, String, CommentKind)> = Vec::new();
        for c in &ast.comments {
            if c.kind == CommentKind::Hashbang || directives.contains(&c.span) {
                continue;
            }
            let text = c.span.text(src);
            let value = match c.kind {
                CommentKind::Block => text
                    .get(2..text.len().saturating_sub(2).max(2))
                    .unwrap_or(""),
                _ => text.get(2..).unwrap_or(""),
            };
            if self.allow_warning_comments {
                let lower = value.to_lowercase();
                if !terms_lower.iter().any(|t| lower.contains(t)) {
                    continue;
                }
            }
            for line in value.split('\n') {
                if self.ignore.iter().any(|r| r.test(line)) {
                    continue;
                }
                if self.process(cx, c.span, line, now) {
                    unused.push((c.span, line.to_owned(), c.kind));
                }
            }
        }
        if self.allow_warning_comments {
            return;
        }
        for (span, line, kind) in unused {
            let t = js_trim(&line);
            let directive = match kind {
                CommentKind::Line => t.starts_with("eslint-"),
                _ => {
                    t.starts_with("global ") || t.starts_with("eslint ") || t.starts_with("eslint-")
                }
            };
            if directive && line.contains("no-warning-comments") {
                continue;
            }
            for term in &self.terms {
                if starts_with_term(&line, term) {
                    cx.report(span, format!("Unexpected '{term}': '{}'.", display(&line)))
                        .id("unexpectedComment");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces() {
        assert_eq!(bracket_arguments(" TODO [a] b [c]"), Some("a] b [c"));
        assert_eq!(bracket_arguments(" TODO []"), None);
        assert_eq!(bracket_arguments("[a}] [b]"), Some("b"));
        assert_eq!(todo_message(" TODO [x]: hi "), "hi");
        assert!(parse_date("2000-02-31").is_some());
        assert!(parse_date("2000-02-32").is_none());
        assert_eq!(parse_date("1970-01-02"), Some(1));
        assert!(starts_with_term("  ToDo x", "todo"));
        assert!(!starts_with_term(" todos", "todo"));
    }
}
