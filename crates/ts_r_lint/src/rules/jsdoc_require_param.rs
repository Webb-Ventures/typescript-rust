//! `jsdoc/require-param`: every function parameter (and, for destructured parameters, every
//! property path under the root) has a `@param` tag.
//!
//! What the plugin does, in our words:
//! - Blocks that `avoidDocs` (replacing tags, constructors unless `checkConstructors`,
//!   getters/setters unless asked, `exemptedBy`) or have `@type` are skipped.
//! - Plain names are looked up among the `@param` names (`this` never needs one). A rest
//!   parameter expects its name, typed `{...any}` in the fix.
//! - A destructured root takes its own name, or the name of the undotted `@param` at its
//!   position (skipped when that tag has a type outside `checkTypesPattern`), or the next
//!   generated name (`unnamedRootBase` entries, then the last one with a counter from
//!   `autoIncrementBase`). The root itself and every flattened property path under it
//!   are required (rest properties only with `checkRestProperty`; paths under a tag with a
//!   non-object type aren't).
//! - The fix inserts one line per missing tag in a fixed order, mutating the same block:
//!   a root that fills an unnamed `@param` replaces that line; anything else goes before
//!   the first existing tag documenting a later parameter, else after the `@param` lines.
//!   The position is a count of tag lines used as a tag index too (the plugin's own
//!   arithmetic, kept so fixes match). A one-line block is made multi-line first.

use std::collections::HashMap;

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder, Tag, Target, Tokens};
use crate::jsdoc_util::{
    ContextOpts, Flat, Inner, ItemName, ParamName, drop_path_quotes, exempted_by, flatten_roots,
    function_param_names_with, has_exempting_tag, regex_string_matches, replaced_docs, same_path,
    target_params,
};

const DEFAULT_TYPES: &str = "/^(?:[oO]bject|[aA]rray|PlainObject|Generic(?:Object|Array))$/";

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-param",
        "Requires that all function parameters are documented with a `@param` tag.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<RequireParam>,
};

#[derive(Debug)]
struct RequireParam {
    ctx: ContextOpts,
    auto_increment_base: i64,
    check_constructors: bool,
    check_getters: bool,
    check_setters: bool,
    check_destructured: bool,
    check_destructured_roots: bool,
    check_rest_property: bool,
    check_types: String,
    enable_fixer: bool,
    enable_rest_element_fixer: bool,
    enable_root_fixer: bool,
    ignore_when_all_missing: bool,
    interface_exempts: bool,
    unnamed_root_base: Vec<String>,
    use_default_object_properties: bool,
}

impl FromOptions for RequireParam {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut ctx = ContextOpts::parse(opts)?;
        ctx.no_tracking = true;
        let o = &ctx.opts;
        let flag = |k: &str, d: bool| bool_field(o, k, d);
        let unnamed_root_base = match o.get("unnamedRootBase").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => vec!["root".to_owned()],
        };
        Ok(RequireParam {
            auto_increment_base: o
                .get("autoIncrementBase")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            check_constructors: flag("checkConstructors", true),
            check_getters: flag("checkGetters", false),
            check_setters: flag("checkSetters", false),
            check_destructured: flag("checkDestructured", true),
            check_destructured_roots: flag("checkDestructuredRoots", true),
            check_rest_property: flag("checkRestProperty", false),
            check_types: str_field(o, "checkTypesPattern")
                .unwrap_or(DEFAULT_TYPES)
                .to_owned(),
            enable_fixer: flag("enableFixer", true),
            enable_rest_element_fixer: flag("enableRestElementFixer", true),
            enable_root_fixer: flag("enableRootFixer", true),
            ignore_when_all_missing: flag("ignoreWhenAllParamsMissing", false),
            interface_exempts: flag("interfaceExemptsParamsCheck", false),
            unnamed_root_base,
            use_default_object_properties: flag("useDefaultObjectProperties", false),
            ctx,
        })
    }
}

/// The generated names for unnamed roots: the base names in turn, and once one is left,
/// that one with a counter.
struct RootNamer {
    bases: Vec<String>,
    counter: i64,
}

