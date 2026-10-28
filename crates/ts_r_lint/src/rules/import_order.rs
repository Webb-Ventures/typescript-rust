//! `import/order`: the order of a module's imports, requires and (optionally) named
//! specifiers and CommonJS exports.
//!
//! Every import (with specifiers, or any with `warnOnUnassignedImports`), `import x = ...`
//! (not `export import`) and top-level `var x = require("y")` gets a rank: twice the index
//! of the group its type is in (`groups`, default builtin, external, parent, sibling,
//! index; types left out share the rank after the last group). A path group whose glob
//! matches the specifier moves it to its group's rank plus `position / maxPosition`
//! (`before` path groups count down from -1, `after` ones up from 1; maxPosition is 10 or
//! the next power of ten). Type-only imports go to the `type` group when it is listed (with
//! `sortTypesGroup`, to `type + rank / 10`); requires rank 100 higher than imports.
//! `alphabetize` then renumbers each rank's members in sorted order (segment-wise compare,
//! leading `.`/`..` segments skipped; imports with the same text and import kind share the
//! last rank given).
//!
//! An item whose rank is lower than one before it is out of order; when reading the list
//! backwards gives fewer such items those are reported instead ("after"). The fix moves the
//! whole line (with comments on the same line) next to the first item ranked higher, but
//! only when every statement between the two can be crossed (single-declarator requires,
//! imports with specifiers, `import x = require()`): the two statement indexes are sorted
//! as strings, as the plugin does, so the span checked can be empty.
//!
//! `newlines-between` checks blank lines between consecutive items (with `distinctGroup`
//! a rank change is a group change; without it only a jump of at least one whole rank),
//! `consolidateIslands: "inside-groups"` asks for blank lines around multi-line items.
//! `named` checks the specifiers of each import, `export {}`, destructuring require and
//! (`cjsExports`) `module.exports = {}` object; with `alphabetize`, assignments to
//! `exports.x` / `module.exports.x` in one block are ordered too.
//!
//! Written from the plugin's docs, its tests and `src/rules/order.js` (eslint-plugin-import
//! 2.32.0), read to learn the ranking, reporting and fix rules above.

use super::import_util::{resolve, string_literal, walk};
use super::prelude::*;
use crate::modules::import_type::{ImportType, import_type};
use crate::modules::minimatch::{MatchOptions, glob_match};
use rustc_hash::FxHashMap;
use std::cmp::Ordering;
use ts_r_ast::{Ast, CommentKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/order",
        "Enforce a convention in module import order.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<Order>,
};

/// The types a group can name, in the plugin's order.
const TYPES: [&str; 9] = [
    "builtin", "external", "internal", "unknown", "parent", "sibling", "index", "object", "type",
];
const DEFAULT_GROUPS: [&str; 5] = ["builtin", "external", "parent", "sibling", "index"];
const TYPE: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Newlines {
    Ignore,
    Always,
    AlwaysAndInside,
    Never,
}

impl Newlines {
    fn parse(v: Option<&str>) -> Option<Newlines> {
        Some(match v? {
            "always" => Newlines::Always,
            "always-and-inside-groups" => Newlines::AlwaysAndInside,
            "never" => Newlines::Never,
            _ => Newlines::Ignore,
        })
    }
    fn asks_newline(self) -> bool {
        matches!(self, Newlines::Always | Newlines::AlwaysAndInside)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedTypes {
    Mixed,
    TypesFirst,
    TypesLast,
}

#[derive(Debug, Clone, Copy)]
struct Named {
    import: bool,
    export: bool,
    require: bool,
    cjs_exports: bool,
    types: NamedTypes,
}

#[derive(Debug)]
struct PathGroup {
    pattern: String,
    opts: MatchOptions,
    group: Option<usize>,
    position: f64,
}

#[derive(Debug, Clone, Copy)]
struct Alphabetize {
    /// 1 ascending, -1 descending, 0 ignore.
    order: i32,
    order_kind: i32,
    case_insensitive: bool,
}

#[derive(Debug)]
struct Order {
    /// The rank of each of [`TYPES`].
    ranks: [f64; 9],
    type_in_groups: bool,
    path_groups: Vec<PathGroup>,
    max_position: f64,
    excluded: Vec<String>,
    newlines: Newlines,
    newlines_types: Newlines,
    consolidate: bool,
    sorting_types: bool,
    distinct_group: bool,
    warn_unassigned: bool,
    named: Named,
    alpha: Alphabetize,
}

fn direction(v: Option<&str>) -> i32 {
    match v {
        Some("asc") => 1,
        Some("desc") => -1,
        _ => 0,
    }
}

impl FromOptions for Order {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let get = |k: &str| o.get(k);
        let newlines = Newlines::parse(get("newlines-between").and_then(Value::as_str))
            .unwrap_or(Newlines::Ignore);
        let newlines_types = Newlines::parse(get("newlines-between-types").and_then(Value::as_str))
            .unwrap_or(newlines);
        let excluded = match get("pathGroupsExcludedImportTypes").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            None => vec!["builtin".into(), "external".into(), "object".into()],
        };
        let consolidate =
            get("consolidateIslands").and_then(Value::as_str) == Some("inside-groups");
        let sort_types_group = get("sortTypesGroup").and_then(Value::as_bool) == Some(true);
        let named = match get("named") {
            Some(Value::Object(n)) => {
                let enabled = n.get("enabled").and_then(Value::as_bool) == Some(true);
                let flag = |k: &str| match n.get(k) {
                    Some(v) => v.as_bool() == Some(true),
                    None => enabled,
                };
                Named {
                    import: flag("import"),
                    export: flag("export"),
                    require: flag("require"),
                    cjs_exports: flag("cjsExports"),
                    types: match n.get("types").and_then(Value::as_str) {
                        Some("types-first") => NamedTypes::TypesFirst,
                        Some("types-last") => NamedTypes::TypesLast,
                        _ => NamedTypes::Mixed,
                    },
                }
            }
            other => {
                let on = other.and_then(Value::as_bool) == Some(true);
                Named {
                    import: on,
                    export: on,
                    require: on,
                    cjs_exports: on,
                    types: NamedTypes::Mixed,
                }
            }
        };
        let alpha = get("alphabetize");
        let alpha = Alphabetize {
            order: direction(alpha.and_then(|a| a.get("order")).and_then(Value::as_str)),
            order_kind: direction(
                alpha
                    .and_then(|a| a.get("orderImportKind"))
                    .and_then(Value::as_str),
            ),
            case_insensitive: alpha
                .and_then(|a| a.get("caseInsensitive"))
                .and_then(Value::as_bool)
                == Some(true),
        };
        let distinct_group = match get("distinctGroup") {
            None | Some(Value::Null) => true,
            Some(v) => truthy(v),
        };

        // Groups to ranks.
        let groups: Vec<Vec<String>> = match get("groups").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .map(|g| match g {
                    Value::Array(items) => items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                    Value::String(s) => vec![s.clone()],
                    _ => Vec::new(),
                })
                .collect(),
            None => DEFAULT_GROUPS
                .iter()
                .map(|g| vec![(*g).to_owned()])
                .collect(),
        };
        let mut ranks = [f64::NAN; 9];
        for (i, group) in groups.iter().enumerate() {
            for item in group {
                if let Some(t) = TYPES.iter().position(|t| t == item) {
                    ranks[t] = i as f64 * 2.0;
                }
            }
        }
        let type_in_groups = !ranks[TYPE].is_nan();
        for r in &mut ranks {
            if r.is_nan() {
                *r = groups.len() as f64 * 2.0;
            }
        }

