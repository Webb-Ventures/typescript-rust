//! `jsdoc/type-formatting`: JSDoc types written in one house style.
//!
//! The type of each `@param`, `@property`, `@returns`, `@this`, `@throws`, `@type`,
//! `@typedef` and `@yields` tag (exact names, no aliases) is parsed in the configured mode
//! (a type that doesn't parse is left alone). The parser keeps only separators, brackets,
//! dots and quotes, so every spacing choice reads as the printer's default. A pre-order walk
//! compares each node with the options and, at the first difference per node, records a
//! message and sets that node's printing choice. When the result, wrapped in
//! `typeBracketSpacing`, differs from the written type, each message is reported (or, with
//! none, a generic one) at column 1 of the tag's line, with the reprinted type as the fix.

use super::prelude::*;
use crate::jsdoc_type::{self, FieldKey, Id, N, Quote, Sep, SigKind, TypeTree};
use crate::jsdoc_util::{each_block, rewire_type};
use ts_r_lexer::{is_id_part, is_id_start};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/type-formatting",
        "Formats JSDoc type values.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<TypeFormatting>,
};

const TAGS: &[&str] = &[
    "param", "property", "returns", "this", "throws", "type", "typedef", "yields",
];

#[derive(Debug)]
struct TypeFormatting {
    array_square: bool,
    arrow_post_return: String,
    arrow_pre_return: String,
    fixer: bool,
    fn_parameter: String,
    fn_post_generic: String,
    fn_post_return: String,
    fn_pre_return: String,
    fn_type_parameter: String,
    element: String,
    generic_dot: bool,
    post_colon: String,
    post_key: String,
    post_optional: String,
    post_variadic: String,
    method_quote: Quote,
    field_indent: String,
    /// `None` is `null`: quote only when the key needs it.
    field_quote: Option<Quote>,
    separator: String,
    optional_linebreak: bool,
    trailing: bool,
    bracket: String,
    default_value: String,
    post_method_name: String,
    post_new: String,
    single_separator: bool,
    string_quote: Quote,
    trailing_multiline_only: bool,
    type_bracket: String,
    union: String,
}

fn quote_of(s: Option<&str>, default: Quote) -> Quote {
    match s {
        Some("single") => Quote::Single,
        Some("double") => Quote::Double,
        _ => default,
    }
}

fn quote_name(q: Option<Quote>) -> &'static str {
    match q {
        Some(Quote::Single) => "single",
        Some(Quote::Double) => "double",
        _ => "null",
    }
}

impl FromOptions for TypeFormatting {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let s = |k: &str, d: &str| str_field(o, k).unwrap_or(d).to_owned();
        let b = |k: &str, d: bool| bool_field(o, k, d);
        Ok(TypeFormatting {
            array_square: str_field(o, "arrayBrackets") != Some("angle"),
            arrow_post_return: s("arrowFunctionPostReturnMarkerSpacing", " "),
            arrow_pre_return: s("arrowFunctionPreReturnMarkerSpacing", " "),
            fixer: b("enableFixer", true),
            fn_parameter: s("functionOrClassParameterSpacing", " "),
            fn_post_generic: s("functionOrClassPostGenericSpacing", ""),
            fn_post_return: s("functionOrClassPostReturnMarkerSpacing", " "),
            fn_pre_return: s("functionOrClassPreReturnMarkerSpacing", ""),
            fn_type_parameter: s("functionOrClassTypeParameterSpacing", " "),
            element: s("genericAndTupleElementSpacing", " "),
            generic_dot: b("genericDot", false),
            post_colon: s("keyValuePostColonSpacing", " "),
            post_key: s("keyValuePostKeySpacing", ""),
            post_optional: s("keyValuePostOptionalSpacing", ""),
            post_variadic: s("keyValuePostVariadicSpacing", ""),
            method_quote: quote_of(str_field(o, "methodQuotes"), Quote::Double),
            field_indent: s("objectFieldIndent", ""),
            field_quote: match str_field(o, "objectFieldQuote") {
                Some(q) => Some(quote_of(Some(q), Quote::None)).filter(|q| *q != Quote::None),
                None => None,
            },
            separator: s("objectFieldSeparator", "comma"),
            optional_linebreak: b("objectFieldSeparatorOptionalLinebreak", true),
            trailing: b("objectFieldSeparatorTrailingPunctuation", false),
            bracket: s("objectTypeBracketSpacing", ""),
            default_value: s("parameterDefaultValueSpacing", " "),
            post_method_name: s("postMethodNameSpacing", ""),
            post_new: s("postNewSpacing", " "),
            single_separator: b("separatorForSingleObjectField", false),
            string_quote: quote_of(str_field(o, "stringQuotes"), Quote::Double),
            trailing_multiline_only: b("trailingPunctuationMultilineOnly", false),
            type_bracket: s("typeBracketSpacing", ""),
            union: s("unionSpacing", " "),
        })
    }
}

