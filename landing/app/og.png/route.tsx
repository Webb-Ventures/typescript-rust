import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { ImageResponse } from 'next/og';
import { tiles } from '@/lib/bench';
import { site } from '@/lib/site';

// The share image for every page, rendered to /og.png at build time. A route with a file
// extension (not opengraph-image.tsx) so static hosts serve it as image/png.
export const dynamic = 'force-static';
const size = { width: 1200, height: 630 };

const font = (weight: number) => readFile(join(process.cwd(), `node_modules/@fontsource/inter/files/inter-latin-${weight}-normal.woff`));

export async function GET() {
  const [regular, bold] = await Promise.all([font(400), font(800)]);
  const stats = tiles().slice(0, 6);
  return new ImageResponse(
    (
      <div
        style={{
          width: '100%', height: '100%', display: 'flex', flexDirection: 'column', padding: '64px 72px',
          background: 'radial-gradient(800px 500px at 0% 0%, rgba(240,129,74,.22), transparent 70%), radial-gradient(800px 500px at 100% 0%, rgba(122,162,255,.18), transparent 70%), #0d0f13',
          color: '#e6e8ec', fontFamily: 'Inter',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 20 }}>
          <div style={{ width: 64, height: 64, borderRadius: 16, background: 'linear-gradient(135deg, #f0814a, #7aa2ff)', display: 'flex', alignItems: 'center', justifyContent: 'center', color: '#0d0f13', fontSize: 34, fontWeight: 800 }}>
            {'</>'}
          </div>
          <div style={{ fontSize: 44, fontWeight: 800 }}>ts-r</div>
          <div style={{ marginLeft: 12, fontSize: 20, color: '#7aa2ff', border: '1px solid rgba(122,162,255,.5)', borderRadius: 999, padding: '4px 16px' }}>{site.mode === 'launched' ? 'alpha' : 'coming soon'}</div>
        </div>
        <div style={{ display: 'flex', flexDirection: 'column', marginTop: 56, fontSize: 68, fontWeight: 800, letterSpacing: -2, lineHeight: 1.05 }}>
          <div style={{ display: 'flex' }}>TypeScript tooling,</div>
          <div style={{ display: 'flex', color: '#f0814a' }}>rebuilt in Rust.</div>
        </div>
        <div style={{ display: 'flex', gap: 20, marginTop: 'auto' }}>
          {stats.map(s => (
            <div key={s.tool} style={{ display: 'flex', flexDirection: 'column', flex: 1, background: '#14171d', border: '1px solid rgba(230,232,236,.12)', borderRadius: 16, padding: '18px 20px' }}>
              <div style={{ display: 'flex', fontSize: 40, fontWeight: 800, color: '#f0814a' }}>{s.ratio}×</div>
              <div style={{ display: 'flex', fontSize: 17, color: '#a9b0bd', marginTop: 4 }}>{s.what.replace('faster ', 'faster ')}</div>
            </div>
          ))}
        </div>
      </div>
    ),
    {
      ...size,
      fonts: [
        { name: 'Inter', data: regular, weight: 400, style: 'normal' },
        { name: 'Inter', data: bold, weight: 800, style: 'normal' },
      ],
    },
  );
}