        // Path groups to positions.
        let mut path_groups = Vec::new();
        let mut after: FxHashMap<String, u32> = FxHashMap::default();
        let mut before: Vec<(String, Vec<usize>)> = Vec::new();
        for (index, pg) in get("pathGroups")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let group = pg.get("group").and_then(Value::as_str).unwrap_or("");
            let mut position = 0.0;
            match pg.get("position").and_then(Value::as_str) {
                Some("after") => {
                    let next = after.entry(group.to_owned()).or_insert(1);
                    position = f64::from(*next);
                    *next += 1;
                }
                Some("before") => match before.iter_mut().find(|(g, _)| g == group) {
                    Some((_, list)) => list.push(index),
                    None => before.push((group.to_owned(), vec![index])),
                },
                _ => {}
            }
            let opts = match pg.get("patternOptions").and_then(Value::as_object) {
                Some(po) => MatchOptions {
                    nocomment: po.get("nocomment").is_some_and(truthy),
                    nonegate: po.get("nonegate").is_some_and(truthy),
                },
                None => MatchOptions {
                    nocomment: true,
                    nonegate: false,
                },
            };
            path_groups.push(PathGroup {
                pattern: pg
                    .get("pattern")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                opts,
                group: TYPES.iter().position(|t| *t == group),
                position,
            });
        }
        let mut max_position: f64 = 1.0;
        for (_, list) in &before {
            let len = list.len();
            for (i, &at) in list.iter().enumerate() {
                path_groups[at].position = -((len - i) as f64);
            }
            max_position = max_position.max(len as f64);
        }
        for next in after.values() {
            max_position = max_position.max(f64::from(*next) - 1.0);
        }
        let max_position = if max_position > 10.0 {
            10f64.powf(max_position.log10().ceil())
        } else {
            10.0
        };

        Ok(Order {
            ranks,
            type_in_groups,
            path_groups,
            max_position,
            excluded,
            newlines,
            newlines_types,
            consolidate,
            sorting_types: type_in_groups && sort_types_group,
            distinct_group,
            warn_unassigned: get("warnOnUnassignedImports").is_some_and(truthy),
            named,
            alpha,
        })
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryType {
    Import,
    ImportObject,
    Require,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cat {
    Import,
    Named,
    Exports,
}

#[derive(Debug, Clone)]
struct Entry {
    /// The node reported (declaration, `require()` call, specifier, assignment).
    node: NodeId,
    value: String,
    display: String,
    alias: Option<String>,
    ty: EntryType,
    /// ESTree's `importKind` of the node (`None` where the parser leaves it out).
    import_kind: Option<&'static str>,
    /// `exportKind: "type"` (export specifiers).
    type_export: bool,
    rank: f64,
    multiline: bool,
}

impl Entry {
    fn description(&self) -> &'static str {
        if self.ty == EntryType::Export {
            return if self.type_export {
                "type export"
            } else {
                "export"
            };
        }
        match self.import_kind {
            Some("type") => "type import",
            Some("typeof") => "typeof import",
            _ => "import",
        }
    }
    fn key(&self) -> String {
        format!("{}|{}", self.value, self.import_kind.unwrap_or("undefined"))
    }
}

/// A named specifier (or property) before ranking.
struct NamedItem {
    node: NodeId,
    value: String,
    alias: Option<String>,
    ty: EntryType,
    /// `importKind` / `exportKind` as the named grouping reads it (`None`: value).
    kind: Option<&'static str>,
    import_kind: Option<&'static str>,
    type_export: bool,
}

fn type_name(t: ImportType) -> &'static str {
    match t {
        ImportType::Internal => "internal",
        ImportType::Absolute => "absolute",
        ImportType::Builtin => "builtin",
        ImportType::Parent => "parent",
        ImportType::Index => "index",
        ImportType::Sibling => "sibling",
        ImportType::External => "external",
        ImportType::Unknown => "unknown",
    }
}

