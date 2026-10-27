//! `unicorn/comment-content`: preferred spellings of names and acronyms in comments
//! (`nodejs` -> `Node.js`, `github` -> `GitHub`, `json` -> `JSON`), plus custom replacements.
//!
//! One report per comment: the earliest match of any replacement, fixed by the replacement.
//! Before matching, the parts of a comment that are not prose are blanked out (each byte
//! becomes DEL, so offsets stay put and a match touching a blank is dropped): fenced code
//! blocks, `@example` sections, JSDoc tag syntax, code-like lines (and the lines of a bracket
//! a code-like line leaves open, carried across adjacent `//` comments), inline code and
//! quoted strings, URLs, bare domains, MIME types, package specifiers and paths, Markdown
//! links, markup tags, and the `ignore` patterns. A match is also dropped when its
//! neighbours make it part of a path, file name, property access, hyphenated word or call.
//! Default patterns use Unicode word boundaries (letters, marks, numbers and `_`).
//! Not done: the bracket member-access line shapes (`a[b].c`, `fn [x](y)`).

use super::no_misleading_character_class::is_mark;
use super::prelude::*;
use super::unicorn_names::RegexOpt;
use crate::directives::InlineConfig;
use crate::regex_exec;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/comment-content",
        "Enforce better comment content.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<CommentContent>,
};

/// `(pattern, replacement, case-sensitive)`.
const DEFAULTS: &[(&str, &str, bool)] = &[
    (r"\bapplication\b(?!/)", "app", true),
    (r"\bapplications\b", "apps", true),
    (r"\bapi\b", "API", false),
    (r"\bascii\b", "ASCII", false),
    (r"\bast\b", "AST", false),
    (r"\bavif\b", "AVIF", false),
    (r"\baws\b", "AWS", false),
    (r"\bazure\b", "Azure", false),
    (r"\bbitcoin\b", "Bitcoin", false),
    (r"\bbom\b", "BOM", false),
    (r"\bcd\b", "CD", false),
    (r"\bcdn\b", "CDN", false),
    (r"\bci\b", "CI", false),
    (r"\bcjs\b", "CJS", false),
    (r"\bcli\b", "CLI", false),
    (r"\bcors\b", "CORS", false),
    (r"\bcpu\b", "CPU", false),
    (r"\bcrud\b", "CRUD", false),
    (r"\bcsrf\b", "CSRF", false),
    (r"\bcsv\b", "CSV", false),
    (r"\bcss\b", "CSS", false),
    (r"\bdeno\b", "Deno", false),
    (r"\bdevops\b", "DevOps", false),
    (r"\bdns\b", "DNS", false),
    (r"\bdocker\b", "Docker", false),
    (r"\bdom\b", "DOM", false),
    (r"\bdiscord\b", "Discord", false),
    (r"\bec2\b", "EC2", false),
    (r"\beof\b", "EOF", false),
    (r"\beol\b", "EOL", false),
    (r"\beslint(?:\.js)?\b", "ESLint", false),
    (r"\besm\b", "ESM", false),
    (r"\bfacebook\b", "Facebook", false),
    (r"\bftp\b", "FTP", false),
    (r"\bgcp\b", "GCP", false),
    (r"\bgif\b", "GIF", false),
    (r"\bgit\b", "Git", false),
    (r"\bgithub\b", "GitHub", false),
    (r"\bgpu\b", "GPU", false),
    (r"\bgraphql\b", "GraphQL", false),
    (r"\bgrunt(?:\.js)?\b", "Grunt", false),
    (r"\bgulp(?:\.js)?\b", "Gulp", false),
    (r"\bhtml\b", "HTML", false),
    (r"\bhttps\b", "HTTPS", false),
    (r"\bhttp\b", "HTTP", false),
    (r"\bide\b", "IDE", false),
    (r"\biife\b", "IIFE", false),
    (r"\bios\b", "iOS", false),
    (r"\bjavascript\b", "JavaScript", false),
    (r"\bjpeg\b", "JPEG", false),
    (r"\bjpg\b", "JPG", false),
    (r"\bjson\b", "JSON", false),
    (r"\bjsx\b", "JSX", false),
    (r"\bjquery\b", "jQuery", false),
    (r"\bjwt\b", "JWT", false),
    (r"\bk8s\b", "K8s", false),
    (r"\bkubernetes\b", "Kubernetes", false),
    (r"\blinux\b", "Linux", false),
    (r"\blsp\b", "LSP", false),
    (r"\bltr\b", "LTR", false),
    (r"\bmdn\b", "MDN", false),
    (r"\bmime\b", "MIME", false),
    (r"\bmongodb\b", "MongoDB", false),
    (r"\bmysql\b", "MySQL", false),
    (r"\bnfc\b", "NFC", false),
    (r"\bnfd\b", "NFD", false),
    (r"\bnginx\b", "NGINX", false),
    (r"\bnode\.?js\b", "Node.js", false),
    (r"\bnpm\b", "npm", false),
    (r"\boauth\b", "OAuth", false),
    (r"\bpdf\b", "PDF", false),
    (r"\bpng\b", "PNG", false),
    (r"\bposix\b", "POSIX", false),
    (r"\bpostgresql\b", "PostgreSQL", false),
    (r"\bram\b", "RAM", false),
    (r"\breact\.?js\b", "React", false),
    (r"\breddit\b", "Reddit", false),
    (r"\bredis\b", "Redis", false),
    (r"\brtl\b", "RTL", false),
    (r"\bs3\b", "S3", false),
    (r"\bsdk\b", "SDK", false),
    (r"\bsftp\b", "SFTP", false),
    (r"\bsql\b", "SQL", false),
    (r"\bsqlite\b", "SQLite", false),
    (r"\bssh\b", "SSH", false),
    (r"\bssl\b", "SSL", false),
    (r"\bstack\s?overflow\b", "Stack Overflow", false),
    (r"\bstderr\b", "stderr", false),
    (r"\bstdin\b", "stdin", false),
    (r"\bstdout\b", "stdout", false),
    (r"\bsvelte\b", "Svelte", false),
    (r"\bsvg\b", "SVG", false),
    (r"\btcp\b", "TCP", false),
    (r"\btls\b", "TLS", false),
    (r"\btsv\b", "TSV", false),
    (r"\btwitch(?:tv)?\b", "Twitch", false),
    (r"\btypescript\b", "TypeScript", false),
    (r"\bui\b", "UI", false),
    (r"\budp\b", "UDP", false),
    (r"\bumd\b", "UMD", false),
    (r"\bunicode\b", "Unicode", false),
    (r"\bunix\b", "Unix", false),
    (r"\burl\b", "URL", false),
    (r"\buri\b", "URI", false),
    (r"\butf-?8\b", "UTF-8", false),
    (r"\bux\b", "UX", false),
    (r"\buuid\b", "UUID", false),
    (r"\bvite\b", "Vite", false),
    (r"\bvue(?:\.?js)?\b", "Vue.js", false),
    (r"\bvs\s?code\b", "VS Code", false),
    (r"\b(?:webassembly|wasm)\b", "WebAssembly", false),
    (r"\bwebgl\b", "WebGL", false),
    (r"\bwebp\b", "WebP", false),
    (r"\bwebrtc\b", "WebRTC", false),
    (r"\bwebsocket\b", "WebSocket", false),
    (r"\bxml\b", "XML", false),
    (r"\bxss\b", "XSS", false),
    (r"\byaml\b", "YAML", false),
    (r"\byou\s?tube\b", "YouTube", false),
    (r"\b(?:mac\s?os(?!\s?x)|(mac\s?)?os\s?x)\b", "macOS", false),
];

