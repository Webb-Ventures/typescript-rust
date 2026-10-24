import Link from 'next/link';
import { site } from '@/lib/site';
import type { Tile } from '@/lib/bench';
import { MethodList } from './charts';

export function Logo({ size = 28, id = 'lg' }: { size?: number; id?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
      <defs>
        <linearGradient id={id} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#f0814a" />
          <stop offset="1" stopColor="#7aa2ff" />
        </linearGradient>
      </defs>
      <rect x="1" y="1" width="30" height="30" rx="8" fill={`url(#${id})`} />
      <path d="M11 10 L6 16 L11 22 M21 10 L26 16 L21 22 M18 9 L14 23" stroke="#0d0f13" strokeWidth="2.6" fill="none" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function Header() {
  const launched = site.mode === 'launched';
  return (
    <>
      <a className="skip" href="#main">Skip to content</a>
      <header className="site-header">
        <div className="wrap">
          <Link className="brand" href="/" aria-label="ts-r home">
            <Logo size={26} />
            <span>ts-r</span>
          </Link>
          <nav className="site-nav" aria-label="Main">
            <Link href="/tools/">Tools</Link>
            {launched ? <Link href="/benchmarks/">Benchmarks</Link> : <Link href="/#numbers">Benchmarks</Link>}
            {launched && <Link href="/docs/">Docs</Link>}
            {launched && <a href={site.repo} className="nav-wide">GitHub</a>}
            <Link href="/donate/" className="nav-quiet">Sponsor</Link>
          </nav>
        </div>
      </header>
    </>
  );
}

export function Footer() {
  return (
    <footer className="site-footer">
      <div className="wrap">
        <p className="foot-brand">
          <Logo size={20} id="lg-foot" />
          <span>ts-r is open source and MIT licensed, by {site.org}.</span>
        </p>
        <nav className="foot-nav" aria-label="Footer">
          <Link href="/tools/">Tools</Link>
          <Link href="/donate/">Sponsor</Link>
          {site.mode === 'launched' && (
            <>
              <Link href="/docs/">Docs</Link>
              <Link href="/errors/">Error codes</Link>
              <Link href="/benchmarks/">Benchmarks</Link>
              <a href="/llms.txt">llms.txt</a>
              <a href={site.repo}>GitHub</a>
            </>
          )}
        </nav>
      </div>
    </footer>
  );
}

// CSS custom properties for a bar: --w is the bar's length as a fraction of the track.
const bar = (w: number, extra?: Record<string, string>) => ({ '--w': w.toFixed(4), ...extra }) as React.CSSProperties;

// Two horizontal bars, ours against theirs. Lower is better, so the incumbent's bar fills the
// track and ours is drawn at our share of it. The bars are decoration: the names and values are text.
function Bars({ t, race = false }: { t: Tile; race?: boolean }) {
  // In the hero race, each bar takes as long to fill as its measured time, scaled up so the
  // slower one takes 1.6 s.
  const slow = 1.6;
  const rows = [
    { cls: 'ours', name: t.oursName, value: t.ours, w: t.share },
    { cls: 'theirs', name: t.otherName, value: t.other, w: 1 },
  ];
  return (
    <ul className={`bars${race ? ' race' : ''}`}>
      {rows.map(r => (
        <li key={r.cls} className={r.cls} style={bar(r.w, race ? { '--d': `${(slow * r.w).toFixed(3)}s` } : undefined)}>
          <span className="bar-name">{r.name}</span>
          <span className="bar-val">{r.value}</span>
          <span className="bar" aria-hidden="true"><i /></span>
        </li>
      ))}
    </ul>
  );
}

// The hero's one moment of motion: the headline benchmark as a race. Static under reduced motion.
export function Race({ t, sub = 'Shorter is faster. Measured on open-source code.' }: { t: Tile; sub?: string }) {
  return (
    <figure className="race-card">
      <figcaption>
        <span className="race-title">{t.measure}</span>
        <span className="race-sub">{sub}</span>
      </figcaption>
      <Bars t={t} race />
      <p className="race-result">
        <span className="ratio">{t.ratio}×</span> <span>{t.what}</span>
      </p>
    </figure>
  );
}

export function TileCard({ t }: { t: Tile }) {
  return (
    <article className="tile">
      <p className="tile-tool">
        <Link href={`/tools/${t.tool}/`}><code>{t.tool}</code></Link> <span className="tag">Measured</span>
        <span className="tile-replaces">replaces {t.replaces}</span>
      </p>
      <p className="tile-ratio">
        <span className="ratio">{t.ratio}×</span> <span className="what">{t.what}</span>
      </p>
      <Bars t={t} />
      <p className="tile-measure">{t.measure}</p>
      <details className="method-card">
        <summary>How we measured</summary>
        <MethodList m={t.method} />
      </details>
    </article>
  );
}

export function JsonLd({ data }: { data: object }) {
  // Escape "<" so the JSON can't close the script element.
  return <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(data).replace(/</g, '\\u003c') }} />;
}
