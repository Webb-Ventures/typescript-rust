//! `no-useless-assignment`: disallow variable assignments when the value is not used.
//!
//! Backward liveness over the code-path graph of each function (or program / static
//! block): a write is reported when no read of the variable is reachable from it before
//! the next write. Variables read from another function are skipped (a closure may run at
//! any time), as are exported variables, globals and unreachable writes, and (as ESLint does)
//! writes inside the block of a `try` statement of the same code path.

use super::prelude::*;
use crate::cfg::Cfg;
use crate::semantic::Symbol;
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-assignment",
        "Disallow variable assignments when the value is not used",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUselessAssignment>,
};

#[derive(Debug, Default)]
struct NoUselessAssignment;

/// The function, static block, class field initializer or program that runs `node`.
fn container(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut prev = node;
    let mut cur = Some(node);
    while let Some(c) = cur {
        match *cx.node(c) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::StaticBlock { .. }
            | Node::Program { .. } => return Some(c),
            Node::PropertyDef { value: Some(v), .. } if v == prev && c != node => {
                return Some(c);
            }
            _ => {}
        }
        prev = c;
        cur = cx.parent(c);
    }
    None
}

fn is_read(flags: RefFlags) -> bool {
    flags.intersects(RefFlags::READ | RefFlags::JSX) && !flags.contains(RefFlags::TYPE)
}

/// The binding is the target of a `for (... in/of ...)` head.
fn is_for_head(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut prev = node;
    let mut cur = cx.parent(node);
    while let Some(p) = cur {
        match *cx.node(p) {
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == prev,
            Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::BindingProperty { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. }
            | Node::VarDeclarator { .. }
            | Node::VarDecl { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::Property { .. }
            | Node::ShorthandProp { .. }
            | Node::Spread { .. }
            | Node::Paren { .. } => {}
            _ => return false,
        }
        prev = p;
        cur = cx.parent(p);
    }
    false
}

/// The write sits in the block of a `try` statement of its own code path (observed: ESLint
/// never reports those, whatever the `catch` or `finally` do).
fn in_try_block(cx: &Ctx<'_, '_>, node: NodeId, home: NodeId) -> bool {
    let mut prev = node;
    let mut cur = cx.parent(node);
    while let Some(p) = cur {
        if p == home {
            return false;
        }
        match *cx.node(p) {
            Node::Try { block, .. } if block == prev => return true,
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::StaticBlock { .. } => return false,
            _ => {}
        }
        prev = p;
        cur = cx.parent(p);
    }
    false
}

fn declared(cx: &Ctx<'_, '_>, s: &Symbol) -> bool {
    !s.decls.is_empty()
        && s.decls.iter().all(|d| {
            matches!(
                d.kind,
                DeclKind::Var
                    | DeclKind::Let
                    | DeclKind::Const
                    | DeclKind::Function
                    | DeclKind::Class
                    | DeclKind::Param
                    | DeclKind::CatchParam
            ) && !matches!(*cx.node(d.parent), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::DECLARE))
        })
}

impl Rule for NoUselessAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if cx.ast().source_kind == ts_r_ast::SourceKind::Dts {
            return;
        }
        let sem = cx.semantic();
        // Tracked symbols grouped by the container that declares them.
        let mut by_container: FxHashMap<NodeId, Vec<SymbolId>> = FxHashMap::default();
        for (i, s) in sem.symbols().iter().enumerate() {
            if s.flags.intersects(
                SymbolFlags::FN_EXPR_NAME
                    | SymbolFlags::CLASS_EXPR_NAME
                    | SymbolFlags::ARGUMENTS
                    | SymbolFlags::COMMENT_GLOBAL
                    | SymbolFlags::EXPORTED
                    | SymbolFlags::GLOBAL_AUGMENT,
            ) || !declared(cx, s)
            {
                continue;
            }
            let scope = sem.scope(s.scope);
            if matches!(scope.kind, ScopeKind::Global | ScopeKind::Module)
                && cx
                    .file
                    .exported
                    .iter()
                    .any(|e| *e == sem.name(SymbolId(i as u32)))
            {
                continue;
            }
            let Some(home) = container(cx, scope.node) else {
                continue;
            };
            let mut reads = 0;
            let mut writes = 0;
            let mut escapes = false;
            for &r in &s.refs {
                let r = sem.reference(r);
                if r.flags.contains(RefFlags::EXPORT) {
                    escapes = true;
                    break;
                }
                let read = is_read(r.flags);
                let inside = container(cx, r.node) == Some(home);
                if read {
                    reads += 1;
                    if !inside {
                        escapes = true;
                        break;
                    }
                }
                if r.flags.contains(RefFlags::WRITE) && inside {
                    writes += 1;
                }
            }
            if escapes || reads == 0 || writes == 0 {
                continue;
            }
            by_container
                .entry(home)
                .or_default()
                .push(SymbolId(i as u32));
        }
        let mut found: Vec<NodeId> = Vec::new();
        let ast = cx.ast();
        let mut homes: Vec<_> = by_container.into_iter().collect();
        homes.sort_by_key(|(h, _)| *h);
        for (home, syms) in homes {
            let cfg = match *ast.node(home) {
                Node::Program { stmts } => Cfg::statements(ast, ast.list(stmts)),
                Node::StaticBlock { body } => Cfg::statements(ast, ast.list(body)),
                _ => Cfg::function(ast, home),
            };
            // Event node -> (symbol index, flags, report candidate).
            let mut events: FxHashMap<NodeId, (usize, RefFlags, bool)> = FxHashMap::default();
            for (k, &sid) in syms.iter().enumerate() {
                let s = sem.symbol(sid);
                let skip_init = s
                    .decls
                    .iter()
                    .any(|d| matches!(d.kind, DeclKind::Param | DeclKind::CatchParam));
                for &r in &s.refs {
                    let r = sem.reference(r);
                    let write = r.flags.contains(RefFlags::WRITE);
                    if !write && !is_read(r.flags) {
                        continue;
                    }
                    let skipped = (skip_init && r.flags.contains(RefFlags::INIT))
                        || is_for_head(cx, r.node)
                        || in_try_block(cx, r.node, home);
                    let candidate = write && !skipped;
                    events.insert(r.node, (k, r.flags, candidate));
                }
            }
            for w in dead_writes(&cfg, syms.len(), &events) {
                found.push(w);
            }
        }
        found.sort_by_key(|&n| cx.span(n).start);
        for n in found {
            let msg = format!(
                "The value assigned to '{}' is not used in subsequent statements.",
                cx.text(n)
            );
            let r = cx.report_node(n, msg).id("unnecessaryAssignment");
            let _ = r;
        }
    }
}

