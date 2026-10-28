//! Yarn Plug'n'Play: finding the directory of a package named by a bare specifier through a
//! `.pnp.data.json` manifest, for projects without `node_modules`.
//!
//! The manifest's `packageRegistryData` lists every package as
//! `[name, [[reference, {packageLocation, packageDependencies, ...}], ...]]` (`null` name and
//! reference for the top-level workspace). A file belongs to the package whose
//! `packageLocation` (relative to the manifest's directory) is the longest prefix of its path;
//! a bare `name/sub` from that file names the package its `packageDependencies` maps `name`
//! to (`[name, reference]`, or `[name, [alias, reference]]` for an aliased dependency; a `null`
//! reference is an unmet peer dependency). With `enableTopLevelFallback`, a name missing from
//! the file's own dependencies is looked up in `fallbackPool`, unless the package is in
//! `fallbackExclusionList`. Behaviour follows Yarn's PnP specification and esbuild's
//! `bundler_yarnpnp_test.go` cases.
//!
//! Lookups are bounded: one walk up the directory tree for the manifest, one pass over its
//! packages, no recursion.

use crate::fs::FileSystem;
use crate::jsonc::{self, Value};
use crate::path;

/// Manifest file names, in the order they are looked for in each directory.
const MANIFESTS: &[&str] = &[".pnp.data.json"];

/// A package: its name (`None` for the top level) and reference.
type Locator = (Option<String>, Option<String>);

#[derive(Debug)]
struct Package {
    locator: Locator,
    /// Absolute directory, with a trailing `/`.
    location: String,
    /// Dependency name to the locator it resolves to (`None`: an unmet peer).
    deps: Vec<(String, Option<Locator>)>,
}

/// A parsed `.pnp.data.json`.
#[derive(Debug)]
pub struct PnpManifest {
    packages: Vec<Package>,
    fallback: bool,
    fallback_pool: Vec<(String, Option<Locator>)>,
    fallback_excluded: Vec<Locator>,
}

impl PnpManifest {
    /// Finds the manifest that governs `dir`: the nearest `.pnp.data.json` in `dir` or an
    /// ancestor.
    pub fn find(fs: &dyn FileSystem, dir: &str) -> Option<PnpManifest> {
        let mut dir = path::normalize(dir);
        loop {
            for name in MANIFESTS {
                let file = path::combine(&dir, name);
                if fs.file_exists(&file) {
                    let text = fs.read_file(&file)?;
                    let root = jsonc::parse(&text, &file).root?;
                    return Some(Self::from_json(&root, &dir));
                }
            }
            let parent = path::directory_of(&dir).to_owned();
            if parent == dir || parent.is_empty() {
                return None;
            }
            dir = parent;
        }
    }

