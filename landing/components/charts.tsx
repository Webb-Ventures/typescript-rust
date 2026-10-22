// Charts drawn from data/bench.json. Every bar carries its name and value as text, so colour
// (ours orange, others blue, a baseline grey) is never the only signal. Charts marked `.reveal`
// race when they scroll into view (components/reveal.tsx): each bar takes as long to fill as its
// measured time. They are complete and static without JavaScript or under reduced motion.
import type { Chart, Method, Section, Table } from '@/lib/bench';
import { hmrTarget, shown } from '@/lib/bench';
import { site } from '@/lib/site';
import { published } from '@/lib/commits.mjs';

const KIND_NAME: Record<string, string> = { ours: 'ts-r', other: 'Other tools', floor: 'Baseline' };

const vars = (v: Record<string, string>) => v as React.CSSProperties;

export function Legend({ kinds, target }: { kinds: string[]; target?: string }) {
  return (
    <ul className="legend" aria-label="Legend">
      {kinds.map(k => (
        <li key={k} className={`k-${k}`}><i aria-hidden="true" />{KIND_NAME[k] ?? k}</li>
      ))}
      {target && <li className="k-target"><i aria-hidden="true" />{target}</li>}
    </ul>
  );
}

// `id` (the /benchmarks/ page) anchors the chart and adds its target line, verdict and a table
// view with every row's notes and extra columns.
export function BarChart({ c, id }: { c: Chart; id?: string }) {
  const higher = c.better === 'higher';
  const target = id ? c.target : undefined;
  const max = c.max ?? Math.max(...c.rows.map(r => r.value), higher && c.unit === '%' ? 100 : 0, target?.value ?? 0);
  // The slowest bar takes 1.4 s; higher-is-better charts fill at one speed.
  const slow = 1.4;
  const kinds = [...new Set(c.rows.map(r => r.kind))].sort((a, b) => Object.keys(KIND_NAME).indexOf(a) - Object.keys(KIND_NAME).indexOf(b));
  return (
    <figure className="chart reveal" id={id}>
      <figcaption>
        <span className="chart-title">{c.title}</span>
        <span className="chart-sub">
          {higher ? 'Longer is better' : 'Shorter is faster'}{c.unit && c.unit !== '%' ? `, in ${c.unit}` : ''}
          {id && c.verdict && <> · <span className={`verdict ${c.verdict.ok ? 'ok' : 'miss'}`}>{c.verdict.text}</span></>}
        </span>
      </figcaption>
      {(kinds.length > 1 || target) && <Legend kinds={kinds} target={target?.label} />}
      <ul className={`bars chart-bars${target ? ' has-target' : ''}`} style={target ? vars({ '--t': (target.value / max).toFixed(4) }) : undefined}>
        {c.rows.map(r => {
          const w = max ? r.value / max : 0;
          return (
            <li key={r.label} className={r.kind === 'ours' ? 'ours' : r.kind === 'floor' ? 'floor' : 'theirs'} style={vars({ '--w': w.toFixed(4), '--d': `${(higher ? 0.9 : slow * w).toFixed(3)}s` })}>
              <span className="bar-name">{r.label}</span>
              <span className="bar-val">{shown(r, c.unit)}</span>
              <span className="bar" aria-hidden="true"><i /></span>
            </li>
          );
        })}
      </ul>
      {id && <ChartTable c={c} />}
    </figure>
  );
}