/// ESTree's `name` of an identifier, as JavaScript prints a missing one.
fn estree_name(ast: &Ast, id: NodeId) -> String {
    ident_name(ast, id).unwrap_or("undefined").to_owned()
}

/// ESTree nodes with a `body` property, where the root search stops.
fn has_body(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Program
            | NodeKind::Block
            | NodeKind::ModuleBlock
            | NodeKind::StaticBlock
            | NodeKind::FunctionDecl
            | NodeKind::FunctionExpr
            | NodeKind::Arrow
            | NodeKind::ClassDecl
            | NodeKind::ClassExpr
            | NodeKind::For
            | NodeKind::ForIn
            | NodeKind::ForOf
            | NodeKind::While
            | NodeKind::DoWhile
            | NodeKind::Labeled
            | NodeKind::With
            | NodeKind::CatchClause
            | NodeKind::Module
            | NodeKind::Interface
    )
}

fn find_root(cx: &Ctx<'_, '_>, node: NodeId) -> NodeId {
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        if has_body(cx.ast().kind(p)) {
            break;
        }
        cur = p;
    }
    cur
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.kind(skip_parens(ast, id)),
        NodeKind::StringLit
            | NodeKind::NumberLit
            | NodeKind::BoolLit
            | NodeKind::NullLit
            | NodeKind::RegexLit
            | NodeKind::BigIntLit
    )
}

/// `require(<literal>)` (the plugin's `isRequireExpression`).
fn is_require_expression(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } => {
            ident_name(ast, callee) == Some("require")
                && ast.list(args).len() == 1
                && is_literal(ast, ast.list(args)[0])
        }
        _ => false,
    }
}

/// `require("x")` with one string argument.
fn is_static_require(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } if ident_name(ast, callee) == Some("require") && ast.list(args).len() == 1 => {
            string_literal(ast, ast.list(args)[0])
        }
        _ => None,
    }
}

fn exported(ast: &Ast, id: NodeId) -> bool {
    ast.node(id).modifiers().contains(Modifiers::EXPORT)
}

/// A statement an import may be moved across.
fn can_cross(ast: &Ast, stmt: NodeId) -> bool {
    match *ast.node(stmt) {
        Node::VarDecl { decls, .. } if !exported(ast, stmt) => {
            let [decl] = ast.list(decls) else {
                return false;
            };
            let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(*decl)
            else {
                return false;
            };
            if !matches!(ast.kind(name), NodeKind::Ident | NodeKind::ObjectPattern) {
                return false;
            }
            if is_require_expression(ast, init) {
                return true;
            }
            match *ast.node(skip_parens(ast, init)) {
                Node::Call {
                    callee,
                    optional: false,
                    ..
                } => match *ast.node(skip_parens(ast, callee)) {
                    Node::Member {
                        obj,
                        optional: false,
                        ..
                    }
                    | Node::Index {
                        obj,
                        optional: false,
                        ..
                    } => is_require_expression(ast, obj),
                    _ => false,
                },
                _ => false,
            }
        }
        Node::ImportDecl { specifiers, .. } => !ast.list(specifiers).is_empty(),
        Node::ImportEquals { module_ref, .. } => {
            ast.kind(module_ref) == NodeKind::ExternalModuleRef
        }
        _ => false,
    }
}

fn can_reorder(cx: &Ctx<'_, '_>, first: NodeId, second: NodeId) -> bool {
    let ast = cx.ast();
    let Some(body) = cx.parent(first).and_then(|p| statement_list(ast, p)) else {
        return true;
    };
    // Statements are in source order, so a binary search finds each one.
    let index = |n: NodeId| -> i64 {
        let start = cx.span(n).start;
        let at = body.partition_point(|&s| cx.span(s).start < start);
        if body.get(at) == Some(&n) {
            at as i64
        } else {
            -1
        }
    };
    // `[a, b].sort()` compares the numbers as strings.
    let mut pair = [index(first), index(second)];
    pair.sort_by_key(|n| n.to_string());
    let [a, b] = pair;
    let len = body.len() as i64;
    let clamp = |n: i64| -> usize {
        let n = if n < 0 { (len + n).max(0) } else { n.min(len) };
        n as usize
    };
    let (from, to) = (clamp(a), clamp(b + 1));
    from >= to || body[from..to].iter().all(|&s| can_cross(ast, s))
}

/// JavaScript's `substring`-style slice that yields "" for a reversed or broken range.
fn slice(src: &str, a: u32, b: u32) -> &str {
    if a >= b {
        return "";
    }
    src.get(a as usize..b as usize).unwrap_or("")
}

fn line(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    cx.file.lines.line_of(pos)
}

/// Comments right after `end` (nothing but whitespace before each) that sit on one line,
/// the line `node_end_line`; returns where the last one ends.
fn same_line_comments_after(cx: &Ctx<'_, '_>, end: u32, node_end_line: u32) -> u32 {
    let src = cx.src();
    let mut last = end;
    for (n, c) in cx
        .ast()
        .comments_in(Span::new(end, src.len() as u32))
        .iter()
        .enumerate()
    {
        if n >= 100 || !slice(src, last, c.span.start).trim().is_empty() {
            break;
        }
        if !matches!(c.kind, CommentKind::Line | CommentKind::Block)
            || line(cx, c.span.start) != node_end_line
            || line(cx, c.span.end) != node_end_line
        {
            break;
        }
        last = c.span.end;
    }
    last
}

/// Comments right before `start` that sit on the line `node_end_line`; returns where the
/// first one starts.
fn same_line_comments_before(cx: &Ctx<'_, '_>, start: u32, node_end_line: u32) -> u32 {
    let src = cx.src();
    let before = cx.ast().comments_between(0, start);
    let mut first = start;
    for (n, c) in before.iter().rev().enumerate() {
        if n >= 100 || c.span.end > first || !slice(src, c.span.end, first).trim().is_empty() {
            break;
        }
        if !matches!(c.kind, CommentKind::Line | CommentKind::Block)
            || line(cx, c.span.start) != node_end_line
            || line(cx, c.span.end) != node_end_line
        {
            break;
        }
        first = c.span.start;
    }
    first
}

