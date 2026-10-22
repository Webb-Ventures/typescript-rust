import { markdownResponse } from '@/components/doc-page';
import { assertContent } from '@/lib/content/docs.mjs';

export const dynamic = 'force-static';
export const dynamicParams = false;
export const generateStaticParams = () =>
  assertContent().docs.filter(d => d.slug !== 'index' && !d.slug.includes('/')).map(d => ({ page: d.slug }));

export async function GET(_: Request, { params }: { params: Promise<{ page: string }> }) {
  return markdownResponse(assertContent().markdown.get(`docs/${(await params).page}/`));
}
