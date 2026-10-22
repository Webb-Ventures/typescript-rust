import config from '@/data/site.json';

export type Mode = 'coming-soon' | 'launched';
// `orMore`: an open-ended top tier ("$5,000+"); the link prefills the amount and the donor can raise it.
export type Tier = { amount: number; name: string; perks: string[]; url?: string; featured?: boolean; orMore?: boolean };
export type Sponsor = { name: string; url?: string; tier?: string };

// SITE_MODE (a repository variable in CI) overrides `mode` in data/site.json.
const mode = (process.env.SITE_MODE || config.mode) as Mode;
if (mode !== 'coming-soon' && mode !== 'launched') {
  throw new Error(`site mode must be coming-soon or launched, not "${mode}"`);
}

export const site = {
  mode,
  url: (process.env.SITE_URL || 'https://ts-r.dev').replace(/\/$/, ''),
  name: 'ts-r',
  title: 'ts-r: TypeScript tooling, rebuilt in Rust',
  description:
    'ts-r is six fast TypeScript tools written from scratch in Rust: a dev server, bundler, type checker, linter, runner and language server. Each replaces one tool you use today.',
  repo: 'https://github.com/Webb-Ventures/typescript-rust',
  org: 'Webb Ventures',
  donate: config.donate,
  tiers: config.tiers as Tier[],
  sponsors: config.sponsors as Sponsor[],
};

export const ogImage = {
  url: '/og.png',
  width: 1200,
  height: 630,
  alt: 'ts-r: TypeScript tooling, rebuilt in Rust. Measured speed-ups against Vite, ESLint, esbuild, tsc, tsx and tsserver.',
  type: 'image/png',
};

export const money = (n: number) => `$${n.toLocaleString('en-US')}`;

// The donate link is a Stripe "customer chooses what to pay" Payment Link, which takes the
// amount in cents as ?prefilled_amount=. A tier's own `url` wins if it has one.
export function tierUrl(t: Tier): string {
  if (t.url) return t.url;
  const u = new URL(site.donate.url);
  u.searchParams.set('prefilled_amount', String(Math.round(t.amount * 100)));
  return u.toString();
}

if (!site.donate.url) throw new Error('set donate.url in data/site.json');
