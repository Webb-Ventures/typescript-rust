import type { Metadata } from 'next';
import Link from 'next/link';

export const metadata: Metadata = { title: 'Page not found', robots: { index: false } };

export default function NotFound() {
  return (
    <main id="main" className="wrap">
      <section className="hero" aria-labelledby="title">
        <h1 id="title">Page not found</h1>
        <p className="lede">That page doesn’t exist.</p>
        <div className="cta"><Link className="btn primary" href="/">Home</Link></div>
      </section>
    </main>
  );
}
