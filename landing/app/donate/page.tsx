import type { Metadata } from 'next';
import { JsonLd } from '@/components/parts';
import { Sponsors, Tiers } from '@/components/sections';
import { ogImage, site } from '@/lib/site';

const title = 'Sponsor ts-r';
const description = 'Fund the development of ts-r, fast TypeScript tooling written from scratch in Rust. One-off donations from $10 to $5,000 and up, with sponsor perks at every tier.';

export const metadata: Metadata = {
  title: { absolute: `${title}: fund fast TypeScript tooling in Rust` },
  description,
  alternates: { canonical: '/donate/' },
  openGraph: { url: '/donate/', title, description, images: [ogImage] },
  twitter: { card: 'summary_large_image', title, description, images: [ogImage] },
};

export default function Donate() {
  return (
    <main id="main" className="wrap">
      <JsonLd
        data={{
          '@context': 'https://schema.org',
          '@type': 'BreadcrumbList',
          itemListElement: [
            { '@type': 'ListItem', position: 1, name: 'ts-r', item: `${site.url}/` },
            { '@type': 'ListItem', position: 2, name: title, item: `${site.url}/donate/` },
          ],
        }}
      />
      <section className="hero" aria-labelledby="title">
        <h1 id="title">Help build ts-r</h1>
        <p className="lede">
          ts-r will be free and open source. Sponsorship pays for the time to build it: the type checker, the bundler, the linter
          and the editor tooling. Every tier includes the perks of the tiers below it.
        </p>
      </section>
      <section className="block tiers-block" aria-labelledby="tiers">
        <h2 id="tiers" className="sr-only">Sponsor tiers</h2>
        <Tiers />
      </section>
      <Sponsors />
    </main>
  );
}