impl RootNamer {
    /// The next name and whether it was counted.
    fn next(&mut self) -> (String, bool) {
        if self.bases.len() <= 1 {
            let base = self
                .bases
                .first()
                .cloned()
                .unwrap_or_else(|| "undefined".to_owned());
            let n = self.counter;
            self.counter += 1;
            (format!("{base}{n}"), true)
        } else {
            (self.bases.remove(0), false)
        }
    }
}

/// One tag to add.
struct Missing {
    /// Index into the flattened parameter names (or, with `remove`, into the `@param` tags).
    idx: Option<usize>,
    name: String,
    /// The name came from the root namer's counter.
    inc: bool,
    /// Replace the unnamed `@param` at `idx`.
    remove: bool,
    typ: Option<&'static str>,
}

/// The parameters of what a block documents, as `getFunctionParameterNames` finds them: the
/// node's own `params`, its function-typed annotation's, or its `value`'s.
fn doc_params(f: &Finder<'_>, t: Option<Target>) -> Option<ts_r_ast::NodeList> {
    let Some(Target::Node(id)) = t else {
        return None;
    };
    let ast = f.ast;
    if let Some(p) = target_params(ast, id) {
        return Some(p);
    }
    if let Node::FunctionType { params, .. } = *ast.node(id) {
        return Some(params);
    }
    let value = match *ast.node(id) {
        Node::Property { value, .. } => Some(value),
        Node::PropertyDef { value, .. } => value,
        _ => None,
    }?;
    let v = skip_parens(ast, value);
    match *ast.node(v) {
        Node::FunctionExpr { params, .. } | Node::Arrow { params, .. } => Some(params),
        _ => None,
    }
}

/// `interfaceExemptsParamsCheck`: the function initialises a variable with a type
/// annotation.
fn typed_declarator(f: &Finder<'_>, t: Option<Target>) -> bool {
    let Some(Target::Node(mut id)) = t else {
        return false;
    };
    for _ in 0..64 {
        let Some(p) = f.parents.parent(id) else {
            return false;
        };
        match *f.ast.node(p) {
            Node::Paren { .. } => id = p,
            Node::VarDeclarator { type_ann, init, .. } => {
                return init == Some(id) && type_ann.is_some();
            }
            _ => return false,
        }
    }
    false
}

