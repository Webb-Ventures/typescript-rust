//! `unicorn/no-invalid-boolean-attribute-value`: `el.setAttribute(name, value)` (two
//! arguments, a static member name) for an HTML boolean attribute (global ones like `inert`,
//! or one of the element-specific ones) with a static value that is neither `""` nor the
//! attribute's name (ASCII case-insensitive), on a receiver known to be a native HTML
//! element that has the attribute.
//!
//! The receiver's element types come from `document.createElement(tag)`,
//! `createElementNS(html, tag)` and `querySelector(selector)` on the global `document`
//! (or `window.document`, ... and `const` aliases; a selector must be one tag with only
//! class or id selectors, and `a`, `script`, `style`, `title` are ambiguous), from type
//! annotations and assertions naming native `HTML*Element` interfaces (through unions,
//! parentheses and type aliases, `null`/`undefined` ignored), and through variables. Any
//! other call on `document`, or a non-native tag, gives no types; type information is not
//! used. The suggestion sets `""`, or for `"false"` switches to `removeAttribute` and drops
//! the value, unless that would remove a comment.

use super::prelude::*;
use super::unicorn_ast::remove_argument;
use super::unicorn_static::{Static, is_global_ident, static_value_control_flow};
use super::unicorn_text::{paren_span, would_remove_comments};
use crate::ast_util::skip_trivia_backward;
use rustc_hash::FxHashSet;
use ts_r_ast::KeywordTypeKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-boolean-attribute-value",
        "Disallow invalid values for HTML boolean attributes.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoInvalidBooleanAttributeValue>,
};

#[derive(Debug, Default)]
struct NoInvalidBooleanAttributeValue;

const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const MAX_STEPS: u32 = 64;

/// Native HTML element interfaces and their tags (`HTMLElement` only for tags; it does not
/// name a native element type by itself).
const ELEMENT_TAGS: &[(&str, &str)] = &[
    ("HTMLAnchorElement", "a"),
    (
        "HTMLElement",
        "abbr address article aside b bdi bdo cite code dd dfn dt em figcaption figure footer header hgroup i kbd main mark nav noscript rp rt ruby s samp search section small strong sub summary sup u var wbr",
    ),
    ("HTMLAreaElement", "area"),
    ("HTMLAudioElement", "audio"),
    ("HTMLBaseElement", "base"),
    ("HTMLQuoteElement", "blockquote q"),
    ("HTMLBodyElement", "body"),
    ("HTMLBRElement", "br"),
    ("HTMLButtonElement", "button"),
    ("HTMLCanvasElement", "canvas"),
    ("HTMLTableCaptionElement", "caption"),
    ("HTMLTableColElement", "col colgroup"),
    ("HTMLDataElement", "data"),
    ("HTMLDataListElement", "datalist"),
    ("HTMLModElement", "del ins"),
    ("HTMLDetailsElement", "details"),
    ("HTMLDialogElement", "dialog"),
    ("HTMLDivElement", "div"),
    ("HTMLDListElement", "dl"),
    ("HTMLEmbedElement", "embed"),
    ("HTMLFieldSetElement", "fieldset"),
    ("HTMLFormElement", "form"),
    ("HTMLHeadingElement", "h1 h2 h3 h4 h5 h6"),
    ("HTMLHeadElement", "head"),
    ("HTMLHRElement", "hr"),
    ("HTMLHtmlElement", "html"),
    ("HTMLIFrameElement", "iframe"),
    ("HTMLImageElement", "img"),
    ("HTMLInputElement", "input"),
    ("HTMLLabelElement", "label"),
    ("HTMLLegendElement", "legend"),
    ("HTMLLIElement", "li"),
    ("HTMLLinkElement", "link"),
    ("HTMLMapElement", "map"),
    ("HTMLMenuElement", "menu"),
    ("HTMLMetaElement", "meta"),
    ("HTMLMeterElement", "meter"),
    ("HTMLObjectElement", "object"),
    ("HTMLOListElement", "ol"),
    ("HTMLOptGroupElement", "optgroup"),
    ("HTMLOptionElement", "option"),
    ("HTMLOutputElement", "output"),
    ("HTMLParagraphElement", "p"),
    ("HTMLPictureElement", "picture"),
    ("HTMLPreElement", "pre"),
    ("HTMLProgressElement", "progress"),
    ("HTMLScriptElement", "script"),
    ("HTMLSelectElement", "select"),
    ("HTMLSelectedContentElement", "selectedcontent"),
    ("HTMLSlotElement", "slot"),
    ("HTMLSourceElement", "source"),
    ("HTMLSpanElement", "span"),
    ("HTMLStyleElement", "style"),
    ("HTMLTableElement", "table"),
    ("HTMLTableSectionElement", "tbody tfoot thead"),
    ("HTMLTableCellElement", "td th"),
    ("HTMLTemplateElement", "template"),
    ("HTMLTextAreaElement", "textarea"),
    ("HTMLTimeElement", "time"),
    ("HTMLTitleElement", "title"),
    ("HTMLTableRowElement", "tr"),
    ("HTMLTrackElement", "track"),
    ("HTMLUListElement", "ul"),
    ("HTMLVideoElement", "video"),
];

