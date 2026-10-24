// Numbers for the landing page and the tool pages, read from data/bench.json (a copy of the
// benchmark data the docs site publishes, with each result's method). Nothing here is typed in by
// hand: change a number by updating bench.json.
import bench from '@/data/bench.json';
import conformance from '@/data/conformance.json';

export type Row = { label: string; value: number; kind: 'ours' | 'other' | 'floor' | string; display?: string; note?: string; extra?: string[] };
export type Chart = {
  id?: string;
  title: string;
  unit: string;
  better?: string;
  rows: Row[];
  max?: number;
  /** A reference line, never a bar. */
  target?: { value: number; label: string };
  verdict?: { ok: boolean; text: string };
  valueLabel?: string;
  /** Headers for each row's `extra` values. */
  columns?: string[];
};
export type Table = { columns: string[]; rows: string[][] };
export type Method = { measured: string; machine: string; software: string; input: string; command?: string; commit: string; sources?: string[] };
export type Section = {
  id: string;
  /** The /benchmarks/ page's contents label. */
  nav?: string;
  title: string;
  intro: string;
  charts?: Chart[];
  table?: Table;
  /** "conformance": the table is data/conformance.json's rows marked `home`. */
  tableFrom?: 'conformance';
  tableTitle?: string;
  method?: Method;
  notes?: string;
  status?: { ok: boolean; text: string };
};

type Bench = { updated: string; about: string; sections: Section[]; machines: Record<string, string>; notMeasured: string[] };
const data = bench as unknown as Bench;

export const benchUpdated = data.updated;
export const benchAbout = data.about;
export const notMeasured = data.notMeasured;
/** Every section, in bench.json order, with machines resolved (the /benchmarks/ page). */
export const sections = () => data.sections.map(s => section(s.id));

// A section with `"tableFrom": "conformance"` (the correctness table) has no rows in bench.json:
// its table is the data/conformance.json rows marked `home`, the same quote-checked rows the status page shows, so the two can't disagree.
type ConformanceRow = { area: string; against: string; result: string; source?: string; quote?: string; commit?: string; home?: string };
const correctnessRows = (conformance as { rows: ConformanceRow[] }).rows
  .filter(r => r.home)
  .map(r => [r.home!, r.result, `${r.against}. ${r.commit ? `Commit ${r.commit}.` : `Recorded in ${r.source}.`}`]);

export function section(id: string): Section {
  const found = data.sections.find(x => x.id === id);
  if (!found) throw new Error(`data/bench.json has no section "${id}"`);
  const s = found.tableFrom === 'conformance' ? { ...found, table: { columns: ['What', 'Result', 'Measured on'], rows: correctnessRows } } : found;
  // Resolve the machine key to its description once, here.
  return s.method ? { ...s, method: { ...s.method, machine: data.machines[s.method.machine] ?? s.method.machine } } : s;
}

function findChart(sectionId: string, titleStart: string): Chart | undefined {
  return data.sections.find(x => x.id === sectionId)?.charts?.find(x => x.title.startsWith(titleStart));
}

export function chart(sectionId: string, titleStart: string): Chart {
  const c = findChart(sectionId, titleStart);
  if (!c) throw new Error(`data/bench.json has no "${titleStart}" chart in ${sectionId}`);
  return c;
}

/** Rows of the correctness table, as { what, result, on }. */
export function correctness(): { what: string; result: string; on: string }[] {
  return (section('correctness').table?.rows ?? []).map(([what, result, on]) => ({ what, result, on }));
}

export type Tile = {
  tool: string;
  replaces: string;
  what: string;
  ratio: string;
  ours: string;
  other: string;
  oursName: string;
  otherName: string;
  /** Our time as a fraction of theirs (1 / speed-up), for drawing the bars. */
  share: number;
  measure: string;
  method: Method;
};

// One headline tile per tool, each drawn from a like-for-like chart (same input, same work on
// both sides). A tool without such a chart has no tile: ts-r-build's excalidraw runs skip the
// 61 locale chunks esbuild emits, so they are in the build section's table instead.
const TILES = [
  { tool: 'ts-r-dev', replaces: 'the Vite dev server', section: 'dev-vs-vite', chart: 'Save to page updated', ours: 'ts-r-dev', other: 'Vite 8.3.4', what: 'faster hot reload' },
  { tool: 'ts-r-check', replaces: 'tsc', section: 'check-linux', chart: 'Check three projects', ours: 'ts-r-check', other: 'tsc 7.0.2', what: 'faster type checks' },
  { tool: 'ts-r-lint', replaces: 'ESLint + typescript-eslint', section: 'lint-same-rules', chart: 'tRPC, same 48 rules', ours: 'ts-r-lint, config not cached', other: 'ESLint 10.10.0', what: 'faster linting' },
  { tool: 'ts-r-run', replaces: 'tsx, ts-node', section: 'run', chart: 'Run a 1,000-module', ours: 'ts-r-run, warm cache', other: 'tsx 4.23', what: 'faster runs' },
  { tool: 'ts-r-lsp', replaces: 'tsserver and the ESLint server', section: 'editor', chart: 'tRPC router.ts: first diagnostics', ours: 'ts-r-lsp', other: 'tsserver', what: 'faster first errors' },
];

const num = (n: number) => n.toLocaleString('en-US', { maximumFractionDigits: 2 });

// [low, high]: `display` holds ranges such as "0.01–0.04 s".
export function range(row: Row): [number, number] {
  const m = /^([\d.,]+)\s*[–-]\s*([\d.,]+)/.exec(row.display ?? '');
  if (m) return [Number(m[1].replace(/,/g, '')), Number(m[2].replace(/,/g, ''))];
  return [row.value, row.value];
}

export const shown = (row: Row, unit: string) => row.display ?? (unit === '%' ? `${num(row.value)}%` : `${num(row.value)} ${unit}`);

/**
 * The headline tiles, in TILES order. A tile whose chart or rows are no longer in bench.json is
 * left out (with a build warning) rather than failing the build: a measurement moved to a table
 * must not take the home page down. Pages lay out whatever number of tiles this returns.
 */
export function tiles(): Tile[] {
  return TILES.flatMap((t): Tile | Tile[] => {
    const c = findChart(t.section, t.chart);
    const ours = c?.rows.find(r => r.label === t.ours);
    const other = c?.rows.find(r => r.label === t.other);
    const s = c && section(t.section);
    if (!c || !s?.method || !ours || !other) {
      console.warn(`landing: no ${t.tool} tile: data/bench.json has no "${t.ours}" and "${t.other}" rows with a method in ${t.section} / "${t.chart}"`);
      return [];
    }
    // Conservative for ranges: our slowest run against their fastest.
    const better = c.better === 'higher' ? range(ours)[0] / range(other)[1] : range(other)[0] / range(ours)[1];
    return {
      tool: t.tool,
      replaces: t.replaces,
      what: t.what,
      ratio: better >= 10 ? Math.round(better).toString() : better.toFixed(1),
      ours: shown(ours, c.unit),
      other: shown(other, c.unit),
      oursName: t.ours,
      otherName: t.other,
      share: 1 / better,
      measure: c.title,
      method: s.method,
    };
  });
}

export const tileFor = (tool: string) => tiles().find(t => t.tool === tool);

/** The hot-reload target in ms, from the hmr section's intro ("target is under 30 ms"). */
export function hmrTarget(): number {
  const m = /target is under (\d+) ms/.exec(section('hmr').intro);
  if (!m) throw new Error('data/bench.json: the hmr intro no longer states the target as "target is under N ms"');
  return Number(m[1]);
}
