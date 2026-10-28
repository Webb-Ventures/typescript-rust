//! One output stylesheet from the CSS files a chunk uses, in esbuild's order:
//!
//! - Each root (a CSS entry, or the CSS files a JavaScript entry imports, first import
//!   first) expands depth-first: a file's `@import`s come before its own rules, and every
//!   `@import` adds its conditions to the chain of the files it pulls in. An import of a
//!   file already on the current path (a cycle) is skipped.
//! - Duplicates: the CSS cascade lets the last copy of a file win, so an earlier copy is
//!   dropped when a later copy applies wherever it does (same layers, and the later one's
//!   `@supports` / `@media` conditions are equal or absent). A dropped copy that declares
//!   layers leaves a `@layer` statement so layer order is unchanged.
//! - External `@import`s and the `@layer` statements before a file's imports are hoisted
//!   to the top, in order. An external import under more than one level of conditions is
//!   nested in `data:text/css` imports, one per level, as esbuild prints it.
//! - Conditions wrap a file's rules innermost first: `@layer`, then `@supports`, then
//!   `@media`; an empty `@layer x {}` is printed as `@layer x;`, other empty wrappers vanish.
//!
//! Observed in esbuild 0.28.2 (`bundler_css_test.go`: `CSSAtImportConditions*`,
//! `CSSAtLayerBeforeImport*`).

use super::print::Printer;
use super::{Node, Sheet};
use crate::graph::{ModuleId, Target};

/// What ordering and printing need from the bundle.
pub trait Host {
    fn sheet(&self, m: ModuleId) -> Option<&Sheet>;
    /// Where record `r` of `m` points.
    fn target(&self, m: ModuleId, r: u32) -> Target;
    /// The text of `url()` record `r` of `m`.
    fn url(&self, m: ModuleId, r: u32) -> String;
    /// The final name of local name `n` of `m`.
    fn local(&self, m: ModuleId, n: u32) -> String;
    fn pretty(&self, m: ModuleId) -> String;
}

/// One level of `@import` conditions, as printed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Cond {
    /// `Some("")` is an anonymous layer.
    layer: Option<String>,
    supports: Option<String>,
    media: String,
}

impl Cond {
    /// What keeping this level in an item costs from the expansion budget.
    fn cost(&self) -> usize {
        1 + self.layer.as_ref().map_or(0, String::len)
            + self.supports.as_ref().map_or(0, String::len)
            + self.media.len()
    }

    fn text(&self) -> String {
        let mut parts = Vec::new();
        match &self.layer {
            Some(l) if l.is_empty() => parts.push("layer".to_owned()),
            Some(l) => parts.push(format!("layer({l})")),
            None => {}
        }
        if let Some(s) = &self.supports {
            parts.push(format!("supports({s})"));
        }
        if !self.media.is_empty() {
            parts.push(self.media.clone());
        }
        parts.join(" ")
    }
}

enum Item {
    File(ModuleId, Vec<Cond>),
    External(String, Vec<Cond>),
    /// `@layer a, b;` under conditions: a file's statements before its `@import`s (`true`:
    /// above the external imports, only before the first file) or what a dropped duplicate
    /// declared (`false`: kept in place).
    Layers(Vec<String>, Vec<Cond>, bool),
}

/// Nesting limit for `@import` chains.
const MAX_DEPTH: usize = 64;

/// How much one chunk's `@import` expansion may produce: one unit per file, external
/// import or `@layer` statement, plus the bytes of the conditions each carries. A file
/// imported twice by each file of a chain is expanded 2^n times (no cycle, no deep
/// nesting), so past this the stylesheet is an error instead of gigabytes of copies.
pub const MAX_EXPANSION: usize = 1 << 18;

/// Why [`bundle`] gave up: the expansion budget ran out under this root.
#[derive(Debug, PartialEq, Eq)]
pub struct TooManyImports {
    pub root: ModuleId,
}

