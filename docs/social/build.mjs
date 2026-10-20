// Builds the X/Twitter benchmark cards (1600x900) from docs/metrics.md numbers.
// Usage: node docs/social/build.mjs   (writes docs/social/*.html; render.sh turns them into PNGs)
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

// Site tokens (landing/app/globals.css). ours = --ours, theirs = --theirs, as on the site.
const C = {
  bg: "#0d0f13", card: "#14171d", track: "#1d2129", line: "rgba(230,232,236,.11)",
  ink: "#e6e8ec", ink2: "#a9b0bd", muted: "#8b93a1",
  ours: "#d95926", theirs: "#3987e5", brand: "#f0814a", brand2: "#7aa2ff",
};

const FOOT = "Measured on a real TypeScript monorepo (1,983 modules) · Apple Silicon, 16 cores · pre-alpha, Oct 2026";

const logo = `<svg width="44" height="44" viewBox="0 0 72 72"><defs><linearGradient id="lg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${C.brand}"/><stop offset="1" stop-color="${C.brand2}"/></linearGradient></defs><rect width="72" height="72" rx="18" fill="url(#lg)"/><path d="M24.75 22.5 L13.5 36 L24.75 49.5 M47.25 22.5 L58.5 36 L47.25 49.5 M40.5 20.25 L31.5 51.75" stroke="${C.bg}" stroke-width="5.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

function page(body, { legend = false, foot = FOOT } = {}) {
  return `<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box;margin:0;padding:0}
html,body{width:1600px;height:900px;overflow:hidden}
body{background:${C.bg};color:${C.ink};font-family:Inter,-apple-system,"SF Pro Display",system-ui,sans-serif;
  background-image:radial-gradient(900px 600px at 8% -10%,rgba(240,129,74,.16),transparent 70%),
  radial-gradient(900px 600px at 100% 0%,rgba(122,162,255,.12),transparent 70%);
  padding:56px 80px 0;display:flex;flex-direction:column}
