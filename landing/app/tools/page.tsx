import type { Metadata } from 'next';
import { JsonLd } from '@/components/parts';
import { ToolGrid } from '@/components/sections';
import { ogImage, site } from '@/lib/site';

const title = 'The ts-r tools';
const description = 'Six TypeScript tools written from scratch in Rust: a dev server, bundler, type checker, linter, runner and language server. Each replaces one tool you use today.';

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: '/tools/' },
  openGraph: { url: '/tools/', title, description, images: [ogImage] },
  twitter: { card: 'summary_large_image', title, description, images: [ogImage] },
};

export default function ToolsIndex() {
  return (
    <main id="main" className="wrap">
      <JsonLd
        data={{
          '@context': 'https://schema.org',
          '@type': 'BreadcrumbList',
          itemListElement: [
            { '@type': 'ListItem', position: 1, name: 'ts-r', item: `${site.url}/` },
            { '@type': 'ListItem', position: 2, name: 'Tools', item: `${site.url}/tools/` },
          ],
        }}
      />
      <section className="hero" aria-labelledby="title">
        <h1 id="title">Six tools, one at a time</h1>
        <p className="lede">
          Each tool is its own binary that reads the config you already have and works next to tsc, ESLint and Vite. Pick the one that
          hurts most today; keep the rest.
        </p>
      </section>
      <section className="block tools-index" aria-label="Tools">
        <ToolGrid />
      </section>
    </main>
  );
}
