//! Name helpers shared by the round-6 unicorn naming rules (`filename-case`,
//! `name-replacements`): regular-expression options and the word splitting and case
//! conversions of the `change-case` package, as those rules use them.

use serde_json::Value;

/// A regular expression from rule options: a string (compiled with the `u` flag) or a
/// `RegExp` as the config loader and oracle data write it (`{ "regex", "flags" }`).
#[derive(Debug, Clone)]
pub struct RegexOpt {
    pub source: String,
    pub flags: String,
}

impl RegexOpt {
    pub fn from_value(v: &Value) -> Option<RegexOpt> {
        match v {
            Value::String(s) => Some(RegexOpt {
                source: s.clone(),
                flags: "u".to_owned(),
            }),
            Value::Object(m) => Some(RegexOpt {
                source: m.get("regex")?.as_str()?.to_owned(),
                flags: m
                    .get("flags")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            }),
            _ => None,
        }
    }

    /// `regex.test(text)` (a pattern that does not parse never matches).
    pub fn test(&self, text: &str) -> bool {
        // `test` looks for the first match; `g`/`y` only change where a reused regex starts.
        let flags: String = self.flags.chars().filter(|&c| c != 'g').collect();
        crate::regex_exec::match_all(&self.source, &flags, text).is_some_and(|m| !m.is_empty())
    }
}

/// Reads a list of patterns; `Err` when an item is neither a string nor a regular expression.
pub fn regex_list(v: Option<&Value>, name: &str) -> Result<Vec<RegexOpt>, String> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let list = v
        .as_array()
        .ok_or_else(|| format!("`{name}` must be an array"))?;
    list.iter()
        .map(|p| {
            RegexOpt::from_value(p).ok_or_else(|| {
                format!("The `{name}` option only accepts strings and regular expressions.")
            })
        })
        .collect()
}

fn is_lower(c: char) -> bool {
    c.is_lowercase()
}
fn is_upper(c: char) -> bool {
    c.is_uppercase()
}

/// `change-case`'s word split: a break between a lowercase letter or digit and an uppercase
/// letter, and inside an uppercase run before its last letter when a lowercase letter follows
/// (`FAQPage` is `FAQ`, `Page`); anything that is not a letter or digit separates words.
pub fn split_words(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.trim().chars().collect();
    // Breaks before index i.
    let mut brk = vec![false; chars.len() + 1];
    let mut i = 0;
    while i + 1 < chars.len() {
        if (is_lower(chars[i]) || chars[i].is_ascii_digit()) && is_upper(chars[i + 1]) {
            brk[i + 1] = true;
            i += 2;
        } else {
            i += 1;
        }
    }
    let mut i = 0;
    while i + 2 < chars.len() {
        if is_upper(chars[i]) && is_upper(chars[i + 1]) && is_lower(chars[i + 2]) {
            brk[i + 1] = true;
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let word_char = c.is_alphabetic() || c.is_ascii_digit();
        if (brk[i] || !word_char) && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        if word_char {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn pascal_word(word: &str, index: usize, out: &mut String) {
    let mut cs = word.chars();
    let Some(first) = cs.next() else { return };
    if index > 0 && first.is_ascii_digit() {
        out.push('_');
        out.push(first);
    } else {
        out.extend(first.to_uppercase());
    }
    out.push_str(&cs.as_str().to_lowercase());
}

/// `change-case` `camelCase`.
pub fn camel_case(s: &str) -> String {
    let mut out = String::new();
    for (i, w) in split_words(s).iter().enumerate() {
        if i == 0 {
            out.push_str(&w.to_lowercase());
        } else {
            pascal_word(w, i, &mut out);
        }
    }
    out
}

/// `change-case` `pascalCase`.
pub fn pascal_case(s: &str) -> String {
    let mut out = String::new();
    for (i, w) in split_words(s).iter().enumerate() {
        pascal_word(w, i, &mut out);
    }
    out
}

/// `change-case` `kebabCase` (`delim` `-`) and `snakeCase` (`_`).
pub fn delimited_case(s: &str, delim: &str) -> String {
    split_words(s)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(delim)
}

/// `a`, `a or b`, `a, b, or c` (`Intl.ListFormat` `en-US` disjunction).
pub fn disjunction(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases() {
        assert_eq!(split_words("FAQPage"), ["FAQ", "Page"]);
        assert_eq!(pascal_case("FAQpage"), "FaQpage");
        assert_eq!(camel_case("foo_bar"), "fooBar");
        assert_eq!(camel_case("a_1b"), "a_1b");
        assert_eq!(delimited_case("fooBar", "-"), "foo-bar");
        assert_eq!(split_words("__"), Vec::<String>::new());
    }
}