#[derive(Debug)]
struct Replacement {
    pattern: String,
    replacement: String,
    case_sensitive: bool,
    /// A default pattern: its match needs Unicode word boundaries on both sides.
    unicode_bounds: bool,
    /// A word the text must contain (lowercase when case-insensitive) for a match.
    required: Option<String>,
}

#[derive(Debug)]
struct CommentContent {
    check_uniform_case: bool,
    replacements: Vec<Replacement>,
    ignore: Vec<RegexOpt>,
}

/// The literal word a single-word pattern starts with (`\bgrunt(?:\.js)?\b` -> `grunt`);
/// a quantifier right after it makes its last letter optional.
fn required_text(pattern: &str) -> Option<String> {
    if pattern.contains('|') {
        return None;
    }
    let rest = pattern.strip_prefix(r"\b")?;
    let n = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
    if n == 0 {
        return None;
    }
    let quantified = matches!(rest.as_bytes().get(n), Some(b'*' | b'?' | b'{'));
    Some(rest[..if quantified { n - 1 } else { n }].to_owned())
}

fn replacement(pattern: &str, value: &Value, default: bool) -> Option<Replacement> {
    let (text, case_sensitive) = match value {
        Value::String(s) => (s.clone(), true),
        Value::Object(o) => (
            o.get("replacement")?.as_str()?.to_owned(),
            o.get("caseSensitive") != Some(&Value::Bool(false)),
        ),
        _ => return None,
    };
    let required =
        required_text(pattern).map(|r| if case_sensitive { r } else { r.to_lowercase() });
    Some(Replacement {
        pattern: pattern.to_owned(),
        replacement: text,
        case_sensitive,
        unicode_bounds: default,
        required,
    })
}

fn default_value(text: &str, case_sensitive: bool) -> Value {
    if case_sensitive {
        Value::String(text.to_owned())
    } else {
        serde_json::json!({ "replacement": text, "caseSensitive": false })
    }
}

impl FromOptions for CommentContent {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let extend = bool_field(o, "extendDefaultReplacements", true);
        let custom = o.get("replacements").and_then(Value::as_object);
        // Defaults first (in their order, a custom value replacing a default in place),
        // then the custom patterns that are not defaults.
        let mut entries: Vec<(String, Value, bool)> = Vec::new();
        if extend {
            for &(p, r, cs) in DEFAULTS {
                match custom.and_then(|c| c.get(p)) {
                    Some(v) => entries.push((p.to_owned(), v.clone(), false)),
                    None => entries.push((p.to_owned(), default_value(r, cs), true)),
                }
            }
        }
        for (p, v) in custom.into_iter().flatten() {
            if !(extend && DEFAULTS.iter().any(|d| d.0 == p)) {
                entries.push((p.clone(), v.clone(), false));
            }
        }
        let replacements = entries
            .iter()
            .filter(|(_, v, _)| *v != Value::Bool(false))
            .filter_map(|(p, v, d)| replacement(p, v, *d))
            .collect();
        let mut ignore = Vec::new();
        for p in o
            .get("ignore")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let mut r = RegexOpt::from_value(p).ok_or(
                "The `ignore` option only accepts strings and regular expressions.".to_owned(),
            )?;
            if p.is_string() {
                r.flags = "v".to_owned();
            }
            if !r.flags.contains('g') {
                r.flags.push('g');
            }
            ignore.push(r);
        }
        Ok(CommentContent {
            check_uniform_case: bool_field(o, "checkUniformCase", true),
            replacements,
            ignore,
        })
    }
}

/// `[\p{Letter}\p{Mark}\p{Number}_]`.
fn uword(c: char) -> bool {
    c == '_' || c.is_alphanumeric() || is_mark(c as u32)
}

/// `[\w$]`.
fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn before(s: &str, i: usize) -> Option<char> {
    s.get(..i)?.chars().next_back()
}

fn after(s: &str, i: usize) -> Option<char> {
    s.get(i..)?.chars().next()
}

fn line_end(s: &str, i: usize) -> usize {
    s.get(i..)
        .and_then(|r| r.find('\n'))
        .map_or(s.len(), |n| i + n)
}

/// `(start, end)` of each `\n`-separated line.
fn lines(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let end = line_end(s, start);
        out.push((start, end));
        start = end + 1;
    }
    out
}

/// The blanked-out parts of one comment value, per byte.
struct Masked<'a> {
    text: &'a str,
    hidden: Vec<bool>,
}

impl Masked<'_> {
    fn hide(&mut self, start: usize, end: usize) {
        let end = end.min(self.text.len());
        for i in start..end {
            if self.text.as_bytes()[i] != b'\n' {
                self.hidden[i] = true;
            }
        }
    }

    fn is_hidden(&self, i: usize) -> bool {
        self.hidden.get(i).copied().unwrap_or(false)
    }

    fn any_hidden(&self, start: usize, end: usize) -> bool {
        self.hidden[start..end.min(self.hidden.len())].contains(&true)
    }

    /// The text with every hidden byte replaced by DEL.
    fn searchable(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        for (i, c) in self.text.char_indices() {
            if self.hidden[i] {
                out.extend(std::iter::repeat_n('\u{7f}', c.len_utf8()));
            } else {
                out.push(c);
            }
        }
        out
    }
}

/// A comment line without its leading `*` decoration, trimmed.
fn clean_line(line: &str) -> &str {
    let t = line.trim_start_matches(js_space);
    let t = match t.strip_prefix('*') {
        Some(r) => r.strip_prefix(js_space).unwrap_or(r),
        None => t,
    };
    t.trim_matches(js_space)
}

fn skip_spaces(s: &str) -> &str {
    s.trim_start_matches(js_space)
}

