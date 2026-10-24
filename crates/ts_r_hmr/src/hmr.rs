//! Turning file changes into HMR messages (the critical path of the time budget):
//! read → transform (prepare) → propagate through the graph → push.

use std::fs;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use ts_r_cache::Key;

use crate::graph::{FullReload, ModuleId, ModuleKind};
use crate::server::{self, ModuleError, Shared, Target};
use crate::timing::UpdateTiming;
use crate::watch::{ChangeBatch, ChangeKind, FileChange};

/// Everything one batch produces.
#[derive(Default)]
struct Outcome {
    updates: Vec<Value>,
    pruned: Vec<String>,
    full_reload: Option<Option<String>>,
    errored: bool,
    files: Vec<String>,
}

/// Handles one settled batch from the watcher.
pub(crate) fn on_batch(shared: &Shared, batch: ChangeBatch) {
    if shared.shutdown.load(Ordering::SeqCst) {
        return;
    }
    let _serial = shared.change_lock.lock().unwrap_or_else(|e| e.into_inner());
    let started = Instant::now();
    let mut timing = UpdateTiming {
        id: shared.update_seq.fetch_add(1, Ordering::Relaxed),
        ..Default::default()
    };
    // watch: file mtime → first native event; debounce: first event → flush.
    if let Some(first) = batch.changes.iter().min_by_key(|c| c.first_event) {
        timing.debounce = batch.flushed.saturating_duration_since(first.first_event);
        let mtime = fs::metadata(&first.path).and_then(|m| m.modified()).ok();
        timing.watch = mtime.and_then(|m| first.first_event_wall.duration_since(m).ok());
        // Clamp nonsense (clock skew, a file touched long before the event).
        if timing.watch.is_some_and(|w| w > Duration::from_secs(5)) {
            timing.watch = None;
        }
        timing.origin = Some(match (mtime, timing.watch) {
            (Some(m), Some(_)) => m,
            _ => first.first_event_wall,
        });
    }
    let timestamp = server::next_timestamp(shared);
    let mut out = Outcome::default();
    let mut retry_errored = false;

    for change in &batch.changes {
        if out.full_reload.is_some() {
            break;
        }
        handle_change(
            shared,
            change,
            timestamp,
            &mut timing,
            &mut out,
            &mut retry_errored,
        );
    }
    if retry_errored && out.full_reload.is_none() {
        // A new file may fix a "Failed to resolve import" error elsewhere.
        let errored: Vec<(ModuleId, std::path::PathBuf)> = {
            let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            g.ids()
                .filter(|&i| g.node(i).errored)
                .map(|i| (i, g.node(i).file.clone()))
                .collect()
        };
        for (_, file) in errored {
            let change = FileChange {
                path: file,
                kind: ChangeKind::Modified,
                first_event: started,
                first_event_wall: SystemTime::now(),
            };
            handle_change(
                shared,
                &change,
                timestamp,
                &mut timing,
                &mut out,
                &mut retry_errored,
            );
        }
    }
    if out.files.is_empty() {
        return; // nothing the page uses changed
    }
    finish(shared, out, timing);
}