impl RequireParam {
    /// `avoidDocs` with this rule's special-method options.
    fn avoid(&self, f: &Finder<'_>, doc: &Doc) -> bool {
        if replaced_docs(f, doc) {
            return true;
        }
        let b = &doc.block;
        let own = match doc.target {
            Some(Target::Node(id)) => match *f.ast.node(id) {
                Node::MethodDef { kind, .. } => Some(kind),
                _ => None,
            },
            _ => None,
        };
        let parent = match doc.target {
            Some(Target::Node(id)) => f.parents.parent(id).and_then(|p| match *f.ast.node(p) {
                Node::MethodDef { kind, value, .. } if value == id => Some(kind),
                _ => None,
            }),
            _ => None,
        };
        let ctor = own == Some(MethodKind::Constructor) || parent == Some(MethodKind::Constructor);
        if !self.check_constructors && (ctor || b.has_any_tag(&["class", "constructor"])) {
            return true;
        }
        if parent == Some(MethodKind::Get) && !self.check_getters {
            return true;
        }
        if parent == Some(MethodKind::Set) && !self.check_setters {
            return true;
        }
        let exempted = exempted_by(&self.ctx.opts, f.settings.mode == "closure");
        has_exempting_tag(b, &exempted)
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, f: &Finder<'_>, doc: &mut Doc) {
        if self.avoid(f, doc) || doc.block.has_tag("type") {
            return;
        }
        if self.interface_exempts && typed_declarator(f, doc.target) {
            return;
        }
        let Some(tag_name) = doc.preferred(cx, f, "param") else {
            return;
        };
        let Some(params) = doc_params(f, doc.target) else {
            return;
        };
        let names = function_param_names_with(
            f.ast,
            f.src,
            params,
            self.use_default_object_properties,
            self.interface_exempts,
        );
        if names.is_empty() {
            return;
        }
        // (name, type) of every `@param`, and the undotted ones.
        let jsdoc: Vec<(String, String)> = doc
            .block
            .tags_named(&tag_name)
            .map(|(_, t)| (t.name.clone(), t.typ.clone()))
            .collect();
        if self.ignore_when_all_missing && jsdoc.is_empty() {
            return;
        }
        let shallow: Vec<&(String, String)> =
            jsdoc.iter().filter(|(n, _)| !n.contains('.')).collect();
        let types_match = |ty: &str| regex_string_matches(&self.check_types, ty);
        let flattened = flatten_roots(&names, "").names;
        let mut index: HashMap<String, usize> = HashMap::new();
        for (i, n) in flattened.iter().enumerate() {
            index.insert(drop_path_quotes(n), i);
        }
        let get_index = |n: &str| index.get(&drop_path_quotes(n)).copied();

        let mut missing: Vec<Missing> = Vec::new();
        let mut namer = RootNamer {
            bases: self.unnamed_root_base.clone(),
            counter: self.auto_increment_base,
        };
        let (mut next_root, mut incremented) = namer.next();
        let this_offset =
            usize::from(matches!(names.first(), Some(ParamName::Name(n)) if n == "this"));
        let empty_flat = Flat::default();
        for (pi, p) in names.iter().enumerate() {
            match p {
                ParamName::Root(own, inner) => {
                    let flat = match &**inner {
                        Inner::Flat(fl) => fl,
                        Inner::Items(_) => &empty_flat,
                    };
                    let matched = pi.checked_sub(this_offset).and_then(|i| shallow.get(i));
                    let mut inc = false;
                    let root = if let Some(n) = own {
                        n.clone()
                    } else if let Some((n, ty)) = matched.filter(|(n, _)| !n.is_empty()) {
                        if !ty.is_empty() && !types_match(ty) {
                            continue;
                        }
                        n.clone()
                    } else {
                        inc = incremented;
                        next_root.clone()
                    };
                    (next_root, incremented) = namer.next();
                    if (!self.enable_rest_element_fixer && flat.has_rest_element)
                        || !self.check_destructured_roots
                    {
                        continue;
                    }
                    let mut not_checking: Vec<String> = Vec::new();
                    for (i, pname) in flat.names.iter().enumerate() {
                        if !jsdoc.iter().any(|(n, _)| *n == root)
                            && !missing.iter().any(|m| m.name == root)
                        {
                            match jsdoc.iter().position(|(n, _)| n.is_empty()) {
                                Some(e) => missing.push(Missing {
                                    idx: Some(e),
                                    name: root.clone(),
                                    inc,
                                    remove: true,
                                    typ: None,
                                }),
                                None => missing.push(Missing {
                                    idx: get_index(&root).or_else(|| get_index(pname)),
                                    name: root.clone(),
                                    inc,
                                    remove: false,
                                    typ: None,
                                }),
                            }
                        }
                        if !self.check_destructured {
                            continue;
                        }
                        if !self.check_rest_property && flat.rests.get(i).copied().unwrap_or(false)
                        {
                            continue;
                        }
                        let full = format!("{root}.{pname}");
                        if let Some((n, _)) = jsdoc.iter().find(|(n, ty)| {
                            same_path(n, &full) && !ty.is_empty() && !types_match(ty)
                        }) {
                            not_checking.push(n.clone());
                        }
                        if not_checking.iter().any(|n| full.starts_with(n.as_str())) {
                            continue;
                        }
                        if !jsdoc.iter().any(|(n, _)| same_path(n, &full)) {
                            let at = if own.is_some() { full.as_str() } else { pname };
                            missing.push(Missing {
                                idx: get_index(at),
                                name: full.clone(),
                                inc,
                                remove: false,
                                typ: (flat.has_rest_element && !flat.has_property_rest)
                                    .then_some("{...any}"),
                            });
                        }
                    }
                }
                ParamName::Item {
                    name, rest_element, ..
                } => {
                    if !self.enable_rest_element_fixer && *rest_element {
                        continue;
                    }
                    let shown = name.display();
                    let found = match name {
                        ItemName::Str(s) => jsdoc.iter().any(|(n, _)| n == s),
                        _ => false,
                    };
                    if !found {
                        missing.push(Missing {
                            idx: get_index(&shown),
                            name: shown,
                            inc: false,
                            remove: false,
                            typ: Some("{...any}"),
                        });
                    }
                }
                ParamName::Name(n) => {
                    if n != "this" && !jsdoc.iter().any(|(t, _)| t == n) {
                        missing.push(Missing {
                            idx: get_index(n),
                            name: n.clone(),
                            inc: false,
                            remove: false,
                            typ: None,
                        });
                    }
                }
                ParamName::Missing => {}
            }
        }
        if missing.is_empty() {
            return;
        }
        if doc.block.source.len() == 1 {
            make_multiline(&mut doc.block, &doc.indent);
        }
        let fix = self.enable_fixer.then(|| {
            let mut ed = Editor {
                block: doc.block.clone(),
                new_add: vec![false; doc.block.tags.len()],
                tag_name: &tag_name,
                flattened: &flattened,
            };
            for m in &missing {
                if m.inc && !self.enable_root_fixer {
                    continue;
                }
                ed.add(m);
            }
            Fix::replace(doc.comment, ed.block.stringify())
        });
        for m in &missing {
            let msg = format!("Missing JSDoc @{tag_name} \"{}\" declaration.", m.name);
            let r = cx.report(doc.comment, msg);
            if let Some(fix) = &fix {
                r.fix(fix.clone());
            }
        }
    }
}

