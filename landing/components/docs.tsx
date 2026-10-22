// The docs shell shared by docs pages, error-code pages and the error index (launched mode):
// a sidebar with the docs nav and a search box, breadcrumbs, the article and an "On this page"
// list. Article bodies are HTML rendered from content/ by lib/content/docs.mjs.
import Link from 'next/link';
import { JsonLd } from '@/components/parts';
import { content, SECTIONS } from '@/lib/content/docs.mjs';
import { site } from '@/lib/site';

export type NavLink = { path: string; label: string };
export type TocItem = { level: number; id: string; html: string };
export type Badge = { cls: string; text: string };

function DocNav({ current }: { current: string }) {
  const { nav } = content();
  return (
    <nav className="doc-nav" aria-label="Docs">
      <form className="doc-search" action="/search/" method="get" role="search">
        <label className="sr-only" htmlFor="doc-q">Search docs and error codes</label>
        <input id="doc-q" name="q" type="search" placeholder="Search docs and errors" autoComplete="off" />
      </form>
      {SECTIONS.map(s => (
        <div key={s}>
          <p className="doc-nav-h">{s}</p>
          <ul>
            {nav.filter(i => i.section === s).map(i => (
              <li key={i.path}>
                <Link href={`/${i.path}`} aria-current={i.path === current ? 'page' : undefined}>{i.label}</Link>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  );
}

export function Toc({ items }: { items: TocItem[] }) {
  if (items.length < 2) return null;
  return (
    <nav className="doc-toc" aria-label="On this page">
      <p className="doc-nav-h">On this page</p>
      <ul>
        {items.map(h => (
          <li key={h.id} className={`lvl${h.level}`}>
            <a href={`#${h.id}`} dangerouslySetInnerHTML={{ __html: h.html }} />
          </li>
        ))}
      </ul>
    </nav>
  );
}

export function Badges({ items }: { items: Badge[] }) {
  if (!items.length) return null;
  return (
    <ul className="badges" aria-label="Status">
      {items.map(b => <li key={b.text} className={`badge${b.cls ? ` status ${b.cls}` : ''}`}>{b.text}</li>)}
    </ul>
  );
}

export function Pager({ prev, next }: { prev: NavLink | null; next: NavLink | null }) {
  if (!prev && !next) return null;
  return (
    <nav className="tool-pager doc-pager" aria-label="Previous and next">
      {prev ? <Link href={`/${prev.path}`} rel="prev"><span>Previous</span><strong>{prev.label}</strong></Link> : <i />}
      {next ? <Link href={`/${next.path}`} rel="next"><span>Next</span><strong>{next.label}</strong></Link> : <i />}
    </nav>
  );
}

// "ts-r / Docs / ts-r-check": every crumb but the last is a link. Also emits the BreadcrumbList.
export function Crumbs({ items }: { items: [string, string][] }) {
  const all: [string, string][] = [['ts-r', ''], ...items];
  return (
    <>
      <JsonLd
        data={{
          '@context': 'https://schema.org',
          '@type': 'BreadcrumbList',
          itemListElement: all.map(([name, p], i) => ({ '@type': 'ListItem', position: i + 1, name, item: `${site.url}/${p}` })),
        }}
      />
      <nav className="crumbs" aria-label="Breadcrumb">
        {all.map(([name, p], i) =>
          i === all.length - 1 ? (
            <span key={p} aria-current="page">{name}</span>
          ) : (
            <span key={p} className="crumb">
              <Link href={`/${p}`}>{name}</Link>
              <span aria-hidden="true">/</span>
            </span>
          ),
        )}
      </nav>
    </>
  );
}

export function DocShell({ current, toc, children }: { current: string; toc: TocItem[]; children: React.ReactNode }) {
  return (
    <div className="wrap doc-layout">
      <DocNav current={current} />
      <main id="main" className="doc">
        <div className="doc-grid">
          <article className="doc-body">{children}</article>
          <Toc items={toc} />
        </div>
      </main>
    </div>
  );
}

export function Prose({ html }: { html: string }) {
  return <div className="prose-doc" dangerouslySetInnerHTML={{ __html: html }} />;
}
