//! The module graph: which served modules import which, who accepts hot updates, and how
//! an update propagates to HMR boundaries.
//!
//! Nodes are created lazily as the browser requests modules (unbundled ESM: the graph is
//! exactly what the page loaded). Ids are `u32` indices into one `Vec`; nodes are never
//! removed, so ids stay valid for the life of the server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Index of a module in the [`ModuleGraph`].
pub type ModuleId = u32;

/// How a module is served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    /// JS produced by a [`crate::Transform`].
    Script,
    /// A stylesheet. Imported from JS it is served as a self-accepting JS module; linked
    /// from HTML it is served as CSS and updated with `css-update`.
    Css,
    /// JSON, `?raw`/`?url`/asset imports served as JS (never self-accepting).
    Asset,
}

/// One module in the graph.
#[derive(Debug)]
pub struct ModuleNode {
    /// Root-relative URL without query (`/src/main.ts`) or `/@fs/<abs>`.
    pub url: String,
    pub file: PathBuf,
    pub kind: ModuleKind,
    /// Modules that import this one, sorted by id, no duplicates.
    pub importers: Vec<ModuleId>,
    /// Modules this one imports, sorted by id, no duplicates.
    pub importees: Vec<ModuleId>,
    /// Importees this module accepts updates for (`import.meta.hot.accept("./dep")`).
    pub accepted_deps: Vec<ModuleId>,
    /// `import.meta.hot.accept()` with no deps (or a CSS module served as JS).
    pub self_accepting: bool,
    /// `import.meta.hot.decline()`: any update reaching it reloads the page.
    pub declined: bool,
    /// Timestamp of the last hot update that invalidated this module (0 = never). Importers
    /// add `?t=<ts>` to its URL so the browser fetches the new instance.
    pub last_hmr_timestamp: u64,
    /// Served to a `<link rel=stylesheet>` (CSS only).
    pub linked_css: bool,
    /// Served as a JS module at least once.
    pub served_as_js: bool,
    /// Prepared (transformed + resolved) output, owned by the server.
    pub prepared: Option<Arc<crate::server::Prepared>>,
    /// The last prepare failed (transform or resolve error).
    pub errored: bool,
    /// Lost its last importer and wasn't imported or requested since: the page no longer
    /// uses it, so changes to its file are ignored (a late watcher event after the import
    /// was removed must not reload the page).
    pub pruned: bool,
    /// Files outside the graph its transform read ([`crate::Transform::deps`]).
    pub watch_deps: Vec<PathBuf>,
}

/// A module that accepted an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boundary {
    /// The accepting module.
    pub boundary: ModuleId,
    /// The module the browser must re-import (== `boundary` for a self-accepting module).
    pub accepted_via: ModuleId,
}

/// Why an update can't be applied in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FullReload {
    /// Propagation reached a module nobody imports without finding an accepting module.
    NoBoundary { root: ModuleId },
    /// Propagation reached a module that called `import.meta.hot.decline()`.
    Declined { module: ModuleId },
}

/// Result of [`ModuleGraph::propagate`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Propagation {
    /// Accepting modules, in discovery order, deduplicated.
    pub boundaries: Vec<Boundary>,
    /// Every module that must be re-executed (on some path from the change to a boundary),
    /// including the changed module, excluding boundaries that only accept a dependency.
    pub invalidated: Vec<ModuleId>,
    /// Some boundary sits on an import cycle; the client reloads if re-importing fails.
    pub within_circular: bool,
    pub full_reload: Option<FullReload>,
}

#[derive(Debug, Default)]
pub struct ModuleGraph {
    nodes: Vec<ModuleNode>,
    by_file: HashMap<PathBuf, ModuleId>,
    by_url: HashMap<String, ModuleId>,
    /// File -> modules whose transform read it (reverse of `watch_deps`).
    by_dep: HashMap<PathBuf, Vec<ModuleId>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    New,
    OnStack,
    Done,
}

impl ModuleGraph {
    pub fn new() -> ModuleGraph {
        ModuleGraph::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: ModuleId) -> &ModuleNode {
        &self.nodes[id as usize]
    }

    pub fn node_mut(&mut self, id: ModuleId) -> &mut ModuleNode {
        &mut self.nodes[id as usize]
    }

    pub fn by_file(&self, file: &Path) -> Option<ModuleId> {
        self.by_file.get(file).copied()
    }

    pub fn by_url(&self, url: &str) -> Option<ModuleId> {
        self.by_url.get(url).copied()
    }

