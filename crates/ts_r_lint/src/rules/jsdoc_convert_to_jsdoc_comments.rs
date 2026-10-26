//! `jsdoc/convert-to-jsdoc-comments`: a plain comment right before (or, for some
//! contexts, right after on the same line) a documented node becomes a JSDoc block.
//!
//! Listeners as the plugin builds them: `contexts` (the option, the `contexts` setting,
//! then functions) check the comment before the node; `contextsAfter` the comment after it
//! on its line (after one `;` or `,`); `contextsBeforeAndAfter` (variable declarators,
//! property signatures and class properties by default) the comment before, then, until a
//! "before" comment has been found anywhere in the file, the one after. A key in a later
//! list replaces the same key in an earlier one. The comment before must be the token just
//! before the node (or its decorator), not a JSDoc block, and within the `minLines` /
//! `maxLines` settings of it. Comments starting with an `allowedPrefixes` entry are left
//! alone. The fix wraps the comment's text, multi-line (`/**`, ` * text`, ` */` at the
//! node's indent plus one) or, with `enforceJsdocLineStyle: "single"` or the context's
//! `inlineCommentBlock`, as `/** text */`; a comment after the node moves before it.

use super::prelude::*;
use crate::diagnostic::{Fix, Loc};
use crate::esquery::{self, Selector};
use crate::estree::{EsTree, Origin};
use crate::jsdoc::{Finder, Settings};
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/convert-to-jsdoc-comments",
        "Converts non-JSDoc comments preceding or following nodes into JSDoc ones",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ConvertToJsdocComments>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Line,
    Block,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Before,
    After,
    BeforeAndAfter,
}

#[derive(Debug, Clone)]
struct Ctxt {
    context: String,
    inline: bool,
}

#[derive(Debug)]
struct ConvertToJsdocComments {
    allowed_prefixes: Vec<String>,
    contexts: Option<Vec<Ctxt>>,
    contexts_after: Vec<Ctxt>,
    contexts_before_and_after: Vec<Ctxt>,
    enable_fixer: bool,
    single: bool,
    style: Style,
}

fn ctxts(v: &Value) -> Vec<Ctxt> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| match e {
                    Value::String(s) => Some(Ctxt {
                        context: s.clone(),
                        inline: false,
                    }),
                    Value::Object(o) => Some(Ctxt {
                        context: o.get("context").and_then(Value::as_str)?.to_owned(),
                        inline: o
                            .get("inlineCommentBlock")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn strings(names: &[&str]) -> Vec<Ctxt> {
    names
        .iter()
        .map(|s| Ctxt {
            context: (*s).to_owned(),
            inline: false,
        })
        .collect()
}

impl FromOptions for ConvertToJsdocComments {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        let get = |k: &str| o.and_then(|o| o.get(k));
        Ok(ConvertToJsdocComments {
            allowed_prefixes: match get("allowedPrefixes").and_then(Value::as_array) {
                Some(a) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                None => ["@ts-", "istanbul ", "c8 ", "v8 ", "eslint", "prettier-"]
                    .map(str::to_owned)
                    .to_vec(),
            },
            contexts: get("contexts").filter(|v| v.is_array()).map(ctxts),
            contexts_after: get("contextsAfter").map(ctxts).unwrap_or_default(),
            contexts_before_and_after: match get("contextsBeforeAndAfter") {
                Some(v) => ctxts(v),
                None => strings(&[
                    "VariableDeclarator",
                    "TSPropertySignature",
                    "PropertyDefinition",
                ]),
            },
            enable_fixer: get("enableFixer").and_then(Value::as_bool).unwrap_or(true),
            single: get("enforceJsdocLineStyle").and_then(Value::as_str) == Some("single"),
            style: match get("lineOrBlockStyle").and_then(Value::as_str) {
                Some("line") => Style::Line,
                Some("block") => Style::Block,
                _ => Style::Both,
            },
        })
    }
}

/// A comment's value: the text between its delimiters.
fn value<'s>(src: &'s str, c: &ts_r_ast::Comment) -> &'s str {
    let t = c.span.text(src);
    let t = t.get(2..).unwrap_or("");
    match c.kind {
        CommentKind::Block => t.strip_suffix("*/").unwrap_or(t),
        _ => t,
    }
}