fn end_of_line_with_comments(cx: &Ctx<'_, '_>, node: NodeId) -> u32 {
    let span = cx.span(node);
    let b = cx.src().as_bytes();
    let end = same_line_comments_after(cx, span.end, line(cx, span.end));
    let mut result = end as usize;
    for (i, &c) in b.iter().enumerate().skip(end as usize) {
        if c == b'\n' {
            result = i + 1;
            break;
        }
        if c != b' ' && c != b'\t' && c != b'\r' {
            break;
        }
        result = i + 1;
    }
    result as u32
}

fn start_of_line_with_comments(cx: &Ctx<'_, '_>, node: NodeId) -> u32 {
    let span = cx.span(node);
    let b = cx.src().as_bytes();
    let start = same_line_comments_before(cx, span.start, line(cx, span.end));
    let mut result = start as usize;
    let mut i = start as usize;
    while i > 1 {
        i -= 1;
        if b[i] != b' ' && b[i] != b'\t' {
            break;
        }
        result = i;
    }
    result as u32
}

/// Where a specifier's text starts: just after the `,` or `{` before it.
fn specifier_start(cx: &Ctx<'_, '_>, node: NodeId) -> u32 {
    skip_trivia_backward(cx.ast(), cx.src(), cx.span(node).start as usize) as u32
}

/// Where a specifier's trailing trivia ends: at the `,` or `}` after it.
fn specifier_end(cx: &Ctx<'_, '_>, node: NodeId) -> u32 {
    skip_trivia_forward(cx.src(), cx.span(node).end as usize) as u32
}

/// UTF-16 order, as JavaScript compares strings.
fn compare_js(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn sorter(alpha: Alphabetize, a: &Entry, b: &Entry) -> i32 {
    let (va, vb) = if alpha.case_insensitive {
        (a.value.to_lowercase(), b.value.to_lowercase())
    } else {
        (a.value.clone(), b.value.clone())
    };
    let ord = |o: Ordering| match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    };
    let mut result = 0;
    if !va.contains('/') && !vb.contains('/') {
        result = ord(compare_js(&va, &vb));
    } else {
        let sa: Vec<&str> = va.split('/').collect();
        let sb: Vec<&str> = vb.split('/').collect();
        for i in 0..sa.len().min(sb.len()) {
            let rel = |s: &str| s == "." || s == "..";
            if i == 0 && rel(sa[i]) && rel(sb[i]) {
                if sa[i] != sb[i] {
                    break;
                }
                continue;
            }
            result = ord(compare_js(sa[i], sb[i]));
            if result != 0 {
                break;
            }
        }
        if result == 0 && sa.len() != sb.len() {
            result = if sa.len() < sb.len() { -1 } else { 1 };
        }
    }
    result *= alpha.order;
    if result == 0 && alpha.order_kind != 0 {
        result = alpha.order_kind
            * ord(compare_js(
                a.import_kind.unwrap_or("value"),
                b.import_kind.unwrap_or("value"),
            ));
    }
    result
}

/// A stable merge sort that never panics on an inconsistent comparison.
fn stable_sort(items: &mut Vec<usize>, cmp: &dyn Fn(usize, usize) -> i32) {
    if items.len() < 2 {
        return;
    }
    let mid = items.len() / 2;
    let mut right = items.split_off(mid);
    stable_sort(items, cmp);
    stable_sort(&mut right, cmp);
    let left = std::mem::take(items);
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if cmp(right[j], left[i]) < 0 {
            items.push(right[j]);
            j += 1;
        } else {
            items.push(left[i]);
            i += 1;
        }
    }
    items.extend_from_slice(&left[i..]);
    items.extend_from_slice(&right[j..]);
}

fn alphabetize_ranks(list: &mut [Entry], alpha: Alphabetize) {
    // Members of each rank, ranks ascending, members in list order.
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by(|&a, &b| {
        list[a]
            .rank
            .partial_cmp(&list[b].rank)
            .unwrap_or(Ordering::Equal)
    });
    let mut new_rank = 0.0;
    let mut by_key: FxHashMap<String, f64> = FxHashMap::default();
    let mut start = 0;
    while start < order.len() {
        let rank = list[order[start]].rank;
        let mut end = start + 1;
        while end < order.len() && list[order[end]].rank == rank {
            end += 1;
        }
        let mut members = order[start..end].to_vec();
        stable_sort(&mut members, &|a, b| sorter(alpha, &list[a], &list[b]));
        for i in members {
            by_key.insert(list[i].key(), rank.trunc() + new_rank);
            new_rank += 1.0;
        }
        start = end;
    }
    for e in list.iter_mut() {
        if let Some(&r) = by_key.get(&e.key()) {
            e.rank = r;
        }
    }
}

fn find_out_of_order(list: &[Entry]) -> Vec<usize> {
    let Some(first) = list.first() else {
        return Vec::new();
    };
    let mut max = first.rank;
    let mut out = Vec::new();
    for (i, e) in list.iter().enumerate() {
        if e.rank < max {
            out.push(i);
        }
        if max < e.rank {
            max = e.rank;
        }
    }
    out
}

