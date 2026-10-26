//! Variable dynamic imports: `` import(`./dir/${name}.js`) `` becomes a lookup in an
//! `import.meta.glob` over the files the template can name, as Vite's dev server does
//! (`vite: packages/vite/src/node/plugins/dynamicImportVars.ts`, which builds the glob with
//! `@rollup/plugin-dynamic-import-vars`). The glob is then expanded like a written one, so
//! the imported files join the module graph and late plugin hooks see the expanded code.
//!
//! The rules, in our words:
//! - only a template literal that is the whole argument, with at least one substitution,
//!   and whose text starts with `.` (aliased heads are resolved to URLs elsewhere);
//! - the glob is the template's text with each substitution replaced by `*` (a string
//!   literal substitution stays literal), glob characters in the text escaped; `**` is
//!   folded to `*`;
//! - the import is left alone when the text holds a `*`, the glob has no `*`, or it does
//!   not start with `./` or `../`, globs its own directory (`./*.js`) or has no extension;
//! - a trailing query (`?raw`) leaves the glob and becomes its `query` option (with
//!   `import: '*'` for `?raw`, `?url` and workers);
//! - the call becomes `helper(glob, `normalized template`, segment count)`; the helper
//!   picks the glob entry the runtime path names, or rejects with Vite's message.

use std::path::Path;

use crate::render::js_string;
use crate::scan::TemplateImport;

/// Where the runtime helper is served (Vite's `\0vite/dynamic-import-helper.js` id).
pub const HELPER_URL: &str = "/@id/__x00__vite/dynamic-import-helper.js";

/// The name the helper is imported as.
pub const HELPER_NAME: &str = "__variableDynamicImportRuntimeHelper";

/// The helper module: looks the runtime path up in the glob's object.
pub fn helper() -> String {
    concat!(
        "export default (glob, path, segments) => {\n",
        "  const entry = glob[path];\n",
        "  if (entry) return typeof entry === \"function\" ? entry() : Promise.resolve(entry);\n",
        "  return new Promise((_, reject) => {\n",
        "    const later = typeof queueMicrotask === \"function\" ? queueMicrotask : setTimeout;\n",
        "    const deep = path.split(\"/\").length !== segments\n",
        "      ? \". Note that variables only represent file names one level deep.\"\n",
        "      : \"\";\n",
        "    later(() => reject(new Error(\"Unknown variable dynamic import: \" + path + deep)));\n",
        "  });\n",
        "};\n",
    )
    .to_owned()
}

/// The rewritten module: `code` with the calls replaced and the helper import (`prelude`,
/// on line 1) prepended; `edits` are `(start, end, replacement)` in the original code.
#[derive(Clone, Debug, Default)]
pub struct Rewritten {
    pub code: String,
    pub prelude: String,
    pub edits: Vec<(usize, usize, String)>,
}

/// Rewrites the variable dynamic imports in `code` (found by the scanner) for a module at
/// `importer`. `None` when none of them qualifies.
pub fn rewrite(code: &str, calls: &[TemplateImport], importer: &Path) -> Option<Rewritten> {
    let importer_dir = {
        let id = crate::paths::id(importer);
        match id.rfind('/') {
            Some(i) => id[..i].to_owned(),
            None => String::new(),
        }
    };
    let mut edits = Vec::new();
    for call in calls {
        let (s, e) = (call.start as usize, call.end as usize);
        let (ts, te) = (call.template_start as usize, call.template_end as usize);
        if !(s <= ts && ts + 2 <= te && te <= e && e <= code.len()) {
            continue;
        }
        let Some(text) = code.get(ts + 1..te - 1) else {
            continue;
        };
        let Some(lookup) = glob_lookup(text, &importer_dir) else {
            continue;
        };
        let newlines = code[s..e].bytes().filter(|&b| b == b'\n').count();
        let mut replacement = format!("{HELPER_NAME}({lookup})");
        replacement.extend(std::iter::repeat_n('\n', newlines));
        edits.push((s, e, replacement));
    }
    if edits.is_empty() {
        return None;
    }
    let prelude = format!("import {HELPER_NAME} from {};", js_string(HELPER_URL));
    let mut out = String::with_capacity(code.len() + prelude.len() + 64 * edits.len());
    out.push_str(&prelude);
    let mut at = 0;
    for (s, e, t) in &edits {
        out.push_str(&code[at..*s]);
        out.push_str(t);
        at = *e;
    }
    out.push_str(&code[at..]);
    Some(Rewritten {
        code: out,
        prelude,
        edits,
    })
}

