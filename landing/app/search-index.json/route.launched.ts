import { assertContent } from '@/lib/content/docs.mjs';

export const dynamic = 'force-static';

// What /search/ searches: [{ t: title, u: path, s: kind, d: description }].
export function GET() {
  return new Response(`${JSON.stringify(assertContent().searchIndex)}\n`, { headers: { 'content-type': 'application/json' } });
}