struct Walk<'a> {
    host: &'a dyn Host,
    minify: bool,
    syntax: bool,
    items: Vec<Item>,
    stack: Vec<ModuleId>,
    /// Budget left ([`MAX_EXPANSION`]); `None` once it ran out.
    left: Option<usize>,
}

impl Walk<'_> {
    /// Adds `item`, paying for it; `false` once the budget is used up.
    fn push(&mut self, item: Item) -> bool {
        let chain = match &item {
            Item::File(_, c) | Item::External(_, c) | Item::Layers(_, c, _) => c,
        };
        let cost = 1 + chain.iter().map(Cond::cost).sum::<usize>();
        match self.left.and_then(|l| l.checked_sub(cost)) {
            Some(l) => {
                self.left = Some(l);
                self.items.push(item);
                true
            }
            None => {
                self.left = None;
                false
            }
        }
    }

    fn visit(&mut self, m: ModuleId, chain: Vec<Cond>) {
        if self.left.is_none() || self.stack.contains(&m) || self.stack.len() >= MAX_DEPTH {
            return;
        }
        let host = self.host;
        let Some(sheet) = host.sheet(m) else { return };
        self.stack.push(m);
        let url = |r: u32| host.url(m, r);
        let local = |n: u32| host.local(m, n);
        let p = Printer {
            sheet,
            minify: self.minify,
            syntax: self.syntax,
            url: &url,
            local: &local,
        };
        // `@layer` statements before the first `@import` are hoisted with the imports.
        let mut leading = Vec::new();
        for n in &sheet.rules {
            match n {
                Node::At {
                    name,
                    prelude,
                    body: None,
                    ..
                } if name == "layer" => leading.extend(super::layer_names(sheet, prelude)),
                Node::Import(_) => break,
                _ => {
                    leading.clear();
                    break;
                }
            }
        }
        if !leading.is_empty() && sheet.rules.iter().any(|n| matches!(n, Node::Import(_))) {
            self.push(Item::Layers(leading, chain.clone(), true));
        }
        // `@import`s and the files `composes ... from` names (as unconditional imports,
        // observed in esbuild), in source order.
        let mut imports = sheet.imports.iter().peekable();
        for (r, rec) in sheet.records.iter().enumerate() {
            if self.left.is_none() {
                break;
            }
            let r = r as u32;
            let mut next = chain.clone();
            match rec.kind {
                super::RecKind::Import => {
                    let Some(imp) = imports.next_if(|i| i.record == r) else {
                        continue;
                    };
                    let c = &imp.conditions;
                    let cond = Cond {
                        layer: c.layer.as_ref().map(|l| p.values_text(l)),
                        supports: c.supports.as_ref().map(|s| p.values_text(s)),
                        media: p.values_text(&c.media),
                    };
                    if !(cond.layer.is_none() && cond.supports.is_none() && cond.media.is_empty()) {
                        next.push(cond);
                    }
                }
                super::RecKind::Composes => {
                    if !matches!(host.target(m, r), Target::Module(_)) {
                        continue;
                    }
                }
                super::RecKind::Url => continue,
            }
            match host.target(m, r) {
                Target::Module(t) => {
                    if host.sheet(t).is_some() {
                        self.visit(t, next);
                    }
                }
                Target::External(path) => {
                    let path = crate::graph::split_external(&path).0.to_owned();
                    self.push(Item::External(path, next));
                }
                Target::Missing => {}
            }
        }
        self.stack.pop();
        self.push(Item::File(m, chain));
    }
}

/// Condition chains seen per key, for asking whether one of them makes a new chain
/// [`redundant`]. An exact repeat (the usual duplicate) is a hash lookup; only distinct
/// chains of the same key are compared one by one.
struct Seen<K> {
    exact: rustc_hash::FxHashSet<(K, Vec<Cond>)>,
    by_key: rustc_hash::FxHashMap<K, Vec<Vec<Cond>>>,
}