/// The helper's arguments for the template text `text` (between the backticks):
/// `(import.meta.glob("<glob>"<, options>)), `<path>`, <segments>`.
fn glob_lookup(text: &str, importer_dir: &str) -> Option<String> {
    if !text.starts_with('.') {
        return None;
    }
    let glob = template_glob(text)?;
    if !glob.contains('*') {
        return None;
    }
    let glob = glob.replace("**", "*");
    if !(glob.starts_with("./") || glob.starts_with("../")) || own_directory(&glob) {
        return None;
    }
    if extension(&glob).is_empty() {
        return None;
    }
    let pattern = match query_split(&glob) {
        // The query's `?` is escaped in the glob: drop the backslash with it.
        Some(i) if i > 0 && glob.as_bytes()[i - 1] == b'\\' => &glob[..i - 1],
        Some(i) => &glob[..i],
        None => &glob[..],
    };
    let (raw_path, query) = match query_split(text) {
        Some(i) => {
            let rest = &text[i + 1..];
            // Only the text up to a second query mark, as a two-piece split keeps.
            let search = query_split(rest).map_or(rest, |j| &rest[..j]);
            (&text[..i], Some(format!("?{search}")))
        }
        None => (text, None),
    };
    let options = match &query {
        Some(q) if namespace_query(q) => {
            format!(", {{\"query\":{},\"import\":\"*\"}}", js_string(q))
        }
        Some(q) => format!(", {{\"query\":{}}}", js_string(q)),
        None => String::new(),
    };
    let path = normalized_relative(importer_dir, raw_path);
    let segments = path.split('/').count();
    Some(format!(
        "(import.meta.glob({}{options})), `{path}`, {segments}",
        js_string(pattern)
    ))
}

/// The template text as a glob: literal parts escaped, substitutions as `*` (or their text
/// when they are a plain string). `None` when a literal part holds a `*`.
fn template_glob(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut literal_start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'$' if b.get(i + 1) == Some(&b'{') => {
                out.push_str(&escape_literal(text.get(literal_start..i)?)?);
                let close = substitution_end(b, i + 2)?;
                let expr = text.get(i + 2..close)?.trim();
                match plain_string(expr) {
                    Some(s) => out.push_str(&escape_literal(s)?),
                    None => out.push('*'),
                }
                i = close + 1;
                literal_start = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&escape_literal(text.get(literal_start..)?)?);
    Some(out)
}