impl Order {
    fn compute_rank(&self, cx: &mut Ctx<'_, '_>, e: &Entry) -> f64 {
        let type_only = e.import_kind == Some("type");
        let excluded = |t: &str| self.excluded.iter().any(|x| x == t);
        let excluded_from_path = type_only && self.type_in_groups && excluded("type");
        let imp_type: &str = if e.ty == EntryType::ImportObject {
            "object"
        } else if type_only && self.type_in_groups && !self.sorting_types {
            "type"
        } else {
            let resolved = resolve(cx, &e.value);
            type_name(import_type(
                cx.file.modules(),
                &e.value,
                resolved.as_ref().and_then(|r| r.path()),
            ))
        };
        let mut rank = None;
        if !excluded(imp_type) && !excluded_from_path {
            rank = self
                .path_groups
                .iter()
                .find(|pg| glob_match(&e.value, &pg.pattern, pg.opts))
                .map(|pg| {
                    pg.group.map_or(f64::NAN, |g| self.ranks[g]) + pg.position / self.max_position
                });
        }
        let mut rank = match rank {
            Some(r) => r,
            None => match TYPES.iter().position(|t| *t == imp_type) {
                Some(t) => self.ranks[t],
                None => return -1.0,
            },
        };
        if type_only && self.sorting_types {
            rank = self.ranks[TYPE] + rank / 10.0;
        }
        if !matches!(e.ty, EntryType::Import | EntryType::ImportObject) {
            rank += 100.0;
        }
        rank
    }

    fn register(
        &self,
        cx: &mut Ctx<'_, '_>,
        mut e: Entry,
        multiline_node: NodeId,
        blocks: &mut Vec<(NodeId, Vec<Entry>)>,
        block: NodeId,
    ) {
        let rank = self.compute_rank(cx, &e);
        if rank == -1.0 {
            return;
        }
        let span = cx.span(multiline_node);
        e.rank = rank;
        e.multiline = line(cx, span.start) != line(cx, span.end);
        match blocks.iter_mut().find(|(b, _)| *b == block) {
            Some((_, list)) => list.push(e),
            None => blocks.push((block, vec![e])),
        }
    }

    fn named_report(&self, cx: &mut Ctx<'_, '_>, items: Vec<NamedItem>) {
        if items.len() < 2 {
            return;
        }
        let groups: &[&str] = match self.named.types {
            NamedTypes::Mixed => &[],
            NamedTypes::TypesLast => &["value"],
            NamedTypes::TypesFirst => &["type"],
        };
        let mut list: Vec<Entry> = items
            .into_iter()
            .map(|it| {
                let kind = it.kind.unwrap_or("value");
                let rank = groups
                    .iter()
                    .position(|g| *g == kind)
                    .unwrap_or(groups.len());
                Entry {
                    node: it.node,
                    display: it.value.clone(),
                    value: format!("{}:{}", it.value, it.alias.as_deref().unwrap_or("")),
                    alias: it.alias,
                    ty: it.ty,
                    import_kind: it.import_kind,
                    type_export: it.type_export,
                    rank: rank as f64,
                    multiline: false,
                }
            })
            .collect();
        if self.alpha.order != 0 {
            alphabetize_ranks(&mut list, self.alpha);
        }
        out_of_order_report(cx, list, Cat::Named);
    }

    fn newlines_report(&self, cx: &mut Ctx<'_, '_>, list: &[Entry]) {
        let consolidating = self.consolidate
            && (self.newlines == Newlines::AlwaysAndInside
                || self.newlines_types == Newlines::AlwaysAndInside);
        let lines = cx.file.lines;
        let src = cx.src();
        for pair in list.windows(2) {
            let (prev, cur) = (&pair[0], &pair[1]);
            let prev_end = line(cx, cx.span(prev.node).end);
            let cur_start = line(cx, cx.span(cur.node).start);
            let empty_lines = (prev_end + 1..cur_start)
                .filter(|&l| {
                    lines
                        .line_span(l)
                        .is_some_and(|s| s.text(src).trim().is_empty())
                })
                .count();
            let distinct_start = cur.rank - 1.0 >= prev.rank;
            let type_only = cur.import_kind == Some("type");
            let prev_type_only = prev.import_kind == Some("type");
            let next_to_type = type_only != prev_type_only && self.sorting_types;
            let type_relevant = type_only && self.sorting_types;
            let multiline = prev.multiline || cur.multiline;
            let newlines = if self.sorting_types
                && consolidating
                && multiline
                && self.newlines == Newlines::Never
            {
                Newlines::AlwaysAndInside
            } else {
                self.newlines
            };
            let newlines_types = if self.sorting_types
                && consolidating
                && (next_to_type || multiline)
                && self.newlines_types == Newlines::Never
            {
                Newlines::AlwaysAndInside
            } else {
                self.newlines_types
            };
            let not_ignored = (type_relevant && newlines_types != Newlines::Ignore)
                || (!type_relevant && newlines != Newlines::Ignore);
            if !not_ignored {
                continue;
            }
            let types_side = type_relevant || next_to_type;
            let assert_between = if types_side {
                newlines_types.asks_newline()
            } else {
                newlines.asks_newline()
            };
            let assert_none_within = if types_side {
                newlines_types != Newlines::AlwaysAndInside
            } else {
                newlines != Newlines::AlwaysAndInside
            };
            let assert_none_between =
                !self.sorting_types || !next_to_type || newlines_types == Newlines::Never;
            let same_group = (self.distinct_group && cur.rank == prev.rank)
                || (!self.distinct_group && !distinct_start);
            let mut reported = false;
            if assert_between {
                if cur.rank != prev.rank && empty_lines == 0 {
                    if self.distinct_group || distinct_start {
                        reported = true;
                        let fix = add_newline_fix(cx, prev);
                        cx.report_node(
                            prev.node,
                            "There should be at least one empty line between import groups",
                        )
                        .fix(fix);
                    }
                } else if empty_lines > 0 && assert_none_within && same_group {
                    reported = true;
                    let fix = remove_newline_fix(cx, cur, prev);
                    cx.report_node(
                        prev.node,
                        "There should be no empty line within import group",
                    )
                    .fix_opt(fix);
                }
            } else if empty_lines > 0 && assert_none_between {
                reported = true;
                let fix = remove_newline_fix(cx, cur, prev);
                cx.report_node(
                    prev.node,
                    "There should be no empty line between import groups",
                )
                .fix_opt(fix);
            }
            if !reported && consolidating {
                if empty_lines == 0 && cur.multiline {
                    let fix = add_newline_fix(cx, prev);
                    cx.report_node(
                        prev.node,
                        "There should be at least one empty line between this import and the multi-line import that follows it",
                    )
                    .fix(fix);
                } else if empty_lines == 0 && prev.multiline {
                    let fix = add_newline_fix(cx, prev);
                    cx.report_node(
                        prev.node,
                        "There should be at least one empty line between this multi-line import and the import that follows it",
                    )
                    .fix(fix);
                } else if empty_lines > 0 && !prev.multiline && !cur.multiline && same_group {
                    let fix = remove_newline_fix(cx, cur, prev);
                    cx.report_node(
                        prev.node,
                        "There should be no empty lines between this single-line import and the single-line import that follows it",
                    )
                    .fix_opt(fix);
                }
            }
        }
    }
}