fn handle_change(
    shared: &Shared,
    change: &FileChange,
    timestamp: u64,
    timing: &mut UpdateTiming,
    out: &mut Outcome,
    retry_errored: &mut bool,
) {
    let path = &change.path;
    let url = server::url_for_file(&shared.root, path);
    shared
        .changed_at
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.clone(), timestamp);
    // `.env*`: new values everywhere, so the page reloads (Vite restarts its server).
    if let Some(dir) = &shared.env_options.dir
        && crate::env::is_env_file(dir, path)
    {
        let before = shared.env();
        shared.reload_env();
        if *shared.env() != *before {
            shared.log(&format!("{} changed, reloading the page", url));
            out.files.push(url);
            out.full_reload = Some(None);
        }
        return;
    }
    // A file appearing or disappearing under an `import.meta.glob` pattern changes the
    // importer's expansion.
    if change.kind != ChangeKind::Modified {
        let globbing: Vec<ModuleId> = shared
            .glob_watches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, ws)| ws.iter().any(|w| w.matches(path)))
            .map(|(&id, _)| id)
            .collect();
        for id in globbing {
            reprepare(shared, id, timestamp, timing, out);
        }
    }
    // Modules whose transform read this file (types behind `parse<T>`, plugin modules
    // like `x.svg?react`): re-prepared and propagated as if they had changed themselves.
    let (mut dependents, mut own) = {
        let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        (g.dependents(path), g.by_file(path))
    };
    // Plugins' `handleHotUpdate` may narrow the modules to update (empty: handled).
    if change.kind != ChangeKind::Removed
        && let Some(host) = shared
            .plugins
            .as_deref()
            .filter(|_| shared.plugin_hooks.hot_update)
    {
        let t = Instant::now();
        let urls: Vec<String> = {
            let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            own.iter()
                .chain(&dependents)
                .map(|&i| g.node(i).url.clone())
                .collect()
        };
        let keep = host.handle_hot_update(path, &urls);
        timing.transform += t.elapsed();
        match keep {
            Ok(Some(keep)) => {
                let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
                let kept = |i: &ModuleId| keep.contains(&g.node(*i).url);
                own = own.filter(kept);
                dependents.retain(kept);
                if own.is_none() && dependents.is_empty() {
                    return;
                }
            }
            Ok(None) => {}
            Err(message) => shared.log(&format!("handleHotUpdate failed: {message}")),
        }
    }
    // A file a page's `<style>` reads or references: the page reloads.
    let page = shared
        .html_deps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, deps)| deps.binary_search(path).is_ok())
        .map(|(page, _)| page.clone())
        .min();
    if let Some(page) = page {
        let page_url = server::url_for_file(&shared.root, &page);
        out.files.push(url.clone());
        out.full_reload = Some(Some(page_url));
        return;
    }
    // A page reloads, unless it is also a module (`import x from './a.html?raw'`).
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if (ext == "html" || ext == "htm") && path.starts_with(&shared.root) && own.is_none() {
        out.files.push(url.clone());
        out.full_reload = Some(Some(url));
        return;
    }
    for d in dependents {
        reprepare(shared, d, timestamp, timing, out);
    }
    let Some(id) = own else {
        if change.kind == ChangeKind::Created {
            *retry_errored = true;
        }
        return;
    };
    // The page no longer imports it (often a late event from before the import was removed).
    // Nothing to send, but an import restored later must fetch the new file, not reuse the
    // browser's instance of the old URL.
    {
        let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        if g.node(id).pruned {
            g.node_mut(id).last_hmr_timestamp = timestamp;
            return;
        }
    }
    if change.kind == ChangeKind::Removed {
        out.files.push(url);
        out.full_reload = Some(None);
        return;
    }

    let t = Instant::now();
    let own_url = {
        let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        g.node(id).url.clone()
    };
    let Ok(source) = server::module_source(shared, path, &own_url) else {
        return;
    };
    let hash = Key::of(&source);
    timing.read += t.elapsed();

    let (kind, unchanged, served_as_js, linked_css, node_url) = {
        let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.node(id);
        let same = !n.errored && n.prepared.as_ref().is_some_and(|p| p.hash == hash);
        (n.kind, same, n.served_as_js, n.linked_css, n.url.clone())
    };
    if unchanged {
        return; // touch, or the second of a double write
    }
    out.files.push(node_url.clone());

    match kind {
        ModuleKind::Script => {
            let t = Instant::now();
            let result = server::prepare_script(shared, id, path, &source, hash);
            timing.transform += t.elapsed();
            match result {
                Ok((_, pruned)) => {
                    let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
                    out.pruned
                        .extend(pruned.into_iter().map(|p| g.node(p).url.clone()));
                }
                Err(e) => {
                    shared
                        .graph
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .node_mut(id)
                        .errored = true;
                    report_error(shared, &e);
                    out.errored = true;
                    return;
                }
            }
        }
        ModuleKind::Css | ModuleKind::Asset => {
            let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            server::set_hash_only(g.node_mut(id), hash);
        }
    }

    if kind == ModuleKind::Css {
        css_changed(
            shared,
            id,
            &node_url,
            (linked_css, served_as_js),
            timestamp,
            timing,
            out,
        );
        return;
    }
    let t = Instant::now();
    propagate(shared, id, false, timestamp, out);
    timing.graph += t.elapsed();
}

/// A stylesheet changed (its file, or one it imports or references): a `<link>`ed one is
/// swapped in place, one imported from JS updates like a module.
fn css_changed(
    shared: &Shared,
    id: ModuleId,
    url: &str,
    (linked, as_js): (bool, bool),
    timestamp: u64,
    timing: &mut UpdateTiming,
    out: &mut Outcome,
) {
    let t = Instant::now();
    if linked {
        out.updates.push(json!({
            "type": "css-update",
            "path": url,
            "acceptedPath": url,
            "timestamp": timestamp,
            "explicitImportRequired": false,
            "isWithinCircularImport": false,
        }));
    }
    if as_js {
        propagate(shared, id, false, timestamp, out);
    }
    timing.graph += t.elapsed();
}

