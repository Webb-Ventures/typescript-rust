//! The `--build` project graph: the projects named on the command line and everything they
//! reference, in build order (depth-first, references before the project, each once), as
//! tsc 7 lists them under "Projects in this build". A reference cycle is TS6202 with the
//! chain of config paths from the outermost project; `circular: true` references (and
//! everything reached through them) are followed for order but never reported.

use ts_r_config::{ConfigDiagnostic, FileSystem, ParsedConfig, path};

/// One project of the build.
#[derive(Debug, Clone)]
pub struct Node {
    /// Absolute config path.
    pub config_path: String,
    /// `None` when the config file does not exist.
    pub config: Option<ParsedConfig>,
    /// Indices (into [`Graph::order`]) of the projects it references, in reference order,
    /// with the reference's resolved path (tsc names a dependency by it).
    pub references: Vec<(usize, String)>,
    /// Named on the command line (not only reached through a reference).
    pub root: bool,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub order: Vec<Node>,
    /// TS6202 for each cycle.
    pub cycles: Vec<ConfigDiagnostic>,
}

/// The config file a `--build` argument names: a file as given, else `<dir>/tsconfig.json`.
pub fn resolve_project(fs: &dyn FileSystem, cwd: &str, arg: &str) -> String {
    let abs = path::resolve(cwd, &path::normalize_slashes(arg));
    if fs.file_exists(&abs) || abs.to_ascii_lowercase().ends_with(".json") {
        abs
    } else {
        path::combine(&abs, "tsconfig.json")
    }
}

/// Builds the graph from `roots` (absolute config paths). `load` loads a config that
/// exists.
pub fn build_graph(
    fs: &dyn FileSystem,
    roots: &[String],
    load: &mut dyn FnMut(&str) -> ParsedConfig,
) -> Graph {
    let cs = fs.case_sensitive();
    let mut g = Graph::default();
    // key → index in order (finished), and the DFS stack of (key, config path).
    let mut done: Vec<(String, usize)> = Vec::new();
    let mut stack: Vec<(String, String)> = Vec::new();
    for r in roots {
        if let Some(i) = visit(fs, cs, r, false, &mut g, &mut done, &mut stack, load) {
            g.order[i].root = true;
        }
    }
    g
}

#[allow(clippy::too_many_arguments)]
fn visit(
    fs: &dyn FileSystem,
    cs: bool,
    config_path: &str,
    circular_ok: bool,
    g: &mut Graph,
    done: &mut Vec<(String, usize)>,
    stack: &mut Vec<(String, String)>,
    load: &mut dyn FnMut(&str) -> ParsedConfig,
) -> Option<usize> {
    let key = path::key(config_path, cs);
    if let Some(&(_, i)) = done.iter().find(|(k, _)| *k == key) {
        return Some(i);
    }
    if stack.iter().any(|(k, _)| *k == key) {
        if !circular_ok && g.cycles.is_empty() {
            let chain: Vec<&str> = stack.iter().map(|(_, p)| p.as_str()).collect();
            g.cycles.push(ConfigDiagnostic::global(
                6202,
                format!(
                    "Project references may not form a circular graph. Cycle detected: {}",
                    chain.join("\n")
                ),
            ));
        }
        return None;
    }
    // Recursion is bounded by the number of distinct configs (each is on the stack once).
    if stack.len() > 4096 {
        return None;
    }
    let config = fs.file_exists(config_path).then(|| load(config_path));
    stack.push((key.clone(), config_path.to_owned()));
    let mut references = Vec::new();
    if let Some(c) = &config {
        for r in &c.references {
            // `circular: true` holds for everything reached through that reference
            // (observed: a -> b (circular) -> a is not an error).
            let ok = circular_ok || r.circular;
            if !fs.file_exists(&r.config_path) {
                // TS6053 at the reference (the loader reported it). The missing project is
                // still in the build order, where it reports its own TS6053, but it is not
                // an upstream of this one (observed: `tsbuildWatch/sample/watches-config-
                // files-that-are-not-present`).
                visit(fs, cs, &r.config_path, ok, g, done, stack, load);
                continue;
            }
            let shown = path::resolve(&c.config_dir, &r.path);
            if let Some(i) = visit(fs, cs, &r.config_path, ok, g, done, stack, load) {
                references.push((i, shown));
            }
        }
    }
    stack.pop();
    let i = g.order.len();
    g.order.push(Node {
        config_path: config_path.to_owned(),
        config,
        references,
        root: false,
    });
    done.push((key, i));
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_config::{ConfigLoader, MemoryFs};

    fn graph(files: &[(&str, &str)], roots: &[&str]) -> Graph {
        let mut fs = MemoryFs::new();
        for (p, t) in files {
            fs.add_file(p, t);
        }
        let roots: Vec<String> = roots.iter().map(|r| (*r).to_owned()).collect();
        let fs2 = fs.clone();
        build_graph(&fs, &roots, &mut |p| ConfigLoader::new(&fs2).load(p))
    }

    #[test]
    fn post_order_and_cycles() {
        let g = graph(
            &[
                ("/p/a/tsconfig.json", r#"{"references":[{"path":"../b"}]}"#),
                ("/p/b/tsconfig.json", r#"{"references":[{"path":"../c"}]}"#),
                ("/p/c/tsconfig.json", r#"{"references":[{"path":"../a"}]}"#),
                ("/p/a/x.ts", ""),
                ("/p/b/x.ts", ""),
                ("/p/c/x.ts", ""),
            ],
            &["/p/a/tsconfig.json"],
        );
        let order: Vec<&str> = g.order.iter().map(|n| n.config_path.as_str()).collect();
        assert_eq!(
            order,
            [
                "/p/c/tsconfig.json",
                "/p/b/tsconfig.json",
                "/p/a/tsconfig.json"
            ]
        );
        assert_eq!(g.cycles.len(), 1);
        assert!(
            g.cycles[0]
                .message
                .ends_with("/p/a/tsconfig.json\n/p/b/tsconfig.json\n/p/c/tsconfig.json")
        );
    }

    #[test]
    fn missing_project_is_a_node() {
        let g = graph(&[], &["/p/x/tsconfig.json"]);
        assert_eq!(g.order.len(), 1);
        assert!(g.order[0].config.is_none());
    }
}
