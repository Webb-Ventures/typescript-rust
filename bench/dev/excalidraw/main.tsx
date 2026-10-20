// Entry for the ts-r benchmarks on excalidraw/excalidraw: renders the whole Excalidraw app
// (excalidraw-app/App.tsx and its packages, as source) like excalidraw-app/index.tsx does,
// minus the service worker registration (`virtual:pwa-register`) and Sentry. `cargo xtask
// bench --fetch-repos --repos excalidraw` copies this directory to ts-r-bench-dev/ in the
// checkout; the dev-server harness (bench/dev/dev.mjs), build-excalidraw and
// rebuild-excalidraw (bench/scenarios.toml) start from it.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import ExcalidrawApp from "../excalidraw-app/App";

window.__EXCALIDRAW_SHA__ = import.meta.env.VITE_APP_GIT_SHA;
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ExcalidrawApp />
  </StrictMode>,
);
