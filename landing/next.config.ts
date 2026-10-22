import type { NextConfig } from 'next';
import site from './data/site.json';

// Static export: `next build` writes plain files to out/, deployed to GitHub Pages.
//
// One build serves both modes. Routes only published once ts-r launches (docs, error pages,
// benchmarks, search, llms.txt, the Markdown copies and raw data) are files named
// `page.launched.tsx` or `route.launched.ts`: in coming-soon mode those extensions aren't page
// extensions, so the routes don't exist and nothing of them is exported. SITE_MODE wins over
// `mode` in data/site.json, as in lib/site.ts.
const mode = process.env.SITE_MODE || site.mode;
const launched = mode === 'launched';

const config: NextConfig = {
  output: 'export',
  trailingSlash: true,
  images: { unoptimized: true },
  reactStrictMode: true,
  pageExtensions: launched ? ['tsx', 'ts', 'launched.tsx', 'launched.ts'] : ['tsx', 'ts'],
  // At most 4 static-generation workers: one per core used about 4 GB for the ~640 launched
  // pages on a 16-core machine (each worker loads the content). The repo's limit is 4 threads.
  experimental: { cpus: 4 },
};

export default config;
