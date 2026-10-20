//! Splitting a program into levels (design record 0005, "Layers").
//!
//! Level 0 holds the lib files, level 1 the dependencies (files found through
//! `node_modules`), level 2 the project's own files. Lower levels are checked once and shared
//! (see `ts_r_checker::layers`), so the split must keep the *closure rule*: a file never
//! depends on a file of a higher level. Starting from that guess, files move down until:
//!
//! * every import, `/// <reference>` and module augmentation target of a file is at the
//!   file's level or below (the target moves down);
//! * a file that declares a global name (script globals, `declare global`, `export as
//!   namespace`) is at or below every other file that declares or freely references that
//!   name (the declaring file moves down): otherwise a lower level would have resolved or
//!   merged the name without it;
//! * a file that declares an ambient module (`declare module "x"`, also a module
//!   augmentation of a name no file resolves to) is at or below every file that imports `x`
//!   without resolving it to a file, and every other declaration of `x`.
//!
//! Levels only decrease, so the fix-point terminates. Empty levels are dropped.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_checker::ProgramFiles;

/// Level of the lib files.
pub const LIB: u8 = 0;
/// Level of dependencies (`node_modules`).
pub const DEPS: u8 = 1;
/// Level of the project's own files.
pub const PROJECT: u8 = 2;
/// Level of the files being edited and everything that depends on them.
pub const HOT: u8 = 3;

/// The "must be at or below" relation between a program's files, as groups: every file of
/// `needers` needs every file of `needed` at its level or below. Plain imports are groups
/// with one file on each side; a global name is a group whose needers are its declarers and
/// free referencers and whose needed are its declarers.
#[derive(Debug, Default)]
pub struct Graph {
    groups: Vec<(Vec<u32>, Vec<u32>)>,
    /// Groups each file is a needer of / is needed by.
    needs: Vec<Vec<u32>>,
    needed_by: Vec<Vec<u32>>,
}

impl Graph {
    /// Builds the relation of `program` (see the module docs).
    pub fn build(program: &ProgramFiles) -> Graph {
        let files = &program.files;
        let n = files.len();
        let index: FxHashMap<&str, u32> = files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_str(), i as u32))
            .collect();
        let mut edges: Vec<(u32, u32)> = Vec::new();
        // Unresolved module specifiers, with their importers.
        let mut unresolved: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        let mut declares: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        let mut references: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        let mut ambient: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        for (i, f) in files.iter().enumerate() {
            let i = i as u32;
            let b = &f.bound;
            if let Some(ds) = program.deps.get(&f.path) {
                for d in ds {
                    if let Some(&j) = index.get(d.as_str()) {
                        edges.push((i, j));
                    }
                }
            }
            for (spec, _) in crate::module_requests(f, &program.options) {
                match program
                    .resolutions
                    .get(&(f.path.clone(), spec.to_string()))
                    .and_then(|r| r.as_ref())
                {
                    Some(r) => {
                        if let Some(&j) = index.get(r.resolved.as_str()) {
                            edges.push((i, j));
                        }
                    }
                    None => unresolved.entry(spec.to_string()).or_default().push(i),
                }
            }
            let mut declare = |name: &str| declares.entry(name.to_owned()).or_default().push(i);
            for &(name, _) in b.script_globals() {
                declare(b.name_text(&f.ast, name));
            }
            for &(name, _) in b.table(b.global_augmentations) {
                declare(b.name_text(&f.ast, name));
            }
            if let Some((name, _)) = b.umd_name {
                declare(b.name_text(&f.ast, name));
            }
            for &(name, _) in b.table(b.ambient_modules) {
                ambient
                    .entry(b.name_text(&f.ast, name).to_owned())
                    .or_default()
                    .push(i);
            }
            for &(name, _) in b.table(b.module_augmentations) {
                let spec = b.name_text(&f.ast, name).to_owned();
                match program
                    .resolutions
                    .get(&(f.path.clone(), spec.clone()))
                    .and_then(|r| r.as_ref())
                    .and_then(|r| index.get(r.resolved.as_str()))
                {
                    // Augmenting a file: both must be in one level (they merge).
                    Some(&j) => {
                        edges.push((j, i));
                        edges.push((i, j));
                    }
                    None => ambient.entry(spec).or_default().push(i),
                }
            }
            let mut seen: FxHashSet<&str> = FxHashSet::default();
            for r in &b.refs.free {
                let name = b.name_text(&f.ast, r.name);
                if seen.insert(name) {
                    references.entry(name.to_owned()).or_default().push(i);
                }
            }
        }
        let mut g = Graph {
            groups: Vec::new(),
            needs: vec![Vec::new(); n],
            needed_by: vec![Vec::new(); n],
        };
        edges.sort_unstable();
        edges.dedup();
        for (i, j) in edges {
            if i != j {
                g.push(vec![i], vec![j]);
            }
        }
        let mut names: Vec<(String, Vec<u32>)> = declares.into_iter().collect();
        names.sort_unstable();
        for (name, decl) in names {
            let mut needers = decl.clone();
            if let Some(r) = references.get(&name) {
                needers.extend(r);
            }
            g.push(needers, decl);
        }
        let mut amb: Vec<(String, Vec<u32>)> = ambient.into_iter().collect();
        amb.sort_unstable();
        for (name, decl) in amb {
            let mut needers = decl.clone();
            for (spec, imps) in &unresolved {
                if ambient_matches(&name, spec) {
                    needers.extend(imps);
                }
            }
            g.push(needers, decl);
        }
        g
    }

    fn push(&mut self, mut needers: Vec<u32>, mut needed: Vec<u32>) {
        needers.sort_unstable();
        needers.dedup();
        needed.sort_unstable();
        needed.dedup();
        if needed.is_empty() || (needers.len() == 1 && needed == needers) {
            return;
        }
        let k = self.groups.len() as u32;
        for &i in &needers {
            self.needs[i as usize].push(k);
        }
        for &j in &needed {
            self.needed_by[j as usize].push(k);
        }
        self.groups.push((needers, needed));
    }

    /// Number of files.
    pub fn len(&self) -> usize {
        self.needs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.needs.is_empty()
    }

    /// Lowers `level` until no file needs a file of a higher level (levels only decrease).
    pub fn settle(&self, level: &mut [u8]) {
        let mut queue: Vec<u32> = (0..self.groups.len() as u32).collect();
        let mut queued = vec![true; self.groups.len()];
        while let Some(k) = queue.pop() {
            queued[k as usize] = false;
            let (needers, needed) = &self.groups[k as usize];
            let min = needers
                .iter()
                .chain(needed)
                .map(|&i| level[i as usize])
                .min()
                .unwrap_or(0);
            for &j in needed {
                if level[j as usize] > min {
                    level[j as usize] = min;
                    // Groups `j` is part of may now be violated.
                    let j = j as usize;
                    for &k2 in self.needs[j].iter().chain(&self.needed_by[j]) {
                        if !queued[k2 as usize] {
                            queued[k2 as usize] = true;
                            queue.push(k2);
                        }
                    }
                }
            }
        }
    }

    /// Files that (transitively) need one of `seeds`, the seeds included.
    pub fn dependents(&self, seeds: &[usize]) -> Vec<bool> {
        let mut out = vec![false; self.len()];
        let mut stack: Vec<u32> = Vec::new();
        for &s in seeds {
            if s < out.len() && !out[s] {
                out[s] = true;
                stack.push(s as u32);
            }
        }
        let mut done = vec![false; self.groups.len()];
        while let Some(j) = stack.pop() {
            for &k in &self.needed_by[j as usize] {
                if std::mem::replace(&mut done[k as usize], true) {
                    continue;
                }
                for &i in &self.groups[k as usize].0 {
                    if !out[i as usize] {
                        out[i as usize] = true;
                        stack.push(i);
                    }
                }
            }
        }
        out
    }
}

