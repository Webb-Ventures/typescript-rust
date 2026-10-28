//! `import/no-cycle`: an import whose module leads back to the linted file through the
//! imports on disk.
//!
//! - Type-only import declarations (and side-effect imports, which have no value
//!   specifiers), unresolved modules, non-module files and self-imports are skipped.
//! - The modules on disk reachable from the linted file form a graph (type-only edges
//!   left out); an import only counts when its module is in the linted file's strongly
//!   connected component, and the search only walks modules of that component
//!   (`disableScc` turns this off).
//! - The search is breadth first from the imported module and shares one visited set for
//!   the whole file, so a module walked for one import is not walked again for the next.
//!   A module whose import of the linted file is type-only does not close a cycle.
//! - `maxDepth` bounds the route length; `ignoreExternal` skips external packages (by the
//!   import type of the specifier as resolved from the linted file);
//!   `allowUnsafeDynamicCyclicDependency` skips `import()` and gives up on a module with
//!   any dynamic import of the next one.
//! - The message lists the route: `Dependency cycle via ./b:1=>./c:2`, or
//!   `Dependency cycle detected.` for a direct cycle.

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::sync::Arc;

use super::import_util::{MODULE_KINDS, VisitorOptions, resolve, walk};
use super::prelude::*;
use crate::modules::import_type::{ImportType, import_type, is_module, is_scoped};
use crate::modules::{ExportMap, ModCtx};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-cycle",
        "Forbid a module from importing a module with a dependency path back to itself.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoCycle>,
};

#[derive(Debug)]
struct NoCycle {
    visitor: VisitorOptions,
    max_depth: f64,
    ignore_external: bool,
    allow_dynamic: bool,
    disable_scc: bool,
}

impl FromOptions for NoCycle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoCycle {
            visitor: VisitorOptions::from_options(opts, true, false),
            max_depth: o
                .get("maxDepth")
                .and_then(Value::as_f64)
                .unwrap_or(f64::INFINITY),
            ignore_external: bool_field(o, "ignoreExternal", false),
            allow_dynamic: bool_field(o, "allowUnsafeDynamicCyclicDependency", false),
            disable_scc: bool_field(o, "disableScc", false),
        })
    }
}

/// Strongly connected components of the module graph reachable from one file.
#[derive(Default)]
struct Components {
    of: FxHashMap<Arc<str>, u32>,
}

impl Components {
    fn get(&self, p: &str) -> Option<u32> {
        self.of.get(p).copied()
    }

    /// The graph of value imports reachable from `start`'s map on disk, split by an
    /// iterative Tarjan.
    fn build(m: &ModCtx, start: &str) -> Components {
        let mut nodes: Vec<Arc<str>> = Vec::new();
        let mut index: FxHashMap<Arc<str>, usize> = FxHashMap::default();
        let mut edges: Vec<Vec<usize>> = Vec::new();
        let mut id = |p: &Arc<str>, nodes: &mut Vec<Arc<str>>, edges: &mut Vec<Vec<usize>>| {
            *index.entry(p.clone()).or_insert_with(|| {
                nodes.push(p.clone());
                edges.push(Vec::new());
                nodes.len() - 1
            })
        };
        let mut work: Vec<Arc<ExportMap>> = m.map_for(start).into_iter().collect();
        let mut seen: FxHashSet<Arc<str>> = FxHashSet::default();
        while let Some(map) = work.pop() {
            if !seen.insert(map.path.clone()) {
                continue;
            }
            for (to, decls) in &map.imports {
                if decls.iter().all(|d| d.only_types) {
                    continue;
                }
                let f = id(&map.path, &mut nodes, &mut edges);
                let t = id(to, &mut nodes, &mut edges);
                if !edges[f].contains(&t) {
                    edges[f].push(t);
                }
                if !seen.contains(to)
                    && let Some(next) = m.map_for(to)
                {
                    work.push(next);
                }
            }
        }
        let comp = tarjan(&edges);
        Components {
            of: nodes.into_iter().zip(comp).collect(),
        }
    }
}

/// The component number of every node (iterative Tarjan, no recursion).
fn tarjan(edges: &[Vec<usize>]) -> Vec<u32> {
    const UNSET: usize = usize::MAX;
    let n = edges.len();
    let mut idx = vec![UNSET; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut comp = vec![0u32; n];
    let mut stack = Vec::new();
    let mut next = 0usize;
    let mut count = 0u32;
    for root in 0..n {
        if idx[root] != UNSET {
            continue;
        }
        // (node, position in its edge list)
        let mut call = vec![(root, 0usize)];
        idx[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut pos)) = call.last_mut() {
            if let Some(&w) = edges[v].get(*pos) {
                *pos += 1;
                if idx[w] == UNSET {
                    idx[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(idx[w]);
                }
                continue;
            }
            call.pop();
            if let Some(&(parent, _)) = call.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == idx[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp[w] = count;
                    if w == v {
                        break;
                    }
                }
                count += 1;
            }
        }
    }
    comp
}

