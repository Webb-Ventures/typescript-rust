//! Project references (design record 0012, E3): a module that resolves to a source file of a
//! referenced project resolves to that file's declaration output instead, as tsc does with
//! `references` (`-p` and `-b` alike). The map is built by the caller (`ts_r_projects`, which
//! knows each referenced project's files and output rules); the resolver only consults it.
//!
//! - **Source to output**: a result that is a source of a referenced project becomes its
//!   `.d.ts` output, with [`crate::ResolvedModule::project_reference_source`] naming the
//!   source. Whether the output exists is the caller's business (tsc reports TS6305
//!   "Output file ... has not been built from source file ..." when it does not).
//! - **Output to source** (`outDir` mapping): a declaration output of a referenced project
//!   counts as existing while its source exists, so a specifier that names the output
//!   directory (`../core/dist/index`, a package `types` field pointing into `dist/`)
//!   resolves before the project is built. A JavaScript output found by the resolver maps
//!   to its source and then to the declaration output, like a source.
//! - **Format**: a source or output of a referenced project whose `module` is an ES module
//!   kind (`es2015`..`esnext`) is an ES module file to the importer, whatever the importer's
//!   options say (tsc 7: no synthetic default import of it, TS1192).
//! - **Options**: requests in a file of a referenced project (any file of its file list, or
//!   a declaration output standing in for a source) resolve with that project's compiler
//!   options ([`ProjectReferences::add_project`]), as tsc does ("Using compiler options of
//!   project reference redirect").

use std::collections::{HashMap, HashSet};

use ts_r_config::path;

use crate::ResolverOptions;

/// Sources and outputs of the referenced projects (transitively) of one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectReferences {
    /// Source key → (source path, declaration output path).
    sources: HashMap<String, (String, String)>,
    /// Output key (declaration or JavaScript) → source key.
    outputs: HashMap<String, String>,
    /// Source keys of projects whose `module` is an ES module kind.
    esm: HashSet<String>,
    /// Each referenced project's config path and resolver options.
    projects: Vec<(String, ResolverOptions)>,
    /// File key (a file of a referenced project's file list) → project index.
    members: HashMap<String, usize>,
    case_sensitive: bool,
}

impl ProjectReferences {
    pub fn new(case_sensitive: bool) -> ProjectReferences {
        ProjectReferences {
            case_sensitive,
            ..ProjectReferences::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.members.is_empty()
    }

    fn key(&self, p: &str) -> String {
        path::key(p, self.case_sensitive)
    }

    /// Records `source` of a referenced project with its declaration output `dts` and its
    /// JavaScript output `js` (absolute, normalised paths). The first project to claim a
    /// source keeps it.
    pub fn add(&mut self, source: &str, dts: &str, js: Option<&str>) {
        let k = self.key(source);
        if self.sources.contains_key(&k) {
            return;
        }
        self.outputs.insert(self.key(dts), k.clone());
        if let Some(js) = js {
            self.outputs.insert(self.key(js), k.clone());
        }
        self.sources.insert(k, (source.to_owned(), dts.to_owned()));
    }

    /// Records a referenced project (its config path and the resolver options of its
    /// compiler options) and returns its index for [`add_member`](Self::add_member).
    /// Its `project_references` are ignored: they are this map.
    pub fn add_project(&mut self, config_path: &str, mut options: ResolverOptions) -> usize {
        options.project_references = None;
        self.projects.push((config_path.to_owned(), options));
        self.projects.len() - 1
    }

    /// Records `file` (from the file list of project `project`) as resolving its requests
    /// with that project's options. A file in several projects goes with the last one
    /// added (observed: with `pkg` → `pkg/tsconfig.native.json` both listing a file, its
    /// imports resolve with the native project's `moduleSuffixes`).
    pub fn add_member(&mut self, file: &str, project: usize) {
        if project < self.projects.len() {
            self.members.insert(self.key(file), project);
        }
    }

    /// How many projects [`add_project`](Self::add_project) recorded.
    pub fn project_count(&self) -> usize {
        self.projects.len()
    }

    /// The project whose options requests in `file` resolve with: `file` is in its file list,
    /// or is the declaration output of such a file. Index and config path.
    pub fn project_of(&self, file: &str) -> Option<(usize, &str)> {
        let k = self.key(file);
        let i = match self.members.get(&k) {
            Some(&i) => i,
            None => *self.members.get(self.outputs.get(&k)?)?,
        };
        Some((i, self.projects.get(i)?.0.as_str()))
    }

    /// The resolver options of project `index` (without `project_references`).
    pub fn project_options(&self, index: usize) -> &ResolverOptions {
        &self.projects[index].1
    }

    /// Marks `source` (already [`add`](Self::add)ed) as compiled with an ES `module` kind.
    pub fn mark_esm(&mut self, source: &str) {
        let k = self.key(source);
        if self.sources.contains_key(&k) {
            self.esm.insert(k);
        }
    }

    /// True when `file` (a source or an output of a referenced project) was marked with
    /// [`mark_esm`](Self::mark_esm).
    pub fn is_esm(&self, file: &str) -> bool {
        let k = self.key(file);
        match self.outputs.get(&k) {
            Some(s) => self.esm.contains(s),
            None => self.esm.contains(&k),
        }
    }

    /// The declaration output standing in for `file` when it is a source of a referenced
    /// project: `(output, source)`.
    pub fn redirect(&self, file: &str) -> Option<(&str, &str)> {
        let k = self.key(file);
        let (src, dts) = match self.sources.get(&k) {
            Some(e) => e,
            None => self.sources.get(self.outputs.get(&k)?)?,
        };
        Some((dts.as_str(), src.as_str()))
    }

    /// The source a declaration or JavaScript output of a referenced project comes from.
    pub fn source_of_output(&self, output: &str) -> Option<&str> {
        let k = self.outputs.get(&self.key(output))?;
        self.sources.get(k).map(|(s, _)| s.as_str())
    }

    /// True when `file` is a declaration output of a referenced project (whether it exists
    /// or not).
    pub fn is_declaration_output(&self, file: &str) -> bool {
        let k = self.key(file);
        self.outputs
            .get(&k)
            .and_then(|s| self.sources.get(s))
            .is_some_and(|(_, dts)| self.key(dts) == k)
    }

    /// Every `(source, declaration output)` pair, in no particular order.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.sources.values().map(|(s, d)| (s.as_str(), d.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_sources_and_outputs() {
        let mut r = ProjectReferences::new(true);
        r.add(
            "/p/core/index.ts",
            "/p/core/dist/index.d.ts",
            Some("/p/core/dist/index.js"),
        );
        assert_eq!(
            r.redirect("/p/core/index.ts"),
            Some(("/p/core/dist/index.d.ts", "/p/core/index.ts"))
        );
        assert_eq!(
            r.redirect("/p/core/dist/index.js"),
            Some(("/p/core/dist/index.d.ts", "/p/core/index.ts"))
        );
        assert_eq!(
            r.source_of_output("/p/core/dist/index.d.ts"),
            Some("/p/core/index.ts")
        );
        assert!(r.is_declaration_output("/p/core/dist/index.d.ts"));
        assert!(!r.is_declaration_output("/p/core/dist/index.js"));
        assert_eq!(r.redirect("/p/other.ts"), None);
        assert!(!r.is_esm("/p/core/dist/index.d.ts"));
        r.mark_esm("/p/core/index.ts");
        assert!(r.is_esm("/p/core/dist/index.d.ts") && r.is_esm("/p/core/index.ts"));
    }
}