impl<K: std::hash::Hash + Eq + Clone> Seen<K> {
    fn new() -> Self {
        Seen {
            exact: Default::default(),
            by_key: Default::default(),
        }
    }

    /// Whether a seen chain of `key` makes `chain` redundant (`redundant(chain, seen)`).
    fn covers(&self, key: &K, chain: &[Cond]) -> bool {
        if self.exact.contains(&(key.clone(), chain.to_vec())) {
            return true;
        }
        self.by_key
            .get(key)
            .is_some_and(|cs| cs.iter().any(|c| redundant(chain, c)))
    }

    fn insert(&mut self, key: K, chain: Vec<Cond>) {
        if self.exact.insert((key.clone(), chain.clone())) {
            self.by_key.entry(key).or_default().push(chain);
        }
    }
}

/// Whether a later import with conditions `later` makes an earlier one with `earlier`
/// redundant: every level of `later` has the same layer as `earlier`'s and either the
/// same conditions or fewer of them.
fn redundant(earlier: &[Cond], later: &[Cond]) -> bool {
    if later.len() > earlier.len() {
        return false;
    }
    later.iter().zip(earlier).all(|(b, a)| {
        if a.layer != b.layer {
            return false;
        }
        let same_supports = a.supports == b.supports;
        let same_media = a.media == b.media;
        (same_media && (same_supports || b.supports.is_none()))
            || (same_supports && b.media.is_empty())
    })
}