fn is_blank(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `getIndent` of the node's line up to the node: its leading spaces and tabs, plus one.
fn indent_at(src: &str, line_start: usize, pos: usize) -> String {
    let head = src.get(line_start..pos).unwrap_or("");
    let ws: String = head
        .chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .collect();
    format!("{ws} ")
}

struct Run<'r, 'a> {
    rule: &'r ConvertToJsdocComments,
    f: Finder<'a>,
    /// `contexts` without the setting fallback's defaults: where `inlineCommentBlock` is
    /// looked up for "before" comments.
    before_ctxts: Vec<Ctxt>,
    reporting_before: bool,
}

impl Run<'_, '_> {
    fn src(&self) -> &str {
        self.f.src
    }

    /// The comment ending right before `pos`, if any.
    fn comment_ending_at(&self, pos: u32) -> Option<usize> {
        let src = self.src();
        let head = src.get(..pos as usize)?;
        let end = head.trim_end_matches(is_blank).len() as u32;
        let cs = &self.f.ast.comments;
        let i = cs.partition_point(|c| c.span.end < end);
        (i < cs.len() && cs[i].span.end == end && cs[i].span.start < end).then_some(i)
    }

    /// `getNonJsdocComment`: the plain comment just before the node.
    fn comment_before(&self, id: NodeId, reduce: bool) -> Option<usize> {
        let node = if reduce { self.f.reduce(id) } else { id };
        let start = self.f.start_of(node);
        let i = self.comment_ending_at(start)?;
        let c = &self.f.ast.comments[i];
        if c.kind == CommentKind::Block && value(self.src(), c).starts_with('*') {
            return None;
        }
        let lines = self.f.lines;
        let d = i64::from(lines.line_of(start)) - i64::from(lines.line_of(c.span.end));
        let s = self.f.settings;
        (d >= s.min_lines && d <= s.max_lines).then_some(i)
    }

    /// `getFollowingComment`: the comment after the node on its last line (past one `;`
    /// or `,`).
    fn comment_after(&self, end: u32) -> Option<usize> {
        let src = self.src();
        let rest = src.get(end as usize..)?;
        let mut off = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if rest[off..].starts_with([';', ',']) {
            off += 1;
            let r = &rest[off..];
            off += r.len() - r.trim_start_matches([' ', '\t']).len();
        }
        let at = end + off as u32;
        let cs = &self.f.ast.comments;
        let i = cs.partition_point(|c| c.span.start < at);
        (i < cs.len() && cs[i].span.start == at).then_some(i)
    }

    fn allowed(&self, v: &str) -> bool {
        let t = v.trim_start_matches(is_blank);
        self.rule
            .allowed_prefixes
            .iter()
            .any(|p| t.starts_with(p.as_str()))
    }

    fn lines_setting(&self) -> i64 {
        let s = self.f.settings;
        if s.min_lines == 0 && s.max_lines >= 1 {
            1
        } else {
            s.min_lines
        }
    }

    /// The node's indent (from its decorator, if it has one) plus one space.
    fn indent_of(&self, id: NodeId, reduce: bool) -> String {
        let node = if reduce { self.f.reduce(id) } else { id };
        let pos = self.f.start_of(node);
        let line = self.f.lines.line_of(pos);
        let line_start = self.f.lines.line_start(line).unwrap_or(0) as usize;
        indent_at(self.src(), line_start, pos as usize)
    }

    fn report(&mut self, cx: &mut Ctx<'_, '_>, comment: usize, fix: Fix) {
        let c = self.f.ast.comments[comment];
        let msg = match c.kind {
            CommentKind::Block => {
                if self.rule.style == Style::Line {
                    return;
                }
                "Block comments should be JSDoc-style."
            }
            _ => {
                if self.rule.style == Style::Block {
                    return;
                }
                "Line comments should be JSDoc-style."
            }
        };
        let line = self.f.lines.line_of(c.span.start);
        let at = self.f.lines.line_start(line).unwrap_or(c.span.start);
        let r = cx.report(Span::new(at, at), msg).loc(Loc::at(line + 1, 1));
        if self.rule.enable_fixer {
            r.fix(fix);
        }
    }

    fn inline_for(list: &[Ctxt], ty: &str) -> bool {
        list.iter()
            .find(|c| c.context == ty)
            .is_some_and(|c| c.inline)
    }

    fn text(&self, inline: bool, v: &str, indent: &str) -> String {
        if inline || self.rule.single {
            format!("/** {} ", v.trim_matches(is_blank))
        } else {
            format!("/**\n{indent}*{}\n{indent}", v.trim_end_matches(is_blank))
        }
    }

    fn check_before(&mut self, cx: &mut Ctx<'_, '_>, id: NodeId, ty: &str, reduce: bool) {
        let Some(i) = self.comment_before(id, reduce) else {
            return;
        };
        let c = self.f.ast.comments[i];
        let v = value(self.src(), &c).to_owned();
        if self.allowed(&v) {
            return;
        }
        self.reporting_before = true;
        let indent = self.indent_of(id, reduce);
        let inline = Self::inline_for(&self.before_ctxts, ty);
        let lines = self.lines_setting();
        let text = format!(
            "{}*/{}",
            self.text(inline, &v, &indent),
            "\n".repeat((if lines == 0 { 1 } else { lines } - 1).clamp(0, 100) as usize)
        );
        self.report(cx, i, Fix::replace(c.span, text));
    }

    fn check_after(&mut self, cx: &mut Ctx<'_, '_>, id: NodeId, ty: &str, end: u32, list: &[Ctxt]) {
        let Some(i) = self.comment_after(end) else {
            return;
        };
        let c = self.f.ast.comments[i];
        let v = value(self.src(), &c).to_owned();
        if v.starts_with('*') || self.allowed(&v) {
            return;
        }
        let indent = self.indent_of(id, true);
        let inline = Self::inline_for(list, ty);
        let lines = self.lines_setting();
        let tail = if lines != 0 {
            let mut it = indent.chars();
            it.next();
            format!("\n{}", it.as_str())
        } else {
            " ".to_owned()
        };
        let text = format!(
            "{}*/{}{tail}",
            self.text(inline, &v, &indent),
            "\n".repeat((if lines == 0 { 1 } else { lines } - 1).clamp(0, 100) as usize)
        );
        let target = match self.f.ast.node(id) {
            Node::VarDeclarator { .. } => self.f.parent(id).unwrap_or(id),
            _ => id,
        };
        let at = self.f.start_of(target);
        self.report(cx, i, Fix::delete(c.span).and(Span::new(at, at), text));
    }
}

impl Rule for ConvertToJsdocComments {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if !crate::jsdoc::settings_load(cx) {
            return;
        }
        let settings = Settings::from_cx(cx);
        let setting_ctxts = settings
            .contexts
            .as_ref()
            .map(|v| ctxts(&Value::Array(v.clone())));
        let before_ctxts = self
            .contexts
            .clone()
            .or_else(|| setting_ctxts.clone())
            .unwrap_or_default();
        // An empty list is still a list (`[]` is truthy).
        let enforced = self
            .contexts
            .clone()
            .or(setting_ctxts)
            .unwrap_or_else(|| strings(crate::jsdoc::DEFAULT_CONTEXTS));
        // The listener keys, a later list replacing an earlier one's key.
        let mut keys: Vec<(String, Check)> = Vec::new();
        for (list, check) in [
            (&enforced, Check::Before),
            (&self.contexts_after, Check::After),
            (&self.contexts_before_and_after, Check::BeforeAndAfter),
        ] {
            let own: Vec<String> = list.iter().map(|c| c.context.clone()).collect();
            keys.retain(|(k, _)| !own.contains(k));
            for k in own {
                // The same key twice in one list runs its check twice.
                keys.push((k, check));
            }
        }
        let mut sels: Vec<(Selector, Check)> = Vec::new();
        for (k, check) in keys {
            if let Ok(s) = esquery::parse(k.strip_suffix(":exit").unwrap_or(&k)) {
                sels.push((s, check));
            }
        }
        sels.sort_by(|a, b| a.0.compare(&b.0));
        let file = cx.file;
        let tree = EsTree::build(file.ast, file.src);
        let prepared: Vec<_> = sels.iter().map(|(s, c)| (s.prepare(), *c)).collect();
        let mut run = Run {
            rule: self,
            f: Finder {
                ast: file.ast,
                src: file.src,
                parents: file.parents,
                lines: file.lines,
                settings: &settings,
            },
            before_ctxts,
            reporting_before: false,
        };
        for n in 0..tree.len() as u32 {
            let node = &tree.nodes[n as usize];
            let reduce = match node.origin {
                Origin::Real => true,
                Origin::Export => false,
                Origin::Synthetic => continue,
            };
            let (id, ty, end) = (node.ast, node.ty, node.span.end);
            for (sel, check) in &prepared {
                if !sel.matches_listener(&tree, n) {
                    continue;
                }
                match check {
                    Check::Before => run.check_before(cx, id, ty, reduce),
                    Check::After => {
                        run.check_after(cx, id, ty, end, &self.contexts_after);
                    }
                    Check::BeforeAndAfter => {
                        run.check_before(cx, id, ty, reduce);
                        if !run.reporting_before {
                            run.check_after(cx, id, ty, end, &self.contexts_before_and_after);
                        }
                    }
                }
            }
        }
    }
}
