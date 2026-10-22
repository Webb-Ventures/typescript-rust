import type { MetadataRoute } from 'next';
import { assertContent } from '@/lib/content/docs.mjs';
import { site } from '@/lib/site';
import { TOOLS } from '@/lib/tools';

export const dynamic = 'force-static';

// Launched mode also lists the docs, the error-code pages and the benchmarks (not /search/,
// which is noindex).
function launchedPages(): MetadataRoute.Sitemap {
  if (site.mode !== 'launched') return [];
  const { docs, errors } = assertContent();
  return ['benchmarks/', ...docs.map(d => d.path), 'errors/', ...errors.map(e => e.path)]
    .map(p => ({ url: `${site.url}/${p}`, changeFrequency: 'weekly' as const, priority: 0.5 }));
}

export default function sitemap(): MetadataRoute.Sitemap {
  return [
    { url: `${site.url}/`, changeFrequency: 'weekly', priority: 1 },
    { url: `${site.url}/tools/`, changeFrequency: 'weekly', priority: 0.8 },
    ...TOOLS.map(t => ({ url: `${site.url}/tools/${t.name}/`, changeFrequency: 'weekly' as const, priority: 0.7 })),
    { url: `${site.url}/donate/`, changeFrequency: 'monthly', priority: 0.4 },
    ...launchedPages(),
  ];
}