    pub fn ids(&self) -> impl Iterator<Item = ModuleId> + '_ {
        (0..self.nodes.len() as u32).map(|i| i as ModuleId)
    }

    /// Returns the node for `file`, creating it with `url` and `kind` if new.
    pub fn ensure(&mut self, file: &Path, url: &str, kind: ModuleKind) -> ModuleId {
        if let Some(&id) = self.by_file.get(file) {
            return id;
        }
        let id = self.nodes.len() as ModuleId;
        self.nodes.push(ModuleNode {
            url: url.to_owned(),
            file: file.to_path_buf(),
            kind,
            importers: Vec::new(),
            importees: Vec::new(),
            accepted_deps: Vec::new(),
            self_accepting: kind == ModuleKind::Css,
            declined: false,
            last_hmr_timestamp: 0,
            linked_css: false,
            served_as_js: false,
            prepared: None,
            errored: false,
            pruned: false,
            watch_deps: Vec::new(),
        });
        self.by_file.insert(file.to_path_buf(), id);
        self.by_url.insert(url.to_owned(), id);
        id
    }

    /// Replaces the files `id`'s transform read.
    pub fn set_watch_deps(&mut self, id: ModuleId, deps: Vec<PathBuf>) {
        if self.nodes[id as usize].watch_deps == deps {
            return;
        }
        for d in std::mem::take(&mut self.nodes[id as usize].watch_deps) {
            if let Some(v) = self.by_dep.get_mut(&d) {
                v.retain(|&m| m != id);
                if v.is_empty() {
                    self.by_dep.remove(&d);
                }
            }
        }
        for d in &deps {
            let v = self.by_dep.entry(d.clone()).or_default();
            if !v.contains(&id) {
                v.push(id);
            }
        }
        self.nodes[id as usize].watch_deps = deps;
    }

    /// Modules whose transform read `file`, in id order.
    pub fn dependents(&self, file: &Path) -> Vec<ModuleId> {
        let mut v = self.by_dep.get(file).cloned().unwrap_or_default();
        v.sort_unstable();
        v
    }

    /// Replaces `id`'s outgoing edges and hot-accept info. Returns importees that lost
    /// their last importer (their `prune` callbacks should run in the browser).
    pub fn set_imports(
        &mut self,
        id: ModuleId,
        mut importees: Vec<ModuleId>,
        mut accepted_deps: Vec<ModuleId>,
        self_accepting: bool,
        declined: bool,
    ) -> Vec<ModuleId> {
        importees.sort_unstable();
        importees.dedup();
        importees.retain(|&m| m != id);
        accepted_deps.sort_unstable();
        accepted_deps.dedup();
        let old = std::mem::take(&mut self.nodes[id as usize].importees);
        let mut pruned = Vec::new();
        for &gone in old.iter().filter(|m| importees.binary_search(m).is_err()) {
            let n = &mut self.nodes[gone as usize];
            if let Ok(pos) = n.importers.binary_search(&id) {
                n.importers.remove(pos);
            }
            if n.importers.is_empty() && !n.linked_css {
                n.pruned = true;
                pruned.push(gone);
            }
        }
        for &new in importees.iter().filter(|m| old.binary_search(m).is_err()) {
            let n = &mut self.nodes[new as usize];
            if let Err(pos) = n.importers.binary_search(&id) {
                n.importers.insert(pos, id);
            }
            n.pruned = false;
        }
        let node = &mut self.nodes[id as usize];
        node.importees = importees;
        node.accepted_deps = accepted_deps;
        node.self_accepting = self_accepting || node.kind == ModuleKind::Css;
        node.declined = declined;
        pruned
    }

    /// Finds the HMR boundaries for a change to `changed`: walks up importers, stopping at
    /// modules that accept the update (themselves, or the importee the walk came from).
    /// Reaching a module with no importers, or a declining module, means a full reload.
    ///
    /// `skip_self` ignores `changed`'s own self-acceptance (used for
    /// `import.meta.hot.invalidate()`, which hands the update to the importers).
    ///
    /// Iterative DFS; each module is expanded once, so the cost is bounded by the part of
    /// the graph above the change, not by the project size.
    pub fn propagate(&self, changed: ModuleId, skip_self: bool) -> Propagation {
        let mut out = Propagation::default();
        let start = self.node(changed);
        if start.declined {
            out.full_reload = Some(FullReload::Declined { module: changed });
            return out;
        }
        if start.self_accepting && !skip_self {
            out.boundaries.push(Boundary {
                boundary: changed,
                accepted_via: changed,
            });
            out.invalidated.push(changed);
            return out;
        }
        if start.importers.is_empty() {
            out.full_reload = Some(FullReload::NoBoundary { root: changed });
            return out;
        }

        let mut visit: HashMap<ModuleId, Visit> = HashMap::new();
        let mut invalidated: HashMap<ModuleId, ()> = HashMap::new();
        // (node, index of the next importer to look at)
        let mut stack: Vec<(ModuleId, usize)> = vec![(changed, 0)];
        visit.insert(changed, Visit::OnStack);

        // Marks every module on the current DFS path as invalidated. If the top is already
        // marked, everything below it is too (its path prefix hasn't changed since).
        fn mark_chain(
            stack: &[(ModuleId, usize)],
            extra: Option<ModuleId>,
            invalidated: &mut HashMap<ModuleId, ()>,
            out: &mut Vec<ModuleId>,
        ) {
            if let Some(e) = extra
                && invalidated.insert(e, ()).is_none()
            {
                out.push(e);
            }
            for &(m, _) in stack.iter().rev() {
                if invalidated.insert(m, ()).is_some() {
                    break;
                }
                out.push(m);
            }
        }

        while let Some(top) = stack.last_mut() {
            let (node, idx) = *top;
            let importers = &self.node(node).importers;
            if idx >= importers.len() {
                visit.insert(node, Visit::Done);
                stack.pop();
                continue;
            }
            top.1 += 1;
            let imp = importers[idx];
            let imp_node = self.node(imp);
            if imp_node.accepted_deps.binary_search(&node).is_ok() {
                push_boundary(&mut out.boundaries, imp, node);
                mark_chain(&stack, None, &mut invalidated, &mut out.invalidated);
                continue;
            }
            match visit.get(&imp).copied().unwrap_or(Visit::New) {
                Visit::OnStack => {
                    out.within_circular = true;
                    mark_chain(&stack, None, &mut invalidated, &mut out.invalidated);
                }
                Visit::Done => {
                    mark_chain(&stack, None, &mut invalidated, &mut out.invalidated);
                }
                Visit::New => {
                    if imp_node.declined {
                        out.full_reload = Some(FullReload::Declined { module: imp });
                        return out;
                    }
                    if imp_node.self_accepting {
                        push_boundary(&mut out.boundaries, imp, imp);
                        mark_chain(&stack, Some(imp), &mut invalidated, &mut out.invalidated);
                        visit.insert(imp, Visit::Done);
                        continue;
                    }
                    if imp_node.importers.is_empty() {
                        out.full_reload = Some(FullReload::NoBoundary { root: imp });
                        return out;
                    }
                    visit.insert(imp, Visit::OnStack);
                    stack.push((imp, 0));
                }
            }
        }
        out
    }

    /// Stamps every invalidated module with the update timestamp so importers re-fetch it.
    pub fn apply_timestamp(&mut self, p: &Propagation, timestamp: u64) {
        for &m in &p.invalidated {
            self.nodes[m as usize].last_hmr_timestamp = timestamp;
        }
    }
}