fn sep_name(s: Sep) -> &'static str {
    match s {
        Sep::Comma => "comma",
        Sep::Semicolon => "semicolon",
        Sep::Linebreak => "linebreak",
        Sep::CommaLinebreak => "comma-and-linebreak",
        Sep::SemicolonLinebreak => "semicolon-and-linebreak",
    }
}

fn sep_from(s: &str) -> Option<Sep> {
    Some(match s {
        "comma" => Sep::Comma,
        "semicolon" => Sep::Semicolon,
        "linebreak" => Sep::Linebreak,
        "comma-and-linebreak" => Sep::CommaLinebreak,
        "semicolon-and-linebreak" => Sep::SemicolonLinebreak,
        _ => return None,
    })
}

fn is_identifier(k: &str) -> bool {
    let mut cs = k.chars();
    cs.next().is_some_and(is_id_start) && cs.all(is_id_part)
}

/// `1`, `1.`, `1.5`, `.5`, each with an optional exponent.
fn is_number(k: &str) -> bool {
    let b = k.as_bytes();
    let digits = |i: usize| b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut i = digits(0);
    if i > 0 {
        if b.get(i) == Some(&b'.') {
            i += 1 + digits(i + 1);
        }
    } else if b.first() == Some(&b'.') && digits(1) > 0 {
        i = 1 + digits(1);
    } else {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let d = digits(i);
        if d == 0 {
            return false;
        }
        i += d;
    }
    i == b.len()
}