/// `[*+\-]` or `\d+\.`, then spaces; `None` without a marker.
fn strip_list_marker(s: &str) -> Option<&str> {
    let rest = if let Some(r) = s.strip_prefix(['*', '+', '-']) {
        r
    } else {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        s[n..].strip_prefix('.').filter(|_| n > 0)?
    };
    Some(skip_spaces(rest))
}

fn optional_list_marker(s: &str) -> &str {
    skip_spaces(strip_list_marker(s).unwrap_or(s))
}

/// `word` at the start of `s`, not followed by a Unicode word character.
fn starts_word<'s>(s: &'s str, word: &str) -> Option<&'s str> {
    let rest = s.strip_prefix(word)?;
    (!after(rest, 0).is_some_and(uword)).then_some(rest)
}

fn take_ident(s: &str) -> Option<(&str, &str)> {
    let n = s.find(|c: char| !ident_char(c)).unwrap_or(s.len());
    (n > 0).then(|| s.split_at(n))
}

fn take_code_ident(s: &str) -> Option<&str> {
    let first = after(s, 0)?;
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    take_ident(s).map(|(_, r)| r)
}

/// `[...]` (one or more characters inside).
fn take_bracket(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('[')?;
    let close = rest.find(']')?;
    (close > 0).then(|| &rest[close + 1..])
}

/// One `.x` / `?.x` / `.[x]` / `?.[x]` member step.
fn take_member(s: &str) -> Option<&str> {
    if let Some(r) = s.strip_prefix("?.").or_else(|| s.strip_prefix('.')) {
        let r = skip_spaces(r);
        if let Some((_, rest)) = take_ident(r) {
            return Some(rest);
        }
        return take_bracket(r);
    }
    None
}

/// The text after an optional `name:` or `[const|let|var] target =` prefix, each candidate.
fn after_assignment(s: &str) -> Vec<&str> {
    let mut out = vec![s];
    if let Some((_, r)) = take_ident(s)
        && let Some(r) = r.strip_prefix(':')
    {
        out.push(skip_spaces(r));
    }
    let mut t = s;
    for kw in ["const", "let", "var"] {
        if let Some(r) = s.strip_prefix(kw)
            && r.starts_with(js_space)
        {
            t = skip_spaces(r);
        }
    }
    for start in [s, t] {
        let Some((_, mut r)) = take_ident(start) else {
            continue;
        };
        loop {
            let sp = skip_spaces(r);
            match take_member(sp).or_else(|| take_bracket(sp)) {
                Some(n) => r = n,
                None => break,
            }
        }
        if let Some(r) = skip_spaces(r).strip_prefix('=')
            && !r.starts_with('=')
        {
            out.push(skip_spaces(r));
        }
    }
    out
}

/// The starts of a call line: list marker, an opening `(`, an assignment prefix.
fn call_line_starts(line: &str) -> Vec<&str> {
    let s = optional_list_marker(line);
    let s = match s.strip_prefix('(') {
        Some(r) => skip_spaces(r),
        None => s,
    };
    after_assignment(s)
}

/// `a.b(`, `a?.b(`, `a.b?.(`, `a[b](`, `a.[b](` (possibly assigned).
fn is_member_call_line(line: &str) -> bool {
    call_line_starts(line).into_iter().any(|s| {
        let Some((_, r)) = take_ident(s) else {
            return false;
        };
        if let Some(r) = take_bracket(r) {
            return r.starts_with('(');
        }
        let Some(r) = take_member(skip_spaces(r)) else {
            return false;
        };
        let r = skip_spaces(r);
        let r = r.strip_prefix("?.").map_or(r, skip_spaces);
        r.starts_with('(')
    })
}

/// `fn(` or `new Fn(`, or `fn (` ending the line (possibly assigned).
fn is_bare_call_line(line: &str) -> bool {
    call_line_starts(line).into_iter().any(|s| {
        let s = match s.strip_prefix("new") {
            Some(r) if r.starts_with(js_space) => skip_spaces(r),
            _ => s,
        };
        let Some(r) = take_code_ident(s) else {
            return false;
        };
        r.starts_with('(')
            || (r.starts_with(js_space)
                && skip_spaces(r)
                    .strip_prefix('(')
                    .is_some_and(|t| skip_spaces(t).is_empty()))
    })
}

fn is_keyword_line(line: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "import",
        "export",
        "const",
        "let",
        "var",
        "type",
        "interface",
        "class",
        "function",
        "return",
        "await",
        "throw",
    ];
    KEYWORDS.iter().any(|k| starts_word(line, k).is_some())
}

fn is_control_flow_line(line: &str) -> bool {
    for k in ["if", "for", "while", "switch", "catch"] {
        if let Some(r) = line.strip_prefix(k)
            && skip_spaces(r).starts_with('(')
        {
            return true;
        }
    }
    if let Some(r) = line.strip_prefix("else") {
        if r.starts_with(js_space)
            && let Some(t) = skip_spaces(r).strip_prefix("if")
            && skip_spaces(t).starts_with('(')
        {
            return true;
        }
        let t = skip_spaces(r);
        if t.is_empty() || t.starts_with('{') {
            return true;
        }
    }
    for k in ["try", "do", "finally"] {
        if let Some(r) = line.strip_prefix(k) {
            let t = skip_spaces(r);
            if t.is_empty() || t.starts_with('{') {
                return true;
            }
        }
    }
    if let Some(r) = starts_word(line, "case")
        && let Some(colon) = r.find(':')
        && colon > 0
    {
        return true;
    }
    line.strip_prefix("default")
        .is_some_and(|r| skip_spaces(r).starts_with(':'))
}

fn is_command_line(line: &str) -> bool {
    let s = optional_list_marker(line);
    if let Some(r) = s.strip_prefix('$')
        && r.starts_with(js_space)
        && !skip_spaces(r).is_empty()
    {
        return true;
    }
    let Some(r) = s.strip_prefix('>') else {
        return false;
    };
    let r = skip_spaces(r);
    [
        "bun", "curl", "deno", "docker", "git", "nodejs", "node", "npm", "npx", "pnpm", "yarn",
    ]
    .iter()
    .any(|t| starts_word(r, t).is_some())
}

/// `"..."` or `'...'` with no other quote of its kind inside.
fn is_quoted(s: &str) -> bool {
    let Some(q) = after(s, 0).filter(|&q| q == '"' || q == '\'') else {
        return false;
    };
    s.len() >= 2 && s.ends_with(q) && !s[1..s.len() - 1].contains(q)
}

fn all_chars(s: &str, f: impl Fn(char) -> bool) -> bool {
    !s.is_empty() && s.chars().all(f)
}