/// The bundled stylesheet for `roots`. `None` when no root is a stylesheet. `syntax`
/// (`--minify-syntax`) drops rules an identical later rule under the same conditions repeats,
/// across files too (see `print`).
/// Past [`MAX_EXPANSION`] it gives up with the root whose expansion ran out.
pub fn bundle(
    host: &dyn Host,
    roots: &[ModuleId],
    minify: bool,
    syntax: bool,
) -> Result<Option<String>, TooManyImports> {
    let mut w = Walk {
        host,
        minify,
        syntax,
        items: Vec::new(),
        stack: Vec::new(),
        left: Some(MAX_EXPANSION),
    };
    for &r in roots {
        w.visit(r, Vec::new());
        if w.left.is_none() {
            return Err(TooManyImports { root: r });
        }
    }
    if w.items.is_empty() {
        return Ok(None);
    }
    // Only the leading `@layer` statements before the first file go above the external
    // imports; later ones stay in place (observed in esbuild).
    let first_file = w.items.iter().position(|i| matches!(i, Item::File(..)));
    for (i, item) in w.items.iter_mut().enumerate() {
        if let Item::Layers(_, _, hoisted) = item
            && first_file.is_some_and(|f| i > f)
        {
            *hoisted = false;
        }
    }
    // Drop earlier duplicates (walking backwards, comparing with the copies kept).
    let mut kept: Vec<Option<Item>> = Vec::with_capacity(w.items.len());
    let mut later_files: Seen<ModuleId> = Seen::new();
    let mut later_ext: Seen<String> = Seen::new();
    for item in w.items.into_iter().rev() {
        match item {
            Item::File(m, chain) => {
                if later_files.covers(&m, &chain) {
                    // Keep the layer order the dropped copy established.
                    if chain.iter().any(|c| c.layer.as_deref() == Some("")) {
                        kept.push(None);
                        continue;
                    }
                    let mut names = Vec::new();
                    if let Some(sheet) = host.sheet(m) {
                        super::declared_layers(sheet, &sheet.rules, "", &mut names);
                    }
                    if names.is_empty() && !chain.iter().any(|c| c.layer.is_some()) {
                        kept.push(None);
                    } else {
                        kept.push(Some(Item::Layers(names, chain, false)));
                    }
                    continue;
                }
                later_files.insert(m, chain.clone());
                kept.push(Some(Item::File(m, chain)));
            }
            Item::External(p, chain) => {
                if later_ext.covers(&p, &chain) {
                    kept.push(None);
                    continue;
                }
                later_ext.insert(p.clone(), chain.clone());
                kept.push(Some(Item::External(p, chain)));
            }
            other => kept.push(Some(other)),
        }
    }
    kept.reverse();
    let items = tidy_layers(host, kept.into_iter().flatten().collect());
    // Each file's top-level rules, printed (`None`: dropped as repeated later).
    let mut printed: Vec<Vec<Option<String>>> = items
        .iter()
        .map(|item| {
            let Item::File(m, chain) = item else {
                return Vec::new();
            };
            let Some(sheet) = host.sheet(*m) else {
                return Vec::new();
            };
            let url = |r: u32| host.url(*m, r);
            let local = |n: u32| host.local(*m, n);
            let p = Printer {
                sheet,
                minify,
                syntax,
                url: &url,
                local: &local,
            };
            let depth = wrap_depth(chain);
            // Rules after the hoisted leading `@layer` statements.
            let skip = leading_layer_count(sheet);
            sheet
                .rules
                .iter()
                .skip(skip)
                .map(|n| {
                    (!matches!(n, Node::Import(_))).then(|| {
                        let mut s = String::new();
                        p.node(n, depth, &mut s);
                        s
                    })
                })
                .collect()
        })
        .collect();
    if syntax {
        let mut seen = rustc_hash::FxHashSet::default();
        for (item, texts) in items.iter().zip(printed.iter_mut()).rev() {
            let Item::File(m, chain) = item else { continue };
            let Some(sheet) = host.sheet(*m) else {
                continue;
            };
            let skip = leading_layer_count(sheet);
            let conds: Vec<String> = chain.iter().map(Cond::text).collect();
            for (n, text) in sheet.rules.iter().skip(skip).zip(texts.iter_mut()).rev() {
                if let Some(t) = text
                    && super::print::droppable(n)
                    && !seen.insert((conds.clone(), t.trim().to_owned()))
                {
                    *text = None;
                }
            }
        }
    }
    let mut head = String::new();
    let mut body = String::new();
    let nl = if minify { "" } else { "\n" };
    for (item, texts) in items.iter().zip(&printed) {
        match item {
            Item::External(p, chain) => {
                head.push_str(&external_import(p, chain, minify));
                head.push_str(nl);
            }
            Item::Layers(names, chain, hoisted) if !names.is_empty() || !chain.is_empty() => {
                let depth = wrap_depth(chain);
                let inner = if names.is_empty() {
                    String::new()
                } else {
                    format!(
                        "{}@layer {};{nl}",
                        if minify {
                            String::new()
                        } else {
                            "  ".repeat(depth)
                        },
                        names.join(if minify { "," } else { ", " })
                    )
                };
                let text = wrap(inner, chain, minify, 0);
                if *hoisted {
                    head.push_str(&text)
                } else {
                    body.push_str(&text)
                }
            }
            Item::Layers(..) => {}
            Item::File(m, chain) => {
                let sheet = host.sheet(*m);
                if !minify {
                    if !body.is_empty() || !head.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(&format!("/* {} */\n", host.pretty(*m)));
                }
                if sheet.is_none() {
                    continue;
                }
                let inner: String = texts.iter().flatten().map(String::as_str).collect();
                body.push_str(&wrap(inner, chain, minify, 0));
            }
        }
    }
    let mut out = String::new();
    if items
        .iter()
        .any(|i| matches!(i, Item::File(m, _) if host.sheet(*m).is_some_and(|s| s.charset)))
    {
        out.push_str(super::CHARSET);
        out.push_str(nl);
    }
    out.push_str(&head);
    if !out.is_empty() && !body.is_empty() && !minify && !body.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(&body);
    if minify && !out.is_empty() {
        out.push('\n');
    }
    Ok(Some(out))
}

/// The layers an item declares, with its conditions, for comparing the statements left by
/// dropped duplicates and leading `@layer` statements.
type Signature = (Vec<String>, Vec<Cond>);