impl TypeFormatting {
    /// Checks node `id`, adjusting how it prints; the message for its first difference.
    fn check(&self, t: &mut TypeTree, id: Id) -> Option<String> {
        let node = t.nodes[id].clone();
        match node {
            N::Signature { kind, .. } => {
                if matches!(kind, SigKind::Construct) && self.post_new != " " {
                    t.meta_mut(id).post_new = Some(self.post_new.clone());
                    return Some(format!(
                        "Post-`new` spacing should be \"{}\"",
                        self.post_new
                    ));
                }
                if let SigKind::Method { name, quote } = &kind {
                    if !self.post_method_name.is_empty() {
                        t.meta_mut(id).post_method_name = Some(self.post_method_name.clone());
                        return Some(format!(
                            "Post-method-name spacing should be \"{}\"",
                            self.post_method_name
                        ));
                    }
                    if *quote != Quote::None && *quote != self.method_quote {
                        let name = name.clone();
                        if let N::Signature { kind, .. } = &mut t.nodes[id] {
                            *kind = SigKind::Method {
                                name,
                                quote: self.method_quote,
                            };
                        }
                        return Some(format!(
                            "Method quoting style should be \"{}\"",
                            quote_name(Some(self.method_quote))
                        ));
                    }
                }
                self.function_spacing(t, id)
            }
            N::Function { arrow: true, .. } => {
                if self.arrow_post_return != " " {
                    t.meta_mut(id).post_return = Some(self.arrow_post_return.clone());
                    Some(format!(
                        "Post-return-marker spacing should be \"{}\"",
                        self.arrow_post_return
                    ))
                } else if self.arrow_pre_return != " " {
                    t.meta_mut(id).pre_return = Some(self.arrow_pre_return.clone());
                    Some(format!(
                        "Pre-return-marker spacing should be \"{}\"",
                        self.arrow_pre_return
                    ))
                } else {
                    None
                }
            }
            N::Function { .. } | N::ComputedMethod { .. } => self.function_spacing(t, id),
            N::Generic {
                left, square, dot, ..
            } => {
                if matches!(&t.nodes[left], N::Name(n) if n == "Array") {
                    if square == self.array_square {
                        return None;
                    }
                    if let N::Generic { square, .. } = &mut t.nodes[id] {
                        *square = self.array_square;
                    }
                    let style = if self.array_square { "square" } else { "angle" };
                    Some(format!("Array bracket style should be {style}"))
                } else if dot != self.generic_dot {
                    if let N::Generic { dot, .. } = &mut t.nodes[id] {
                        *dot = self.generic_dot;
                    }
                    Some(format!("Dot usage should be {}", self.generic_dot))
                } else {
                    self.element_spacing(t, id)
                }
            }
            N::KeyValue { variadic, .. } => {
                let m = t.meta_mut(id);
                if !self.post_key.is_empty() {
                    m.post_key = Some(self.post_key.clone());
                    Some(format!("Post key spacing should be \"{}\"", self.post_key))
                } else if self.post_colon != " " {
                    m.post_colon = Some(self.post_colon.clone());
                    Some(format!(
                        "Post colon spacing should be \"{}\"",
                        self.post_colon
                    ))
                } else if !self.post_optional.is_empty() {
                    m.post_optional = Some(self.post_optional.clone());
                    Some(format!(
                        "Post optional (`?`) spacing should be \"{}\"",
                        self.post_optional
                    ))
                } else if variadic && !self.post_variadic.is_empty() {
                    m.post_variadic = Some(self.post_variadic.clone());
                    Some(format!(
                        "Post variadic (`...`) spacing should be \"{}\"",
                        self.post_variadic
                    ))
                } else {
                    None
                }
            }
            N::Object { elements, sep } => {
                let wanted = self.separator.as_str();
                let current = sep_name(sep);
                let separator_differs = current != wanted
                    && !(self.optional_linebreak
                        && wanted.ends_with("-linebreak")
                        && wanted.starts_with(current));
                let indent_differs = current.ends_with("-linebreak") && self.field_indent != "  ";
                let trailing_differs = self.trailing
                    && (!self.trailing_multiline_only || t.stringify_at(id).contains('\n'));
                if separator_differs || self.single_separator || indent_differs || trailing_differs
                {
                    let next = if self.optional_linebreak && !current.ends_with("and-linebreak") {
                        wanted.strip_suffix("-and-linebreak").unwrap_or(wanted)
                    } else {
                        wanted
                    };
                    if let Some(next) = sep_from(next) {
                        t.nodes[id] = N::Object {
                            elements,
                            sep: next,
                        };
                    }
                    let m = t.meta_mut(id);
                    m.separator_for_single = self.single_separator;
                    m.property_indent = Some(self.field_indent.clone());
                    m.trailing_punctuation = self.trailing;
                    Some(format!("Inconsistent {wanted} separator usage"))
                } else {
                    // Upstream adjusts the spacing without a message of its own.
                    if !self.bracket.is_empty() {
                        t.meta_mut(id).bracket_spacing = Some(self.bracket.clone());
                    }
                    None
                }
            }
            N::ObjectField { key, quote, .. } => {
                let quotable = match &key {
                    FieldKey::Text(k) => {
                        (self.field_quote.is_some() || is_identifier(k)) && !is_number(k)
                    }
                    FieldKey::Node(_) => self.field_quote.is_some(),
                };
                if quotable && quote != self.field_quote.unwrap_or(Quote::None) {
                    if let N::ObjectField { quote, .. } = &mut t.nodes[id] {
                        *quote = self.field_quote.unwrap_or(Quote::None);
                    }
                    return Some(format!(
                        "Inconsistent object field quotes {}",
                        quote_name(self.field_quote)
                    ));
                }
                let m = t.meta_mut(id);
                if !self.post_key.is_empty() {
                    m.post_key = Some(self.post_key.clone());
                    Some(format!("Post key spacing should be \"{}\"", self.post_key))
                } else if self.post_colon != " " {
                    m.post_colon = Some(self.post_colon.clone());
                    Some(format!(
                        "Post colon spacing should be \"{}\"",
                        self.post_colon
                    ))
                } else if !self.post_optional.is_empty() {
                    m.post_optional = Some(self.post_optional.clone());
                    Some(format!(
                        "Post optional (`?`) spacing should be \"{}\"",
                        self.post_optional
                    ))
                } else {
                    None
                }
            }
            N::Str { value, quote } if quote != self.string_quote => {
                t.nodes[id] = N::Str {
                    value,
                    quote: self.string_quote,
                };
                Some(format!(
                    "Inconsistent {} string quotes usage",
                    quote_name(Some(self.string_quote))
                ))
            }
            N::Tuple(_) => self.element_spacing(t, id),
            N::TypeParameter {
                default: Some(_), ..
            } if self.default_value != " " => {
                t.meta_mut(id).default_value_spacing = Some(self.default_value.clone());
                Some(format!(
                    "Default value spacing should be \"{}\"",
                    self.default_value
                ))
            }
            N::Union(_) if self.union != " " => {
                t.meta_mut(id).spacing = Some(self.union.clone());
                Some(format!(
                    "Inconsistent \"{}\" union spacing usage",
                    self.union
                ))
            }
            _ => None,
        }
    }

