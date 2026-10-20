//! Deterministic text dumps of a [`BoundFile`] for snapshot tests and debugging.

use std::fmt::Write;

use ts_r_ast::{Ast, NodeId};
use ts_r_span::LineIndex;

use crate::flow::{FlowId, FlowKind};
use crate::symbols::{SymbolId, TableId};
use crate::{AliasTarget, BoundFile, ImportName};

struct Ctx<'a> {
    b: &'a BoundFile,
    ast: &'a Ast,
    lines: LineIndex,
}

impl Ctx<'_> {
    fn pos(&self, node: NodeId) -> String {
        let lc = self.lines.line_col(self.ast.span(node).start);
        format!("{}:{}", lc.line + 1, lc.col + 1)
    }

    fn pos_off(&self, off: u32) -> String {
        let lc = self.lines.line_col(off);
        format!("{}:{}", lc.line + 1, lc.col + 1)
    }

    fn table(&self, t: TableId) -> String {
        let mut entries: Vec<(&str, SymbolId)> = self
            .b
            .table(t)
            .iter()
            .map(|&(n, s)| (self.b.name_text(self.ast, n), s))
            .collect();
        entries.sort();
        let parts: Vec<String> = entries
            .iter()
            .map(|(n, s)| format!("{n}=#{}", s.0))
            .collect();
        format!("{{{}}}", parts.join(", "))
    }
}

pub(crate) fn dump(b: &BoundFile, ast: &Ast, src: &str) -> String {
    let c = Ctx {
        b,
        ast,
        lines: LineIndex::new(src),
    };
    let mut out = String::new();
    let _ = writeln!(out, "{}", if b.is_module { "module" } else { "script" });
    out.push_str("symbols:\n");
    for (i, s) in b.symbols.iter().enumerate() {
        let id = SymbolId(i as u32);
        let decls: Vec<String> = b.decls(id).iter().map(|&d| c.pos(d)).collect();
        let _ = write!(out, "  #{i} {} {:?}", b.name_text(ast, s.name), s.flags);
        if !s.facts.is_empty() {
            let _ = write!(out, " ({:?})", s.facts);
        }
        let _ = write!(out, " @[{}]", decls.join(" "));
        if let Some(p) = s.parent {
            let _ = write!(out, " parent=#{}", p.0);
        }
        out.push('\n');
        for t in b.member_tables(id) {
            if !b.table(t).is_empty() {
                let _ = writeln!(out, "      members {}", c.table(t));
            }
        }
        if !s.exports.is_none() && !b.table(s.exports).is_empty() {
            let _ = writeln!(out, "      exports {}", c.table(s.exports));
        }
        for a in b.aliases_of(id) {
            let target = match a.target {
                AliasTarget::Module { request, import } => {
                    let spec = b
                        .module_requests
                        .get(request as usize)
                        .map_or("?", |r| ast.atom(r.specifier));
                    let what = match import {
                        ImportName::Default => "default".to_owned(),
                        ImportName::Named(n) => b.name_text(ast, n).to_owned(),
                        ImportName::Namespace => "*".to_owned(),
                        ImportName::ExportEquals => "export=".to_owned(),
                    };
                    format!("{what} from {spec:?}")
                }
                AliasTarget::Entity(n) => format!("entity @{}", c.pos(n)),
            };
            let _ = writeln!(out, "      alias -> {target}");
        }
    }
    out.push_str("scopes:\n");
    for (i, s) in b.scopes.iter().enumerate() {
        let _ = write!(out, "  s{i} {:?} @{}", s.kind, c.pos(s.node));
        if let Some(p) = s.parent {
            let _ = write!(out, " in s{}", p.0);
        }
        if let Some(sym) = s.symbol {
            let _ = write!(out, " sym=#{}", sym.0);
        }
        if !b.table(s.locals).is_empty() {
            let _ = write!(out, " {}", c.table(s.locals));
        }
        out.push('\n');
    }
    for (label, t) in [
        ("ambient modules", b.ambient_modules),
        ("module augmentations", b.module_augmentations),
        ("global augmentations", b.global_augmentations),
    ] {
        if !b.table(t).is_empty() {
            let _ = writeln!(out, "{label}: {}", c.table(t));
        }
    }
    if !b.module_requests.is_empty() {
        out.push_str("requests:\n");
        for r in &b.module_requests {
            let _ = writeln!(
                out,
                "  {:?} {:?}{} @{}",
                r.kind,
                ast.atom(r.specifier),
                if r.type_only { " type" } else { "" },
                c.pos(r.node)
            );
        }
    }
    for s in &b.export_stars {
        let _ = writeln!(
            out,
            "export * request={} container=#{}",
            s.request, s.container.0
        );
    }
    if let Some((n, _)) = b.umd_name {
        let _ = writeln!(out, "umd: {}", b.name_text(ast, n));
    }
    if !b.refs.resolved.is_empty() {
        out.push_str("refs:");
        for &(n, s) in &b.refs.resolved {
            let _ = write!(
                out,
                " {}={}#{}",
                ast.name_of(n).unwrap_or("?"),
                c.pos(n),
                s.0
            );
        }
        out.push('\n');
    }
    if !b.refs.free.is_empty() {
        out.push_str("free:");
        for f in &b.refs.free {
            let _ = write!(out, " {}@{}", b.name_text(ast, f.name), c.pos(f.node));
        }
        out.push('\n');
    }
    if !b.diagnostics.is_empty() {
        out.push_str("diagnostics:\n");
        for d in &b.diagnostics {
            let _ = writeln!(
                out,
                "  {} TS{}: {}",
                c.pos_off(d.span.start),
                d.code,
                d.message
            );
        }
    }
    out
}

