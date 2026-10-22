import type { Metadata, Viewport } from 'next';
import { Instrument_Sans, JetBrains_Mono } from 'next/font/google';
import { Footer, Header } from '@/components/parts';
import { Reveal } from '@/components/reveal';
import { ogImage, site } from '@/lib/site';
import './globals.css';
import './docs.css';

// Downloaded at build time and served from ts-r.dev: no requests to Google from the page.
// Instrument Sans for text and display (its width axis condenses the big numbers); JetBrains Mono for tool names.
const sans = Instrument_Sans({ subsets: ['latin'], display: 'swap', variable: '--font-sans', axes: ['wdth'] });
const mono = JetBrains_Mono({ subsets: ['latin'], display: 'swap', variable: '--font-mono' });

const REVEAL_BOOT = `try{if('IntersectionObserver'in window&&!matchMedia('(prefers-reduced-motion: reduce)').matches)document.documentElement.classList.add('io')}catch(e){}`;

export const metadata: Metadata = {
  metadataBase: new URL(site.url),
  title: { default: site.title, template: '%s · ts-r' },
  description: site.description,
  applicationName: site.name,
  keywords: [
    'TypeScript', 'Rust', 'TypeScript compiler', 'type checker', 'tsc alternative', 'bundler', 'esbuild alternative',
    'Vite alternative', 'dev server', 'hot module replacement', 'linter', 'ESLint alternative', 'typescript-eslint',
    'language server', 'tsserver', 'tsx', 'ts-node',
  ],
  authors: [{ name: site.org }],
  creator: site.org,
  publisher: site.org,
  category: 'technology',
  alternates: { canonical: '/' },
  openGraph: {
    type: 'website',
    url: '/',
    siteName: site.name,
    title: site.title,
    description: site.description,
    locale: 'en_US',
    images: [ogImage],
  },
  twitter: { card: 'summary_large_image', title: site.title, description: site.description, images: [ogImage] },
  robots: { index: true, follow: true, googleBot: { index: true, follow: true, 'max-image-preview': 'large', 'max-snippet': -1 } },
  formatDetection: { email: false, address: false, telephone: false },
};

export const viewport: Viewport = {
  themeColor: '#0d0f13',
  colorScheme: 'dark',
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`${sans.variable} ${mono.variable}`} suppressHydrationWarning>
      <head>
        {/* Before first paint: hold the charts at zero until they scroll into view (components/reveal.tsx). */}
        <script dangerouslySetInnerHTML={{ __html: REVEAL_BOOT }} />
      </head>
      <body>
        <Header />
        {children}
        <Footer />
        <Reveal />
      </body>
    </html>
  );
}
