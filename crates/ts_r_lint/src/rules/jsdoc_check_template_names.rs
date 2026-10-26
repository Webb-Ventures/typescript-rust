//! `jsdoc/check-template-names`: every `@template` name is used by what the block documents.
//!
//! Names count as used when they appear as a type name (`JsdocTypeName`) in: the `@param`
//! and `@returns` types of a block with `@callback` or `@function`; the `@typedef` type and
//! `@property` types of a block with exactly one `@typedef`; otherwise, for the node the
//! block documents (directly or through `export`/`export default`), the node's own type
//! parameters, plus the `@param`/`@returns` types for a function declaration, or the
//! `@param`/`@returns`/`@type` types of the blocks of a class's members; and the
//! `@augments`/`@extends`/`@implements` types. A type that does not parse in the mode
//! (`permissive` tries each) adds nothing. A `@template` tag may list several names
//! (`@template A, B`), split on top-level commas; `[T=x]` and `T=x` give `T`.

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder};
use crate::jsdoc_type::{self, Mode, N};
use crate::jsdoc_util::each_block;
use std::collections::BTreeSet;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-template-names",
        "Checks that any `@template` names are actually used in the connected `@typedef` or type alias.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<CheckTemplateNames>,
};

#[derive(Debug, Default)]
struct CheckTemplateNames;

/// `splitTopLevelCommas`.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0u32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if c == ',' && depth == 0 {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

/// `parseClosureTemplateTag`: each name, `[T=x]` / `T=x` cut to `T`.
fn template_names(name: &str) -> Vec<String> {
    split_top_level_commas(name)
        .iter()
        .map(|p| {
            let t = p.trim();
            // `^\[?(.*?)=.*$` (no line breaks): drop a `[` and everything from the first `=`.
            match t.find('=') {
                Some(i) if !t.contains(['\n', '\r', '\u{2028}', '\u{2029}']) => {
                    let head = &t[..i];
                    head.strip_prefix('[').unwrap_or(head).to_owned()
                }
                _ => t.to_owned(),
            }
        })
        .collect()
}

struct Used<'m> {
    mode: &'m str,
    names: BTreeSet<String>,
}

impl Used<'_> {
    fn add_type(&mut self, ty: &str) {
        let tree = match self.mode {
            "permissive" => jsdoc_type::try_parse(ty, false),
            m => match Mode::from_setting(m) {
                Some(m) => jsdoc_type::parse(ty, m, false),
                None => None,
            },
        };
        let Some(tree) = tree else { return };
        for (id, _, _) in tree.walk() {
            if let N::Name(v) = &tree.nodes[id] {
                self.names.insert(v.clone());
            }
        }
    }

    fn add_tags(&mut self, b: &Block, name: Option<&str>) {
        let Some(name) = name else { return };
        for t in b.tags.iter().filter(|t| t.tag == name) {
            self.add_type(&t.typ);
        }
    }
}

/// The type parameter names of a declaration.
fn type_param_names(f: &Finder<'_>, tp: Option<NodeId>, out: &mut BTreeSet<String>) {
    let Some(tp) = tp else { return };
    let Node::TypeParams { params } = *f.ast.node(tp) else {
        return;
    };
    for &p in f.ast.list(params) {
        if let Node::TypeParam { name, .. } = *f.ast.node(p)
            && let Some(n) = ident_name(f.ast, name)
        {
            out.insert(n.to_owned());
        }
    }
}

impl Rule for CheckTemplateNames {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, f, doc| check(cx, f, doc));
    }
}

fn check(cx: &mut Ctx<'_, '_>, f: &Finder<'_>, doc: &Doc) {
    let Some(tg) = doc.preferred(cx, f, "template") else {
        return;
    };
    let b = &doc.block;
    let mut used = Used {
        mode: f.settings.mode.as_str(),
        names: BTreeSet::new(),
    };
    let report = |cx: &mut Ctx<'_, '_>, used: &Used<'_>| {
        for (i, t) in b.tags.iter().enumerate() {
            if t.tag != tg {
                continue;
            }
            for n in template_names(&t.name) {
                if !used.names.contains(&n) {
                    let span = doc.line_span(f, b.tag_line(i));
                    cx.report(span, format!("@{tg} {n} not in use"));
                }
            }
        }
    };
    let params_and_returns = |cx: &mut Ctx<'_, '_>, used: &mut Used<'_>, inner: &Block| {
        let param = doc.preferred(cx, f, "param");
        used.add_tags(inner, param.as_deref());
        let returns = doc.preferred(cx, f, "returns");
        used.add_tags(inner, returns.as_deref());
    };
    if b.tags_named("callback").next().is_some() || b.tags_named("function").next().is_some() {
        params_and_returns(cx, &mut used, b);
        report(cx, &used);
        return;
    }
    let typedefs: Vec<&crate::jsdoc::Tag> = b.tags.iter().filter(|t| t.tag == "typedef").collect();
    if typedefs.len() == 1 {
        used.add_type(&typedefs[0].typ);
        let property = doc.preferred(cx, f, "property");
        used.add_tags(b, property.as_deref());
        report(cx, &used);
        return;
    }
    let Some(t) = doc.target else { return };
    let id = t.id();
    let (tp, kind) = match *f.ast.node(id) {
        Node::ClassDecl { type_params, .. } => (type_params, 'c'),
        Node::Interface { type_params, .. } | Node::TypeAlias { type_params, .. } => {
            (type_params, 't')
        }
        Node::FunctionDecl {
            type_params,
            body: Some(_),
            ..
        } => (type_params, 'f'),
        _ => return,
    };
    type_param_names(f, tp, &mut used.names);
    match kind {
        'f' => params_and_returns(cx, &mut used, b),
        'c' => {
            if let Node::ClassDecl { members, .. } = *f.ast.node(id) {
                for &m in f.ast.list(members) {
                    let Some(c) = f.jsdoc_of(m) else { continue };
                    let inner = Block::parse(f.ast.comments[c].span.text(f.src));
                    params_and_returns(cx, &mut used, &inner);
                    let ty = doc.preferred(cx, f, "type");
                    used.add_tags(&inner, ty.as_deref());
                }
            }
        }
        _ => {}
    }
    for h in ["augments", "extends", "implements"] {
        used.add_tags(b, Some(h));
    }
    report(cx, &used);
}