/// The `}` closing the substitution whose text starts at `from` (braces counted, strings
/// and nested templates skipped).
fn substitution_end(b: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'}' if depth == 0 => return Some(i),
            b'}' => depth -= 1,
            b'{' => depth += 1,
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The value of `expr` when it is a string or template literal without escapes or
/// substitutions.
fn plain_string(expr: &str) -> Option<&str> {
    let q = expr.chars().next()?;
    if !matches!(q, '"' | '\'' | '`') || expr.len() < 2 || !expr.ends_with(q) {
        return None;
    }
    let inner = &expr[1..expr.len() - 1];
    let escaped = inner.contains(q) || inner.contains('\\');
    let substituted = q == '`' && inner.contains("${");
    (!escaped && !substituted).then_some(inner)
}

/// One literal part of the template as glob text: glob characters escaped with `\`
/// (as tinyglobby's POSIX `escapePath` does). `None` when it holds a `*`.
fn escape_literal(s: &str) -> Option<String> {
    if s.contains('*') {
        return None;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let escaped_already = i > 0 && chars[i - 1] == '\\';
        let special = match c {
            '(' | ')' | '[' | ']' | '{' | '}' | '?' | '|' => true,
            '!' if i == 0 => true,
            '!' | '+' | '@' => next == Some('('),
            '\\' => !next.is_some_and(|n| "()[]{}!*+?@|".contains(n)),
            _ => false,
        };
        if special && !escaped_already {
            out.push('\\');
        }
        out.push(c);
    }
    Some(out)
}

/// Where the query starts: the first `?` with no `/`, `|` or `}` after it.
fn query_split(s: &str) -> Option<usize> {
    s.char_indices()
        .filter(|&(_, c)| c == '?')
        .map(|(i, _)| i)
        .find(|&i| !s[i + 1..].contains(['/', '|', '}']))
}

/// Whether the query is `?raw`, `?url`, `?worker` or `?sharedworker`, which Vite's glob
/// asks to import as the whole namespace (`import: '*'`).
fn namespace_query(query: &str) -> bool {
    query
        .trim_start_matches('?')
        .split('&')
        .any(|p| matches!(p, "raw" | "url" | "worker" | "sharedworker"))
}

/// `./*.ext`: a glob over the importer's own directory, which Vite refuses.
fn own_directory(glob: &str) -> bool {
    glob.strip_prefix("./*.").is_some_and(|ext| {
        !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The extension of the last path segment (`""` when it has none), like `path.extname`.
fn extension(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rfind('.') {
        Some(i) if i > 0 && base != ".." => &base[i..],
        _ => "",
    }
}

/// `raw` (relative to `dir`) joined to `dir`, normalized and made relative to `dir` again,
/// starting with `./` or `../`.
fn normalized_relative(dir: &str, raw: &str) -> String {
    let mut joined: Vec<&str> = Vec::new();
    for seg in dir.split('/').chain(raw.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                joined.pop();
            }
            s => joined.push(s),
        }
    }
    let base: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let common = base.iter().zip(&joined).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = std::iter::repeat_n("..", base.len() - common).collect();
    parts.extend(&joined[common..]);
    let rel = parts.join("/");
    if rel.starts_with("./") || rel.starts_with("../") {
        rel
    } else {
        format!("./{rel}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(text: &str) -> Option<String> {
        glob_lookup(text, "/root/src")
    }

    #[test]
    fn substitutions_become_stars() {
        assert_eq!(
            lookup("./dir/${name}.js").as_deref(),
            Some("(import.meta.glob(\"./dir/*.js\")), `./dir/${name}.js`, 3")
        );
        assert_eq!(
            lookup("../views/${a}/${b}.vue").as_deref(),
            Some("(import.meta.glob(\"../views/*/*.vue\")), `../views/${a}/${b}.vue`, 4")
        );
        assert_eq!(
            lookup("./x/${'lit'}${n}.js").as_deref(),
            Some("(import.meta.glob(\"./x/lit*.js\")), `./x/${'lit'}${n}.js`, 3")
        );
        // `${a}${b}` folds to one star.
        assert_eq!(
            lookup("./x/${a}${b}.js").as_deref(),
            Some("(import.meta.glob(\"./x/*.js\")), `./x/${a}${b}.js`, 3")
        );
    }

    #[test]
    fn query_becomes_an_option() {
        assert_eq!(
            lookup("./dir/${n}.js?raw").as_deref(),
            Some(
                "(import.meta.glob(\"./dir/*.js\", {\"query\":\"?raw\",\"import\":\"*\"})), `./dir/${n}.js`, 3"
            )
        );
        assert_eq!(
            lookup("./dir/${n}.css?inline").as_deref(),
            Some(
                "(import.meta.glob(\"./dir/*.css\", {\"query\":\"?inline\"})), `./dir/${n}.css`, 3"
            )
        );
    }

    #[test]
    fn path_is_normalized_against_the_importer() {
        assert_eq!(
            lookup("../src/dir/${n}.js").as_deref(),
            Some("(import.meta.glob(\"../src/dir/*.js\")), `./dir/${n}.js`, 3")
        );
        assert_eq!(
            normalized_relative("/root/src", "./a/../b/${x}.js"),
            "./b/${x}.js"
        );
        assert_eq!(
            normalized_relative("/root/src", "../../up/${x}.js"),
            "../../up/${x}.js"
        );
    }

    #[test]
    fn glob_characters_are_escaped() {
        assert_eq!(
            lookup("./(group)/${n}.js").as_deref(),
            Some("(import.meta.glob(\"./\\\\(group\\\\)/*.js\")), `./(group)/${n}.js`, 3")
        );
    }

    #[test]
    fn imports_vite_leaves_alone() {
        for text in [
            "./dir/name.js",   // no substitution, no star
            "./${n}.js",       // its own directory
            "./dir/${n}",      // no extension
            "./dir/*${n}.js",  // a star in the text
            "/abs/${n}.js",    // absolute
            "@/alias/${n}.js", // aliased (resolved elsewhere)
            "${base}/x.js",    // starts with a substitution
            "dir/${n}.js",     // bare
        ] {
            assert_eq!(lookup(text), None, "{text}");
        }
    }

    #[test]
    fn rewrites_calls_and_prepends_the_helper() {
        let code = "const n = 'a';\nexport const m = import(`./dir/${n}.js`);\nimport(`./x.js`);";
        let calls = crate::scan::scan(code).template_imports;
        let r = rewrite(code, &calls, Path::new("/root/src/main.js")).unwrap();
        assert_eq!(
            r.code,
            "import __variableDynamicImportRuntimeHelper from \"/@id/__x00__vite/dynamic-import-helper.js\";const n = 'a';\nexport const m = __variableDynamicImportRuntimeHelper((import.meta.glob(\"./dir/*.js\")), `./dir/${n}.js`, 3);\nimport(`./x.js`);"
        );
        assert_eq!(r.edits.len(), 1);
    }

    #[test]
    fn malformed_templates_do_not_panic() {
        for text in ["./${", "./${'a}", "./${`}", ".\\", "./é${n}.js", "./${n}.é"] {
            let _ = lookup(text);
        }
        let code = "import(`./${a`)";
        let calls = crate::scan::scan(code).template_imports;
        let _ = rewrite(code, &calls, Path::new("/r/m.js"));
    }
}
