import Link from 'next/link';
import { money, site, tierUrl, type Tier } from '@/lib/site';
import { benchUpdated, chart, correctness, hmrTarget, notMeasured, tiles } from '@/lib/bench';
import { TOOLS } from '@/lib/tools';
import { BarChart, Meters, ScaleChart } from './charts';
import { TileCard } from './parts';

export const FAQ = [
  { q: 'What is ts-r?', a: 'Six command-line tools for TypeScript, written from scratch in Rust: a dev server, a bundler, a type checker, a linter, a runner and a language server. Each one replaces one tool you use today.' },
  { q: 'Do I have to switch everything at once?', a: 'No. Each tool works on its own next to tsc, ESLint and Vite, and reads the config you already have: tsconfig.json, your ESLint config, your vite.config. Switch one at a time, in any order.' },
  { q: 'Is it a fork of tsc, esbuild or oxc?', a: 'No. Every layer (lexer, parser, type checker, bundler, minifier, linter) is written in this project. Other tools’ test suites and outputs are used to check that ts-r gives the same answers.' },
  { q: 'Will it be free?', a: 'Yes. ts-r will be open source under MIT OR Apache-2.0, with no paid tier. Sponsorships help fund development time.' },
  { q: 'When can I use it?', a: `${site.mode === 'launched' ? 'Now, as an alpha: npm install --save-dev @ts-r.dev/cli@next.' : 'It runs from source today and isn’t on npm yet.'} The type checker doesn’t give the same errors as tsc on every project yet, so keep tsc in CI until it does.` },
];

