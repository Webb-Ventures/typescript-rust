import { markdownResponse } from '@/components/doc-page';
import { assertContent } from '@/lib/content/docs.mjs';

export const dynamic = 'force-static';
export const dynamicParams = false;
export const generateStaticParams = () =>
  assertContent().docs.filter(d => d.slug.startsWith('tools/')).map(d => ({ tool: d.slug.slice('tools/'.length) }));

export async function GET(_: Request, { params }: { params: Promise<{ tool: string }> }) {
  return markdownResponse(assertContent().markdown.get(`docs/tools/${(await params).tool}/`));
}