/// Re-prepares script module `id` from its current source (a file its transform read
/// changed) and propagates the update.
fn reprepare(
    shared: &Shared,
    id: ModuleId,
    timestamp: u64,
    timing: &mut UpdateTiming,
    out: &mut Outcome,
) {
    if out.full_reload.is_some() {
        return;
    }
    let (file, url, kind, css_use, pruned) = {
        let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.node(id);
        let css_use = (n.linked_css, n.served_as_js);
        (n.file.clone(), n.url.clone(), n.kind, css_use, n.pruned)
    };
    if kind == ModuleKind::Css && !pruned {
        out.files.push(url.clone());
        css_changed(shared, id, &url, css_use, timestamp, timing, out);
        return;
    }
    if kind != ModuleKind::Script {
        return;
    }
    let t = Instant::now();
    let Ok(source) = server::module_source(shared, &file, &url) else {
        return;
    };
    let hash = Key::of(&source);
    timing.read += t.elapsed();
    let t = Instant::now();
    let (before, errored) = {
        let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.node(id);
        (n.prepared.clone(), n.errored)
    };
    let result = server::prepare_script(shared, id, &file, &source, hash);
    timing.validators += t.elapsed();
    match result {
        Ok((prepared, pruned)) => {
            // Same output (the read file changed in a way this module doesn't see): no update,
            // unless the module is recovering from an error the page still shows.
            if !errored
                && before.is_some_and(|b| {
                    b.output == prepared.output && b.asset_urls == prepared.asset_urls
                })
            {
                return;
            }
            out.files.push(url);
            let g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
            out.pruned
                .extend(pruned.into_iter().map(|p| g.node(p).url.clone()));
        }
        Err(e) => {
            shared
                .graph
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .node_mut(id)
                .errored = true;
            report_error(shared, &e);
            out.files.push(url);
            out.errored = true;
            return;
        }
    }
    let t = Instant::now();
    propagate(shared, id, false, timestamp, out);
    timing.graph += t.elapsed();
}

/// Runs propagation for `id` and appends the resulting updates (or a full reload).
fn propagate(shared: &Shared, id: ModuleId, skip_self: bool, timestamp: u64, out: &mut Outcome) {
    let mut g = shared.graph.lock().unwrap_or_else(|e| e.into_inner());
    let p = g.propagate(id, skip_self);
    if let Some(reason) = &p.full_reload {
        let why = match reason {
            FullReload::NoBoundary { root } => {
                format!("no HMR boundary (reached {})", g.node(*root).url)
            }
            FullReload::Declined { module } => format!("{} declined updates", g.node(*module).url),
        };
        drop(g);
        shared.log(&format!(
            "page reload: {} changed, {why}",
            out.files.join(", ")
        ));
        out.full_reload = Some(None);
        return;
    }
    g.apply_timestamp(&p, timestamp);
    for b in &p.boundaries {
        let via = g.node(b.accepted_via);
        let explicit = matches!(via.kind, ModuleKind::Css | ModuleKind::Asset);
        // One update per representation the boundary accepts (`./x.txt?raw`, `./x.txt`).
        let mut accepted: Vec<String> = g
            .node(b.boundary)
            .prepared
            .iter()
            .flat_map(|prep| &prep.accept_targets)
            .filter_map(|t| match t {
                Target::Module { id, query } if *id == b.accepted_via => {
                    Some(server::accepted_url(&via.url, query))
                }
                _ => None,
            })
            .collect();
        accepted.sort();
        accepted.dedup();
        if accepted.is_empty() {
            accepted.push(via.url.clone()); // self-accepting
        }
        for accepted_path in accepted {
            let update = json!({
                "type": "js-update",
                "path": g.node(b.boundary).url,
                "acceptedPath": accepted_path,
                "timestamp": timestamp,
                "explicitImportRequired": explicit,
                "isWithinCircularImport": p.within_circular,
            });
            if !out.updates.contains(&update) {
                out.updates.push(update);
            }
        }
    }
}

/// Sends what a batch (or an invalidation) produced and records its timing.
fn finish(shared: &Shared, out: Outcome, mut timing: UpdateTiming) {
    let t = Instant::now();
    timing.files = out.files.clone();
    if let Some(path) = out.full_reload {
        let msg = match &path {
            Some(p) => json!({ "type": "full-reload", "path": p }),
            None => json!({ "type": "full-reload" }),
        };
        // Cleared first: a client joining after the reload goes out must not get the old error.
        clear_error(shared);
        shared.hub.broadcast(&msg.to_string());
        timing.push = t.elapsed();
        timing.outcome = "full-reload".into();
        let t = shared.timings.finished(timing);
        shared.log(&t.log_line());
        return;
    }
    if out.updates.is_empty() {
        timing.outcome = if out.errored { "error" } else { "no-op" }.into();
        let t = shared.timings.finished(timing);
        if out.errored {
            shared.log(&t.log_line());
        }
        return;
    }
    clear_error(shared);
    let kinds: Vec<&str> = out
        .updates
        .iter()
        .filter_map(|u| u["type"].as_str())
        .collect();
    let msg = json!({ "type": "update", "updates": out.updates, "tsrId": timing.id });
    let sent = shared.hub.broadcast(&msg.to_string());
    if !out.pruned.is_empty() {
        shared
            .hub
            .broadcast(&json!({ "type": "prune", "paths": out.pruned }).to_string());
    }
    timing.push = t.elapsed();
    timing.outcome = if kinds.iter().all(|k| *k == kinds[0]) {
        kinds[0].to_owned()
    } else {
        kinds.join("+")
    };
    if sent > 0 {
        shared.timings.sent(timing);
    } else {
        let t = shared.timings.finished(timing);
        shared.log(&t.log_line());
    }
}

