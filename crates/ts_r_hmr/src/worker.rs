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

/// `url` with `worker_file&type=<ty>` first in its query, the URL's own parameters after
/// it (and its `#hash` kept last), as a `new Worker(new URL(...))` argument is rewritten.
pub fn inject_worker_query(url: &str, ty: &str) -> String {
    let (no_hash, hash) = match url.find('#') {
        Some(i) => url.split_at(i),
        None => (url, ""),
    };
    let (path, query) = match no_hash.split_once('?') {
        Some((p, q)) => (p, q),
        None => (no_hash, ""),
    };
    let mut out = format!("{path}?worker_file&type={ty}");
    if !query.is_empty() {
        out.push('&');
        out.push_str(query);
    }
    out.push_str(hash);
    out
}

/// A classic worker can't `import` the client's `injectQuery`: its own copy, appended to the
/// worker's code (`import(expr)` gets `?import` added at run time, as in modules).
pub const CLASSIC_INJECT_QUERY: &str = "\nfunction __vite__injectQuery(url, query) {\n  if (url[0] !== '.' && url[0] !== '/') return url;\n  const at = url.search(/[?#]/);\n  const path = at < 0 ? url : url.slice(0, at);\n  const { search, hash } = new URL(url, 'http://localhost');\n  return path + '?' + query + (search ? '&' + search.slice(1) : '') + hash;\n}\n";

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
/// module, imported (`type=module`) or loaded with `importScripts` (`type=classic`). A
/// worker whose options carried `/* @vite-ignore */` (`type=ignore`) may be either kind, so
/// the env module's code (`env_code`) is put in instead, or nothing when it is empty
/// (`vite: packages/vite/src/node/plugins/worker.ts`, the `worker_file` transform). The
/// text after the env part is `;\n`, as Vite writes it.
pub fn env_prelude(query: &str, env_url: &str, env_code: &str) -> Option<String> {
    http::query_param(query, "worker_file")?;
    let url = serde_json::to_string(env_url).unwrap_or_else(|_| "\"\"".to_owned());
    Some(match http::query_param(query, "type") {
        Some("classic") => format!("importScripts({url})\n;\n"),
        Some("ignore") if env_code.trim().is_empty() => String::new(),
        Some("ignore") => format!("{}\n;\n", env_code.trim_end()),
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
    fn worker_query_goes_first() {
        assert_eq!(
            inject_worker_query("/es/w.js", "classic"),
            "/es/w.js?worker_file&type=classic"
        );
        assert_eq!(
            inject_worker_query("/w.ts?raw=1#x", "module"),
            "/w.ts?worker_file&type=module&raw=1#x"
        );
    }

    #[test]
    fn env_preludes() {
        assert_eq!(
            env_prelude("worker_file&type=module", "/@vite/env", "").as_deref(),
            Some("import \"/@vite/env\"\n;\n")
        );
        assert_eq!(
            env_prelude("worker_file&type=classic", "/b/@vite/env", "").as_deref(),
            Some("importScripts(\"/b/@vite/env\")\n;\n")
        );
        assert_eq!(
            env_prelude(
                "worker_file&type=ignore",
                "/@vite/env",
                "globalThis.X = 1;\n"
            )
            .as_deref(),
            Some("globalThis.X = 1;\n;\n")
        );
        assert_eq!(
            env_prelude("worker_file&type=ignore", "/@vite/env", "").as_deref(),
            Some("")
        );
        assert_eq!(env_prelude("import", "/@vite/env", ""), None);
    }
}