/// The starting level of a file: lib, dependency or project.
pub fn base_level(f: &ts_r_checker::SourceFile) -> u8 {
    if f.is_lib {
        LIB
    } else if f.is_external || f.path.contains("/node_modules/") {
        DEPS
    } else {
        PROJECT
    }
}

/// Plans the level of every file of `program` (by index): lib, dependency, project, and
/// [`HOT`] for `hot` files and everything that needs them. See the module docs.
pub fn plan(program: &ProgramFiles, graph: &Graph, hot: &[usize]) -> Vec<u8> {
    let mut level: Vec<u8> = program.files.iter().map(|f| base_level(f)).collect();
    if !hot.is_empty() {
        for (i, h) in graph.dependents(hot).into_iter().enumerate() {
            if h && level[i] == PROJECT {
                level[i] = HOT;
            }
        }
    }
    graph.settle(&mut level);
    level
}

/// `declare module "name"` (possibly a `*` pattern) covers specifier `spec`.
pub(crate) fn ambient_matches(name: &str, spec: &str) -> bool {
    match name.split_once('*') {
        Some((pre, post)) => {
            spec.len() >= pre.len() + post.len() && spec.starts_with(pre) && spec.ends_with(post)
        }
        None => name == spec,
    }
}

/// `program` with its files reordered by `levels` (stable within a level). Returns the new
/// program and, per kept level, the index one past its last file.
pub fn reorder(program: &ProgramFiles, levels: &[u8]) -> (ProgramFiles, Vec<(u8, usize)>) {
    let mut order: Vec<usize> = (0..program.files.len()).collect();
    order.sort_by_key(|&i| levels[i]);
    let files: Vec<_> = order.iter().map(|&i| program.files[i].clone()).collect();
    let mut ends: Vec<(u8, usize)> = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        match ends.last_mut() {
            Some((l, end)) if *l == levels[i] => *end = k + 1,
            _ => ends.push((levels[i], k + 1)),
        }
    }
    (with_files(program, files), ends)
}

/// A copy of `program` with other files (resolutions and dependencies kept).
pub fn with_files(
    program: &ProgramFiles,
    files: Vec<std::sync::Arc<ts_r_checker::SourceFile>>,
) -> ProgramFiles {
    ProgramFiles {
        files,
        resolutions: program.resolutions.clone(),
        mode_resolutions: program.mode_resolutions.clone(),
        options: program.options.clone(),
        roots: program.roots.clone(),
        missing_libs: program.missing_libs.clone(),
        missing_types: program.missing_types.clone(),
        ambiguous_roots: program.ambiguous_roots.clone(),
        deps: program.deps.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_patterns() {
        assert!(ambient_matches("*.svg", "./a.svg"));
        assert!(ambient_matches("foo", "foo"));
        assert!(!ambient_matches("foo", "foo/bar"));
        assert!(ambient_matches("foo/*", "foo/bar"));
        assert!(!ambient_matches("*.svg", "a.png"));
    }
}