/// `makeMultiline`: a one-line block becomes `/**`, its content line and the closing line.
fn make_multiline(b: &mut Block, indent: &str) {
    let first = b.source[0];
    let t = b.lines[first].tokens.clone();
    let (mut post_name, mut post_type, mut post_tag) = (t.post_name, t.post_type, t.post_tag);
    if t.description.is_empty() {
        if !post_name.is_empty() {
            post_name.clear();
        } else if !post_type.is_empty() {
            post_type.clear();
        } else if !post_tag.is_empty() {
            post_tag.clear();
        }
    }
    let line_end = t.line_end.clone();
    let delimiter = b.lines[first].tokens.delimiter.clone();
    b.lines[first].tokens = Tokens {
        delimiter,
        ..Tokens::default()
    };
    let content = Tokens {
        start: format!("{indent} "),
        delimiter: "*".to_owned(),
        post_delimiter: t.post_delimiter,
        tag: t.tag,
        post_tag,
        typ: t.typ,
        post_type,
        name: t.name,
        post_name,
        description: t.description.trim_end().to_owned(),
        ..Tokens::default()
    };
    let closing = Tokens {
        start: format!("{indent} "),
        end: "*/".to_owned(),
        line_end,
        ..Tokens::default()
    };
    let n = b.lines[first].number;
    let a = b.new_line(n + 1, content);
    let c = b.new_line(n + 2, closing);
    b.source.push(a);
    b.source.push(c);
}

/// JavaScript's `splice` start: negative counts from the end, clamped to the length.
fn splice_at(i: i64, len: usize) -> usize {
    if i < 0 {
        (len as i64 + i).max(0) as usize
    } else {
        (i as usize).min(len)
    }
}

/// The fixer's state: the block it rewrites and which tags it added.
struct Editor<'a> {
    block: Block,
    new_add: Vec<bool>,
    tag_name: &'a str,
    flattened: &'a [String],
}

