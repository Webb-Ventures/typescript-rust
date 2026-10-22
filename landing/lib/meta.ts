import type { Metadata } from 'next';
import { ogImage } from '@/lib/site';

// Metadata for a page at `path` ("docs/x/"): canonical, Open Graph and Twitter card. `absolute`
// skips the layout's "%s · ts-r" title template (for titles that already name ts-r).
export function pageMeta({ title, description, path, absolute = false, noindex = false, markdown = false }: {
  title: string;
  description: string;
  path: string;
  absolute?: boolean;
  noindex?: boolean;
  /** The page has a Markdown copy at index.md. */
  markdown?: boolean;
}): Metadata {
  const url = `/${path}`;
  return {
    title: absolute ? { absolute: title } : title,
    description,
    alternates: { canonical: url, ...(markdown ? { types: { 'text/markdown': `${url}index.md` } } : {}) },
    openGraph: { type: markdown ? 'article' : 'website', url, title, description, images: [ogImage] },
    twitter: { card: 'summary_large_image', title, description, images: [ogImage] },
    ...(noindex ? { robots: { index: false, follow: true } } : {}),
  };
}
