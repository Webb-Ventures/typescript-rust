//! `jsdoc/informative-docs`: a description (the block's, or a tag's) must say more than the
//! name it documents.
//!
//! The names come from the node the block documents: a declaration's own name; a class
//! member's class name and key; a variable declaration's first declarator, whose name and
//! initializer (a named function or class, or an identifier) both count; an object
//! property's key. A method signature gives none (the plugin looks for an `id` it doesn't
//! have). A tag's own name counts for its description. Text and names are cut into words
//! (letters and digits, camelCase split, lowercased); a description is uninformative when
//! it has words but none left once the names, their `aliases` and the `uselessWords` are
//! taken out.

use super::prelude::*;
use crate::jsdoc::{Finder, Target};
use crate::jsdoc_util::{description, each_block};
use std::collections::BTreeSet;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/informative-docs",
        "This rule reports doc comments that only restate their attached name.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<InformativeDocs>,
};

#[derive(Debug)]
struct InformativeDocs {
    aliases: Vec<(String, Vec<String>)>,
    excluded_tags: Vec<String>,
    useless_words: Vec<String>,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl FromOptions for InformativeDocs {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        let aliases = match o.and_then(|o| o.get("aliases")).and_then(Value::as_object) {
            Some(m) => m
                .iter()
                .map(|(k, v)| {
                    let to = strings(v).iter().map(|s| s.to_lowercase()).collect();
                    (k.to_lowercase(), to)
                })
                .collect(),
            None => vec![("a".to_owned(), vec!["an".to_owned(), "our".to_owned()])],
        };
        let useless_words = match o.and_then(|o| o.get("uselessWords")) {
            Some(v) => strings(v),
            None => ["a", "an", "i", "in", "of", "s", "the"]
                .map(str::to_owned)
                .to_vec(),
        };
        Ok(InformativeDocs {
            aliases,
            excluded_tags: o
                .and_then(|o| o.get("excludedTags"))
                .map(strings)
                .unwrap_or_default(),
            useless_words: useless_words.iter().map(|s| s.to_lowercase()).collect(),
        })
    }
}

/// The words of `text`: runs of letters and digits, split where a lowercase letter meets
/// an uppercase one, lowercased.
fn words(text: &str, out: &mut BTreeSet<String>) {
    for run in text.split(|c: char| !c.is_alphanumeric()) {
        let mut cur = String::new();
        let mut prev_lower = false;
        for c in run.chars() {
            if prev_lower && c.is_uppercase() && !cur.is_empty() {
                out.insert(std::mem::take(&mut cur).to_lowercase());
            }
            prev_lower = c.is_lowercase();
            cur.push(c);
        }
        if !cur.is_empty() {
            out.insert(cur.to_lowercase());
        }
    }
}

const MAX_DEPTH: u32 = 8;

/// The names the documented node carries (see the module docs).
fn node_names(f: &Finder<'_>, id: Option<NodeId>, out: &mut Vec<String>, depth: u32) {
    let Some(id) = id else { return };
    if depth > MAX_DEPTH {
        return;
    }
    let ast = f.ast;
    let name = |n: Option<NodeId>, out: &mut Vec<String>| node_names(f, n, out, depth + 1);
    match *ast.node(id) {
        Node::Ident { name: atom } => out.push(ast.atom(atom).to_owned()),
        Node::MethodDef { key, .. } if f.in_object_literal(id) => name(Some(key), out),
        Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => {
            let class = f.parent(id).filter(|&p| {
                matches!(ast.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. })
            });
            name(class, out);
            name(Some(key), out);
        }
        Node::ComputedKey { expr } => {
            if matches!(ast.node(expr), Node::Ident { .. }) {
                name(Some(expr), out);
            }
        }
        Node::ClassDecl { name: n, .. }
        | Node::ClassExpr { name: n, .. }
        | Node::FunctionDecl { name: n, .. }
        | Node::FunctionExpr { name: n, .. } => name(n, out),
        Node::Interface { name: n, .. }
        | Node::TypeAlias { name: n, .. }
        | Node::Enum { name: n, .. }
        | Node::EnumMember { name: n, .. }
        | Node::Module { name: n, .. } => name(Some(n), out),
        Node::ExportDefaultExpr { expr } => name(Some(expr), out),
        Node::Property { key, .. } => name(Some(key), out),
        Node::ShorthandProp { name: n, .. } => name(Some(n), out),
        Node::VarDecl { decls, .. } => name(ast.list(decls).first().copied(), out),
        Node::VarDeclarator { name: n, init, .. } => {
            name(Some(n), out);
            let mut init = init;
            while let Some(i) = init {
                match *ast.node(i) {
                    Node::Paren { expr } => init = Some(expr),
                    _ => break,
                }
            }
            name(init, out);
        }
        _ => {}
    }
}

impl InformativeDocs {
    /// Whether `text` has words, all of them names, aliases of names or useless words.
    fn redundant(&self, text: &str, names: &[&str]) -> bool {
        let mut docs = BTreeSet::new();
        words(text, &mut docs);
        if docs.is_empty() {
            return false;
        }
        let mut known = BTreeSet::new();
        for n in names {
            words(n, &mut known);
        }
        for (from, to) in &self.aliases {
            if known.contains(from) {
                known.extend(to.iter().cloned());
            }
        }
        known.extend(self.useless_words.iter().cloned());
        docs.is_subset(&known)
    }
}

impl Rule for InformativeDocs {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, f, doc| {
            let mut names = Vec::new();
            node_names(f, doc.target.map(Target::id), &mut names, 0);
            let b = &doc.block;
            for (i, t) in b.tags.iter().enumerate() {
                if self.excluded_tags.contains(&t.tag) {
                    continue;
                }
                let mut with_tag: Vec<&str> = vec![&t.name];
                with_tag.extend(names.iter().map(String::as_str));
                if self.redundant(&t.description, &with_tag) {
                    let (span, loc) = doc.report_at(f, b.tag_line(i), None);
                    cx.report(
                        span,
                        "This tag description only repeats the name it describes.",
                    )
                    .loc(loc);
                }
            }
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            if self.redundant(&description(b).text(), &names) {
                cx.report(
                    doc.comment,
                    "This description only repeats the name it describes.",
                );
            }
        });
    }
}
