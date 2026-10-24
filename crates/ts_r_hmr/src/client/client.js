// ts-r dev client runtime. Served at /@ts-r/client (alias /@vite/client).
//
// - HMR over a WebSocket to the dev server (protocol messages are Vite's, so plugins written
//   for Vite keep working): connected, update (js-update / css-update), full-reload, prune,
//   error, custom.
// - import.meta.hot: accept (self, one dep, many deps), acceptExports, dispose, prune, data,
//   invalidate, on/off/send, decline; events vite:beforeUpdate, vite:afterUpdate,
//   vite:beforeFullReload, vite:beforePrune, vite:invalidate, vite:error,
//   vite:ws:connect, vite:ws:disconnect.
// - An error overlay (<vite-error-overlay>, the tag Vite's tests and plugins look for).
// - Latency reporting: after applying an update it sends `ts-r:applied` with the time spent.

const base = "/";
const wsPath = "/@ts-r/ws";
const pingPath = "/@ts-r/ping";
const tag = "[ts-r]";

// ---------------------------------------------------------------------------------------
// Socket

let socket;
const outbox = [];

function connect() {
  console.debug(`${tag} connecting...`);
  const proto = location.protocol === "https:" ? "wss" : "ws";
  socket = new WebSocket(`${proto}://${location.host}${wsPath}`, "vite-hmr");
  socket.addEventListener("open", () => {
    emit("vite:ws:connect", { webSocket: socket });
    flushOutbox();
  });
  socket.addEventListener("message", (event) => {
    let payload;
    try {
      payload = JSON.parse(event.data);
    } catch {
      return;
    }
    onMessage(payload, performance.now());
  });
  socket.addEventListener("close", async () => {
    emit("vite:ws:disconnect", { webSocket: socket });
    console.log(`${tag} server connection lost. Waiting for it to come back...`);
    await waitForServer();
    location.reload();
  });
}

function flushOutbox() {
  if (!socket || socket.readyState !== WebSocket.OPEN) return;
  while (outbox.length) socket.send(outbox.shift());
}

function send(message) {
  outbox.push(JSON.stringify(message));
  flushOutbox();
}

async function waitForServer() {
  for (;;) {
    await new Promise((r) => setTimeout(r, 1000));
    try {
      const res = await fetch(pingPath, { cache: "no-store" });
      if (res.ok || res.status === 204) return;
    } catch {
      // still down
    }
  }
}

// ---------------------------------------------------------------------------------------
// Message handling

let firstUpdate = true;
let chain = Promise.resolve();
let reloadPending = false;

function pageReload() {
  if (reloadPending) return;
  reloadPending = true;
  setTimeout(() => location.reload(), 20);
}

function onMessage(payload, receivedAt) {
  switch (payload.type) {
    case "connected":
      console.debug(`${tag} connected.`);
      flushOutbox();
      break;
    case "update":
      chain = chain.then(() => applyUpdate(payload, receivedAt));
      break;
    case "full-reload":
      emit("vite:beforeFullReload", payload);
      if (payload.path && payload.path.endsWith(".html")) {
        const page = decodeURI(location.pathname);
        const changed = base + payload.path.slice(1);
        const isPage =
          page === changed ||
          payload.path === "/index.html" ||
          (page.endsWith("/") && page + "index.html" === changed);
        if (isPage) pageReload();
      } else {
        pageReload();
      }
      break;
    case "prune":
      emit("vite:beforePrune", payload);
      for (const path of payload.paths) {
        const fn = pruneMap.get(path);
        if (fn) fn(dataMap.get(path));
      }
      break;
    case "error":
      emit("vite:error", payload);
      console.error(`${tag} ${payload.err.message}\n${payload.err.frame || ""}`);
      showOverlay(payload.err);
      break;
    case "custom":
      emit(payload.event, payload.data);
      break;
    case "ping":
      break;
    default:
      break;
  }
}

async function applyUpdate(payload, receivedAt) {
  emit("vite:beforeUpdate", payload);
  if (firstUpdate && hasOverlay()) {
    // The page broke while loading; nothing to patch.
    pageReload();
    return;
  }
  clearOverlay();
  firstUpdate = false;
  // Fetch every updated module in parallel, then run accept callbacks in order.
  const appliers = await Promise.all(
    payload.updates.map((u) => (u.type === "js-update" ? fetchUpdate(u) : updateLinkedCss(u))),
  );
  for (const apply of appliers) {
    if (typeof apply === "function") {
      try {
        apply();
      } catch (e) {
        console.error(e);
      }
    }
  }
  emit("vite:afterUpdate", payload);
  if (payload.tsrId != null) {
    send({
      type: "custom",
      event: "ts-r:applied",
      data: { id: payload.tsrId, ms: performance.now() - receivedAt },
    });
  }
}

// The module that started the invalidation chain the running accept callbacks belong to.
let firstInvalidatedBy;