fn is_key_value_line(line: &str) -> bool {
    let line = strip_list_marker(line).unwrap_or(line);
    let Some(colon) = line.find(':') else {
        return false;
    };
    let key = line[..colon].trim_matches(js_space);
    let mut value = line[colon + 1..].trim_matches(js_space);
    if let Some(v) = value.strip_suffix(',') {
        value = v.trim_end_matches(js_space);
    }
    // A ` # comment` after the value.
    if let Some(hash) = value
        .char_indices()
        .find(|&(i, c)| c == '#' && i > 0 && before(value, i).is_some_and(js_space))
        .map(|(i, _)| i)
    {
        value = value[..hash].trim_end_matches(js_space);
    }
    let key_ok = is_quoted(key) || all_chars(key, |c| ident_char(c) || c == '-');
    let wrapped = |o: char, c: char| value.len() >= 2 && value.starts_with(o) && value.ends_with(c);
    let url = ["http://", "https://"].iter().any(|p| {
        value
            .strip_prefix(p)
            .is_some_and(|r| all_chars(r, |c| !js_space(c)))
    });
    key_ok
        && (is_quoted(value)
            || url
            || all_chars(value, |c| ident_char(c) || matches!(c, '-' | '.' | '/'))
            || wrapped('{', '}')
            || wrapped('[', ']'))
}

fn is_object_literal_line(line: &str) -> bool {
    let line = strip_list_marker(line).unwrap_or(line);
    let line = match line.strip_suffix(',') {
        Some(l) => l.trim_end_matches(js_space),
        None => line,
    };
    line.starts_with('{') && line.ends_with('}') && line.contains(':')
}

/// A line that reads as code: a declaration or statement keyword, control flow, a shell
/// command, `key: value`, an object literal, a trailing `;`, an arrow, or a member call.
fn is_code_line(line: &str) -> bool {
    let line = clean_line(line);
    is_keyword_line(line)
        || is_control_flow_line(line)
        || is_command_line(line)
        || is_key_value_line(line)
        || is_object_literal_line(line)
        || line.ends_with(';')
        || line.contains("=>")
        || is_member_call_line(&without_inline_links(line))
}

/// The change in open brackets over a line (quoted text and comments do not count).
fn bracket_delta(text: &str) -> i32 {
    let mut delta = 0;
    let mut quote: Option<char> = None;
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if let Some(q) = quote {
            if c == '\\' {
                it.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '/' if text[i + 1..].starts_with('/') => break,
            '/' if text[i + 1..].starts_with('*') => match text[i + 2..].find("*/") {
                Some(n) => {
                    let end = i + 2 + n + 2;
                    while it.peek().is_some_and(|&(j, _)| j < end) {
                        it.next();
                    }
                }
                None => break,
            },
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => delta += 1,
            ')' | ']' | '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// How many brackets a call-like line leaves open (0 for other lines).
fn opening_depth(line: &str) -> i32 {
    let line = without_inline_links(clean_line(line));
    if is_bare_call_line(&line) || is_member_call_line(&line) {
        bracket_delta(&line).max(0)
    } else {
        0
    }
}

fn next_depth(depth: i32, line: &str) -> i32 {
    if depth > 0 {
        (depth + bracket_delta(line)).max(0)
    } else {
        opening_depth(line)
    }
}

/// A Markdown link title: `"..."`, `'...'` or `(...)` on one line.
fn take_link_title(s: &str) -> Option<&str> {
    let (open, close) = match after(s, 0)? {
        '"' => ('"', '"'),
        '\'' => ('\'', '\''),
        '(' => ('(', ')'),
        _ => return None,
    };
    let rest = &s[open.len_utf8()..];
    let end = rest.find([close, '\n'])?;
    if !rest[end..].starts_with(close) || (open == '(' && end == 0) {
        return None;
    }
    Some(&rest[end + 1..])
}

/// A destination (no spaces, `)` or `[`), then an optional title after spaces.
fn take_link_destination(s: &str) -> Option<&str> {
    let n = s
        .find(|c: char| js_space(c) || c == ')' || c == '[')
        .unwrap_or(s.len());
    if n == 0 {
        return None;
    }
    let rest = &s[n..];
    let spaces = rest.len() - rest.trim_start_matches(|c| js_space(c) && c != '\n').len();
    if spaces > 0
        && let Some(r) = take_link_title(&rest[spaces..])
    {
        return Some(r);
    }
    Some(rest)
}

/// `[label]`: 1 to 200 characters, no `]` or line break.
fn take_link_label(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('[')?;
    let (n, (end, c)) = rest
        .char_indices()
        .take(201)
        .enumerate()
        .find(|&(_, (_, c))| c == ']' || c == '\n')?;
    (c == ']' && n >= 1).then(|| &rest[end + 1..])
}

/// The end of a Markdown link whose label starts at `i`: `[a](dest "t")`, and unless
/// `inline_only`, `[a]: dest` and `[a][ref]`.
fn markdown_link_end(text: &str, i: usize, inline_only: bool) -> Option<usize> {
    let rest = take_link_label(&text[i..])?;
    let end = if let Some(r) = rest.strip_prefix('(') {
        take_link_destination(r)?.strip_prefix(')')?
    } else if inline_only {
        return None;
    } else if let Some(r) = rest.strip_prefix(':') {
        take_link_destination(r.trim_start_matches(|c| js_space(c) && c != '\n'))?
    } else {
        let mut r = take_link_label(rest)?;
        while let Some(n) = take_link_label(r) {
            r = n;
        }
        r
    };
    Some(text.len() - end.len())
}

/// Markdown links, each with the space or `(` before it: `(start, end)`.
fn markdown_links(text: &str, inline_only: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = after(text, i) {
        let label = if i == 0 && c == '[' {
            Some(0)
        } else if (js_space(c) || c == '(') && text[i + c.len_utf8()..].starts_with('[') {
            Some(i + c.len_utf8())
        } else {
            None
        };
        match label.and_then(|l| markdown_link_end(text, l, inline_only)) {
            Some(end) => {
                out.push((i, end));
                i = end;
            }
            None => i += c.len_utf8(),
        }
    }
    out
}

fn without_inline_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for (s, e) in markdown_links(line, true) {
        out.push_str(&line[last..s]);
        last = e;
    }
    out.push_str(&line[last..]);
    out
}

fn mask_code_lines(m: &mut Masked<'_>) {
    let mut depth = 0;
    for (s, e) in lines(m.text) {
        if m.any_hidden(s, e) {
            depth = 0;
            continue;
        }
        let line = &m.text[s..e];
        let open = opening_depth(line);
        if depth > 0 || open > 0 || is_code_line(line) {
            m.hide(s, e);
            depth = if depth > 0 {
                (depth + bracket_delta(line)).max(0)
            } else {
                open
            };
        }
    }
}

/// ```` ``` ```` / `~~~` fenced blocks (a missing closing fence runs to the end).
fn mask_fences(m: &mut Masked<'_>) {
    let mut open: Option<(u8, usize, usize)> = None;
    for (s, e) in lines(m.text) {
        let line = &m.text[s..e];
        let t = line.trim_start_matches([' ', '\t']);
        let t = match t.strip_prefix('*') {
            Some(r) => r.strip_prefix(js_space).unwrap_or(r),
            None => t,
        };
        let Some(&ch) = t.as_bytes().first().filter(|&&b| b == b'`' || b == b'~') else {
            continue;
        };
        let size = t.bytes().take_while(|&b| b == ch).count();
        if size < 3 {
            continue;
        }
        match open {
            None => open = Some((ch, size, s)),
            Some((c, n, start)) if c == ch && size >= n => {
                m.hide(start, e);
                open = None;
            }
            _ => {}
        }
    }
    if let Some((_, _, start)) = open {
        m.hide(start, m.text.len());
    }
}

/// `@example` sections, up to the next tag line.
fn mask_examples(m: &mut Masked<'_>) {
    let mut start: Option<usize> = None;
    for (s, e) in lines(m.text) {
        let t = clean_line(&m.text[s..e]);
        if let Some(from) = start
            && t.strip_prefix('@')
                .is_some_and(|r| after(r, 0).is_some_and(ident_char_ascii))
        {
            m.hide(from, s);
            start = None;
        }
        if start.is_none() && starts_word(t, "@example").is_some() {
            start = Some(s);
        }
    }
    if let Some(from) = start {
        m.hide(from, m.text.len());
    }
}

fn ident_char_ascii(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The end of a balanced `{...}`/`[...]`/`(...)` starting at `i` (quotes skipped).
fn delimited_end(text: &str, i: usize, end: usize) -> Option<usize> {
    let open = after(text, i)?;
    let close = match open {
        '{' => '}',
        '[' => ']',
        '(' => ')',
        _ => return None,
    };
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut it = text[i..end].char_indices();
    while let Some((j, c)) = it.next() {
        if let Some(q) = quote {
            if c == '\\' {
                it.next();
            } else if c == q {
                quote = None;
            }
        } else if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + j + 1);
            }
        }
    }
    None
}