pub(crate) fn dump_flow(b: &BoundFile, ast: &Ast, src: &str) -> String {
    let c = Ctx {
        b,
        ast,
        lines: LineIndex::new(src),
    };
    let g = &b.flow;
    let mut out = String::new();
    for (i, n) in g.nodes.iter().enumerate().skip(1) {
        let id = FlowId(i as u32);
        let _ = write!(out, "f{i} {:?}", n.kind);
        if let Some(node) = n.node {
            let span = ast.span(node);
            let text = src
                .get(span.start as usize..span.end as usize)
                .unwrap_or("");
            let text: String = text.chars().take(24).collect();
            let _ = write!(out, " {}{:?}", c.pos(node), text.replace('\n', " "));
        }
        if n.flags != 0 {
            let _ = write!(out, " flags={}", n.flags);
        }
        match n.kind {
            FlowKind::BranchLabel | FlowKind::LoopLabel => {
                let a: Vec<String> = g
                    .antecedents(id)
                    .iter()
                    .map(|f| format!("f{}", f.0))
                    .collect();
                let _ = write!(out, " <- [{}]", a.join(", "));
            }
            FlowKind::ReduceLabel => {
                if let Some((target, allowed)) = g.reduce_target(id) {
                    let a: Vec<String> = allowed.iter().map(|f| format!("f{}", f.0)).collect();
                    let _ = write!(out, " <- f{} (f{} = [{}])", n.a, target.0, a.join(", "));
                }
            }
            FlowKind::SwitchClause => {
                let _ = write!(out, " <- f{} clauses {}..{}", n.a, n.b & 0xFFFF, n.b >> 16);
            }
            FlowKind::Start => {
                if n.a != 0 {
                    let _ = write!(out, " created at f{}", n.a);
                }
            }
            FlowKind::Unreachable => {}
            _ => {
                let _ = write!(out, " <- f{}", n.a);
            }
        }
        out.push('\n');
    }
    for f in &g.functions {
        let _ = writeln!(
            out,
            "fn @{}: start f{} end f{} returns f{}",
            c.pos(f.function),
            f.start.0,
            f.end.0,
            f.returns.0
        );
    }
    out
}
