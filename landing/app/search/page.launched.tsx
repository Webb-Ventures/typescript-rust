import Link from 'next/link';
import { Search } from '@/components/search';
import { assertContent, stripMd } from '@/lib/content/docs.mjs';
import { pageMeta } from '@/lib/meta';

export const metadata = pageMeta({
  title: 'Search the ts-r docs',
  description: 'Search the ts-r docs and error-code pages.',
  path: 'search/',
  absolute: true,
  noindex: true,
});

export default function SearchPage() {
  const { docs, errors } = assertContent();
  return (
    <main id="main" className="wrap search-page">
      <section className="hero doc-hero" aria-labelledby="title">
        <h1 id="title">Search</h1>
      </section>
      <Search />
      <noscript>
        <p className="small muted">Without JavaScript this page lists every page below; use your browser’s find to search it.</p>
      </noscript>
      <div id="all-pages" className="prose-doc">
        <h2 id="docs">Docs</h2>
        <ul className="code-list">
          {docs.map(d => (
            <li key={d.path}><Link href={`/${d.path}`}>{stripMd(d.title)}</Link> <span>{d.description}</span></li>
          ))}
          <li><Link href="/benchmarks/">Benchmarks</Link> <span>Measured numbers for every tool and how each was measured.</span></li>
        </ul>
        <h2 id="errors">Error codes</h2>
        <ul className="code-list">
          {errors.map(e => (
            <li key={e.code}><Link href={`/${e.path}`}><code>{e.code}</code></Link> <span>{e.plain}</span></li>
          ))}
        </ul>
      </div>
    </main>
  );
}
