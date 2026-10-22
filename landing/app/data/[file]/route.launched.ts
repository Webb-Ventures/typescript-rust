import fs from 'node:fs';
import path from 'node:path';

export const dynamic = 'force-static';
export const dynamicParams = false;

// The raw data behind the benchmarks, status and error pages, published as /data/<file>.
const FILES = ['bench.json', 'conformance.json', 'diagnostics.json', 'error-examples.json', 'error-selection.json'];
export const generateStaticParams = () => FILES.map(file => ({ file }));

export async function GET(_: Request, { params }: { params: Promise<{ file: string }> }) {
  const file = (await params).file;
  if (!FILES.includes(file)) return new Response('not found', { status: 404 });
  return new Response(fs.readFileSync(path.join(process.cwd(), 'data', file), 'utf8'), { headers: { 'content-type': 'application/json' } });
}
