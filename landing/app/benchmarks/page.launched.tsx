import Link from 'next/link';
import { BenchSection } from '@/components/charts';
import { Crumbs } from '@/components/docs';
import { benchAbout, benchUpdated, notMeasured, sections } from '@/lib/bench';
import { pageMeta } from '@/lib/meta';

export const metadata = pageMeta({
  title: 'ts-r benchmarks: measured numbers and how we measured them',
  description:
    'Measured results for every ts-r tool against the tool it replaces: Vite, esbuild, ESLint, tsc 7, tsserver and tsx, plus ecosystem pass rates. Every number says how it was measured.',
  path: 'benchmarks/',
  absolute: true,
});

// Every section of data/bench.json, each with its method. The home and tool pages draw a subset.
export default function Benchmarks() {
  const all = sections();
  return (
    <main id="main" className="wrap bench-page">
      <section className="hero doc-hero" aria-labelledby="title">
        <Crumbs items={[['Benchmarks', 'benchmarks/']]} />
        <ul className="badges" aria-label="Status">
          <li className="badge status warn">Pre-alpha</li>
          <li className="badge">Updated {benchUpdated}</li>
        </ul>
        <h1 id="title">Benchmarks</h1>
        <p className="lede">{benchAbout}</p>
        <nav aria-label="Sections">
          <ul className="bench-toc">
            {all.map(s => <li key={s.id}><a href={`#${s.id}`}>{s.nav ?? s.title.split(':')[0]}</a></li>)}
            <li><a href="#not-measured">Not measured yet</a></li>
          </ul>
        </nav>
      </section>
      {all.map(s => <BenchSection key={s.id} s={s} full />)}
      <section id="not-measured" className="bench-section" aria-labelledby="not-measured-h">
        <header className="bench-head">
          <h2 id="not-measured-h">Not measured yet</h2>
          <p>The plan compares every tool against the one it replaces. These comparisons have no numbers yet:</p>
        </header>
        <ul className="checks no">{notMeasured.map(n => <li key={n}>{n}</li>)}</ul>
        <p className="bench-note">
          The raw data for this page is <a href="/data/bench.json">bench.json</a>. Each tool’s page (<Link href="/tools/">Tools</Link>) draws
          the sections that belong to it.
        </p>
      </section>
    </main>
  );
}