    fn element_spacing(&self, t: &mut TypeTree, id: Id) -> Option<String> {
        if self.element == " " {
            return None;
        }
        t.meta_mut(id).element_spacing = Some(self.element.clone());
        Some(format!("Element spacing should be \"{}\"", self.element))
    }

    /// Parameter, generic and return-marker spacing of a function or signature.
    fn function_spacing(&self, t: &mut TypeTree, id: Id) -> Option<String> {
        let m = t.meta_mut(id);
        let (slot, value, what) = if self.fn_parameter != " " {
            (&mut m.parameter_spacing, &self.fn_parameter, "Parameter")
        } else if !self.fn_post_generic.is_empty() {
            (&mut m.post_generic, &self.fn_post_generic, "Post-generic")
        } else if self.fn_post_return != " " {
            (
                &mut m.post_return,
                &self.fn_post_return,
                "Post-return-marker",
            )
        } else if !self.fn_pre_return.is_empty() {
            (&mut m.pre_return, &self.fn_pre_return, "Pre-return-marker")
        } else if self.fn_type_parameter != " " {
            (
                &mut m.type_parameter_spacing,
                &self.fn_type_parameter,
                "Type parameter",
            )
        } else {
            return None;
        };
        *slot = Some(value.clone());
        Some(format!("{what} spacing should be \"{value}\""))
    }
}

impl Rule for TypeFormatting {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            for ti in 0..doc.block.tags.len() {
                let tag = &doc.block.tags[ti];
                if tag.typ.is_empty() || !TAGS.contains(&tag.tag.as_str()) {
                    continue;
                }
                let typ = tag.typ.clone();
                let Some(mut tree) =
                    jsdoc_type::parse_for_setting(&typ, &finder.settings.mode, false)
                else {
                    continue;
                };
                let mut messages = Vec::new();
                let tb = self.type_bracket.as_str();
                if !tb.is_empty() && (!typ.starts_with(tb) || !typ.ends_with(tb)) {
                    messages.push(format!("Must have initial and final \"{tb}\" spacing"));
                } else if tb.is_empty() && typ.trim() != typ {
                    messages.push("Must have no initial spacing".to_owned());
                }
                for (id, _, _) in tree.walk() {
                    messages.extend(self.check(&mut tree, id));
                }
                let printed = tree.stringify();
                if typ == format!("{tb}{printed}{tb}") {
                    continue;
                }
                if messages.is_empty() {
                    messages.push("There was an error with type formatting".to_owned());
                }
                let fix = self.fixer.then(|| {
                    rewire_type(&mut doc.block, ti, &printed, &doc.indent, tb);
                    doc.fix()
                });
                let span = doc.line_span(finder, doc.block.tag_line(ti));
                for message in messages {
                    let r = cx.report(span, message);
                    if let Some(f) = &fix {
                        r.fix(f.clone());
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsdoc_type::Mode;

    /// The messages and the reprinted type for `typ` under `opts`.
    fn format(opts: Value, typ: &str) -> (Vec<String>, String) {
        let r = TypeFormatting::from_options(&[opts]).unwrap();
        let mut t = jsdoc_type::parse(typ, Mode::Typescript, false).unwrap();
        let mut msgs = Vec::new();
        for (id, _, _) in t.walk() {
            msgs.extend(r.check(&mut t, id));
        }
        (msgs, t.stringify())
    }

    #[test]
    fn spacing_options_reprint() {
        let (m, s) = format(
            serde_json::json!({"unionSpacing": "", "objectFieldSeparator": "semicolon"}),
            "{a: string|number, 'b': 'x'}",
        );
        assert_eq!(
            m,
            [
                "Inconsistent semicolon separator usage",
                "Inconsistent \"\" union spacing usage",
                "Inconsistent object field quotes null",
                "Inconsistent double string quotes usage",
            ]
        );
        assert_eq!(s, "{a: string|number; b: \"x\"}");
        let (m, s) = format(
            serde_json::json!({"arrowFunctionPreReturnMarkerSpacing": ""}),
            "(a: string) => void",
        );
        assert_eq!(m, ["Pre-return-marker spacing should be \"\""]);
        assert_eq!(s, "(a: string)=> void");
    }

    #[test]
    fn number_and_unicode_keys() {
        assert!(is_number("1.5e+3") && is_number(".5") && is_number("1."));
        assert!(!is_number("1e") && !is_number(".") && !is_number("1.5.") && !is_number(""));
        assert!(is_identifier("ключ") && !is_identifier("a-b"));
        let (m, _) = format(serde_json::json!({}), "{'1': string, 'a-b': number}");
        assert!(m.is_empty());
    }
}
