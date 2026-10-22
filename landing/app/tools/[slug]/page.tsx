import type { Metadata } from 'next';
import Link from 'next/link';
import { notFound } from 'next/navigation';
import { BenchSection, Meters } from '@/components/charts';
import { JsonLd, Race } from '@/components/parts';
import { correctness, section, tileFor } from '@/lib/bench';
import { ogImage, site } from '@/lib/site';
import { TOOLS, toolBySlug, type Tool } from '@/lib/tools';

export const dynamicParams = false;
export const generateStaticParams = () => TOOLS.map(t => ({ slug: t.name }));

type Props = { params: Promise<{ slug: string }> };

const sentence = (s: string) => s[0].toUpperCase() + s.slice(1);

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const t = toolBySlug((await params).slug);
  if (!t) return {};
  const title = `${t.name}: ${t.title}`;
  const description = `${sentence(t.title)} in Rust that replaces ${t.replaces}. What works today, what doesn’t yet, and every benchmark with its method.`;
  const url = `/tools/${t.name}/`;
  return {
    title,
    description,
    alternates: { canonical: url },
    openGraph: { url, title, description, images: [ogImage] },
    twitter: { card: 'summary_large_image', title, description, images: [ogImage] },
  };
}

function Diff({ d }: { d: Tool['diff'] }) {
  return (
    <figure className="term diff reveal">
      <figcaption>{d.file}</figcaption>
      <pre><code>
        {d.before.map(l => <span key={l} className="ctx">{`  ${l}\n`}</span>)}
        {d.changes.map(([a, b]) => (
          <span key={a}>
            <span className="del">{`-   ${a}\n`}</span>
            <span className="add">{`+   ${b}\n`}</span>
          </span>
        ))}
        {d.after.map(l => <span key={l} className="ctx">{`  ${l}\n`}</span>)}
      </code></pre>
    </figure>
  );
}

export default async function ToolPage({ params }: Props) {
  const t = toolBySlug((await params).slug);
  if (!t) notFound();
  const launched = site.mode === 'launched';
  const tile = tileFor(t.name);
  const rows = correctness().filter(r => t.correctness.includes(r.what));
  const i = TOOLS.indexOf(t);
  const prev = TOOLS[(i + TOOLS.length - 1) % TOOLS.length];
  const next = TOOLS[(i + 1) % TOOLS.length];
  return (
    <main id="main" className="wrap tool-page">
      <JsonLd
        data={{
          '@context': 'https://schema.org',
          '@type': 'BreadcrumbList',
          itemListElement: [
            { '@type': 'ListItem', position: 1, name: 'ts-r', item: `${site.url}/` },
            { '@type': 'ListItem', position: 2, name: 'Tools', item: `${site.url}/tools/` },
            { '@type': 'ListItem', position: 3, name: t.name, item: `${site.url}/tools/${t.name}/` },
          ],
        }}
      />
      <section className={`hero tool-hero${tile ? '' : ' no-race'}`} aria-labelledby="title">
        <div className="tool-hero-copy">
          <nav className="crumbs" aria-label="Breadcrumb">
            <Link href="/tools/">Tools</Link>
            <span aria-hidden="true">/</span>
            <span aria-current="page">{t.name}</span>
          </nav>
          <h1 id="title"><code>{t.name}</code></h1>
          <p className="tool-title">{sentence(t.title)}. Replaces {t.replaces}.</p>
          <ul className="badges" aria-label="Status">
            <li className="badge status warn">{launched ? 'Alpha' : 'Pre-alpha'}</li>
            <li className="badge">{launched ? 'On npm' : 'Works from source'}</li>
          </ul>
          <p className="lede">{t.intro}</p>
        </div>
        {tile && <Race t={tile} sub={launched ? 'Shorter is faster.' : 'Shorter is faster. Pre-release build, real app.'} />}
      </section>

      <section className="block tool-switch" aria-labelledby="switch-h">
        <header className="block-head">
          <h2 id="switch-h">What switching looks like</h2>
          <p>
            {launched
              ? `${t.name} reads the config you already have, so switching is a change to the command, not the project.`
              : `Nothing is on npm yet. Once it is, switching looks like this: ${t.name} reads the config you already have, so the change is to the command, not the project.`}
          </p>
        </header>
        <Diff d={t.diff} />
      </section>

      <section className="block" aria-labelledby="status-h">
        <header className="block-head">
          <h2 id="status-h">What works, and what doesn’t yet</h2>
        </header>
        <div className="status-cols">
          <div>
            <h3>Works today</h3>
            <ul className="checks yes">{t.works.map(w => <li key={w}>{w}</li>)}</ul>
          </div>
          <div>
            <h3>Not yet</h3>
            <ul className="checks no">{t.notYet.map(w => <li key={w}>{w}</li>)}</ul>
          </div>
        </div>
      </section>

      <section className="block" aria-labelledby="measured-h">
        <header className="block-head">
          <h2 id="measured-h">Measured</h2>
          <p>Every run, on the same reference app as the home page, with the command and commit that recorded it.</p>
        </header>
        {t.sections.map(id => <BenchSection key={id} s={section(id)} />)}
        {rows.length > 0 && (
          <section className="bench-section" aria-labelledby="b-correct">
            <header className="bench-head">
              <h3 id="b-correct">Correctness</h3>
              <p>Compared with the tool {t.name} replaces, on its own tests and on real code.</p>
            </header>
            <Meters rows={rows} />
          </section>
        )}
      </section>

      <section className="block cli-block" aria-labelledby="cli-h">
        <div>
          <h2 id="cli-h">Command line</h2>
          <p className="muted">
            An excerpt of <code>{t.name} --help</code>. You can also run it through the launcher as <code>ts-r {t.name.replace('ts-r-', '')}</code>.
          </p>
          <h3 className="next-to">{t.nextTo.title}</h3>
          <p>{t.nextTo.body}</p>
        </div>
        <figure className="term">
          <figcaption>{t.name} --help</figcaption>
          <pre><code>{t.usage}</code></pre>
        </figure>
      </section>

      <nav className="tool-pager" aria-label="Other tools">
        <Link href={`/tools/${prev.name}/`} rel="prev"><span>Previous</span><code>{prev.name}</code></Link>
        <Link href={`/tools/${next.name}/`} rel="next"><span>Next</span><code>{next.name}</code></Link>
      </nav>
    </main>
  );
}