fn push_boundary(list: &mut Vec<Boundary>, boundary: ModuleId, accepted_via: ModuleId) {
    let b = Boundary {
        boundary,
        accepted_via,
    };
    if !list.contains(&b) {
        list.push(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a graph from `name -> [imports]` lines; names ending in `!` self-accept,
    /// `name accepts dep` lines add dep acceptance, `name declines` marks decline.
    struct G {
        g: ModuleGraph,
    }

    impl G {
        fn new(edges: &[(&str, &[&str])]) -> G {
            let mut g = ModuleGraph::new();
            for (from, tos) in edges {
                let f = Self::id(&mut g, from);
                let ids: Vec<_> = tos.iter().map(|t| Self::id(&mut g, t)).collect();
                g.set_imports(f, ids, vec![], false, false);
            }
            G { g }
        }

        fn id(g: &mut ModuleGraph, name: &str) -> ModuleId {
            g.ensure(Path::new(name), &format!("/{name}"), ModuleKind::Script)
        }

        fn get(&self, name: &str) -> ModuleId {
            self.g.by_url(&format!("/{name}")).unwrap()
        }

        fn self_accept(mut self, name: &str) -> G {
            let id = self.get(name);
            self.g.node_mut(id).self_accepting = true;
            self
        }

        fn accept_dep(mut self, name: &str, dep: &str) -> G {
            let (id, d) = (self.get(name), self.get(dep));
            self.g.node_mut(id).accepted_deps.push(d);
            self.g.node_mut(id).accepted_deps.sort_unstable();
            self
        }

        fn decline(mut self, name: &str) -> G {
            let id = self.get(name);
            self.g.node_mut(id).declined = true;
            self
        }

        fn names(&self, ids: &[ModuleId]) -> Vec<String> {
            let mut v: Vec<_> = ids
                .iter()
                .map(|&i| self.g.node(i).url[1..].to_owned())
                .collect();
            v.sort();
            v
        }

        fn run(&self, changed: &str) -> (Vec<(String, String)>, Vec<String>, Propagation) {
            let p = self.g.propagate(self.get(changed), false);
            let b = p
                .boundaries
                .iter()
                .map(|b| {
                    (
                        self.g.node(b.boundary).url[1..].to_owned(),
                        self.g.node(b.accepted_via).url[1..].to_owned(),
                    )
                })
                .collect();
            let inv = self.names(&p.invalidated);
            (b, inv, p)
        }
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_owned(), b.to_owned())
    }

    #[test]
    fn watch_deps_index_the_modules_that_read_a_file() {
        let mut g = G::new(&[("a", &[]), ("b", &[])]);
        let (a, b) = (g.get("a"), g.get("b"));
        let t = PathBuf::from("types.ts");
        let u = PathBuf::from("user.ts");
        g.g.set_watch_deps(b, vec![t.clone()]);
        g.g.set_watch_deps(a, vec![t.clone(), u.clone()]);
        assert_eq!(g.g.dependents(&t), [a, b]);
        assert_eq!(g.g.dependents(&u), [a]);
        // Replacing a module's deps drops its old entries.
        g.g.set_watch_deps(a, vec![u.clone()]);
        assert_eq!(g.g.dependents(&t), [b]);
        g.g.set_watch_deps(b, Vec::new());
        assert!(g.g.dependents(&t).is_empty());
        assert!(g.g.dependents(Path::new("other.ts")).is_empty());
        assert_eq!(g.g.node(a).watch_deps, [u]);
    }

    #[test]
    fn self_accepting_module_is_its_own_boundary() {
        let g = G::new(&[("main", &["app"]), ("app", &["button"])]).self_accept("button");
        let (b, inv, p) = g.run("button");
        assert_eq!(b, vec![pair("button", "button")]);
        assert_eq!(inv, ["button"]);
        assert_eq!(p.full_reload, None);
    }

    #[test]
    fn propagates_to_self_accepting_importer() {
        let g = G::new(&[("main", &["app"]), ("app", &["util"]), ("util", &["leaf"])])
            .self_accept("app");
        let (b, inv, _) = g.run("leaf");
        assert_eq!(b, vec![pair("app", "app")]);
        assert_eq!(inv, ["app", "leaf", "util"]);
    }

    #[test]
    fn dep_accepting_importer_stops_propagation() {
        let g = G::new(&[("main", &["app"]), ("app", &["dep"]), ("dep", &["leaf"])])
            .accept_dep("app", "dep");
        let (b, inv, _) = g.run("leaf");
        assert_eq!(b, vec![pair("app", "dep")]);
        // The accepting importer is not re-executed; only the path up to the dep is.
        assert_eq!(inv, ["dep", "leaf"]);
    }

    #[test]
    fn reaching_the_root_without_boundary_is_full_reload() {
        let g = G::new(&[("main", &["app"]), ("app", &["leaf"])]);
        let (_, _, p) = g.run("leaf");
        assert_eq!(
            p.full_reload,
            Some(FullReload::NoBoundary {
                root: g.get("main")
            })
        );
        let (_, _, p) = g.run("main");
        assert_eq!(
            p.full_reload,
            Some(FullReload::NoBoundary {
                root: g.get("main")
            })
        );
    }

    #[test]
    fn one_unaccepted_path_forces_full_reload() {
        // leaf is imported by an accepting module and by main directly.
        let g = G::new(&[("main", &["app", "leaf"]), ("app", &["leaf"])]).self_accept("app");
        let (_, _, p) = g.run("leaf");
        assert!(matches!(p.full_reload, Some(FullReload::NoBoundary { .. })));
    }

    #[test]
    fn multiple_boundaries_and_diamond() {
        //      main
        //     /    \
        //    a!     b!
        //     \    /
        //      mid
        //       |
        //      leaf
        let g = G::new(&[
            ("main", &["a", "b"]),
            ("a", &["mid"]),
            ("b", &["mid"]),
            ("mid", &["leaf"]),
        ])
        .self_accept("a")
        .self_accept("b");
        let (b, inv, p) = g.run("leaf");
        assert_eq!(b, vec![pair("a", "a"), pair("b", "b")]);
        assert_eq!(inv, ["a", "b", "leaf", "mid"]);
        assert_eq!(p.full_reload, None);
    }

    #[test]
    fn shared_path_is_walked_once_but_every_chain_is_invalidated() {
        // leaf <- x <- y!   and leaf <- z <- x (x reached twice)
        let g = G::new(&[
            ("main", &["y"]),
            ("y", &["x"]),
            ("x", &["leaf", "z"]),
            ("z", &["leaf"]),
        ])
        .self_accept("y");
        let (b, inv, _) = g.run("leaf");
        assert_eq!(b, vec![pair("y", "y")]);
        assert_eq!(inv, ["leaf", "x", "y", "z"]);
    }

    #[test]
    fn declined_module_forces_full_reload() {
        let g = G::new(&[("main", &["app"]), ("app", &["leaf"])])
            .self_accept("main")
            .decline("app");
        let (_, _, p) = g.run("leaf");
        assert_eq!(
            p.full_reload,
            Some(FullReload::Declined {
                module: g.get("app")
            })
        );
        let g = G::new(&[("main", &["leaf"])]).decline("leaf");
        let (_, _, p) = g.run("leaf");
        assert!(matches!(p.full_reload, Some(FullReload::Declined { .. })));
    }

    #[test]
    fn cycles_terminate_and_are_flagged() {
        // a <-> b, both under self-accepting app.
        let g = G::new(&[
            ("main", &["app"]),
            ("app", &["a"]),
            ("a", &["b"]),
            ("b", &["a"]),
        ])
        .self_accept("app");
        let (b, inv, p) = g.run("a");
        assert_eq!(b, vec![pair("app", "app")]);
        assert!(p.within_circular);
        assert_eq!(inv, ["a", "app", "b"]);
        assert_eq!(p.full_reload, None);
    }

    #[test]
    fn invalidate_skips_own_acceptance() {
        let g = G::new(&[("main", &["app"]), ("app", &["comp"])])
            .self_accept("comp")
            .self_accept("app");
        let p = g.g.propagate(g.get("comp"), true);
        assert_eq!(
            p.boundaries,
            vec![Boundary {
                boundary: g.get("app"),
                accepted_via: g.get("app")
            }]
        );
        let g = G::new(&[("main", &["comp"])]).self_accept("comp");
        assert!(g.g.propagate(g.get("comp"), true).full_reload.is_some());
    }

    #[test]
    fn set_imports_maintains_importers_and_reports_pruned() {
        let mut g = ModuleGraph::new();
        let [a, b, c] = ["a", "b", "c"].map(|n| g.ensure(Path::new(n), n, ModuleKind::Script));
        let d = g.ensure(Path::new("d"), "d", ModuleKind::Script);
        assert!(
            g.set_imports(a, vec![b, c, b], vec![], false, false)
                .is_empty()
        );
        assert!(g.set_imports(d, vec![c], vec![], false, false).is_empty());
        assert_eq!(g.node(b).importers, [a]);
        assert_eq!(g.node(c).importers, [a, d]);
        // a drops b (now orphaned → pruned) and c (still imported by d).
        let pruned = g.set_imports(a, vec![], vec![], true, false);
        assert_eq!(pruned, [b]);
        assert!(g.node(b).importers.is_empty());
        assert!(g.node(b).pruned && !g.node(c).pruned);
        assert_eq!(g.node(c).importers, [d]);
        assert!(g.node(a).self_accepting);
    }

    #[test]
    fn deep_chain_does_not_overflow() {
        let mut g = ModuleGraph::new();
        let n = 200_000u32;
        let ids: Vec<_> = (0..n)
            .map(|i| {
                g.ensure(
                    Path::new(&format!("m{i}")),
                    &format!("/m{i}"),
                    ModuleKind::Script,
                )
            })
            .collect();
        for w in ids.windows(2) {
            g.set_imports(w[0], vec![w[1]], vec![], false, false);
        }
        g.node_mut(ids[0]).self_accepting = true;
        let p = g.propagate(ids[n as usize - 1], false);
        assert_eq!(p.boundaries.len(), 1);
        assert_eq!(p.invalidated.len(), n as usize);
    }

    #[test]
    fn timestamps_apply_to_invalidated_only() {
        let mut g = G::new(&[("main", &["app"]), ("app", &["leaf"])]).self_accept("app");
        let p = g.g.propagate(g.get("leaf"), false);
        g.g.apply_timestamp(&p, 42);
        let (main, app, leaf) = (g.get("main"), g.get("app"), g.get("leaf"));
        assert_eq!(g.g.node(main).last_hmr_timestamp, 0);
        assert_eq!(g.g.node(app).last_hmr_timestamp, 42);
        assert_eq!(g.g.node(leaf).last_hmr_timestamp, 42);
    }
}