const GLOBAL_BOOLEAN_ATTRIBUTES: &[&str] = &["autofocus", "headingreset", "inert", "itemscope"];

const BOOLEAN_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("allowfullscreen", &["HTMLIFrameElement"]),
    ("alpha", &["HTMLInputElement"]),
    ("async", &["HTMLScriptElement"]),
    ("autoplay", &["HTMLAudioElement", "HTMLVideoElement"]),
    ("checked", &["HTMLInputElement"]),
    (
        "controls",
        &["HTMLAudioElement", "HTMLVideoElement", "HTMLImageElement"],
    ),
    ("default", &["HTMLTrackElement"]),
    ("defer", &["HTMLScriptElement"]),
    (
        "disabled",
        &[
            "HTMLButtonElement",
            "HTMLInputElement",
            "HTMLOptGroupElement",
            "HTMLOptionElement",
            "HTMLSelectElement",
            "HTMLTextAreaElement",
            "HTMLFieldSetElement",
            "HTMLLinkElement",
        ],
    ),
    ("formnovalidate", &["HTMLButtonElement", "HTMLInputElement"]),
    ("ismap", &["HTMLImageElement"]),
    ("loop", &["HTMLAudioElement", "HTMLVideoElement"]),
    ("multiple", &["HTMLInputElement", "HTMLSelectElement"]),
    ("muted", &["HTMLAudioElement", "HTMLVideoElement"]),
    ("nomodule", &["HTMLScriptElement"]),
    ("novalidate", &["HTMLFormElement"]),
    ("open", &["HTMLDetailsElement", "HTMLDialogElement"]),
    ("playsinline", &["HTMLVideoElement"]),
    ("readonly", &["HTMLInputElement", "HTMLTextAreaElement"]),
    (
        "required",
        &[
            "HTMLInputElement",
            "HTMLSelectElement",
            "HTMLTextAreaElement",
        ],
    ),
    ("reversed", &["HTMLOListElement"]),
    ("selected", &["HTMLOptionElement"]),
    ("shadowrootclonable", &["HTMLTemplateElement"]),
    ("shadowrootcustomelementregistry", &["HTMLTemplateElement"]),
    ("shadowrootdelegatesfocus", &["HTMLTemplateElement"]),
    ("shadowrootserializable", &["HTMLTemplateElement"]),
];

type Types = FxHashSet<&'static str>;

fn type_of_tag(tag: &str) -> Option<&'static str> {
    ELEMENT_TAGS
        .iter()
        .find(|(_, tags)| tags.split(' ').any(|t| t == tag))
        .map(|&(ty, _)| ty)
}

fn is_native_type(name: &str) -> bool {
    name != "HTMLElement" && ELEMENT_TAGS.iter().any(|&(ty, _)| ty == name)
}

fn static_str(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    match static_value_control_flow(cx, id)? {
        Static::Str(s) => Some(s),
        _ => None,
    }
}

fn member_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => static_str(cx, index),
        _ => None,
    }
}