/// Writes whose value no reachable read observes.
fn dead_writes(
    cfg: &Cfg,
    n: usize,
    events: &FxHashMap<NodeId, (usize, RefFlags, bool)>,
) -> Vec<NodeId> {
    let words = n.div_ceil(64);
    let nb = cfg.blocks.len();
    let mut live_in = vec![vec![0u64; words]; nb];
    let transfer = |live: &mut [u64], e: NodeId| {
        if let Some(&(k, flags, _)) = events.get(&e) {
            let bit = 1u64 << (k % 64);
            if flags.contains(RefFlags::WRITE) {
                live[k / 64] &= !bit;
            }
            if is_read(flags) {
                live[k / 64] |= bit;
            }
        }
    };
    let live_out = |live_in: &[Vec<u64>], b: usize| {
        let mut out = vec![0u64; words];
        for &t in &cfg.blocks[b].succ {
            for (o, w) in out.iter_mut().zip(&live_in[t as usize]) {
                *o |= w;
            }
        }
        out
    };
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut live = live_out(&live_in, b);
            for &e in cfg.blocks[b].events.iter().rev() {
                transfer(&mut live, e);
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
    }
    // Write node -> whether any reachable occurrence is live.
    let mut writes: FxHashMap<NodeId, bool> = FxHashMap::default();
    for b in 0..nb {
        if !cfg.is_reachable(b as u32) {
            continue;
        }
        let mut live = live_out(&live_in, b);
        for &e in cfg.blocks[b].events.iter().rev() {
            if let Some(&(k, _, true)) = events.get(&e) {
                let is_live = live[k / 64] & (1u64 << (k % 64)) != 0;
                *writes.entry(e).or_insert(false) |= is_live;
            }
            transfer(&mut live, e);
        }
    }
    writes
        .into_iter()
        .filter(|&(_, live)| !live)
        .map(|(w, _)| w)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::engine::lint_source;
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};

    /// The 1-based lines of the rule's reports.
    fn lines(code: &str) -> Vec<usize> {
        let cfg = serde_json::json!([{
            "files": ["**/*.ts"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
            "rules": { "no-useless-assignment": "error" },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        lint_source("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("no-useless-assignment"))
            .map(|d| code[..d.span.start as usize].matches('\n').count() + 1)
            .collect()
    }

    #[test]
    fn writes_inside_a_try_block_are_skipped() {
        // Observed from ESLint 10: only the declaration outside the inner try (line 2) is
        // reported; writes in any try block of the function never are.
        let code = "export async function f(run: () => Promise<number>) {
    let raw: number | undefined = undefined
    try {
        raw = await run()
    } catch (error) {
        throw error
    }
    return raw
}
export async function g(run: () => Promise<number>) {
    try {
        let raw: number | undefined = undefined
        try {
            raw = await run()
        } catch (error) {
            throw error
        }
        return raw
    } finally {
        await run()
    }
}
";
        assert_eq!(lines(code), vec![2]);
    }

    #[test]
    fn decorator_arguments_read_variables() {
        let code = "const trim = { to: (v: string) => v };
class A {
    @Column({ transformer: [trim] })
    name = '';
}
export { A };
declare function Column(o: object): PropertyDecorator;
";
        assert_eq!(lines(code), Vec::<usize>::new());
    }
}