    /// Reads the manifest's JSON; `dir` is the directory it was found in. Malformed entries
    /// are skipped.
    pub fn from_json(root: &Value, dir: &str) -> PnpManifest {
        let mut packages = Vec::new();
        for entry in root
            .get("packageRegistryData")
            .and_then(Value::as_array)
            .unwrap_or_default()
        {
            let Some([name, refs]) = entry.as_array() else {
                continue;
            };
            let name = name.as_str().map(str::to_owned);
            for r in refs.as_array().unwrap_or_default() {
                let Some([reference, info]) = r.as_array() else {
                    continue;
                };
                let Some(loc) = info.get("packageLocation").and_then(Value::as_str) else {
                    continue;
                };
                let mut location = path::resolve(dir, loc);
                if !location.ends_with('/') {
                    location.push('/');
                }
                packages.push(Package {
                    locator: (name.clone(), reference.as_str().map(str::to_owned)),
                    location,
                    deps: dependency_list(info.get("packageDependencies")),
                });
            }
        }
        let fallback_excluded = root
            .get("fallbackExclusionList")
            .and_then(Value::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| {
                let [name, refs] = e.as_array()? else {
                    return None;
                };
                let name = name.as_str()?.to_owned();
                Some(
                    refs.as_array()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|r| r.as_str())
                        .map(|r| (Some(name.clone()), Some(r.to_owned())))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        PnpManifest {
            packages,
            fallback: root
                .get("enableTopLevelFallback")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            fallback_pool: dependency_list(root.get("fallbackPool")),
            fallback_excluded,
        }
    }

    /// The package that owns `file`: the longest `packageLocation` prefix of its path.
    fn owner(&self, file: &str) -> Option<&Package> {
        let file = path::normalize(file);
        self.packages
            .iter()
            .filter(|p| file.starts_with(&p.location) || format!("{file}/") == p.location)
            .max_by_key(|p| p.location.len())
    }

    fn location_of(&self, locator: &Locator) -> Option<&str> {
        self.packages
            .iter()
            .find(|p| &p.locator == locator)
            .map(|p| p.location.as_str())
    }

    /// The directory (with a trailing `/`) of package `name` as seen from `issuer` (a file
    /// or directory path), or `None` when the manifest does not resolve it.
    pub fn package_dir(&self, name: &str, issuer: &str) -> Option<String> {
        let owner = self.owner(issuer)?;
        let dep = match owner.deps.iter().find(|(n, _)| n == name) {
            Some((_, dep)) => dep.clone()?,
            None if self.fallback && !self.fallback_excluded.contains(&owner.locator) => self
                .fallback_pool
                .iter()
                .find(|(n, _)| n == name)
                .and_then(|(_, d)| d.clone())?,
            None => return None,
        };
        self.location_of(&dep).map(str::to_owned)
    }
}

/// `[[name, reference | [alias, reference] | null], ...]` as (name, locator).
fn dependency_list(v: Option<&Value>) -> Vec<(String, Option<Locator>)> {
    v.and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|d| {
            let [name, target] = d.as_array()? else {
                return None;
            };
            let name = name.as_str()?.to_owned();
            let locator = if target.is_null() {
                None
            } else if let Some(r) = target.as_str() {
                Some((Some(name.clone()), Some(r.to_owned())))
            } else if let Some([alias, r]) = target.as_array() {
                Some((
                    alias.as_str().map(str::to_owned),
                    r.as_str().map(str::to_owned),
                ))
            } else {
                return None;
            };
            Some((name, locator))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemoryFs;

    const DATA: &str = r#"{
      "enableTopLevelFallback": true,
      "fallbackPool": [["left-pad", "npm:1.0.0"]],
      "fallbackExclusionList": [["strict", ["workspace:packages/strict"]]],
      "packageRegistryData": [
        [null, [[null, { "packageLocation": "./", "packageDependencies": [["app", "workspace:packages/app"]] }]]],
        ["app", [["workspace:packages/app", { "packageLocation": "./packages/app/",
          "packageDependencies": [["cfg", "workspace:packages/cfg"], ["alias", ["cfg", "workspace:packages/cfg"]], ["peer", null]] }]]],
        ["cfg", [["workspace:packages/cfg", { "packageLocation": "./packages/cfg/", "packageDependencies": [] }]]],
        ["strict", [["workspace:packages/strict", { "packageLocation": "./packages/strict/", "packageDependencies": [] }]]],
        ["left-pad", [["npm:1.0.0", { "packageLocation": "./.yarn/cache/left-pad/" }]]],
        "malformed", [1, 2]
      ]
    }"#;

    fn manifest() -> PnpManifest {
        let mut fs = MemoryFs::new();
        fs.add_file("/w/.pnp.data.json", DATA);
        PnpManifest::find(&fs, "/w/packages/app/src").expect("manifest")
    }

    #[test]
    fn resolves_dependencies_aliases_and_fallbacks() {
        let m = manifest();
        let from = "/w/packages/app/tsconfig.json";
        assert_eq!(
            m.package_dir("cfg", from).as_deref(),
            Some("/w/packages/cfg/")
        );
        assert_eq!(
            m.package_dir("alias", from).as_deref(),
            Some("/w/packages/cfg/")
        );
        assert_eq!(m.package_dir("peer", from), None);
        assert_eq!(m.package_dir("missing", from), None);
        // Not a dependency of `app`: the fallback pool.
        assert_eq!(
            m.package_dir("left-pad", from).as_deref(),
            Some("/w/.yarn/cache/left-pad/")
        );
        // Excluded from the fallback.
        assert_eq!(m.package_dir("left-pad", "/w/packages/strict/a.json"), None);
        // The top level owns everything outside a package.
        assert_eq!(
            m.package_dir("app", "/w/tsconfig.json").as_deref(),
            Some("/w/packages/app/")
        );
    }

    #[test]
    fn no_manifest_or_malformed_manifest() {
        let mut fs = MemoryFs::new();
        assert!(PnpManifest::find(&fs, "/w/a").is_none());
        fs.add_file("/w/.pnp.data.json", "{ \"packageRegistryData\": 3 ");
        let m = PnpManifest::find(&fs, "/w/a").expect("parsed with errors");
        assert_eq!(m.package_dir("x", "/w/a/b.json"), None);
    }
}