/// The single declaration of the symbol `id` names, with its plain-name initializer and
/// declared type.
struct Definition {
    symbol: SymbolId,
    is_const: bool,
    init: Option<NodeId>,
    ty: Option<NodeId>,
}

fn definition(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Definition> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let symbol = sem.symbol_of_ident(id)?;
    let [decl] = sem.symbol(symbol).decls.as_slice() else {
        return None;
    };
    let mut d = Definition {
        symbol,
        is_const: false,
        init: None,
        ty: None,
    };
    match *ast.node(decl.node) {
        Node::VarDeclarator {
            name,
            type_ann,
            init,
            ..
        } if name == decl.name => {
            d.init = init;
            d.ty = type_ann;
            d.is_const = cx.parent(decl.node).is_some_and(|p| {
                matches!(
                    ast.node(p),
                    Node::VarDecl {
                        kind: ts_r_ast::VarKind::Const,
                        ..
                    }
                )
            });
        }
        Node::Param {
            pattern, type_ann, ..
        } if pattern == decl.name => d.ty = type_ann,
        _ => {}
    }
    Some(d)
}

fn is_document(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > MAX_STEPS {
        return false;
    }
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => {
            if is_global_ident(cx, id, "document") {
                return true;
            }
            definition(cx, id)
                .filter(|d| d.is_const)
                .and_then(|d| d.init)
                .is_some_and(|init| is_document(cx, init, depth + 1))
        }
        Node::Member {
            obj,
            optional: false,
            ..
        }
        | Node::Index {
            obj,
            optional: false,
            ..
        } => {
            member_name(cx, id).as_deref() == Some("document")
                && ident_name(ast, obj)
                    .is_some_and(|n| matches!(n, "globalThis" | "self" | "window"))
                && ident_name(ast, obj).is_some_and(|n| is_global_ident(cx, obj, n))
        }
        _ => false,
    }
}

/// CSS `ident` unescaping (`\63` and `\c`).
fn css_decode(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
            hex.extend(chars.next());
        }
        if hex.is_empty() {
            out.push(chars.next()?);
        } else {
            let n = u32::from_str_radix(&hex, 16).ok()?;
            out.push(
                char::from_u32(n)
                    .filter(|&c| n != 0 && c != '\0')
                    .unwrap_or('\u{fffd}'),
            );
            if chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                chars.next();
            }
        }
    }
    Some(out)
}