fn skip_hspace(text: &str, i: usize, end: usize) -> usize {
    i + text[i..end].len()
        - text[i..end]
            .trim_start_matches(|c| js_space(c) && c != '\n')
            .len()
}

/// After a tag: a `{type}` (not an inline `{@link}`); returns where it ends.
fn mask_doc_type(m: &mut Masked<'_>, i: usize, end: usize) -> usize {
    let i = skip_hspace(m.text, i, end);
    if !m.text[i..end].starts_with('{') || is_inline_tag(&m.text[i..end]) {
        return i;
    }
    match delimited_end(m.text, i, end) {
        Some(e) => {
            m.hide(i, e);
            e
        }
        None => i,
    }
}

/// After a tag: a name or value (a bracketed or quoted one, else up to a space).
fn mask_doc_value(m: &mut Masked<'_>, i: usize, end: usize) -> usize {
    let i = skip_hspace(m.text, i, end);
    let Some(c) = after(m.text, i).filter(|_| i < end) else {
        return i;
    };
    if c == '-' {
        return i;
    }
    let e = match c {
        '{' | '[' | '(' => delimited_end(m.text, i, end),
        '"' | '\'' | '`' => m.text[i + 1..end].find(c).map(|n| i + 1 + n + 1),
        _ => Some(m.text[i..end].find(js_space).map_or(end, |n| i + n)),
    };
    match e {
        Some(e) => {
            m.hide(i, e);
            e
        }
        None => i,
    }
}

fn is_inline_tag(s: &str) -> bool {
    let Some(r) = s.strip_prefix('{') else {
        return false;
    };
    let r = skip_spaces(r);
    let Some(r) = r.strip_prefix('@') else {
        return false;
    };
    let lower = r.get(..12.min(r.len())).unwrap_or(r).to_ascii_lowercase();
    ["linkcode", "linkplain", "link", "tutorial", "inheritdoc"]
        .iter()
        .any(|t| {
            lower.starts_with(t)
                && r[t.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| js_space(c) || c == '}')
        })
}

fn has(words: &str, name: &str) -> bool {
    words.split(' ').any(|w| w == name)
}