impl Editor<'_> {
    fn add(&mut self, m: &Missing) {
        let b = &self.block;
        let offset = b
            .source
            .iter()
            .position(|&id| {
                let t = &b.lines[id].tokens;
                !t.tag.is_empty() || !t.end.is_empty()
            })
            .map_or(-1, |i| i as i64);
        if m.remove {
            let i = m.idx.unwrap_or(0) as i64;
            self.insert(m, i, offset + i, 1);
            return;
        }
        let (found, count) = self.expected_index(m.idx);
        let at_tag = format!("@{}", self.tag_name);
        let first_param = b
            .source
            .iter()
            .position(|&id| b.lines[id].tokens.tag == at_tag);
        let base = match first_param {
            Some(p) if !found => p as i64,
            _ => offset,
        };
        self.insert(m, count, base + count, 0);
    }

    /// `findExpectedIndex`: whether an existing tag documents a parameter after `idx`, and
    /// the number of open lines in the tags before it (or in all `@param` tags).
    fn expected_index(&self, idx: Option<usize>) -> (bool, i64) {
        let b = &self.block;
        let from = (idx.unwrap_or(0) + 1).min(self.flattened.len());
        let remaining = &self.flattened[from..];
        let found = b.tags.iter().enumerate().position(|(i, t)| {
            !self.new_add.get(i).copied().unwrap_or(false)
                && remaining
                    .iter()
                    .any(|r| t.name == *r || t.name.ends_with(&format!(".{r}")))
        });
        let tags: Vec<&Tag> = match found {
            Some(f) => b.tags[..f].iter().collect(),
            None => b.tags.iter().filter(|t| t.tag == self.tag_name).collect(),
        };
        let count = tags
            .iter()
            .flat_map(|t| t.source.iter())
            .filter(|&&id| b.lines[id].tokens.end.is_empty())
            .count() as i64;
        (found.is_some(), count)
    }

    /// `createTokens`: a new `@param` line at source index `at` (replacing `count` lines)
    /// and a new tag at tag index `tag_at`.
    fn insert(&mut self, m: &Missing, tag_at: i64, at: i64, count: usize) {
        let b = &mut self.block;
        let at = splice_at(at, b.source.len());
        let start = b
            .source
            .get(at)
            .map(|&id| b.lines[id].tokens.start.clone())
            .unwrap_or_default();
        let typ = m.typ.unwrap_or("").to_owned();
        let tokens = Tokens {
            start,
            delimiter: "*".to_owned(),
            post_delimiter: " ".to_owned(),
            tag: format!("@{}", self.tag_name),
            post_tag: " ".to_owned(),
            post_type: if typ.is_empty() { "" } else { " " }.to_owned(),
            typ: typ.clone(),
            name: m.name.clone(),
            ..Tokens::default()
        };
        let first_number = b.source.first().map_or(0, |&id| b.lines[id].number);
        let id = b.new_line(at as i64 + 1, tokens);
        let tag = Tag {
            tag: self.tag_name.to_owned(),
            name: m.name.clone(),
            typ,
            description: String::new(),
            source: vec![id],
            problems: Vec::new(),
        };
        let tag_at = splice_at(tag_at, b.tags.len());
        let tag_end = (tag_at + count).min(b.tags.len());
        b.tags.splice(tag_at..tag_end, [tag]);
        let flag_end = (tag_at + count).min(self.new_add.len());
        self.new_add
            .splice(tag_at.min(self.new_add.len())..flag_end, [true]);
        let end = (at + count).min(b.source.len());
        b.source.splice(at..end, [id]);
        for (i, &line) in b.source[at..].iter().enumerate() {
            b.lines[line].number = first_number + (at + i) as i64;
        }
    }
}

impl Rule for RequireParam {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        self.ctx.each_doc(cx, |cx, f, doc| self.check(cx, f, doc));
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::engine::lint_source;
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};

    /// The rule's messages and the first report's fixed text.
    fn run(code: &str) -> (Vec<String>, Option<String>) {
        let cfg = serde_json::json!([{
            "files": ["**/*.ts"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
            "rules": { "jsdoc/require-param": "error" },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        let r = lint_source("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0));
        let ds: Vec<_> = r
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("jsdoc/require-param"))
            .collect();
        let fixed = ds.first().and_then(|d| d.fix.as_ref()).map(|f| {
            let mut s = code.to_owned();
            for e in f.edits.iter().rev() {
                s.replace_range(e.span.start as usize..e.span.end as usize, &e.text);
            }
            s
        });
        (ds.iter().map(|d| d.message.clone()).collect(), fixed)
    }

    #[test]
    fn one_line_block_becomes_multiline() {
        let (msgs, fixed) = run("/** Doc. */\nfunction f(a, {b}) {}\n");
        assert_eq!(
            msgs,
            [
                "Missing JSDoc @param \"a\" declaration.",
                "Missing JSDoc @param \"root0\" declaration.",
                "Missing JSDoc @param \"root0.b\" declaration.",
            ]
        );
        assert_eq!(
            fixed.as_deref(),
            Some(
                "/**\n * Doc.\n * @param a\n * @param root0\n * @param root0.b\n */\nfunction f(a, {b}) {}\n"
            )
        );
    }

    #[test]
    fn deep_destructuring_does_not_overflow() {
        let depth = 3000;
        let code = format!(
            "/** Doc. */\nfunction f({}) {{}}\n",
            "{a:".repeat(depth) + "x" + &"}".repeat(depth)
        );
        let _ = run(&code);
    }
}
