//! Web worker imports in dev (`vite: packages/vite/src/node/plugins/worker.ts`):
//!
//! - `import W from './w.js?worker'` (or `?sharedworker`) is a module whose default export
//!   constructs the worker: `new Worker(url, { type: "module", name: options?.name })`.
//!   `&url` instead exports the URL; `&inline` means nothing in dev.
//! - The worker's URL is the file's with `worker_file&type=module` first in its query and
//!   the import's other parameters after it. In dev a worker is always a module worker.
//! - A request with `worker_file` serves the file as a script with the env module run
//!   first: `import "/@vite/env"` for a module worker, `importScripts(...)` for a classic
//!   one, so `import.meta.env` defines are there before the worker's own code.

use crate::http;

/// Query keys that select the worker wrapper, dropped from the worker file's URL.
const WRAPPER_KEYS: &[&str] = &["worker", "sharedworker", "inline", "url", "import", "t"];

/// The constructor a `?worker` / `?sharedworker` import wraps, if the query asks for one.
/// A request for the worker file itself (`worker_file`) is not a wrapper.
pub fn wrapper_constructor(query: &str) -> Option<&'static str> {
    if http::query_param(query, "worker_file").is_some() {
        return None;
    }
    if http::query_param(query, "sharedworker").is_some() {
        Some("SharedWorker")
    } else if http::query_param(query, "worker").is_some() {
        Some("Worker")
    } else {
        None
    }
}

/// The worker file's URL for a wrapper request of `url` with `query`.
pub fn file_url(url: &str, query: &str) -> String {
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && !WRAPPER_KEYS.contains(&p.split('=').next().unwrap_or(p)))
        .collect();
    let mut out = format!("{url}?worker_file&type=module");
    for p in kept {
        out.push('&');
        out.push_str(p);
    }
    out
}

/// The wrapper module: the URL (`&url`) or a function constructing the worker.
pub fn wrapper_module(constructor: &str, file_url: &str, query: &str) -> String {
    let url = serde_json::to_string(file_url).unwrap_or_else(|_| "\"\"".to_owned());
    if http::query_param(query, "url").is_some() {
        return format!("export default {url}\n");
    }
    format!(
        "export default function WorkerWrapper(options) {{\n  return new {constructor}({url}, {{ type: \"module\", name: options?.name }});\n}}\n"
    )
}

/// What runs before a worker file's code, if `query` asks for the worker file: the env
/// module, imported (`type=module`) or loaded with `importScripts` (`type=classic`).
pub fn env_prelude(query: &str, env_url: &str) -> Option<String> {
    http::query_param(query, "worker_file")?;
    let url = serde_json::to_string(env_url).unwrap_or_else(|_| "\"\"".to_owned());
    Some(match http::query_param(query, "type") {
        Some("classic") => format!("importScripts({url})\n;\n"),
        _ => format!("import {url}\n;\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_urls_keep_other_parameters() {
        assert_eq!(wrapper_constructor("worker"), Some("Worker"));
        assert_eq!(
            wrapper_constructor("sharedworker&name=a"),
            Some("SharedWorker")
        );
        assert_eq!(wrapper_constructor("worker_file&type=module"), None);
        assert_eq!(wrapper_constructor("import"), None);
        assert_eq!(
            file_url("/w.js", "sharedworker&name=shared&inline"),
            "/w.js?worker_file&type=module&name=shared"
        );
        assert_eq!(
            file_url("/w.ts", "worker&t=123"),
            "/w.ts?worker_file&type=module"
        );
    }

    #[test]
    fn wrapper_modules() {
        let m = wrapper_module("Worker", "/w.js?worker_file&type=module", "worker");
        assert!(m.contains("new Worker(\"/w.js?worker_file&type=module\", { type: \"module\", name: options?.name })"), "{m}");
        assert_eq!(
            wrapper_module("Worker", "/w.js?worker_file&type=module", "worker&url"),
            "export default \"/w.js?worker_file&type=module\"\n"
        );
    }

    #[test]
    fn env_preludes() {
        assert_eq!(
            env_prelude("worker_file&type=module", "/@vite/env").as_deref(),
            Some("import \"/@vite/env\"\n;\n")
        );
        assert_eq!(
            env_prelude("worker_file&type=classic", "/b/@vite/env").as_deref(),
            Some("importScripts(\"/b/@vite/env\")\n;\n")
        );
        assert_eq!(env_prelude("import", "/@vite/env"), None);
    }
}