async function fetchUpdate({ path, acceptedPath, timestamp, explicitImportRequired, isWithinCircularImport, firstInvalidatedBy: first }) {
  const mod = hotModules.get(path);
  if (!mod) return; // not loaded in this page
  const isSelf = path === acceptedPath;
  const callbacks = mod.callbacks.filter((c) => c.deps.includes(acceptedPath));
  let fresh;
  if (isSelf || callbacks.length > 0) {
    const dispose = disposeMap.get(acceptedPath);
    if (dispose) {
      disposeMap.delete(acceptedPath);
      await dispose(dataMap.get(acceptedPath));
    }
    const [file, query] = acceptedPath.split("?");
    const url =
      base + file.slice(1) + "?" + (explicitImportRequired ? "import&" : "") + "t=" + timestamp + (query ? "&" + query : "");
    try {
      fresh = await import(/* @vite-ignore */ url);
    } catch (e) {
      console.error(`${tag} failed to reload ${acceptedPath}. This could be due to syntax errors or importing non-existent modules (see errors above).`);
      console.error(e);
      if (isWithinCircularImport) pageReload();
      return;
    }
  }
  return () => {
    firstInvalidatedBy = first;
    try {
      for (const { deps, fn } of callbacks) {
        fn(deps.map((d) => (d === acceptedPath ? fresh : undefined)));
      }
    } finally {
      firstInvalidatedBy = undefined;
    }
    console.debug(`${tag} hot updated: ${isSelf ? path : `${acceptedPath} via ${path}`}`);
  };
}

const staleLinks = new WeakSet();

function updateLinkedCss({ path, timestamp }) {
  const links = [...document.querySelectorAll("link")].filter((el) => {
    if (staleLinks.has(el) || !/\bstylesheet\b/i.test(el.rel)) return false;
    try {
      return new URL(el.href, location.href).pathname === path;
    } catch {
      return false;
    }
  });
  return Promise.all(
    links.map(
      (el) =>
        new Promise((resolve) => {
          staleLinks.add(el);
          const next = el.cloneNode();
          // Keeps the link's own query (`?param=x`), with `t` replaced.
          const url = new URL(el.href, location.href);
          url.searchParams.delete("t");
          next.href = url.pathname + (url.search ? url.search + "&" : "?") + "t=" + timestamp;
          const done = () => {
            el.remove();
            resolve();
          };
          next.addEventListener("load", done);
          next.addEventListener("error", done);
          el.after(next);
        }),
    ),
  );
}

// ---------------------------------------------------------------------------------------
// import.meta.hot

const hotModules = new Map(); // ownerPath -> { id, callbacks: [{ deps, fn }] }
const dataMap = new Map(); // ownerPath -> data object, shared across instances
const disposeMap = new Map(); // ownerPath -> fn(data)
const pruneMap = new Map(); // ownerPath -> fn(data)
const listeners = new Map(); // event -> [fn]
const ownerListeners = new Map(); // ownerPath -> Map(event -> [fn]) registered by the current instance

function emit(event, data) {
  const fns = listeners.get(event);
  if (fns) for (const fn of [...fns]) fn(data);
}

function addTo(map, key, fn) {
  const list = map.get(key) || [];
  list.push(fn);
  map.set(key, list);
}

function removeFrom(map, key, fn) {
  const list = map.get(key);
  if (!list) return;
  const next = list.filter((f) => f !== fn);
  if (next.length) map.set(key, next);
  else map.delete(key);
}

export function createHotContext(ownerPath) {
  if (!dataMap.has(ownerPath)) dataMap.set(ownerPath, {});
  // A new instance of the module is executing: forget the previous instance's accept
  // callbacks and event listeners.
  const previous = hotModules.get(ownerPath);
  if (previous) previous.callbacks = [];
  const stale = ownerListeners.get(ownerPath);
  if (stale) {
    for (const [event, fns] of stale) for (const fn of fns) removeFrom(listeners, event, fn);
  }
  const mine = new Map();
  ownerListeners.set(ownerPath, mine);

  function acceptDeps(deps, fn = () => {}) {
    const mod = hotModules.get(ownerPath) || { id: ownerPath, callbacks: [] };
    mod.callbacks.push({ deps, fn });
    hotModules.set(ownerPath, mod);
  }

  return {
    get data() {
      return dataMap.get(ownerPath);
    },
    accept(deps, callback) {
      if (typeof deps === "function" || deps === undefined) {
        acceptDeps([ownerPath], ([mod]) => deps && deps(mod));
      } else if (typeof deps === "string") {
        acceptDeps([deps], ([mod]) => callback && callback(mod));
      } else if (Array.isArray(deps)) {
        acceptDeps(deps, callback);
      } else {
        throw new Error("invalid import.meta.hot.accept() usage");
      }
    },
    acceptExports(_names, callback) {
      acceptDeps([ownerPath], ([mod]) => callback && callback(mod));
    },
    dispose(callback) {
      disposeMap.set(ownerPath, callback);
    },
    prune(callback) {
      pruneMap.set(ownerPath, callback);
    },
    // The server reads decline() statically and turns updates that reach this module into
    // full reloads; nothing to do at runtime.
    decline() {},
    invalidate(message) {
      emit("vite:invalidate", { path: ownerPath, message });
      send({ type: "custom", event: "vite:invalidate", data: { path: ownerPath, message, firstInvalidatedBy } });
      console.debug(`${tag} invalidate ${ownerPath}${message ? `: ${message}` : ""}`);
    },
    on(event, fn) {
      addTo(listeners, event, fn);
      addTo(mine, event, fn);
    },
    off(event, fn) {
      removeFrom(listeners, event, fn);
      removeFrom(mine, event, fn);
    },
    send(event, data) {
      send({ type: "custom", event, data });
    },
  };
}