/// JSDoc syntax: tag names, their types and names, metadata lines, and the targets of
/// `{@link ...}`-style inline tags.
fn mask_jsdoc(m: &mut Masked<'_>) {
    const TYPE_AND_NAME: &str = "param arg argument property prop typedef callback member var const constant class constructor module namespace";
    const TYPE_OR_VALUE: &str =
        "type this extends augments implements satisfies import package enum";
    const TYPE_ONLY: &str = "returns return throws exception yields yield private protected public";
    const NAME: &str = "access alias constructs function method event emits external exports fires func global host inner instance interface kind lends listens memberof mixes mixin name requires see template tutorial variation";
    const METADATA: &str =
        "author borrows copyright license version since date created modified default defaultvalue";
    let text = m.text;
    let ls = lines(text);
    let mut tags = Vec::new();
    for (k, &(s, e)) in ls.iter().enumerate() {
        let t = text[s..e].trim_start_matches(js_space);
        let t = match t.strip_prefix('*') {
            Some(r) => r.trim_start_matches(js_space),
            None => t,
        };
        let at = e - t.len();
        let Some(r) = t.strip_prefix('@') else {
            continue;
        };
        if !after(r, 0).is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let n = r
            .find(|c: char| !(c.is_alphanumeric() || c == '-'))
            .unwrap_or(r.len());
        let bang = r[n..].starts_with('!') as usize;
        let next = after(r, n + bang);
        if !next.is_none_or(|c| js_space(c) || c == '{') {
            continue;
        }
        tags.push((k, at, at + 1 + n + bang, r[..n].to_lowercase()));
    }
    for (j, (k, at, tag_end, name)) in tags.iter().enumerate() {
        let line_end = ls[*k].1;
        let region_end = tags.get(j + 1).map_or(text.len(), |t| ls[t.0].0);
        m.hide(*at, *tag_end);
        let name = name.as_str();
        if has(TYPE_AND_NAME, name) {
            let i = skip_hspace(text, *tag_end, region_end);
            if text[i..region_end].starts_with('{') {
                let e = mask_doc_type(m, i, region_end);
                if e != i {
                    mask_doc_value(m, e, region_end);
                }
            } else {
                let e = mask_doc_value(m, i, region_end);
                mask_doc_type(m, e, region_end);
            }
        } else if has(TYPE_OR_VALUE, name) {
            let i = skip_hspace(text, *tag_end, region_end);
            if mask_doc_type(m, i, region_end) == i {
                mask_doc_value(m, i, region_end);
            }
        } else if has(TYPE_ONLY, name) {
            mask_doc_type(m, *tag_end, region_end);
        } else if name == "template" {
            let i = mask_doc_type(m, *tag_end, region_end);
            mask_doc_value(m, i, region_end);
        } else if has(NAME, name) {
            mask_doc_value(m, *tag_end, line_end);
        } else if has(METADATA, name) {
            m.hide(*tag_end, line_end);
        }
    }
    // Inline tags: the tag name and its target (up to `|`, a space after it, or `}`).
    let mut i = 0;
    while let Some(n) = text[i..].find('{') {
        let open = i + n;
        i = open + 1;
        if !is_inline_tag(&text[open..]) {
            continue;
        }
        let at = open + text[open..].find('@').unwrap_or(0);
        let name_end = text[at + 1..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .map_or(text.len(), |k| at + 1 + k);
        m.hide(at, name_end);
        let end = line_end(text, open);
        let mut target_end = None;
        let mut seen = false;
        let mut close = end;
        let mut it = text[name_end..end].char_indices();
        while let Some((k, c)) = it.next() {
            let pos = name_end + k;
            match c {
                '\\' => {
                    it.next();
                }
                '|' if target_end.is_none() => target_end = Some(pos),
                '}' => {
                    close = pos;
                    break;
                }
                c if js_space(c) => {
                    if seen && target_end.is_none() {
                        target_end = Some(pos);
                    }
                }
                _ => seen = true,
            }
        }
        m.hide(name_end, target_end.unwrap_or(close));
        i = close.max(i);
    }
}

/// Backtick code spans and `"`/`'` strings (a `'` after a word character is an apostrophe);
/// an unclosed one runs to the end of its line.
fn mask_code_and_quotes(m: &mut Masked<'_>) {
    let text = m.text;
    let mut i = 0;
    while i < text.len() {
        let Some(c) = after(text, i) else { break };
        if m.is_hidden(i) {
            i += c.len_utf8();
            continue;
        }
        let end = if c == '`' {
            let size = text[i..].bytes().take_while(|&b| b == b'`').count();
            let mut j = i + size;
            let mut found = None;
            while j < text.len() {
                if text.as_bytes()[j] != b'`' || m.is_hidden(j) {
                    j += 1;
                    continue;
                }
                let n = text[j..].bytes().take_while(|&b| b == b'`').count();
                if n == size {
                    found = Some(j + n);
                    break;
                }
                j += n;
            }
            found
        } else if c == '"' || (c == '\'' && !before(text, i).is_some_and(uword)) {
            let mut j = i + 1;
            let mut found = None;
            while j < text.len() {
                let b = text.as_bytes()[j];
                if m.is_hidden(j) {
                    j += 1;
                } else if b == b'\\' {
                    j += 2;
                } else if b == c as u8 {
                    found = Some(j + 1);
                    break;
                } else {
                    j += 1;
                }
            }
            found
        } else {
            i += c.len_utf8();
            continue;
        };
        let end = end.unwrap_or_else(|| line_end(text, i)).min(text.len());
        m.hide(i, end);
        i = end.max(i + 1);
        while !text.is_char_boundary(i.min(text.len())) {
            i += 1;
        }
    }
}

fn is_ascii_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `scheme://...` (a scheme of up to 32 characters) and `www....`, up to a space.
fn mask_urls(m: &mut Masked<'_>) {
    let text = m.text;
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphabetic() || (i > 0 && is_ascii_word_byte(b[i - 1])) {
            i += 1;
            continue;
        }
        let run = b[i..]
            .iter()
            .take(33)
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
            .count();
        let body = if run <= 32 && text[i + run..].starts_with("://") {
            Some(i + run + 3)
        } else if text
            .get(i..i + 4)
            .is_some_and(|w| w.eq_ignore_ascii_case("www."))
        {
            Some(i + 4)
        } else {
            None
        };
        if let Some(from) = body {
            let end = text[from..].find(js_space).map_or(text.len(), |n| from + n);
            if end > from {
                m.hide(i, end);
                i = end;
                continue;
            }
        }
        i += 1;
    }
}