function ChartTable({ c }: { c: Chart }) {
  const cols = c.columns ?? [];
  const notes = c.rows.some(r => r.note);
  return (
    <details className="method-card chart-table">
      <summary>Show as table</summary>
      <div className="table-wrap">
        <table className="data">
          <caption>{c.title} ({c.unit}, {c.better === 'higher' ? 'higher is better' : 'lower is better'})</caption>
          <thead>
            <tr>
              <th scope="col">Setup</th>
              <th scope="col">{c.valueLabel ?? 'Result'}</th>
              {cols.map(h => <th key={h} scope="col">{h}</th>)}
              {notes && <th scope="col">Notes</th>}
            </tr>
          </thead>
          <tbody>
            {c.rows.map(r => (
              <tr key={r.label}>
                <th scope="row">{r.label}</th>
                <td>{shown(r, c.unit)}</td>
                {(r.extra ?? []).map((e, i) => <td key={i}>{e}</td>)}
                {notes && <td>{r.note ?? ''}</td>}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}

// Hot reload latency against project size, on a log x-axis, with the plan's 30 ms target as a
// reference line. Lines are SVG stretched to the box; points and labels are HTML so they stay
// round and legible at any width.
export function ScaleChart({ c, target, id }: { c: Chart; target: number; id?: string }) {
  const sizes = c.rows.map(r => Number(r.label.replace(/[^\d]/g, '')));
  const lo = Math.log10(sizes[0]);
  const hi = Math.log10(sizes[sizes.length - 1]);
  const yMax = Math.ceil((target * 1.2) / 10) * 10;
  const pts = c.rows.map((r, i) => ({ r, x: 6 + ((Math.log10(sizes[i]) - lo) / (hi - lo)) * 88, y: 100 - (r.value / yMax) * 100 }));
  const ticks = Array.from({ length: yMax / 10 + 1 }, (_, i) => i * 10);
  const path = pts.map((p, i) => `${i ? 'L' : 'M'}${p.x} ${p.y}`).join(' ');
  const last = pts[pts.length - 1];
  return (
    <figure className="chart scale reveal" id={id}>
      <figcaption>
        <span className="chart-title">{c.title}, as the app grows</span>
        <span className="chart-sub">Lower is faster, in ms. Generated apps from 1,000 to 50,000 modules.</span>
      </figcaption>
      <div className="scale-plot">
        <div className="scale-y" aria-hidden="true">
          {ticks.map(t => <span key={t} style={vars({ '--y': `${100 - (t / yMax) * 100}%` })}>{t}</span>)}
        </div>
        <div className="scale-area">
          <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            {ticks.map(t => <line key={t} className="grid" x1="0" x2="100" y1={100 - (t / yMax) * 100} y2={100 - (t / yMax) * 100} />)}
            <line className="target" x1="0" x2="100" y1={100 - (target / yMax) * 100} y2={100 - (target / yMax) * 100} />
            <path className="area" d={`${path} L${last.x} 100 L${pts[0].x} 100 Z`} />
            <path className="line" d={path} pathLength={1} />
          </svg>
          <span className="target-label" style={vars({ '--y': `${100 - (target / yMax) * 100}%` })}>Target: under {target} ms</span>
          <ul className="scale-points">
            {pts.map((p, i) => (
              <li key={p.r.label} style={vars({ '--x': `${p.x}%`, '--y': `${p.y}%`, '--i': String(i) })} tabIndex={0}>
                <span className="dot" aria-hidden="true" />
                <span className={`pt-label${i === pts.length - 1 ? ' end' : ''}`}>{shown(p.r, c.unit)} <span>at {p.r.label}</span></span>
              </li>
            ))}
          </ul>
        </div>
        <div className="scale-x" aria-hidden="true">
          {pts.map(p => <span key={p.r.label} style={vars({ '--x': `${p.x}%` })}>{p.r.label.replace(' modules', '')}</span>)}
        </div>
      </div>
      {id && <ChartTable c={c} />}
    </figure>
  );
}

// Counts, mismatch summaries and percentages become a fraction for a meter.
export function fraction(result: string): number | undefined {
  const pct = /^([\d.]+)%/.exec(result);
  if (pct) return Number(pct[1]) / 100;
  const n = (s: string) => Number(s.replace(/,/g, ''));
  const of = /([\d,]+) of ([\d,]+)/.exec(result);
  if (of) return n(of[1]) / n(of[2]);
  const md = /([\d,]+) match(?:es)?, ([\d,]+) diff/.exec(result);
  if (md) return n(md[1]) / (n(md[1]) + n(md[2]));
  const resolutions = /([\d,]+) resolutions, ([\d,]+) mismatches/.exec(result);
  if (resolutions) return (n(resolutions[1]) - n(resolutions[2])) / n(resolutions[1]);
  const files = /([\d,]+) files ok, ([\d,]+) mismatches/.exec(result);
  if (files) return n(files[1]) / (n(files[1]) + n(files[2]));
  return undefined;
}

// A percentage the result states itself ("646 of 746 ... (86.6%)"), so the label can't disagree with it.
const stated = (result: string) => /\(([\d.]+%)/.exec(result)?.[1];

export function Meters({ rows }: { rows: { what: string; result: string; on: string }[] }) {
  return (
    <ul className="meters reveal">
      {rows.map((r, i) => {
        const f = fraction(r.result);
        return (
          <li key={`${r.what}-${i}`} style={vars({ '--w': (f ?? 0).toFixed(4), '--i': String(i) })}>
            <div className="meter-head">
              <span className="meter-what">{r.what}</span>
              {f !== undefined && <span className="meter-pct">{f === 1 ? '100%' : (stated(r.result) ?? `${(Math.floor(f * 1000) / 10).toFixed(1)}%`)}</span>}
            </div>
            {f !== undefined && <span className={`meter${f === 1 ? ' full' : ''}`} aria-hidden="true"><i /></span>}
            <p className="meter-result">{r.result}</p>
            <p className="meter-on">{r.on}</p>
          </li>
        );
      })}
    </ul>
  );
}

export function DataTable({ t, title }: { t: Table; title?: string }) {
  return (
    <div className="table-wrap">
      <table className="data">
        {title && <caption>{title}</caption>}
        <thead><tr>{t.columns.map(c => <th key={c} scope="col">{c}</th>)}</tr></thead>
        <tbody>
          {t.rows.map((r, i) => (
            <tr key={i}>{r.map((cell, j) => (j === 0 ? <th key={j} scope="row">{cell}</th> : <td key={j}>{cell}</td>))}</tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function MethodList({ m, sources = false }: { m: Method; sources?: boolean }) {
  return (
    <dl className="method">
      <dt>Measured</dt><dd>{m.measured}</dd>
      <dt>Machine</dt><dd>{m.machine}</dd>
      <dt>Software</dt><dd>{m.software}</dd>
      <dt>Input</dt><dd>{m.input}</dd>
      {m.command && <><dt>Command</dt><dd><code className="wrap-code">{m.command}</code></dd></>}
      {sources ? (
        <>
          <dt>Recorded in</dt>
          <dd>
            {published(m.commit) ? (
              <>
                commit <a href={`${site.repo}/commit/${m.commit}`}><code>{m.commit}</code></a>
                {m.sources?.map(s => <span key={s}>, <a href={`${site.repo}/blob/${m.commit}/${s}`}><code>{s}</code></a></span>)}
              </>
            ) : (
              // A development-repository commit: the sources link to their current version.
              <>
                commit <code>{m.commit}</code>
                {m.sources?.map(s => <span key={s}>, <a href={`${site.repo}/blob/main/${s}`}><code>{s}</code></a></span>)}
              </>
            )}
          </dd>
        </>
      ) : (
        <><dt>Recorded in</dt><dd>commit <code>{m.commit}</code></dd></>
      )}
    </dl>
  );
}

// One bench.json section in full: its charts, table and method. `full` (the /benchmarks/ page)
// anchors the section at #<id> and each chart at #<id>-<chart id>, uses an h2, adds table views
// and target lines, and links the method's commit and sources.
export function BenchSection({ s, full = false }: { s: Section; full?: boolean }) {
  const H = full ? 'h2' : 'h3';
  return (
    <section className="bench-section" aria-labelledby={`b-${s.id}`} id={full ? s.id : undefined}>
      <header className="bench-head">
        <H id={`b-${s.id}`}>{s.title}</H>
        {s.status && <p className={`state ${s.status.ok ? 'ok' : 'miss'}`}>{s.status.text}</p>}
        <p>{s.intro}</p>
      </header>
      {s.id === 'hmr' && s.charts?.[0] ? (
        <ScaleChart c={s.charts[0]} target={hmrTarget()} id={full ? `${s.id}-${s.charts[0].id}` : undefined} />
      ) : (
        <div className={`charts${(s.charts?.length ?? 0) > 1 ? ' two' : ''}`}>
          {s.charts?.map(c => <BarChart key={c.title} c={c} id={full ? `${s.id}-${c.id}` : undefined} />)}
        </div>
      )}
      {s.table && <DataTable t={s.table} title={s.tableTitle} />}
      {s.notes && <p className="bench-note">{s.notes}</p>}
      {s.method && (
        <details className="method-card" open={full || undefined}>
          <summary>How we measured</summary>
          <MethodList m={s.method} sources={full} />
        </details>
      )}
    </section>
  );
}