fn add_newline_fix(cx: &Ctx<'_, '_>, prev: &Entry) -> Fix {
    let root = find_root(cx, prev.node);
    let span = cx.span(root);
    let end = same_line_comments_after(cx, span.end, line(cx, span.end));
    Fix::insert(end, "\n")
}

fn remove_newline_fix(cx: &Ctx<'_, '_>, cur: &Entry, prev: &Entry) -> Option<Fix> {
    let from = end_of_line_with_comments(cx, find_root(cx, prev.node));
    let to = start_of_line_with_comments(cx, find_root(cx, cur.node));
    if from > to {
        return None;
    }
    slice(cx.src(), from, to)
        .trim()
        .is_empty()
        .then(|| Fix::replace(Span::new(from, to), ""))
}

fn out_of_order_report(cx: &mut Ctx<'_, '_>, list: Vec<Entry>, category: Cat) {
    let out = find_out_of_order(&list);
    if out.is_empty() {
        return;
    }
    let mut reversed: Vec<Entry> = list
        .iter()
        .rev()
        .map(|e| Entry {
            rank: -e.rank,
            ..e.clone()
        })
        .collect();
    let reversed_out = find_out_of_order(&reversed);
    if reversed_out.len() < out.len() {
        report_list(cx, &mut reversed, &reversed_out, "after", category);
    } else {
        let mut list = list;
        report_list(cx, &mut list, &out, "before", category);
    }
}

fn report_list(
    cx: &mut Ctx<'_, '_>,
    list: &mut [Entry],
    out: &[usize],
    order: &str,
    category: Cat,
) {
    // The first item ranked above `r` is where the running maximum first passes `r`.
    let mut max = f64::NEG_INFINITY;
    let prefix_max: Vec<f64> = list
        .iter()
        .map(|e| {
            if e.rank > max {
                max = e.rank;
            }
            max
        })
        .collect();
    for &i in out {
        let rank = list[i].rank;
        let found = prefix_max.partition_point(|&m| m <= rank);
        if found >= list.len() {
            continue;
        }
        fix_out_of_order(cx, list, found, i, order, category);
    }
}

fn fix_out_of_order(
    cx: &mut Ctx<'_, '_>,
    list: &mut [Entry],
    first: usize,
    second: usize,
    order: &str,
    category: Cat,
) {
    let named = category == Cat::Named;
    let (first_node, second_node) = (list[first].node, list[second].node);
    let (first_root, second_root) = if named {
        (first_node, second_node)
    } else {
        (find_root(cx, first_node), find_root(cx, second_node))
    };
    let (first_start, first_end, second_start, second_end) = if named {
        (
            specifier_start(cx, first_root),
            specifier_end(cx, first_root),
            specifier_start(cx, second_root),
            specifier_end(cx, second_root),
        )
    } else {
        (
            start_of_line_with_comments(cx, first_root),
            end_of_line_with_comments(cx, first_root),
            start_of_line_with_comments(cx, second_root),
            end_of_line_with_comments(cx, second_root),
        )
    };
    if list[first].display == list[second].display {
        for i in [first, second] {
            if let Some(alias) = list[i].alias.clone() {
                list[i].display = format!("{} as {alias}", list[i].display);
            }
        }
    }
    let message = format!(
        "`{}` {} should occur {order} {} of `{}`",
        list[second].display,
        list[second].description(),
        list[first].description(),
        list[first].display
    );
    let src = cx.src();
    let fr_end = cx.span(first_root).end;
    let sr_end = cx.span(second_root).end;
    let fix = if named {
        let first_code = slice(src, first_start, fr_end);
        let first_trivia = slice(src, fr_end, first_end);
        let second_code = slice(src, second_start, sr_end);
        let second_trivia = slice(src, sr_end, second_end);
        if order == "before" {
            let trimmed = second_trivia.trim_end();
            let gap = slice(src, first_end, second_start.saturating_sub(1));
            let ws = &second_trivia[trimmed.len()..];
            Some(Fix::replace(
                Span::new(first_start, second_end),
                format!("{second_code},{trimmed}{first_code}{first_trivia}{gap}{ws}"),
            ))
        } else {
            let trimmed = first_trivia.trim_end();
            let gap = slice(src, second_end + 1, first_start);
            let ws = &first_trivia[trimmed.len()..];
            Some(Fix::replace(
                Span::new(second_start, first_end),
                format!("{gap}{first_code},{trimmed}{second_code}{ws}"),
            ))
        }
    } else {
        let can_fix = category == Cat::Exports || can_reorder(cx, first_root, second_root);
        let mut new_code = slice(src, second_start, second_end).to_owned();
        if !new_code.ends_with('\n') {
            new_code.push('\n');
        }
        can_fix.then(|| {
            if order == "before" {
                Fix::replace(
                    Span::new(first_start, second_end),
                    format!("{new_code}{}", slice_any(src, first_start, second_start)),
                )
            } else {
                Fix::replace(
                    Span::new(second_start, first_end),
                    format!("{}{new_code}", slice_any(src, second_end, first_end)),
                )
            }
        })
    };
    cx.report_node(second_node, message).fix_opt(fix);
}