fn is_bare_domain(text: &str) -> bool {
    if text.len() > 253 || !text.contains('.') {
        return false;
    }
    let lower = text.to_lowercase();
    let parts: Vec<&str> = lower.split('.').collect();
    let (tld, rest) = parts.split_last().expect("split gives one part");
    *tld != "js"
        && (2..=63).contains(&tld.len())
        && tld.bytes().all(|c| c.is_ascii_lowercase())
        && rest.iter().all(|p| {
            (1..=63).contains(&p.len())
                && p.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

/// Space-separated tokens: `(start, end)`, split at characters `stop` accepts.
fn tokens(text: &str, stop: impl Fn(char) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (stop(c), start) {
            (true, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out
}

/// `example.com`, `api.github.com` (not `x.js`), and very long dotted tokens.
fn mask_domains(m: &mut Masked<'_>) {
    for (mut s, mut e) in tokens(m.text, js_space) {
        let t = m.text;
        while s < e && t[s..e].starts_with(['(', '[', '{', '<']) {
            s += 1;
        }
        while e > s && t[s..e].ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '}', '>']) {
            e -= 1;
        }
        let d = &t[s..e];
        if is_bare_domain(d) || (d.len() > 253 && d.contains('.')) {
            m.hide(s, e);
        }
    }
}

/// `application/json`, `image/svg+xml`, ...
fn mask_mime_types(m: &mut Masked<'_>) {
    const TOP: &[&str] = &[
        "application",
        "audio",
        "font",
        "image",
        "message",
        "model",
        "multipart",
        "text",
        "video",
    ];
    let text = m.text;
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if i > 0 && is_ascii_word_byte(b[i - 1]) {
            i += 1;
            continue;
        }
        let hit = TOP.iter().find_map(|t| {
            let r = text.get(i..i + t.len() + 1)?;
            (r.get(..t.len()).is_some_and(|w| w.eq_ignore_ascii_case(t)) && r.ends_with('/'))
                .then_some(i + t.len() + 1)
        });
        let Some(sub) = hit else {
            i += 1;
            continue;
        };
        let run = b[sub..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
            .count();
        // The longest run ending at a word boundary.
        let word_end = (sub + 1..=sub + run).rev().find(|&e| {
            b[e - 1].is_ascii_alphanumeric() && !b.get(e).is_some_and(|&c| is_ascii_word_byte(c))
        });
        match word_end {
            Some(end) if b[sub].is_ascii_alphanumeric() => {
                m.hide(i, end);
                i = end;
            }
            _ => i += 1,
        }
    }
}

/// `api/[id]`-free pairs of acronyms (`JSON/YAML`) are prose, not paths.
fn is_slash_pair_prose(text: &str) -> bool {
    let text = text.strip_suffix(['!', '.', '?']).unwrap_or(text);
    let parts: Vec<&str> = text.split('/').collect();
    let longest = DEFAULTS
        .iter()
        .filter(|d| d.1 == d.1.to_uppercase())
        .map(|d| d.0.len() - 4)
        .max()
        .unwrap_or(0);
    parts.len() == 2
        && parts.iter().all(|p| {
            p.len() <= longest
                && DEFAULTS
                    .iter()
                    .filter(|d| d.1 == d.1.to_uppercase())
                    .any(|d| regex_exec::test(&format!("^(?:{})$", d.0), "iu", p))
        })
}

fn is_package_specifier(text: &str) -> bool {
    if !text.contains('/') || text.contains("://") {
        return false;
    }
    let from = text.starts_with('@') as usize;
    let base = text[from..]
        .find(['#', '?', '@'])
        .map_or(text, |n| &text[..from + n]);
    let parts: Vec<&str> = base.split('/').collect();
    let part_ok = |p: &str| all_chars(p, |c| ident_char_ascii(c) || c == '-' || c == '.');
    let shape = match parts[0].strip_prefix('@') {
        Some(scope) => part_ok(scope) && parts[1..].iter().all(|p| part_ok(p)),
        None => parts.iter().all(|p| part_ok(p)),
    };
    parts.len() >= 2 && shape && !is_slash_pair_prose(text)
}

/// Package specifiers and paths (`@scope/name`, `src/api/index.js`).
fn mask_packages(m: &mut Masked<'_>) {
    let stop = |c: char| js_space(c) || "\"'`()[]{}<>,".contains(c);
    for (s, e) in tokens(m.text, stop) {
        let t = &m.text[s..e];
        if is_package_specifier(t) || (t.len() > 253 && t.contains('/')) {
            m.hide(s, e);
        }
    }
}

fn mask_markdown_links(m: &mut Masked<'_>) {
    for (s, e) in markdown_links(m.text, false) {
        let label = s + m.text[s..].find('[').unwrap_or(0);
        if !m.is_hidden(label) {
            m.hide(s, e);
        }
    }
}

/// `<tag ...>` and `</tag>` on one line.
fn mask_markup(m: &mut Masked<'_>) {
    let text = m.text;
    let mut i = 0;
    while let Some(n) = text[i..].find('<') {
        let at = i + n;
        i = at + 1;
        if m.is_hidden(at) {
            continue;
        }
        let r = &text[at + 1..];
        let name = r.strip_prefix('/').unwrap_or(r);
        if !after(name, 0).is_some_and(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        let end = line_end(text, at);
        let Some(close) = text[at + 1..end].find('>').map(|k| at + 1 + k) else {
            i = end + 1;
            continue;
        };
        let n = name
            .find(|c: char| !(ident_char_ascii(c) || c == '-' || c == ':'))
            .unwrap_or(name.len());
        let tail = &name[n..];
        if tail.starts_with(|c: char| js_space(c) && c != '\n')
            || tail.starts_with('>')
            || tail.starts_with("/>")
        {
            m.hide(at, close + 1);
        }
        i = close + 1;
    }
}

/// The match's neighbours make it a path, file name or extension (`api.js`, `src/api/x`,
/// `.svg`, `foo_api`, `C:\api`).
fn is_path_like(v: &str, s: usize, e: usize) -> bool {
    let prev = before(v, s);
    let next = after(v, e);
    let next2 = next.and_then(|c| after(v, e + c.len_utf8()));
    let before_ext = next == Some('.') && next2.is_some_and(uword);
    let before_path_ext = next == Some('/') && {
        let rest = &v[e + 1..];
        let path = &rest[..rest
            .find(|c: char| js_space(c) || "\"'`<>".contains(c))
            .unwrap_or(rest.len())];
        path.char_indices().any(|(k, c)| {
            c == '.' && after(path, k + 1).is_some_and(|d| d == '_' || d.is_alphanumeric())
        })
    };
    (v[s..e].contains('.') && prev == Some('/'))
        || (prev == Some('/') && (next == Some('/') || before_ext))
        || before_path_ext
        || matches!(prev, Some('.' | '_' | '\\'))
        || matches!(next, Some('_' | '\\'))
        || before_ext
}

/// The match is an object or property of a member access (`a.api(`, `api.get()`, `x[api]`).
fn is_property_access(v: &str, s: usize, e: usize) -> bool {
    let prev = before(v, s);
    let next = after(v, e);
    let ls = v[..s].rfind('\n').map_or(0, |n| n + 1);
    let le = line_end(v, e);
    let left = &v[ls..s];
    let right = &v[e..le];
    let dotted = v[s..e].contains('.');
    let is_property = !dotted && {
        let l = left.trim_end_matches(js_space);
        let l = l
            .strip_suffix("?.")
            .or_else(|| l.strip_suffix('.'))
            .map(|r| r.trim_end_matches(js_space));
        let r = right.trim_start_matches(js_space);
        l.is_some_and(|l| {
            let id = l.len() - l.trim_end_matches(ident_char).len();
            id > 0 && !before(l, l.len() - id).is_some_and(ident_char)
        }) && (r.is_empty() || r.starts_with(['(', '.', '[']))
    };
    let is_object = !dotted && member_access_follows(right);
    v[..s].ends_with("?.")
        || v[e..].starts_with("?.")
        || (prev == Some('[') && (before(v, s - 1).is_some_and(uword) || v[..s].ends_with("?.[")))
        || next == Some('[')
        || is_property
        || is_object
}

/// After a member access object: `.x(`, `.x.y`, `.x[`, `.x` at the end, `[k]`, ` [k].x`.
fn member_access_follows(right: &str) -> bool {
    let tail_ok = |r: &str| {
        let t = r.trim_start_matches(js_space);
        t.is_empty()
            || t.starts_with(['[', '('])
            || t.strip_prefix('.')
                .is_some_and(|x| after(x, 0).is_some_and(ident_char))
    };
    let dot = right
        .strip_prefix("?.")
        .or_else(|| right.strip_prefix('.'))
        .or_else(|| {
            let t = right.trim_start_matches(js_space);
            (t.len() < right.len())
                .then(|| t.strip_prefix('.'))
                .flatten()
        });
    if let Some(r) = dot
        && let Some((_, rest)) = take_ident(skip_spaces(r))
        && tail_ok(rest)
    {
        return true;
    }
    let r = right.strip_prefix("?.").map_or(right, skip_spaces);
    if let Some(rest) = take_bracket(r)
        && tail_ok(rest)
    {
        return true;
    }
    if right.starts_with(js_space)
        && let Some(rest) = take_bracket(skip_spaces(right))
    {
        let t = skip_spaces(rest);
        return t.starts_with('[')
            || t.strip_prefix('.')
                .is_some_and(|x| after(x, 0).is_some_and(ident_char));
    }
    false
}

fn skip_match(v: &str, s: usize, e: usize) -> bool {
    is_path_like(v, s, e)
        || is_property_access(v, s, e)
        || before(v, s) == Some('-')
        || matches!(after(v, e), Some('-' | '('))
}

/// Letters and numbers only, lowercased.
fn letters(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_mixed_case(s: &str) -> bool {
    s != s.to_lowercase() && s != s.to_uppercase()
}

impl CommentContent {
    /// The earliest replacement in one comment value: `(start, end, replacement)`.
    fn problem<'r>(&'r self, value: &str, jsdoc: bool) -> Option<(usize, usize, &'r str)> {
        let mut m = Masked {
            text: value,
            hidden: vec![false; value.len()],
        };
        mask_fences(&mut m);
        mask_examples(&mut m);
        if jsdoc {
            mask_jsdoc(&mut m);
        }
        mask_code_lines(&mut m);
        mask_code_and_quotes(&mut m);
        mask_urls(&mut m);
        mask_domains(&mut m);
        mask_mime_types(&mut m);
        mask_packages(&mut m);
        mask_markdown_links(&mut m);
        mask_markup(&mut m);
        for p in &self.ignore {
            for (s, e) in regex_exec::match_spans(&p.source, &p.flags, value).unwrap_or_default() {
                m.hide(s, e);
            }
        }
        let text = m.searchable();
        // `ſ` folds to `s` under `iu` but not under `to_lowercase`.
        let lower = (!text.contains('ſ')).then(|| text.to_lowercase());
        let mut best: Option<(usize, usize, &str)> = None;
        for r in &self.replacements {
            if let Some(req) = &r.required {
                let present = if r.case_sensitive {
                    text.contains(req.as_str())
                } else {
                    lower.as_ref().is_none_or(|l| l.contains(req.as_str()))
                };
                if !present {
                    continue;
                }
            }
            let flags = if r.case_sensitive { "gu" } else { "giu" };
            let spans = regex_exec::match_spans(&r.pattern, flags, &text).unwrap_or_default();
            for (s, e) in spans {
                if s == e {
                    continue;
                }
                let found = &text[s..e];
                if found == r.replacement || found.contains('\u{7f}') {
                    continue;
                }
                if r.unicode_bounds
                    && (before(&text, s).is_some_and(uword) || after(&text, e).is_some_and(uword))
                {
                    continue;
                }
                if !self.check_uniform_case
                    && letters(found) == letters(&r.replacement)
                    && !is_mixed_case(found)
                {
                    continue;
                }
                if skip_match(value, s, e) {
                    continue;
                }
                if best.is_none_or(|b| s < b.0) {
                    best = Some((s, e, r.replacement.as_str()));
                }
                break;
            }
        }
        best
    }
}

/// `/* eslint ... */`, `/* global ... */`, `/* exported ... */` and `eslint-env`.
fn is_inline_config(text: &str) -> bool {
    let t = text.trim_start_matches(js_space);
    ["eslint", "globals", "global", "exported"]
        .iter()
        .any(|w| starts_word(t, w).is_some())
}

impl Rule for CommentContent {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if ast.comments.is_empty() || self.replacements.is_empty() {
            return;
        }
        let src = cx.src();
        let directives: Vec<Span> = InlineConfig::parse(ast, src, cx.file.lines)
            .directives
            .iter()
            .map(|d| d.comment)
            .collect();
        let mut depth = 0;
        let mut prev_line_end: Option<usize> = None;
        for c in &ast.comments {
            let (start, end) = (c.span.start as usize, c.span.end as usize);
            if c.kind == CommentKind::Hashbang {
                depth = 0;
                prev_line_end = None;
                continue;
            }
            let line = c.kind == CommentKind::Line;
            let body_end = if line {
                end
            } else {
                end.saturating_sub(2).max(start + 2)
            };
            let value = &src[start + 2..body_end];
            if directives.contains(&c.span) || (!line && is_inline_config(value)) {
                depth = 0;
                prev_line_end = None;
                continue;
            }
            let adjacent = line
                && prev_line_end.is_some_and(|p| {
                    let gap = &src[p..start];
                    gap.matches('\n').count() == 1 && gap.trim_matches(js_space).is_empty()
                });
            if !adjacent {
                depth = 0;
            }
            if depth <= 0 {
                let jsdoc = src[start..].starts_with("/**") && !src[start..].starts_with("/***");
                if let Some((s, e, r)) = self.problem(value, jsdoc) {
                    let span = Span::new((start + 2 + s) as u32, (start + 2 + e) as u32);
                    let msg = format!("Prefer `{r}` over `{}`.", &value[s..e]);
                    cx.report(span, msg)
                        .id("comment-content")
                        .fix(Fix::replace(span, r.to_owned()));
                }
            }
            if line {
                depth = next_depth(depth, value);
                prev_line_end = Some(end);
            } else {
                depth = 0;
                prev_line_end = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(opts: &[Value], v: &str) -> Option<String> {
        let r = CommentContent::from_options(opts).unwrap();
        r.problem(v, false)
            .map(|(s, e, r)| format!("{}->{r}", &v[s..e]))
    }

    #[test]
    fn non_ascii_text_and_folding() {
        assert_eq!(problem(&[], " the ſql query").as_deref(), Some("ſql->SQL"));
        assert_eq!(problem(&[], " ast中 ast١ ast\u{301}er"), None);
        assert_eq!(
            problem(&[], " é “json” and github.").as_deref(),
            Some("json->JSON")
        );
    }

    #[test]
    fn code_paths_and_urls_are_skipped() {
        assert_eq!(problem(&[], " import api from './api.js';"), None);
        assert_eq!(
            problem(&[], " see https://github.com and src/api/x.js"),
            None
        );
        assert_eq!(
            problem(&[], " [docs](https://github.com) and github").as_deref(),
            Some("github->GitHub")
        );
    }

    #[test]
    fn pathological_lines_finish() {
        let long = format!(" {}", " [".repeat(20_000));
        assert_eq!(problem(&[], &long), None);
        let dotted = format!(" {}", "a.".repeat(20_000));
        assert_eq!(problem(&[], &dotted), None);
    }
}