.mono{font-family:"JetBrains Mono",ui-monospace,Menlo,monospace}
header{display:flex;align-items:center;gap:16px;height:44px}
.brand{font-size:30px;font-weight:800;letter-spacing:-.02em}
.pill{font-size:15px;font-weight:600;color:#fbbf24;border:1px solid rgba(251,191,36,.5);background:rgba(251,191,36,.1);border-radius:99px;padding:4px 12px}
.legend{margin-left:auto;display:flex;gap:28px;font-size:20px;color:${C.ink2}}
.legend span{display:flex;align-items:center;gap:10px}
.sw{width:18px;height:18px;border-radius:4px}
h1{font-size:60px;font-weight:800;letter-spacing:-.025em;line-height:1.05;margin-top:40px}
h1 .g{background:linear-gradient(90deg,${C.brand},#ffb37a 45%,${C.brand2});-webkit-background-clip:text;color:transparent}
.sub{font-size:24px;color:${C.ink2};margin-top:14px}
main{flex:1;min-height:0}
footer{height:64px;display:flex;align-items:center;justify-content:space-between;font-size:17px;color:${C.muted};border-top:1px solid ${C.line}}
</style></head><body>
<header>${logo}<span class="brand">ts-r</span><span class="pill">pre-alpha</span>${legend ? `<div class="legend"><span><i class="sw" style="background:${C.ours}"></i>ts-r</span><span><i class="sw" style="background:${C.theirs}"></i>The tool it replaces</span></div>` : ""}</header>
${body}
<footer><span>${foot}</span><span class="mono">ts-r · written from scratch in Rust</span></footer>
</body></html>`;
}

// 1. Headline multipliers: the job is "one number each", so stat tiles, not a chart.
const tiles = [
  ["10×", "faster hot reload", "ts-r-dev vs Vite 8", "7.7 ms vs 77.5 ms, save to screen"],
  ["59×", "faster linting", "ts-r-lint vs ESLint", "0.047 s vs 2.79 s, 690 files, cached"],
  ["5.9×", "faster rebuilds", "ts-r-build vs esbuild", "~10 ms vs 59 ms after a change"],
  ["12×", "faster first errors", "ts-r-lsp vs tsserver", "18 ms vs 218 ms on opening a file"],
  ["74×", "less editor memory", "ts-r-lint vs ESLint server", "19 MB vs 1,405 MB, 557 files open"],
  ["10×", "less LSP memory", "ts-r-lsp vs tsserver", "43 MB vs 442 MB, one file open"],
];
const hero = page(`
<h1>TypeScript tooling, <span class="g">rebuilt in Rust.</span></h1>
<p class="sub">Dev server, bundler, linter, type checker and language server, against the tools they replace.</p>
<main style="display:grid;grid-template-columns:repeat(3,minmax(0,1fr));grid-template-rows:repeat(2,minmax(0,1fr));gap:20px;padding:40px 0 36px">
${tiles.map(([n, what, who, how]) => `<div style="background:${C.card};border:1px solid ${C.line};border-radius:20px;padding:28px 32px;display:flex;flex-direction:column;justify-content:center;min-width:0">
  <div style="display:flex;align-items:baseline;gap:16px"><span style="font-size:88px;font-weight:800;letter-spacing:-.04em;line-height:1;color:${C.brand}">${n}</span><span style="font-size:26px;font-weight:700;line-height:1.15">${what}</span></div>
  <div style="font-size:21px;color:${C.ink};margin-top:16px" class="mono">${who}</div>
  <div style="font-size:18px;color:${C.muted};margin-top:6px">${how}</div>
</div>`).join("")}
</main>`);

// 2 and 3. Paired bars. `shared` draws every row on one axis (same unit); otherwise each row
// has its own scale and says so.
function bars(rows, { shared, unit }) {
  const max = shared ? Math.max(...rows.map((r) => r.theirs[1])) : 0;
  const fmt = (v) => (unit === "s" ? `${v} s` : unit === "ms" ? `${v} ms` : `${v.toLocaleString("en-US")} MB`);
  return rows.map((r) => {
    const m = shared ? max : r.theirs[1];
    const w = (v) => Math.max(0.6, (v / m) * 100);
    const bar = (color, name, v, label) => `<div style="display:flex;align-items:center;gap:16px;height:34px">
      <div style="flex:0 0 auto;width:calc(${w(v)}% * .78);height:30px;background:${color};border-radius:0 4px 4px 0"></div>
      <span style="font-size:21px;white-space:nowrap">${(label ?? fmt(v)).replace(/^([~\d.,]+) (.*)$/, '<b class="mono" style="font-weight:700">$1</b> <b style="font-weight:600">$2</b>')}<span style="color:${C.ink2}"> ${name}</span></span></div>`;
    return `<div style="display:grid;grid-template-columns:330px minmax(0,1fr) 150px;align-items:center;column-gap:28px;padding:${shared ? 30 : 18}px 0;border-top:1px solid ${C.line}">
      <div><div style="font-size:25px;font-weight:700;line-height:1.15">${r.label}</div><div style="font-size:17px;color:${C.muted};margin-top:6px">${r.note}</div></div>
      <div style="display:flex;flex-direction:column;gap:4px;border-left:2px solid ${C.muted};min-width:0">${bar(C.ours, r.ours[0], r.ours[1], r.ours[2])}${bar(C.theirs, r.theirs[0], r.theirs[1], r.theirs[2])}</div>
      <div style="text-align:right"><div style="font-size:52px;font-weight:800;letter-spacing:-.03em;line-height:1">${r.x}</div><div style="font-size:17px;color:${C.muted};margin-top:4px">${r.xnote}</div></div>
    </div>`;
  }).join("");
}

const speed = page(`
<h1>Lower is better. <span class="g">Much lower.</span></h1>
<p class="sub">Time per task, ts-r against the tool it replaces. Each row has its own scale.</p>
<main style="padding-top:26px">${bars([
  { label: "Hot reload", note: "save to updated screen, p50", ours: ["ts-r-dev", 7.7], theirs: ["Vite 8.3", 77.5], x: "10×", xnote: "faster", unit: "ms" },
  { label: "Lint 690 files", note: "same rules, config cached", ours: ["ts-r-lint", 0.047, "0.047 s"], theirs: ["ESLint", 2.79, "2.79 s"], x: "59×", xnote: "faster" },
  { label: "Rebuild", note: "after one file changes", ours: ["ts-r-build", 10, "~10 ms"], theirs: ["esbuild 0.28", 59, "59 ms"], x: "5.9×", xnote: "faster" },
  { label: "First errors", note: "opening a file in the editor", ours: ["ts-r-lsp", 18, "18 ms"], theirs: ["tsserver (TS 6)", 218, "218 ms"], x: "12×", xnote: "faster" },
  { label: "Production build", note: "minified, 1,983 modules", ours: ["ts-r-build", 65, "~65 ms"], theirs: ["esbuild 0.28", 86, "~86 ms"], x: "1.3×", xnote: "faster" },
], { shared: false, unit: "ms" })}</main>`, { legend: true });

const memory = page(`
<h1>A fraction of the <span class="g">memory.</span></h1>
<p class="sub">Peak memory, ts-r against the tool it replaces. One shared scale.</p>
<main style="padding-top:26px">${bars([
  { label: "Linting in the editor", note: "557 files open", ours: ["ts-r-lint", 19], theirs: ["ESLint server", 1405], x: "74×", xnote: "less" },
  { label: "Language server", note: "whole project open, 649 files", ours: ["ts-r-lsp", 410, "~410 MB"], theirs: ["tsserver (TS 6)", 1132], x: "2.8×", xnote: "less" },
  { label: "Language server", note: "one file open", ours: ["ts-r-lsp", 43], theirs: ["tsserver (TS 6)", 442], x: "10×", xnote: "less" },
  { label: "Dev server", note: "running, after 50 edits", ours: ["ts-r-dev", 25], theirs: ["Vite 8.3", 190, "~190 MB"], x: "7.6×", xnote: "less" },
], { shared: true, unit: "MB" })}</main>`, { legend: true });

// 4. Correctness: single series, so no legend; the label names each bar.
const checks = [
  ["Lint rules give ESLint's answers", "ESLint + typescript-eslint rule tests", 8597, 8597, "8,597 / 8,597"],
  ["Bundles run the same as esbuild's", "top 1,000 npm packages", 915, 915, "915 / 915"],
  ["Parser round-trips tsc's test files", "parse, print, compare", 18260, 18260, "18,260 / 18,260"],
  ["Parses the top npm packages", "top 1,000 npm packages", 988, 989, "99.9%"],
  ["Type checker matches tsc exactly", "tsc's conformance suite · in progress", 7592, 13398, "59.5%"],
];
const correct = page(`
<h1>Fast, and <span class="g">the same answers.</span></h1>
<p class="sub">Every tool is checked against the original tool's own test suite and output.</p>
<main style="padding-top:30px">${checks.map(([label, note, n, d, v]) => `<div style="display:grid;grid-template-columns:520px minmax(0,1fr) 330px;align-items:center;column-gap:32px;padding:20px 0;border-top:1px solid ${C.line}">
  <div><div style="font-size:26px;font-weight:700">${label}</div><div style="font-size:17px;color:${C.muted};margin-top:6px">${note}</div></div>
  <div style="height:30px;background:${C.track};border-radius:4px;overflow:hidden"><div style="height:100%;width:${(n / d) * 100}%;background:${C.ours};border-radius:0 4px 4px 0"></div></div>
  <div class="mono" style="text-align:right;font-size:34px;font-weight:700;white-space:nowrap">${v}</div>
</div>`).join("")}</main>`, { foot: "Oracles: ESLint + typescript-eslint rule tests, esbuild, tsc 7 · top 1,000 npm packages · pre-alpha, Oct 2026" });

for (const [name, html] of [["1-headline", hero], ["2-speed", speed], ["3-memory", memory], ["4-correctness", correct]]) {
  writeFileSync(join(here, `${name}.html`), html);
}