fn signature(host: &dyn Host, item: &Item) -> Option<Signature> {
    match item {
        Item::File(m, chain) => {
            let mut names = Vec::new();
            if let Some(sheet) = host.sheet(*m) {
                super::declared_layers(sheet, &sheet.rules, "", &mut names);
            }
            Some((names, chain.clone()))
        }
        Item::Layers(names, chain, _) => Some((names.clone(), chain.clone())),
        Item::External(..) => None,
    }
}

/// Removes the `@layer` statements that change nothing (observed in esbuild 0.28.2): one
/// declaring what an earlier item declared under the same or fewer conditions (see
/// [`redundant`]), and one followed directly by an item declaring the same under the same
/// conditions. Consecutive statements under the same conditions then merge into one.
/// Hoisted statements stay as they are.
fn tidy_layers(host: &dyn Host, items: Vec<Item>) -> Vec<Item> {
    let sigs: Vec<_> = items.iter().map(|i| signature(host, i)).collect();
    let is_stmt = |i: &Item| matches!(i, Item::Layers(_, _, false));
    let mut keep = vec![true; items.len()];
    // The kept items so far, by the layers they declare.
    let mut earlier: Seen<Vec<String>> = Seen::new();
    for (i, sig) in sigs.iter().enumerate() {
        let Some((names, chain)) = sig else { continue };
        if is_stmt(&items[i]) && earlier.covers(names, chain) {
            keep[i] = false;
            continue;
        }
        earlier.insert(names.clone(), chain.clone());
    }
    let mut next: Option<usize> = None;
    for i in (0..items.len()).rev() {
        if !keep[i] {
            continue;
        }
        if is_stmt(&items[i]) && next.is_some_and(|n| sigs[n].is_some() && sigs[n] == sigs[i]) {
            keep[i] = false;
            continue;
        }
        next = Some(i);
    }
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for (item, k) in items.into_iter().zip(keep) {
        if !k {
            continue;
        }
        if let (Some(Item::Layers(prev, pc, false)), Item::Layers(names, chain, false)) =
            (out.last_mut(), &item)
            && pc == chain
        {
            prev.extend(names.iter().cloned());
            continue;
        }
        out.push(item);
    }
    out
}

/// The number of leading `@layer` statements hoisted for a file with imports.
fn leading_layer_count(sheet: &Sheet) -> usize {
    if !sheet.rules.iter().any(|n| matches!(n, Node::Import(_))) {
        return 0;
    }
    sheet
        .rules
        .iter()
        .take_while(|n| matches!(n, Node::At { name, body: None, .. } if name == "layer"))
        .count()
}

fn wrap_depth(chain: &[Cond]) -> usize {
    chain
        .iter()
        .map(|c| {
            usize::from(c.layer.is_some())
                + usize::from(c.supports.is_some())
                + usize::from(!c.media.is_empty())
        })
        .sum()
}

/// `inner` (printed at the chain's depth) wrapped in the chain's conditions.
fn wrap(mut inner: String, chain: &[Cond], minify: bool, base: usize) -> String {
    let mut depth = base + wrap_depth(chain);
    let pad = |d: usize| {
        if minify {
            String::new()
        } else {
            "  ".repeat(d)
        }
    };
    let nl = if minify { "" } else { "\n" };
    let open = if minify { "{" } else { " {\n" };
    for c in chain.iter().rev() {
        let mut layers: Vec<String> = Vec::new();
        if let Some(l) = &c.layer {
            layers.push(if l.is_empty() {
                "@layer".to_owned()
            } else {
                format!("@layer {l}")
            });
        }
        let mut wrappers: Vec<(String, bool)> = layers.into_iter().map(|l| (l, true)).collect();
        if let Some(s) = &c.supports {
            wrappers.push((format!("@supports ({s})"), false));
        }
        if !c.media.is_empty() {
            wrappers.push((format!("@media {}", c.media), false));
        }
        for (w, is_layer) in wrappers {
            depth = depth.saturating_sub(1);
            if inner.is_empty() {
                if is_layer && w != "@layer" {
                    inner = format!("{}{w};{nl}", pad(depth));
                }
                continue;
            }
            inner = format!("{}{w}{open}{inner}{}}}{nl}", pad(depth), pad(depth));
        }
    }
    inner
}

