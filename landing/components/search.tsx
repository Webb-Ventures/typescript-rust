'use client';
// Client-side search on /search/ over /search-index.json (built from content/ at build time).
// Without JavaScript the page lists every page instead (rendered on the server, #all-pages).
import { useEffect, useState } from 'react';

type Entry = { t: string; u: string; s: string; d: string };

const norm = (s: string) => s.toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '');

function score(e: Entry, terms: string[], raw: string) {
  const t = norm(e.t);
  const d = norm(e.d || '');
  let s = 0;
  for (const term of terms) {
    if (t === term) s += 100;
    else if (t.startsWith(term)) s += 40;
    else if (t.includes(term)) s += 20;
    else if (d.includes(term)) s += 5;
    else return 0;
  }
  // "2322" finds TS2322.
  if (/^\d{4,6}$/.test(raw) && t === `ts${raw}`) s += 200;
  if (e.s === 'Section') s -= 3;
  return s;
}

export function Search() {
  const [index, setIndex] = useState<Entry[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [q, setQ] = useState('');

  useEffect(() => {
    setQ(new URLSearchParams(location.search).get('q') ?? '');
    fetch('/search-index.json')
      .then(r => r.json())
      .then(setIndex)
      .catch(() => setFailed(true));
  }, []);

  const raw = norm(q.trim());
  const hits = index && raw
    ? index
        .map(e => [score(e, raw.split(/\s+/), raw), e] as const)
        .filter(([s]) => s > 0)
        .sort((a, b) => b[0] - a[0] || a[1].t.localeCompare(b[1].t))
        .slice(0, 50)
    : [];

  useEffect(() => {
    if (!index) return;
    const url = new URL(location.href);
    if (q.trim()) url.searchParams.set('q', q.trim());
    else url.searchParams.delete('q');
    history.replaceState(null, '', url);
    const all = document.getElementById('all-pages');
    if (all) all.hidden = Boolean(raw);
  }, [q, raw, index]);

  const status = failed
    ? 'Search index unavailable; every page is listed below.'
    : !index
      ? 'Loading the search index…'
      : !raw
        ? `${index.length} pages and sections. Type to search.`
        : hits.length
          ? `${hits.length === 50 ? 'First 50' : hits.length} result${hits.length === 1 ? '' : 's'} for “${q.trim()}”.`
          : `No results for “${q.trim()}”.`;

  return (
    <>
      <form className="search-box" action="/search/" method="get" role="search" onSubmit={ev => ev.preventDefault()}>
        <label htmlFor="q">Search docs and error codes</label>
        <div className="row">
          <input id="q" name="q" type="search" placeholder="TS2322, ts-r-lint, sound mode…" autoComplete="off" value={q} onChange={ev => setQ(ev.target.value)} />
          <button className="btn" type="submit">Search</button>
        </div>
      </form>
      <p id="search-status" className="small muted" role="status" aria-live="polite">{status}</p>
      {hits.length > 0 && (
        <ol id="search-results" className="search-results">
          {hits.map(([, e]) => (
            <li key={e.u}>
              <a href={`/${e.u}`}>{e.t}</a> <span className="kind">{e.s}</span>
              {e.d && <p>{e.d}</p>}
            </li>
          ))}
        </ol>
      )}
    </>
  );
}