// ---------------------------------------------------------------------------------------
// CSS modules served as JS

const styles = new Map();

export function updateStyle(id, content) {
  let el = styles.get(id);
  if (!el) {
    el = document.createElement("style");
    el.setAttribute("type", "text/css");
    el.setAttribute("data-vite-dev-id", id);
    el.textContent = content;
    document.head.appendChild(el);
    styles.set(id, el);
  } else {
    el.textContent = content;
  }
}

export function removeStyle(id) {
  const el = styles.get(id);
  if (el) {
    el.remove();
    styles.delete(id);
  }
}

export function injectQuery(url, query) {
  if (url[0] !== "." && url[0] !== "/") return url;
  const [path, rest] = url.split("?", 2);
  return `${path}?${query}${rest ? "&" + rest : ""}`;
}

// ---------------------------------------------------------------------------------------
// Error overlay

const overlayTag = "vite-error-overlay";

const overlayStyle = `
:host { position: fixed; inset: 0; z-index: 99999; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.66); overflow-y: auto; display: flex; align-items: flex-start; justify-content: center; }
.window { margin: 48px 16px; max-width: 960px; width: 100%; background: #181818; color: #d8d8d8; border-top: 6px solid #ff5555; border-radius: 6px; padding: 24px 28px; box-shadow: 0 16px 48px rgba(0,0,0,.5); }
.plugin { color: #cfa8ff; font-size: 13px; }
.message { color: #ff7a7a; font-size: 16px; font-weight: 600; line-height: 1.4; white-space: pre-wrap; margin: 8px 0 12px; }
.file { color: #8fd2ff; font-size: 13px; margin-bottom: 12px; }
pre { margin: 0 0 12px; padding: 12px 14px; background: #232323; border-radius: 4px; overflow-x: auto; font-size: 13px; line-height: 1.5; }
.frame { color: #f2d388; }
.stack { color: #9a9a9a; }
.tip { color: #9a9a9a; font-size: 12px; border-top: 1px dotted #444; padding-top: 10px; }
pre:empty { display: none; }
`;

const OverlayBase = typeof HTMLElement === "undefined" ? class {} : HTMLElement;

export class ErrorOverlay extends OverlayBase {
  constructor(err) {
    super();
    this.root = this.attachShadow({ mode: "open" });
    this.root.innerHTML = `<style>${overlayStyle}</style>
<div class="backdrop" part="backdrop"><div class="window" part="window">
<div class="plugin"></div><div class="message"></div><div class="file"></div>
<pre class="frame"></pre><pre class="stack"></pre>
<div class="tip">Click outside or press Esc to close. Fix the code and save to continue; the page updates automatically.</div>
</div></div>`;
    const set = (sel, text) => (this.root.querySelector(sel).textContent = text || "");
    set(".plugin", err.plugin ? `[plugin:${err.plugin}]` : "");
    set(".message", err.message);
    const loc = err.loc;
    set(".file", loc && loc.file ? `${loc.file}${loc.line ? `:${loc.line}:${loc.column || 0}` : ""}` : err.id || "");
    set(".frame", err.frame);
    set(".stack", err.stack);
    this.root.querySelector(".backdrop").addEventListener("click", (e) => {
      if (e.target === e.currentTarget) this.close();
    });
    this.onKey = (e) => {
      if (e.key === "Escape") this.close();
    };
    document.addEventListener("keydown", this.onKey);
  }

  close() {
    document.removeEventListener("keydown", this.onKey);
    if (this.parentNode) this.parentNode.removeChild(this);
  }
}

if (typeof customElements !== "undefined" && !customElements.get(overlayTag)) {
  customElements.define(overlayTag, ErrorOverlay);
}

function hasOverlay() {
  return document.querySelectorAll(overlayTag).length > 0;
}

function clearOverlay() {
  document.querySelectorAll(overlayTag).forEach((el) => el.close());
}

function showOverlay(err) {
  const show = () => {
    clearOverlay();
    document.body.appendChild(new ErrorOverlay(err));
  };
  if (document.body) show();
  else document.addEventListener("DOMContentLoaded", show, { once: true });
}

connect();
