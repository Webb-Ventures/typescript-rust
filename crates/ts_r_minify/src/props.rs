//! Property mangling (esbuild's `--mangle-props`), from its docs and observed output.
//!
//! A property name is mangled when the caller's predicate accepts it (the `--mangle-props`
//! pattern, minus `--reserve-props` and the names that are never mangled: `__proto__`,
//! `constructor`, `prototype`). The names in every file of one output unit get one short
//! name each: [`collect`] counts the uses of each name per file, [`assign`] gives the most
//! used names the shortest names (ties in first-use order), never a name some property keeps
//! as written, and [`apply`] rewrites a file.
//!
//! What counts as a property name, on the tree after the TypeScript transform:
//!
//! - `x.name` and `x?.name`; object literal keys, methods, getters and setters; class fields
//!   and methods (not `#private` names); destructuring keys; JSX attribute names and
//!   `<A.B>` members when JSX is preserved. Shorthands are expanded (`{ a: x }`).
//! - Written strings, when `quoted` (`--mangle-quoted`): `x["name"]`, `"name" in x`, quoted
//!   keys and computed keys, through `?:` branches and the last expression of a `,` list.
//! - A string right after a `/* @__KEY__ */` (or `#__KEY__`) comment, always.
//! - Strings the transform writes for a property name: the key of a lowered class field
//!   (`Object.defineProperty(this, "name", ...)`), a decorated member (`__decorate(...,
//!   C.prototype, "name", ...)`) and the excluded keys of a lowered rest pattern
//!   (`__rest(o, ["name"])`). The transform's `exports.name` of a converted module is an
//!   export, not a property; enum member strings are never mangled (as in esbuild, an enum
//!   member read `E.name` is, so mangling breaks enums).
//! - With `skip_namespace_members` (bundling), `ns.name` of an `import * as ns` is an
//!   import of `name`, not a property.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{AssignOp, Ast, BinaryOp, CommentKind, MethodKind, Node, NodeId, Span};

use crate::{Scopes, SymbolId};

/// Names [`assign`] never gives and the predicate should never accept.
pub const NEVER_MANGLED: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// How [`collect`] and [`apply`] find property names.
#[allow(missing_debug_implementations)] // holds a closure
pub struct PropOptions<'a> {
    /// Whether a property name is mangled.
    pub mangles: &'a dyn Fn(&str) -> bool,
    /// `--mangle-quoted`: written strings in property positions are property names too.
    pub quoted: bool,
    /// `ns.x` of an `import * as ns` is not a property (bundling).
    pub skip_namespace_members: bool,
}

/// The property names of one file: mangled names with their use counts, in first-use
/// order (first as written in the source, then generated), and names kept as written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PropNames {
    pub mangled: Vec<(String, u32)>,
    pub kept: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SiteKind {
    /// An identifier (`x.name`, a key, a JSX attribute).
    Name,
    /// A shorthand property `{ name }` / `{ name = init }` (the site is the property).
    Shorthand,
    /// A shorthand binding `{ name }` in a pattern (the site is the binding property).
    BindingShorthand,
    /// A string written in a property position (mangled only with `quoted`).
    Quoted,
    /// A string (or a template without substitutions) after `/* @__KEY__ */`.
    Key,
    /// A string the transform wrote for a property name: mangled when the file mangles the
    /// name elsewhere (it may come from a quoted key, which is not mangled).
    Synth,
}

#[derive(Clone, Copy)]
struct Site {
    /// The node that holds the name (for shorthands, the property node itself).
    parent: Option<NodeId>,
    /// The `Ident` or `StringLit` with the name.
    child: NodeId,
    kind: SiteKind,
}

fn is_key_comment(text: &str) -> bool {
    let t = text
        .strip_prefix("/*")
        .and_then(|t| t.strip_suffix("*/"))
        .unwrap_or("")
        .trim();
    t == "@__KEY__" || t == "#__KEY__"
}

