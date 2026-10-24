// What a bench run can skip, with no I/O so it can be tested (bench-reuse.test.cjs): which of
// our binaries the scenarios run (only those are built), and which reference tools (tsc, ESLint,
// esbuild, ...) a later run may take from an earlier one instead of measuring them again. The
// box's job steps (box/job.mjs) use it; the dispatcher keeps the store.
'use strict';

// The binaries `bench/scenarios.toml` runs, from its text: every `{bin}/<name>` anywhere in it
// (commands, setups, `requires`), and `ts-r-<sub>` for the launcher's `{bin}/ts-r <sub>`. The
// examples (`{bin}/examples/...`) are left out: the box never built them, so their scenarios are
// skipped as before. A scenario whose binary is not built is skipped (its `requires`), never
// wrong, so this errs on the side of too many.
function scanBins(toml) {
  const bins = new Set();
  for (const [, name, sub] of String(toml).matchAll(/\{bin\}\/((?:examples\/)?[A-Za-z0-9_-]+)(?:[ \t]+([a-z][a-z0-9-]*))?/g)) {
    if (name.startsWith('examples/')) continue;
    bins.add(name);
    if (name === 'ts-r' && sub) bins.add(`ts-r-${sub}`);
  }
  return [...bins].sort();
}

// The reference tools whose numbers do not depend on our binaries, as "<scenario id>\n<tool>"
// keys. A scenario that mentions `{bin}` outside its own tools (a setup that asks ts-r-lint for
// its rules, say) has none, and neither has a reference tool that mentions `{bin}` itself. Read
// line by line, so anything unusual counts against reuse rather than for it.
function reusableTools(toml) {
  const keys = new Set();
  let scenario = null;
  let tool = null;
  const close = () => {
    if (scenario && tool && scenario.id && tool.name && !scenario.bin && !tool.ours && !tool.bin) keys.add(`${scenario.id}\n${tool.name}`);
    tool = null;
  };
  for (const line of String(toml).split('\n')) {
    const header = /^\s*\[\[?\s*([^\]]+?)\s*\]\]?\s*(#.*)?$/.exec(line);
    if (header) {
      close();
      if (header[1] === 'scenario') scenario = { id: null, bin: false };
      else if (header[1] === 'scenario.tool' && scenario) tool = { name: null, ours: false, bin: false };
      else scenario = null;
      continue;
    }
    if (!scenario) continue;
    const at = tool ?? scenario;
    if (line.includes('{bin}')) at.bin = true;
    const value = /^\s*(id|name|ours)\s*=\s*(?:"([^"]*)"|(true|false))\s*(#.*)?$/.exec(line);
    if (!value) continue;
    if (value[1] === 'id' && !tool) scenario.id = value[2] ?? null;
    if (value[1] === 'name' && tool) tool.name = value[2] ?? null;
    if (value[1] === 'ours' && tool) tool.ours = value[3] !== 'false';
  }
  close();
  return keys;
}

const key = (scenario, tool) => `${scenario.id}\n${tool.name}`;

// The reusable reference measurements of a full run (a results file of `cargo xtask bench`), to
// store: [{ id, tools: [tool result] }]. Null when one of them failed, so a bad run is not
// reused for days; a skipped one (a missing requirement) is simply not stored.
function referenceOf(results, reusable) {
  const out = [];
  for (const scenario of results?.scenarios ?? []) {
    const tools = [];
    for (const tool of scenario.tools ?? []) {
      if (tool.ours || !reusable.has(key(scenario, tool))) continue;
      if (tool.status === 'failed') return null;
      if (tool.status === 'ok' && tool.stats) tools.push(tool);
    }
    if (tools.length) out.push({ id: scenario.id, tools });
  }
  return out;
}

// An ours-only run with the stored reference tools put back in the places the run left for them
// (the reference tools it skipped for --ours-only). Nothing else changes: ours stay as measured.
// `missing` replaces the reason of a reusable tool the store has no measurement of.
const OURS_ONLY = 'reference tool (--ours-only)';
function mergeReference(results, stored, reusable, missing) {
  const byKey = new Map();
  for (const scenario of stored ?? []) for (const tool of scenario.tools ?? []) byKey.set(`${scenario.id}\n${tool.name}`, tool);
  let reused = 0;
  const scenarios = (results?.scenarios ?? []).map((scenario) => ({
    ...scenario,
    tools: (scenario.tools ?? []).map((tool) => {
      if (tool.ours || tool.status !== 'skipped' || tool.reason !== OURS_ONLY || !reusable.has(key(scenario, tool))) return tool;
      const cached = byKey.get(key(scenario, tool));
      if (!cached) return { ...tool, reason: missing };
      reused++;
      const { status, stats, max_rss_mb } = cached;
      return { name: tool.name, ours: false, status, stats, ...(max_rss_mb === undefined ? {} : { max_rss_mb }) };
    }),
  }));
  return { results: { ...results, scenarios }, reused };
}

// The tables of the merged run (`rendered`, from `cargo xtask bench --compare`) with the gate
// paragraph of the run itself (`original`), which is the one that counted: a re-render compares
// as a confirming re-run would, and reference tools never enter the gate anyway. Null if either
// is not shaped as expected, and the caller keeps the original.
const GATE = '\nGate: ';
function spliceTables(rendered, original, note) {
  const at = String(rendered).indexOf(GATE);
  const from = String(original).indexOf(GATE);
  if (at < 0 || from < 0 || !String(rendered).startsWith('### ')) return null;
  return `${String(rendered).slice(0, at)}${String(original).slice(from).replace(/\n*$/, '\n')}\n${note}\n`;
}

module.exports = { scanBins, reusableTools, referenceOf, mergeReference, spliceTables, OURS_ONLY };