const fmtDate = (d: string) => new Date(`${d}T00:00:00Z`).toLocaleDateString('en-GB', { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' });

// Two columns for two or four tiles (no row with a lone tile), three otherwise.
const tileCols = (n: number) => (n === 2 || n === 4 ? 'cols-2' : 'cols-3');

export function Numbers() {
  const all = tiles();
  return (
    <section className="block" aria-labelledby="numbers">
      <header className="block-head">
        <h2 id="numbers">Measured on open-source code, against the tools you use today</h2>
        <p>
          Early numbers from pre-release builds on pinned open-source projects and generated programs, each tile comparing both tools on
          the same input. Every number says how it was measured: open “How we measured” on any tile.
        </p>
      </header>
      <div className={`grid ${tileCols(all.length)} tiles`}>
        {all.map(t => <TileCard key={t.tool} t={t} />)}
      </div>
      <details className="not-measured">
        <summary>What we haven’t measured yet</summary>
        <ul>{notMeasured.map(n => <li key={n}>{n}</li>)}</ul>
      </details>
    </section>
  );
}

// The shared crates, grouped by what they do. `for` lists the tools built on each, from lib/tools.ts.
const CRATES: { group: string; items: { id: string; what: string }[] }[] = [
  { group: 'Front end', items: [
    { id: 'lexer', what: 'TS, TSX and JS tokens' },
    { id: 'parser', what: 'Error-recovering parser' },
    { id: 'ast', what: 'Compact, index-based AST' },
  ] },
  { group: 'Project', items: [
    { id: 'config', what: 'tsconfig.json and extends' },
    { id: 'resolver', what: 'Node and TS resolution' },
    { id: 'cache', what: 'Content-addressed disk cache' },
  ] },
  { group: 'Types', items: [
    { id: 'binder', what: 'Scopes and symbols' },
    { id: 'checker', what: 'Types, inference, diagnostics' },
  ] },
  { group: 'Output', items: [
    { id: 'transform', what: 'Strips types, lowers JSX' },
    { id: 'codegen', what: 'JavaScript and source maps' },
    { id: 'bundler', what: 'Graph, tree shaking, chunks' },
    { id: 'minify', what: 'Shorter names and syntax' },
  ] },
  { group: 'Services', items: [
    { id: 'hmr', what: 'HTTP, WebSocket, hot updates' },
    { id: 'lint', what: 'Rules and ESLint configs' },
  ] },
];

const CRATE_COUNT = CRATES.reduce((n, g) => n + g.items.length, 0);

// What we're building: six binaries on one engine. Picking a tool lights the crates it is built
// on (radio inputs and :has() in globals.css; no JavaScript).
export function Engine() {
  return (
    <section className="block" aria-labelledby="engine-h">
      <header className="block-head">
        <h2 id="engine-h">One engine under six tools</h2>
        <p>
          Every tool parses with the same front end, resolves modules the same way and shares one cache, so work done by one is reused by the
          next. Nothing underneath is borrowed: no swc, no oxc, no esbuild, no TypeScript. Pick a tool to see what it’s built on.
        </p>
      </header>
      <div className="engine">
        <fieldset className="engine-pick">
          <legend className="sr-only">Show the crates a tool is built on</legend>
          <label><input type="radio" name="engine-tool" value="all" defaultChecked /><span>All six</span></label>
          {TOOLS.map(t => (
            <label key={t.name}><input type="radio" name="engine-tool" value={t.name} /><span>{t.name}</span></label>
          ))}
        </fieldset>
        <div className="engine-board">
          {CRATES.map(g => (
            <div className="crate-group" key={g.group}>
              <p className="crate-group-name">{g.group}</p>
              <ul>
                {g.items.map((c, i) => (
                  <li
                    key={c.id}
                    className="crate"
                    data-for={TOOLS.filter(t => t.crates.includes(c.id)).map(t => t.name).join(' ')}
                    style={{ '--i': String(i) } as React.CSSProperties}
                  >
                    <code>ts_r_{c.id}</code>
                    <span>{c.what}</span>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
        <p className="engine-caption" aria-live="polite">
          <span data-show="all">{CRATE_COUNT} shared crates, each with its own tests and notes, behind six separate binaries.</span>
          {TOOLS.map(t => (
            <span key={t.name} data-show={t.name}>
              <code>{t.name}</code> is built on {t.crates.length} of the {CRATE_COUNT}. <Link href={`/tools/${t.name}/`}>How {t.name} works</Link>
            </span>
          ))}
        </p>
      </div>
    </section>
  );
}

export function Scale() {
  const c = chart('hmr', 'Save to browser updated');
  const target = hmrTarget();
  return (
    <section className="block split" aria-labelledby="scale-h">
      <header className="block-head">
        <h2 id="scale-h">Hot reload stays flat as the app grows</h2>
        <p>
          ts-r-dev transforms only the file you saved and pushes that one update to the browser, so a save costs about the same at
          50,000 modules as at 1,000. The plan’s target is under {target} ms.
        </p>
        <p className="small-link"><Link href="/tools/ts-r-dev/">More on ts-r-dev</Link></p>
      </header>
      <ScaleChart c={c} target={target} />
    </section>
  );
}

export function Correctness() {
  const rows = correctness();
  const eco = chart('ecosystem', 'Packages passing');
  return (
    <section className="block" aria-labelledby="correct-h">
      <header className="block-head">
        <h2 id="correct-h">Checked against the tools it replaces</h2>
        <p>
          Speed only counts if the answers are right. Each part runs the reference tool’s own tests, or compares output on real code, and
          the gaps are published with the wins.
        </p>
      </header>
      <div className="correct">
        <Meters rows={rows} />
        <div className="correct-side">
          <BarChart c={eco} />
          <p className="small muted">
            The top 1,000 npm packages, each stage compared with tsc 7 or esbuild. No package’s own test suite passes with ts-r swapped in yet.
          </p>
        </div>
      </div>
    </section>
  );
}

export function Tools() {
  return (
    <section className="block" aria-labelledby="tools">
      <header className="block-head">
        <h2 id="tools">Six tools, each replacing one you use today</h2>
        <p>Each reads the config you already have and works on its own, so you can switch one at a time.</p>
      </header>
      <ToolGrid />
    </section>
  );
}

export function ToolGrid() {
  const byTool = new Map(tiles().map(t => [t.tool, t]));
  return (
    <ul className="tools">
      {TOOLS.map(t => {
        const tile = byTool.get(t.name);
        return (
          <li key={t.name}>
            <Link className="tool" href={`/tools/${t.name}/`}>
              <h3><code>{t.name}</code></h3>
              <p className="replaces">Replaces {t.replaces}</p>
              <p className="tool-body">{t.body}</p>
              {tile && <p className="tool-stat"><b>{tile.ratio}×</b> {tile.what}</p>}
              <span className="tool-more">Read more about {t.name}</span>
            </Link>
          </li>
        );
      })}
    </ul>
  );
}

// How the project works: the rules every change follows, and the repo commands that enforce them.
const PRINCIPLES = [
  { h: 'Written from scratch', p: 'The lexer, parser, binder, checker, bundler, minifier, linter, dev server and language server are all written in this repo, in Rust, with no other compiler underneath.' },
  { h: 'Other tools are the oracle', p: 'TypeScript’s conformance tests, esbuild’s bundler tests and ESLint’s rule tests run against ts-r. Where ts-r disagrees, ts-r is wrong until shown otherwise.' },
  { h: 'Every number has a commit', p: 'Each benchmark records its machine, input, command and the commit that measured it. Numbers that miss their targets are published too.' },
  { h: 'One tool at a time', p: 'Each tool is its own binary that reads your existing config. Adopt the linter this week and keep tsc, Vite and ESLint for everything else.' },
];

export function HowBuilt() {
  return (
    <section className="block" aria-labelledby="how-h">
      <header className="block-head">
        <h2 id="how-h">How it’s built</h2>
      </header>
      <div className="how">
        <ul className="principles">
          {PRINCIPLES.map(x => (
            <li key={x.h}><h3>{x.h}</h3><p>{x.p}</p></li>
          ))}
        </ul>
        <figure className="term">
          <figcaption>The repo’s own checks</figcaption>
          <pre><code>
            <span className="c"># TypeScript’s tests, libs and diagnostics, pinned</span>{'\n'}
            <span className="p">$</span> cargo xtask upstream sync{'\n'}
            <span className="c"># tsc 7 as the reference for every diagnostic</span>{'\n'}
            <span className="p">$</span> cargo xtask oracle{'\n'}
            <span className="c"># against tsc 7, esbuild, oxc and tsx; 5% regression gate</span>{'\n'}
            <span className="p">$</span> cargo xtask bench{'\n'}
            <span className="c"># the top 1,000 npm packages, stage by stage</span>{'\n'}
            <span className="p">$</span> cargo xtask ecosystem
          </code></pre>
        </figure>
      </div>
      <p className="small muted updated">Numbers on this site were last updated on {fmtDate(benchUpdated)}.</p>
    </section>
  );
}

export function Faq() {
  return (
    <section className="block faq-block" aria-labelledby="faq">
      <header className="block-head">
        <h2 id="faq">Frequently asked questions</h2>
      </header>
      <div className="faq">
        {FAQ.map(f => (
          <details key={f.q}>
            <summary><h3>{f.q}</h3></summary>
            <p>{f.a}</p>
          </details>
        ))}
      </div>
    </section>
  );
}

// The only mention of money on the home page: one quiet line at the end.
export function SponsorNote() {
  return (
    <aside className="sponsor-note" aria-label="Sponsorship">
      <p>
        ts-r is free and open source. If your team would like to fund its development, <Link href="/donate/">sponsorship is open</Link>.
      </p>
    </aside>
  );
}

// Don't break "ts-r.dev" or "ts-r" at the hyphen.
const keepTogether = (text: string) =>
  text.split(/(ts-r[\w.-]*\w)/).map((part, i) => (i % 2 ? <span key={i} className="nowrap">{part}</span> : part));

function TierCard({ t }: { t: Tier }) {
  return (
    <article className={`tier${t.featured ? ' featured' : ''}${t.orMore ? ' top' : ''}`}>
      {t.featured && <p className="tier-flag">Recommended</p>}
      <h3>{t.name}</h3>
      <p className="tier-amount">{money(t.amount)}{t.orMore && '+'}</p>
      <ul className="perks">
        {t.perks.map(p => <li key={p}>{keepTogether(p)}</li>)}
      </ul>
      <a className={`btn${t.featured ? ' primary' : ''}`} href={tierUrl(t)} rel="noopener" aria-label={`Give ${money(t.amount)}${t.orMore ? ' or more' : ''}: ${t.name}`}>
        Give {money(t.amount)}{t.orMore && ' or more'}
      </a>
    </article>
  );
}

export function Tiers() {
  return (
    <>
      <div className="grid tiers cols-3">
        {site.tiers.map(t => <TierCard key={t.amount} t={t} />)}
      </div>
      <p className="small muted tier-note">
        Payments are handled by {site.donate.platform}. Any other amount: <a href={site.donate.url} rel="noopener">give what you like</a>.
      </p>
    </>
  );
}

export function Sponsors() {
  return (
    <section className="block" aria-labelledby="sponsors">
      <header className="block-head">
        <h2 id="sponsors">Thank you</h2>
      </header>
      {site.sponsors.length ? (
        <ul className="sponsors">
          {site.sponsors.map(s => <li key={s.name}>{s.url ? <a href={s.url} rel="sponsored noopener">{s.name}</a> : s.name}</li>)}
        </ul>
      ) : (
        <p className="sponsors-empty">No sponsors yet. Sponsors are listed here once they join.</p>
      )}
    </section>
  );
}