impl NoCycle {
    fn ignore_module(&self, cx: &mut Ctx<'_, '_>, name: &str) -> bool {
        if !self.ignore_external {
            return false;
        }
        let resolved = resolve(cx, name);
        let m = cx.file.modules();
        (is_module(name) || is_scoped(name))
            && import_type(m, name, resolved.as_ref().and_then(|r| r.path()))
                == ImportType::External
    }

    fn check(
        &self,
        cx: &mut Ctx<'_, '_>,
        importer: NodeId,
        value: &str,
        scc: Option<&Components>,
        traversed: &mut FxHashSet<Arc<str>>,
    ) {
        if self.ignore_module(cx, value) {
            return;
        }
        let ast = cx.ast();
        match *ast.node(importer) {
            Node::ImportCall { .. } if self.allow_dynamic => return,
            Node::ImportDecl {
                type_only,
                specifiers,
                ..
            } => {
                let all_types = ast.list(specifiers).iter().all(|&s| {
                    matches!(
                        *ast.node(s),
                        Node::ImportSpecifier {
                            type_only: true,
                            ..
                        }
                    )
                });
                if type_only || all_types {
                    return;
                }
            }
            _ => {}
        }
        let Some(imported) = super::import_util::exports_of(cx, value) else {
            return;
        };
        let my_path: Arc<str> = cx.file.modules().file.as_str().into();
        if imported.path == my_path {
            return;
        }
        let mine = scc.map(|s| s.get(&my_path));
        if let Some(s) = scc
            && mine != Some(s.get(&imported.path))
        {
            return;
        }
        let mut queue: VecDeque<(Option<Arc<ExportMap>>, Route)> = VecDeque::new();
        queue.push_back((Some(imported), Vec::new()));
        while let Some((map, route)) = queue.pop_front() {
            let Some(map) = map else {
                continue;
            };
            if !traversed.insert(map.path.clone()) {
                continue;
            }
            match self.walk_module(cx, &map, &route, &my_path, scc, mine, traversed) {
                Step::Cycle => {
                    let message = if route.is_empty() {
                        "Dependency cycle detected.".to_owned()
                    } else {
                        let parts: Vec<String> =
                            route.iter().map(|(v, l)| format!("{v}:{l}")).collect();
                        format!("Dependency cycle via {}", parts.join("=>"))
                    };
                    let span = cx.span(importer);
                    cx.report(span, message);
                    return;
                }
                Step::Next(more) => {
                    let m = cx.file.modules();
                    for (p, r) in more {
                        queue.push_back((m.map_for(&p), r));
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_module(
        &self,
        cx: &mut Ctx<'_, '_>,
        map: &ExportMap,
        route: &[(String, u32)],
        my_path: &Arc<str>,
        scc: Option<&Components>,
        mine: Option<Option<u32>>,
        traversed: &FxHashSet<Arc<str>>,
    ) -> Step {
        let mut more = Vec::new();
        for (p, decls) in &map.imports {
            if let Some(s) = scc
                && mine != Some(s.get(p))
            {
                continue;
            }
            if traversed.contains(p) {
                continue;
            }
            let mut to_traverse = Vec::new();
            for d in decls {
                if !d.only_types && !self.ignore_module(cx, &d.source) {
                    to_traverse.push(d);
                }
            }
            if self.allow_dynamic && to_traverse.iter().any(|d| d.dynamic) {
                return Step::Next(Vec::new());
            }
            if p == my_path && !to_traverse.is_empty() {
                return Step::Cycle;
            }
            if ((route.len() + 1) as f64) < self.max_depth {
                for d in to_traverse {
                    let mut r = route.to_vec();
                    r.push((d.source.clone(), d.line));
                    more.push((p.clone(), r));
                }
            }
        }
        Step::Next(more)
    }
}

/// The specifiers walked so far, with their lines.
type Route = Vec<(String, u32)>;

enum Step {
    Cycle,
    Next(Vec<(Arc<str>, Route)>),
}

impl Rule for NoCycle {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut found = Vec::new();
        walk(ast, |id| {
            if MODULE_KINDS.contains(&ast.kind(id)) {
                for (r, v) in self.visitor.refs(ast, id) {
                    found.push((r.importer, v));
                }
            }
        });
        if found.is_empty() {
            return;
        }
        let scc = (!self.disable_scc).then(|| {
            let m = cx.file.modules();
            Components::build(m, &m.file)
        });
        let mut traversed = FxHashSet::default();
        for (importer, value) in found {
            self.check(cx, importer, value, scc.as_ref(), &mut traversed);
        }
    }
}