/// `@import "path" conditions;`, nested in `data:text/css` imports for a chain of more
/// than one level of conditions.
/// `@import "path" conditions;` (`@import"path"conditions;` minified, as esbuild prints it).
fn external_import(path: &str, chain: &[Cond], minify: bool) -> String {
    let q = super::lexer::quote;
    let Some((last, outer)) = chain.split_last() else {
        return with_conditions(&q(path), "", minify);
    };
    let mut s = with_conditions(&q(path), &last.text(), minify);
    for c in outer.iter().rev() {
        s = with_conditions(&q(&format!("data:text/css,{s}")), &c.text(), minify);
    }
    s
}

pub(super) fn with_conditions(target: &str, conditions: &str, minify: bool) -> String {
    let sp = if minify { "" } else { " " };
    if conditions.is_empty() {
        format!("@import{sp}{target};")
    } else {
        format!("@import{sp}{target}{sp}{conditions};")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(layer: Option<&str>, supports: Option<&str>, media: &str) -> Cond {
        Cond {
            layer: layer.map(str::to_owned),
            supports: supports.map(str::to_owned),
            media: media.to_owned(),
        }
    }

    /// Files by name; `@import "name"` resolves to the file, `http:` imports are external.
    struct Files(Vec<(&'static str, Sheet)>);

    impl Host for Files {
        fn sheet(&self, m: ModuleId) -> Option<&Sheet> {
            self.0.get(m as usize).map(|f| &f.1)
        }
        fn target(&self, m: ModuleId, r: u32) -> Target {
            let spec = &self.0[m as usize].1.records[r as usize].spec;
            if spec.starts_with("http:") {
                return Target::External(spec.clone());
            }
            match self.0.iter().position(|f| f.0 == spec) {
                Some(i) => Target::Module(i as ModuleId),
                None => Target::Missing,
            }
        }
        fn url(&self, m: ModuleId, r: u32) -> String {
            self.0[m as usize].1.records[r as usize].spec.clone()
        }
        fn local(&self, m: ModuleId, n: u32) -> String {
            self.0[m as usize].1.locals[n as usize].clone()
        }
        fn pretty(&self, m: ModuleId) -> String {
            self.0[m as usize].0.to_owned()
        }
    }

    /// The minified bundle of `entry` with `a.css` (`@layer first`), `b.css` (`@layer last`)
    /// and `c.css` (`@layer c;` before an external import).
    fn run(entry: &str, syntax: bool) -> String {
        let parse =
            |s: &str| super::super::parse(std::sync::Arc::from(s), super::super::NameMode::Global);
        let files = Files(vec![
            ("entry.css", parse(entry)),
            ("a.css", parse("@layer first { body { color: red } }")),
            ("b.css", parse("@layer last { body { color: green } }")),
            ("c.css", parse("@layer c; @import \"http://x/c.css\";")),
            ("p.css", parse("@charset \"utf-8\"; p { color: blue }")),
        ]);
        bundle(&files, &[0], true, syntax)
            .expect("within budget")
            .unwrap_or_default()
    }

    /// `f0.css` .. `f{n-1}.css`, each importing the next twice; the last has one rule.
    fn doubling(n: usize) -> Files {
        let parse =
            |s: &str| super::super::parse(std::sync::Arc::from(s), super::super::NameMode::Global);
        let names: Vec<&'static str> = (0..n)
            .map(|i| &*Box::leak(format!("f{i}.css").into_boxed_str()))
            .collect();
        Files(
            (0..n)
                .map(|i| {
                    let src = if i + 1 == n {
                        "a { b: c }".to_owned()
                    } else {
                        format!("@import '{0}'; @import '{0}';", names[i + 1])
                    };
                    (names[i], parse(&src))
                })
                .collect(),
        )
    }

    #[test]
    fn doubling_imports_stop_at_the_budget() {
        // 2^27 copies of the last file without the budget; the walk stops after
        // `MAX_EXPANSION` items instead.
        assert_eq!(
            bundle(&doubling(28), &[0], true, false),
            Err(TooManyImports { root: 0 })
        );
        // Below the budget the copies collapse to one of each file.
        assert_eq!(
            bundle(&doubling(12), &[0], true, false),
            Ok(Some("a{b:c}\n".to_owned()))
        );
    }

    #[test]
    fn dropped_duplicates_keep_only_needed_layer_statements() {
        let r = |s: &str| run(s, false);
        assert_eq!(
            r("@import 'a.css'; @import 'a.css';"),
            "@layer first{body{color:red}}\n"
        );
        assert_eq!(
            r("@import 'a.css'; @import 'b.css'; @import 'a.css'; @import 'b.css';"),
            "@layer first,last;@layer first{body{color:red}}@layer last{body{color:green}}\n"
        );
        assert_eq!(
            r(
                "@import 'a.css'; @import 'b.css'; @import 'a.css'; @import 'b.css'; @import 'a.css';"
            ),
            "@layer first;@layer last{body{color:green}}@layer first{body{color:red}}\n"
        );
        assert_eq!(
            r("@import 'a.css' supports(x:y) screen; @import 'b.css'; @import 'a.css' screen;"),
            "@media screen{@supports (x:y){@layer first;}}@layer last{body{color:green}}@media screen{@layer first{body{color:red}}}\n"
        );
    }

    #[test]
    fn only_the_first_files_leading_layers_go_above_external_imports() {
        assert_eq!(
            run(
                "@import 'c.css' supports(f:1); @import 'b.css'; @import 'c.css' supports(s:1);",
                false
            ),
            "@supports (f:1){@layer c;}@import\"http://x/c.css\"supports(f:1);@import\"http://x/c.css\"supports(s:1);@layer last{body{color:green}}@supports (s:1){@layer c;}\n"
        );
    }

    #[test]
    fn charset_and_late_imports() {
        assert_eq!(
            run("@import 'p.css'; a { b: c } @import 'a.css';", false),
            "@charset \"UTF-8\";p{color:blue}a{b:c}@import \"a.css\";\n"
        );
    }

    #[test]
    fn minify_syntax_drops_repeated_rules() {
        assert_eq!(
            run(
                "@import 'p.css'; a { x: 1; x: 2; x: 1 } p { color: blue } @media s { & a { y: z } } @media s { a { y: z } }",
                true
            ),
            "@charset \"UTF-8\";a{x:2;x:1}p{color:#00f}@media s{a{y:z}}\n"
        );
    }

    #[test]
    fn redundancy() {
        assert!(redundant(&[c(None, None, "screen")], &[]));
        assert!(redundant(
            &[c(Some("a"), Some("s"), "m")],
            &[c(Some("a"), None, "m")]
        ));
        assert!(!redundant(
            &[c(Some("a"), Some("s"), "")],
            &[c(Some("a"), None, "m")]
        ));
        assert!(!redundant(&[c(None, None, "")], &[c(Some("a"), None, "")]));
    }

    #[test]
    fn wrapping() {
        let chain = [c(Some("a"), Some("s"), "m")];
        assert_eq!(
            wrap(String::new(), &chain, true, 0),
            "@media m{@supports (s){@layer a;}}"
        );
        assert_eq!(wrap(String::new(), &[c(None, Some("s"), "")], true, 0), "");
        assert_eq!(
            external_import(
                "x",
                &[c(Some("a"), None, "print"), c(Some("b"), None, "")],
                false
            ),
            "@import 'data:text/css,@import \"x\" layer(b);' layer(a) print;"
        );
    }
}
