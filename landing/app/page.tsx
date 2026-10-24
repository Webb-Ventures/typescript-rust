import Link from 'next/link';
import { JsonLd, Race } from '@/components/parts';
import { Correctness, Engine, FAQ, Faq, HowBuilt, Numbers, Scale, SponsorNote, Tools } from '@/components/sections';
import { TOOLS } from '@/lib/tools';
import { tiles } from '@/lib/bench';
import { site } from '@/lib/site';

const structuredData = {
  '@context': 'https://schema.org',
  '@graph': [
    {
      '@type': 'Organization',
      '@id': `${site.url}/#org`,
      name: site.org,
      url: site.url,
      logo: `${site.url}/icon.svg`,
      sameAs: [site.repo],
    },
    {
      '@type': 'WebSite',
      '@id': `${site.url}/#website`,
      name: site.name,
      url: `${site.url}/`,
      description: site.description,
      publisher: { '@id': `${site.url}/#org` },
      inLanguage: 'en',
    },
    {
      '@type': 'SoftwareApplication',
      name: site.name,
      description: site.description,
      url: `${site.url}/`,
      applicationCategory: 'DeveloperApplication',
      applicationSubCategory: 'TypeScript compiler, bundler, linter and language server',
      operatingSystem: 'macOS, Linux, Windows',
      softwareVersion: site.mode === 'launched' ? 'alpha' : 'coming soon',
      license: 'https://spdx.org/licenses/MIT.html',
      offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
      publisher: { '@id': `${site.url}/#org` },
      featureList: TOOLS.map(t => `${t.name}: replaces ${t.replaces}`),
    },
    {
      '@type': 'FAQPage',
      mainEntity: FAQ.map(f => ({ '@type': 'Question', name: f.q, acceptedAnswer: { '@type': 'Answer', text: f.a } })),
    },
  ],
};

export default function Home() {
  const launched = site.mode === 'launched';
  const hero = tiles()[0];
  return (
    <main id="main" className="wrap">
      <JsonLd data={structuredData} />
      <section className="hero hero-home" aria-labelledby="title">
        <div className="hero-head">
          <ul className="badges" aria-label="Status">
            {launched ? <li className="badge status warn">Alpha</li> : <li className="badge status next">Coming soon</li>}
            <li className="badge">Written from scratch in Rust</li>
            <li className="badge">Open source</li>
          </ul>
          <h1 id="title">TypeScript tooling, rebuilt in Rust.</h1>
        </div>
        <div className="hero-copy">
          <p className="lede">
            Six small, fast tools that each replace one you use today: <code>ts-r-dev</code>, <code>ts-r-build</code>,{' '}
            <code>ts-r-check</code>, <code>ts-r-lint</code>, <code>ts-r-run</code> and <code>ts-r-lsp</code>. Each one reads the config you
            already have and works on its own, so you can switch one at a time.
          </p>
          <div className="cta">
            {launched ? (
              <>
                <pre className="cmd"><code>npx @ts-r.dev/cli@next dev</code></pre>
                <a className="btn primary" href="/docs/getting-started/">Get started</a>
              </>
            ) : (
              <Link className="btn primary" href="/tools/">Explore the tools</Link>
            )}
            <a className="btn" href="#numbers">See the numbers</a>
          </div>
        </div>
        <Race t={hero} />
      </section>
      <Numbers />
      <div className="callout" role="note">
        <p>
          <b>{launched ? 'Alpha.' : 'Not ready yet.'}</b> {launched ? 'Expect rough edges.' : 'Nothing is on npm yet.'} The type checker doesn’t give
          the same errors as tsc on every project, so keep tsc in CI until it does.
        </p>
      </div>
      <Engine />
      <Tools />
      <Scale />
      <Correctness />
      <HowBuilt />
      <Faq />
      <SponsorNote />
    </main>
  );
}