/// An identifier token at the start of `s` (with escapes), and the rest.
fn css_ident(s: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    while end < s.len() {
        let c = s[end..].chars().next()?;
        if c == '\\' {
            end += 1;
            let hex = s[end..]
                .bytes()
                .take(6)
                .take_while(u8::is_ascii_hexdigit)
                .count();
            if hex == 0 {
                end += s[end..].chars().next()?.len_utf8();
            } else {
                end += hex;
                if s[end..].starts_with(|c: char| c.is_ascii_whitespace()) {
                    end += 1;
                }
            }
            continue;
        }
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then(|| (&s[..end], &s[end..]))
}

/// The tag of a selector that is one type selector with only class and id selectors.
fn selector_tag(selector: &str) -> Option<String> {
    let s = selector.trim_matches(|c: char| c.is_ascii_whitespace());
    let (tag, mut rest) = css_ident(s)?;
    if tag.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    for _ in 0..s.len() + 1 {
        if rest.is_empty() {
            break;
        }
        let r = rest.strip_prefix('.').or_else(|| rest.strip_prefix('#'))?;
        let (_, after) = css_ident(r)?;
        rest = after;
    }
    if !rest.is_empty() {
        return None;
    }
    let tag = css_decode(tag)?.to_ascii_lowercase();
    (!matches!(tag.as_str(), "a" | "script" | "style" | "title")).then_some(tag)
}

fn types_from_annotation(
    cx: &Ctx<'_, '_>,
    ty: NodeId,
    visited: &mut Vec<SymbolId>,
) -> Option<Types> {
    let ast = cx.ast();
    if visited.len() > MAX_STEPS as usize {
        return None;
    }
    match *ast.node(ty) {
        Node::ParenType { ty } => types_from_annotation(cx, ty, visited),
        Node::KeywordType {
            kind: KeywordTypeKind::Null | KeywordTypeKind::Undefined,
        } => Some(Types::default()),
        Node::UnionType { types } => {
            let mut out = Types::default();
            for &t in ast.list(types) {
                out.extend(types_from_annotation(cx, t, visited)?);
            }
            Some(out)
        }
        Node::TypeRef { name, .. } => {
            let n = ident_name(ast, name)?;
            let sem = cx.semantic();
            let Some(symbol) = sem.symbol_of_ident(name) else {
                let ty = ELEMENT_TAGS
                    .iter()
                    .find(|&&(t, _)| t == n)
                    .map(|&(t, _)| t)?;
                return is_native_type(ty).then(|| [ty].into_iter().collect());
            };
            let [decl] = sem.symbol(symbol).decls.as_slice() else {
                return None;
            };
            let Node::TypeAlias { ty: aliased, .. } = *ast.node(decl.node) else {
                return None;
            };
            if visited.contains(&symbol) {
                return None;
            }
            visited.push(symbol);
            let out = types_from_annotation(cx, aliased, visited);
            visited.pop();
            out
        }
        _ => None,
    }
}

fn receiver_types(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> Option<Types> {
    let ast = cx.ast();
    if visited.len() > MAX_STEPS as usize {
        return None;
    }
    match *ast.node(id) {
        Node::Paren { expr } | Node::NonNull { expr } | Node::Satisfies { expr, .. } => {
            receiver_types(cx, expr, visited)
        }
        Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => {
            types_from_annotation(cx, ty, &mut Vec::new())
        }
        Node::Ident { .. } => {
            let d = definition(cx, id);
            if let Some(d) = &d {
                if visited.contains(&d.symbol) {
                    return None;
                }
                if let Some(init) = d.init {
                    visited.push(d.symbol);
                    let types = receiver_types(cx, init, visited);
                    visited.pop();
                    if let Some(t) = types
                        && (d.is_const || t.is_empty())
                    {
                        return Some(t);
                    }
                }
            }
            types_from_annotation(cx, d?.ty?, &mut Vec::new())
        }
        Node::Call { callee, args, .. } => {
            let callee = skip_parens(ast, callee);
            let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
                return None;
            };
            let method = member_name(cx, callee)?;
            if !matches!(
                method.as_str(),
                "createElement" | "createElementNS" | "querySelector"
            ) {
                return None;
            }
            if !is_document(cx, obj, 0) {
                return Some(Types::default());
            }
            let args = ast.list(args);
            let tag = match (method.as_str(), args) {
                ("createElement", &[a]) => static_str(cx, a).map(|s| s.to_ascii_lowercase()),
                ("createElementNS", &[ns, a])
                    if static_str(cx, ns).as_deref() == Some(HTML_NAMESPACE) =>
                {
                    static_str(cx, a)
                }
                ("querySelector", &[a]) => static_str(cx, a).and_then(|s| selector_tag(&s)),
                _ => None,
            };
            Some(tag.as_deref().and_then(type_of_tag).into_iter().collect())
        }
        _ => None,
    }
}

impl Rule for NoInvalidBooleanAttributeValue {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee_inner = skip_parens(ast, callee);
        let &[name_node, value_node] = ast.list(args) else {
            return;
        };
        if ast
            .list(args)
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let (Node::Member { obj, prop, .. }
        | Node::Index {
            obj, index: prop, ..
        }) = *ast.node(callee_inner)
        else {
            return;
        };
        let computed = matches!(ast.node(callee_inner), Node::Index { .. });
        if member_name(cx, callee_inner).as_deref() != Some("setAttribute") {
            return;
        }
        let Some(attribute) = static_str(cx, name_node).map(|s| s.to_ascii_lowercase()) else {
            return;
        };
        let is_global = GLOBAL_BOOLEAN_ATTRIBUTES.contains(&attribute.as_str());
        let element_types = BOOLEAN_ATTRIBUTES
            .iter()
            .find(|(a, _)| *a == attribute)
            .map(|&(_, t)| t);
        if !is_global && element_types.is_none() {
            return;
        }
        let value = match static_value_control_flow(cx, value_node) {
            Some(
                v @ (Static::Str(_)
                | Static::Num(_)
                | Static::Bool(_)
                | Static::Null
                | Static::Undefined
                | Static::BigInt(_)),
            ) => v,
            _ => return,
        };
        let Some(value) = value.to_js_string().map(|s| s.to_ascii_lowercase()) else {
            return;
        };
        if value.is_empty() || value == attribute {
            return;
        }
        let Some(types) = receiver_types(cx, obj, &mut Vec::new()) else {
            return;
        };
        if types.is_empty()
            || (!is_global
                && types
                    .iter()
                    .any(|t| !element_types.is_some_and(|e| e.contains(t))))
        {
            return;
        }
        let value_span = paren_span(cx, value_node);
        let should_remove = value == "false";
        let removal = if should_remove {
            let at = skip_trivia_backward(ast, cx.src(), value_span.start as usize);
            let start = if at > 0 && cx.src().as_bytes()[at - 1] == b',' {
                at as u32 - 1
            } else {
                value_span.start
            };
            Span::new(start, cx.span(node).end.saturating_sub(1).max(start))
        } else {
            cx.span(skip_parens(ast, value_node))
        };
        let blocked = would_remove_comments(cx, removal, &[])
            || (should_remove && would_remove_comments(cx, cx.span(prop), &[]));
        let fix = if blocked {
            None
        } else if should_remove {
            let text = if computed {
                "'removeAttribute'"
            } else {
                "removeAttribute"
            };
            let prop_span = if computed {
                paren_span(cx, prop)
            } else {
                cx.span(prop)
            };
            remove_argument(cx, node, 1).map(|r| {
                r.edits
                    .into_iter()
                    .fold(Fix::replace(prop_span, text), |f, e| f.and(e.span, e.text))
            })
        } else {
            Some(Fix::replace(cx.span(skip_parens(ast, value_node)), "''"))
        };
        let msg = format!(
            "Invalid value for boolean attribute `{attribute}`. Use an empty string or the attribute name; remove the attribute to represent false."
        );
        let reported = skip_parens(ast, value_node);
        let r = cx
            .report_node(reported, msg)
            .id("no-invalid-boolean-attribute-value");
        if let Some(fix) = fix {
            let (id, desc) = if should_remove {
                (
                    "no-invalid-boolean-attribute-value/remove",
                    format!("Remove the `{attribute}` attribute."),
                )
            } else {
                (
                    "no-invalid-boolean-attribute-value/empty",
                    format!("Set the `{attribute}` attribute to an empty string."),
                )
            };
            r.suggest(id, desc, fix);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{css_decode, selector_tag};
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-invalid-boolean-attribute-value": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn selectors() {
        assert_eq!(selector_tag("button.a#b").as_deref(), Some("button"));
        assert_eq!(selector_tag(" \\62 utton ").as_deref(), Some("button"));
        assert_eq!(selector_tag("div > p"), None);
        assert_eq!(selector_tag("a.x"), None);
        assert_eq!(selector_tag("1a"), None);
        assert_eq!(selector_tag("p\\"), None);
        assert_eq!(css_decode("\\0").as_deref(), Some("\u{fffd}"));
    }

    #[test]
    fn cycles_and_malformed_input() {
        assert_eq!(
            lint("document.createElement('input').setAttribute('checked', 'false');"),
            1
        );
        assert_eq!(lint("let a = b, b = a; a.setAttribute('inert', 'no');"), 0);
        let chain: String = (0..300)
            .map(|i| format!("const e{} = e{};\n", i + 1, i))
            .collect();
        let src = format!(
            "const e0 = document.createElement('dialog');\n{chain}e300.setAttribute('open', 'x');"
        );
        assert_eq!(lint(&src), 0);
        for src in [
            "el.setAttribute('inert',",
            "document.querySelector('\\\\').setAttribute('inert', 'x')",
            "(x as HTMLInputElement).setAttribute(",
        ] {
            let _ = lint(src);
        }
    }
}