/// JavaScript's `substring`, which swaps a reversed range.
fn slice_any(src: &str, a: u32, b: u32) -> &str {
    slice(src, a.min(b), a.max(b))
}

/// The block (`Program`) a `require()` call at `call` belongs to: the call (possibly
/// followed by member accesses and calls) must initialise a top-level variable.
fn require_block(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut n = call;
    let parent = |n: NodeId| parent_skip_parens(ast, cx.file.parents, n);
    while let Some(p) = parent(n) {
        let up = match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => skip_parens(ast, obj) == n,
            Node::Call { callee, .. } => skip_parens(ast, callee) == n,
            _ => false,
        };
        if !up {
            break;
        }
        n = p;
    }
    if chain_has_optional(ast, n) {
        return None;
    }
    let decl = parent(n)?;
    let Node::VarDeclarator { init: Some(_), .. } = *ast.node(decl) else {
        return None;
    };
    let var = cx.parent(decl)?;
    if ast.kind(var) != NodeKind::VarDecl || exported(ast, var) {
        return None;
    }
    let program = cx.parent(var)?;
    (ast.kind(program) == NodeKind::Program).then_some(program)
}

/// `module.exports` or `exports`, not shadowed by a declaration in the scope at `node`.
fn is_cjs_exports(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let node = skip_parens(ast, node);
    let name = match *ast.node(node) {
        Node::Member {
            obj,
            prop,
            optional: false,
        } if ident_name(ast, skip_parens(ast, obj)) == Some("module")
            && ident_name(ast, prop) == Some("exports") =>
        {
            "module"
        }
        Node::Ident { .. } if ident_name(ast, node) == Some("exports") => "exports",
        _ => return false,
    };
    let sem = cx.semantic();
    let scope = sem.enclosing_scope(node);
    let sc = sem.scope(scope);
    let declared = sc.symbols.iter().any(|&s| sem.name(s) == name)
        || (sc.kind == ScopeKind::Global && sem.is_global(name));
    !declared
}