/// Every property-name site of the tree, in arena order (source order for parsed nodes).
fn sites(ast: &Ast, src: &str, scopes: Option<&Scopes>, opts: &PropOptions<'_>) -> Vec<Site> {
    let mut out = Vec::new();
    // Ends of `/* @__KEY__ */` comments: a string starting after one (only whitespace in
    // between) is a key.
    let key_ends: Vec<u32> = ast
        .comments
        .iter()
        .filter(|c| {
            c.kind == CommentKind::Block
                && src
                    .get(c.span.start as usize..c.span.end as usize)
                    .is_some_and(is_key_comment)
        })
        .map(|c| c.span.end)
        .collect();
    let after_key_comment = |id: NodeId| -> bool {
        if key_ends.is_empty() {
            return false;
        }
        let s = ast.span(id);
        if s.start >= s.end {
            return false;
        }
        let i = key_ends.partition_point(|&e| e <= s.start);
        i > 0
            && src
                .get(key_ends[i - 1] as usize..s.start as usize)
                .is_some_and(|gap| gap.chars().all(char::is_whitespace))
    };
    let written = |id: NodeId| {
        let s = ast.span(id);
        s.start < s.end
    };
    let ns_syms: FxHashSet<SymbolId> = match scopes {
        Some(sc) if opts.skip_namespace_members => ast
            .nodes
            .iter()
            .filter_map(|n| match *n {
                Node::ImportNamespaceSpecifier { local } => sc.symbol_of(local),
                _ => None,
            })
            .collect(),
        _ => FxHashSet::default(),
    };
    // A string in a quoted position, through `?:` branches, `,` lists and parentheses.
    fn quoted(ast: &Ast, parent: NodeId, e: NodeId, out: &mut Vec<Site>, depth: u32) {
        if depth > 64 {
            return;
        }
        match *ast.node(e) {
            Node::StringLit { .. } if ast.span(e).start < ast.span(e).end => out.push(Site {
                parent: Some(parent),
                child: e,
                kind: SiteKind::Quoted,
            }),
            Node::Conditional { cons, alt, .. } => {
                quoted(ast, e, cons, out, depth + 1);
                quoted(ast, e, alt, out, depth + 1);
            }
            Node::Seq { exprs } => {
                if let Some(&last) = ast.list(exprs).last() {
                    quoted(ast, e, last, out, depth + 1);
                }
            }
            Node::Paren { expr } => quoted(ast, e, expr, out, depth + 1),
            _ => {}
        }
    }
    let key = |parent: NodeId, k: NodeId, out: &mut Vec<Site>| match *ast.node(k) {
        Node::Ident { .. } => out.push(Site {
            parent: Some(parent),
            child: k,
            kind: SiteKind::Name,
        }),
        Node::StringLit { .. } if written(k) => out.push(Site {
            parent: Some(parent),
            child: k,
            kind: SiteKind::Quoted,
        }),
        Node::ComputedKey { expr } => quoted(ast, k, expr, out, 0),
        _ => {}
    };
    let is_ident = |id: NodeId, name: &str| matches!(*ast.node(id), Node::Ident { name: n } if ast.atom(n) == name);
    let is_object_define = |callee: NodeId| match *ast.node(callee) {
        Node::Member { obj, prop, .. } => {
            is_ident(obj, "Object") && is_ident(prop, "defineProperty")
        }
        _ => false,
    };
    let synth_string = |id: NodeId| matches!(ast.node(id), Node::StringLit { .. }) && !written(id);
    // The tree as printed, in source order (the transform leaves replaced nodes in the
    // arena; a node it shares between two parents is visited for each).
    let mut stack: Vec<(NodeId, Option<NodeId>)> = vec![(ast.root, None)];
    let mut children: Vec<NodeId> = Vec::new();
    let mut steps = 0usize;
    // A shared subtree is visited once per parent; bound the walk on a pathological tree.
    let limit = ast.nodes.len().saturating_mul(8).max(1 << 16);
    while let Some((id, from)) = stack.pop() {
        steps += 1;
        if steps > limit {
            break;
        }
        children.clear();
        ast.for_each_child(id, |c| children.push(c));
        stack.extend(children.iter().rev().map(|&c| (c, Some(id))));
        match *ast.node(id) {
            Node::Member { obj, prop, .. } => {
                if !matches!(ast.node(prop), Node::Ident { .. }) {
                    continue;
                }
                // The transform's `exports.x = ...` of a converted module.
                if !written(id) && is_ident(obj, "exports") {
                    continue;
                }
                if !ns_syms.is_empty()
                    && matches!(ast.node(obj), Node::Ident { .. })
                    && scopes
                        .and_then(|s| s.symbol_of(obj))
                        .is_some_and(|s| ns_syms.contains(&s))
                {
                    continue;
                }
                out.push(Site {
                    parent: Some(id),
                    child: prop,
                    kind: SiteKind::Name,
                });
            }
            Node::JsxMemberExpr { prop, .. } => {
                if matches!(ast.node(prop), Node::Ident { .. }) {
                    out.push(Site {
                        parent: Some(id),
                        child: prop,
                        kind: SiteKind::Name,
                    });
                }
            }
            Node::JsxAttr { name, .. } => {
                if matches!(ast.node(name), Node::Ident { .. }) {
                    out.push(Site {
                        parent: Some(id),
                        child: name,
                        kind: SiteKind::Name,
                    });
                }
            }
            Node::Property { key: k, .. } => key(id, k, &mut out),
            // The field a TypeScript parameter property declares (`constructor(public x_)`
            // under define semantics) keeps its name, as in esbuild; `this.x_` is mangled.
            Node::PropertyDef {
                key: k,
                value: None,
                ..
            } if !written(id) && matches!(ast.node(k), Node::Ident { .. }) => {}
            Node::PropertyDef { key: k, .. } => key(id, k, &mut out),
            Node::MethodDef { kind, key: k, .. } => {
                if kind != MethodKind::Constructor {
                    key(id, k, &mut out);
                }
            }
            Node::ShorthandProp { name, .. } => out.push(Site {
                parent: Some(id),
                child: name,
                kind: SiteKind::Shorthand,
            }),
            Node::BindingProperty { key: k, value, .. } => {
                if value.is_none() && matches!(ast.node(k), Node::Ident { .. }) {
                    out.push(Site {
                        parent: Some(id),
                        child: k,
                        kind: SiteKind::BindingShorthand,
                    });
                } else {
                    key(id, k, &mut out);
                }
            }
            Node::Index { index, .. } => quoted(ast, id, index, &mut out, 0),
            Node::Binary {
                left,
                op: BinaryOp::In,
                ..
            } => quoted(ast, id, left, &mut out, 0),
            Node::StringLit { .. } | Node::TemplateLit { .. } => {
                // A `/* @__KEY__ */` string is a key wherever it is.
                if let Some(p) = from
                    && after_key_comment(id)
                    && key_text(ast, id).is_some()
                {
                    out.push(Site {
                        parent: Some(p),
                        child: id,
                        kind: SiteKind::Key,
                    });
                }
            }
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                let synth = |slot: usize| args.get(slot).copied().filter(|&a| synth_string(a));
                if is_object_define(callee) {
                    if let (Some(&target), Some(s)) = (args.first(), synth(1))
                        && !is_ident(target, "exports")
                    {
                        out.push(Site {
                            parent: Some(id),
                            child: s,
                            kind: SiteKind::Synth,
                        });
                    }
                } else if is_ident(callee, "__decorate") {
                    if let Some(s) = synth(2) {
                        out.push(Site {
                            parent: Some(id),
                            child: s,
                            kind: SiteKind::Synth,
                        });
                    }
                } else if is_ident(callee, "__rest")
                    && let Some(&list) = args.get(1)
                    && let Node::ArrayLit { elements } = *ast.node(list)
                {
                    for &e in ast.list(elements) {
                        if synth_string(e) {
                            out.push(Site {
                                parent: Some(list),
                                child: e,
                                kind: SiteKind::Synth,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // A `/* @__KEY__ */` string in a quoted position is one key, not also a quoted name.
    let keyed: FxHashSet<(Option<NodeId>, NodeId)> = out
        .iter()
        .filter(|s| s.kind == SiteKind::Key)
        .map(|s| (s.parent, s.child))
        .collect();
    if !keyed.is_empty() {
        out.retain(|s| s.kind == SiteKind::Key || !keyed.contains(&(s.parent, s.child)));
    }
    out
}

/// The text of a string, or of a template literal without substitutions.
fn key_text(ast: &Ast, id: NodeId) -> Option<ts_r_ast::Atom> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(value),
        Node::TemplateLit { parts } => match *ast.list(parts) {
            [only] => match *ast.node(only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(cooked),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn site_name(ast: &Ast, s: &Site) -> Option<ts_r_ast::Atom> {
    match *ast.node(s.child) {
        Node::Ident { name } => Some(name),
        _ => key_text(ast, s.child),
    }
}

/// The sites with their names, and whether each is mangled.
fn mangled_sites(
    ast: &Ast,
    src: &str,
    scopes: Option<&Scopes>,
    opts: &PropOptions<'_>,
) -> Vec<(Site, ts_r_ast::Atom, bool)> {
    let mut out: Vec<(Site, ts_r_ast::Atom, bool)> = sites(ast, src, scopes, opts)
        .into_iter()
        .filter_map(|s| {
            let atom = site_name(ast, &s)?;
            let name = ast.atom(atom);
            let ok = (s.kind != SiteKind::Quoted || opts.quoted)
                && !NEVER_MANGLED.contains(&name)
                && (opts.mangles)(name);
            Some((s, atom, ok))
        })
        .collect();
    if out.iter().any(|(s, _, _)| s.kind == SiteKind::Synth) {
        let elsewhere: FxHashSet<ts_r_ast::Atom> = out
            .iter()
            .filter(|(s, _, ok)| *ok && s.kind != SiteKind::Synth)
            .map(|(_, a, _)| *a)
            .collect();
        for (s, a, ok) in &mut out {
            if s.kind == SiteKind::Synth {
                *ok = *ok && elsewhere.contains(a);
            }
        }
    }
    out
}

/// The property names of a file (see the module docs). `scopes` is needed for
/// `skip_namespace_members`.
pub fn collect(ast: &Ast, src: &str, scopes: Option<&Scopes>, opts: &PropOptions<'_>) -> PropNames {
    let mut counts: FxHashMap<&str, usize> = FxHashMap::default();
    let mut out = PropNames::default();
    let mut kept: FxHashSet<&str> = FxHashSet::default();
    let found = mangled_sites(ast, src, scopes, opts);
    // First-use order counts names as written first: what the transform generates (a JSX
    // factory call, a lowered field) comes after.
    let written = |s: &Site| {
        let sp = ast.span(s.child);
        sp.start < sp.end
    };
    for (_, atom, _) in found.iter().filter(|(s, _, ok)| *ok && written(s)) {
        let name = ast.atom(*atom);
        if !counts.contains_key(name) {
            counts.insert(name, out.mangled.len());
            out.mangled.push((name.to_owned(), 0));
        }
    }
    for (_, atom, ok) in found {
        let name = ast.atom(atom);
        if ok {
            match counts.get(name) {
                Some(&i) => out.mangled[i].1 += 1,
                None => {
                    counts.insert(name, out.mangled.len());
                    out.mangled.push((name.to_owned(), 1));
                }
            }
        } else if kept.insert(name) {
            out.kept.push(name.to_owned());
        }
    }
    out
}

/// The `n`th property name: `a`, ..., `z`, `A`, ..., `Z`, `_`, `$`, `aa`, `ba`, ...
fn nth_prop_name(mut n: usize) -> String {
    const FIRST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_$";
    const REST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_$0123456789";
    let mut s = String::with_capacity(3);
    s.push(FIRST[n % FIRST.len()] as char);
    n /= FIRST.len();
    while n > 0 {
        n -= 1;
        s.push(REST[n % REST.len()] as char);
        n /= REST.len();
    }
    s
}

/// One short name per mangled name over `files` (one output unit, in module order): most
/// used first, ties in first-use order, skipping every name a file keeps as written.
/// Sorted by original name.
pub fn assign<'a>(files: impl IntoIterator<Item = &'a PropNames>) -> Vec<(String, String)> {
    let mut index: FxHashMap<&str, usize> = FxHashMap::default();
    let mut all: Vec<(&str, u64)> = Vec::new();
    let mut kept: FxHashSet<&str> = FxHashSet::default();
    for f in files {
        for (name, n) in &f.mangled {
            match index.get(name.as_str()) {
                Some(&i) => all[i].1 += u64::from(*n),
                None => {
                    index.insert(name, all.len());
                    all.push((name, u64::from(*n)));
                }
            }
        }
        kept.extend(f.kept.iter().map(String::as_str));
    }
    // Stable: ties keep first-use order.
    all.sort_by(|a, b| b.1.cmp(&a.1));
    let mut next = 0usize;
    let mut out: Vec<(String, String)> = Vec::with_capacity(all.len());
    for (name, _) in all {
        let short = loop {
            let c = nth_prop_name(next);
            next += 1;
            if !kept.contains(c.as_str()) && !NEVER_MANGLED.contains(&c.as_str()) {
                break c;
            }
        };
        out.push((name.to_owned(), short));
    }
    out.sort();
    out
}

/// Rewrites the property names of a file that `names` maps (sorted by original name, as
/// [`assign`] returns them). Shorthands become `key: value`; written strings print as their
/// new value.
pub fn apply(
    ast: &mut Ast,
    src: &str,
    scopes: Option<&Scopes>,
    opts: &PropOptions<'_>,
    names: &[(String, String)],
) {
    if names.is_empty() {
        return;
    }
    let lookup = |n: &str| {
        names
            .binary_search_by(|(k, _)| k.as_str().cmp(n))
            .ok()
            .map(|i| names[i].1.as_str())
    };
    let found = mangled_sites(ast, src, scopes, opts);
    // `(parent, old child, new child)`, applied once per parent at the end: remapping a
    // parent per child would copy its child list (an array of 30,000 keys) every time.
    let mut pending: Vec<(NodeId, NodeId, NodeId)> = Vec::new();
    for (s, atom, ok) in found {
        if !ok {
            continue;
        }
        let name = ast.atom(atom).to_owned();
        let Some(short) = lookup(&name) else { continue };
        let span = ast.span(s.child);
        let short_atom = ast.intern(short);
        match s.kind {
            SiteKind::Name => {
                let new = ast.add(Node::Ident { name: short_atom }, span);
                if let Some(p) = s.parent {
                    pending.push((p, s.child, new));
                }
            }
            SiteKind::Shorthand => {
                let Some(p) = s.parent else { continue };
                let Node::ShorthandProp {
                    modifiers,
                    name: value,
                    init,
                } = *ast.node(p)
                else {
                    continue;
                };
                let key = ast.add(Node::Ident { name: short_atom }, span);
                let value = match init {
                    Some(right) => {
                        let at = ast.span(p);
                        ast.add(
                            Node::Assign {
                                left: value,
                                op: AssignOp::Assign,
                                right,
                            },
                            at,
                        )
                    }
                    None => value,
                };
                *ast.node_mut(p) = Node::Property {
                    modifiers,
                    key,
                    value,
                };
            }
            SiteKind::BindingShorthand => {
                let Some(p) = s.parent else { continue };
                let key = ast.add(Node::Ident { name: short_atom }, span);
                if let Node::BindingProperty { key: k, value, .. } = ast.node_mut(p) {
                    *value = Some(*k);
                    *k = key;
                }
            }
            SiteKind::Quoted | SiteKind::Key | SiteKind::Synth => {
                // An empty span prints the value, not the source text.
                let new = ast.add(
                    Node::StringLit { value: short_atom },
                    Span::new(span.start, span.start),
                );
                if let Some(p) = s.parent {
                    pending.push((p, s.child, new));
                }
            }
        }
    }
    replace_children(ast, &mut pending);
}

/// Points each parent's child `old` at `new`, remapping every parent once (children are
/// never mutated in place: the transform may share a node between two parents).
fn replace_children(ast: &mut Ast, pending: &mut [(NodeId, NodeId, NodeId)]) {
    pending.sort_by_key(|&(p, old, _)| (p.raw(), old.raw()));
    for group in pending.chunk_by(|a, b| a.0 == b.0) {
        let parent = group[0].0;
        let node = *ast.node(parent);
        let mapped = node.map_children(ast, &mut |_, c| match group
            .binary_search_by_key(&c.raw(), |&(_, old, _)| old.raw())
        {
            Ok(i) => group[i].2,
            Err(_) => c,
        });
        *ast.node_mut(parent) = mapped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn run(src: &str, quoted: bool, pattern: &dyn Fn(&str) -> bool) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let opts = PropOptions {
            mangles: pattern,
            quoted,
            skip_namespace_members: false,
        };
        let names = collect(&ast, src, None, &opts);
        let map = assign([&names]);
        apply(&mut ast, src, None, &opts, &map);
        let cg = ts_r_codegen::CodegenOptions {
            comments: true,
            ..ts_r_codegen::CodegenOptions::default()
        };
        ts_r_codegen::print(&ast, src, &cg).code.trim().to_owned()
    }

    fn underscore(n: &str) -> bool {
        n.ends_with('_')
    }

    /// Thousands of rewritten children of one parent remap that parent once: the list
    /// storage grows by one copy of the array, not one copy per element.
    #[test]
    fn many_keys_in_one_array_stay_linear() {
        let n = 32_768;
        let src = format!("x = [{}];", "/* @__KEY__ */ 'x_',".repeat(n));
        let mut ast = ts_r_parser::parse(&src, SourceKind::Js).ast;
        let opts = PropOptions {
            mangles: &underscore,
            quoted: false,
            skip_namespace_members: false,
        };
        let names = collect(&ast, &src, None, &opts);
        let map = assign([&names]);
        let before = ast.list_items.len();
        apply(&mut ast, &src, None, &opts, &map);
        let grown = ast.list_items.len() - before;
        assert!(grown <= n + 16, "list storage grew by {grown}");
        let out = ts_r_codegen::print(&ast, &src, &ts_r_codegen::CodegenOptions::default()).code;
        assert_eq!(out.matches("\"a\"").count(), n, "{}", &out[..80]);
    }

    #[test]
    fn members_keys_and_shorthands() {
        assert_eq!(
            run("x.foo_ = x.foo_ + x.bar_;", false, &underscore),
            "x.a = x.a + x.b;"
        );
        assert_eq!(
            run(
                "let { bar_, baz_: q } = o; ({ bar_ } = o); f({ bar_ });",
                false,
                &underscore
            ),
            "let { a: bar_, b: q } = o;\n({ a: bar_ } = o);\nf({ a: bar_ });"
        );
        assert_eq!(
            run(
                "class A { a_ = 1; static b_() {} #c_ = 1; get d_() {} }",
                false,
                &underscore
            ),
            "class A {\n    a = 1;\n    static b() {}\n    #c_ = 1;\n    get c() {}\n}"
        );
    }

    #[test]
    fn collisions_and_reserved_names() {
        // `a` and `b` are kept property names; `__proto__` is never mangled.
        assert_eq!(
            run(
                "({ foo_: 0, bar_: 1, a: 2, b: 3, __proto__: {} });",
                false,
                &|_| true
            ),
            "({ a: 0, b: 1, c: 2, d: 3, __proto__: {} });"
        );
        assert_eq!(
            run("({ foo_: 0, bar_: 1, a: 2, b: 3 });", false, &underscore),
            "({ c: 0, d: 1, a: 2, b: 3 });"
        );
    }

    #[test]
    fn quoted_names_and_key_comments() {
        let src = "x['_m']; x[y ? '_m' : z]; ({ '_m': 1 }); '_m' in x; x._m;";
        assert_eq!(
            run(src, false, &|n: &str| n.starts_with('_')),
            "x['_m'];\nx[y ? '_m' : z];\n({ '_m': 1 });\n'_m' in x;\nx.a;"
        );
        assert_eq!(
            run(src, true, &|n: &str| n.starts_with('_')),
            "x[\"a\"];\nx[y ? \"a\" : z];\n({ \"a\": 1 });\n\"a\" in x;\nx.a;"
        );
        assert_eq!(
            run(
                "x(/* @__KEY__ */ '_k', /* __KEY__ */ '_n'); x._k;",
                false,
                &|n: &str| n.starts_with('_')
            ),
            "x(\"a\", '_n');\nx.a;"
        );
        assert_eq!(
            run(
                "x(/* @__KEY__ */ `_k`, `_n`); x[/* #__KEY__ */ '_k'];",
                false,
                &|n: &str| n.starts_with('_')
            ),
            "x(\"a\", `_n`);\nx[\"a\"];"
        );
    }

    #[test]
    fn names_by_use_count() {
        assert_eq!(
            run("x.foo_; x.bar_; x.zed_; x.zed_;", false, &underscore),
            "x.b;\nx.c;\nx.a;\nx.a;"
        );
        assert_eq!(nth_prop_name(0), "a");
        assert_eq!(nth_prop_name(54), "aa");
    }
}