/// Messages from browsers.
pub(crate) fn on_client_message(shared: &Shared, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if v["type"] != "custom" {
        return;
    }
    match v["event"].as_str() {
        Some("ts-r:applied") => {
            let Some(id) = v["data"]["id"].as_u64() else {
                return;
            };
            if let Some(t) = shared.timings.acknowledged(id, v["data"]["ms"].as_f64()) {
                shared.log(&t.log_line());
            }
        }
        Some("vite:invalidate") => {
            let Some(path) = v["data"]["path"].as_str() else {
                return;
            };
            invalidate(
                shared,
                path,
                v["data"]["message"].as_str(),
                v["data"]["firstInvalidatedBy"].as_str(),
            );
        }
        Some(event) => {
            // `server.ws.on(event)` listeners in plugins.
            if let Some(host) = shared
                .plugins
                .as_deref()
                .filter(|_| shared.plugin_hooks.client_events)
            {
                host.client_event(event, &v["data"]);
            }
        }
        None => {}
    }
}

/// A page's socket opened or closed: `vite:client:connect` / `vite:client:disconnect` for
/// plugins listening with `server.ws.on`.
pub(crate) fn on_client_lifecycle(shared: &Shared, connected: bool) {
    if let Some(host) = shared
        .plugins
        .as_deref()
        .filter(|_| shared.plugin_hooks.client_events)
    {
        let event = if connected {
            "vite:client:connect"
        } else {
            "vite:client:disconnect"
        };
        host.client_event(event, &Value::Null);
    }
}

/// `import.meta.hot.invalidate()`: the module can't take the update itself; hand it to
/// its importers. `first` is the module that started a chain of invalidations (an accept
/// callback that ran for an invalidation and invalidated again); an update that would
/// re-run it is a loop, so the page reloads instead.
fn invalidate(shared: &Shared, url: &str, message: Option<&str>, first: Option<&str>) {
    let _serial = shared.change_lock.lock().unwrap_or_else(|e| e.into_inner());
    let Some(id) = shared
        .graph
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .by_url(url)
    else {
        return;
    };
    shared.log(&format!(
        "invalidate {url}{}",
        message.map(|m| format!(": {m}")).unwrap_or_default()
    ));
    let timing = UpdateTiming {
        id: shared.update_seq.fetch_add(1, Ordering::Relaxed),
        origin: Some(SystemTime::now()),
        ..Default::default()
    };
    let timestamp = server::next_timestamp(shared);
    let mut out = Outcome {
        files: vec![url.to_owned()],
        ..Default::default()
    };
    let t = Instant::now();
    propagate(shared, id, true, timestamp, &mut out);
    mark_invalidation_chain(shared, &mut out, first.unwrap_or(url));
    let mut timing = timing;
    timing.graph = t.elapsed();
    finish(shared, out, timing);
}

/// Tags an invalidation's updates with the module that started the chain, or turns them
/// into a page reload when one of them would re-run that module.
fn mark_invalidation_chain(shared: &Shared, out: &mut Outcome, first: &str) {
    if out.full_reload.is_some() {
        return;
    }
    if out.updates.iter().any(|u| u["path"] == first) {
        shared.log(&format!("page reload: invalidation loop back to {first}"));
        out.updates.clear();
        out.full_reload = Some(None);
        return;
    }
    for u in &mut out.updates {
        u["firstInvalidatedBy"] = json!(first);
    }
}

/// Broadcasts an error and remembers it for clients that connect later.
pub(crate) fn report_error(shared: &Shared, err: &ModuleError) {
    let payload = err.payload();
    shared.log(&format!("error {}: {}", err.url, err.message));
    // Held while broadcasting: a client joining now gets the error once (see `websocket` in server.rs).
    let mut last = shared.last_error.lock().unwrap_or_else(|e| e.into_inner());
    shared.hub.broadcast(&payload.to_string());
    *last = Some((err.url.clone(), payload));
}

fn clear_error(shared: &Shared) {
    *shared.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A module that previously failed was served successfully.
pub(crate) fn clear_error_for(shared: &Shared, url: &str) {
    let mut e = shared.last_error.lock().unwrap_or_else(|e| e.into_inner());
    if e.as_ref().is_some_and(|(u, _)| u == url) {
        *e = None;
    }
}
