//! What the fast path knows about a lib file without parsing it: the global names it
//! declares and its `/// <reference lib>` values, cached per lib text hash. With them the
//! fast path computes deep hashes and the lib order before parsing any lib, and parses the
//! DOM libs only when a re-checked file can see them (design record 0005, implementation
//! notes "Lazy loading").

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_binder::BindOptions;
use ts_r_cache::{Cache, Key, KeyBuilder};
use ts_r_checker::{LibStore, SourceFile};

use crate::codec::{Reader, Writer};
use crate::shape::Hash;

/// Lib files parsed only when a re-checked file can see one of their names. They declare no
/// type the checker looks up by itself except `ImportMeta` (`import.meta`), and other libs
/// never reference them.
pub(crate) const DOM_GROUP: [&str; 3] = [
    "lib.dom.d.ts",
    "lib.dom.iterable.d.ts",
    "lib.dom.asynciterable.d.ts",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LibInfo {
    /// Global names it declares (as [`crate::shape::lib_global_names`] collects them).
    pub globals: Vec<String>,
    /// Its `/// <reference lib="...">` values, in order.
    pub refs: Vec<String>,
}

impl LibInfo {
    pub fn of(f: &SourceFile) -> LibInfo {
        let mut globals = crate::shape::lib_globals_of(f);
        globals.sort_unstable();
        globals.dedup();
        LibInfo {
            globals,
            refs: f
                .references
                .iter()
                .filter(|(k, _)| *k == "lib")
                .map(|(_, v)| v.clone())
                .collect(),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.count(self.globals.len());
        for g in &self.globals {
            w.str(g);
        }
        w.count(self.refs.len());
        for r in &self.refs {
            w.str(r);
        }
        w.out
    }

    fn decode(data: &[u8]) -> Option<LibInfo> {
        let mut r = Reader::new(data);
        let n = r.count()?;
        let mut globals = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            globals.push(r.str()?);
        }
        let n = r.count()?;
        let mut refs = Vec::with_capacity(n.min(1 << 10));
        for _ in 0..n {
            refs.push(r.str()?);
        }
        r.done().then_some(LibInfo { globals, refs })
    }
}

fn key(hash: &Hash) -> Key {
    KeyBuilder::new("ts-r/check-lib-info")
        .str(env!("CARGO_PKG_VERSION"))
        .str(&crate::persist::exe_stamp())
        .u64(crate::persist::FORMAT_VERSION as u64)
        .bytes(hash)
        .key()
}

/// Infos of `libs` (name, text hash; the hashes were verified), from the cache or by parsing
/// (parsed files stay in `store`'s cache). `None` when a lib cannot be read.
pub(crate) fn infos(
    cache: &Cache,
    store: &LibStore,
    libs: &[(String, Hash)],
    opts: &BindOptions,
    threads: usize,
) -> Option<Vec<LibInfo>> {
    let found: Vec<Option<LibInfo>> = libs
        .iter()
        .map(|(_, h)| cache.get(&key(h)).and_then(|b| LibInfo::decode(&b)))
        .collect();
    let missing: Vec<usize> = (0..libs.len()).filter(|&i| found[i].is_none()).collect();
    let parsed: Vec<Option<Arc<SourceFile>>> =
        crate::lazy::par_map(&missing, threads, |&i| store.get(&libs[i].0, opts));
    let mut out = found;
    for (&i, f) in missing.iter().zip(parsed) {
        let f = f?;
        let info = LibInfo::of(&f);
        let _ = cache.put(&key(&libs[i].1), &info.encode());
        out[i] = Some(info);
    }
    out.into_iter().collect()
}

/// Caches the infos of `program`'s lib files (with their saved names and hashes, in program
/// order) that are not cached yet.
pub(crate) fn remember(
    cache: &Cache,
    program: &ts_r_checker::ProgramFiles,
    libs: &[(String, Hash)],
) {
    for (f, (_, h)) in program.files.iter().filter(|f| f.is_lib).zip(libs) {
        let k = key(h);
        if cache.get(&k).is_none() {
            let _ = cache.put(&k, &LibInfo::of(f).encode());
        }
    }
}

/// The lib files `load_program` would load, in its order (referenced libs first), from the
/// infos; and the names it would report missing. `None` when the saved list `libs` is not
/// that order (a lib appeared): the caller starts over.
pub(crate) fn walk(
    roots: &[&str],
    libs: &[(String, Hash)],
    infos: &[LibInfo],
    exists: &dyn Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let at: FxHashMap<&str, usize> = libs
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.as_str(), i))
        .collect();
    let mut out: Vec<usize> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut seen: FxHashSet<String> = FxHashSet::default();
    let mut stack: Vec<(String, usize, u32)> = Vec::new();
    for &name in roots {
        if !seen.insert(name.to_owned()) {
            continue;
        }
        stack.push((name.to_owned(), 0, 0));
        while let Some((n, i, depth)) = stack.pop() {
            let Some(&li) = at.get(n.as_str()) else {
                if exists(&n) {
                    return None;
                }
                missing.push(n);
                continue;
            };
            let next = infos[li]
                .refs
                .iter()
                .enumerate()
                .skip(i)
                .find_map(|(j, v)| ts_r_config::lib_file_name(v).map(|file| (j, file)));
            match next {
                Some((j, file)) => {
                    stack.push((n, j + 1, depth));
                    if depth < 64 && seen.insert(file.to_owned()) {
                        stack.push((file.to_owned(), 0, depth + 1));
                    }
                }
                None => out.push(li),
            }
        }
    }
    (out == (0..libs.len()).collect::<Vec<_>>()).then_some(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_round_trips() {
        let i = LibInfo {
            globals: vec!["Array".into(), "document".into()],
            refs: vec!["es5".into()],
        };
        assert_eq!(LibInfo::decode(&i.encode()), Some(i.clone()));
        let b = i.encode();
        assert_eq!(LibInfo::decode(&b[..b.len() - 1]), None);
    }

    #[test]
    fn walk_puts_referenced_libs_first() {
        let libs: Vec<(String, Hash)> = ["lib.es5.d.ts", "lib.dom.d.ts", "lib.es2015.d.ts"]
            .iter()
            .map(|n| ((*n).to_owned(), [0; 32]))
            .collect();
        let infos = vec![
            LibInfo::default(),
            LibInfo::default(),
            LibInfo {
                globals: Vec::new(),
                refs: vec!["es5".into(), "dom".into()],
            },
        ];
        let none = |_: &str| false;
        assert_eq!(
            walk(&["lib.es2015.d.ts"], &libs, &infos, &none),
            Some(Vec::new())
        );
        // Saved in another order: start over.
        assert_eq!(
            walk(&["lib.dom.d.ts", "lib.es2015.d.ts"], &libs, &infos, &none),
            None
        );
        // A referenced lib that is not saved: missing when it does not exist, else start over.
        let mut more = infos.clone();
        more[0].refs.push("es2016".into());
        assert_eq!(
            walk(&["lib.es2015.d.ts"], &libs, &more, &none),
            Some(vec!["lib.es2016.d.ts".to_owned()])
        );
        assert_eq!(walk(&["lib.es2015.d.ts"], &libs, &more, &|_| true), None);
    }
}
