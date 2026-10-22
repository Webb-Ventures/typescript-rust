import { assertContent } from '@/lib/content/docs.mjs';

export const dynamic = 'force-static';

// Every docs and error page, with links to their Markdown copies (https://llmstxt.org).
export function GET() {
  return new Response(assertContent().llms, { headers: { 'content-type': 'text/plain; charset=utf-8' } });
}