/// The property names after `exports` / `module.exports` in an assignment target.
fn named_cjs_exports(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Vec<String>> {
    let ast = cx.ast();
    let mut root = skip_parens(ast, node);
    let mut names = Vec::new();
    let mut parent = None;
    loop {
        let (obj, prop) = match *ast.node(root) {
            Node::Member { obj, prop, .. } => (obj, prop),
            Node::Index { obj, index, .. } => (obj, skip_parens(ast, index)),
            _ => break,
        };
        names.push(ident_name(ast, prop)?.to_owned());
        parent = Some(root);
        root = skip_parens(ast, obj);
    }
    parent?;
    names.reverse();
    if is_cjs_exports(cx, root) {
        return Some(names);
    }
    if parent.is_some_and(|p| is_cjs_exports(cx, p)) {
        names.remove(0);
        return Some(names);
    }
    None
}

impl Rule for Order {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let ts = cx.env().typescript;
        let mut nodes = Vec::new();
        walk(ast, |id| {
            if matches!(
                ast.kind(id),
                NodeKind::ImportDecl
                    | NodeKind::ImportEquals
                    | NodeKind::Call
                    | NodeKind::VarDeclarator
                    | NodeKind::ExportNamed
                    | NodeKind::Assign
            ) {
                nodes.push(id);
            }
        });
        let mut imports: Vec<(NodeId, Vec<Entry>)> = Vec::new();
        let mut exports: Vec<(NodeId, Vec<Entry>)> = Vec::new();
        let kind_of = |type_only: bool| ts.then_some(if type_only { "type" } else { "value" });
        for id in nodes {
            match *ast.node(id) {
                Node::ImportDecl {
                    type_only,
                    specifiers,
                    source,
                    ..
                } => {
                    let specs = ast.list(specifiers);
                    if specs.is_empty() && !self.warn_unassigned {
                        continue;
                    }
                    let Some(block) = cx.parent(id) else {
                        continue;
                    };
                    let name = string_literal(ast, source).unwrap_or("").to_owned();
                    let e = entry(id, name, EntryType::Import, kind_of(type_only));
                    self.register(cx, e, id, &mut imports, block);
                    if self.named.import {
                        let items = specs
                            .iter()
                            .filter_map(|&s| match *ast.node(s) {
                                Node::ImportSpecifier {
                                    type_only,
                                    imported,
                                    local,
                                } => Some(NamedItem {
                                    node: s,
                                    value: estree_name(ast, imported.unwrap_or(local)),
                                    alias: imported.map(|_| estree_name(ast, local)),
                                    ty: EntryType::Import,
                                    kind: kind_of(type_only),
                                    import_kind: kind_of(type_only),
                                    type_export: false,
                                }),
                                _ => None,
                            })
                            .collect();
                        self.named_report(cx, items);
                    }
                }
                Node::ImportEquals {
                    type_only,
                    module_ref,
                    ..
                } => {
                    if exported(ast, id) {
                        continue;
                    }
                    let Some(block) = cx.parent(id) else {
                        continue;
                    };
                    let (value, display, ty) = match *ast.node(module_ref) {
                        Node::ExternalModuleRef { expr } => {
                            let v = string_literal(ast, expr).unwrap_or("").to_owned();
                            (v.clone(), v, EntryType::Import)
                        }
                        _ => (
                            String::new(),
                            cx.text(module_ref).to_owned(),
                            EntryType::ImportObject,
                        ),
                    };
                    let mut e = entry(
                        id,
                        value,
                        ty,
                        Some(if type_only { "type" } else { "value" }),
                    );
                    e.display = display;
                    self.register(cx, e, id, &mut imports, block);
                }
                Node::Call { .. } => {
                    let Some(name) = is_static_require(ast, id) else {
                        continue;
                    };
                    let Some(block) = require_block(cx, id) else {
                        continue;
                    };
                    // The declaration counts for multi-line when the call is its whole
                    // initialiser.
                    let multiline_node = cx
                        .parent(id)
                        .filter(|&d| ast.kind(d) == NodeKind::VarDeclarator)
                        .and_then(|d| cx.parent(d))
                        .filter(|&v| ast.kind(v) == NodeKind::VarDecl)
                        .unwrap_or(id);
                    let e = entry(id, name.to_owned(), EntryType::Require, None);
                    self.register(cx, e, multiline_node, &mut imports, block);
                }
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } if self.named.require => {
                    let Node::ObjectPattern { props } = *ast.node(name) else {
                        continue;
                    };
                    if !is_require_expression(ast, init) {
                        continue;
                    }
                    let mut items = Vec::new();
                    let mut ok = true;
                    for &p in ast.list(props) {
                        let Node::BindingProperty {
                            key,
                            value,
                            init: None,
                        } = *ast.node(p)
                        else {
                            ok = false;
                            break;
                        };
                        let Some(k) = ident_name(ast, key) else {
                            ok = false;
                            break;
                        };
                        let alias = match value {
                            None => None,
                            Some(v) => match ident_name(ast, v) {
                                Some(v) => Some(v.to_owned()),
                                None => {
                                    ok = false;
                                    break;
                                }
                            },
                        };
                        items.push(NamedItem {
                            node: p,
                            value: k.to_owned(),
                            alias,
                            ty: EntryType::Require,
                            kind: None,
                            import_kind: None,
                            type_export: false,
                        });
                    }
                    if ok {
                        self.named_report(cx, items);
                    }
                }
                Node::ExportNamed { specifiers, .. } if self.named.export => {
                    let items = ast
                        .list(specifiers)
                        .iter()
                        .filter_map(|&s| match *ast.node(s) {
                            Node::ExportSpecifier {
                                type_only,
                                local,
                                exported,
                            } => {
                                let kind = ts.then_some(if type_only { "type" } else { "value" });
                                Some(NamedItem {
                                    node: s,
                                    value: estree_name(ast, local),
                                    alias: exported.map(|e| estree_name(ast, e)),
                                    ty: EntryType::Export,
                                    kind,
                                    import_kind: None,
                                    type_export: kind == Some("type"),
                                })
                            }
                            _ => None,
                        })
                        .collect();
                    self.named_report(cx, items);
                }
                Node::Assign { left, right, .. } if self.named.cjs_exports => {
                    let Some(stmt) = parent_skip_parens(ast, cx.file.parents, id) else {
                        continue;
                    };
                    if ast.kind(stmt) != NodeKind::ExprStmt {
                        continue;
                    }
                    if is_cjs_exports(cx, left) {
                        let Node::ObjectLit { props } = *ast.node(skip_parens(ast, right)) else {
                            continue;
                        };
                        let mut items = Vec::new();
                        let mut ok = true;
                        for &p in ast.list(props) {
                            let (key, alias) = match *ast.node(p) {
                                Node::Property { key, value, .. } => {
                                    match (
                                        ident_name(ast, key),
                                        ident_name(ast, skip_parens(ast, value)),
                                    ) {
                                        (Some(k), Some(v)) => (k, Some(v.to_owned())),
                                        _ => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                                Node::ShorthandProp {
                                    name, init: None, ..
                                } => match ident_name(ast, name) {
                                    Some(k) => (k, None),
                                    None => {
                                        ok = false;
                                        break;
                                    }
                                },
                                _ => {
                                    ok = false;
                                    break;
                                }
                            };
                            items.push(NamedItem {
                                node: p,
                                value: key.to_owned(),
                                alias,
                                ty: EntryType::Export,
                                kind: None,
                                import_kind: None,
                                type_export: false,
                            });
                        }
                        if ok {
                            self.named_report(cx, items);
                        }
                    } else if let Some(parts) = named_cjs_exports(cx, left)
                        && !parts.is_empty()
                    {
                        let Some(block) = cx.parent(stmt) else {
                            continue;
                        };
                        let name = parts.join(".");
                        let mut e = entry(id, name, EntryType::Export, None);
                        e.rank = 0.0;
                        match exports.iter_mut().find(|(b, _)| *b == block) {
                            Some((_, list)) => list.push(e),
                            None => exports.push((block, vec![e])),
                        }
                    }
                }
                _ => {}
            }
        }

        for (_, mut list) in imports {
            if self.newlines != Newlines::Ignore || self.newlines_types != Newlines::Ignore {
                self.newlines_report(cx, &list);
            }
            if self.alpha.order != 0 {
                alphabetize_ranks(&mut list, self.alpha);
            }
            out_of_order_report(cx, list, Cat::Import);
        }
        if self.alpha.order != 0 {
            for (_, mut list) in exports {
                alphabetize_ranks(&mut list, self.alpha);
                out_of_order_report(cx, list, Cat::Exports);
            }
        }
    }
}

fn entry(node: NodeId, value: String, ty: EntryType, import_kind: Option<&'static str>) -> Entry {
    Entry {
        node,
        display: value.clone(),
        value,
        alias: None,
        ty,
        import_kind,
        type_export: false,
        rank: 0.0,
        multiline: false,
    }
}
